//! 设置页 ↔ Rust 的消息协议（`ARCHITECTURE.md` §13.2）。
//!
//! 两个方向都是 JSON：
//! ```text
//! Rust → JS : { "type": "state", "payload": { config, version, conflicts, dataDir } }
//!             { "type": "toast", "payload": { "level": "info", "text": "..." } }
//! JS → Rust : { "type": "set",    "payload": { "path": "lyrics.enabled", "value": true } }
//!             { "type": "action", "payload": "openLogDir" }
//!             { "type": "record", "payload": "start" | "stop" }
//! ```
//!
//! **`path` 走白名单 match，不用反射**（§13.2 第 5 条）：反射会让 JS 侧能写任意字段，
//! 一旦 HTML 被注入就是配置任意写。白名单只有下面 `apply_set` 里列出的那几条。

use serde_json::{json, Value};

use crate::config::{Config, HotkeyBinding, HotkeyConfig, LangSerde, VolumeScope};
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
pub fn state_json(cfg: &Config, version: &str, conflicts: &[(&str, u32)]) -> String {
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

/// 组装 `cacheStats` 消息（v1.8.0 / R2）：设置页「歌词磁盘缓存」卡片的占用统计。
///
/// **刻意不进 `state`**：`state_json` 是纯函数且是热路径（每次 `set` 之后都推一次），
/// 往里塞 `read_dir` 等于用户拨滑条时扫目录（A5.3 明令禁止）；而且统计是「一次点查的
/// 结果」，没有「窗口重建后重推」的语义——重推它会让扫描次数飘。所以走独立消息类型。
///
/// `dir` 由 Rust 组装下发 → JS **不拼路径**（A4.3）。
pub fn cache_stats_json(st: &crate::lyric::cache::Stats) -> String {
    json!({
        "type": "cacheStats",
        "payload": {
            "dir": st.dir.to_string_lossy(),
            "files": st.files,
            "bytes": st.bytes,
            "truncated": st.truncated,
        }
    })
    .to_string()
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
                "transparent" | "Transparent" => BackgroundMode::Transparent,
                // "blur" = v1.1.11 删除模糊前的旧值，迁移为半透明
                _ => BackgroundMode::Translucent,
            };
            true
        }
        // v1.1.12：常规背景模式 + 两态不透明度（百分比 1..=100）
        "appearance.idle_mode" => {
            let Some(s) = value.as_str() else { return false };
            cfg.appearance.idle_mode = match s {
                "translucent" | "Translucent" => BackgroundMode::Translucent,
                _ => BackgroundMode::Transparent,
            };
            true
        }
        "appearance.hover_alpha" => {
            let Some(v) = value.as_f64() else { return false };
            cfg.appearance.hover_alpha = (v as f32).round().clamp(1.0, 100.0) as u8;
            true
        }
        "appearance.idle_alpha" => {
            let Some(v) = value.as_f64() else { return false };
            cfg.appearance.idle_alpha = (v as f32).round().clamp(1.0, 100.0) as u8;
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
        // v1.7.0：在线歌词落盘（只新建不覆盖）
        "lyrics.write_back" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.lyrics.write_back = v;
            true
        }
        // v1.8.0：歌词查询顺序（false = 磁盘缓存优先，保持 v1.7.0 现状；
        // true = 本地歌词文件优先）。只认 JSON 布尔——"yes" / 1 / null 一律拒绝。
        "lyrics.local_first" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.lyrics.local_first = v;
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
            // v1.1.17（用户裁决）：会话音量暂禁用——「当前播放器」不再可设，
            // 任何值一律归一化为系统主音量（与 Config::load 同口径）。
            cfg.volume.scope = VolumeScope::System;
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
        // 无实际变化返回 false（调用方据此跳过迁移弹窗）。
        "storage.data_dir" => {
            let Some(s) = value.as_str() else { return false };
            let default_root = crate::platform::barline_dir();
            let new_val = match crate::storage::resolve_custom_dir(s, &default_root) {
                Some(p) if p == default_root => String::new(),
                Some(p) => p.to_string_lossy().to_string(),
                None => String::new(),
            };
            if cfg.storage.data_dir == new_val {
                return false;
            }
            cfg.storage.data_dir = new_val;
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
            // v1.2.3（G4）：逐行「恢复默认」——`hotkeys.reset.<action>`
            // （整组重置是上面的 `hotkeys.reset`，两者前缀重叠，所以必须**先**判这条）
            if let Some(action) = path.strip_prefix("hotkeys.reset.") {
                let Some(action) = crate::event::HotkeyAction::parse(action) else {
                    crate::log_warn!("设置页请求恢复未知快捷键动作：{path}");
                    return false;
                };
                set_binding(&mut cfg.hotkeys, action, HotkeyConfig::default_binding(action));
                return true;
            }
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
    let Some(action) = crate::event::HotkeyAction::parse(action) else {
        return false;
    };
    let b = HotkeyBinding {
        modifiers: (mods as u32) & 0x000F, // 只保留 ALT/CONTROL/SHIFT/WIN
        vk: vk as u32,
    };
    set_binding(cfg, action, b);
    true
}

/// 把某个动作的绑定写进配置（v1.2.3：`apply_binding` 与逐行「恢复默认」共用）。
fn set_binding(
    cfg: &mut crate::config::HotkeyConfig,
    action: crate::event::HotkeyAction,
    b: HotkeyBinding,
) {
    use crate::event::HotkeyAction as A;
    match action {
        A::PlayPause => cfg.play_pause = b,
        A::Prev => cfg.prev = b,
        A::Next => cfg.next = b,
        A::ToggleLyrics => cfg.toggle_lyrics = b,
        A::Mute => cfg.mute = b,
        A::ToggleWidget => cfg.toggle_widget = b,
    }
}

/// 动作名（日志 / 冲突上报用）。
pub fn action_name(a: crate::event::HotkeyAction) -> &'static str {
    a.as_str()
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
        // v1.1.11：透明档 + 旧 blur 值迁移为半透明
        assert!(apply_set(
            &mut c,
            "appearance.background_mode",
            &json!("transparent")
        ));
        assert_eq!(c.appearance.background_mode, BackgroundMode::Transparent);
        assert!(apply_set(
            &mut c,
            "appearance.background_mode",
            &json!("blur")
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

    /// v1.7.0：`lyrics.write_back` 必须在白名单里（它是显式 match，
    /// 不加就被 `_` 分支静默拒绝——设置页的勾选会永远无效且不报错）。
    #[test]
    fn apply_set_lyrics_write_back() {
        let mut c = Config::default();
        assert!(apply_set(&mut c, "lyrics.write_back", &json!(false)));
        assert!(!c.lyrics.write_back);
        assert!(apply_set(&mut c, "lyrics.write_back", &json!(true)));
        assert!(c.lyrics.write_back);
        // 类型不对 → 拒绝，且不改配置
        assert!(!apply_set(&mut c, "lyrics.write_back", &json!("yes")));
        assert!(c.lyrics.write_back);
    }

    /// v1.8.0：`lyrics.local_first` 必须在白名单里（显式 match，不加就被 `_`
    /// 分支静默拒绝 → 开关永远无效且不报错），且**只接受 `as_bool()`**。
    #[test]
    fn apply_set_lyrics_local_first() {
        let mut c = Config::default();
        assert!(!c.lyrics.local_first, "默认 = 磁盘缓存优先");
        assert!(apply_set(&mut c, "lyrics.local_first", &json!(true)));
        assert!(c.lyrics.local_first);
        assert!(apply_set(&mut c, "lyrics.local_first", &json!(false)));
        assert!(!c.lyrics.local_first);
        // 类型不对 → 拒绝，且**配置不变**（A3.4）
        assert!(apply_set(&mut c, "lyrics.local_first", &json!(true)));
        for bad in [json!("yes"), json!("true"), json!(1), json!(0), json!(null)] {
            assert!(
                !apply_set(&mut c, "lyrics.local_first", &bad),
                "{bad} 不是 JSON 布尔，必须拒绝"
            );
            assert!(c.lyrics.local_first, "被拒绝的写入不许改动配置：{bad}");
        }
    }

    /// v1.8.0 / R2：`cacheStats` 消息的形状（PRD `:104` 已定，不增字段）。
    #[test]
    fn cache_stats_json_shape() {
        let st = crate::lyric::cache::Stats {
            dir: std::path::PathBuf::from("E:\\数据目录\\lyrics-cache"),
            files: 93,
            bytes: 421_888,
            truncated: false,
        };
        let s = cache_stats_json(&st);
        let v: Value = serde_json::from_str(&s).expect("cacheStats 不是合法 JSON");
        assert_eq!(v["type"], "cacheStats");
        assert_eq!(v["payload"]["dir"], "E:\\数据目录\\lyrics-cache");
        assert_eq!(v["payload"]["files"], 93);
        assert_eq!(v["payload"]["bytes"], 421_888);
        assert_eq!(v["payload"]["truncated"], false);
        assert!(v["payload"]["files"].is_u64(), "files 必须是数字，不是字符串");
        assert!(v["payload"]["bytes"].is_u64(), "bytes 必须是数字");

        // 截断态
        let more = crate::lyric::cache::Stats {
            truncated: true,
            ..st
        };
        let v: Value = serde_json::from_str(&cache_stats_json(&more)).expect("合法 JSON");
        assert_eq!(v["payload"]["truncated"], true);
    }

    /// v1.2.2：「界面缩放」整项删除，白名单必须同时关掉这个口子——
    /// 旧页面（用户机器上可能还有缓存的 WebView2 页面）再发 set 也不能写进配置。
    #[test]
    fn apply_set_rejects_removed_ui_scale() {
        let mut c = Config::default();
        assert!(
            !apply_set(&mut c, "appearance.ui_scale", &json!(1.25)),
            "已删除的 appearance.ui_scale 必须被白名单拒绝"
        );
        assert!(!apply_set(&mut c, "appearance.ui_scale", &json!(1.0)));
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

    /// v1.2.3（G4）：逐行「恢复默认」只动**目标那一组**，其它 5 组必须原样保留
    /// （否则用户精心改过的组合会被一个 ↺ 全清掉）。
    #[test]
    fn reset_one_hotkey_restores_that_binding() {
        use crate::event::HotkeyAction as A;
        use crate::config::defaults::mods;
        let mut c = Config::default();
        // 先把 6 组都改成非默认值，才能证明"只动一组"
        let custom = json!({"modifiers": mods::CONTROL | mods::ALT | mods::SHIFT, "vk": 0x51});
        for action in A::ALL {
            assert!(apply_set(
                &mut c,
                &format!("hotkeys.{}", action.as_str()),
                &custom
            ));
        }
        assert_eq!(c.hotkeys.prev.vk, 0x51);

        assert!(apply_set(&mut c, "hotkeys.reset.prev", &json!(true)));

        let want = HotkeyConfig::default_binding(A::Prev);
        assert_eq!(c.hotkeys.prev, want, "prev 必须回到默认 Ctrl+Alt+←");
        assert_eq!((c.hotkeys.prev.modifiers, c.hotkeys.prev.vk), (mods::CONTROL | mods::ALT, crate::config::defaults::vk::LEFT));
        // 其余 5 组不受影响
        for action in [A::PlayPause, A::Next, A::ToggleLyrics, A::Mute, A::ToggleWidget] {
            let now = match action {
                A::PlayPause => c.hotkeys.play_pause,
                A::Next => c.hotkeys.next,
                A::ToggleLyrics => c.hotkeys.toggle_lyrics,
                A::Mute => c.hotkeys.mute,
                _ => c.hotkeys.toggle_widget,
            };
            assert_eq!(now.vk, 0x51, "{} 被逐行恢复误伤", action.as_str());
            assert_eq!(now, custom_binding());
        }
    }

    /// 上面那条用的自定义绑定（与 JSON 里写的一致）。
    fn custom_binding() -> HotkeyBinding {
        use crate::config::defaults::mods;
        HotkeyBinding {
            modifiers: mods::CONTROL | mods::ALT | mods::SHIFT,
            vk: 0x51,
        }
    }

    /// 未知动作名必须被拒绝（不许 panic、不许静默成功）。
    #[test]
    fn reset_unknown_action_is_rejected() {
        let mut c = Config::default();
        assert!(!apply_set(&mut c, "hotkeys.reset.bogus", &json!(true)));
        assert!(!apply_set(&mut c, "hotkeys.reset.", &json!(true)));
        assert!(!apply_set(&mut c, "hotkeys.reset.PlayPause", &json!(true)));
        // 整组重置仍然可用（不能被新前缀抢走）
        c.hotkeys.prev.vk = 0x51;
        assert!(apply_set(&mut c, "hotkeys.reset", &json!(true)));
        assert_eq!(c.hotkeys.prev, HotkeyConfig::default_binding(crate::event::HotkeyAction::Prev));
    }

    /// `default_binding` 必须与 `HotkeyConfig::default()` 逐项一致（防两处漂移）。
    #[test]
    fn default_binding_matches_default_config() {
        let d = HotkeyConfig::default();
        for (action, binding) in crate::hotkey::bindings(&d) {
            assert_eq!(HotkeyConfig::default_binding(action), *binding, "{:?}", action);
        }
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

    /// v1.8.1（G6）：`lyrics.local_dirs_add` 白名单分支删除——「添加目录」实际走
    /// `addLyricsDirBrowse` 动作（`app.rs` 里直接 push），从不发这条 set。
    /// 现在只剩「删除」这一侧，并且**必须仍然拒绝**旧页面残留的 add 请求。
    #[test]
    fn local_dirs_remove_and_add_path_is_gone() {
        let mut c = Config::default();
        c.lyrics.local_dirs = vec!["D:/lyrics".to_string()];
        assert!(
            !apply_set(&mut c, "lyrics.local_dirs_add", &json!("E:/other")),
            "已删除的 add 路径必须走兜底并返回 false（忽略，不报错）"
        );
        assert_eq!(
            c.lyrics.local_dirs,
            vec!["D:/lyrics".to_string()],
            "add 请求不得改动目录列表"
        );
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
        let s = state_json(&c, "0.0.5", &[("prev", 1409)]);
        let v: Value = serde_json::from_str(&s).expect("state 不是合法 JSON");
        assert_eq!(v["type"], "state");
        assert_eq!(v["payload"]["version"], "0.0.5");
        assert_eq!(v["payload"]["conflicts"][0]["action"], "prev");
        assert_eq!(v["payload"]["config"]["lyrics"]["enabled"], true);
        assert!(v["payload"]["dataDir"].as_str().map(|d| !d.is_empty()).unwrap_or(false));
    }
}
