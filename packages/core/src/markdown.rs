//! 会话正文的归一化表示 + Markdown 渲染。
//!
//! 各家 agent 的原始存储形态差别很大（opencode 是两张表的 JSON 字符串、
//! codebuddy 是事件流、zed 是整包压缩 JSON），但**对话本身的形状是一样的**：
//! 一条消息有角色（用户 / 助手 / 系统），消息里混着正文、推理和工具调用。
//! 所以各 source 只负责把 `dump()` 解析成这里的 [`Message`]，真正的排版
//! 收敛到 [`render`] 一处 —— 否则三个 source 各写一遍标题、缩进、代码围栏，
//! 很快就会各走各的。
//!
//! # 格式参照
//!
//! 标题与工具调用沿用 zed 的导出格式（`crates/agent/src/thread.rs` 与
//! `crates/acp_thread/src/acp_thread.rs`）：
//!
//! - 用户 / 助手分别是 `## User` / `## Assistant`；
//! - 推理包在 `<thinking>` / `</thinking>` 里（zed ACP 的写法）；
//! - 工具调用 `**Tool Use**: name (ID: id)` + JSON 参数块，
//!   结果 `**Tool Result**: name (ID: id)` + 原始输出（不套围栏，保持可搜索）。
//!
//! 与 zed 的差别只有两处，都是为了「一个会话一个文件」这个场景：
//! 文件开头多一个元信息头（标题 / app / 目录 / 时间），空会话显式写一句
//! 「没有消息」而不是留一个空文件。

use std::fmt::Write as _;

use serde_json::Value;

use crate::model::SessionMeta;

/// 一条消息的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    /// Markdown 里的二级标题。
    pub fn heading(self) -> &'static str {
        match self {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        }
    }
}

/// 一次工具调用的完整记录（调用 + 结果）。
///
/// 各家的工具结果都跟着调用走（opencode 在同一个 block 里、zed 在
/// `tool_results` 映射里、codebuddy 单独一个事件），这里合并成一个结构，
/// 渲染时就能把「调用」和「结果」紧挨着排版。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// 调用 id（opencode 的 `callID`、zed 的 `ToolUse.id`）。
    pub id: String,
    /// 工具名。
    pub name: String,
    /// 状态（`completed` / `error` / `running` …，上游没记录时为 `None`）。
    pub status: Option<String>,
    /// 调用参数。已经是 JSON 值，不再保留原始字符串。
    pub input: Value,
    /// 输出正文。没有输出（还在跑 / 空结果）时为 `None`。
    pub output: Option<String>,
    /// 错误信息（`status == error` 时通常有）。
    pub error: Option<String>,
}

impl ToolCall {
    /// 新建一次工具调用（结果随后补）。
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status: None,
            input,
            output: None,
            error: None,
        }
    }
}

/// 消息里的一个片段。
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// 正文（用户输入或模型输出）。
    Text(String),
    /// 推理 / thinking。默认不折叠，保留全文供搜索。
    Thinking(String),
    /// 工具调用（含结果）。
    Tool(ToolCall),
}

/// 归一化后的一条消息。
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn new(role: Role, parts: Vec<Part>) -> Self {
        Self { role, parts }
    }

    pub fn user(parts: Vec<Part>) -> Self {
        Self::new(Role::User, parts)
    }

    pub fn assistant(parts: Vec<Part>) -> Self {
        Self::new(Role::Assistant, parts)
    }

    pub fn system(parts: Vec<Part>) -> Self {
        Self::new(Role::System, parts)
    }

    /// 没有可渲染的内容（只有被跳过的 `step-start` 之类）。
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

/// 把一个会话渲染成 Markdown。
///
/// `session` 只用来写文件头（标题 / app / 目录 / 时间），`messages` 是
/// 对话正文。空对话也返回完整的文件头 + 一行说明，避免导出 0 字节文件。
pub fn render(session: &SessionMeta, messages: &[Message]) -> String {
    let mut md = String::new();

    let title = one_line(&session.title);
    let title = if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    };
    let _ = writeln!(md, "# {title}");
    let _ = writeln!(md);

    // 只写有值的字段：`undated` / 空目录并不比不写更有信息量。
    meta_line(&mut md, "app", &session.app);
    meta_line(&mut md, "session", &session.id);
    meta_line(&mut md, "directory", &session.directory);
    if let Some(agent) = &session.agent {
        meta_line(&mut md, "agent", agent);
    }
    if let Some(model) = &session.model {
        meta_line(&mut md, "model", &model_label(model));
    }
    if let Some(parent) = &session.parent_id {
        meta_line(&mut md, "parent", parent);
    }
    if let Some(created) = fmt_datetime(session.time_created) {
        meta_line(&mut md, "created", &created);
    }
    if let Some(updated) = fmt_datetime(session.time_updated) {
        meta_line(&mut md, "updated", &updated);
    }

    if messages.iter().all(Message::is_empty) {
        let _ = writeln!(md, "\n_此会话没有消息。_");
        return md;
    }

    // 元信息与第一条消息之间空一行。之后每段内容自己以 `\n\n` 收尾，
    // 所以这里只补一次，避免消息之间出现两个空行。
    let _ = writeln!(md);
    for message in messages {
        if message.is_empty() {
            continue;
        }
        let _ = writeln!(md, "## {}", message.role.heading());
        let _ = writeln!(md);
        for part in &message.parts {
            render_part(&mut md, part);
        }
    }

    md
}

/// `SessionMeta.model` 在各 source 里大多是 JSON 字符串
/// （`{"id":"…","providerID":"…"}`），直接写进 Markdown 是一坨转义文本。
/// 能认出这种形态就写成 `id (provider)`，认不出就原样显示。
fn model_label(model: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(model) else {
        return one_line(model);
    };
    let Some(id) = value.get("id").and_then(Value::as_str) else {
        return one_line(model);
    };
    match value.get("providerID").and_then(Value::as_str) {
        Some(provider) => format!("{id} ({provider})"),
        None => id.to_string(),
    }
}

fn meta_line(md: &mut String, key: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let _ = writeln!(md, "- {key}: {}", one_line(value));
}

fn render_part(md: &mut String, part: &Part) {
    match part {
        Part::Text(text) => {
            let text = text.trim_end();
            if text.is_empty() {
                return;
            }
            md.push_str(text);
            md.push_str("\n\n");
        }
        Part::Thinking(text) => {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let _ = writeln!(md, "<thinking>");
            md.push_str(text);
            let _ = writeln!(md, "\n</thinking>");
            let _ = writeln!(md);
        }
        Part::Tool(tool) => render_tool(md, tool),
    }
}

fn render_tool(md: &mut String, tool: &ToolCall) {
    let _ = writeln!(md, "**Tool Use**: {} (ID: {})", tool.name, tool.id);
    if let Some(status) = &tool.status {
        let _ = writeln!(md, "Status: {status}");
    }
    let _ = writeln!(md);

    if let Some(input) = tool_input_block(&tool.input) {
        let _ = writeln!(md, "{}", fenced("json", &input));
        let _ = writeln!(md);
    }

    // 结果：错误优先，其次才是正文。两者都为空就只留调用（还在跑）。
    if let Some(error) = tool
        .error
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        let _ = writeln!(md, "**Tool Result**: {} (ID: {})", tool.name, tool.id);
        let _ = writeln!(md);
        let _ = writeln!(md, "**ERROR:**");
        let _ = writeln!(md);
        md.push_str(error);
        md.push_str("\n\n");
    } else if let Some(output) = tool
        .output
        .as_deref()
        .map(str::trim_end)
        .filter(|o| !o.is_empty())
    {
        let _ = writeln!(md, "**Tool Result**: {} (ID: {})", tool.name, tool.id);
        let _ = writeln!(md);
        md.push_str(output);
        md.push_str("\n\n");
    }
}

/// 工具参数块的内容；空参数返回 `None`（不画一个空 JSON 块）。
fn tool_input_block(input: &Value) -> Option<String> {
    match input {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        Value::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        Value::Array(items) if items.is_empty() => None,
        other => serde_json::to_string_pretty(other).ok(),
    }
}

/// 用足够长的围栏包住正文，正文里出现 ``` 也不会截断代码块。
fn fenced(lang: &str, body: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{lang}\n{}\n{fence}", body.trim_end())
}

/// 把多行文本压成一行（标题 / 元信息字段用）。
fn one_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Unix 毫秒 → `YYYY-MM-DD HH:MM UTC`；时间缺失（<= 0）返回 `None`。
///
/// 不引 chrono：civil_from_days 只有几行，和 CLI 表格里的日期格式化同理。
fn fmt_datetime(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    Some(format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        tod / 3600,
        (tod % 3600) / 60
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> SessionMeta {
        SessionMeta {
            app: "opencode".into(),
            id: "ses_demo".into(),
            project_id: "p".into(),
            project_path: "/home/aa/repos/demo".into(),
            parent_id: None,
            title: "修复导出".into(),
            directory: "/home/aa/repos/demo".into(),
            agent: Some("build".into()),
            model: Some("a-model".into()),
            time_created: 1_792_843_200_000,
            time_updated: 1_792_846_800_000,
            tokens_input: 0,
            tokens_output: 0,
            tokens_reasoning: 0,
            cost: None,
            files_changed: 0,
            archived_at: None,
        }
    }

    #[test]
    fn header_carries_title_and_metadata() {
        let md = render(&session(), &[]);
        assert!(md.starts_with("# 修复导出\n\n"), "{md}");
        assert!(md.contains("- app: opencode\n"), "{md}");
        assert!(md.contains("- session: ses_demo\n"), "{md}");
        assert!(md.contains("- directory: /home/aa/repos/demo\n"), "{md}");
        assert!(md.contains("- agent: build\n"), "{md}");
        assert!(md.contains("- created: 2026-10-24 12:00 UTC\n"), "{md}");
        assert!(md.ends_with("_此会话没有消息。_\n"), "{md}");
    }

    #[test]
    fn user_and_assistant_use_zed_headings() {
        let messages = vec![
            Message::user(vec![Part::Text("你好".into())]),
            Message::assistant(vec![Part::Text("你好！".into())]),
        ];
        let md = render(&session(), &messages);
        assert!(md.contains("\n## User\n\n你好\n\n"), "{md}");
        assert!(md.contains("\n## Assistant\n\n你好！\n\n"), "{md}");
    }

    #[test]
    fn json_model_string_is_shown_as_id_and_provider() {
        let mut s = session();
        s.model = Some(r#"{"id":"gemma4:31b","providerID":"ollama-cloud"}"#.into());
        let md = render(&s, &[]);
        assert!(md.contains("- model: gemma4:31b (ollama-cloud)\n"), "{md}");
    }

    #[test]
    fn empty_title_falls_back_to_untitled() {
        let mut s = session();
        s.title = "  \n ".into();
        assert!(
            render(&s, &[]).starts_with("# Untitled\n"),
            "{}",
            render(&s, &[])
        );
    }

    #[test]
    fn thinking_is_wrapped_in_tags_not_folded_away() {
        // 推理必须保留全文：导出物的用途就是给编辑器全文搜索。
        let messages = vec![Message::assistant(vec![Part::Thinking(
            "先看 git status".into(),
        )])];
        let md = render(&session(), &messages);
        assert!(
            md.contains("<thinking>\n先看 git status\n</thinking>\n\n"),
            "{md}"
        );
    }

    #[test]
    fn tool_call_renders_input_and_result() {
        let mut tool = ToolCall::new("call_1", "read", json!({"filePath": "src/main.rs"}));
        tool.status = Some("completed".into());
        tool.output = Some("1: fn main() {}".into());

        let md = render(&session(), &[Message::assistant(vec![Part::Tool(tool)])]);
        assert!(
            md.contains("**Tool Use**: read (ID: call_1)\nStatus: completed\n\n```json\n"),
            "{md}"
        );
        assert!(md.contains("\"filePath\": \"src/main.rs\""), "{md}");
        assert!(
            md.contains("**Tool Result**: read (ID: call_1)\n\n1: fn main() {}"),
            "{md}"
        );
    }

    #[test]
    fn failed_tool_shows_error_instead_of_empty_result() {
        let mut tool = ToolCall::new("call_2", "shell", json!({"command": "rg 'x'"}));
        tool.status = Some("error".into());
        tool.error = Some("regex parse error".into());
        let md = render(&session(), &[Message::assistant(vec![Part::Tool(tool)])]);
        assert!(md.contains("**ERROR:**\n\nregex parse error\n\n"), "{md}");
    }

    #[test]
    fn fence_grows_when_input_contains_backticks() {
        let mut tool = ToolCall::new(
            "call_3",
            "write",
            json!({"text": "```rust\nfn f() {}\n```"}),
        );
        tool.output = None;
        let md = render(&session(), &[Message::assistant(vec![Part::Tool(tool)])]);
        // 内容里有三个反引号 → 围栏必须至少四个
        assert!(md.contains("````json\n"), "{md}");
        assert!(md.contains("\n````\n"), "{md}");
    }

    #[test]
    fn empty_object_input_is_not_rendered_as_a_block() {
        let tool = ToolCall::new("call_4", "todo", json!({}));
        let md = render(&session(), &[Message::assistant(vec![Part::Tool(tool)])]);
        assert!(!md.contains("```json"), "{md}");
    }

    #[test]
    fn missing_timestamps_are_omitted_not_rendered_as_1970() {
        let mut s = session();
        s.time_created = 0;
        s.time_updated = 0;
        let md = render(&s, &[]);
        assert!(!md.contains("created:"), "{md}");
        assert!(!md.contains("1970-01-01"), "{md}");
    }

    #[test]
    fn system_messages_have_their_own_heading() {
        let md = render(
            &session(),
            &[Message::system(vec![Part::Text(
                "The available tools have changed.".into(),
            )])],
        );
        assert!(
            md.contains("## System\n\nThe available tools have changed."),
            "{md}"
        );
    }

    #[test]
    fn text_is_not_trimmed_at_the_start() {
        // 首行缩进是代码块的一部分，只去掉尾部空白。
        let md = render(
            &session(),
            &[Message::user(vec![Part::Text("    缩进保留\n".into())])],
        );
        assert!(md.contains("\n## User\n\n    缩进保留\n\n"), "{md}");
    }

    #[test]
    fn title_newlines_are_collapsed() {
        let mut s = session();
        s.title = "第一行\n第二行".into();
        assert!(
            render(&s, &[]).starts_with("# 第一行 第二行\n"),
            "{}",
            render(&s, &[])
        );
    }
}
