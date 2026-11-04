//! 小组件右键上下文菜单（PRD §4.3 / T08 第 6 条）。
//!
//! 菜单项（v1.1.1 起行数固定单行；v1.1.11 起背景无模糊；**v1.1.16 起删除
//! 随机/循环两项**——SMTC 远程变更只是「请求」，网易云/汽水等大量播放器
//! 声明能力却不响应，功能形同虚设；**v1.2.1 起背景从 2 项扩成 4 项**——
//! 常规背景与悬停背景各有透明/半透明两档，两组各自独立勾选）：
//! ```text
//! ✓ 显示歌词                        ← ToggleLyrics
//! ─────────────────
//! ✓ 常规背景：透明 /   常规背景：半透明   ← IdleTransparent / IdleTranslucent
//! ✓ 悬停背景：透明 /   悬停背景：半透明   ← HoverTransparent / HoverTranslucent
//! ─────────────────
//!   设置 / 关于 / 退出
//! ```
//!
//! 两组的项内顺序统一为「透明 → 半透明」，与设置页的两个下拉一致；
//! 术语也统一：`idle` = 常规背景（不悬停时），`hover` = 悬停背景。
//!
//! 文案一律走 `i18n::t(lang, "menu.*")`，与托盘菜单共用同一套词条
//! （v1.1.22 起 `menu.settings` 中文为「设置」，不再是「显示设置」）。
//! 托盘菜单（`ui::tray`）**没有**背景项，故意不跟着扩。
//!
//! ## 两个必须照做的细节
//!
//! 1. **`TrackPopupMenuEx` 之前要 `SetForegroundWindow`**，否则菜单点外面不消失
//!    （这是 Win32 菜单的经典坑，MSDN 明确要求）。之后补一条 `WM_NULL` 更稳。
//! 2. 用 `TPM_RETURNCMD` 同步取回命令号，**不用** `WM_COMMAND`——
//!    我们的窗口没有菜单栏，走 `WM_COMMAND` 会和任务栏的消息流混在一起。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow, TrackPopupMenuEx,
    HMENU, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::config::Config;
use crate::i18n;
use crate::platform::wide::to_wide;
use crate::render::theme::BackgroundMode;

/// 背景设置落到的**配置槽位**（v1.2.1）。
///
/// 两个槽位对应 `AppearanceConfig` 的两个字段，互不影响：
/// * [`BgSlot::Idle`] → `appearance.idle_mode`（常规背景，不悬停时）
/// * [`BgSlot::Hover`] → `appearance.background_mode`（悬停背景）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgSlot {
    Idle,
    Hover,
}

impl BgSlot {
    /// 日志 / 配置键名用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            BgSlot::Idle => "idle",
            BgSlot::Hover => "hover",
        }
    }
}

/// 菜单命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    ToggleLyrics,
    /// 常规背景 = 透明
    IdleTransparent,
    /// 常规背景 = 半透明
    IdleTranslucent,
    /// 悬停背景 = 透明
    HoverTransparent,
    /// 悬停背景 = 半透明
    HoverTranslucent,
    OpenSettings,
    ShowAbout,
    Quit,
}

impl MenuCommand {
    /// 菜单项 ID（同时是 `TrackPopupMenuEx` 的返回值）。
    ///
    /// v1.2.1：背景 4 项占 2..=5，其后整体后移（`OpenSettings` 4→6 等）。
    pub fn id(self) -> usize {
        match self {
            MenuCommand::ToggleLyrics => 1,
            MenuCommand::IdleTransparent => 2,
            MenuCommand::IdleTranslucent => 3,
            MenuCommand::HoverTransparent => 4,
            MenuCommand::HoverTranslucent => 5,
            MenuCommand::OpenSettings => 6,
            MenuCommand::ShowAbout => 7,
            MenuCommand::Quit => 8,
        }
    }

    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            1 => MenuCommand::ToggleLyrics,
            2 => MenuCommand::IdleTransparent,
            3 => MenuCommand::IdleTranslucent,
            4 => MenuCommand::HoverTransparent,
            5 => MenuCommand::HoverTranslucent,
            6 => MenuCommand::OpenSettings,
            7 => MenuCommand::ShowAbout,
            8 => MenuCommand::Quit,
            _ => return None,
        })
    }

    /// 若本命令是背景项，返回「写到哪个槽位 + 写成什么模式」。
    ///
    /// 抽出来的目的：让 `App` 侧只需要**一个** `set_background(slot, mode)`，
    /// 而不是四段复制粘贴的 match 分支（v1.2.1 之前两段就已经几乎一模一样）。
    /// 非背景命令返回 `None`。
    pub fn bg_target(self) -> Option<(BgSlot, BackgroundMode)> {
        Some(match self {
            MenuCommand::IdleTransparent => (BgSlot::Idle, BackgroundMode::Transparent),
            MenuCommand::IdleTranslucent => (BgSlot::Idle, BackgroundMode::Translucent),
            MenuCommand::HoverTransparent => (BgSlot::Hover, BackgroundMode::Transparent),
            MenuCommand::HoverTranslucent => (BgSlot::Hover, BackgroundMode::Translucent),
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
        // 两组勾选态各取各的配置，互不影响（v1.2.1）
        let idle = config.appearance.idle_mode;
        let hover = config.appearance.background_mode;
        let idle_transparent = idle == BackgroundMode::Transparent;
        let hover_transparent = hover == BackgroundMode::Transparent;

        add(menu, i18n::t(lang, "menu.toggle_lyrics"), MenuCommand::ToggleLyrics, true, lyrics_on);

        separator(menu);

        // 常规背景：透明 → 半透明
        add(
            menu,
            i18n::t(lang, "menu.bg.idle.transparent"),
            MenuCommand::IdleTransparent,
            true,
            idle_transparent,
        );
        add(
            menu,
            i18n::t(lang, "menu.bg.idle.translucent"),
            MenuCommand::IdleTranslucent,
            true,
            !idle_transparent,
        );

        // 悬停背景：透明 → 半透明
        add(
            menu,
            i18n::t(lang, "menu.bg.hover.transparent"),
            MenuCommand::HoverTransparent,
            true,
            hover_transparent,
        );
        add(
            menu,
            i18n::t(lang, "menu.bg.hover.translucent"),
            MenuCommand::HoverTranslucent,
            true,
            !hover_transparent,
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

/// 菜单项 ID 的调试名。
pub fn command_name(cmd: MenuCommand) -> &'static str {
    match cmd {
        MenuCommand::ToggleLyrics => "ToggleLyrics",
        MenuCommand::IdleTransparent => "IdleTransparent",
        MenuCommand::IdleTranslucent => "IdleTranslucent",
        MenuCommand::HoverTransparent => "HoverTransparent",
        MenuCommand::HoverTranslucent => "HoverTranslucent",
        MenuCommand::OpenSettings => "OpenSettings",
        MenuCommand::ShowAbout => "ShowAbout",
        MenuCommand::Quit => "Quit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_roundtrip() {
        let all = [
            MenuCommand::ToggleLyrics,
            MenuCommand::IdleTransparent,
            MenuCommand::IdleTranslucent,
            MenuCommand::HoverTransparent,
            MenuCommand::HoverTranslucent,
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
        // v1.2.1 的 ID 布局是「歌词 1 / 背景 2..=5 / 其余 6..=8」，钉住防漂移
        for (cmd, want) in [
            (MenuCommand::ToggleLyrics, 1),
            (MenuCommand::IdleTransparent, 2),
            (MenuCommand::IdleTranslucent, 3),
            (MenuCommand::HoverTransparent, 4),
            (MenuCommand::HoverTranslucent, 5),
            (MenuCommand::OpenSettings, 6),
            (MenuCommand::ShowAbout, 7),
            (MenuCommand::Quit, 8),
        ] {
            assert_eq!(cmd.id(), want, "{} 的 ID 应为 {want}", command_name(cmd));
        }
    }

    /// v1.2.1（G2 问题 6）：4 个背景项各自映射到「哪个槽位 + 哪个模式」，
    /// 其余命令一律 `None`。槽位映射错了就会出现"点了常规背景却改了悬停背景"。
    #[test]
    fn bg_commands_map_to_slot_and_mode() {
        assert_eq!(
            MenuCommand::IdleTransparent.bg_target(),
            Some((BgSlot::Idle, BackgroundMode::Transparent))
        );
        assert_eq!(
            MenuCommand::IdleTranslucent.bg_target(),
            Some((BgSlot::Idle, BackgroundMode::Translucent))
        );
        assert_eq!(
            MenuCommand::HoverTransparent.bg_target(),
            Some((BgSlot::Hover, BackgroundMode::Transparent))
        );
        assert_eq!(
            MenuCommand::HoverTranslucent.bg_target(),
            Some((BgSlot::Hover, BackgroundMode::Translucent))
        );

        for cmd in [
            MenuCommand::ToggleLyrics,
            MenuCommand::OpenSettings,
            MenuCommand::ShowAbout,
            MenuCommand::Quit,
        ] {
            assert_eq!(
                cmd.bg_target(),
                None,
                "{} 不该被当成背景项",
                command_name(cmd)
            );
        }
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
