//! 全局快捷键（`TASKS.md` T15 / PRD P1-02）。
//!
//! 6 组动作固定映射到 id 1..=6（`WM_HOTKEY` 的 `wParam`），注册到**消息窗**上
//! （`RegisterHotKey(Some(msg_hwnd), ..)`），这样热键消息与其它跨线程唤醒走同一条路径。
//!
//! ## 冲突处理（R4 拍板）
//!
//! `RegisterHotKey` 失败且 `GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED` 时：
//! **只记录、不阻止启动**。日志写清哪个组合键注册失败、错误码是什么，
//! 设置页据此把该项标红，但允许用户坚持保留（此时该键在系统层面不生效）。
//!
//! ## 录制期间挂起（PRD P1-02）
//!
//! 设置页录制新快捷键时，先 [`Hotkeys::suspend`] 全部注销，否则用户按下旧组合键
//! 会直接触发动作，录制框里就录不到了。

use windows::Win32::Foundation::{HWND, WIN32_ERROR};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
};

use crate::config::HotkeyBinding;
use crate::event::HotkeyAction;

/// `ERROR_HOTKEY_ALREADY_REGISTERED`。
pub const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;

/// 动作 → 注册 id（1..=6）。`0` 表示无效。
pub fn id_of(action: HotkeyAction) -> i32 {
    match action {
        HotkeyAction::PlayPause => 1,
        HotkeyAction::Prev => 2,
        HotkeyAction::Next => 3,
        HotkeyAction::ToggleLyrics => 4,
        HotkeyAction::Mute => 5,
        HotkeyAction::ToggleWidget => 6,
    }
}

/// 注册 id → 动作。
pub fn action_of_id(id: i32) -> Option<HotkeyAction> {
    Some(match id {
        1 => HotkeyAction::PlayPause,
        2 => HotkeyAction::Prev,
        3 => HotkeyAction::Next,
        4 => HotkeyAction::ToggleLyrics,
        5 => HotkeyAction::Mute,
        6 => HotkeyAction::ToggleWidget,
        _ => return None,
    })
}

/// 把一组绑定渲染成**人可读**文本（日志专用）：`Ctrl+Alt+P`、`Ctrl+Alt+←`。
///
/// v1.2.3（G4）：默认值回到 `Ctrl+Alt+*` 之后，`P / L / M / H / ← / →` 这几组
/// 在这台机器上**历史上曾被其它软件占用（错误码 1409）**，所以注册失败的日志
/// 必须能一眼看出「哪一组动作 + 什么组合 + 什么错误码」，用户复现时直接截图即可。
/// 格式与设置页的 `bindingText()` 保持一致（同一套修饰键顺序与键名）。
pub fn describe(binding: HotkeyBinding) -> String {
    use crate::config::defaults::mods;
    let mut out = String::with_capacity(24);
    if binding.modifiers & mods::CONTROL != 0 {
        out.push_str("Ctrl+");
    }
    if binding.modifiers & mods::ALT != 0 {
        out.push_str("Alt+");
    }
    if binding.modifiers & mods::SHIFT != 0 {
        out.push_str("Shift+");
    }
    if binding.modifiers & mods::WIN != 0 {
        out.push_str("Win+");
    }
    push_vk_name(&mut out, binding.vk);
    out
}

/// 键码 → 可读名（只覆盖常用键，其余退化为 `VK(0x..)`，永不失败）。
fn push_vk_name(out: &mut String, vk: u32) {
    match vk {
        0x08 => out.push_str("Backspace"),
        0x09 => out.push_str("Tab"),
        0x0D => out.push_str("Enter"),
        0x1B => out.push_str("Esc"),
        0x20 => out.push_str("Space"),
        0x25 => out.push('←'),
        0x26 => out.push('↑'),
        0x27 => out.push('→'),
        0x28 => out.push('↓'),
        0x30..=0x39 | 0x41..=0x5A => {
            // 0x30..=0x39 是 '0'..='9'，0x41..=0x5A 是 'A'..='Z'（ASCII 码点即字符）
            match char::from_u32(vk) {
                Some(c) => out.push(c),
                None => out.push('?'),
            }
        }
        0x70..=0x87 => {
            out.push('F');
            out.push_str(&(vk - 0x6F).to_string());
        }
        other => {
            out.push_str("VK(0x");
            out.push_str(&format!("{other:02X}"));
            out.push(')');
        }
    }
}

/// 一次注册失败的记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conflict {
    pub action: HotkeyAction,
    pub binding: HotkeyBinding,
    /// Win32 错误码（1409 = 已被占用）
    pub code: u32,
}

/// 已注册的热键集合。
#[derive(Debug, Default)]
pub struct Hotkeys {
    /// 当前真正注册成功的动作
    active: Vec<HotkeyAction>,
    /// 上次注册时的冲突列表（设置页据此标红）
    pub conflicts: Vec<Conflict>,
    /// 是否处于挂起（录制）状态
    suspended: bool,
}

impl Hotkeys {
    /// 注册全部 6 组。已注册的先注销，避免重复注册失败。
    pub fn register_all(&mut self, hwnd: HWND, cfg: &crate::config::HotkeyConfig) {
        self.unregister_all(hwnd);
        self.conflicts.clear();
        self.suspended = false;
        if !cfg.enabled {
            crate::log_info!("全局快捷键：配置里已关闭，跳过注册");
            return;
        }
        for (action, binding) in bindings(cfg) {
            if !binding.is_bound() {
                continue;
            }
            let mods = HOT_KEY_MODIFIERS(binding.modifiers | MOD_NOREPEAT.0);
            // SAFETY: hwnd 是消息窗（有效）；id 固定 1..=6 且互不重复；
            // 修饰位与虚拟键码都来自配置（用户可改，非法值只会让注册失败）。
            let r = unsafe { RegisterHotKey(Some(hwnd), id_of(action), mods, binding.vk) };
            match r {
                Ok(()) => {
                    self.active.push(action);
                    crate::log_info!(
                        "热键已注册：{} = {}",
                        action.as_str(),
                        describe(*binding)
                    );
                }
                Err(e) => {
                    let code = e.code().0 as u32;
                    // v1.2.3：日志必须带「哪一组 + 什么组合 + 错误码」，用户复现时直接截图
                    crate::log_warn!(
                        "热键注册失败：{} = {}，错误码 {}（{}）",
                        action.as_str(),
                        describe(*binding),
                        code,
                        if code == ERROR_HOTKEY_ALREADY_REGISTERED {
                            "已被其它程序占用"
                        } else {
                            "其它错误"
                        }
                    );
                    self.conflicts.push(Conflict {
                        action,
                        binding: *binding,
                        code,
                    });
                }
            }
        }
        crate::log_info!(
            "全局快捷键：{} 组生效，{} 组冲突",
            self.active.len(),
            self.conflicts.len()
        );
    }

    /// 注销全部已注册项。
    pub fn unregister_all(&mut self, hwnd: HWND) {
        for action in self.active.drain(..) {
            // SAFETY: 只注销自己注册过的 id；重复注销返回错误可忽略。
            let _ = unsafe { UnregisterHotKey(Some(hwnd), id_of(action)) };
        }
    }

    /// 录制期间挂起（全部注销）。
    pub fn suspend(&mut self, hwnd: HWND) {
        if self.suspended {
            return;
        }
        self.unregister_all(hwnd);
        self.suspended = true;
        crate::log_info!("全局快捷键已挂起（录制中）");
    }

    /// 录制结束恢复。
    pub fn resume(&mut self, hwnd: HWND, cfg: &crate::config::HotkeyConfig) {
        if !self.suspended {
            return;
        }
        self.register_all(hwnd, cfg);
        crate::log_info!("全局快捷键已恢复");
    }

    /// 当前生效的动作数（诊断用）。
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// 该动作是否注册成功。
    pub fn is_active(&self, action: HotkeyAction) -> bool {
        self.active.contains(&action)
    }
}

/// 配置里的 6 组绑定（顺序固定，便于日志对齐）。
pub fn bindings(cfg: &crate::config::HotkeyConfig) -> [(HotkeyAction, &HotkeyBinding); 6] {
    [
        (HotkeyAction::PlayPause, &cfg.play_pause),
        (HotkeyAction::Prev, &cfg.prev),
        (HotkeyAction::Next, &cfg.next),
        (HotkeyAction::ToggleLyrics, &cfg.toggle_lyrics),
        (HotkeyAction::Mute, &cfg.mute),
        (HotkeyAction::ToggleWidget, &cfg.toggle_widget),
    ]
}

/// `WM_HOTKEY` 的 `wParam` → 动作。
pub fn action_of_wparam(wparam: usize) -> Option<HotkeyAction> {
    action_of_id(wparam as i32)
}

/// 把 `WIN32_ERROR` 转成可读名字（日志用）。
pub fn error_name(code: u32) -> &'static str {
    match code {
        ERROR_HOTKEY_ALREADY_REGISTERED => "ERROR_HOTKEY_ALREADY_REGISTERED",
        _ => "UNKNOWN",
    }
}

/// 诊断用：`WIN32_ERROR` 的原始值。
pub fn raw_code(e: &windows::core::Error) -> u32 {
    WIN32_ERROR(e.code().0 as u32).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_roundtrip() {
        for (i, a) in [
            HotkeyAction::PlayPause,
            HotkeyAction::Prev,
            HotkeyAction::Next,
            HotkeyAction::ToggleLyrics,
            HotkeyAction::Mute,
            HotkeyAction::ToggleWidget,
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(id_of(*a), i as i32 + 1);
            assert_eq!(action_of_id(i as i32 + 1), Some(*a));
        }
    }

    #[test]
    fn unknown_ids_are_ignored() {
        assert_eq!(action_of_id(0), None);
        assert_eq!(action_of_id(7), None);
        assert_eq!(action_of_id(-1), None);
    }

    /// v1.2.3（G4）：注册日志要能一眼看出「哪一组 + 什么组合」。
    /// 这里钉住默认 6 组的可读文本，并验证冷门键位的兜底格式不 panic。
    #[test]
    fn describe_renders_default_combos() {
        use crate::config::defaults::{mods, vk};
        let b = |vk| HotkeyBinding {
            modifiers: mods::CONTROL | mods::ALT,
            vk,
        };
        assert_eq!(describe(b(vk::P)), "Ctrl+Alt+P");
        assert_eq!(describe(b(vk::L)), "Ctrl+Alt+L");
        assert_eq!(describe(b(vk::M)), "Ctrl+Alt+M");
        assert_eq!(describe(b(vk::H)), "Ctrl+Alt+H");
        assert_eq!(describe(b(vk::LEFT)), "Ctrl+Alt+←");
        assert_eq!(describe(b(vk::RIGHT)), "Ctrl+Alt+→");

        // 三代默认值里的 v1（带 Shift）
        assert_eq!(
            describe(HotkeyBinding {
                modifiers: mods::CONTROL | mods::ALT | mods::SHIFT,
                vk: vk::P
            }),
            "Ctrl+Alt+Shift+P"
        );
        // 冷门键位退化为 VK(0x..)，不 panic、不丢信息
        assert_eq!(
            describe(HotkeyBinding {
                modifiers: mods::WIN,
                vk: 0xE0
            }),
            "Win+VK(0xE0)"
        );
    }

    #[test]
    fn wparam_maps_to_action() {
        assert_eq!(action_of_wparam(1), Some(HotkeyAction::PlayPause));
        assert_eq!(action_of_wparam(6), Some(HotkeyAction::ToggleWidget));
        assert_eq!(action_of_wparam(99), None);
    }

    #[test]
    fn bindings_follow_config_order() {
        let mut cfg = crate::config::HotkeyConfig::default();
        cfg.play_pause = HotkeyBinding { modifiers: 3, vk: 80 };
        let list = bindings(&cfg);
        assert_eq!(list[0].0, HotkeyAction::PlayPause);
        assert_eq!(list[0].1.vk, 80);
        assert_eq!(list.len(), 6);
    }

    #[test]
    fn conflict_error_name_is_recognised() {
        assert_eq!(
            error_name(ERROR_HOTKEY_ALREADY_REGISTERED),
            "ERROR_HOTKEY_ALREADY_REGISTERED"
        );
    }
}
