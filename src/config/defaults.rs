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
    /// 歌词区最小宽度（逻辑像素；v1.1.1：信息区宽 = max(歌名+歌手自然宽, 此值)，
    /// 歌词开关过渡动画把信息区最小宽在 0 ↔ 此值之间插值）
    pub const LYRIC_W: f32 = 160.0;
    /// 歌词区单行矩形高度（逻辑像素；48px 条带 = 2 + 22 + 22 + 2，上行歌名歌手、下行歌词）
    pub const LYRIC_LINE_H: f32 = 22.0;
    /// 上下行边距（逻辑像素）
    pub const LYRIC_PAD: f32 = 2.0;
    /// 上行内歌名与歌手名的横向间隔（逻辑像素）
    pub const TITLE_ARTIST_GAP: f32 = 6.0;
    /// 下行歌词相对行矩形中心的整体上移量（逻辑像素，v1.1.3：再上移 2px，累计 3.5px）
    pub const LYRIC_LIFT: f32 = 3.5;
    /// 长歌词滚动（跑马灯）：滚动窗口低于该值（秒）不滚，只显示行首（v1.1.4）。
    /// 窗口太短的行在窗口内匀速滚完全程会快到看不清，宁可只显示行首。
    pub const MARQUEE_MIN_WINDOW_SECS: f32 = 1.5;
    /// 长歌词滚动：**提前量**（v1.1.4）——比「最后一个字唱完」提前这么多毫秒
    /// 滚到尾，行尾 + 行间间歇留给用户读最后一个字；实际提前量同时钳到
    /// 演唱窗口的 1/4，避免短句提前太多导致滚动过快。
    pub const MARQUEE_LEAD_MS: i64 = 1200;
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
    /// 歌名字号（v1.1.1：两行结构内歌名最大、歌词略小、歌手最小偏灰）
    pub const TITLE_PX: f32 = 12.0;
    /// 歌手字号
    pub const ARTIST_PX: f32 = 10.0;
    /// 歌词字号（v1.1.1 单行大字：7 → 11px，用户核心诉求「歌词大一点」）
    pub const LYRIC_PX: f32 = 11.0;
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

/// 快捷键默认值的版本号。
///
/// 0 = M2/M3 时代的 `Ctrl+Alt+*`；1 = 现在的 `Ctrl+Alt+Shift+*`。
/// 为什么要版本号：旧配置里存的是**旧默认值**，但那也可能是用户**手动选过的**
/// 同款组合。只有"6 组全都等于旧默认值"才能判定为"用户没改过"→ 可以迁移；
/// 改过任何一组就原样保留（见 `HotkeyConfig::migrate`）。
pub const HOTKEYS_VERSION: u32 = 1;

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
            storage: crate::config::StorageConfig::default(),
        }
    }
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            background_mode: BackgroundMode::Translucent,
            idle_mode: BackgroundMode::Transparent,
            hover_alpha: 70,
            idle_alpha: 50,
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
            // v1.1.1 起行数固定单行：`mode` 字段仅为旧配置反序列化兼容保留，
            // 布局层不再读取（见 `WidgetForm::from_lyric_enabled`）
            mode: LyricModeSerde::OneLine,
            karaoke_sweep: true,
            // v1.1.2：长歌词滚动（跑马灯），默认开
            scroll: true,
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

impl HotkeyConfig {
    /// **旧**默认值（`Ctrl+Alt+*`）。只用于迁移判定，不要拿来注册。
    ///
    /// 实测这台机器上 `Ctrl+Alt+P / ← / → / L / M` 全被其它软件占用（1409），
    /// 所以默认值整体换成 `Ctrl+Alt+Shift+*`。
    pub fn legacy_defaults() -> Self {
        let ctrl_alt = mods::CONTROL | mods::ALT;
        Self {
            enabled: true,
            hotkeys_version: 0,
            conflict_notified: false,
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

    /// 6 组绑定是否完全一致（不含 `enabled` 与元数据字段）。
    pub fn same_bindings(&self, other: &Self) -> bool {
        self.play_pause == other.play_pause
            && self.prev == other.prev
            && self.next == other.next
            && self.toggle_lyrics == other.toggle_lyrics
            && self.mute == other.mute
            && self.toggle_widget == other.toggle_widget
    }

    /// 旧配置迁移：只有「版本 0 + 6 组全是旧默认值」才换成新默认值。
    /// 返回 `true` 表示配置被改动，调用方应落盘。
    pub fn migrate(&mut self) -> bool {
        if self.hotkeys_version >= HOTKEYS_VERSION {
            return false;
        }
        let legacy = Self::legacy_defaults();
        if self.same_bindings(&legacy) {
            let enabled = self.enabled;
            let notified = self.conflict_notified;
            *self = Self::default();
            self.enabled = enabled;
            self.conflict_notified = notified;
            crate::log_info!("快捷键默认值已从 Ctrl+Alt+* 迁移到 Ctrl+Alt+Shift+*");
        } else {
            crate::log_info!("快捷键默认值保持用户自定义值（不迁移）");
        }
        self.hotkeys_version = HOTKEYS_VERSION;
        true
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        let ctrl_alt_shift = mods::CONTROL | mods::ALT | mods::SHIFT;
        Self {
            enabled: true,
            hotkeys_version: HOTKEYS_VERSION,
            conflict_notified: false,
            play_pause: HotkeyBinding {
                modifiers: ctrl_alt_shift,
                vk: vk::P,
            },
            prev: HotkeyBinding {
                modifiers: ctrl_alt_shift,
                vk: vk::LEFT,
            },
            next: HotkeyBinding {
                modifiers: ctrl_alt_shift,
                vk: vk::RIGHT,
            },
            toggle_lyrics: HotkeyBinding {
                modifiers: ctrl_alt_shift,
                vk: vk::L,
            },
            mute: HotkeyBinding {
                modifiers: ctrl_alt_shift,
                vk: vk::M,
            },
            toggle_widget: HotkeyBinding {
                modifiers: ctrl_alt_shift,
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
