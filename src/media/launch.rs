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
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
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
use core::time::Duration;

/// 激活结果（用于日志与验收）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// 找到了主窗口并成功置前
    Window,
    /// 通过模拟托盘图标点击恢复（CEF/Electron 托盘恢复型应用）
    Tray,
    /// 走了 AUMID 激活
    Aumid,
    /// 全部失败
    Failed,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Window => "window",
            Activation::Tray => "tray",
            Activation::Aumid => "aumid",
            Activation::Failed => "failed",
        }
    }
}

/// CEF/Electron「托盘恢复型」应用：窗口 Hide 到托盘后，**外部 `ShowWindow` +
/// `SetForegroundWindow` 无论怎么组合都会让 CEF 输入管线挂起**——窗口显示、
/// 甚至前台激活成功，但内容完全不可点击（`SendInput` 真实点击都无效），
/// 只有用应用自己的托盘点击路径恢复才正常。
///
/// 这类应用走「模拟托盘图标点击」通道（[`tray_click_activate`]）：
/// `Shell_NotifyIconGetRect` 精确定位**该进程注册的**托盘图标（按 hWnd+uID 匹配，
/// 不会误点别的图标），`SendInput` 发一次真实左键单击——与应用被用户亲手点击
/// 完全同源。v1.1.20 实测汽水音乐 3.1.x：恢复 + 前台 + 点击 × 验证全部正常。
const TRAY_RECOVERY_APPS: &[&str] = &["sodamusic.exe"];

/// 进程 / AUMID 是否命中托盘恢复型应用。
fn is_tray_recovery_app(process_name: &str, aumid: &str) -> bool {
    TRAY_RECOVERY_APPS
        .iter()
        .any(|n| process_name.eq_ignore_ascii_case(n))
        || aumid == "汽水音乐"
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

    // ---- 0. CEF/Electron 托盘恢复型应用（汽水）——用户裁决（2026-11-02）：搁置 ----
    // 实测结论：这类应用 Hide 到托盘后，**任何外部 ShowWindow 都会让 CEF 输入
    // 管线挂起**（窗口显示但内容不可点击，点 × 都无响应）；唯一可靠的恢复通道
    // 是模拟托盘图标点击（[`tray_click_activate`]，实现保留），但 SendInput 会
    // **抢鼠标**（光标被拉到托盘再回来），用户裁决不启用、以后再解决。
    // 现行策略：窗口可见（非托盘态）时正常前台激活；托盘隐藏态**不做任何
    // 激活**——不制造卡死窗口、不抢鼠标，由用户点汽水托盘自行恢复。
    let tray_recovery = is_tray_recovery_app(&session.process_name, &session.aumid)
        || pids.iter().any(|&pid| {
            let name = process_image_name(pid);
            is_tray_recovery_app(&name, "")
        });
    if tray_recovery {
        for pid in &pids {
            if let Some(hwnd) = find_main_window(*pid) {
                let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
                if visible && show_and_foreground(hwnd) {
                    crate::log_info!(
                        "已激活播放器窗口 hwnd=0x{:X} pid={pid}（托盘恢复型：可见窗口）",
                        hwnd.0 as usize
                    );
                    return Activation::Window;
                }
                if !visible {
                    crate::log_info!(
                        "汽水等托盘恢复型应用处于托盘隐藏态：激活暂禁用（缺陷搁置，待办 D），不制造卡死窗口"
                    );
                    return Activation::Failed;
                }
            }
        }
        crate::log_debug!("托盘恢复型应用未找到主窗口，继续窗口激活链");
    }

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

// ---------------------------------------------------------------- 托盘点击通道

/// [`top_level_windows_of`] 的 EnumWindows 回调上下文。
struct CollectCtx {
    pid: u32,
    out: Vec<HWND>,
}

/// 收集进程的**全部**顶层窗口（含隐藏；托盘图标宿主通常是个隐藏小窗）。
fn top_level_windows_of(pid: u32) -> Vec<HWND> {
    let mut ctx = CollectCtx { pid, out: Vec::new() };
    // SAFETY: ctx 地址在 EnumWindows 返回前有效；回调只写 ctx。
    unsafe {
        let _ = EnumWindows(Some(collect_proc), LPARAM(&mut ctx as *mut CollectCtx as isize));
    }
    ctx.out
}

unsafe extern "system" fn collect_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: lparam 是 top_level_windows_of 传入的 &mut CollectCtx。
    let ctx = unsafe { &mut *(lparam.0 as *mut CollectCtx) };
    let mut wnd_pid: u32 = 0;
    // SAFETY: 标准 Win32 查询。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut wnd_pid)) };
    if wnd_pid == ctx.pid {
        ctx.out.push(hwnd);
    }
    windows::core::BOOL(1)
}

/// 模拟「用户点击系统托盘图标」恢复主窗口（托盘恢复型应用的唯一可靠通道）。
///
/// 步骤：枚举进程全部窗口 → 逐个试 `Shell_NotifyIconGetRect`（hWnd+uID 二元组
/// 精确定位**该进程注册的**托盘图标，不会误点别人的）→ 记录光标位置 →
/// `SendInput` 真实左键单击 → 恢复光标 → 等待主窗口重新可见。
///
/// 返回恢复后的主窗口 hwnd。全程阻塞 ≤ ~3s（点击 ~0.3s + 等待上限 ~1.5s）。
///
/// **当前未启用**（用户裁决 2026-11-02）：SendInput 会把用户光标拉到托盘再
/// 恢复，「抢鼠标」不可接受。续修方向（待办 D）：① 逆向托盘回调消息号直接
/// `PostMessage`（零鼠标移动，Electron NotifyIconHost 的 WM_APP 消息）；②
/// 探测汽水 deep-link / second-instance 无重启唤起通道；③ 仅在用户光标静止
/// 时才模拟点击 + 光标恢复动画。实现经 diag 实测有效（恢复+点击×验证通过）。
#[allow(dead_code)]
fn tray_click_activate(pids: &[u32]) -> Option<HWND> {
    use windows::Win32::UI::Shell::{Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    // SAFETY: 本函数全部为标准 Win32 调用；本地缓冲在作用域内有效。
    unsafe {
        let (vx, vy) = (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
        );
        let (vw, vh) = (
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        );

        for pid in pids {
            for hwnd in top_level_windows_of(*pid) {
                for uid in 0..64u32 {
                    let ni = NOTIFYICONIDENTIFIER {
                        cbSize: core::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                        hWnd: hwnd,
                        uID: uid,
                        guidItem: windows::core::GUID::zeroed(),
                    };
                    let Ok(rect) = Shell_NotifyIconGetRect(&ni) else {
                        continue;
                    };
                    if rect.right <= rect.left || rect.bottom <= rect.top {
                        continue;
                    }

                    let cx = (rect.left + rect.right) / 2;
                    let cy = (rect.top + rect.bottom) / 2;
                    crate::log_info!(
                        "定位托盘图标：pid={pid} uID={uid} rect=({},{},{},{})，模拟点击 ({cx},{cy})",
                        rect.left, rect.top, rect.right, rect.bottom
                    );

                    // 虚拟屏坐标 → SendInput 绝对坐标（0..65535）
                    let ax = ((cx - vx) * 65535) / (vw - 1);
                    let ay = ((cy - vy) * 65535) / (vh - 1);
                    let mi = |flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS| INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: ax,
                                dy: ay,
                                mouseData: 0,
                                dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };

                    // 记录光标 → 移动点击 → 恢复（尽量减少对用户操作的干扰）
                    let mut origin = windows::Win32::Foundation::POINT::default();
                    let _ = GetCursorPos(&mut origin);
                    let _ = SendInput(&[mi(MOUSEEVENTF_MOVE)], core::mem::size_of::<INPUT>() as i32);
                    std::thread::sleep(Duration::from_millis(90));
                    let _ = SendInput(&[mi(MOUSEEVENTF_LEFTDOWN)], core::mem::size_of::<INPUT>() as i32);
                    std::thread::sleep(Duration::from_millis(50));
                    let _ = SendInput(&[mi(MOUSEEVENTF_LEFTUP)], core::mem::size_of::<INPUT>() as i32);
                    let _ = SetCursorPos(origin.x, origin.y);

                    // 等待主窗口重新可见（最多 1.5s，100ms 步进）
                    for _ in 0..15 {
                        std::thread::sleep(Duration::from_millis(100));
                        if let Some(main) = find_main_window(*pid) {
                            if IsWindowVisible(main).as_bool() {
                                return Some(main);
                            }
                        }
                    }
                    crate::log_debug!("托盘图标 uID={uid} 点击后主窗口未恢复，继续尝试");
                }
            }
        }
    }
    None
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

/// 恢复 / 显示窗口后再抢前台（v1.1.19 重写：**验证-重试轮换策略**）。
///
/// 实测取证（v1.1.18 后的诊断程序 `examples/diag_soda.rs`，2026-11-02）：
///
/// 1. `SetForegroundWindow` **返回 true ≠ 前台真的转移**——存在返回 true 但
///    前台几百毫秒内仍在原窗口（甚至漂到别的窗口）的竞态；旧代码见到 true
///    就短路，永不重试，导致汽水窗口「显示了但从未激活」。
/// 2. 窗口「显示但从未激活」是**死态**：模拟真实用户点击（SendInput）都无法
///    激活它（前台纹丝不动），CEF 输入挂起，表现为「能拖动、内容全部点不动」。
///    窗口线程内部 `GUITHREADINFO` 还显示 active/focus 指向自己（幽灵激活），
///    但系统前台在别处。
/// 3. 突破前台锁最有效的是 **ALT 键解锁**（`keybd_event(VK_MENU)` down/up，
///    让系统认为刚有用户输入），比 `AttachThreadInput` 更可靠；后者在本机
///    实测全部失败。
///
/// 因此激活改为「轮换策略 + 每步验证」：每次尝试后真正确认前台已转移，
/// 没转移就换下一个策略，最多 6 步 ≈ 1.2s（成功路径通常第 0/1 步内完成）。
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
    if try_foreground(hwnd) {
        return true;
    }
    crate::log_warn!("轮换 6 步策略后前台仍未转移到目标窗口");
    false
}

/// 轮换激活策略，每步真正验证前台转移，成功立即返回。
///
/// 策略序：0 裸 `SetForegroundWindow` → 1/2 ALT 解锁前台锁 → 3/4
/// `AttachThreadInput`+`SetFocus`/`SetActiveWindow` → 5 `SwitchToThisWindow`。
/// 用户点封面时我们是前台进程，策略 0 通常直接成功（几十 ms）；
/// 前台权被别的程序持有时 ALT 解锁兜住（diag 实测策略 1 成功）。
fn try_foreground(hwnd: HWND) -> bool {
    const VK_MENU: u8 = 0x12; // ALT
    // SAFETY: 标准输入/窗口 API；所有查询在作用域内完成。
    unsafe {
        let target_tid = GetWindowThreadProcessId(hwnd, None);
        let self_tid = windows::Win32::System::Threading::GetCurrentThreadId();

        for step in 0..6u32 {
            match step {
                1 | 2 => {
                    // ALT 按下再抬起：系统视作刚有用户输入，前台锁放行
                    keybd_event(VK_MENU, 0, KEYEVENTF_EXTENDEDKEY, 0);
                    keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, 0);
                    std::thread::sleep(Duration::from_millis(60));
                }
                3 | 4 => {
                    if target_tid != 0 && target_tid != self_tid {
                        let _ = AttachThreadInput(self_tid, target_tid, true);
                    }
                    // 附加输入队列期间把焦点显式还给目标窗口
                    let _ = SetFocus(Some(hwnd));
                    let _ = SetActiveWindow(hwnd);
                }
                5 => {
                    // 任务栏切窗口同款 API，绕前台锁且不碰输入队列
                    SwitchToThisWindow(hwnd, false);
                    std::thread::sleep(Duration::from_millis(200));
                    if GetForegroundWindow() == hwnd {
                        return true;
                    }
                    continue;
                }
                _ => {}
            }

            let ok = SetForegroundWindow(hwnd).as_bool();
            if target_tid != 0 && target_tid != self_tid && (step == 3 || step == 4) {
                let _ = AttachThreadInput(self_tid, target_tid, false);
            }
            // 关键：返回 true 只代表「请求被接受」——必须等前台真的转移
            std::thread::sleep(Duration::from_millis(180));
            if GetForegroundWindow() == hwnd {
                if !ok {
                    crate::log_debug!("策略 {step}：SetForegroundWindow 返回 false 但前台已转移");
                }
                crate::log_debug!("策略 {step} 成功，前台已转移到目标窗口");
                return true;
            }
            crate::log_debug!("策略 {step} 未生效（SetForegroundWindow={ok}），换下一个");
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
        assert_eq!(Activation::Tray.as_str(), "tray");
        assert_eq!(Activation::Failed.as_str(), "failed");
    }
}
