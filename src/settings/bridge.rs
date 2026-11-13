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
    state_json_with_section(cfg, version, conflicts, None)
}

/// 组装带可选 `openSection` 的 `state` 消息（v1.9.0 / G7 P1-03）。
///
/// 托盘「设置」且当前无会话时带 `"blacklist"`：页面**首次**收到 state 时一次性
/// 导航到黑名单分区（`navInitialized` 守卫），并走与 nav 点击同一条
/// "分区激活 → 首次枚举" 路径——保证直达后列表不空。
pub fn state_json_with_section(
    cfg: &Config,
    version: &str,
    conflicts: &[(&str, u32)],
    open_section: Option<&str>,
) -> String {
    let conflicts: Vec<Value> = conflicts
        .iter()
        .map(|(action, code)| json!({ "action": action, "code": code }))
        .collect();
    let mut payload = json!({
        "config": cfg,
        "version": version,
        "conflicts": conflicts,
        // v1.1.5：当前生效的数据目录（storage.data_dir 为空时 = 默认根目录）
        "dataDir": crate::platform::data_dir().to_string_lossy(),
    });
    if let Some(section) = open_section {
        payload["openSection"] = json!(section);
    }
    json!({ "type": "state", "payload": payload }).to_string()
}

/// 组装 `mediaApps` 消息（v1.9.0 / G7，ARCH-G7 §6.1）。
///
/// 字段名与 PRD 一致 + `resolved` / `source` 两个（U1=B）：
/// 前端必须能区分"真实名"（`resolved=true`）与"AUMID 末段兜底"（`false`）。
/// **已拉黑的条目照常下发**（Rust 不过滤），JS 按 `state.config.media.blacklist`
/// 渲染时过滤——点「加入」后配置回推即可让该行消失，不需要重新枚举。
/// `at` 是 epoch **毫秒**整数（JS 用 `new Date(at)` 格式化，Rust 不写时间格式化）。
pub fn media_apps_json(snap: &crate::media::MediaAppsSnapshot) -> String {
    let apps: Vec<Value> = snap
        .apps
        .iter()
        .map(|a| {
            json!({
                "process": a.process,
                "displayName": a.display_name,
                "playable": a.playable,
                "resolved": a.resolved,
                "source": a.source,
            })
        })
        .collect();
    json!({
        "type": "mediaApps",
        "payload": {
            "apps": apps,
            "at": snap.at_ms,
            "overBudget": snap.over_budget,
        }
    })
    .to_string()
}

/// 这个键"能不能被识别"——U1=B 要求桥接层回传的状态，**别只回 ok**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRecognition {
    /// 该进程名在本机的内存缓存（`MediaIndex.app_names` 的值里）或
    /// `player_map::TABLE` 里出现过 → 下次它开播时**保证**命中
    Verified,
    /// 两处都查不到：可能有效（若它的 AUMID 末段恰好等于这个值），也可能永不命中
    /// → 添加成功，但必须温和提示用户"从「检测到的应用」点加入更稳"
    Unverified,
}

/// 黑名单写入的结果分档（决定给用户哪条 toast）。
/// **带 payload，不是 `Copy`**（`Added` 携带 `KeyRecognition`），实现用 `Clone`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlacklistOutcome {
    /// 加成功；`recognition` 决定要不要多一条"未能验证"的温和提示
    Added(KeyRecognition),
    Duplicate,
    Invalid(crate::media::blacklist::NormError),
    Full,
    Removed,
    NotFound,
    Cleared,
    BadPath,
}

/// 黑名单写入的**唯一**入口（`apply_set` 不走这条路）。
///
/// 为什么不复用 `apply_set` 的 `bool`：黑名单要区分
/// 「重复 / 非法 / 超长 / 已满 / **未能验证**」五种结果给不同 toast，`bool` 表达不了。
///
/// `index` 用于 `KeyRecognition` 判据（纯内存，UI 线程可直接算）：
/// `Verified` ⟺ `index.knows_process(key)`——它同时查内存缓存值与
/// `player_map::TABLE` 的进程名主干。**不查运行时 AUMID**（那要跨进程），
/// 所以 `Unverified` 不等于"一定不生效"，只是"不能保证"——文案必须这么写。
pub fn apply_blacklist(
    cfg: &mut Config,
    path: &str,
    value: &Value,
    index: &crate::media::blacklist::MediaIndex,
) -> BlacklistOutcome {
    match path {
        "media.blacklist_add" => {
            let Some(s) = value.as_str() else {
                return BlacklistOutcome::Invalid(crate::media::blacklist::NormError::Empty);
            };
            let key = match crate::media::blacklist::normalize_entry(s) {
                Ok(k) => k,
                Err(e) => return BlacklistOutcome::Invalid(e),
            };
            if cfg.media.blacklist.contains(&key) {
                return BlacklistOutcome::Duplicate;
            }
            if cfg.media.blacklist.len()
                >= crate::config::defaults::limits::BLACKLIST_MAX_ENTRIES
            {
                return BlacklistOutcome::Full;
            }
            let recognition = if index.knows_process(&key) {
                KeyRecognition::Verified
            } else {
                KeyRecognition::Unverified
            };
            cfg.media.blacklist.push(key);
            BlacklistOutcome::Added(recognition)
        }
        "media.blacklist_remove" => {
            let Some(s) = value.as_str() else {
                return BlacklistOutcome::BadPath;
            };
            let key = match crate::media::blacklist::normalize_entry(s) {
                Ok(k) => k,
                Err(_) => return BlacklistOutcome::BadPath,
            };
            let before = cfg.media.blacklist.len();
            cfg.media.blacklist.retain(|e| *e != key);
            if cfg.media.blacklist.len() == before {
                BlacklistOutcome::NotFound
            } else {
                BlacklistOutcome::Removed
            }
        }
        "media.blacklist_clear" => {
            // 只清 blacklist，**不动 app_names**（D12：缓存是"AUMID → 真实名"的
            // 实测事实，与用户意图无关；清了只会让下次枚举白跑一遍反查）
            cfg.media.blacklist.clear();
            BlacklistOutcome::Cleared
        }
        _ => BlacklistOutcome::BadPath,
    }
}

/// 组装 `toast` 消息。
pub fn toast_json(level: &str, text: &str) -> String {
    json!({ "type": "toast", "payload": { "level": level, "text": text } }).to_string()
}

/// 组装 `detectFailed` 消息（v1.11.0 / D5）。
///
/// 只干一件事：让「检测到的应用」的刷新按钮从置灰恢复。**不**复用 `mediaApps`
/// ——那条消息会清空列表并渲染成「没检测到应用」，可这里其实一次都没枚举成功，
/// 拿空列表冒充结果等于对用户说谎。
pub fn detect_failed_json() -> String {
    json!({ "type": "detectFailed" }).to_string()
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
        // v1.10.0（G8-③）：双击小组件空白 / 信息区 / 歌词区 → 打开设置。
        //
        // 与 `autostart` 不同，这里**没有**外部世界要同步：开关只影响下一次
        // 双击的判定，而判定处（`App::reduce_input`）每次都读当前 `cfg`，
        // 不在 UI 状态机里存镜像，所以写完即生效、无需回滚。
        "general.double_click_open_settings" => {
            let Some(v) = value.as_bool() else { return false };
            cfg.general.double_click_open_settings = v;
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
        // ---------------- 黑名单（v1.9.0 / G7）----------------
        // 黑名单必须走 apply_blacklist（归一化 + 分档 toast + KeyRecognition）。
        // 从 apply_set 进来的一律拒绝（app.rs 的 reduce_settings_message 已前置分流，
        // 这里是纵深防御）：保证旧页面/注入脚本无法绕过归一化写入原始字符串。
        "media.blacklist_add" | "media.blacklist_remove" | "media.blacklist_clear" => {
            crate::log_warn!("黑名单写入必须走 apply_blacklist，拒绝路径：{path}");
            false
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

    /// **v1.10.0 / G8-③**：设置页「通用」页的双击开关必须走白名单，
    /// 且**类型不对就拒绝**（页面注入脚本发 `"true"` 这种字符串进来时
    /// 不许把配置写成"看起来开了其实没开"）。
    #[test]
    fn apply_set_double_click_open_settings() {
        let mut c = Config::default();
        assert!(c.general.double_click_open_settings, "默认开");

        assert!(apply_set(
            &mut c,
            "general.double_click_open_settings",
            &json!(false)
        ));
        assert!(!c.general.double_click_open_settings, "关掉必须真的写进配置");

        assert!(apply_set(
            &mut c,
            "general.double_click_open_settings",
            &json!(true)
        ));
        assert!(c.general.double_click_open_settings, "再打开也必须写进去");

        // 类型错误：拒绝且不改配置
        for bad in [json!("yes"), json!(0), json!(1), json!(null)] {
            assert!(
                !apply_set(&mut c, "general.double_click_open_settings", &bad),
                "非布尔值必须被拒绝：{bad}"
            );
            assert!(
                c.general.double_click_open_settings,
                "被拒绝的写入不得改动配置（{bad}）"
            );
        }
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

    // ------------------------------------------------ v1.9.0（G7）黑名单桥接

    /// `state_json_with_section`：带 openSection 时 payload 里出现该字段；None 时不出现。
    #[test]
    fn state_json_open_section_is_optional() {
        let c = Config::default();
        let with = state_json_with_section(&c, "1.9.0", &[], Some("blacklist"));
        let v: Value = serde_json::from_str(&with).expect("合法 JSON");
        assert_eq!(v["payload"]["openSection"], "blacklist");
        let without = state_json(&c, "1.9.0", &[]);
        let v: Value = serde_json::from_str(&without).expect("合法 JSON");
        assert!(v["payload"].get("openSection").is_none(), "None 时不得出现 openSection");
    }

    /// `media_apps_json` 的形状（ARCH-G7 §6.1，PRD `:399` 字段不增不减）。
    #[test]
    fn media_apps_json_shape() {
        let snap = crate::media::MediaAppsSnapshot {
            apps: vec![
                crate::media::smtc::MediaAppEntry {
                    process: "cloudmusic.exe".into(),
                    display_name: "网易云音乐".into(),
                    playable: true,
                    resolved: true,
                    source: "cache",
                },
                crate::media::smtc::MediaAppEntry {
                    process: String::new(),
                    display_name: "未知播放器".into(),
                    playable: false,
                    resolved: false,
                    source: "aumid",
                },
            ],
            at_ms: 1_762_222_200_000,
            learned: vec![],
            over_budget: true,
        };
        let s = media_apps_json(&snap);
        let v: Value = serde_json::from_str(&s).expect("mediaApps 不是合法 JSON");
        assert_eq!(v["type"], "mediaApps");
        assert_eq!(v["payload"]["apps"].as_array().unwrap().len(), 2);
        let first = &v["payload"]["apps"][0];
        assert_eq!(first["process"], "cloudmusic.exe");
        assert_eq!(first["displayName"], "网易云音乐");
        assert_eq!(first["playable"], true);
        assert_eq!(first["resolved"], true);
        assert_eq!(first["source"], "cache");
        assert_eq!(v["payload"]["at"], 1_762_222_200_000u64, "at 是 epoch 毫秒数字");
        assert!(v["payload"]["at"].is_u64());
        assert_eq!(v["payload"]["overBudget"], true);
        // playable=false 的行同样带全部字段（前端灰态渲染用）
        let second = &v["payload"]["apps"][1];
        assert_eq!(second["playable"], false);
        assert_eq!(second["resolved"], false);
    }

    /// add：表内进程名（CloudMusic）→ `Added(Verified)`；大小写/路径/无后缀都归一化。
    #[test]
    fn apply_blacklist_add_verified_for_table_process() {
        let cfg = &mut Config::default();
        let idx = crate::media::blacklist::MediaIndex::new(&crate::config::MediaConfig::default());
        let out = apply_blacklist(cfg, "media.blacklist_add", &json!("  CloudMusic  "), &idx);
        assert_eq!(out, BlacklistOutcome::Added(KeyRecognition::Verified));
        assert_eq!(cfg.media.blacklist, vec!["cloudmusic.exe".to_string()]);
    }

    /// add：从未出现过的名字 → `Added(Unverified)`（U1=B 失败不静默的桥接层落点）。
    /// 重复添加（任何大小写/形态）→ `Duplicate`，且不产生第二条。
    #[test]
    fn apply_blacklist_add_unverified_for_unknown_process() {
        let cfg = &mut Config::default();
        let idx = crate::media::blacklist::MediaIndex::new(&crate::config::MediaConfig::default());
        let out = apply_blacklist(cfg, "media.blacklist_add", &json!("zzz-test.exe"), &idx);
        assert_eq!(out, BlacklistOutcome::Added(KeyRecognition::Unverified));
        assert_eq!(cfg.media.blacklist, vec!["zzz-test.exe".to_string()]);
        // 同一条的其它形态 → Duplicate（重复判定先于识别判定）
        let out = apply_blacklist(cfg, "media.blacklist_add", &json!("ZZZ-TEST"), &idx);
        assert_eq!(out, BlacklistOutcome::Duplicate);
        assert_eq!(cfg.media.blacklist.len(), 1);
        // 添加成功后缓存里学到了它 → 同名键变 Verified（同源判据）
        let mut learned = crate::config::MediaConfig::default();
        learned.app_names.insert("a!b".into(), "zzz-test.exe".into());
        let idx2 = crate::media::blacklist::MediaIndex::new(&learned);
        let mut cfg2 = Config::default();
        let out = apply_blacklist(&mut cfg2, "media.blacklist_add", &json!("zzz-test"), &idx2);
        assert_eq!(out, BlacklistOutcome::Added(KeyRecognition::Verified));
    }

    /// add：非法字符 / 空串 / 超长 / 满 200 → 各自的分档。
    #[test]
    fn apply_blacklist_add_rejects_invalid_empty_too_long_full() {
        use crate::media::blacklist::NormError;
        let cfg = &mut Config::default();
        let idx = crate::media::blacklist::MediaIndex::new(&crate::config::MediaConfig::default());
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_add", &json!("a*b.exe"), &idx),
            BlacklistOutcome::Invalid(NormError::InvalidChar)
        );
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_add", &json!(""), &idx),
            BlacklistOutcome::Invalid(NormError::Empty)
        );
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_add", &json!(".exe"), &idx),
            BlacklistOutcome::Invalid(NormError::Empty)
        );
        let long = format!("{}.exe", "a".repeat(300));
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_add", &json!(long), &idx),
            BlacklistOutcome::Invalid(NormError::TooLong)
        );
        // 满 200
        for i in 0..crate::config::defaults::limits::BLACKLIST_MAX_ENTRIES {
            cfg.media.blacklist.push(format!("app{i}.exe"));
        }
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_add", &json!("overflow.exe"), &idx),
            BlacklistOutcome::Full
        );
        assert_eq!(cfg.media.blacklist.len(), crate::config::defaults::limits::BLACKLIST_MAX_ENTRIES);
        // 非 string value → Invalid(Empty)（toast「请输入进程名」）
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_add", &json!(42), &idx),
            BlacklistOutcome::Invalid(NormError::Empty)
        );
    }

    /// remove：归一化后精确删除（大小写不敏感）；不存在 → NotFound。
    #[test]
    fn apply_blacklist_remove_normalizes_and_reports_not_found() {
        let cfg = &mut Config::default();
        cfg.media.blacklist = vec!["cloudmusic.exe".into()];
        let idx = crate::media::blacklist::MediaIndex::new(&crate::config::MediaConfig::default());
        // 无后缀 / 大小写 / 路径形态都能删掉同一条
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_remove", &json!("C:\\x\\CloudMusic"), &idx),
            BlacklistOutcome::Removed
        );
        assert!(cfg.media.blacklist.is_empty());
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_remove", &json!("cloudmusic.exe"), &idx),
            BlacklistOutcome::NotFound
        );
    }

    /// clear：只清 blacklist，**绝不清 app_names**（D12：缓存是实测事实）。
    #[test]
    fn apply_blacklist_clear_keeps_learned_app_names() {
        let cfg = &mut Config::default();
        cfg.media.blacklist = vec!["a.exe".into(), "b.exe".into()];
        cfg.media
            .app_names
            .insert("x!y".into(), "real.exe".into());
        let idx = crate::media::blacklist::MediaIndex::new(&cfg.media);
        assert_eq!(
            apply_blacklist(cfg, "media.blacklist_clear", &json!(true), &idx),
            BlacklistOutcome::Cleared
        );
        assert!(cfg.media.blacklist.is_empty());
        assert_eq!(cfg.media.app_names.len(), 1, "app_names 不许被 clear 连带清掉");
    }

    /// apply_set 显式拒绝黑名单三条 path（纵深防御：防旧页面/注入脚本绕过归一化）。
    #[test]
    fn apply_set_rejects_media_blacklist_paths() {
        let mut c = Config::default();
        for path in [
            "media.blacklist_add",
            "media.blacklist_remove",
            "media.blacklist_clear",
        ] {
            assert!(!apply_set(&mut c, path, &json!("cloudmusic.exe")), "{path} 必须被拒绝");
        }
        assert!(c.media.blacklist.is_empty(), "被拒绝的写入不许改动配置");
    }

    /// **v1.11.0 / D6**：`BlacklistOutcome::BadPath` 的三条产出路径各钉一条。
    ///
    /// 这个结果在 UI 侧是「静默记日志、不打扰用户」（`app.rs` 的 `other =>` 臂），
    /// 一旦被误改成 `Removed` / `NotFound`，用户会看到一条假的「已移除」提示，
    /// 而配置其实没动。三条来源：值不是字符串、entry 归一化失败、path 不认识。
    #[test]
    fn apply_blacklist_bad_path_covers_three_sources_and_keeps_config() {
        let mut cfg = Config::default();
        cfg.media.blacklist.push(String::from("cloudmusic.exe"));
        let before = cfg.media.blacklist.clone();
        let idx = crate::media::blacklist::MediaIndex::new(&cfg.media);

        for (path, value, why) in [
            ("media.blacklist_remove", json!(42), "值不是字符串"),
            ("media.blacklist_remove", json!(""), "entry 归一化失败"),
            (
                "media.blacklist_unknown",
                json!("cloudmusic.exe"),
                "path 不认识",
            ),
        ] {
            assert_eq!(
                apply_blacklist(&mut cfg, path, &value, &idx),
                BlacklistOutcome::BadPath,
                "{why}（path={path}）必须返回 BadPath"
            );
            assert_eq!(
                cfg.media.blacklist, before,
                "{why}：BadPath 不许改动配置（否则 UI 静默、配置却变了）"
            );
        }
    }
}
