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
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos,
    GetWindowLongPtrW, LoadImageW, PostMessageW, RegisterClassW, SetForegroundWindow,
    TrackPopupMenuEx, GWLP_USERDATA, IMAGE_ICON, LR_DEFAULTCOLOR, MF_CHECKED, MF_SEPARATOR,
    MF_STRING, MF_UNCHECKED, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_LBUTTONDBLCLK, WM_NULL,
    WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
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

/// 托盘图标句柄 + 宿主窗口。
pub struct Tray {
    hwnd: HWND,
    icon: isize,
    added: bool,
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

    /// 弹出右键菜单并返回选中的命令。
    pub fn show_menu(&self, lang: Lang, hidden: bool) -> Option<TrayCommand> {
        // SAFETY: 菜单句柄由本函数创建并在末尾销毁；所有字符串缓冲在调用期间存活。
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            append(menu, i18n::t(lang, "menu.settings"), CMD_OPEN, false);
            append(menu, i18n::t(lang, "menu.toggle_widget"), CMD_TOGGLE, hidden);
            // SAFETY: 分隔线不需要文本。
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            append(menu, i18n::t(lang, "menu.quit"), CMD_QUIT, false);

            // MSDN：弹出前必须把宿主窗口设为前台，否则点别处菜单不消失。
            // 宿主是隐藏窗口时 SetForegroundWindow 可能失败，这里显式记日志便于诊断。
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
        let tip = to_wide(i18n::t(lang, "tray.tip"));
        let n = tip.len().min(d.szTip.len());
        d.szTip[..n].copy_from_slice(&tip[..n]);
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

/// 追加一个菜单项（`checked` 为真时带勾）。
fn append(menu: windows::Win32::UI::WindowsAndMessaging::HMENU, label: &str, id: usize, checked: bool) {
    let wide = to_wide(label);
    let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
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
}
