//! 主题与背景数据（`ARCHITECTURE.md` §7.4 / §11.2 / §11.4）。
//!
//! 本模块只做**纯计算**：给定背景相对亮度与强调色，算出「最终可画」的颜色集合。
//! 抓屏与模糊在 [`crate::render::sampler`]（T3 线程），绘制在
//! [`crate::render::painter`]，三者互不耦合。
//!
//! 两条与验收直接相关的规则：
//! 1. **黑白切换按对比度择优，而不是按 `luma > 0.5` 硬切**（PRD P0-07 要求 ≥ 4.5:1）。
//!    纯中灰（#808080，相对亮度 0.216）下白字只有 3.78:1、黑字有 4.98:1，
//!    用 0.5 阈值会选错。正确的交叉点是 0.179（`(L+0.05)² = 0.05·1.05` 的解）。
//! 2. 强调色必须做**亮度归一化**（§11.4 第 6 步），否则深色封面会让进度条在深色壁纸上消失。

use serde::{Deserialize, Serialize};

use crate::media::Rgb;

/// 带 alpha 的颜色（直通 alpha，非预乘）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgb(c: Rgb, a: u8) -> Self {
        Self {
            r: c.r,
            g: c.g,
            b: c.b,
            a,
        }
    }

    /// 按比例缩放 alpha（用于淡入淡出）。`t` ∈ 0..=1。
    pub fn with_alpha_scale(self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            a: ((self.a as f32) * t).round() as u8,
            ..self
        }
    }

    /// 打包成 `0xAARRGGBB`，作为笔刷缓存的 key。
    pub fn packed(self) -> u32 {
        ((self.a as u32) << 24)
            | ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | (self.b as u32)
    }
}

/// 背景模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundMode {
    /// 悬停时显示「抓屏 + 自研 box blur」的模糊背景（§11.3）
    #[default]
    Blur,
    /// 悬停时显示半透明纯色（70% 不透明度，PRD §4.2）
    Translucent,
}

impl BackgroundMode {
    /// 日志 / 菜单友好名。
    pub fn as_str(self) -> &'static str {
        match self {
            BackgroundMode::Blur => "Blur",
            BackgroundMode::Translucent => "Translucent",
        }
    }
}

/// 主题。所有颜色都是「最终可画」的值，渲染层不做二次推导。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// 背景平均相对亮度（0..=1），调试用
    pub backdrop_luma: f32,
    /// 背景偏亮 → 文字 / 图标用深色
    pub on_light: bool,
    /// 主文字色
    pub text: Rgb,
    /// 次要文字色（歌手、占位、非当前歌词）
    pub text_dim: Rgb,
    /// 强调色（封面主色或系统强调色或自定义）
    pub accent: Rgb,
    /// 悬停背景色（含 alpha）
    pub hover_bg: Rgba,
    /// 进度条轨道色
    pub track_bg: Rgba,
    /// 是否使用模糊背景
    pub use_blur: bool,
    /// 对比度推到极端仍不达标（背景正好是中灰）→ 文字需要 1px 反色描边
    pub needs_outline: bool,
}

impl Default for Theme {
    /// 兜底主题：深色壁纸假设（首帧还没有采样结果时用）。
    fn default() -> Self {
        Self::from_backdrop(0.0, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur)
    }
}

impl Theme {
    /// 深色文字（浅色壁纸）
    pub const DARK_TEXT: Rgb = Rgb::new(0x0A, 0x0A, 0x0A);
    /// 浅色文字（深色壁纸）
    pub const LIGHT_TEXT: Rgb = Rgb::new(0xFA, 0xFA, 0xFA);
    /// 次要文字的 alpha（PRD §4.1：60% 不透明度）
    pub const DIM_ALPHA: u8 = 0x99;

    /// 由背景亮度 + 强调色 + 背景模式算出完整主题。
    ///
    /// `luma` 是 0..=1 的**相对亮度**（已做 sRGB → 线性）。
    pub fn from_backdrop(luma: f32, accent: Rgb, mode: BackgroundMode) -> Self {
        let luma = luma.clamp(0.0, 1.0);

        // 用「与该亮度等效的中性灰」作为背景代表色，做对比度择优。
        let bg_gray = luma_to_srgb_gray(luma);
        let dark = Self::DARK_TEXT;
        let light = Self::LIGHT_TEXT;
        let c_dark = contrast_ratio(dark, bg_gray);
        let c_light = contrast_ratio(light, bg_gray);
        let (text, on_light) = if c_dark >= c_light {
            (dark, true)
        } else {
            (light, false)
        };
        let best = c_dark.max(c_light);
        let needs_outline = best < crate::config::defaults::limits::MIN_CONTRAST;

        // 次要文字：主文字与背景按 40% 混合（等效于 60% 不透明度压在背景上）。
        let text_dim = mix_rgb(text, bg_gray, 0.4);

        let accent = normalize_accent(accent);

        // 半透明背景色：浅背景用白、深背景用近黑，alpha = 70%（PRD §4.2）
        let hover_bg = if on_light {
            Rgba::new(0xF2, 0xF2, 0xF4, 0xB3)
        } else {
            Rgba::new(0x1A, 0x1A, 0x1E, 0xB3)
        };

        // 进度条轨道：与文字同侧的低对比色，保证在任何背景上都看得见
        let track_bg = if on_light {
            Rgba::new(0x00, 0x00, 0x00, 0x2E)
        } else {
            Rgba::new(0xFF, 0xFF, 0xFF, 0x38)
        };

        Self {
            backdrop_luma: luma,
            on_light,
            text,
            text_dim,
            accent,
            hover_bg,
            track_bg,
            use_blur: mode == BackgroundMode::Blur,
            needs_outline,
        }
    }

    /// 主文字在该背景亮度下的对比度（验收日志用）。
    pub fn text_contrast(&self) -> f32 {
        contrast_ratio(self.text, luma_to_srgb_gray(self.backdrop_luma))
    }

    /// 两份主题在**绘制结果上**是否等价。
    ///
    /// 刻意忽略 `backdrop_luma`：它每次采样都会有 ±0.001 的抖动，直接比
    /// `PartialEq` 会让「1s 心跳重新采样」每次都判定为变化 → 每秒白重绘一帧，
    /// 空闲 CPU 从 ~0 涨到 1%（PRD P0-14 的硬指标是 0.5%）。
    pub fn same_paint(&self, other: &Self) -> bool {
        self.on_light == other.on_light
            && self.text == other.text
            && self.text_dim == other.text_dim
            && self.accent == other.accent
            && self.hover_bg == other.hover_bg
            && self.track_bg == other.track_bg
            && self.use_blur == other.use_blur
            && self.needs_outline == other.needs_outline
    }
}

/// 背景模糊结果。由 T3（[`crate::render::sampler`]）生产，UI 线程只读。
///
/// 注：`ARCHITECTURE.md` 把本类型标注在 `render/sampler.rs`，M1 为了先定义
/// `AppEvent::BackdropReady` 放在本文件；M2 沿用（避免无谓的搬动），
/// 语义与字段与契约逐字一致。
#[derive(Debug, Clone)]
pub struct BlurBuffer {
    pub width: u32,
    pub height: u32,
    /// 预乘 alpha 的 BGRA，尺寸 = 窗口物理尺寸
    pub bgra: Vec<u8>,
    /// 该缓冲对应的几何代次，避免用到过期背景
    pub geometry_epoch: u64,
}

/// sRGB 分量（0..=1）→ 线性分量。
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 线性分量 → sRGB（0..=1）。
pub fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// 相对亮度（WCAG 定义，输入为 sRGB 0..=1 的 8 位色）。
pub fn relative_luminance(c: Rgb) -> f32 {
    let r = srgb_to_linear(c.r as f32 / 255.0);
    let g = srgb_to_linear(c.g as f32 / 255.0);
    let b = srgb_to_linear(c.b as f32 / 255.0);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// 单个 8 位 sRGB 通道 → 线性（查表版，采样热路径用；与 [`srgb_to_linear`] 等价）。
pub fn srgb8_to_linear(v: u8) -> f32 {
    srgb_to_linear(v as f32 / 255.0)
}

/// 相对亮度 → 等效中性灰（8 位 sRGB）。
pub fn luma_to_srgb_gray(luma: f32) -> Rgb {
    let v = (linear_to_srgb(luma.clamp(0.0, 1.0)) * 255.0).round() as i32;
    let v = v.clamp(0, 255) as u8;
    Rgb::new(v, v, v)
}

/// WCAG 对比度 `(L1 + 0.05) / (L2 + 0.05)`。
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// 两色线性插值（`t = 0` 取 `a`，`t = 1` 取 `b`）。
pub fn mix_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round().clamp(0.0, 255.0) as u8;
    Rgb::new(f(a.r, b.r), f(a.g, b.g), f(a.b, b.b))
}

/// 强调色亮度归一化（§11.4 第 6 步）：
/// 相对亮度落在 0.25..=0.85 之外时，把明度拉回该区间，保证进度条在任何封面上都可见。
pub fn normalize_accent(c: Rgb) -> Rgb {
    let l = relative_luminance(c);
    if (0.25..=0.85).contains(&l) {
        return c;
    }
    // 在 sRGB 空间按比例调明度（保持色相与饱和度，避免偏色）
    let target = if l < 0.25 { 0.35 } else { 0.75 };
    let cur = l.max(0.0001);
    let k = (target / cur).sqrt().clamp(0.35, 3.0);
    let f = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
    let boosted = Rgb::new(f(c.r), f(c.g), f(c.b));
    // 夹紧后仍越界（极端纯黑封面）→ 直接退到系统强调色
    let l2 = relative_luminance(boosted);
    if l2 < 0.12 {
        Rgb::SYSTEM_ACCENT_FALLBACK
    } else {
        boosted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_white_contrast_is_21() {
        let c = contrast_ratio(Rgb::new(0, 0, 0), Rgb::new(255, 255, 255));
        assert!((c - 21.0).abs() < 0.05, "contrast = {c}");
    }

    #[test]
    fn dark_backdrop_gets_light_text() {
        let t = Theme::from_backdrop(0.0, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur);
        assert!(!t.on_light);
        assert_eq!(t.text, Theme::LIGHT_TEXT);
        assert!(t.text_contrast() >= 4.5);
    }

    #[test]
    fn light_backdrop_gets_dark_text() {
        let t = Theme::from_backdrop(1.0, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur);
        assert!(t.on_light);
        assert_eq!(t.text, Theme::DARK_TEXT);
        assert!(t.text_contrast() >= 4.5);
    }

    /// 纯中灰是最容易选错的一档：白字 3.78:1 不达标，黑字 4.98:1 达标。
    #[test]
    fn mid_gray_picks_the_readable_side() {
        let luma = relative_luminance(Rgb::new(0x80, 0x80, 0x80));
        let t = Theme::from_backdrop(luma, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur);
        assert!(t.on_light, "中灰必须选深色文字");
        assert!(
            t.text_contrast() >= 4.5,
            "中灰背景对比度 {} 未达 AA",
            t.text_contrast()
        );
        assert!(!t.needs_outline);
    }

    #[test]
    fn accent_luma_is_normalised() {
        // 极深与极浅的强调色都要被拉进可读区间
        for c in [
            Rgb::new(0, 0, 0),
            Rgb::new(2, 2, 4),
            Rgb::new(255, 255, 255),
        ] {
            let a = normalize_accent(c);
            let l = relative_luminance(a);
            assert!(l >= 0.10, "归一化后亮度 {l} 太低");
        }
    }

    #[test]
    fn translucent_mode_disables_blur() {
        let t = Theme::from_backdrop(0.2, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Translucent);
        assert!(!t.use_blur);
    }

    #[test]
    fn luma_gray_roundtrip_is_stable() {
        for l in [0.0f32, 0.05, 0.2, 0.5, 0.9, 1.0] {
            let g = luma_to_srgb_gray(l);
            let back = relative_luminance(g);
            assert!((back - l).abs() < 0.02, "luma {l} → gray → {back}");
        }
    }

    #[test]
    fn alpha_scaling_keeps_rgb() {
        let c = Rgba::new(1, 2, 3, 0xFF).with_alpha_scale(0.5);
        assert_eq!((c.r, c.g, c.b), (1, 2, 3));
        assert_eq!(c.a, 0x80);
    }

    /// `same_paint` 必须忽略 luma 抖动，否则 1s 心跳采样会让空闲 CPU 涨到 1%。
    #[test]
    fn same_paint_ignores_luma_jitter() {
        let a = Theme::from_backdrop(0.20, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur);
        let mut b = a;
        b.backdrop_luma = 0.200_4;
        assert_ne!(a, b, "整体 PartialEq 会认为不同");
        assert!(a.same_paint(&b), "绘制结果必须认为相同");
    }

    /// 文字颜色真的变了就必须判定为不同（否则黑字会留在深色背景上）。
    #[test]
    fn same_paint_detects_text_flip() {
        let dark = Theme::from_backdrop(0.02, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur);
        let light = Theme::from_backdrop(0.95, Rgb::SYSTEM_ACCENT_FALLBACK, BackgroundMode::Blur);
        assert!(!dark.same_paint(&light));
    }
}
