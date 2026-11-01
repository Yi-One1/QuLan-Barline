//! 配置持久化（`ARCHITECTURE.md` §7.5 / §14.1）。
//!
//! - 路径：`%LOCALAPPDATA%\Barline\config.json`（`SHGetKnownFolderPath`，不用环境变量）
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

/// 强调色来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccentSource {
    #[default]
    Cover,
    System,
    Custom,
}

/// 独立的 serde 枚举，避免 render 层类型泄漏到配置层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricModeSerde {
    #[default]
    TwoLine,
    OneLine,
}

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
    pub const NONE: Self = Self {
        modifiers: 0,
        vk: 0,
    };

    pub fn is_bound(&self) -> bool {
        self.vk != 0
    }
}

/// 外观。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    pub background_mode: BackgroundMode,
    /// 强调色来源：封面 / 系统 / 自定义
    pub accent_source: AccentSource,
    /// 自定义强调色，形如 "#0078D4"
    pub accent_custom: String,
    /// 界面缩放：1.0 / 1.1 / 1.25
    pub ui_scale: f32,
    /// 显示显示器：0 = 主显示器，否则为显示器序号
    pub monitor_index: u32,
}

/// 歌词。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsConfig {
    pub enabled: bool,
    pub mode: LyricModeSerde,
    pub karaoke_sweep: bool,
    pub online_enabled: bool,
    /// 本地歌词搜索目录（绝对路径）。空数组表示只用默认目录。
    pub local_dirs: Vec<String>,
}

/// 音量。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VolumeConfig {
    pub wheel_enabled: bool,
    pub scope: VolumeScope,
    /// 每格步进：0.01 / 0.02 / 0.05
    pub step: f32,
    /// HUD 停留时长（毫秒）：800 / 1200 / 2000
    pub hud_ms: u32,
}

/// 热键（6 组）。全部字段都是 `Copy`，整体也 `Copy`——注册时要同时借用
/// `config.hotkeys` 与 `&mut app.hotkeys`，值语义最省事。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub enabled: bool,
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
}

/// 记忆的窗口 / 交互状态，重启后恢复。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowStateConfig {
    pub settings_w: i32,
    pub settings_h: i32,
    pub settings_x: i32,
    pub settings_y: i32,
    pub lyrics_visible: bool,
    pub lyrics_two_line: bool,
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
    pub window: WindowStateConfig,
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
            Ok(cfg) => {
                crate::log_info!(
                    "配置已加载：version={} lyrics.mode={:?} ui_scale={}",
                    cfg.version,
                    cfg.lyrics.mode,
                    cfg.appearance.ui_scale
                );
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
        assert_eq!(back.appearance.ui_scale, cfg.appearance.ui_scale);
        assert_eq!(back.appearance.accent_custom, cfg.appearance.accent_custom);
        assert_eq!(back.lyrics.mode, cfg.lyrics.mode);
        assert_eq!(back.lyrics.enabled, cfg.lyrics.enabled);
        assert_eq!(back.volume.step, cfg.volume.step);
        assert_eq!(back.volume.hud_ms, cfg.volume.hud_ms);
        assert_eq!(back.hotkeys.play_pause, cfg.hotkeys.play_pause);
        assert_eq!(back.hotkeys.toggle_widget, cfg.hotkeys.toggle_widget);
        assert_eq!(back.general.language, cfg.general.language);
        assert_eq!(back.window.settings_w, cfg.window.settings_w);
        assert_eq!(back.window.lyrics_two_line, cfg.window.lyrics_two_line);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // 只给一个字段，其余全靠 #[serde(default)]
        let json = br#"{"version":1,"appearance":{"ui_scale":1.25}}"#;
        let cfg: Config = serde_json::from_slice(json).expect("缺字段应能用默认值补齐");
        assert_eq!(cfg.appearance.ui_scale, 1.25);
        assert_eq!(cfg.appearance.accent_custom, "#0078D4");
        assert!(cfg.lyrics.enabled);
        assert_eq!(cfg.volume.step, 0.02);
        assert!(cfg.hotkeys.play_pause.is_bound());
    }

    #[test]
    fn empty_object_yields_full_defaults() {
        let cfg: Config = serde_json::from_slice(b"{}").expect("空对象应可用");
        assert_eq!(cfg.version, defaults::CURRENT_VERSION);
        assert_eq!(cfg.hotkeys.next.vk, defaults::vk::RIGHT);
    }

    #[test]
    fn hex_colour_parsing_is_used_by_appearance() {
        use crate::media::Rgb;
        assert_eq!(
            Rgb::parse_hex(&AppearanceConfig::default().accent_custom),
            Some(Rgb::SYSTEM_ACCENT_FALLBACK)
        );
    }
}
