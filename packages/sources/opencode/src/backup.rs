//! 写 opencode 库前的备份。
//!
//! # 为什么用 SQLite 的 `VACUUM INTO` 而不是文件复制
//!
//! 直接 `cp opencode.db` 是**错的**：库是 WAL 模式的，`-wal` 里可能还有未
//! checkpoint 的事务。只复制主文件会得到一个**残缺**的快照。
//!
//! - `VACUUM INTO 'path'` —— SQLite 自己做的在线一致性快照，包含 WAL 内容，
//!   产出的是一个干净的、独立的、可正常打开的数据库；
//! - `VACUUM INTO` 要求连接有写权限，所以要用读写连接打开（备份本身**不改**
//!   任何业务数据，只是产出新文件）。
//!
//! 备份文件是**完整库**（约 6 GB）。这是有意的：宁可占磁盘，也不能在需要时
//! 发现备份不完整。
//!
//! # 为什么文件名带时间戳
//!
//! 每次写操作都产出**新的**备份文件，永不复用同名文件 —— 既保留「绝不覆盖
//! 上一次撤销点」的安全语义，又不会因为遗留的旧备份而拒绝执行（早期版本
//! 用固定文件名，重跑 `rename-project --apply` 前必须手动删旧备份或加
//! `--backup-to`）。带时间戳也便于按文件名判断备份的先后顺序。

use aa_session_core::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 备份文件名后缀（时间戳前）。
const BACKUP_SUFFIX: &str = ".aa_session_backup";

/// 备份文件名扩展名。
const BACKUP_EXT: &str = ".sqlite";

/// 为数据库生成一个**带时间戳**的备份路径（同一目录，便于定位）。
///
/// 形如 `opencode.db.aa_session_backup.20261110_045339.sqlite`。时间戳取
/// UTC 秒，格式紧凑、按字典序即按时间序。
pub fn backup_path_for(db: &Path) -> PathBuf {
    let stamp = unix_stamp();
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    name.push(format!(".{stamp}"));
    name.push(BACKUP_EXT);
    db.with_file_name(name)
}

/// UTC 时间戳，格式 `YYYYMMDD_HHMMSS`。
///
/// 只用标准库手算公历，不引入 chrono：这里只需要「人类可读且可排序」，
/// 不需要时区或闰秒。系统时钟异常时回退到 epoch，仍是一个合法文件名。
fn unix_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}_{:02}{:02}{:02}",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// 把自 1970-01-01 起的天数转成公历 `(年, 月, 日)`。
///
/// Howard Hinnant 的 civil_from_days 算法，纯整数、无依赖。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 把 `db` 备份到 `dest`。
///
/// `dest` 已存在时**拒绝覆盖** —— 备份文件被静默覆盖等于没有备份。
/// `backup_path_for` 已保证每次生成新文件名，走到这里通常意味着同一秒内
/// 重复调用，或调用方显式传了 `--backup-to` 指向已有文件；两种都该拒绝。
pub fn backup(db: &Connection, dest: &Path) -> Result<PathBuf> {
    if dest.exists() {
        return Err(aa_session_core::Error::InvalidArgument(format!(
            "备份文件已存在：{}（不会覆盖，请换一个路径）",
            dest.display()
        )));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| aa_session_core::Error::Io {
            path: parent.display().to_string(),
            cause: e.to_string(),
        })?;
    }

    // VACUUM INTO 的目标不能是绑定参数（SQLite 语法限制），只能拼进 SQL，
    // 所以这里对单引号做转义。路径来自我们自己生成的文件名，不来自用户输入。
    let sql = format!(
        "VACUUM INTO '{}'",
        dest.display().to_string().replace('\'', "''")
    );
    db.execute_batch(&sql)
        .map_err(|e| aa_session_core::Error::Io {
            path: dest.display().to_string(),
            cause: format!("VACUUM INTO 失败：{e}"),
        })?;

    Ok(dest.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_path_has_timestamp_suffix() {
        let p = backup_path_for(Path::new("/data/opencode.db"));
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("opencode.db.aa_session_backup."), "{name}");
        assert!(name.ends_with(".sqlite"), "{name}");
        // 中间是 YYYYMMDD_HHMMSS
        let mid = name
            .strip_prefix("opencode.db.aa_session_backup.")
            .unwrap()
            .strip_suffix(".sqlite")
            .unwrap();
        assert_eq!(mid.len(), 15, "{mid}");
        assert_eq!(mid.as_bytes()[8], b'_');
        assert!(mid[..8].bytes().all(|b| b.is_ascii_digit()));
        assert!(mid[9..].bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(p.parent(), Some(Path::new("/data")));
    }

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        assert_eq!(civil_from_days(20_454), (2026, 1, 1));
        assert_eq!(civil_from_days(20_767), (2026, 11, 10));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn backup_refuses_existing_file() {
        let dir =
            std::env::temp_dir().join(format!("aa_session_backup_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join("already.sqlite");
        std::fs::write(&dest, b"old").unwrap();
        let db = Connection::open_in_memory().unwrap();
        let err = backup(&db, &dest).unwrap_err();
        assert!(err.to_string().contains("备份文件已存在"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
