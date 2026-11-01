//! 平台层：最底层的 Win32 封装，只被依赖、不依赖任何业务模块（ARCHITECTURE.md 附录 A）。

pub mod com;
pub mod dpi;
pub mod single_instance;
pub mod wake;
pub mod wide;

use std::path::PathBuf;

use windows::core::PWSTR;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG};

/// `%LOCALAPPDATA%`。走 `SHGetKnownFolderPath` 而不是读环境变量（§14.1：环境变量可能被篡改）。
///
/// 失败时依次退化：`%LOCALAPPDATA%` 环境变量 → 系统临时目录。永不返回空路径。
pub fn local_app_data_dir() -> PathBuf {
    // SAFETY: SHGetKnownFolderPath 返回由 CoTaskMemAlloc 分配的 NUL 结尾宽字符串，
    // 我们把它复制成 String 后立刻 CoTaskMemFree，所有权完整交还。
    let from_api = unsafe {
        match SHGetKnownFolderPath(&FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG(0), None) {
            Ok(pwstr) => {
                let s = pwstr_to_string(pwstr);
                CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void));
                Some(PathBuf::from(s))
            }
            Err(_) => None,
        }
    };
    if let Some(p) = from_api {
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    std::env::temp_dir()
}

/// `%LOCALAPPDATA%\Barline`。日志、配置、歌词缓存都在这里。
pub fn barline_dir() -> PathBuf {
    local_app_data_dir().join("Barline")
}

/// 把 PWSTR 读成 String（读到 NUL 为止），不持有所有权。
fn pwstr_to_string(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: 调用方保证 p 指向一段以 NUL 结尾的合法 UTF-16 序列
    // （SHGetKnownFolderPath 的契约）。
    let mut len = 0usize;
    unsafe {
        while *p.0.add(len) != 0 {
            len += 1;
            // 防御性上限：路径不可能超过 32K 个 UTF-16 码元
            if len > 32768 {
                break;
            }
        }
        let slice = core::slice::from_raw_parts(p.0, len);
        String::from_utf16_lossy(slice)
    }
}
