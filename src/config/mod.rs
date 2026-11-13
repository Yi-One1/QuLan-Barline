//! 配置持久化（`ARCHITECTURE.md` §7.5 / §14.1）。
//!
//! - 路径：`%LOCALAPPDATA%\Barline\config.json`（`SHGetKnownFolderPath`，不用环境变量；
//!   **v1.1.5 起无论数据目录迁移到哪里，config.json 都固定在这里**）
//! - 格式：单行压缩 JSON（约 2 KB）
//! - 读：不存在 → 默认值并写盘；解析失败 → 备份为 `config.bad.<ts>.json`，用默认值并写盘
//! - 写：先写 `config.json.tmp`，再 `rename` 覆盖（Windows 上 Rust 的 `fs::rename`
//!   内部就是 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，原子替换，断电不损坏）

pub mod autostart;
pub mod defaults;

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::render::theme::BackgroundMode;

/// 音量作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeScope {
    #[default]
    System,
    CurrentPlayer,
}

/// 界面语言（含 `System`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LangSerde {
    #[default]
    System,
    Zh,
    En,
}

/// 一组热键绑定。`modifiers` 使用 `MOD_*` 位；`vk` 为虚拟键码；
/// 两者都为 0 表示未绑定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub modifiers: u32,
    pub vk: u32,
}

impl HotkeyBinding {
    pub fn is_bound(&self) -> bool {
        self.vk != 0
    }
}

/// 外观。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    /// 悬停背景模式（v1.1.12：默认档；`background_mode` 字段名沿用旧配置）
    pub background_mode: BackgroundMode,
    /// 常规（不悬停）背景模式：默认透明 = 沿用 v1.1.11 前的行为
    pub idle_mode: BackgroundMode,
    /// 悬停半透明的不透明度百分比（1..=100，默认 70 = 原 0xB3）
    pub hover_alpha: u8,
    /// 常规半透明的不透明度百分比（1..=100，默认 50）
    pub idle_alpha: u8,
    /// 显示显示器：0 = 主显示器，否则为显示器序号
    pub monitor_index: u32,
}

/// 歌词。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsConfig {
    pub enabled: bool,
    pub karaoke_sweep: bool,
    /// 长歌词滚动（跑马灯，v1.1.2 新增；v1.1.4 改行时间窗口驱动 + 提前滚完）：
    /// 歌词超宽时按行时间窗口线性滚动，比该行唱完提前滚到尾；
    /// 关闭时行首截断（超宽部分不显示）。旧配置缺省即 true（#[serde(default)] 兜底）。
    pub scroll: bool,
    pub online_enabled: bool,
    /// 本地歌词搜索目录（绝对路径）。空数组表示只用默认目录。
    pub local_dirs: Vec<String>,
    /// 在线歌词落盘（v1.7.0）：命中在线歌词后保存到第一个本地歌词目录的 Barline 子目录。
    /// 只新建、绝不覆盖已有文件。默认开。
    pub write_back: bool,
    /// 歌词查询顺序（v1.8.0）：
    /// - `false`（默认）= 磁盘缓存优先 → `内存 → 磁盘缓存 → 本地 → 负缓存 → 在线`
    ///   （= v1.7.0 现状：省时优先，重启也命中）；
    /// - `true` = 本地歌词文件优先 → `本地 → 内存 → 磁盘缓存 → 负缓存 → 在线`
    ///   （手改 `.lrc` **立刻**生效，含正在播放的那一首；代价是每次切歌多一次目录遍历）。
    ///
    /// 缺省走容器级 `#[serde(default)]` 兜底 → 旧 `config.json` 删掉该行落到 `false`。
    pub local_first: bool,
}

/// 音量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VolumeConfig {
    pub wheel_enabled: bool,
    pub scope: VolumeScope,
    /// 每格步进：0.01 / 0.02 / 0.05 / 0.1（v1.1.10 问题 5）。
    ///
    /// **序列化按 0.01 粒度取整**——serde_json 序列化 f32 会先拓宽成 f64
    /// （`0.05f32` → `0.05000000074505806`），设置页 JS 用 `String(value)`
    /// 匹配下拉选项 `"0.05"` 永不相等 → 「每次步进」一直显示空白。
    /// 取整后设置页与 config.json 里都存干净值；内存里的 f32 不动。
    #[serde(serialize_with = "serialize_step_percent")]
    pub step: f32,
    /// HUD 停留时长（毫秒）：600 / 1200 / 2000
    pub hud_ms: u32,
}

/// `volume.step` 序列化钩子：按 0.01 粒度取整（v1.1.10 问题 5）。
///
/// `(0.05f32 as f64 * 100.0).round() / 100.0` = `0.05f64`，ryu 最短表示
/// 正好是 `0.05`；0.01 / 0.02 / 0.1 同理。步进值只来自设置页下拉
/// （0.01/0.02/0.05/0.1）或桥接层 clamp(0.01, 0.2)，取整到两位百分数
/// 不会丢失合法输入。
fn serialize_step_percent<S: serde::Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(((*v as f64) * 100.0).round() / 100.0)
}

/// 热键（6 组）。全部字段都是 `Copy`，整体也 `Copy`——注册时要同时借用
/// `config.hotkeys` 与 `&mut app.hotkeys`，值语义最省事。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub enabled: bool,
    /// 默认值版本（0 = 旧 `Ctrl+Alt+*`）。见 `defaults::HOTKEYS_VERSION`
    #[serde(default)]
    pub hotkeys_version: u32,
    /// 「快捷键被占用」气泡是否已经提示过（只提示一次）
    #[serde(default)]
    pub conflict_notified: bool,
    pub play_pause: HotkeyBinding,
    pub prev: HotkeyBinding,
    pub next: HotkeyBinding,
    pub toggle_lyrics: HotkeyBinding,
    pub mute: HotkeyBinding,
    pub toggle_widget: HotkeyBinding,
}

/// 通用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub autostart: bool,
    pub language: LangSerde,
    /// 用户隐藏小组件（PRD 机器 B 门控 1，跨会话记忆）
    pub widget_hidden: bool,
    /// **双击空白/信息区/歌词区 → 打开设置**（v1.10.0 / G8-③，设置页「通用」页开关）。
    ///
    /// 默认 `true`（用户裁决 R1）。**不覆盖**任何控件：三个播控键、右上角 ×、
    /// 封面、进度条的双击语义完全不变（见 `docs/ARCHITECTURE.md` §G8-③）——
    /// 小组件空白区的判定就是 `hit_test` 返回 [`HitTarget::None`]
    /// (crate::ui::hit::HitTarget::None)，即「既不是按钮也不是封面/进度条」。
    ///
    /// 关掉后空白双击**零动作**，与 v1.9.3 及以前逐字节一致（连日志都不写）。
    pub double_click_open_settings: bool,
}

/// 存储配置（v1.1.5 问题 3；v1.1.6 净搬 + A→B 切换）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StorageConfig {
    /// 自定义数据目录（绝对路径推荐；空 = 默认 `%LOCALAPPDATA%\Barline`）。
    /// `config.json` 本身不受影响，永远留在默认根目录；缓存/日志在
    /// 下次启动时自动净搬过去（`storage::init_from_config`）。
    pub data_dir: String,
    /// 上次启动**实际生效**的数据目录（启动后由程序写回并落盘）。
    /// 迁移源以它为准——支持「自定义 A 切换到 B」时把 A 的数据搬去 B。
    /// 空 = 默认根目录。用户不必手改。
    pub active_data_dir: String,
    /// 用户在设置页选择的数据目录迁移选项：下次启动是否把旧目录文件
    /// 迁到新目录（弹窗询问后写入，启动执行后复位）。
    pub pending_migrate: bool,
    /// 用户在设置页选择的数据目录删除选项：下次启动迁移/腾空后是否删除
    /// 原目录里的文件（弹窗询问后写入，启动执行后复位；config.json 永远豁免）。
    pub pending_delete_old: bool,
}

/// 媒体源过滤（v1.9.0 / G7，U1=B，`docs/ARCH-G7-黑名单.md` §3.1）。
///
/// 两个字段都只存**归一化后**的值。
/// * `blacklist` 是**用户资产**（顺序有意义：首次添加顺序，不排序）；
/// * `app_names` 是**实测缓存**（顺序无意义，用 `BTreeMap` 保证 JSON 输出稳定，
///   否则 `HashMap` 的随机迭代顺序会让 `config.json` 每次保存都字节级变化）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaConfig {
    /// 黑名单。上限 `limits::BLACKLIST_MAX_ENTRIES`，单条 ≤ `limits::BLACKLIST_MAX_LEN`。
    /// **只清这个表不清 `app_names`**（缓存是实测事实，与用户意图无关）。
    pub blacklist: Vec<String>,
    /// 白名单（v1.12.0）。语义：**空 = 不启用**；非空 = **只接管名单内的应用**，
    /// 名单外的一律按被拉黑处理（不渲染、不查歌词、不响应播控）。
    ///
    /// 与 `blacklist` **互斥**：同一条目不允许同时存在于两张表。写入路径
    /// （`apply_blacklist`）直接拒绝；手改 json 绕过时 `Config::load` 会把
    /// 冲突项从白名单里剔除（**黑名单优先**，见 `normalize_lists`）。
    pub whitelist: Vec<String>,
    /// AUMID（**小写**）→ 真实进程名（小写、含 `.exe`）。**落盘**，重启后仍有效。
    ///
    /// 来源：设置页枚举时的 WASAPI 反查（ARCH-G7 §5）。用途：`identity()` 的**第①级**。
    /// 上限 `limits::APP_NAME_CACHE_MAX`，满时淘汰字典序最小的键；
    /// `Config::load` 会做双表归一化（手改 json 写坏也不炸）。
    pub app_names: std::collections::BTreeMap<String, String>,
}

/// 全量配置。`#[serde(default)]` 保证缺字段 / 新增字段时都能正常反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 配置结构版本，用于未来迁移
    pub version: u32,
    pub appearance: AppearanceConfig,
    pub lyrics: LyricsConfig,
    pub volume: VolumeConfig,
    pub hotkeys: HotkeyConfig,
    pub general: GeneralConfig,
    /// v1.1.5：存储位置
    pub storage: StorageConfig,
    /// v1.9.0（G7）：媒体源过滤（黑名单 + 真实进程名缓存）
    pub media: MediaConfig,
}

/// `%LOCALAPPDATA%\Barline\config.json`
pub fn config_path() -> PathBuf {
    crate::platform::barline_dir().join("config.json")
}

impl Config {
    /// 读配置。任何异常都**不阻塞启动**：缺失 / 损坏一律退化为默认值。
    pub fn load() -> Self {
        let path = config_path();
        let raw = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    crate::log_info!("配置文件不存在，写入默认配置：{}", path.display());
                } else {
                    crate::log_warn!("读取配置失败（{e}），使用默认配置");
                }
                let cfg = Config::default();
                if let Err(e) = cfg.save() {
                    crate::log_err!("写入默认配置失败：{e}");
                }
                return cfg;
            }
        };

        match serde_json::from_slice::<Config>(&raw) {
            Ok(mut cfg) => {
                crate::log_info!("配置已加载：version={}", cfg.version);
                // v1.1.17（用户裁决）：会话音量（「当前播放器」范围）实测在
                // 网易云/汽水/Edge 上均无法生效（连匹配环节都失败，根因见
                // progress.md 待办），暂时禁用——旧配置里的 current_player
                // 一律归一化为 System，滚轮永不触碰会话音量路径。
                if cfg.volume.scope == VolumeScope::CurrentPlayer {
                    crate::log_warn!("音量范围「当前播放器」已暂时禁用（功能缺陷待修），回退为系统主音量");
                    cfg.volume.scope = VolumeScope::System;
                }
                if cfg.hotkeys.migrate() {
                    // 迁移结果要立刻落盘，否则下次启动又会判定为「旧配置」
                    if let Err(e) = cfg.save() {
                        crate::log_warn!("迁移后的配置保存失败：{e}");
                    }
                }
                // v1.9.0（G7）：黑名单 + 真实名缓存归一化。手改 json 也安全——
                // 坏值静默丢弃，不 panic、不弹窗。**顺序必须"先 normalize 后 save"**
                // （反了会把未归一化的脏表写回磁盘，ARCH-G7 §3.3 / R12）。
                // 归一化必须幂等（R12）：正常第二次启动不再判定为"有变化"。
                // v1.12.0：白名单一起归一化，并执行互斥（冲突项从白名单剔除）。
                let (bl, wl) = crate::media::blacklist::normalize_lists(
                    &cfg.media.blacklist,
                    &cfg.media.whitelist,
                );
                let an = crate::media::blacklist::normalize_app_names(&cfg.media.app_names);
                if bl != cfg.media.blacklist
                    || wl != cfg.media.whitelist
                    || an != cfg.media.app_names
                {
                    crate::log_info!(
                        "media 已归一化：blacklist {} → {} 条，whitelist {} → {} 条，app_names {} → {} 条",
                        cfg.media.blacklist.len(),
                        bl.len(),
                        cfg.media.whitelist.len(),
                        wl.len(),
                        cfg.media.app_names.len(),
                        an.len()
                    );
                    cfg.media.blacklist = bl;
                    cfg.media.whitelist = wl;
                    cfg.media.app_names = an;
                    // 归一化结果立刻落盘，避免每次启动重复清洗
                    if let Err(e) = cfg.save() {
                        crate::log_warn!("归一化后的 media 配置保存失败：{e}");
                    }
                }
                cfg
            }
            Err(e) => {
                crate::log_err!("配置文件损坏（{e}），已备份并回退到默认配置");
                backup_bad_config(&path);
                let cfg = Config::default();
                if let Err(e) = cfg.save() {
                    crate::log_err!("写入默认配置失败：{e}");
                }
                cfg
            }
        }
    }

    /// 原子写盘：先写 `.tmp`，再 `rename` 覆盖。
    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &bytes)?;
        // Windows 上 rename 走 MoveFileExW(MOVEFILE_REPLACE_EXISTING)：目标已存在也原子替换。
        fs::rename(&tmp, &path)?;
        crate::log_debug!("配置已保存（{} 字节）", bytes.len());
        Ok(())
    }
}

fn backup_bad_config(path: &PathBuf) {
    let stamp = crate::log::timestamp_compact();
    let backup = path.with_file_name(format!("config.bad.{stamp}.json"));
    match fs::rename(path, &backup) {
        Ok(()) => crate::log_err!("损坏的配置已备份为 {}", backup.display()),
        Err(e) => crate::log_err!("备份损坏配置失败：{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let cfg = Config::default();
        let json = serde_json::to_vec(&cfg).expect("序列化不应失败");
        let back: Config = serde_json::from_slice(&json).expect("反序列化不应失败");

        assert_eq!(back.version, cfg.version);
        // v1.2.2：`ui_scale` 已删除（死配置），改用它旁边的两个背景模式字段
        assert_eq!(back.appearance.background_mode, cfg.appearance.background_mode);
        assert_eq!(back.appearance.idle_mode, cfg.appearance.idle_mode);
        assert_eq!(back.lyrics.enabled, cfg.lyrics.enabled);
        assert_eq!(back.volume.step, cfg.volume.step);
        assert_eq!(back.volume.hud_ms, cfg.volume.hud_ms);
        assert_eq!(back.hotkeys.play_pause, cfg.hotkeys.play_pause);
        assert_eq!(back.hotkeys.toggle_widget, cfg.hotkeys.toggle_widget);
        assert_eq!(back.general.language, cfg.general.language);
    }

    /// v1.1.2 向后兼容：旧配置没有 `lyrics.scroll` 字段 → 反序列化不报错且默认 true。
    #[test]
    fn legacy_config_without_scroll_defaults_true() {
        let mut cfg = Config::default();
        cfg.lyrics.scroll = false; // 故意写非默认值再删掉字段，验证走 serde default
        let mut json = serde_json::to_value(&cfg).expect("序列化不应失败");
        json["lyrics"]
            .as_object_mut()
            .expect("lyrics 必须是对象")
            .remove("scroll");
        let back: Config = serde_json::from_value(json).expect("缺 scroll 字段不应报错");
        assert!(back.lyrics.scroll, "旧配置缺省 scroll 必须为 true（默认开）");
    }

    /// v1.7.0 向后兼容：旧配置没有 `lyrics.write_back` 字段 → 反序列化不报错且默认 true。
    ///
    /// `LyricsConfig` 容器的 `#[serde(default)]` 负责兜底：缺字段时整个字段取
    /// `LyricsConfig::default()` 的值，所以旧用户升级后落盘是**开着**的，
    /// 不需要额外的配置迁移代码。
    #[test]
    fn legacy_config_without_write_back_defaults_true() {
        let mut cfg = Config::default();
        cfg.lyrics.write_back = false; // 故意写非默认值再删掉字段，验证走 serde default
        let mut json = serde_json::to_value(&cfg).expect("序列化不应失败");
        json["lyrics"]
            .as_object_mut()
            .expect("lyrics 必须是对象")
            .remove("write_back");
        let back: Config = serde_json::from_value(json).expect("缺 write_back 字段不应报错");
        assert!(
            back.lyrics.write_back,
            "旧配置缺省 write_back 必须为 true（默认开）"
        );
    }

    /// **v1.8.0 / A1.1 向后兼容**：旧配置里没有 `lyrics.local_first` 字段 →
    /// 反序列化不报错且落到 `false`（= 磁盘缓存优先 = 与 v1.7.0 行为一致）。
    ///
    /// 兜底机制与 `write_back` 同款：`LyricsConfig` 容器上的 `#[serde(default)]`
    /// 让 serde 用 `LyricsConfig::default()` 逐字段补齐缺失字段。
    #[test]
    fn legacy_config_without_local_first_defaults_false() {
        let mut cfg = Config::default();
        cfg.lyrics.local_first = true; // 故意写非默认值再删掉字段，验证走 serde default
        let mut json = serde_json::to_value(&cfg).expect("序列化不应失败");
        json["lyrics"]
            .as_object_mut()
            .expect("lyrics 必须是对象")
            .remove("local_first");
        let back: Config = serde_json::from_value(json).expect("缺 local_first 字段不应报错");
        assert!(
            !back.lyrics.local_first,
            "旧配置缺省 local_first 必须为 false（默认 = 磁盘缓存优先）"
        );

        // 更极端：整段 lyrics 都没有 → 仍然落到默认值，不 panic
        let only_version: Config = serde_json::from_slice(br#"{"version":1}"#)
            .expect("只有 version 的旧配置必须仍能加载");
        assert!(!only_version.lyrics.local_first);
        assert!(only_version.lyrics.write_back, "同容器的其它默认值不受影响");
    }

    /// **v1.10.0 / G8-③ 向后兼容**：旧配置里没有 `general.double_click_open_settings`
    /// → 反序列化不报错且落到 `true`（= 升级后开箱即用，用户不必去设置页打开）。
    ///
    /// 兜底机制与 `lyrics.write_back` 同款：`GeneralConfig` 上的 `#[serde(default)]`
    /// 让 serde 用 `GeneralConfig::default()` 逐字段补齐缺失字段——所以默认值
    /// **必须**写在 `impl Default for GeneralConfig` 里（`defaults.rs`），
    /// 不能依赖 `derive(Default)` 的 `bool = false`。
    #[test]
    fn legacy_config_without_double_click_open_settings_defaults_true() {
        let mut cfg = Config::default();
        cfg.general.double_click_open_settings = false; // 写非默认值再删字段，验证走 serde default
        let mut json = serde_json::to_value(&cfg).expect("序列化不应失败");
        json["general"]
            .as_object_mut()
            .expect("general 必须是对象")
            .remove("double_click_open_settings");
        let back: Config = serde_json::from_value(json).expect("缺字段不应报错");
        assert!(
            back.general.double_click_open_settings,
            "旧配置缺省 double_click_open_settings 必须为 true（默认开）"
        );

        // 更极端：整段 general 都没有 → 仍然落到默认值 true，不 panic
        let only_version: Config = serde_json::from_slice(br#"{"version":1}"#)
            .expect("只有 version 的旧配置必须仍能加载");
        assert!(
            only_version.general.double_click_open_settings,
            "整段 general 缺失时也必须默认开"
        );
        // 同容器的既有字段默认值不受影响（防止有人顺手改坏 GeneralConfig::default）
        assert!(!only_version.general.autostart);
        assert!(!only_version.general.widget_hidden);
    }

    /// **v1.10.0 / G8-③ 序列化形状**：新字段必须真的进 `config.json`
    /// （否则设置页开关回推后一重启就丢），且键名与设置页白名单**逐字符一致**。
    #[test]
    fn double_click_open_settings_is_persisted_under_the_documented_key() {
        let cfg = Config::default();
        let json = serde_json::to_value(&cfg).expect("序列化不应失败");
        assert_eq!(
            json["general"]["double_click_open_settings"], true,
            "落盘键名必须是 general.double_click_open_settings，且默认 true"
        );
    }

    /// **v1.2.2 旧配置兼容（删除 `ui_scale` 后最重要的回归保障）**：
    /// 用户机器上现存的 `config.json` 里还留着 `appearance.ui_scale`，
    /// 字段删掉之后必须**照常加载**（serde 默认忽略未知字段），
    /// 且同级的其它字段照常解析、缺省字段照常落默认值。
    ///
    /// **v1.8.1（G6）扩表**：本轮又删掉了三组死配置（`window` 整棵、
    /// `appearance.accent_source` / `accent_custom`、`lyrics.mode`），
    /// 老用户的 `config.json` 里必然带着它们。这里把它们一起写进「旧版本写出来的
    /// 真实配置形状」里，钉住「删字段不影响旧文件加载」——这条比原来只测 `ui_scale`
    /// 更强，也是本轮唯一还能观测到 B-6/B-7/B-8 兼容性口径的地方
    /// （原来的 `legacy_two_line_fields_load_without_error` 随着被测字段一起删了）。
    #[test]
    fn legacy_ui_scale_field_is_ignored() {
        // 一段「旧版本写出来的真实配置形状」：既有已删的 ui_scale，也有正常字段，
        // 还有 v1.8.1 删掉的三组死配置键。
        let json = br##"{
            "version": 1,
            "appearance": {
                "ui_scale": 1.25,
                "background_mode": "transparent",
                "hover_alpha": 85,
                "accent_source": "custom",
                "accent_custom": "#112233"
            },
            "lyrics": {"enabled": false, "mode": "two_line"},
            "window": {
                "settings_w": 1024,
                "settings_h": 700,
                "settings_x": 10,
                "settings_y": 20,
                "lyrics_visible": true,
                "lyrics_two_line": true
            }
        }"##;
        let cfg: Config =
            serde_json::from_slice(json).expect("带已删除字段 ui_scale 的旧配置必须仍能加载");

        // 旧字段被忽略（不报错），同级字段照常解析
        assert_eq!(cfg.appearance.background_mode, BackgroundMode::Transparent);
        assert_eq!(cfg.appearance.hover_alpha, 85);
        assert_eq!(cfg.appearance.idle_mode, BackgroundMode::Transparent, "缺省字段落默认值");
        assert!(!cfg.lyrics.enabled);

        // 其余字段全靠 `#[serde(default)]` 补齐，不能因缺字段而失败
        assert_eq!(cfg.volume.step, 0.02);
        assert!(cfg.hotkeys.play_pause.is_bound());
    }

    /// v1.1.10 问题 5：`volume.step` 序列化必须按 0.01 粒度取整。
    ///
    /// serde_json 对 f32 先拓宽 f64（0.05f32 → 0.05000000074505806），
    /// 设置页 JS `String(value)` 与选项 "0.05" 永不相等 → 「每次步进」
    /// 一直空白。回归断言：序列化后的 JSON 数值必须严格等于干净值。
    #[test]
    fn volume_step_serializes_as_clean_percent() {
        // 期望值用 f64 字面量：json!(0.01f32 as f64) 会复现 f32→f64 拓宽，
        // 反而是被修掉的脏值
        for step in [0.01f64, 0.02, 0.05, 0.1] {
            let mut cfg = Config::default();
            cfg.volume.step = step as f32;
            let v = serde_json::to_value(&cfg).expect("序列化不应失败");
            assert_eq!(
                v["volume"]["step"],
                serde_json::json!(step),
                "step={step} 必须序列化为干净百分数"
            );
        }
    }

    #[test]
    fn empty_object_yields_full_defaults() {
        let cfg: Config = serde_json::from_slice(b"{}").expect("空对象应可用");
        assert_eq!(cfg.version, defaults::CURRENT_VERSION);
        assert_eq!(cfg.hotkeys.next.vk, defaults::vk::RIGHT);
    }

    // ------------------------------------------------ v1.9.0（G7）media 分组

    /// 旧配置（v1.8.1 及更早）没有 `media` 字段 → 反序列化不报错且两表均空
    /// （容器级 `#[serde(default)]` 兜底；ARCH-G7 B3 兼容性口径）。
    #[test]
    fn legacy_config_without_media_defaults_empty() {
        let cfg: Config = serde_json::from_slice(br#"{"version":1}"#).expect("旧配置必须仍能加载");
        assert!(cfg.media.blacklist.is_empty());
        assert!(cfg.media.app_names.is_empty());
    }

    /// `media` 双表随 `Config` 一起落盘：roundtrip 后逐字节一致
    /// （BTreeMap 保证 JSON 输出稳定；`blacklist` 顺序有意义不排序）。
    #[test]
    fn media_roundtrip_preserves_blacklist_and_app_names() {
        let mut cfg = Config::default();
        cfg.media.blacklist = vec!["cloudmusic.exe".to_string(), "msedge.exe".to_string()];
        cfg.media
            .app_names
            .insert("com.sesame.qishui.desktop".to_string(), "sodamusic.exe".to_string());
        let json = serde_json::to_vec(&cfg).expect("序列化");
        let back: Config = serde_json::from_slice(&json).expect("反序列化");
        assert_eq!(back.media, cfg.media);
        assert_eq!(back.version, cfg.version);
    }
}
