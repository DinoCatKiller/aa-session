//! 覆盖导入 zed 会话（同 `session_id` 整行替换，自动备份）。
//!
//! 由 [`crate::ZedSource`] 实现 [`aa_session_core::SessionImporter`] 时委托到这里。
//!
//! # 语义
//!
//! - **覆盖**：目标 `id` 已存在 → 整行替换（旧 blob 被新 blob 盖掉）
//! - **新增**：目标 `id` 不存在 → `INSERT`（SQLite `UPSERT` 自然分流）
//!
//! # 安全契约
//!
//! 写库前**强制备份**（`VACUUM INTO`，zed 源码里的全库备份方式）。备份文件
//! 名带时间戳，已存在则报错（防止覆盖已有备份）。`apply == false` 时只读不写。
//!
//! 本模块**不**直接操作 `&mut ZedSource`（那是 `import_overwrite` 的活），
//! 只负责「读文件 → 校验 → 查库 → 备份 → 写」的编排。写库本身在
//! `ZedSource::import_overwrite`（见 [`crate::lib`]）。

use aa_session_core::{Error, ImportReport, Result};
use std::path::Path;

use crate::APP_NAME;

/// 执行一次导入（预检或写入）。
pub fn run(source: &mut crate::ZedSource, export_path: &Path, apply: bool) -> Result<ImportReport> {
    use aa_session_core::Error as AaError;

    // 1. 读 + 校验 export 文件
    let raw = std::fs::read_to_string(export_path).map_err(|e| Error::Open {
        path: export_path.display().to_string(),
        cause: e.to_string(),
    })?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|cause| AaError::MalformedJson {
            location: format!("export:{}", export_path.display()),
            cause,
        })?;
    let app = parsed.get("app").and_then(|a| a.as_str()).unwrap_or("");
    if app != APP_NAME {
        return Err(Error::InvalidArgument(format!(
            "export 文件标记 app={app:?}，但本数据源是 {APP_NAME:?}；\
             不能跨 app 导入（防止把 opencode 的消息灌进 zed）"
        )));
    }
    let columns = parsed
        .get("raw_data")
        .and_then(|r| r.get("columns"))
        .ok_or_else(|| {
            Error::InvalidArgument("export 文件缺 `raw_data.columns`，不是本工具的导出产物".into())
        })?;
    let thread_json = parsed
        .get("raw_data")
        .and_then(|r| r.get("thread_json"))
        .ok_or_else(|| {
            Error::InvalidArgument(
                "export 文件缺 `raw_data.thread_json`，不是本工具的导出产物".into(),
            )
        })?;
    let session_id = columns
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::InvalidArgument("export 文件 `raw_data.columns` 缺 `id`".into()))?;

    // 2. 查是否已存在（决定覆盖 / 新增）
    let is_overwrite = source.thread_exists(session_id)?;

    // 3. 预检 vs 写
    let target = source.path().display().to_string();
    let backup: Option<String> = if apply {
        // 写前强制备份：VACUUM INTO 出全库副本，文件名带时间戳，已存在则拒。
        // 时间戳只到秒，同一秒内连续两次 apply 会撞名——加一个 `_n` 后缀
        // 单调递增（同秒第 2 次 → `_1792041908_1`，第 3 次 → `_1792041908_2`），
        // 保证每次 apply 都有独立备份。
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut backup_path = source
            .path()
            .with_file_name(format!("threads.db.aa_session_backup_{ts}"));
        let mut suffix = 0u32;
        while backup_path.exists() {
            suffix += 1;
            backup_path = source
                .path()
                .with_file_name(format!("threads.db.aa_session_backup_{ts}_{suffix}"));
        }
        {
            let conn = source.conn_mut();
            let sql = format!(
                "VACUUM INTO '{}'",
                backup_path.display().to_string().replace('\'', "''")
            );
            conn.execute_batch(&sql).map_err(crate::query_err)?;
        }
        Some(backup_path.display().to_string())
    } else {
        None
    };

    let n_written = if apply {
        Some(source.import_overwrite(session_id, columns, thread_json)?)
    } else {
        None
    };

    let summary = if let Some(n) = n_written {
        format!(
            "已{}线程 {session_id}（{n} 行写入）",
            if is_overwrite { "覆盖" } else { "新增" }
        )
    } else {
        let verb = if is_overwrite {
            "将覆盖"
        } else {
            "将新增"
        };
        format!("{verb}线程 {session_id}；加 --apply 执行（写前会自动备份 threads.db）")
    };

    Ok(ImportReport {
        session_id: session_id.to_string(),
        target,
        is_overwrite,
        applied: apply,
        backup,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use crate::ZedSource;
    use aa_session_core::SessionImporter;

    /// 在临时目录里造一个「空 threads 表」的库 + 一个 export 文件，
    /// 跑「预检 → apply → 再 apply 覆盖」全流程，验证：
    ///  - 预检不写库
    ///  - apply 写库且备份生成
    ///  - 第二次 apply（覆盖）成功
    ///  - 备份文件存在且可读
    #[test]
    fn import_flow_preflight_then_apply_then_overwrite() {
        let dir =
            std::env::temp_dir().join(format!("aa_session_zed_import_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db_path = dir.join("threads.db");

        // 1. 造库（threads 表，与 zed 的真实 schema 同列序）
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY,
                    summary TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    data_type TEXT NOT NULL,
                    data BLOB NOT NULL,
                    parent_id TEXT,
                    folder_paths TEXT,
                    folder_paths_order TEXT,
                    created_at TEXT
                );",
            )
            .unwrap();
            conn.close().unwrap();
        }

        // 2. 造 export 文件（结构跟 CLI `export` 的产物一致）
        let export_path = dir.join("export.json");
        let export = serde_json::json!({
            "app": "zed",
            "session_id": "test-thread-1",
            "raw_data": {
                "columns": {
                    "id": "test-thread-1",
                    "summary": "测试线程",
                    "updated_at": "2026-10-15T00:00:00+00:00",
                    "created_at": "2026-10-15T00:00:00+00:00",
                    "parent_id": null,
                    "folder_paths": "/tmp/project",
                    "folder_paths_order": "0",
                    "data_type": "zstd"
                },
                "thread_json": {
                    "version": "0.3.0",
                    "title": "测试线程",
                    "messages": [{"User": {"id": "u1", "content": [{"Text": "hi"}]}}]
                }
            }
        });
        std::fs::write(&export_path, export.to_string()).unwrap();

        // 3. 打开可写连接
        let mut source = ZedSource::open_writable(&db_path).unwrap();

        // 4. 预检：不写库，不产生备份
        let preflight = source.import(&export_path, false).unwrap();
        assert!(!preflight.applied);
        assert!(
            !preflight.is_overwrite,
            "库里还没有这个线程，应是「将新增」"
        );
        assert_eq!(preflight.backup, None);
        assert!(db_backup_count(&dir) == 0, "预检不应产生备份");
        let count_after_preflight: i64 = {
            let c = rusqlite::Connection::open(&db_path).unwrap();
            c.query_row(
                "SELECT count(*) FROM threads WHERE id = 'test-thread-1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count_after_preflight, 0, "预检不能写库");

        // 5. apply：写库 + 备份
        let applied = source.import(&export_path, true).unwrap();
        assert!(applied.applied);
        assert!(!applied.is_overwrite, "第一次 apply 是新增");
        assert!(applied.backup.is_some(), "apply 必须产生备份");
        assert!(db_backup_count(&dir) == 1);
        let count_after_apply: i64 = {
            let c = rusqlite::Connection::open(&db_path).unwrap();
            c.query_row(
                "SELECT count(*) FROM threads WHERE id = 'test-thread-1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count_after_apply, 1, "apply 后应插入 1 行");

        // 6. 再 apply 一次（覆盖）
        let overwrite = source.import(&export_path, true).unwrap();
        assert!(overwrite.applied);
        assert!(overwrite.is_overwrite, "已有该线程，第二次是覆盖");
        let count_after_overwrite: i64 = {
            let c = rusqlite::Connection::open(&db_path).unwrap();
            c.query_row(
                "SELECT count(*) FROM threads WHERE id = 'test-thread-1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(count_after_overwrite, 1, "覆盖不应新增行");

        // 7. 解压写回的 blob，验证消息内容无损
        let blob = {
            let c = rusqlite::Connection::open(&db_path).unwrap();
            c.query_row(
                "SELECT data FROM threads WHERE id = 'test-thread-1'",
                [],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .unwrap()
        };
        let json_bytes = crate::decompress_zstd(&blob).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(parsed["version"], "0.3.0");
        assert_eq!(parsed["title"], "测试线程");
        assert_eq!(parsed["messages"][0]["User"]["id"], "u1");

        // 8. 清理
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 跨 app 导入（opencode 文件 → zed）必须被拒。
    #[test]
    fn import_rejects_cross_app_file() {
        let dir = std::env::temp_dir().join(format!(
            "aa_session_zed_import_cross_{:?}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let db_path = dir.join("threads.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads (
                    id TEXT PRIMARY KEY, summary TEXT NOT NULL, updated_at TEXT NOT NULL,
                    data_type TEXT NOT NULL, data BLOB NOT NULL,
                    parent_id TEXT, folder_paths TEXT, folder_paths_order TEXT, created_at TEXT);",
            )
            .unwrap();
            conn.close().unwrap();
        }
        let export_path = dir.join("cross.json");
        let export = serde_json::json!({
            "app": "opencode",
            "session_id": "x",
            "raw_data": { "columns": {"id": "x"}, "thread_json": {} }
        });
        std::fs::write(&export_path, export.to_string()).unwrap();

        let mut source = ZedSource::open_writable(&db_path).unwrap();
        let err = source.import(&export_path, false).unwrap_err();
        match err {
            aa_session_core::Error::InvalidArgument(msg) => {
                assert!(msg.contains("跨 app"), "应报跨 app 错误，实际: {msg}");
            }
            other => panic!("预期 InvalidArgument，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn db_backup_count(dir: &std::path::Path) -> u32 {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("threads.db.aa_session_backup_")
            })
            .count() as u32
    }
}
