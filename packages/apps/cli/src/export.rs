//! 导出：单会话 JSON 组装 + 批量导出到文件夹。
//!
//! # 为什么批量导出是「一会话一文件」
//!
//! 产物是**给人读的文件树**：一条会话一个 JSON，文件名带创建日期、标题 slug
//! 和 id 前缀，这样在编辑器的文件树里按名字就能认出是哪次对话，全文检索又能
//! 直接命中文件内容（导出物保留原始字符串，不做归一）。
//!
//! 不合并成一个大 JSON 的三个理由：
//!
//! 1. 编辑器里每次命中都落在**具体会话**上，而不是一个几十 MB 的巨型文件；
//! 2. 增量重跑时只需跳过已存在的文件，不必重写全部；
//! 3. 单个会话的导出物可以直接喂给 `import`（它就是单会话导出格式）。
//!
//! 批量导出**不写任何 agent 数据库**，只往目标目录落盘。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use aa_session_core::{SessionMeta, SessionSource};
use anyhow::{Context, Result};

use crate::output::{fmt_date, human_bytes, out};

/// 批量导出的索引文件名。
///
/// 下划线开头：在文件树里排在所有会话文件之前，且不会和会话 id 撞名
/// （opencode 的 id 是 `ses_...`，codebuddy 是 uuid，zed 是十六进制串）。
const INDEX_FILE_NAME: &str = "_index.json";

/// 文件名里保留的会话 id 前缀长度。
///
/// 与表格的 `short_id` 一致（12 位）：足够区分，又不会把文件名撑得太长。
const ID_PREFIX: usize = 12;

/// 标题 slug 的最大字符数（按字符计，不是字节）。
///
/// 40 个 CJK 字符 = 120 字节，加上日期和 id 仍远低于常见的 255 字节限制。
const SLUG_CHARS: usize = 40;

/// 组装一个会话的导出对象。
///
/// 结果包一层 `{app, session_id, raw_data}`，`raw_data` 就是 source 的
/// `dump()` 原样返回（opencode 是多张表，codebuddy 是 JSONL 事件流）。
/// 用户的编辑器全文检索直接命中原始字符串，不做任何归一。
pub fn assemble(source: &dyn SessionSource, session_id: &str) -> Result<serde_json::Value> {
    let raw = source.dump(session_id)?;
    Ok(serde_json::json!({
        "app": source.app(),
        "session_id": session_id,
        "raw_data": raw,
    }))
}

/// 单会话导出：写一个 JSON 文件，或只预演。
///
/// 与批量模式共用 `-o/--force/--dry-run`，但这里的 `-o` 是**文件路径**。
pub fn write_single(
    source: &dyn SessionSource,
    session_id: &str,
    output: Option<PathBuf>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let path = output.unwrap_or_else(|| PathBuf::from(format!("{session_id}.json")));

    if path.exists() && !force {
        anyhow::bail!(
            "{} 已存在；加 --force 覆盖，或用 -o 指定别的路径",
            path.display()
        );
    }

    if dry_run {
        // 预演不 dump（那才是贵的部分），但要确认会话真的存在，
        // 否则打错 id 的预演会给出虚假的「将写入」。
        if source.session(session_id)?.is_none() {
            anyhow::bail!("会话 {session_id} 不存在");
        }
        out(format_args!("将写入 {}（预演，不写盘）", path.display()));
        return Ok(());
    }

    let file = assemble(source, session_id)?;
    let text = serde_json::to_string_pretty(&file)?;

    // 顺手创建父目录：`-o exports/foo/chat.json` 不该因为 exports/foo
    // 不存在而失败，批量模式也是同样的行为。
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("无法创建输出目录 {}", parent.display()))?;
    }
    std::fs::write(&path, &text).with_context(|| format!("写入 {} 失败", path.display()))?;

    out(format_args!(
        "已导出（{} 字节）→ {}",
        text.len(),
        path.display()
    ));
    Ok(())
}

/// 批量导出的选项。
#[derive(Debug, Clone)]
pub struct BatchOptions {
    /// 输出目录（不存在则创建）。
    pub out_dir: PathBuf,
    /// 覆盖已存在的同名文件（默认跳过，便于增量重跑）。
    pub force: bool,
    /// 只打印将要写出的文件，不落盘。
    pub dry_run: bool,
}

/// 一次批量导出的结果统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// 本次实际写出的文件数。
    pub exported: usize,
    /// 因文件已存在而跳过的会话数。
    pub skipped: usize,
    /// 导出失败的会话数（不中断整批）。
    pub failed: usize,
    /// 本次实际写出的字节数（不含跳过的旧文件）。
    pub bytes: u64,
}

/// 把一批会话导出到 `opts.out_dir`。
///
/// # 失败策略
///
/// 单条会话导出失败**不中断**整批 —— 一个损坏的会话不该让另外 200 条白等。
/// 失败计入 [`BatchReport::failed`]，其余文件照常写入，最后以非零退出码
/// 结束（由本函数 `bail!`）。
///
/// # 覆盖策略
///
/// 默认**跳过**已存在的同名文件，这样「补导出新增会话」可以反复重跑；
/// `--force` 才覆盖。注意文件名里的日期取自会话创建时间，所以同一会话在
/// 上游被更新后文件名不变，要刷新内容得加 `--force`。
pub fn run_batch(
    source: &dyn SessionSource,
    sessions: &[SessionMeta],
    opts: &BatchOptions,
    filter: serde_json::Value,
) -> Result<BatchReport> {
    if sessions.is_empty() {
        out(format_args!("没有匹配的会话，未写入任何文件。"));
        return Ok(BatchReport::default());
    }

    out(format_args!(
        "将导出 {} 条会话 → {}{}",
        sessions.len(),
        opts.out_dir.display(),
        if opts.dry_run {
            "（预演，不写盘）"
        } else {
            ""
        },
    ));

    if opts.dry_run {
        for s in sessions {
            out(format_args!("  {}  {}", s.id, file_name_for(s)));
        }
        out(format_args!("\n这是预演。确认无误后去掉 --dry-run 执行。"));
        return Ok(BatchReport::default());
    }

    std::fs::create_dir_all(&opts.out_dir)
        .with_context(|| format!("无法创建输出目录 {}", opts.out_dir.display()))?;

    let mut report = BatchReport::default();
    let mut used: HashSet<String> = HashSet::new();
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(sessions.len());

    for s in sessions {
        // 一批里 id 不会重复，但文件名是「日期 + 标题 + id 前缀」，理论上
        // 仍可能撞（同一天、同标题、id 前缀相同）。撞了就改用完整 id，
        // **绝不覆盖同批的另一条会话**。
        let mut file = file_name_for(s);
        if used.contains(&file) {
            file = file_name_with_full_id(s);
        }
        used.insert(file.clone());

        let path = opts.out_dir.join(&file);
        // 先记下「本次之前就存在」：既用于跳过判断，也写进索引，
        // 让用户知道哪些文件是这次新写的、哪些是旧的。
        let pre_existing = path.exists();

        if pre_existing && !opts.force {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            out(format_args!("跳过 {}（{} 已存在）", s.id, file));
            report.skipped += 1;
            entries.push(IndexEntry::new(s, &file, bytes, true));
            continue;
        }

        match write_one(source, s, &path) {
            Ok(bytes) => {
                out(format_args!("✓ {}  {}  {}", s.id, human_bytes(bytes), file));
                report.exported += 1;
                report.bytes += bytes;
                entries.push(IndexEntry::new(s, &file, bytes, pre_existing));
            }
            Err(e) => {
                // `{e:#}` 打印完整错误链，否则只会看到一句「写入失败」。
                out(format_args!("✗ {}  {e:#}", s.id));
                report.failed += 1;
            }
        }
    }

    let index_path = opts.out_dir.join(INDEX_FILE_NAME);
    let index = Index {
        index_version: 1,
        app: source.app().to_string(),
        generated_at: now_ms(),
        session_count: entries.len(),
        exported: report.exported,
        skipped: report.skipped,
        failed: report.failed,
        filter,
        entries,
    };
    std::fs::write(&index_path, serde_json::to_string_pretty(&index)?)
        .with_context(|| format!("无法写入索引 {}", index_path.display()))?;

    out(format_args!(
        "\n已写入 {} 个文件（{}），跳过 {}，失败 {}",
        report.exported,
        human_bytes(report.bytes),
        report.skipped,
        report.failed,
    ));
    out(format_args!("索引：{}", index_path.display()));

    if report.failed > 0 {
        anyhow::bail!(
            "{} 条会话导出失败（其余文件已正常写入，可加 --force 重跑）",
            report.failed
        );
    }

    Ok(report)
}

/// 写一个会话的导出文件，返回字节数。
fn write_one(source: &dyn SessionSource, s: &SessionMeta, path: &Path) -> Result<u64> {
    let file = assemble(source, &s.id)?;
    let text = serde_json::to_string_pretty(&file)?;
    std::fs::write(path, &text).with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(text.len() as u64)
}

/// 为一条会话生成可读的文件名：`<创建日期>_<标题 slug>_<id 前缀>.json`。
///
/// 日期在前，文件按时间自然排序；标题在中间，扫文件名就能认出对话；
/// id 前缀殿后，保证同名不同会话不会互相覆盖。
pub fn file_name_for(s: &SessionMeta) -> String {
    format!(
        "{}_{}_{}.json",
        date_of(s),
        slug_or_untitled(&s.title),
        id_prefix(&s.id, ID_PREFIX),
    )
}

/// 文件名撞车时的保底方案：用完整 id 取代 12 位前缀。
fn file_name_with_full_id(s: &SessionMeta) -> String {
    let id = safe_component(&s.id);
    let id = if id.is_empty() {
        "unknown-id".to_string()
    } else {
        id
    };
    format!("{}_{}_{}.json", date_of(s), slug_or_untitled(&s.title), id,)
}

/// 日期前缀：优先用创建时间，缺失时退回更新时间。
fn date_of(s: &SessionMeta) -> String {
    let ms = if s.time_created > 0 {
        s.time_created
    } else {
        s.time_updated
    };
    fmt_date(ms)
}

fn slug_or_untitled(title: &str) -> String {
    let slug = slugify(title);
    if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    }
}

/// 把标题压成适合做文件名的 slug。
///
/// 保留字母、数字（含 CJK —— `char::is_alphanumeric` 覆盖）和 `_`，
/// 其余一律折叠成一个 `-`（连续分隔符只留一个，首尾不留）。
/// 按**字符数**截断，不会切坏多字节字符。
fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut chars = 0usize;
    let mut pending_sep = false;

    for ch in title.chars() {
        if chars >= SLUG_CHARS {
            break;
        }
        if ch.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
                chars += 1;
                if chars >= SLUG_CHARS {
                    break;
                }
            }
            pending_sep = false;
            out.push(ch);
            chars += 1;
        } else if !out.is_empty() {
            // 空格、斜杠、冒号、emoji 都是分隔符；叠着出现只算一个。
            pending_sep = true;
        }
    }

    out
}

/// 文件名的 id 段：取前 `n` 个字符并消毒。
fn id_prefix(id: &str, n: usize) -> String {
    let head: String = id.chars().take(n).collect();
    let safe = safe_component(&head);
    if safe.is_empty() {
        "unknown-id".to_string()
    } else {
        safe
    }
}

/// 只留可安全做文件名的字符。
///
/// **必须挡住 `/` 和 `..`**：id 来自数据库，理论上不该有怪字符，但一旦有，
/// 拼进路径就可能写出目标目录之外 —— 这类事情不能靠「应该不会」。
fn safe_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// 未取整的当前 Unix 毫秒时间戳。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `_index.json` 的结构。
///
/// 它描述的是**这一次批量导出的结果**，而不是「目录里现在有什么」：
/// 重跑时会整体重写，所以不要编辑它。
#[derive(serde::Serialize)]
struct Index {
    /// 索引格式版本，将来字段变动时靠它区分。
    index_version: u32,
    app: String,
    /// 生成时间（Unix 毫秒）。
    generated_at: i64,
    /// 索引里的条目数（= 写入 + 跳过，不含失败）。
    session_count: usize,
    exported: usize,
    skipped: usize,
    failed: usize,
    /// 本次导出用的筛选条件快照 —— 过几天回来能知道这个目录是怎么筛出来的。
    filter: serde_json::Value,
    entries: Vec<IndexEntry>,
}

/// `_index.json` 里的一条会话。
#[derive(serde::Serialize)]
struct IndexEntry {
    /// 文件名（相对于输出目录）。
    file: String,
    session_id: String,
    title: String,
    directory: String,
    project_path: String,
    agent: Option<String>,
    time_created: i64,
    time_updated: i64,
    tokens_input: i64,
    tokens_output: i64,
    cost: Option<f64>,
    archived: bool,
    /// 文件字节数（跳过时是旧文件的大小）。
    bytes: u64,
    /// 本次运行之前文件就已存在（跳过或 `--force` 覆盖）。
    pre_existing: bool,
}

impl IndexEntry {
    fn new(s: &SessionMeta, file: &str, bytes: u64, pre_existing: bool) -> Self {
        Self {
            file: file.to_string(),
            session_id: s.id.clone(),
            title: s.title.clone(),
            directory: s.directory.clone(),
            project_path: s.project_path.clone(),
            agent: s.agent.clone(),
            time_created: s.time_created,
            time_updated: s.time_updated,
            tokens_input: s.tokens_input,
            tokens_output: s.tokens_output,
            cost: s.cost,
            archived: s.is_archived(),
            bytes,
            pre_existing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, title: &str, created: i64) -> SessionMeta {
        SessionMeta {
            app: "opencode".into(),
            id: id.into(),
            project_id: "p".into(),
            project_path: "/home/aa/repos/ai_ls/ocr-lab".into(),
            parent_id: None,
            title: title.into(),
            directory: "/home/aa/repos/ai_ls/ocr-lab".into(),
            agent: Some("build".into()),
            model: None,
            time_created: created,
            time_updated: created,
            tokens_input: 0,
            tokens_output: 0,
            tokens_reasoning: 0,
            cost: None,
            files_changed: 0,
            archived_at: None,
        }
    }

    #[test]
    fn slug_keeps_cjk_and_collapses_separators() {
        assert_eq!(
            slugify("修复 zed 的 tool-call 损坏"),
            "修复-zed-的-tool-call-损坏"
        );
        // 连续分隔符（空格 / 斜杠 / 冒号）只留一个，首尾不留
        assert_eq!(slugify("  a // b: c  "), "a-b-c");
    }

    #[test]
    fn slug_drops_emoji_and_punctuation() {
        // emoji 不是 alphanumeric，会被当分隔符折叠掉
        assert_eq!(slugify("🚀 deploy!"), "deploy");
        assert_eq!(slugify("！！"), "");
    }

    #[test]
    fn slug_truncates_by_char_not_byte() {
        let long = "很".repeat(100);
        let slug = slugify(&long);
        assert_eq!(slug.chars().count(), SLUG_CHARS);
        // 截断后仍是合法的 UTF-8（按字符截断的必然结果）
        assert!(slug.is_char_boundary(slug.len()));
    }

    #[test]
    fn id_prefix_is_filesystem_safe() {
        // id 里的斜杠绝不能漏进文件名，否则会写到子目录甚至目录外面
        assert_eq!(id_prefix("../../etc/passwd", 12), "------etc-pa");
        assert_eq!(id_prefix("ses_abc", 12), "ses_abc");
    }

    #[test]
    fn file_name_is_date_slug_id() {
        // 2026-10-24T12:00:00Z
        let s = session("ses_0123456789abcdef", "修复/导出", 1_792_843_200_000);
        let name = file_name_for(&s);
        assert!(name.starts_with("2026-10-24_"), "{name}");
        assert!(name.contains("修复-导出"), "{name}");
        assert!(name.ends_with("_ses_01234567.json"), "{name}");
    }

    #[test]
    fn empty_title_falls_back_to_untitled() {
        let s = session("ses_abc", "   ", 1_792_843_200_000);
        assert!(file_name_for(&s).contains("_untitled_"));
    }

    #[test]
    fn missing_timestamps_are_undated_not_1970() {
        // 「上游没记录时间」和「1970 年」是两回事，文件名不该撒谎。
        let s = session("ses_abc", "x", 0);
        assert!(file_name_for(&s).starts_with("undated_"));
    }

    #[test]
    fn full_id_fallback_is_unique_and_safe() {
        let a = session("ses_0123456789XXX", "同一天同一标题", 1_792_843_200_000);
        let b = session("ses_0123456789YYY", "同一天同一标题", 1_792_843_200_000);
        // 12 位前缀相同 → 正常命名会撞
        assert_eq!(file_name_for(&a), file_name_for(&b));
        // 保底方案用完整 id，因此不再撞
        assert_ne!(file_name_with_full_id(&a), file_name_with_full_id(&b));
    }

    // ---------------------------------------------------------------- 批量落盘

    /// 可控的假数据源：指定某些 id 的 `dump` 失败，用来验证「失败不中断整批」。
    struct MockSource {
        fail: Vec<&'static str>,
    }

    impl SessionSource for MockSource {
        fn app(&self) -> &'static str {
            "mock"
        }

        fn projects(&self) -> aa_session_core::Result<Vec<aa_session_core::ProjectMeta>> {
            Ok(Vec::new())
        }

        fn project_paths_like(&self, _pattern: &str) -> aa_session_core::Result<Vec<String>> {
            Ok(Vec::new())
        }

        fn directories(&self) -> aa_session_core::Result<Vec<aa_session_core::DirectoryMeta>> {
            Ok(Vec::new())
        }

        fn sessions(
            &self,
            _filter: &aa_session_core::SessionFilter,
            _sort: aa_session_core::SortKey,
        ) -> aa_session_core::Result<Vec<SessionMeta>> {
            Ok(Vec::new())
        }

        fn session(&self, _session_id: &str) -> aa_session_core::Result<Option<SessionMeta>> {
            Ok(None)
        }

        fn dump(&self, session_id: &str) -> aa_session_core::Result<serde_json::Value> {
            if self.fail.contains(&session_id) {
                return Err(aa_session_core::Error::NotFound(session_id.to_string()));
            }
            Ok(serde_json::json!({ "text": format!("内容 {session_id}") }))
        }

        fn auth(&self, _show_secrets: bool) -> aa_session_core::Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }

    /// 每个测试用独立目录，避免并行跑测试时互相踩。
    fn temp_out_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aa_session_batch_{}_{tag}", std::process::id()))
    }

    fn opts(dir: &Path) -> BatchOptions {
        BatchOptions {
            out_dir: dir.to_path_buf(),
            force: false,
            dry_run: false,
        }
    }

    #[test]
    fn one_broken_session_does_not_abort_the_batch() {
        let dir = temp_out_dir("partial");
        let _ = std::fs::remove_dir_all(&dir);

        let source = MockSource { fail: vec!["bad"] };
        let sessions = vec![
            session("ses_good1", "正常一", 1_792_843_200_000),
            session("bad", "坏的", 1_792_843_200_000),
            session("ses_good2", "正常二", 1_792_843_200_000),
        ];

        let err = run_batch(&source, &sessions, &opts(&dir), serde_json::json!({})).unwrap_err();
        // 有失败必须以非零退出码结束（否则脚本会以为全导出了）
        assert!(format!("{err:#}").contains("1 条会话导出失败"), "{err:#}");

        // 另外两条照常落盘
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 3, "两条会话 + 一个索引，实际 {names:?}");
        assert!(
            names
                .iter()
                .any(|n| n.contains("正常一") && n.ends_with("ses_good1.json"))
        );
        assert!(
            names
                .iter()
                .any(|n| n.contains("正常二") && n.ends_with("ses_good2.json"))
        );

        // 索引如实记录失败数，且只收成功/跳过的条目
        let index: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(INDEX_FILE_NAME)).unwrap())
                .unwrap();
        assert_eq!(index["failed"], 1);
        assert_eq!(index["session_count"], 2);
        assert_eq!(index["entries"].as_array().unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rerun_skips_existing_files_until_forced() {
        let dir = temp_out_dir("rerun");
        let _ = std::fs::remove_dir_all(&dir);

        let source = MockSource { fail: vec![] };
        let sessions = vec![session("ses_rerun", "重跑", 1_792_843_200_000)];

        let first = run_batch(&source, &sessions, &opts(&dir), serde_json::json!({})).unwrap();
        assert_eq!(first.exported, 1);

        // 第二次默认跳过：这是「补导出新增会话」可以反复重跑的前提
        let second = run_batch(&source, &sessions, &opts(&dir), serde_json::json!({})).unwrap();
        assert_eq!(second.exported, 0);
        assert_eq!(second.skipped, 1);
        assert_eq!(second.bytes, 0, "跳过的旧文件不计入本次写入量");

        // --force 才真的覆盖。注意同一会话的文件名不随更新变化（日期取创建时间），
        // 所以刷新内容的唯一办法就是 --force。
        let forced = run_batch(
            &source,
            &sessions,
            &BatchOptions {
                force: true,
                ..opts(&dir)
            },
            serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(forced.exported, 1);
        assert_eq!(forced.skipped, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
