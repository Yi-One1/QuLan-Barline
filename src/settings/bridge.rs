//! 设置页 ↔ Rust 的消息协议（`ARCHITECTURE.md` §13.2）。
//!
//! 两个方向都是 JSON：
//! ```text
//! Rust → JS : { "type": "state", "payload": { config, version, conflicts, logDir } }
//!             { "type": "toast", "payload": { "level": "info", "text": "..." } }
//! JS → Rust : { "type": "set",    "payload": { "path": "lyrics.enabled", "value": true } }
//!             { "type": "action", "payload": "openLogDir" }
//!             { "type": "record", "payload": "start" | "stop" }
//! ```
//!
//! **`path` 走白名单 match，不用反射**（§13.2 第 5 条）：反射会让 JS 侧能写任意字段，
//! 一旦 HTML 被注入就是配置任意写。白名单只有下面 `apply_set` 里列出的那几条。

use serde_json::{json, Value};

use crate::config::{Config, HotkeyBinding, LangSerde, VolumeScope};
use crate::render::theme::BackgroundMode;

/// JS → Rust 的一条消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// 改一个配置项
    Set { path: String, value: Value },
    /// 触发一个动作（打开日志目录等）
    Action(String),
    /// 快捷键录制开始 / 结束（用于挂起全局热键）
    Record(bool),
}

/// 解析 JS 消息。非法消息返回 `None`（记日志后忽略，绝不让页面把宿主搞崩）。
pub fn parse(msg: &str) -> Option<Incoming> {
    let v: Value = serde_json::from_str(msg).ok()?;
    let kind = v.get("type")?.as_str()?;
    let payload = v.get("payload");
    match kind {
        "set" => {
            let p = payload?;
            Some(Incoming::Set {
                path: p.get("path")?.as_str()?.to_string(),
                value: p.get("value").cloned().unwrap_or(Value::Null),
            })
        }
        "action" => Some(Incoming::Action(payload?.as_str()?.to_string())),
        "record" => Some(Incoming::Record(payload?.as_str()? == "start")),
        _ => None,
    }
}

/// 组装 `state` 消息（页面首次加载与每次配置变更都推一次全量）。
pub fn state_json(cfg: &Config, version: &str, conflicts: &[(&str, u32)], log_dir: &str) -> String {
    let conflicts: Vec<Value> = conflicts
        .iter()
        .map(|(action, code)| json!({ "action": action, "code": code }))
        .collect();
    json!({
        "type": "state",
        "payload": {
            "config": cfg,
            "version": version,
            "conflicts": conflicts,
            "logDir": log_dir,
            // v1.1.5：当前生效的数据目录（storage.data_dir 为空时 = 默认根目录）
            "dataDir": crate::platform::data_dir().to_string_lossy(),
        }
    })
    .to_string()
}

/// 组装 `toast` 消息。
pub fn toast_json(level: &str, text: &str) -> String {
    json!({ "type": "toast", "payload": { "level": level, "text": text } }).to_string()
}

/// 应用一条 `set`。返回 `true` 表示配置真的变了。
///
/// 白名单之外的路径一律拒绝（返回 `false` 并记警告）。
pub fn apply_set(cfg: &mut Config, path: &str, value: &Value) -> bool {
    match path {
        // ---------------- 外观 ----------------
        "appearance.background_mode" => {
            let Some(s) = value.as_str() else { return false };
            cfg.appearance.background_mode = match s {
                "translucent" | "Translucent" => BackgroundMode::Translucent,
                _ => BackgroundMode::Blur,
            };
            true
        }
        "appearance.ui_scale" => {
            let Some(v) = value.as_f64() else { return false };
            // 只接受 1.0 / 1.25 / 1.5 / 2.0 四档（PRD P0-15）
            let v = (v as f32).clamp(1.0, 2.0);
            cfg.appearance.ui_scale = v;
            true
        }
        "appearance.monitor_index" => {
            let Some(v) = value.as_u64() else { return false };
            cfg.appearance.monitor_index = v as u32;
            true
        }
        // ---------------- 歌词 ----------------
        // v1.1.1：行数固定单行，`lyrics.mode` 已从白名单删除——设置页不再发送，
        // 旧页面残留的 set 请求走兜底分支返回 false（等于忽略，不报错）。
        "lyrics.enabled" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.lyrics.enabled = v;
            true
        }
        "lyrics.karaoke_sweep" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.lyrics.karaoke_sweep = v;
            true
        }
        // v1.1.2：长歌词滚动（跑马灯）开关
        "lyrics.scroll" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.lyrics.scroll = v;
            true
        }
        "lyrics.online_enabled" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.lyrics.online_enabled = v;
            true
        }
        "lyrics.local_dirs_add" => {
            let Some(s) = value.as_str() else { return false };
            let s = s.trim();
            if s.is_empty() || cfg.lyrics.local_dirs.iter().any(|d| d == s) {
                return false;
            }
            cfg.lyrics.local_dirs.push(s.to_string());
            true
        }
        "lyrics.local_dirs_remove" => {
            let Some(s) = value.as_str() else { return false };
            let before = cfg.lyrics.local_dirs.len();
            cfg.lyrics.local_dirs.retain(|d| d != s);
            cfg.lyrics.local_dirs.len() != before
        }
        // ---------------- 音量 ----------------
        "volume.wheel_enabled" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.volume.wheel_enabled = v;
            true
        }
        "volume.scope" => {
            let Some(s) = value.as_str() else { return false };
            cfg.volume.scope = match s {
                "current_player" | "Session" | "CurrentPlayer" => VolumeScope::CurrentPlayer,
                _ => VolumeScope::System,
            };
            true
        }
        "volume.step" => {
            let Some(v) = value.as_f64() else { return false };
            cfg.volume.step = (v as f32).clamp(0.01, 0.2);
            true
        }
        "volume.hud_ms" => {
            let Some(v) = value.as_u64() else { return false };
            cfg.volume.hud_ms = (v as u32).clamp(400, 5000);
            true
        }
        // ---------------- 通用 ----------------
        "general.language" => {
            let Some(s) = value.as_str() else { return false };
            cfg.general.language = match s {
                "zh" | "Zh" => LangSerde::Zh,
                "en" | "En" => LangSerde::En,
                _ => LangSerde::System,
            };
            true
        }
        "general.autostart" => {
            let Some(v) = value.as_bool() else { return false };
            // 真正写注册表；失败就不改配置（避免 UI 与实际不一致）
            if !crate::config::autostart::set_enabled(v) {
                crate::log_warn!("设置开机自启失败（注册表写入被拒绝）");
                return false;
            }
            cfg.general.autostart = v;
            true
        }
        "general.widget_hidden" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.general.widget_hidden = v;
            true
        }
        // ---------------- 存储（v1.1.5 问题 3）----------------
        // 浏览对话框 / 恢复默认按钮送来的路径。规范化（剥引号、去尾分隔符、
        // 相对锚定）；等于默认根目录时存空串（配置干净，语义同默认）。
        "storage.data_dir" => {
            let Some(s) = value.as_str() else { return false };
            let default_root = crate::platform::barline_dir();
            match crate::storage::resolve_custom_dir(s, &default_root) {
                Some(p) if p == default_root => cfg.storage.data_dir = String::new(),
                Some(p) => cfg.storage.data_dir = p.to_string_lossy().to_string(),
                None => cfg.storage.data_dir = String::new(),
            }
            true
        }
        // ---------------- 快捷键 ----------------
        "hotkeys.enabled" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.hotkeys.enabled = v;
            true
        }
        "hotkeys.reset" => {
            let enabled = cfg.hotkeys.enabled;
            let notified = cfg.hotkeys.conflict_notified;
            cfg.hotkeys = crate::config::HotkeyConfig::default();
            cfg.hotkeys.enabled = enabled;
            cfg.hotkeys.conflict_notified = notified;
            true
        }
        _ => {
            // hotkeys.<action> = { modifiers, vk }
            if let Some(action) = path.strip_prefix("hotkeys.") {
                return apply_binding(&mut cfg.hotkeys, action, value);
            }
            crate::log_warn!("设置页请求了白名单之外的配置路径：{path}");
            false
        }
    }
}

/// `hotkeys.<action>` 的绑定写入。
fn apply_binding(cfg: &mut crate::config::HotkeyConfig, action: &str, value: &Value) -> bool {
    let (Some(mods), Some(vk)) = (value.get("modifiers"), value.get("vk")) else {
        return false;
    };
    let (Some(mods), Some(vk)) = (mods.as_u64(), vk.as_u64()) else {
        return false;
    };
    // 至少要有 Ctrl / Alt / Shift / Win 之一，且必须有一个键
    if mods == 0 || vk == 0 {
        return false;
    }
    let b = HotkeyBinding {
        modifiers: (mods as u32) & 0x000F, // 只保留 ALT/CONTROL/SHIFT/WIN
        vk: vk as u32,
    };
    match action {
        "play_pause" => cfg.play_pause = b,
        "prev" => cfg.prev = b,
        "next" => cfg.next = b,
        "toggle_lyrics" => cfg.toggle_lyrics = b,
        "mute" => cfg.mute = b,
        "toggle_widget" => cfg.toggle_widget = b,
        _ => return false,
    }
    true
}

/// 动作名（日志 / 冲突上报用）。
pub fn action_name(a: crate::event::HotkeyAction) -> &'static str {
    use crate::event::HotkeyAction as A;
    match a {
        A::PlayPause => "play_pause",
        A::Prev => "prev",
        A::Next => "next",
        A::ToggleLyrics => "toggle_lyrics",
        A::Mute => "mute",
        A::ToggleWidget => "toggle_widget",
    }
}

/// 语言枚举 → 页面用的字符串。
pub fn lang_str(l: LangSerde) -> &'static str {
    match l {
        LangSerde::Zh => "Zh",
        LangSerde::En => "En",
        LangSerde::System => "System",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_set_message() {
        let m = parse(r#"{"type":"set","payload":{"path":"lyrics.enabled","value":true}}"#);
        assert_eq!(
            m,
            Some(Incoming::Set {
                path: "lyrics.enabled".into(),
                value: json!(true)
            })
        );
    }

    #[test]
    fn parse_action_and_record() {
        assert_eq!(
            parse(r#"{"type":"action","payload":"openLogDir"}"#),
            Some(Incoming::Action("openLogDir".into()))
        );
        assert_eq!(
            parse(r#"{"type":"record","payload":"start"}"#),
            Some(Incoming::Record(true))
        );
        assert_eq!(
            parse(r#"{"type":"record","payload":"stop"}"#),
            Some(Incoming::Record(false))
        );
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"type":"unknown"}"#), None);
        assert_eq!(parse(r#"{"type":"set"}"#), None);
    }

    #[test]
    fn apply_set_whitelisted_paths() {
        let mut c = Config::default();
        assert!(apply_set(&mut c, "lyrics.enabled", &json!(false)));
        assert!(!c.lyrics.enabled);
        // v1.1.2：长歌词滚动开关
        assert!(apply_set(&mut c, "lyrics.scroll", &json!(false)));
        assert!(!c.lyrics.scroll);
        assert!(apply_set(&mut c, "lyrics.scroll", &json!(true)));
        assert!(c.lyrics.scroll);
        assert!(apply_set(
            &mut c,
            "appearance.background_mode",
            &json!("Translucent")
        ));
        assert_eq!(c.appearance.background_mode, BackgroundMode::Translucent);
    }

    /// v1.1.1：行数固定单行，`lyrics.mode` 不再在白名单里——旧页面/旧脚本的
    /// set 请求必须被静默拒绝（返回 false，不 panic、不改配置）。
    #[test]
    fn apply_set_rejects_removed_lyrics_mode() {
        let mut c = Config::default();
        assert!(!apply_set(&mut c, "lyrics.mode", &json!("OneLine")));
        assert!(!apply_set(&mut c, "lyrics.mode", &json!("two_line")));
    }

    #[test]
    fn apply_set_rejects_unknown_path() {
        let mut c = Config::default();
        assert!(!apply_set(&mut c, "general.something", &json!(1)));
        assert!(!apply_set(&mut c, "version", &json!(9)));
    }

    #[test]
    fn apply_set_rejects_wrong_type() {
        let mut c = Config::default();
        assert!(!apply_set(&mut c, "lyrics.enabled", &json!("yes")));
        assert!(!apply_set(&mut c, "volume.step", &json!("big")));
    }

    #[test]
    fn apply_binding_sets_hotkey() {
        let mut c = Config::default();
        assert!(apply_set(
            &mut c,
            "hotkeys.prev",
            &json!({"modifiers": 7, "vk": 81})
        ));
        assert_eq!(c.hotkeys.prev.modifiers, 7);
        assert_eq!(c.hotkeys.prev.vk, 81);
    }

    #[test]
    fn apply_binding_rejects_empty() {
        let mut c = Config::default();
        assert!(!apply_set(
            &mut c,
            "hotkeys.prev",
            &json!({"modifiers": 0, "vk": 81})
        ));
        assert!(!apply_set(&mut c, "hotkeys.prev", &json!({"modifiers": 3})));
    }

    #[test]
    fn local_dirs_add_and_remove() {
        let mut c = Config::default();
        assert!(apply_set(&mut c, "lyrics.local_dirs_add", &json!("D:/lyrics")));
        assert_eq!(c.lyrics.local_dirs, vec!["D:/lyrics".to_string()]);
        // 重复添加不生效
        assert!(!apply_set(&mut c, "lyrics.local_dirs_add", &json!("D:/lyrics")));
        assert!(apply_set(
            &mut c,
            "lyrics.local_dirs_remove",
            &json!("D:/lyrics")
        ));
        assert!(c.lyrics.local_dirs.is_empty());
    }

    #[test]
    fn state_json_contains_config_and_version() {
        let c = Config::default();
        let s = state_json(&c, "0.0.5", &[("prev", 1409)], "C:/logs");
        let v: Value = serde_json::from_str(&s).expect("state 不是合法 JSON");
        assert_eq!(v["type"], "state");
        assert_eq!(v["payload"]["version"], "0.0.5");
        assert_eq!(v["payload"]["conflicts"][0]["action"], "prev");
        assert_eq!(v["payload"]["config"]["lyrics"]["enabled"], true);
    }
}
