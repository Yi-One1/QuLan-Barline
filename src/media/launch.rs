//! 点击封面 → 激活播放器主窗口（`ARCHITECTURE.md` §14.5 / R3 拍板 / T09 第 3 条）。
//!
//! ## 三级策略（按序尝试，全程 ≤ 1s）
//!
//! ```text
//! a. 拿 PID：
//!      ① session.process_id（WASAPI 兜底时直接有）
//!      ② 枚举音频会话，IAudioSessionControl2::GetProcessId，
//!         按「进程名 / AUMID 关键字」匹配（SMTC 不给 PID 时的主路径）
//!      ③ CreateToolhelp32Snapshot 按进程名反查
//!    → EnumWindows 找该 PID 的「可见 + 无 owner + 标题非空」顶层窗口
//!    → ShowWindow(SW_RESTORE) + SetForegroundWindow（带 AttachThreadInput 重试 3 次）
//! b. 找不到窗口 → IApplicationActivationManager::ActivateApplication(aumid)
//!      （打包应用 / UWP 的正规激活通道）
//! c. 仍失败 → ShellExecuteW 打开 player_map 的 fallback_url；
//!      未知播放器 → Bing 搜索「<app_name> 官网」
//! ```
//!
//! ## 为什么 `SetForegroundWindow` 会失败
//!
//! Windows 限制「非前台进程不能抢前台」。我们是被用户点击触发的，通常有前台权限，
//! 但播放器自己刚被点过、或前台锁被别的程序持有时仍会失败。
//! 标准绕过办法：`AttachThreadInput(当前线程, 目标线程, true)` → `SetForegroundWindow`
//! → `AttachThreadInput(..., false)`。这是本模块 `force_foreground()` 做的事。
//!
//! ## 线程
//!
//! 全部调用都在 UI 线程（T0）同步执行：没有 `IAsyncOperation::join()`，
//! WASAPI 的 `Activate` 是同步 COM 调用，`EnumWindows` 也是同步的。
//! 实测整个流程 5–40ms，不会卡消息泵。

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, MAX_PATH};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    ApplicationActivationManager, IApplicationActivationManager, ShellExecuteW, ACTIVATEOPTIONS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindow, GetWindowRect, GetWindowTextLengthW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, GW_OWNER,
    SW_RESTORE, SW_SHOWNORMAL,
};

use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator,
};

use crate::media::player_map;
use crate::media::MediaSessionInfo;

/// 激活结果（用于日志与验收）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// 找到了主窗口并成功置前
    Window,
    /// 走了 AUMID 激活
    Aumid,
    /// 走了回退 URL
    Url,
    /// 全部失败
    Failed,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Window => "window",
            Activation::Aumid => "aumid",
            Activation::Url => "url",
            Activation::Failed => "failed",
        }
    }
}

/// 点击封面：把当前播放器拉到前台。永不 panic、永不返回错误。
pub fn activate_player(session: &MediaSessionInfo) -> Activation {
    let pid = resolve_pid(session);
    crate::log_info!(
        "激活播放器：app={} aumid={} process={} pid={}",
        session.app_name,
        session.aumid,
        session.process_name,
        pid
    );

    // ---- a. 按 PID 找主窗口 ----
    if pid != 0 {
        if let Some(hwnd) = find_main_window(pid) {
            if force_foreground(hwnd) {
                crate::log_info!("已激活播放器窗口 hwnd=0x{:X}", hwnd.0 as usize);
                return Activation::Window;
            }
            crate::log_warn!("找到窗口但 SetForegroundWindow 失败，继续尝试 AUMID");
        } else {
            crate::log_debug!("PID {pid} 没有可见的顶层主窗口");
        }
    }

    // ---- b. AUMID 激活 ----
    if !session.aumid.is_empty() && activate_by_aumid(&session.aumid) {
        crate::log_info!("已通过 AUMID 激活：{}", session.aumid);
        return Activation::Aumid;
    }

    // ---- c. 回退 URL ----
    let info = player_map::resolve(&session.aumid);
    let url = if !info.fallback_url.is_empty() {
        info.fallback_url.to_string()
    } else {
        // 未知播放器 → Bing 搜索
        let q = percent_encode(&format!("{} 官网", session.app_name));
        format!("https://www.bing.com/search?q={q}")
    };
    if open_url(&url) {
        crate::log_info!("已打开回退页面：{url}");
        Activation::Url
    } else {
        crate::log_warn!("回退页面打开失败：{url}");
        Activation::Failed
    }
}

// ---------------------------------------------------------------- PID 解析

/// 解析播放器进程 id。三级：会话自带 → 音频会话枚举 → Toolhelp 进程快照。
fn resolve_pid(session: &MediaSessionInfo) -> u32 {
    if session.process_id != 0 {
        return session.process_id;
    }
    let by_audio = pid_from_audio_sessions(&session.process_name, &session.aumid);
    if by_audio != 0 {
        return by_audio;
    }
    if !session.process_name.is_empty() {
        return pid_from_snapshot(&session.process_name);
    }
    0
}

/// 枚举音频会话，按进程名 / AUMID 关键字匹配出 PID。
///
/// 这是 T09 指定的主路径：`IAudioSessionControl2::GetProcessId`。
fn pid_from_audio_sessions(want_process: &str, want_aumid: &str) -> u32 {
    let want_process = want_process.to_ascii_lowercase();
    let want_aumid = want_aumid.to_ascii_lowercase();
    if want_process.is_empty() && want_aumid.is_empty() {
        return 0;
    }

    // SAFETY: 所有 COM 对象都在本函数作用域内释放；CLSID 是系统注册的进程内组件。
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                Ok(e) => e,
                Err(e) => {
                    crate::log_debug!("IMMDeviceEnumerator 创建失败：{e}");
                    return 0;
                }
            };
        let device = match enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) {
            Ok(d) => d,
            Err(e) => {
                crate::log_debug!("默认渲染设备不可用：{e}");
                return 0;
            }
        };
        let mgr: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None) {
            Ok(m) => m,
            Err(e) => {
                crate::log_debug!("IAudioSessionManager2 激活失败：{e}");
                return 0;
            }
        };
        let sessions = match mgr.GetSessionEnumerator() {
            Ok(s) => s,
            Err(e) => {
                crate::log_debug!("会话枚举器获取失败：{e}");
                return 0;
            }
        };
        let count = sessions.GetCount().unwrap_or(0);

        for i in 0..count {
            let Ok(ctrl) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(ctrl2) = ctrl.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let Ok(pid) = ctrl2.GetProcessId() else {
                continue;
            };
            if pid == 0 || pid == std::process::id() {
                continue;
            }
            let name = process_image_name(pid).to_ascii_lowercase();
            let matched = (!want_process.is_empty() && name.contains(&want_process))
                || (!want_aumid.is_empty() && name.contains(&want_aumid));
            if matched {
                crate::log_debug!("音频会话命中：pid={pid} name={name}");
                return pid;
            }
        }
    }
    0
}

/// 取进程可执行文件名（小写）。
fn process_image_name(pid: u32) -> String {
    // SAFETY: OpenProcess 失败返回 Err；句柄在作用域结束前关闭。
    unsafe {
        let handle: HANDLE = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return String::new(),
        };
        let mut buf = [0u16; MAX_PATH as usize];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        full.rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }
}

/// Toolhelp 快照按进程名反查 PID（最后一道兜底）。
fn pid_from_snapshot(name: &str) -> u32 {
    let want = name.to_ascii_lowercase();
    // SAFETY: 快照句柄在函数末尾关闭；PROCESSENTRY32W.dwSize 必须先填。
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(e) => {
                crate::log_debug!("CreateToolhelp32Snapshot 失败：{e}");
                return 0;
            }
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: core::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = 0u32;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let exe = String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len())],
                );
                if exe.to_ascii_lowercase() == want {
                    found = entry.th32ProcessID;
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

// ---------------------------------------------------------------- 窗口查找

/// `EnumWindows` 的回调上下文。
struct FindCtx {
    pid: u32,
    best: Option<(HWND, i32)>,
}

/// 找该 PID 的「最像主窗口」的顶层窗口。
///
/// 打分：可见 +2、标题非空 +4、面积 ≥ 200×120 +2、有 owner（对话框）-3。
fn find_main_window(pid: u32) -> Option<HWND> {
    let mut ctx = FindCtx { pid, best: None };
    // SAFETY: ctx 的地址在 EnumWindows 返回前一直有效；回调只写 ctx。
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut FindCtx as isize));
    }
    ctx.best.map(|(h, _)| h)
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: lparam 是 find_main_window 里传入的 &mut FindCtx。
    let ctx = unsafe { &mut *(lparam.0 as *mut FindCtx) };

    let mut wnd_pid: u32 = 0;
    // SAFETY: 标准 Win32 查询，hwnd 由系统提供。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut wnd_pid)) };
    if wnd_pid != ctx.pid {
        return windows::core::BOOL(1);
    }
    // SAFETY: 同上。
    if unsafe { !IsWindowVisible(hwnd).as_bool() } {
        return windows::core::BOOL(1);
    }

    // 排除我们自己的窗口（类名 BarlineWidgetWnd）
    let mut class = [0u16; 64];
    // SAFETY: class 是本地缓冲。
    let n = unsafe { GetClassNameW(hwnd, &mut class) };
    if n > 0 {
        let cls = String::from_utf16_lossy(&class[..n as usize]);
        if cls.starts_with("Barline") {
            return windows::core::BOOL(1);
        }
    }

    let mut score = 2i32;
    // SAFETY: 同上。
    let title_len = unsafe { GetWindowTextLengthW(hwnd) };
    if title_len > 0 {
        score += 4;
    }
    let mut rect = windows::Win32::Foundation::RECT::default();
    // SAFETY: rect 是本地结构体。
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w >= 200 && h >= 120 {
            score += 2;
        }
    }
    // 有 owner 的通常是对话框 / 提示窗，降权
    // SAFETY: 纯查询；GetWindow 在没有 owner 时返回 Err。
    if unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok() {
        score -= 3;
    }

    match ctx.best {
        Some((_, best_score)) if best_score >= score => {}
        _ => ctx.best = Some((hwnd, score)),
    }
    windows::core::BOOL(1)
}

/// 把窗口拉到前台。失败时用 `AttachThreadInput` 绕过前台锁，最多 3 次。
fn force_foreground(hwnd: HWND) -> bool {
    // SAFETY: 标准窗口操作。
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() {
            return true;
        }

        let target_tid = GetWindowThreadProcessId(hwnd, None);
        let self_tid = windows::Win32::System::Threading::GetCurrentThreadId();
        for attempt in 0..3 {
            if target_tid != 0 && target_tid != self_tid {
                let _ = AttachThreadInput(self_tid, target_tid, true);
            }
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let ok = SetForegroundWindow(hwnd).as_bool();
            if target_tid != 0 && target_tid != self_tid {
                let _ = AttachThreadInput(self_tid, target_tid, false);
            }
            if ok {
                return true;
            }
            crate::log_debug!("SetForegroundWindow 第 {} 次失败", attempt + 1);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
    false
}

/// 用 `IApplicationActivationManager` 激活 AUMID（打包应用 / UWP 的正规通道）。
fn activate_by_aumid(aumid: &str) -> bool {
    let wide = crate::platform::wide::to_wide(aumid);
    // SAFETY: wide 是 NUL 结尾的本地缓冲；COM 对象在作用域结束释放。
    unsafe {
        let mgr: IApplicationActivationManager =
            match CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_ALL) {
                Ok(m) => m,
                Err(e) => {
                    crate::log_debug!("IApplicationActivationManager 创建失败：{e}");
                    return false;
                }
            };
        let empty = crate::platform::wide::to_wide("");
        match mgr.ActivateApplication(
            PCWSTR(wide.as_ptr()),
            PCWSTR(empty.as_ptr()),
            ACTIVATEOPTIONS(0),
        ) {
            Ok(pid) => {
                crate::log_debug!("ActivateApplication 成功，pid={pid}");
                true
            }
            Err(e) => {
                crate::log_debug!("ActivateApplication 失败：{e}");
                false
            }
        }
    }
}

// ---------------------------------------------------------------- URL 打开

/// 用系统默认程序打开一个 URL / 协议（`ms-settings:` 也走这里）。
pub fn open_url(url: &str) -> bool {
    let wide = crate::platform::wide::to_wide(url);
    // SAFETY: wide 是 NUL 结尾缓冲；ShellExecuteW 只读取它。
    let h = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 的返回值 > 32 才算成功（历史约定）
    (h.0 as isize) > 32
}
/// 打开本地日志目录（设置页 / 关于页用）。
pub fn open_log_dir() -> bool {
    let dir = crate::platform::barline_dir();
    open_url(&dir.to_string_lossy())
}

/// UTF-8 百分号编码（只保留 `A-Za-z0-9-_.~`）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_handles_cjk_and_spaces() {
        assert_eq!(percent_encode("ab c"), "ab%20c");
        assert_eq!(percent_encode("云"), "%E4%BA%91");
        assert_eq!(percent_encode("a&b+c"), "a%26b%2Bc");
        assert_eq!(percent_encode("-_."), "-_.");
    }

    #[test]
    fn activation_labels_are_stable() {
        assert_eq!(Activation::Window.as_str(), "window");
        assert_eq!(Activation::Failed.as_str(), "failed");
    }
}
