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
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, IsIconic,
    IsWindow, LoadIconW, RegisterClassW, SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, SW_RESTORE,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_SIZE, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

use crate::event::AppEvent;
use crate::platform::wake::EventSink;

/// 设置窗口类名。
pub const SETTINGS_CLASS: PCWSTR = w!("BarlineSettingsWnd");

/// 「窗口已存在」时该用哪个 `ShowWindow` 命令把它呈现给用户（v1.2.1 / G2 问题 4）。
///
/// 抽成枚举 + 纯函数 [`reveal_action`] 的唯一理由是**可测**：Win32 没法单测，
/// 但"最小化 vs 未最小化该调什么"这个决策可以。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    /// 窗口被最小化（在任务栏里躺着）→ 必须 `SW_RESTORE` 才会还原
    Restore,
    /// 窗口正常但被别的窗口盖住 → `SW_SHOW` + 抢前台即可
    Show,
}

/// 决策：窗口最小化了吗？→ 该还原还是只显示。
///
/// **这是本模块唯一的判定逻辑**，真实调用点（[`SettingsService::open`]）必须用它，
/// 不允许再写一份 `if iconic { ... } else { ... }` 的并行分支。
///
/// 为什么重要：`SW_SHOW` 对一个**最小化**窗口是 no-op（不会 un-minimize），
/// 用户点「设置」看到的是"窗口还在任务栏躺着、什么也没发生"。
pub fn reveal_action(iconic: bool) -> Reveal {
    if iconic {
        Reveal::Restore
    } else {
        Reveal::Show
    }
}

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
                // 已经开着：只前置。
                //
                // v1.2.1（G2 问题 4）：`SW_SHOW` 对**最小化**窗口是 no-op（不会还原），
                // 用户点了「设置」看到的却是"窗口还躺在任务栏里"。所以先问一句
                // `IsIconic`，最小化时改用 `SW_RESTORE`；还原后系统会派发一条
                // `WM_SIZE`（带真实尺寸），WebView2 的 bounds 由 `on_settings_resize`
                // → `resize` 自动跟上，这里不需要额外处理。
                let iconic = unsafe_is_iconic(h);
                let reveal = reveal_action(iconic);
                // SAFETY: h 是有效的设置窗口。
                unsafe {
                    let _ = match reveal {
                        Reveal::Restore => ShowWindow(h, SW_RESTORE),
                        Reveal::Show => ShowWindow(h, SW_SHOW),
                    };
                }
                if iconic {
                    crate::log_debug!("设置窗口处于最小化，已用 SW_RESTORE 还原");
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
    ///
    /// **顺序是 QA-M4 缺陷 #2 的修复点**：必须**先**释放 WebView2（slot → None，
    /// 触发 `SettingsWebView::drop`：摘事件 → `controller.Close()` → 释放
    /// webview/env），**再** `DestroyWindow`。旧实现反过来——父窗口先于
    /// controller 消失，WebView2 的宿主侧关联资源回收不掉，实测私有内存恒定
    /// 停留在 ~44MB 不回落（3 轮开关无增长但不回落）。
    pub fn close(&mut self) {
        // 第一步：无条件先放掉 WebView2（borrow_mut 置 None 是幂等的）。
        *self.slot.borrow_mut() = None;
        // 第二步：销毁窗口。hwnd 已被 take，WM_DESTROY 里的二次 close() 走
        // `hwnd == None` 分支直接返回，不会递归 DestroyWindow。
        let Some(h) = self.hwnd.take() else {
            return;
        };
        if unsafe_is_window(h) {
            // SAFETY: h 有效且尚未开始销毁；WM_DESTROY 由本次调用同步触发。
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
        crate::log_info!("设置窗口已关闭，WebView2 已释放");
    }

    /// 推一条 JSON 给页面（没开就记下来，下次打开补推）。
    pub fn push(&mut self, json: &str) {
        self.last_state = Some(json.to_string());
        if let Some(wv) = self.slot.borrow().as_ref() {
            wv.post(json);
        }
    }

    /// 推一条**一次性**消息给页面（v1.8.0 / R2）：只发出去，**不碰 `last_state`**。
    ///
    /// 为什么不能复用 [`Self::push`]：`push` 会把 `last_state` 覆写成刚推的内容。
    /// 拿它推 `cacheStats`，下一次窗口重建时 [`Self::push_last`] 补推的就是
    /// `cacheStats` 而不是 `state` → 页面收不到配置，**所有控件空白**。
    /// 而且这个缺陷只在「关掉设置窗口再打开」时复现（开着窗测永远全绿）。
    ///
    /// 语义上也有区别：`state` 是「配方的全量快照」，必须缓存重推；
    /// `cacheStats` 是「一次点查的结果」，重推会让扫描次数飘（A5 的数次会飘）。
    pub fn post(&mut self, json: &str) {
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
    ///
    /// v1.2.1（G2 问题 4）：加了一道 `0×0` 守卫。最小化时系统会派发
    /// `WM_SIZE(SIZE_MINIMIZED)`，`GetClientRect` 拿到的是 `0×0`——直接传给
    /// `set_bounds` 会让 WebView2 白做一次"缩到零再长回来"的布局。还原时
    /// 系统会再派发一条带真实尺寸的 `WM_SIZE`，所以这里直接跳过没有副作用。
    pub fn resize(&self, hwnd: HWND) {
        let mut rc = RECT::default();
        // SAFETY: hwnd 有效；rc 是本地栈结构体。
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        if w <= 0 || h <= 0 {
            // 最小化 / 尚未完成布局：不把 0×0 传给 WebView2
            return;
        }
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

/// 窗口是否处于最小化（在任务栏里）。
fn unsafe_is_iconic(hwnd: HWND) -> bool {
    // SAFETY: IsIconic 对任意句柄都安全（只返回真假）。
    unsafe { IsIconic(hwnd).as_bool() }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// v1.2.1（G2 问题 4）：最小化的设置窗口必须 `SW_RESTORE`——
    /// `SW_SHOW` 对最小化窗口是 no-op，用户会以为「点了设置没反应」。
    #[test]
    fn iconic_window_is_restored() {
        assert_eq!(reveal_action(true), Reveal::Restore);
    }

    /// 未最小化（只是被别的窗口盖住）→ `SW_SHOW` + 抢前台即可，不需要还原。
    #[test]
    fn visible_window_is_only_shown() {
        assert_eq!(reveal_action(false), Reveal::Show);
    }
}
