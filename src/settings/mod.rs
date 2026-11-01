//! 设置页（`ARCHITECTURE.md` §13 / T13）。
//!
//! 结构：
//! ```text
//! mod.rs     设置窗口（Fluent 标题栏 / Mica / 圆角）+ SettingsService 句柄
//! webview.rs WebView2 环境 + controller + 消息回调（全异步，不堵消息泵）
//! bridge.rs  JSON 双向协议 + 配置白名单写入
//! assets.rs  内嵌的 index.html（include_str!，便携版单 exe）
//! ```
//!
//! ## 内存纪律
//!
//! 关窗 = `DestroyWindow` + drop WebView2（不是 Hide）。用户关掉设置页后
//! `Private Bytes` 必须回落到 15MB 以内，连开关 5 次不能涨。

pub mod assets;
pub mod bridge;
pub mod webview;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, IsWindow,
    LoadIconW, RegisterClassW, SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use crate::event::AppEvent;
use crate::platform::wake::EventSink;

/// 设置窗口类名。
pub const SETTINGS_CLASS: PCWSTR = w!("BarlineSettingsWnd");

/// DwmSetWindowAttribute 的属性号。
///
/// **windows crate 里这几个常量没有具名导出**（38/33/20 是新版 Windows 才加的），
/// 所以直接传字面量：`DWMWA_SYSTEMBACKDROP_TYPE` = 38、`DWMWA_WINDOW_CORNER_PREFERENCE` = 33、
/// `DWMWA_USE_IMMERSIVE_DARK_MODE` = 20。
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
/// `DWMSBT_MAINWINDOW`（Mica）。
const DWMSBT_MAINWINDOW: u32 = 2;
/// `DWMWCP_ROUND`。
const DWMWCP_ROUND: u32 = 2;

/// 设置页服务。
pub struct SettingsService {
    hwnd: Option<HWND>,
    slot: webview::Slot,
    /// 最近一次推给页面的状态（窗口重建后补推）
    last_state: Option<String>,
}

impl SettingsService {
    pub fn new() -> Self {
        Self {
            hwnd: None,
            slot: webview::slot(),
            last_state: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.hwnd.map(|h| !h.is_invalid() && unsafe_is_window(h)).unwrap_or(false)
    }

    pub fn hwnd(&self) -> Option<HWND> {
        self.hwnd
    }

    /// 打开（或前置）设置窗口。
    pub fn open(
        &mut self,
        hinstance: HINSTANCE,
        sink: EventSink<AppEvent>,
        state_json: String,
        dark: bool,
    ) {
        if let Some(h) = self.hwnd {
            if unsafe_is_window(h) {
                // 已经开着：只前置
                // SAFETY: h 是有效的设置窗口。
                unsafe {
                    let _ = ShowWindow(h, SW_SHOW);
                }
                crate::platform::force_foreground(h);
                self.push(&state_json);
                return;
            }
        }
        self.last_state = Some(state_json.clone());

        let class = SETTINGS_CLASS;
        let wc = WNDCLASSW {
            lpfnWndProc: Some(settings_proc),
            hInstance: hinstance,
            lpszClassName: class,
            // SAFETY: 取 exe 里的图标资源（id=1），进程存活期间有效。
            hIcon: unsafe { LoadIconW(Some(hinstance), PCWSTR(1 as *const u16)) }.unwrap_or_default(),
            ..Default::default()
        };
        // SAFETY: wc 是本地栈结构体、字段已填全；重复注册返回 0 不视为错误。
        unsafe {
            RegisterClassW(&wc);
        }

        let (w, h) = (880, 640);
        let (x, y) = centered(w, h);
        // SAFETY: 类已注册；父窗口 None = 顶层窗口；尺寸与位置都是有限值。
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("曲栏 Barline"),
                WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0),
                x,
                y,
                w,
                h,
                None,
                None,
                Some(hinstance),
                None,
            )
        };
        let Ok(hwnd) = hwnd else {
            crate::log_err!("设置窗口创建失败");
            return;
        };
        self.hwnd = Some(hwnd);

        // Fluent 观感：Mica 背景 + 圆角 + 跟随系统明暗
        apply_fluent(hwnd, dark);

        // SAFETY: hwnd 刚创建且有效；SW_SHOW 只改可见性。
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        // 用户可能正在别的程序里操作：用 attach 线程输入队列的办法抢前台
        crate::platform::force_foreground(hwnd);

        // 异步建 WebView2（不阻塞消息泵）
        let html = assets::index_html();
        webview::create(hwnd, html, sink, self.slot.clone());
        crate::log_info!("设置窗口已创建（{}×{}），正在初始化 WebView2", w, h);
    }

    /// 关窗（销毁窗口 + 释放 WebView2）。
    ///
    /// 幂等：`WM_CLOSE` 与 `WM_DESTROY` 都会调进来，只有第一次真正做事。
    pub fn close(&mut self) {
        let Some(h) = self.hwnd.take() else {
            // 已经关过了：只需确保 WebView2 槽是空的
            *self.slot.borrow_mut() = None;
            return;
        };
        if unsafe_is_window(h) {
            // SAFETY: h 有效；DestroyWindow 会触发 WM_DESTROY（本函数幂等，不会递归）。
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
        *self.slot.borrow_mut() = None;
        crate::log_info!("设置窗口已关闭，WebView2 已释放");
    }

    /// 推一条 JSON 给页面（没开就记下来，下次打开补推）。
    pub fn push(&mut self, json: &str) {
        self.last_state = Some(json.to_string());
        if let Some(wv) = self.slot.borrow().as_ref() {
            wv.post(json);
        }
    }

    /// 把最近一次状态重推（WebView2 就绪时用）。
    pub fn push_last(&self) {
        if let (Some(json), Some(wv)) = (self.last_state.as_ref(), self.slot.borrow().as_ref()) {
            wv.post(json);
        }
    }

    /// 窗口尺寸变化 → 同步 WebView2 的 bounds。
    pub fn resize(&self, hwnd: HWND) {
        let mut rc = RECT::default();
        // SAFETY: hwnd 有效；rc 是本地栈结构体。
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if let Some(wv) = self.slot.borrow().as_ref() {
            wv.set_bounds(0, 0, w, h);
        }
    }

    /// WebView2 就绪：设 bounds + 聚焦 + 补推状态。
    pub fn on_webview_ready(&mut self) {
        if let Some(h) = self.hwnd {
            self.resize(h);
            if let Some(wv) = self.slot.borrow().as_ref() {
                wv.focus();
            }
        }
        self.push_last();
    }
}

impl Default for SettingsService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SettingsService {
    fn drop(&mut self) {
        self.close();
    }
}

/// 设置窗口过程：只做尺寸同步与关闭。
unsafe extern "system" fn settings_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_SIZE => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 本函数在 UI 线程执行；app 由 App::bootstrap 的 Box 持有。
                unsafe { (*app).on_settings_resize(hwnd) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 「关闭」= 销毁窗口（释放 WebView2），进程继续跑小组件
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同上。
                unsafe { (*app).on_settings_close_request() };
            } else {
                // SAFETY: hwnd 有效。
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let app = app_ptr(hwnd);
            if !app.is_null() {
                // SAFETY: 同上。
                unsafe { (*app).on_settings_destroyed() };
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: 未处理的消息交回系统。
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn app_ptr(hwnd: HWND) -> *mut crate::app::App {
    // SAFETY: 只读 GWLP_USERDATA 槽，不解引用空指针。
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut crate::app::App }
}

/// 把 App* 绑到设置窗口（App::bootstrap 里调用）。
pub fn bind_app(hwnd: HWND, app: *mut crate::app::App) {
    // SAFETY: hwnd 有效；只写一个指针值。
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
    }
}

fn unsafe_is_window(hwnd: HWND) -> bool {
    // SAFETY: IsWindow 对任意句柄都安全（只返回真假）。
    unsafe { IsWindow(Some(hwnd)).as_bool() }
}

/// Mica + 圆角 + 明暗（失败不致命：老系统没有这些属性）。
fn apply_fluent(hwnd: HWND, dark: bool) {
    let backdrop = DWMSBT_MAINWINDOW;
    let corner = DWMWCP_ROUND;
    let dark_flag: i32 = if dark { 1 } else { 0 };
    // SAFETY: 四个调用都只读本地变量的地址；属性号与取值来自 DWMWA_* 文档。
    // 返回值在旧系统上可能是 E_INVALIDARG，忽略即可（窗口仍可用，只是没 Mica）。
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_SYSTEMBACKDROP_TYPE as i32),
            &backdrop as *const u32 as *const core::ffi::c_void,
            core::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_WINDOW_CORNER_PREFERENCE as i32),
            &corner as *const u32 as *const core::ffi::c_void,
            core::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_USE_IMMERSIVE_DARK_MODE as i32),
            &dark_flag as *const i32 as *const core::ffi::c_void,
            core::mem::size_of::<i32>() as u32,
        );
    }
}

/// 居中到主显示器。
fn centered(w: i32, h: i32) -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    // SAFETY: 两个常量查询都是纯读取。
    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    ((sw - w) / 2, (sh - h) / 2)
}
