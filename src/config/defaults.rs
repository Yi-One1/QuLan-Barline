//! 全部默认值 + 容量常量（`ARCHITECTURE.md` §7.6）。
//!
//! 规矩：**任何魔法数都不许散落在业务代码里**，一律进 `limits`。
//! 这样「封面缓存条数」这类参数在 M5 调优时只需改一处。

/// 容量常量（写死，禁止散落魔法数）。
pub mod limits {
    /// 封面解码目标尺寸（超过则缩放）。64×64 BGRA = 16 KB
    pub const COVER_DECODE_PX: u32 = 64;
    /// 封面位图缓存条数
    pub const COVER_CACHE_CAP: usize = 8;
    /// 歌词内存 LRU 条数
    pub const LYRIC_LRU_CAP: usize = 8;
    /// 磁盘歌词缓存保留天数
    pub const LYRIC_DISK_TTL_DAYS: i64 = 30;
    /// 负缓存保留小时数（PRD 6.2：同一曲目失败后 24h 内不再请求）
    pub const LYRIC_MISS_TTL_HOURS: i64 = 24;
    /// 在线歌词重试退避（毫秒）
    pub const LYRIC_RETRY_BACKOFF_MS: [u64; 3] = [5_000, 15_000, 60_000];
    /// 小组件宽度范围（100% DPI 逻辑像素）
    pub const WIDGET_MIN_W: f32 = 200.0;
    pub const WIDGET_MAX_W: f32 = 340.0;
    /// 与托盘区左边缘的间距
    pub const TRAY_GAP: f32 = 8.0;
    /// 歌词区单行 / 双行附加高度
    pub const LYRIC_ONE_LINE_H: f32 = 26.0;
    pub const LYRIC_TWO_LINE_H: f32 = 46.0;
    /// 命中区最小边长
    pub const HIT_MIN: f32 = 28.0;
    /// 双击判定窗口
    pub const DOUBLE_CLICK_MS: u64 = 250;
    /// 拖动进入 Seeking 的阈值
    pub const SEEK_DRAG_PX: f32 = 3.0;
    pub const SEEK_DRAG_MS: u64 = 100;
    /// 背景采样节流
    pub const SAMPLE_THROTTLE_MS: u64 = 300;
    /// 任务栏轮询兜底周期
    pub const TASKBAR_POLL_MS: u32 = 500;
    /// 媒体轮询周期
    pub const MEDIA_POLL_MS: u64 = 500;
    /// 日志单文件上限
    pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

    // ------------------------------------------------------------------ M2 新增
    /// 悬停背景淡入时长（PRD P0-07：120ms）
    pub const HOVER_FADE_IN_MS: u32 = 120;
    /// 悬停背景淡出时长（PRD P0-07：200ms）
    pub const HOVER_FADE_OUT_MS: u32 = 200;
    /// 动画定时器周期（60fps 的整数近似）
    pub const ANIM_TIMER_MS: u32 = 16;
    /// 单击判定延迟（PRD §4.3：250ms 内第二下视为双击，不产生动作）
    pub const CLICK_DELAY_MS: u32 = 250;
    /// 拖动时时间气泡在松开后保留的时长（PRD P0-10：500ms 内消失）
    pub const SEEK_BUBBLE_MS: u32 = 500;
    /// 封面视觉边长（逻辑像素）
    pub const COVER_SIDE: f32 = 32.0;
    /// 封面圆角（逻辑像素）
    pub const COVER_RADIUS: f32 = 4.0;
    /// 按钮图标边长
    pub const BTN_ICON: f32 = 16.0;
    /// 按钮命中区边长（PRD：≥ 28×28）
    pub const BTN_HIT: f32 = 28.0;
    /// 按钮间距
    pub const BTN_GAP: f32 = 6.0;
    /// 歌名字号
    pub const TITLE_PX: f32 = 14.0;
    /// 歌手字号
    pub const ARTIST_PX: f32 = 11.0;
    /// 歌词字号
    pub const LYRIC_PX: f32 = 13.0;
    /// HUD / 气泡字号
    pub const HUD_PX: f32 = 12.0;
    /// 进度条静止高度
    pub const PROGRESS_H_IDLE: f32 = 2.0;
    /// 进度条悬停高度
    pub const PROGRESS_H_HOVER: f32 = 4.0;
    /// 封面与文本块的间距
    pub const COVER_TEXT_GAP: f32 = 10.0;
    /// 文本块与按钮组的间距
    pub const TEXT_BTN_GAP: f32 = 8.0;
    /// 内容区左右安全边距
    pub const CONTENT_PAD: f32 = 8.0;
    /// 背景采样的块尺寸（4×4 像素一块，对应 §11.2 的「4×4 块统计」）
    pub const SAMPLE_BLOCK: u32 = 4;
    /// 块内 alpha > 8 的像素占比超过它就算「我们自己画的」（§11.2 的 0.10）
    pub const SAMPLE_COVERED_RATIO: f32 = 0.10;
    /// 有效块占比低于它 → 退回全块平均（§11.2 第 4 步）
    pub const SAMPLE_MIN_VALID_RATIO: f32 = 0.30;
    /// 文本区域块的亮度权重倍数（§11.2 第 3 步）
    pub const SAMPLE_TEXT_WEIGHT: f32 = 3.0;
    /// 模糊盒半径（块空间，块 = 4 物理像素 → 半径 3 ≈ 12 物理像素）
    pub const BLUR_RADIUS: i32 = 3;
    /// 模糊遍数（3 次 box ≈ 高斯）
    pub const BLUR_PASSES: u32 = 3;
    /// 模糊结果的整体不透明度（acrylic 观感：略透）
    pub const BLUR_ALPHA: u8 = 0xE6;
    /// 模糊结果上叠加的强调色色调 alpha（§11.3 第 5 步的 0x1A）
    pub const BLUR_TINT_ALPHA: u8 = 0x1A;
    /// 与系统媒体控件之间的间距
    /// 主文字候选色（深 / 浅）
    pub const TEXT_DARK: u8 = 0x0A;
    pub const TEXT_LIGHT: u8 = 0xFA;
    /// WCAG AA 最小对比度
    pub const MIN_CONTRAST: f32 = 4.5;
}

/// 配置结构版本。`Config.version < CURRENT_VERSION` 时走迁移链。
pub const CURRENT_VERSION: u32 = 1;

/// 热键修饰位（与 `MOD_*` 一致）。
pub mod mods {
    pub const ALT: u32 = 0x0001;
    pub const CONTROL: u32 = 0x0002;
    pub const SHIFT: u32 = 0x0004;
    pub const WIN: u32 = 0x0008;
    pub const NOREPEAT: u32 = 0x4000;
}

/// 常用虚拟键码（`RegisterHotKey` 用）。
pub mod vk {
    pub const P: u32 = 0x50;
    pub const L: u32 = 0x4C;
    pub const M: u32 = 0x4D;
    pub const H: u32 = 0x48;
    pub const LEFT: u32 = 0x25;
    pub const RIGHT: u32 = 0x27;
}

use super::{
    AccentSource, AppearanceConfig, Config, GeneralConfig, HotkeyBinding, HotkeyConfig,
    LangSerde, LyricsConfig, LyricModeSerde, VolumeConfig, VolumeScope, WindowStateConfig,
};
use crate::render::theme::BackgroundMode;

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            appearance: AppearanceConfig::default(),
            lyrics: LyricsConfig::default(),
            volume: VolumeConfig::default(),
            hotkeys: HotkeyConfig::default(),
            general: GeneralConfig::default(),
            window: WindowStateConfig::default(),
        }
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            background_mode: BackgroundMode::Blur,
            accent_source: AccentSource::Cover,
            accent_custom: String::from("#0078D4"),
            ui_scale: 1.0,
            monitor_index: 0,
        }
    }
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: LyricModeSerde::TwoLine,
            karaoke_sweep: true,
            online_enabled: true,
            local_dirs: Vec::new(),
        }
    }
}

impl Default for VolumeConfig {
    fn default() -> Self {
        Self {
            wheel_enabled: true,
            scope: VolumeScope::System,
            step: 0.02,
            hud_ms: 1200,
        }
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        let ctrl_alt = mods::CONTROL | mods::ALT;
        Self {
            enabled: true,
            play_pause: HotkeyBinding {
                modifiers: ctrl_alt,
                vk: vk::P,
            },
            prev: HotkeyBinding {
                modifiers: ctrl_alt,
                vk: vk::LEFT,
            },
            next: HotkeyBinding {
                modifiers: ctrl_alt,
                vk: vk::RIGHT,
            },
            toggle_lyrics: HotkeyBinding {
                modifiers: ctrl_alt,
                vk: vk::L,
            },
            mute: HotkeyBinding {
                modifiers: ctrl_alt,
                vk: vk::M,
            },
            toggle_widget: HotkeyBinding {
                modifiers: ctrl_alt,
                vk: vk::H,
            },
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            autostart: false,
            language: LangSerde::System,
            widget_hidden: false,
        }
    }
}

impl Default for WindowStateConfig {
    fn default() -> Self {
        Self {
            settings_w: 880,
            settings_h: 640,
            settings_x: i32::MIN, // MIN 表示「居中」
            settings_y: i32::MIN,
            lyrics_visible: true,
            lyrics_two_line: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkeys_are_bound_and_unique() {
        let hk = HotkeyConfig::default();
        let all = [
            hk.play_pause,
            hk.prev,
            hk.next,
            hk.toggle_lyrics,
            hk.mute,
            hk.toggle_widget,
        ];
        for b in all {
            assert!(b.is_bound(), "默认热键必须已绑定");
        }
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(
                    (all[i].modifiers, all[i].vk),
                    (all[j].modifiers, all[j].vk),
                    "默认热键不能重复"
                );
            }
        }
    }

    #[test]
    fn limits_are_sane() {
        assert!(limits::WIDGET_MIN_W < limits::WIDGET_MAX_W);
        assert!(limits::TRAY_GAP > 0.0);
        assert_eq!(limits::LOG_MAX_BYTES, crate::log::LOG_MAX_BYTES);
    }
}
