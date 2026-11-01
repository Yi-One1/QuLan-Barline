//! 窗口类注册 + 两个窗口过程（小组件窗口、消息窗）。
//!
//! 只有 UI 线程（T0）会执行这里的代码。两个 WndProc 都从 `GWLP_USERDATA`
//! 取回 `App*`，再把语义化消息转交给 `App` 的方法——WndProc 本身**不做业务逻辑**，
//! 保证「消息 → 事件 → reduce → Effect」这条单向数据流不被打破（§3.3）。
//!
//! ## 定时器分工（T19 的分级策略在这里先铺好）
//!
//! | 定时器 | 窗口 | 周期 | 何时存在 |
//! | --- | --- | --- | --- |
//! | `TIMER_GEOM` | 消息窗 | 1000ms | 进程全程（几何兜底探测） |
//! | `TIMER_ANIM` | 小组件 | 16ms | **仅淡入淡出 / HUD 期间**，结束即 `KillTimer` |
//! | `TIMER_CLICK` | 小组件 | 250ms | **仅有一次待判定单击时**，触发即 `KillTimer` |
//!
//! 「空闲 CPU ≈ 0」靠的就是后两个按需启停：静止态只剩 1 秒一次的几何探测。

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, LoadCursorW, RegisterClassW,
    RegisterWindowMessageW, SetWindowLongPtrW, GWLP_USERDATA, IDC_ARROW, MA_NOACTIVATE, WNDCLASSW,
    WM_DISPLAYCHANGE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_PAINT, WM_SETTINGCHANGE,
    WM_THEMECHANGED, WM_TIMER, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::app::App;
use crate::platform::wake::{
    WM_BARLINE_SHOW_SETTINGS, WM_BARLINE_WAKE, WM_BARLINE_WHEEL,
};

/// `WM_MOUSELEAVE`。
///
/// windows-rs 把它放在 `Win32::UI::Controls` 下，而 `ARCHITECTURE.md` §4.2 明确要求
/// **不要**开 `Win32_UI_Controls`（每个顶层 feature 都会增加编译期与二进制）。
/// 它只是一个消息号常量（675），这里本地定义，语义完全一致。
pub const WM_MOUSELEAVE: u32 = 675;

/// 消息窗类名（单实例唤起时作为第二候选查找目标）。
pub const MSG_CLASS_NAME: PCWSTR = w!("BarlineMsgWnd");

/// `RegisterWindowMessageW("TaskbarCreated")` 的返回值。0 表示注册失败。
static TASKBAR_CREATED_MSG: AtomicU32 = AtomicU32::new(0);

/// 几何探测定时器 ID（T03 验收用；T18 会被正式 watcher 取代）。
pub const TIMER_GEOM: usize = 1;
/// 几何探测周期（毫秒）。
pub const TIMER_GEOM_MS: u32 = 1_000;
/// 动画定时器（16ms，仅在淡入淡出 / HUD 期间存在）。
pub const TIMER_ANIM: usize = 2;
/// 单击判定定时器（250ms，仅在有一次待判定单击时存在）。
pub const TIMER_CLICK: usize = 3;
/// 悬停期间模糊背景刷新定时器（250ms，仅「悬停 + 模糊模式」时存在）。
pub const TIMER_BLUR: usize = 4;
/// 悬停期间模糊背景刷新周期（§11.3：悬停持续时每 250ms 重新抓屏）。
pub const TIMER_BLUR_MS: u32 = 250;
/// 歌词推进定时器（T12：逐字扫光需要连续重绘）。
pub const TIMER_LYRIC: usize = 5;
/// 歌词推进周期（40ms ≈ 25fps）。
///
/// 为什么不是 16ms：13px 字号下一个汉字约 13px 宽、平均停留 250–350ms，
/// 扫光边界每帧只移动 ~1px；25fps 已经完全看不出台阶，而 60fps 会让
/// 「播放中空闲 CPU」逼近 PRD P0-14 的 0.5% 红线。
pub const TIMER_LYRIC_MS: u32 = 40;

/// 全局的「小组件窗口句柄」与「App 指针」——`WH_MOUSE_LL` 钩子回调需要它们。
///
/// 用原子量而不是静态 `Mutex`：钩子回调是热路径（每次鼠标移动都会进），
/// 取一个 `isize` 不值得加锁。
static WIDGET_HWND: AtomicIsize = AtomicIsize::new(0);
static WIDGET_APP: AtomicIsize = AtomicIsize::new(0);
/// `HHOOK` 句柄（0 = 未安装）。
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

/// 注册两个窗口类。重复注册（atom 已存在）视为成功。
pub fn register_classes(hinstance: HINSTANCE) -> Result<()> {
    // SAFETY: LoadCursorW(None, IDC_ARROW) 取系统预定义光标，不需要释放。
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();

    let widget = WNDCLASSW {
        style: Default::default(),
        lpfnWndProc: Some(widget_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: Default::default(),
        hCursor: cursor,
        // 不设背景刷：分层窗口从不走 WM_ERASEBKGND 的默认绘制
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: crate::taskbar::host::WIDGET_CLASS_NAME,
    };

    let msg = WNDCLASSW {
        lpfnWndProc: Some(msg_proc),
        lpszClassName: MSG_CLASS_NAME,
        ..widget
    };

    // SAFETY: 两个 WNDCLASSW 都是本地栈结构体，字段已填全（`lpfnWndProc` 指向本模块的
    // `extern "system"` 函数，句柄字段为空表示用默认值）；结构体在本调用期间存活。
    // RegisterClassW 返回 0 表示失败（重复注册同类名也会返回 0，此处不视为错误）。
    unsafe {
        let a = RegisterClassW(&widget);
        let b = RegisterClassW(&msg);
        if a == 0 && b == 0 {
            crate::log_debug!("RegisterClassW 返回 0（类可能已存在），继续创建窗口");
        }
    }

    // SAFETY: 注册一个系统广播消息号，纯读取，无副作用。
    let created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TASKBAR_CREATED_MSG.store(created, Ordering::Relaxed);
    if created == 0 {
        crate::log_warn!("RegisterWindowMessageW(TaskbarCreated) 失败，Explorer 重启后无法自动恢复");
    }

    Ok(())
}

/// 创建隐藏的消息窗（`HWND_MESSAGE`）。所有跨线程唤醒都投递到这里。
pub fn create_msg_window(hinstance: HINSTANCE, user_data: isize) -> Result<HWND> {
    // SAFETY: 类已注册；HWND_MESSAGE 表示「只收消息、不可见」的子窗口；
    // 尺寸为 0 是消息窗的常规做法。
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            MSG_CLASS_NAME,
            w!("BarlineMsg"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )?
    };

    if user_data != 0 {
        // SAFETY: hwnd 刚创建且有效；只写一个指针值到 GWLP_USERDATA。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, user_data) };
    }

    // SAFETY: 给消息窗挂 1 秒定时器，用于几何兜底探测。
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
            Some(hwnd),
            TIMER_GEOM,
            TIMER_GEOM_MS,
            None,
        );
    }

    crate::log_debug!("消息窗已创建：hwnd=0x{:X}", hwnd.0 as usize);
    Ok(hwnd)
}

/// 从窗口句柄取回 `App*`。返回 null 表示还没绑定。
fn app_ptr(hwnd: HWND) -> *mut App {
    // SAFETY: GetWindowLongPtrW 只读窗口的 user data 槽；我们只存过 `App*` 或 0。
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App }
}

// ------------------------------------------------------------------ 滚轮钩子

/// 安装 `WH_MOUSE_LL` 低级鼠标钩子（**仅在悬停期间**，§10.7 / 风险 R7）。
///
/// 为什么需要它：`WS_EX_NOACTIVATE` 的窗口拿不到 `WM_MOUSEWHEEL`——
/// 滚轮按焦点路由，不激活的窗口没有焦点（难点 H4）。
/// `WH_MOUSE_LL` 是用户态钩子，不需要管理员权限；离开时必须卸载，
/// 否则常驻的全局钩子会被安全软件盯上。
pub fn install_wheel_hook(widget: HWND, app: *mut App) {
    if MOUSE_HOOK.load(Ordering::Relaxed) != 0 {
        return;
    }
    WIDGET_HWND.store(widget.0 as isize, Ordering::Relaxed);
    WIDGET_APP.store(app as isize, Ordering::Relaxed);

    // SAFETY: 钩子过程是 'static 函数；hMod 传 None + dwThreadId 传 0 表示全局钩子；
    // 失败返回 Err，我们只记日志（滚轮不可用，其它功能不受影响）。
    let r = unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetWindowsHookExW(
            windows::Win32::UI::WindowsAndMessaging::WH_MOUSE_LL,
            Some(mouse_ll_proc),
            None,
            0,
        )
    };
    match r {
        Ok(h) => {
            MOUSE_HOOK.store(h.0 as isize, Ordering::Relaxed);
            crate::log_debug!("WH_MOUSE_LL 钩子已安装");
        }
        Err(e) => crate::log_warn!("WH_MOUSE_LL 安装失败：{e}（滚轮调音量不可用）"),
    }
}

/// 卸载滚轮钩子。必须与 [`install_wheel_hook`] 配对。
pub fn uninstall_wheel_hook() {
    let raw = MOUSE_HOOK.swap(0, Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    // SAFETY: raw 是 install_wheel_hook 存下的有效 HHOOK；swap 保证只卸载一次。
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(
            windows::Win32::UI::WindowsAndMessaging::HHOOK(raw as *mut core::ffi::c_void),
        );
    }
    crate::log_debug!("WH_MOUSE_LL 钩子已卸载");
}

/// 低级鼠标钩子过程。只处理「光标在小组件内 + WM_MOUSEWHEEL」这一种情况。
unsafe extern "system" fn mouse_ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, MSLLHOOKSTRUCT,
    };

    if code == HC_ACTION as i32 && wparam.0 as u32 == WM_MOUSEWHEEL {
        let raw = WIDGET_HWND.load(Ordering::Relaxed);
        if raw != 0 {
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            // SAFETY: lparam 指向系统提供的 MSLLHOOKSTRUCT，只在本次回调内有效。
            let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if point_in_window(hwnd, ms.pt) {
                let delta = ((ms.mouseData >> 16) as i16) as i32;
                // SAFETY: hwnd 是小组件窗口；PostMessageW 跨线程安全。
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                        Some(hwnd),
                        WM_BARLINE_WHEEL,
                        WPARAM(delta as usize),
                        LPARAM(0),
                    );
                }
                // 吞掉：不传给任务栏（PRD P1-01「滚轮事件不传递给任务栏」）
                return LRESULT(1);
            }
        }
    }

    // SAFETY: 未处理的消息必须交给下一个钩子，否则会破坏整条钩子链。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 屏幕坐标点是否落在窗口矩形内。
fn point_in_window(hwnd: HWND, pt: windows::Win32::Foundation::POINT) -> bool {
    let mut r = windows::Win32::Foundation::RECT::default();
    // SAFETY: r 是本地结构体；hwnd 由我们保存。
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r) }.is_err() {
        return false;
    }
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

// ------------------------------------------------------------------ 窗口过程

/// 小组件窗口过程。
///
/// `WM_MOUSEACTIVATE` 返回 `MA_NOACTIVATE` 是「不抢焦点」的双保险
/// （`WS_EX_NOACTIVATE` 在某些输入法路径下会被绕过，见 §10.2）。
unsafe extern "system" fn widget_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        // 分层窗口自己管绘制，不需要系统擦背景（擦会闪白）
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            // 必须 ValidateRect，否则 WM_PAINT 会一直重投递
            // SAFETY: hwnd 有效；null 表示校验整个客户区。
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ValidateRect(Some(hwnd), None);
            }
            return LRESULT(0);
        }
        _ => {}
    }

    let created = TASKBAR_CREATED_MSG.load(Ordering::Relaxed);
    if created != 0 && msg == created {
        let app = app_ptr(hwnd);
        if !app.is_null() {
            // SAFETY: app 是 UI 线程上存活的 Box<App> 裸指针；本函数在 UI 线程执行，
            // 不存在并发别名。
            unsafe { (*app).on_taskbar_created() };
        }
        return LRESULT(0);
    }

    if msg == WM_BARLINE_SHOW_SETTINGS {
        let app = app_ptr(hwnd);
        if !app.is_null() {
            // SAFETY: 同上。
            unsafe { (*app).on_second_instance() };
        }
        return LRESULT(0);
    }

    // 窗口建立后把句柄与 App 指针记下来，供滚轮钩子使用
    if WIDGET_HWND.load(Ordering::Relaxed) == 0 {
        WIDGET_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    }

    let app = app_ptr(hwnd);
    if !app.is_null() {
        // SAFETY: 同上。
        if let Some(r) = unsafe { (*app).on_widget_message(msg, wparam, lparam) } {
            return r;
        }
    }

    // SAFETY: 未处理的消息交回系统默认处理。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 消息窗过程：`WM_BARLINE_WAKE` → drain 事件队列 → reduce → apply。
unsafe extern "system" fn msg_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_BARLINE_WAKE => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同 widget_proc；唤醒投递与 drain 都在 UI 线程完成。
                unsafe { (*app).on_wake() };
            }
            LRESULT(0)
        }
        WM_BARLINE_SHOW_SETTINGS => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同上。
                unsafe { (*app).on_second_instance() };
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同上。
                unsafe { (*app).on_timer() };
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同上。
                unsafe { (*app).on_display_changed() };
            }
            LRESULT(0)
        }
        // 壁纸 / 主题 / 系统强调色变化 → 重新采样（PRD P0-07：300ms 内完成切换）
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同上。
                unsafe { (*app).on_setting_changed() };
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DESTROY => {
            // SAFETY: 标准的退出消息泵写法。
            unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            let created = TASKBAR_CREATED_MSG.load(Ordering::Relaxed);
            if created != 0 && msg == created {
                let app = app_ptr(hwnd);
                if !app.is_null() {
                    // SAFETY: 同上。
                    unsafe { (*app).on_taskbar_created() };
                }
                return LRESULT(0);
            }
            // SAFETY: 未处理的消息交回系统默认处理。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// 供 `ui::run_message_loop` 使用：把 `App*` 绑定到已创建的窗口。
pub fn bind_app(hwnd: HWND, app: *mut App) {
    // SAFETY: hwnd 有效；只写一个指针值。
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize) };
    if WIDGET_APP.load(Ordering::Relaxed) == 0 && app as isize != 0 {
        WIDGET_APP.store(app as isize, Ordering::Relaxed);
    }
}
