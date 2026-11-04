//! 托盘图标（`TASKS.md` T14 / PRD P0-12）。
//!
//! ## 为什么托盘要自己的窗口
//!
//! `Shell_NotifyIconW` 的回调消息发给我们指定的 `HWND`，而右键菜单需要
//! `SetForegroundWindow` + `TrackPopupMenuEx` 才能「点别处自动消失」。
//! 小组件窗口是 `WS_EX_NOACTIVATE`（**按设计不抢焦点**），拿它当宿主菜单会飘；
//! 消息窗是 `HWND_MESSAGE`（没有 Z 序，无法前台化）。
//! 所以托盘自己建一个**隐藏的普通顶层窗口**，专管图标与菜单。
//!
//! ## Explorer 重启
//!
//! `TaskbarCreated` 广播之后图标会消失，必须重新 `NIM_ADD`。广播由小组件窗口
//! 收到（`App::on_taskbar_created`），那里会回调 [`Tray::add`] 重加图标。

use windows::core::{w, PCWSTR, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos,
    GetWindowLongPtrW, LoadImageW, PostMessageW, RegisterClassW, SetForegroundWindow,
    TrackPopupMenuEx, GWLP_USERDATA, IMAGE_ICON, LR_DEFAULTCOLOR, MF_CHECKED, MF_ENABLED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::event::TrayCommand;
use crate::i18n::{self, Lang};
use crate::platform::wide::to_wide;

/// 托盘窗口类名。
pub const TRAY_CLASS_NAME: PCWSTR = w!("BarlineTrayWnd");
/// 托盘图标 id（`Shell_NotifyIconW` 的 `uID`）。
const TRAY_UID: u32 = 1;
/// 图标资源 id（`assets/barline.rc` 里写死为 1）。
const ICON_RESOURCE_ID: usize = 1;
/// 菜单命令 id。
const CMD_OPEN: usize = 101;
const CMD_TOGGLE: usize = 102;
const CMD_QUIT: usize = 103;
/// v1.9.0（G7）：拉黑/恢复当前应用——共用一个 id，语义由 `App` 决定。
/// 托盘菜单（101..104）与小组件右键菜单（`MenuCommand` 1..8）是两套独立
/// HMENU 命名空间，互不冲突。
const CMD_IGNORE: usize = 104;

/// 托盘菜单「拉黑/恢复当前应用」项的内容（v1.9.0 / G7）。
///
/// 菜单项由 `App` 决定（文案 / 置灰），`tray` 只负责画——这样 `tray.rs`
/// 不依赖 `i18n` 之外的任何业务状态，也不必认识"黑名单"。
pub struct IgnoreMenuItem {
    pub label: String,
    pub enabled: bool,
}

/// 应用名截断：**按 `char` 计数**（不是字节），超长截到 `max_chars` 个字符
/// （含省略号）并在尾部加 `…`。
///
/// 为什么必须按 char：`"网易云音乐".len()==15` 字节但 `chars().count()==5`，
/// 字节截断会把 UTF-8 切碎 → panic。这是"中文截断"必踩的坑。
pub fn clamp_label(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// 托盘图标句柄 + 宿主窗口。
pub struct Tray {
    hwnd: HWND,
    icon: isize,
    added: bool,
}

/// 托盘菜单「显示/隐藏小组件」的勾选态：**小组件正在显示 → 打钩**。
///
/// 单独抽成纯函数，是因为这里历史上出过一个反向 bug：`show_menu` 直接把
/// `config.general.widget_hidden`（是否**隐藏**）当成勾选态传进去，
/// 结果「隐藏时打钩、显示时没钩」，与开关语义完全相反。
/// 现在调用链上**只允许经过本函数**做换算，调用点不许自己写取反。
pub fn toggle_checked(widget_hidden: bool) -> bool {
    !widget_hidden
}

impl Tray {
    /// 注册窗口类并创建一个隐藏的顶层窗口。
    ///
    /// `user_data` 会在创建后写进 `GWLP_USERDATA`（与小组件窗口同样的约定）。
    pub fn install(hinstance: HINSTANCE, user_data: isize) -> Result<Self> {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_proc),
            hInstance: hinstance,
            lpszClassName: TRAY_CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: wc 是本地栈结构体、字段已填全；重复注册返回 0 不视为错误。
        unsafe {
            RegisterClassW(&wc);
        }
        // SAFETY: 类已注册；父窗口为 None 保证是 top-level（前台化菜单需要）；
        // 尺寸为 0 且不 Show 出来——它只用来收消息。
        let hwnd = unsafe {
            windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                WS_EX_TOOLWINDOW,
                TRAY_CLASS_NAME,
                w!("BarlineTray"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )?
        };
        if user_data != 0 {
            // SAFETY: hwnd 刚创建且有效；只写一个指针值到 GWLP_USERDATA。
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                    user_data,
                );
            }
        }
        // SAFETY: 图标资源由本模块的 exe 持有，进程存活期间一直有效。
        let icon = unsafe {
            LoadImageW(
                Some(hinstance),
                PCWSTR(ICON_RESOURCE_ID as *const u16),
                IMAGE_ICON,
                16,
                16,
                LR_DEFAULTCOLOR,
            )
        }
        .map(|h| h.0 as isize)
        .unwrap_or(0);

        crate::log_debug!("托盘窗口已创建：hwnd=0x{:X} icon={icon}", hwnd.0 as usize);
        Ok(Self {
            hwnd,
            icon,
            added: false,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// `NIM_ADD`：常驻通知区。Explorer 重启后要再调一次。
    pub fn add(&mut self, lang: Lang) {
        let mut d = self.data(lang);
        // SAFETY: d 是本地结构体且 cbSize 已填；Shell_NotifyIconW 只读它。
        let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &d) };
        self.added = ok.as_bool();
        if self.added {
            crate::log_info!("托盘图标已添加（tip={}）", i18n::t(lang, "tray.tip"));
        } else {
            crate::log_warn!("托盘图标添加失败（Explorer 可能未就绪）");
        }
        let _ = &mut d;
    }

    /// `NIM_MODIFY`：语言 / tooltip 变化后刷新。
    pub fn refresh(&mut self, lang: Lang) {
        if !self.added {
            self.add(lang);
            return;
        }
        let d = self.data(lang);
        // SAFETY: 同上。
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) }.as_bool() {
            crate::log_debug!("托盘图标已刷新");
        } else {
            // 图标可能因为 Explorer 重启掉了，重新加一次
            self.added = false;
            self.add(lang);
        }
    }

    /// `NIM_DELETE`：退出前必须调，否则通知区留一个死图标。
    pub fn delete(&mut self) {
        if !self.added {
            return;
        }
        let d = self.data(Lang::Zh);
        // SAFETY: 同上。
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
        self.added = false;
        crate::log_debug!("托盘图标已移除");
    }

    /// 弹一次气泡提示（`NIF_INFO`）。
    ///
    /// 用途：首启发现快捷键被占用时提示用户改绑（不弹 MessageBox 打断操作）。
    /// `warning == true` 用黄色感叹号图标。
    pub fn show_balloon(&self, lang: Lang, title: &str, text: &str, warning: bool) {
        if !self.added {
            return;
        }
        let mut d = self.data(lang);
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        // SAFETY: d 是本地结构体且 cbSize 已填；NIM_MODIFY + NIF_INFO 是文档规定的气泡用法。
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) }.as_bool() {
            crate::log_info!("托盘气泡已弹出：{title} / {text}");
        } else {
            crate::log_warn!("托盘气泡弹出失败");
        }
    }
}

impl Tray {
    /// 弹出右键菜单并返回选中的命令。
    ///
    /// `widget_visible` = **小组件当前是否显示**（勾选态语义）。
    /// 传进来的必须是"可见与否"，不是配置里的 `widget_hidden`——
    /// 换算统一走 [`toggle_checked`]，避免再次传反。
    ///
    /// `ignore`（v1.9.0 / G7）：「拉黑/恢复当前应用」项的文案与置灰，由 `App` 组装
    /// （读内存态当前会话身份，**不调 SMTC**）。
    pub fn show_menu(
        &self,
        lang: Lang,
        widget_visible: bool,
        ignore: IgnoreMenuItem,
    ) -> Option<TrayCommand> {
        // SAFETY: 菜单句柄由本函数创建并在末尾销毁；所有字符串缓冲在调用期间存活。
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            append(menu, i18n::t(lang, "menu.settings"), CMD_OPEN, false, true);
            append(
                menu,
                i18n::t(lang, "menu.toggle_widget"),
                CMD_TOGGLE,
                widget_visible,
                true,
            );
            // G7：落位 = 设置 / 显示隐藏 / 拉黑当前应用 / ─── / 退出
            append(menu, &ignore.label, CMD_IGNORE, false, ignore.enabled);
            // SAFETY: 分隔线不需要文本。
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            append(menu, i18n::t(lang, "menu.quit"), CMD_QUIT, false, true);

            // MSDN：弹出前必须把宿主窗口设为前台，否则点别处菜单不消失。
            // 宿主是隐藏窗口时 SetForegroundWindow 可能失败，这里显式记日志便于诊断。
            // 先抢前台（用户正在操作别的程序时 SetForegroundWindow 会被拒）
            crate::platform::force_foreground(self.hwnd);
            let fg_ok = SetForegroundWindow(self.hwnd).as_bool();
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            crate::log_info!(
                "托盘菜单：SetForegroundWindow={fg_ok} 光标=({},{})",
                pt.x,
                pt.y
            );
            let cmd = TrackPopupMenuEx(
                menu,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                pt.x,
                pt.y,
                self.hwnd,
                None,
            )
            .0 as usize;
            crate::log_info!("托盘菜单返回 cmd={cmd}");
            // 菜单关闭后补一条空消息，修复「菜单残留」的经典问题
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);

            match cmd {
                CMD_OPEN => Some(TrayCommand::OpenSettings),
                CMD_TOGGLE => Some(TrayCommand::ToggleWidget),
                CMD_IGNORE => Some(TrayCommand::ToggleIgnoreCurrent),
                CMD_QUIT => Some(TrayCommand::Quit),
                _ => None,
            }
        }
    }

    /// 组装 `NOTIFYICONDATAW`。
    fn data(&self, lang: Lang) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: crate::platform::wake::WM_BARLINE_TRAY,
            hIcon: windows::Win32::UI::WindowsAndMessaging::HICON(self.icon as *mut core::ffi::c_void),
            ..Default::default()
        };
        copy_into(&mut d.szTip, i18n::t(lang, "tray.tip"));
        d
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.delete();
        if !self.hwnd.is_invalid() {
            // SAFETY: hwnd 由本结构体独占；DestroyWindow 后句柄失效，结构体随即丢弃。
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// 把 `&str` 拷进定长 UTF-16 缓冲（截断而不是 panic）。
fn copy_into(dst: &mut [u16], src: &str) {
    let wide = to_wide(src);
    let n = wide.len().min(dst.len());
    dst[..n].copy_from_slice(&wide[..n]);
}

/// 追加一个菜单项（v1.9.0 / G7：扩成五参）。
///
/// `checked` 为真时带勾；`enabled == false` 时置灰（`MF_GRAYED`）。
/// `MF_UNCHECKED` 与 `MF_GRAYED` 都是 0 值，直接 `|` 无副作用。
fn append(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    label: &str,
    id: usize,
    checked: bool,
    enabled: bool,
) {
    let wide = to_wide(label);
    let flags = MF_STRING
        | if checked { MF_CHECKED } else { MF_UNCHECKED }
        | if enabled { MF_ENABLED } else { MF_GRAYED };
    // SAFETY: menu 有效；wide 在调用期间存活且以 NUL 结尾。
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
    }
}

/// 托盘窗口过程：只处理回调消息，其余交回系统。
unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == crate::platform::wake::WM_BARLINE_TRAY {
        // SAFETY: 只读 GWLP_USERDATA 槽（存的是 App* 或 0），不解引用空指针。
        let app = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut crate::app::App };
        if !app.is_null() {
            // SAFETY: 本函数在 UI 线程执行，app 由 App::bootstrap 的 Box 持有，
            // 生命周期覆盖整个消息泵，不存在并发别名。
            if let Some(r) = unsafe { (*app).on_tray_message(lparam) } {
                return r;
            }
        }
        return LRESULT(0);
    }
    // SAFETY: 未处理的消息交回系统默认处理。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 从回调的 `lparam` 解析鼠标事件（`LOWORD(lparam)`）。
pub fn mouse_event_of(lparam: LPARAM) -> u32 {
    (lparam.0 as u32) & 0xFFFF
}

/// 是否是「右键抬起」（弹菜单）。
pub fn is_right_up(lparam: LPARAM) -> bool {
    mouse_event_of(lparam) == WM_RBUTTONUP
}

/// 是否是「左键双击」（打开设置）。
pub fn is_double_click(lparam: LPARAM) -> bool {
    mouse_event_of(lparam) == WM_LBUTTONDBLCLK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v1.3.1：托盘菜单勾选态 = 「小组件正在显示」。
    ///
    /// **历史上这里传错成 `widget_hidden`**（是否隐藏），导致「隐藏时打钩、
    /// 显示时没钩」——与开关语义完全相反。这条测试把正确的换算钉死。
    #[test]
    fn toggle_checked_is_true_when_visible() {
        // 小组件显示中（widget_hidden = false）→ 打钩
        assert!(toggle_checked(false), "显示中必须打钩");
        // 小组件已隐藏（widget_hidden = true）→ 不打钩
        assert!(!toggle_checked(true), "隐藏时不许打钩");
    }

    #[test]
    fn lparam_low_word_is_the_mouse_event() {
        // 回调消息的 lParam 低 16 位是鼠标事件，高位是图标 id
        let lp = LPARAM(((TRAY_UID as isize) << 16) | WM_RBUTTONUP as isize);
        assert!(is_right_up(lp));
        assert!(!is_double_click(lp));
        let lp2 = LPARAM(((TRAY_UID as isize) << 16) | WM_LBUTTONDBLCLK as isize);
        assert!(is_double_click(lp2));
        assert!(!is_right_up(lp2));
    }

    // ------------------------------------------ v1.9.0（G7）：clamp_label

    /// **按 char 截断**——字节截断会把 UTF-8 切碎 panic；这是"中文截断"必踩的坑。
    #[test]
    fn clamp_label_counts_chars_not_bytes() {
        // 5 个中文字符（15 字节）远小于 40 → 原样
        assert_eq!(clamp_label("网易云音乐", 40), "网易云音乐");
        // 45 个中文字符 → 截到 40 char（39 字符 + …）
        let long = "音".repeat(45);
        let clamped = clamp_label(&long, 40);
        assert_eq!(clamped.chars().count(), 40, "必须按 char 计数");
        assert!(clamped.ends_with('…'));
        assert_eq!(clamped.chars().next_back().unwrap(), '…');
    }

    /// 中英混排 + UTF-8 边界：截断点落在多字节字符上不许 panic。
    #[test]
    fn clamp_label_handles_mixed_scripts_and_char_boundaries() {
        let mixed = "CloudMusic网e易云音3乐AB乐";
        let clamped = clamp_label(mixed, 7);
        assert_eq!(clamped.chars().count(), 7);
        assert!(clamped.ends_with('…'));
        // 刚好 40 → 原样不加省略号
        let exact = "a".repeat(40);
        assert_eq!(clamp_label(&exact, 40), exact);
        let exact_zh = "曲".repeat(40);
        assert_eq!(clamp_label(&exact_zh, 40), exact_zh, "恰好 40 char 不截断");
        // max_chars=1 → 只有省略号
        assert_eq!(clamp_label("abcdef", 1), "…");
    }
}
