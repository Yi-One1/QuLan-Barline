//! WebView2 的创建与生命周期（`ARCHITECTURE.md` §13.1）。
//!
//! ## 为什么整条链都是回调
//!
//! `CreateCoreWebView2EnvironmentWithOptions` 与
//! `ICoreWebView2Environment::CreateCoreWebView2Controller` **都是异步的**。
//! 在 UI 线程上同步等（`join` / `wait_with_pump`）会把消息泵堵死——小组件同时失去响应。
//! 所以这里用 `webview2-com` 提供的完成回调包装，拿到 controller 后**只往
//! `Rc<RefCell<..>>` 里放一下**，后续 `resize` / `push` / `close` 全由 App 主动调。
//!
//! ## 释放顺序（40MB 预算的生死线）
//!
//! `remove_WebMessageReceived` → `controller.Close()` → drop controller → drop webview
//! → drop environment。少一步就会漏引用：关窗后 `Private Bytes` 不回落，
//! 开关 5 次能涨到 80MB+。

use std::cell::RefCell;
use std::rc::Rc;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2, ICoreWebView2Controller,
    ICoreWebView2Environment, ICoreWebView2WebMessageReceivedEventArgs,
};
use webview2_com::{
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    WebMessageReceivedEventHandler,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::CoTaskMemFree;

use crate::event::AppEvent;
use crate::platform::wake::EventSink;
use crate::platform::wide::to_wide;

/// 一个活着的 WebView2 实例。
pub struct SettingsWebView {
    pub controller: ICoreWebView2Controller,
    pub webview: ICoreWebView2,
    token: i64,
    _env: ICoreWebView2Environment,
}

impl SettingsWebView {
    /// 按客户区**物理像素**设置 bounds（DIP 与物理像素的换算由 WebView2 自己做）。
    pub fn set_bounds(&self, x: i32, y: i32, w: i32, h: i32) {
        let r = windows::Win32::Foundation::RECT {
            left: x,
            top: y,
            right: x + w.max(1),
            bottom: y + h.max(1),
        };
        // SAFETY: controller 由本结构体持有；RECT 是栈上值。
        unsafe {
            let _ = self.controller.SetBounds(r);
            let _ = self.controller.SetIsVisible(true);
        }
    }

    /// 给页面推一条 JSON（`postWebMessageAsJson`）。
    pub fn post(&self, json: &str) {
        // SAFETY: webview 由本结构体持有；字符串在调用期间存活。
        unsafe {
            let _ = self.webview.PostWebMessageAsJson(&HSTRING::from(json));
        }
    }

    /// 让页面重新聚焦（打开窗口时用）。
    pub fn focus(&self) {
        // SAFETY: 同上。
        unsafe {
            let _ = self.controller.MoveFocus(
                webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
            );
        }
    }
}

impl Drop for SettingsWebView {
    fn drop(&mut self) {
        // SAFETY: 三个调用都在同一个 UI 线程、对象仍存活时执行；
        // 顺序按「先摘事件 → 再关 controller → 最后靠字段 drop 释放 webview/env」。
        unsafe {
            let _ = self.webview.remove_WebMessageReceived(self.token);
            let _ = self.controller.Close();
        }
        crate::log_debug!("WebView2 已释放（controller.Close + 摘事件）");
    }
}

/// 共享槽：controller 就绪后放进来，App 从这里拿。
pub type Slot = Rc<RefCell<Option<SettingsWebView>>>;

/// 建一个空槽。
pub fn slot() -> Slot {
    Rc::new(RefCell::new(None))
}

/// 异步创建 WebView2。
///
/// * `hwnd`  —— 设置窗口（controller 的父窗口）
/// * `html`  —— 内嵌的整页 HTML
/// * `sink`  —— JS 消息与「就绪」事件都通过它回到 UI 线程
/// * `slot`  —— 成功后 controller 放这里
pub fn create(hwnd: HWND, html: String, sink: EventSink<AppEvent>, slot: Slot) {
    let user_data = user_data_dir();
    let user_data_w = to_wide(&user_data.to_string_lossy());
    let html = HSTRING::from(html);

    // 环境创建完成 → 建 controller
    let env_handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |error: windows::core::Result<()>,
              env: Option<ICoreWebView2Environment>| {
            if let Err(e) = error {
                crate::log_err!("WebView2 环境创建失败：{e}（需要安装 WebView2 运行时）");
                sink.send(AppEvent::SettingsWebViewFailed(format!("{e}")));
                return Ok(());
            }
            let Some(env) = env else {
                sink.send(AppEvent::SettingsWebViewFailed("环境为空".into()));
                return Ok(());
            };
            let env_for_slot = env.clone();
            let slot2 = slot.clone();
            let sink2 = sink.clone();
            let html2 = html.clone();
            let ctrl_handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                move |error: windows::core::Result<()>,
                      controller: Option<ICoreWebView2Controller>| {
                    if let Err(e) = error {
                        crate::log_err!("WebView2 controller 创建失败：{e}");
                        sink2.send(AppEvent::SettingsWebViewFailed(format!("{e}")));
                        return Ok(());
                    }
                    let Some(controller) = controller else {
                        sink2.send(AppEvent::SettingsWebViewFailed("controller 为空".into()));
                        return Ok(());
                    };
                    match build_webview(&controller, env_for_slot, &html2, &sink2) {
                        Ok(wv) => {
                            *slot2.borrow_mut() = Some(wv);
                            crate::log_info!("WebView2 就绪");
                            sink2.send(AppEvent::SettingsWebViewReady);
                        }
                        Err(e) => {
                            crate::log_err!("WebView2 初始化失败：{e}");
                            sink2.send(AppEvent::SettingsWebViewFailed(format!("{e}")));
                        }
                    }
                    Ok(())
                },
            ));
            // SAFETY: hwnd 是设置窗口；handler 由 webview2-com 持有引用计数。
            if let Err(e) = unsafe { env.CreateCoreWebView2Controller(hwnd, &ctrl_handler) } {
                crate::log_err!("CreateCoreWebView2Controller 调用失败：{e}");
            }
            Ok(())
        },
    ));

    // SAFETY: 两个 PCWSTR 都由本地 Vec 持有且以 NUL 结尾；handler 的引用计数由 COM 管理。
    let r = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            PCWSTR(user_data_w.as_ptr()),
            None,
            &env_handler,
        )
    };
    if let Err(e) = r {
        crate::log_err!("CreateCoreWebView2EnvironmentWithOptions 失败：{e}");
    }
}

/// controller 拿到之后的配置：可见、关 DevTools、挂消息回调、导航。
fn build_webview(
    controller: &ICoreWebView2Controller,
    env: ICoreWebView2Environment,
    html: &HSTRING,
    sink: &EventSink<AppEvent>,
) -> windows::core::Result<SettingsWebView> {
    // SAFETY: controller 刚由完成回调给出且仍存活。
    unsafe {
        controller.SetIsVisible(true)?;
        let webview = controller.CoreWebView2()?;
        // release 下关掉调试能力与默认右键菜单（PRD 未要求，且是攻击面）
        // DevTools / 默认右键菜单都关掉：release 不需要，而且是多余攻击面
        if let Ok(settings) = webview.Settings() {
            let _ = settings.SetAreDevToolsEnabled(false);
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
        }

        let sink2 = sink.clone();
        let handler = WebMessageReceivedEventHandler::create(Box::new(
            move |_sender: Option<ICoreWebView2>,
                  args: Option<ICoreWebView2WebMessageReceivedEventArgs>| {
                if let Some(args) = args {
                    // SAFETY: args 由 WebView2 提供，本回调期间有效。
                    if let Some(text) = web_message_text(&args) {
                        sink2.send(AppEvent::SettingsMessage(text));
                    }
                }
                Ok(())
            },
        ));
        let mut token: i64 = 0;
        webview.add_WebMessageReceived(&handler, &mut token)?;
        webview.NavigateToString(html)?;

        Ok(SettingsWebView {
            controller: controller.clone(),
            webview,
            token,
            _env: env,
        })
    }
}

/// 从 `WebMessageReceived` 事件参数里取出消息文本（QA-M4 缺陷 #1 的修复点）。
///
/// 页面有两种发法，对应两条**互斥**的读取通道：
/// * `chrome.webview.postMessage("字符串")` → 只有 `TryGetWebMessageAsString` 能拿到；
/// * `chrome.webview.postMessage({...})`   → **AsString 必然失败且静默丢弃**
///   （设置页 JS 全部用对象消息，这正是「开关不落盘」的根因），必须回退
///   `TryGetWebMessageAsJson`。
///
/// 两条路径都归一成 `String` 交给 `AppEvent::SettingsMessage`；都拿不到返回 `None`。
///
/// # Safety
/// `args` 必须是 WebView2 正在回调期间给出的有效事件参数。
unsafe fn web_message_text(args: &ICoreWebView2WebMessageReceivedEventArgs) -> Option<String> {
    let mut pwstr = PWSTR::null();
    // SAFETY: 出参是本地变量；args 由 WebView2 提供，调用期间有效。
    let as_string = args.TryGetWebMessageAsString(&mut pwstr).is_ok() && !pwstr.is_null();
    if !as_string {
        // 对象消息：回退 JSON 通道（本版绑定的 getter 名为 WebMessageAsJson）
        pwstr = PWSTR::null();
        // SAFETY: 同上。
        if args.WebMessageAsJson(&mut pwstr).is_err() || pwstr.is_null() {
            return None;
        }
    }
    // SAFETY: pwstr 是 WebView2 分配的 NUL 结尾宽字符串，读成 String 后立刻归还。
    let text = unsafe { pwstr.to_string() }.unwrap_or_default();
    // SAFETY: 归还给 COM 分配器。
    unsafe { CoTaskMemFree(Some(pwstr.0 as *const core::ffi::c_void)) };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `%LOCALAPPDATA%\Barline\WebView2`（与配置文件同目录，便携版不污染系统）。
fn user_data_dir() -> std::path::PathBuf {
    let dir = crate::platform::barline_dir().join("WebView2");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 探测 WebView2 运行时是否可用（用于「没装运行时」的兜底提示）。
///
/// 走 `GetAvailableCoreWebView2BrowserVersionString`：返回 `Ok(None)` 表示没装。
pub fn runtime_version() -> Option<String> {
    use webview2_com::Microsoft::Web::WebView2::Win32::GetAvailableCoreWebView2BrowserVersionString;
    use windows::core::PWSTR;

    let mut version = PWSTR::null();
    // SAFETY: 出参是本地变量；返回的字符串由 WebView2 分配，下面立刻释放。
    let hr = unsafe { GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version) };
    if hr.is_err() || version.is_null() {
        return None;
    }
    // SAFETY: version 是 NUL 结尾的宽字符串。
    let s = unsafe { version.to_string() }.ok();
    // SAFETY: 同上，必须归还给 COM 分配器。
    unsafe { CoTaskMemFree(Some(version.0 as *const core::ffi::c_void)) };
    s
}

/// 打开 WebView2 运行时下载页（兜底路径用）。
pub fn open_runtime_download() {
    let url = to_wide("https://developer.microsoft.com/microsoft-edge/webview2/");
    // SAFETY: url 以 NUL 结尾且在本调用期间存活。
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

