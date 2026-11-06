//! 跨平台的家目录与 SQLite URI 拼装。
//!
//! 平台差异**只允许**集中在这里：Unix 上的行为与以前逐字节一致，Windows
//! 上的差异（没有 `HOME`、`canonicalize()` 返回 verbatim 路径）在这里消化掉，
//! 不散落到各 source。

use std::path::{Path, PathBuf};

/// 当前用户的家目录。
///
/// - Unix：`$HOME` —— XDG 规范的全部默认值都以它为前提。
/// - Windows：`%USERPROFILE%`。`HOME` 只在 Git Bash / MSYS 之类环境里存在，
///   原生 PowerShell / cmd 里没有。
///
/// 两个都取不到时返回 `None`，**绝不**退回空路径：空路径 join 出的是
/// `.local/share/opencode` 这种相对路径，会把「不知道家目录在哪」伪装成
/// 「库不存在」，正是这次 Windows 上误报的根源。
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    const KEYS: &[&str] = &["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    const KEYS: &[&str] = &["HOME"];

    KEYS.iter()
        .copied()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|path| !path.as_os_str().is_empty())
}

/// 生成 SQLite 的 `file:` URI。
///
/// Unix 上就是 `file:<路径>`，与以前一致。
///
/// Windows 上 `canonicalize()` 返回的是 `\\?\C:\...` 形式：带 verbatim 前缀、
/// 带反斜杠。直接塞进 `file:` URI 会被 SQLite 判为非法路径，报
/// `unable to open database file`。所以 Windows 分支先剥掉 `\\?\`，再把反斜杠
/// 换成正斜杠——SQLite 只认 URI 风格的 `/`。
pub fn sqlite_file_uri(path: &Path, read_only: bool) -> String {
    #[cfg(windows)]
    let path_text = {
        let text = path.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(text.as_ref());
        text.replace('\\', "/")
    };
    #[cfg(not(windows))]
    let path_text = path.display().to_string();

    if read_only {
        format!("file:{path_text}?mode=ro")
    } else {
        format!("file:{path_text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn uri_has_no_windows_artifacts() {
        let uri = sqlite_file_uri(
            Path::new(r"\\?\C:\Users\aa\.local\share\opencode\opencode.db"),
            false,
        );
        assert!(!uri.contains('\\'), "URI 里不应出现反斜杠：{uri}");
        assert!(
            !uri.contains(r"\\?\"),
            "URI 里不应出现 verbatim 前缀：{uri}"
        );
        assert!(uri.starts_with("file:"), "{uri}");
    }

    #[test]
    fn read_only_uri_keeps_mode_ro() {
        let uri = sqlite_file_uri(
            Path::new("/home/aa/.local/share/opencode/opencode.db"),
            true,
        );
        assert!(uri.ends_with("?mode=ro"), "{uri}");
    }
}
