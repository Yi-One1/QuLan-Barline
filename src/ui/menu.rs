//! 小组件右键上下文菜单（PRD §4.3 / T08 第 6 条）。
//!
//! 菜单项（v1.1.1 起行数固定单行；v1.1.11 起背景无模糊；**v1.1.16 起删除
//! 随机/循环两项**——SMTC 远程变更只是「请求」，网易云/汽水等大量播放器
//! 声明能力却不响应，功能形同虚设）：
//! ```text
//! ✓ 显示歌词            ← ToggleLyrics
//! ─────────────────
//!   ● 背景：半透明 / ○ 背景：透明  ← BgTranslucent / BgTransparent
//! ─────────────────
//!   显示设置 / 关于 / 退出
//! ```
//!
//! ## 两个必须照做的细节
//!
//! 1. **`TrackPopupMenuEx` 之前要 `SetForegroundWindow`**，否则菜单点外面不消失
//!    （这是 Win32 菜单的经典坑，MSDN 明确要求）。之后补一条 `WM_NULL` 更稳。
//! 2. 用 `TPM_RETURNCMD` 同步取回命令号，**不用** `WM_COMMAND`——
//!    我们的窗口没有菜单栏，走 `WM_COMMAND` 会和任务栏的消息流混在一起。

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow, TrackPopupMenuEx,
    HMENU, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::config::Config;
use crate::i18n;
use crate::platform::wide::to_wide;
use crate::render::theme::BackgroundMode;

/// 菜单命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    ToggleLyrics,
    BgTranslucent,
    BgTransparent,
    OpenSettings,
    ShowAbout,
    Quit,
}

impl MenuCommand {
    /// 菜单项 ID（同时是 `TrackPopupMenuEx` 的返回值）。
    pub fn id(self) -> usize {
        match self {
            MenuCommand::ToggleLyrics => 1,
            MenuCommand::BgTranslucent => 2,
            MenuCommand::BgTransparent => 3,
            MenuCommand::OpenSettings => 4,
            MenuCommand::ShowAbout => 5,
            MenuCommand::Quit => 6,
        }
    }

    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            1 => MenuCommand::ToggleLyrics,
            2 => MenuCommand::BgTranslucent,
            3 => MenuCommand::BgTransparent,
            4 => MenuCommand::OpenSettings,
            5 => MenuCommand::ShowAbout,
            6 => MenuCommand::Quit,
            _ => return None,
        })
    }
}

/// 弹出菜单并返回用户选择的命令（取消返回 `None`）。
///
/// `screen` 是弹出位置的屏幕坐标（一般是右键点击点）。
pub fn show(hwnd: HWND, screen: (i32, i32), config: &Config, lang: i18n::Lang) -> Option<MenuCommand> {
    // SAFETY: 菜单句柄由本函数创建并在末尾销毁；所有字符串缓冲在调用期间保持存活。
    unsafe {
        let menu: HMENU = CreatePopupMenu().ok()?;

        let lyrics_on = config.lyrics.enabled;
        let translucent = config.appearance.background_mode == BackgroundMode::Translucent;

        add(menu, i18n::t(lang, "menu.toggle_lyrics"), MenuCommand::ToggleLyrics, true, lyrics_on);

        separator(menu);

        add(
            menu,
            i18n::t(lang, "menu.bg_translucent"),
            MenuCommand::BgTranslucent,
            true,
            translucent,
        );
        add(
            menu,
            i18n::t(lang, "menu.bg_transparent"),
            MenuCommand::BgTransparent,
            true,
            !translucent,
        );

        separator(menu);

        add(menu, i18n::t(lang, "menu.settings"), MenuCommand::OpenSettings, true, false);
        add(menu, i18n::t(lang, "menu.about"), MenuCommand::ShowAbout, true, false);
        add(menu, i18n::t(lang, "menu.quit"), MenuCommand::Quit, true, false);

        // MSDN 要求：弹出前把宿主窗口设为前台，否则点击别处菜单不消失
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(
            menu,
            (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
            screen.0,
            screen.1,
            hwnd,
            None,
        )
        .0;
        // 菜单关闭后补一条空消息，修复「菜单残留在屏幕上」的经典问题
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        MenuCommand::from_id(cmd)
    }
}

/// 追加一个可勾选项。`enabled == false` 时置灰。
fn add(menu: HMENU, label: &str, cmd: MenuCommand, enabled: bool, checked: bool) {
    let wide = to_wide(label);
    let mut flags = MF_STRING;
    if checked {
        flags |= MF_CHECKED;
    }
    if !enabled {
        flags |= MF_GRAYED;
    }
    // SAFETY: wide 是 NUL 结尾缓冲，生命周期覆盖调用；menu 由调用方持有。
    unsafe {
        let _ = AppendMenuW(menu, flags, cmd.id(), PCWSTR(wide.as_ptr()));
    }
}

/// 追加分隔线。
fn separator(menu: HMENU) {
    // SAFETY: 分隔线不需要字符串，lpnewitem 传 null。
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

/// 把客户区坐标转成屏幕坐标（菜单需要屏幕坐标）。
///
/// 这里不调 `ClientToScreen`，而是由调用方直接给屏幕坐标——因为 `App` 手上已经有
/// `TaskbarGeometry::target`（屏幕矩形），加一下偏移即可，省一次 API 调用。
pub fn client_to_screen(origin: (i32, i32), client: (i32, i32)) -> (i32, i32) {
    (origin.0 + client.0, origin.1 + client.1)
}

/// 取鼠标当前位置（菜单弹出点兜底用）。
pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: p 是本地结构体，GetCursorPos 只写出它。
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) }.is_ok() {
        (p.x, p.y)
    } else {
        (0, 0)
    }
}

/// 菜单项 ID 的调试名。
pub fn command_name(cmd: MenuCommand) -> &'static str {
    match cmd {
        MenuCommand::ToggleLyrics => "ToggleLyrics",
        MenuCommand::BgTranslucent => "BgTranslucent",
        MenuCommand::BgTransparent => "BgTransparent",
        MenuCommand::OpenSettings => "OpenSettings",
        MenuCommand::ShowAbout => "ShowAbout",
        MenuCommand::Quit => "Quit",
    }
}

/// 「关于」对话框用的常量（避免 app.rs 里塞字面量）。
pub const ABOUT_OK: PCWSTR = w!("OK");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_roundtrip() {
        let all = [
            MenuCommand::ToggleLyrics,
            MenuCommand::BgTranslucent,
            MenuCommand::BgTransparent,
            MenuCommand::OpenSettings,
            MenuCommand::ShowAbout,
            MenuCommand::Quit,
        ];
        for (i, a) in all.iter().enumerate() {
            assert_eq!(MenuCommand::from_id(a.id() as i32), Some(*a));
            for b in all.iter().skip(i + 1) {
                assert_ne!(a.id(), b.id(), "菜单 ID 重复");
            }
        }
        assert_eq!(MenuCommand::from_id(0), None);
        assert_eq!(MenuCommand::from_id(99), None);
    }

    #[test]
    fn client_to_screen_adds_origin() {
        assert_eq!(client_to_screen((100, 200), (10, 5)), (110, 205));
    }

    #[test]
    fn command_names_are_stable() {
        assert_eq!(command_name(MenuCommand::Quit), "Quit");
    }
}
