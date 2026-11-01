//! 点击封面 → 激活播放器主窗口（`ARCHITECTURE.md` §14.5 / R3 拍板 / T09 第 3 条）。
//!
//! ## 两级策略（按序尝试，全程 ≤ 1s；v1.1.16 起**没有网页回退**）
//!
//! ```text
//! a. 拿 PID（候选列表）：
//!      ① session.process_id（WASAPI 兜底时直接有）
//!      ② 枚举音频会话，IAudioSessionControl2::GetProcessId，
//!         按「进程名 / AUMID 关键字」匹配（SMTC 不给 PID 时的主路径）
//!      ③ CreateToolhelp32Snapshot 按进程名反查
//!    → EnumWindows 找该 PID 的「最像主窗口」的顶层窗口——
//!      **隐藏窗口也参与**（托盘最小化 = 窗口被 Hide），可见窗口优先打分
//!    → SW_RESTORE / SW_SHOW 恢复显示 + SetForegroundWindow（AttachThreadInput 重试 3 次）
//! b. 找不到窗口 → IApplicationActivationManager::ActivateApplication(aumid)
//!      （打包应用 / UWP 的正规激活通道）
//! ```
//!
//! v1.1.16（问题 7）：**网页回退（打开官网 / Bing 搜索）已删除**——用户实测
//! 网易云托盘最小化时被带到 music.163.com、汽水被带到「未知播放器 官网」搜索页，
//! 都是回退路径干的。找不到窗口就如实记日志并放弃，绝不打开浏览器。
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
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, IsWindowEnabled, SetActiveWindow, SetFocus,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SetForegroundWindow, ShowWindow, SwitchToThisWindow, GWL_EXSTYLE, GW_OWNER, SW_RESTORE,
    SW_SHOWNORMAL, WS_EX_TOOLWINDOW,
};

use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    MMDeviceEnumerator,
};

use crate::media::MediaSessionInfo;

/// 激活结果（用于日志与验收）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// 找到了主窗口并成功置前
    Window,
    /// 走了 AUMID 激活
    Aumid,
    /// 全部失败
    Failed,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Window => "window",
            Activation::Aumid => "aumid",
            Activation::Failed => "failed",
        }
    }
}

/// 点击封面：把当前播放器拉到前台。永不 panic、永不返回错误。
///
/// v1.1.16（问题 7）：**没有网页回退**——找不到窗口就记日志放弃，
/// 绝不打开浏览器（旧回退会把网易云带去官网、未知播放器带去 Bing 搜索）。
pub fn activate_player(session: &MediaSessionInfo) -> Activation {
    let pids = resolve_pids(session);
    crate::log_info!(
        "激活播放器：app={} aumid={} process={} pids={:?}",
        session.app_name,
        session.aumid,
        session.process_name,
        pids
    );

    // ---- a. 按候选 PID 逐个找主窗口（含托盘最小化的隐藏窗口）----
    for pid in &pids {
        if let Some(hwnd) = find_main_window(*pid) {
            if show_and_foreground(hwnd) {
                crate::log_info!("已激活播放器窗口 hwnd=0x{:X} pid={pid}", hwnd.0 as usize);
                return Activation::Window;
            }
            crate::log_debug!("pid={pid} 找到窗口但置前失败，继续下一个候选");
        }
    }
    crate::log_debug!("候选进程都没有可激活的主窗口，继续尝试 AUMID");

    // ---- b. AUMID 激活（打包应用 / UWP 的正规通道）----
    if !session.aumid.is_empty() && activate_by_aumid(&session.aumid) {
        crate::log_info!("已通过 AUMID 激活：{}", session.aumid);
        return Activation::Aumid;
    }

    crate::log_warn!(
        "未能激活播放器（找不到窗口，AUMID 激活也不可用）；不再打开网页回退"
    );
    Activation::Failed
}

// ---------------------------------------------------------------- PID 解析

/// 解析播放器候选进程 id 列表（v1.1.15 问题 7：多进程应用友好）。
///
/// 三级来源按序合并去重：会话自带 → 音频会话枚举 → Toolhelp 进程快照。
/// 顺序即优先级：SMTC 自带的 PID 最可信，音频会话次之，快照兜底。
fn resolve_pids(session: &MediaSessionInfo) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let push = |pid: u32, out: &mut Vec<u32>| {
        if pid != 0 && pid != std::process::id() && !out.contains(&pid) {
            out.push(pid);
        }
    };
    push(session.process_id, &mut out);
    for pid in pids_from_audio_sessions(&session.process_name, &session.aumid) {
        push(pid, &mut out);
    }
    if !session.process_name.is_empty() {
        for pid in pids_from_snapshot(&session.process_name) {
            push(pid, &mut out);
        }
    }
    out
}

/// 枚举音频会话，返回与进程名 / AUMID 匹配的**全部** PID。
///
/// 这是 T09 指定的主路径：`IAudioSessionControl2::GetProcessId`。
/// v1.1.15：改用 [`crate::media::session_matches`]（含 player_map 反查），
/// 并收集全部命中——浏览器/多进程播放器的音频会话不止一个。
fn pids_from_audio_sessions(want_process: &str, want_aumid: &str) -> Vec<u32> {
    let mut out = Vec::new();
    if want_process.trim().is_empty() && want_aumid.trim().is_empty() {
        return out;
    }

    // SAFETY: 所有 COM 对象都在本函数作用域内释放；CLSID 是系统注册的进程内组件。
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            match CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) {
                Ok(e) => e,
                Err(e) => {
                    crate::log_debug!("IMMDeviceEnumerator 创建失败：{e}");
                    return out;
                }
            };
        let device = match enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) {
            Ok(d) => d,
            Err(e) => {
                crate::log_debug!("默认渲染设备不可用：{e}");
                return out;
            }
        };
        let mgr: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None) {
            Ok(m) => m,
            Err(e) => {
                crate::log_debug!("IAudioSessionManager2 激活失败：{e}");
                return out;
            }
        };
        let sessions = match mgr.GetSessionEnumerator() {
            Ok(s) => s,
            Err(e) => {
                crate::log_debug!("会话枚举器获取失败：{e}");
                return out;
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
            let name = process_image_name(pid);
            if crate::media::session_matches(&name, want_process, want_aumid) {
                crate::log_debug!("音频会话命中：pid={pid} name={name}");
                out.push(pid);
            }
        }
    }
    out
}

/// 取进程可执行文件名（小写）。v1.1.15 起 `wasapi` 的会话音量路径共用。
pub(crate) fn process_image_name(pid: u32) -> String {
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

/// Toolhelp 快照按进程名反查**全部** PID（最后一道兜底；v1.1.15 多进程应用友好）。
fn pids_from_snapshot(name: &str) -> Vec<u32> {
    let want = name.to_ascii_lowercase();
    let mut out = Vec::new();
    // SAFETY: 快照句柄在函数末尾关闭；PROCESSENTRY32W.dwSize 必须先填。
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(e) => {
                crate::log_debug!("CreateToolhelp32Snapshot 失败：{e}");
                return out;
            }
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: core::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
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
                    out.push(entry.th32ProcessID);
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

// ---------------------------------------------------------------- 窗口查找

/// `EnumWindows` 的回调上下文。
struct FindCtx {
    pid: u32,
    best: Option<(HWND, i32)>,
}

/// 找该 PID 的「最像主窗口」的顶层窗口（v1.1.16：**隐藏窗口也参与**）。
///
/// 网易云等播放器「最小化到托盘」= 窗口被 `Hide()`——v1.1.15 只看可见窗口，
/// 托盘态必然扑空然后掉进（已删除的）网页回退。现在的打分：
/// 可见 +2、标题非空 +4、面积 ≥ 200×120 +2、有 owner（对话框）-3；
/// 隐藏窗口也能靠标题 + 面积得分，可见窗口天然排在前面。
///
/// 排除项：工具窗口（`WS_EX_TOOLWINDOW`，托盘辅助窗）、DWM cloaked 窗口
/// （挂起的 UWP 宿主帧）、我们自己的窗口。
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
    unsafe {
        // 工具窗口 / 消息窗口不配当主窗口（GetWindowLongW 返回 i32，标志位 0x80 安全）
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        if ex & (WS_EX_TOOLWINDOW.0 as i32) != 0 {
            return windows::core::BOOL(1);
        }
        // DWM cloaked（挂起的 UWP 宿主帧）：看不见的「可见」窗口，跳过
        let mut cloaked: u32 = 0;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            core::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return windows::core::BOOL(1);
        }
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

    let mut score = 0i32;
    // SAFETY: 同上。
    unsafe {
        if IsWindowVisible(hwnd).as_bool() {
            score += 2;
        }
        let title_len = GetWindowTextLengthW(hwnd);
        if title_len > 0 {
            score += 4;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            if w >= 200 && h >= 120 {
                score += 2;
            }
        }
        // 有 owner 的通常是对话框 / 提示窗，降权
        if GetWindow(hwnd, GW_OWNER).is_ok() {
            score -= 3;
        }
    }

    match ctx.best {
        Some((_, best_score)) if best_score >= score => {}
        _ => ctx.best = Some((hwnd, score)),
    }
    windows::core::BOOL(1)
}

/// 恢复 / 显示窗口后再抢前台（v1.1.18 重写）。
///
/// v1.1.17 实测缺陷：汽水托盘隐藏窗口用 `SW_SHOW` 显示——它**只显示不激活**，
/// CEF/Electron 应用收不到 `WM_ACTIVATE` 就会挂起输入层，表现为「窗口能拖动
/// （NC 区系统处理）但内容全部不可点击」。修复：
/// 1. 隐藏窗口改用 `SW_RESTORE`（带激活语义，任务栏点击同款）；
/// 2. 显示后检查 `IsWindowEnabled`——应用 hide 时可能禁用了窗口输入，
///    外部显示必须恢复（禁用窗口的症状同样是「能拖不能点」）；
/// 3. [`force_foreground`] 失败时用 `SwitchToThisWindow` 兜底（任务栏切窗口
///    同款 API，绕前台锁且完全不碰输入队列）。
fn show_and_foreground(hwnd: HWND) -> bool {
    // SAFETY: 标准窗口操作。
    unsafe {
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            // 最小化 / 托盘隐藏：SW_RESTORE = 激活 + 显示 + 恢复（而不是只显示）
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !IsWindowEnabled(hwnd).as_bool() {
            crate::log_info!("目标窗口被禁用输入（应用托盘化常见），恢复 EnableWindow");
            let _ = EnableWindow(hwnd, true);
        }
    }
    if force_foreground(hwnd) {
        // SetForegroundWindow 返回 true 只表示「请求被接受」——记录前台是否
        // 真的切过去了（v1.1.17 汽水卡死排查经验：返回 true ≠ 激活生效）
        // SAFETY: 纯查询。
        if unsafe { GetForegroundWindow() } != hwnd {
            crate::log_warn!("置前返回成功但前台并非目标窗口，激活可能未生效");
        }
        return true;
    }
    // 兜底：任务栏切窗口同款，绕前台锁且不污染输入队列
    // SAFETY: 标准窗口操作；fAltTab=false 不触发任务切换 UI。
    unsafe {
        SwitchToThisWindow(hwnd, false);
    }
    // SAFETY: 纯查询。
    unsafe { GetForegroundWindow() == hwnd }
}

/// 把窗口拉到前台。失败时用 `AttachThreadInput` 绕过前台锁，最多 3 次。
///
/// 显示 / 恢复已由 [`show_and_foreground`] 处理——这里只管激活，避免对已
/// 可见窗口重复 `SW_RESTORE` 造成闪屏（v1.1.18）。
fn force_foreground(hwnd: HWND) -> bool {
    // SAFETY: 标准窗口操作。
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return true;
        }

        let target_tid = GetWindowThreadProcessId(hwnd, None);
        let self_tid = windows::Win32::System::Threading::GetCurrentThreadId();
        for attempt in 0..3 {
            if target_tid != 0 && target_tid != self_tid {
                let _ = AttachThreadInput(self_tid, target_tid, true);
            }
            // 附加输入队列期间把焦点显式还给目标窗口——前台锁下仅
            // SetForegroundWindow 可能不够，SetFocus/SetActiveWindow 补齐
            let _ = SetFocus(Some(hwnd));
            let _ = SetActiveWindow(hwnd);
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
    let dir = crate::platform::data_dir();
    open_url(&dir.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_labels_are_stable() {
        assert_eq!(Activation::Window.as_str(), "window");
        assert_eq!(Activation::Failed.as_str(), "failed");
    }
}
