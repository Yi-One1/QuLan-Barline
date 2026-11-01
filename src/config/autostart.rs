//! 开机自启（PRD P1-06，`ARCHITECTURE.md` §14.1 / D8）。
//!
//! 一律走 Win32 Registry API —— **绝不调用 `reg.exe`**
//! （本机安全策略禁用 `reg.exe`，且子进程方式本身也不该用在常驻程序里）。
//!
//! 键：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，值名 `Barline`，
//! 值内容为 `"<exe 绝对路径>"`（带引号，路径含空格也安全）。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Barline");

/// RAII：保证 `RegCloseKey` 一定被调用。
struct KeyGuard(HKEY);

impl Drop for KeyGuard {
    fn drop(&mut self) {
        // SAFETY: 句柄来自成功的 RegOpenKeyExW，且本类型不 Clone/Copy，只关闭一次。
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open_run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<KeyGuard> {
    let mut key = HKEY::default();
    // SAFETY: 子键名是静态 NUL 结尾宽字符串；key 是本地栈变量，写出范围合法。
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, access, &mut key) };
    if rc == ERROR_SUCCESS {
        Some(KeyGuard(key))
    } else {
        crate::log_warn!("RegOpenKeyExW(Run) 失败，Win32 错误码 {}", rc.0);
        None
    }
}

/// 当前是否已启用自启。
///
/// 不只判断「值存在」，还要把值内容与当前 exe 路径比对——用户可能把 exe 挪了位置，
/// 那时旧的 Run 值其实已经失效（PRD P1-06 要求校验真实状态）。
pub fn is_enabled() -> bool {
    let Some(key) = open_run_key(KEY_QUERY_VALUE) else {
        return false;
    };

    // 先问需要多大缓冲
    let mut size: u32 = 0;
    // SAFETY: lpdata 传 None 只查询大小；lpcbdata 指向本地变量。
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            VALUE_NAME,
            None,
            None,
            None,
            Some(&mut size as *mut u32),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND {
        return false;
    }
    if rc != ERROR_SUCCESS || size == 0 {
        return false;
    }

    let mut buf = vec![0u8; size as usize];
    let mut kind = REG_SZ;
    // SAFETY: buf 长度 ≥ size，RegQueryValueExW 最多写 size 字节，不会越界。
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            VALUE_NAME,
            None,
            Some(&mut kind as *mut _),
            Some(buf.as_mut_ptr()),
            Some(&mut size as *mut u32),
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }

    // 值内容是 UTF-16LE 字节流
    let words: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let stored = crate::platform::wide::from_wide(&words);
    let stored = stored.trim().trim_matches('"');

    match std::env::current_exe() {
        Ok(exe) => {
            let exe = exe.to_string_lossy();
            let same = stored.eq_ignore_ascii_case(&exe);
            if !same {
                crate::log_info!("自启项存在但指向别处：registry=\"{stored}\" 当前=\"{exe}\"");
            }
            same
        }
        Err(_) => !stored.is_empty(),
    }
}

/// 启用 / 停用自启。返回是否成功。
pub fn set_enabled(enabled: bool) -> bool {
    if !enabled {
        return disable();
    }
    enable()
}

fn enable() -> bool {
    let value = current_exe_quoted();
    if value.is_empty() {
        crate::log_err!("取当前 exe 路径失败，无法设置自启");
        return false;
    }
    let wide = crate::platform::wide::to_wide(&value);
    let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();

    let Some(key) = open_run_key(KEY_SET_VALUE) else {
        return false;
    };
    // SAFETY: 值名静态合法；bytes 是宽字符串的 LE 字节视图，长度 = 2*wide.len()。
    let rc: WIN32_ERROR = unsafe {
        RegSetValueExW(key.0, VALUE_NAME, None, REG_SZ, Some(bytes.as_slice()))
    };
    if rc == ERROR_SUCCESS {
        crate::log_info!("已启用开机自启：{value}");
        true
    } else {
        crate::log_err!("RegSetValueExW 失败，Win32 错误码 {}", rc.0);
        false
    }
}

fn disable() -> bool {
    let Some(key) = open_run_key(KEY_SET_VALUE) else {
        return false;
    };
    // SAFETY: 值名静态合法；删除不存在的值只会返回 ERROR_FILE_NOT_FOUND，视为成功。
    let rc = unsafe { RegDeleteValueW(key.0, VALUE_NAME) };
    if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
        crate::log_info!("已停用开机自启");
        true
    } else {
        crate::log_err!("RegDeleteValueW 失败，Win32 错误码 {}", rc.0);
        false
    }
}

/// 便于测试 / 日志：自启项应写入的 exe 路径（带引号）。
pub fn current_exe_quoted() -> String {
    match std::env::current_exe() {
        Ok(p) => format!("\"{}\"", p.display()),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_exe_path_is_quoted() {
        let s = current_exe_quoted();
        // 测试进程一定有路径
        assert!(s.starts_with('"'));
        assert!(s.ends_with('"'));
    }

    #[test]
    fn read_only_query_does_not_panic() {
        // 只读查询，不改变系统状态。结果取决于机器，不做断言。
        let _ = is_enabled();
    }
}
