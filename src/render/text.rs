//! 文本格式与测量（`ARCHITECTURE.md` §9.3 / T06 第 2 条）。
//!
//! ## 字号换算规则（固化下来，不要再改）
//!
//! WIC 渲染目标的 DPI 被设成显示器 DPI，所以 1 DIP = 1 物理像素。
//! 因此「逻辑像素 × DPI 缩放 = DIP 字号」，`scale = dpi / 96`。
//!
//! ## 为什么省略号要缓存（M1 报告 §5.9 的技术债）
//!
//! `ellipsis()` 内部要建临时 `IDWriteTextLayout`，每帧调用会破坏「空闲 CPU ≈ 0」。
//! M2 的做法：只在**文本或宽度变化时**算一次，结果存进 `WidgetLayout.title_text` /
//! `artist_text`，渲染热路径直接读字符串。

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_HIT_TEST_METRICS, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
};

use crate::config::defaults::limits;
use crate::platform::wide::to_utf16;

/// 字体族。Win11 一定有 `Segoe UI`；`Segoe UI Variable Text` 不是所有版本都在。
pub const FONT_FAMILY: PCWSTR = w!("Segoe UI");
/// 中英文混排的 locale，影响回退字体的选择（中文优先用微软雅黑族）。
pub const LOCALE: PCWSTR = w!("zh-CN");

/// 省略号字符（单字符，宽度固定，便于预算）。
const ELLIPSIS: &str = "…";

/// 文本格式的用途。用于 `measure` 时选对格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Title,
    Artist,
    Lyric,
    Hud,
}

/// 文本格式集合（4 个 `IDWriteTextFormat` + 1 个 HUD 格式）。
///
/// 只在 DPI / 尺寸变化时重建（`Renderer::ensure` 里做），不在渲染热路径创建。
pub struct TextFormats {
    /// 歌名：14px SemiBold
    pub title: IDWriteTextFormat,
    /// 歌手：11px Regular @ 60% 不透明度（alpha 在 painter 里给）
    pub artist: IDWriteTextFormat,
    /// 歌词当前行 / 下一行：7px（v1.1 双行压缩，两行压进 48px 条带）
    pub lyric: IDWriteTextFormat,
    /// HUD / 时间气泡：12px Regular
    pub hud: IDWriteTextFormat,
    /// HUD 居中版（气泡里的时间 / 音量百分比都居中，省掉每帧测量）
    pub hud_center: IDWriteTextFormat,
    /// 本次构建用的缩放（诊断用）
    pub scale: f32,
}

impl TextFormats {
    /// 建全部文本格式。`scale` 是 DPI 缩放（96dpi → 1.0）。
    pub fn new(dwrite: &IDWriteFactory, scale: f32) -> Result<Self> {
        let s = if scale > 0.0 { scale } else { 1.0 };
        let title = create(
            dwrite,
            limits::TITLE_PX * s,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let artist = create(
            dwrite,
            limits::ARTIST_PX * s,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let lyric = create(
            dwrite,
            limits::LYRIC_PX * s,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let hud = create(
            dwrite,
            limits::HUD_PX * s,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_LEADING,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        let hud_center = create(
            dwrite,
            limits::HUD_PX * s,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
        )?;
        Ok(Self {
            title,
            artist,
            lyric,
            hud,
            hud_center,
            scale: s,
        })
    }

    /// 按用途取格式。
    pub fn get(&self, kind: TextKind) -> &IDWriteTextFormat {
        match kind {
            TextKind::Title => &self.title,
            TextKind::Artist => &self.artist,
            TextKind::Lyric => &self.lyric,
            TextKind::Hud => &self.hud,
        }
    }

    /// 测量一段文本在给定最大宽度下的实际宽度（DIP）。
    ///
    /// 只在布局 / 文本变化时调用（不是渲染热路径）。
    pub fn measure(
        &self,
        dwrite: &IDWriteFactory,
        kind: TextKind,
        text: &str,
        max_w: f32,
    ) -> f32 {
        if text.is_empty() || max_w <= 0.0 {
            return 0.0;
        }
        let utf16 = to_utf16(text);
        let format = self.get(kind);
        // SAFETY: utf16 生命周期覆盖调用；format 由 self 持有；宽高都是正的有限值。
        let layout = unsafe { dwrite.CreateTextLayout(&utf16, format, max_w, 4096.0) };
        let Ok(layout) = layout else {
            return 0.0;
        };
        metrics_width(&layout)
    }

    /// 单行省略：超宽时**二分**回退并补 `…`。
    ///
    /// 用二分而不是逐字回退：一个 60 字的中文标题逐字试要建 60 个 `IDWriteTextLayout`，
    /// 二分只要 6 个。虽然两者都只在文本变化时发生，但二分的成本可预测得多。
    pub fn ellipsis(
        &self,
        dwrite: &IDWriteFactory,
        kind: TextKind,
        text: &str,
        max_w: f32,
    ) -> String {
        if text.is_empty() || max_w <= 0.0 {
            return String::new();
        }
        if self.measure(dwrite, kind, text, max_w) <= max_w {
            return text.to_string();
        }
        // 连一个省略号都放不下
        if self.measure(dwrite, kind, ELLIPSIS, max_w) > max_w {
            return String::new();
        }

        // 所有合法字符边界（UTF-8 安全切分点）
        let bounds: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(core::iter::once(text.len()))
            .collect();

        let mut lo = 0usize; // 已知「前缀 + …」放得下的最大下标
        let mut hi = bounds.len() - 1; // 已知放不下（= 全串）

        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            let cut = bounds[mid];
            let candidate = format!("{}{}", text[..cut].trim_end(), ELLIPSIS);
            if self.measure(dwrite, kind, &candidate, max_w) <= max_w {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        if lo == 0 {
            ELLIPSIS.to_string()
        } else {
            format!("{}{}", text[..bounds[lo]].trim_end(), ELLIPSIS)
        }
    }

    /// 建一个**可复用**的 `IDWriteTextLayout`。
    ///
    /// 只给歌词当前行用：扫光需要把「第 N 个码元」映射到 x 坐标
    /// （[`hit_test_x`]），而只有 `IDWriteTextLayout` 知道每个字形的实际位置。
    ///
    /// **只在行文本 / 宽度变化时调用**（结果缓存在 `Renderer` 里），
    /// 绝不能在渲染热路径上每帧新建——那是 PRD P0-14「空闲 CPU ≤ 0.5%」的头号杀手。
    pub fn layout(
        &self,
        dwrite: &IDWriteFactory,
        kind: TextKind,
        text: &str,
        max_w: f32,
    ) -> Option<IDWriteTextLayout> {
        if text.is_empty() || max_w <= 0.0 {
            return None;
        }
        let utf16 = to_utf16(text);
        let format = self.get(kind);
        // SAFETY: utf16 生命周期覆盖调用（CreateTextLayout 会复制内容）；
        // format 由 self 持有；宽高都是正的有限值。
        unsafe { dwrite.CreateTextLayout(&utf16, format, max_w, 4096.0).ok() }
    }
}

/// 把「行内 UTF-16 位置」映射成「相对行首的 x 偏移」。
///
/// `text_len` 是这一行文本的 UTF-16 码元数（调用方知道，不用去问 layout）。
/// `pos` 是**连续值**（例如 `3.4` = 第 4 个码元扫了 40%），所以要在
/// 两个整数位置之间线性插值——这样扫光才会平滑移动，而不是一个字一个字地跳。
///
/// 越界与命中失败都返回 `0.0`（宁可少扫一点，也不要画到行外）。
pub fn hit_test_x(layout: &IDWriteTextLayout, text_len: u32, pos: f32) -> f32 {
    if text_len == 0 {
        return 0.0;
    }
    let clamped = pos.clamp(0.0, text_len as f32);
    let i0 = clamped.floor() as u32;
    let i1 = (i0 + 1).min(text_len);
    let frac = clamped - i0 as f32;
    let x0 = x_at(layout, i0);
    if frac <= 0.001 || i1 == i0 {
        return x0;
    }
    let x1 = x_at(layout, i1);
    x0 + (x1 - x0) * frac
}

/// 单个码元位置的 x 坐标。
fn x_at(layout: &IDWriteTextLayout, text_pos: u32) -> f32 {
    let mut x = 0.0f32;
    let mut y = 0.0f32;
    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
    // SAFETY: 三个出参都是本地栈变量；text_pos 由调用方钳在 [0, len] 内。
    let ok = unsafe {
        layout.HitTestTextPosition(text_pos, false, &mut x, &mut y, &mut metrics)
    };
    if ok.is_err() || !x.is_finite() {
        return 0.0;
    }
    x
}

/// 文本布局的自然宽度（`pub(crate)`：跑马灯需要歌词行的自然宽判断是否超宽）。
pub(crate) fn metrics_width(layout: &IDWriteTextLayout) -> f32 {
    let mut m = DWRITE_TEXT_METRICS::default();
    // SAFETY: m 是本地栈结构体，GetMetrics 只写出它。
    // 返回 Result：失败时 m 保持全 0，下方 is_finite 判断兜底为 0.0。
    let _ = unsafe { layout.GetMetrics(&mut m) };
    if m.widthIncludingTrailingWhitespace.is_finite() {
        m.widthIncludingTrailingWhitespace
    } else {
        m.width
    }
}

fn create(
    dwrite: &IDWriteFactory,
    size: f32,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    align: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
    paragraph: windows::Win32::Graphics::DirectWrite::DWRITE_PARAGRAPH_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    // SAFETY: 字体族与 locale 都是静态 NUL 结尾宽字符串；fontcollection 传 None 表示
    // 用系统字体集合；size 为正有限值。
    let format = unsafe {
        dwrite.CreateTextFormat(
            FONT_FAMILY,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            LOCALE,
        )?
    };
    // SAFETY: 三个 setter 都是对刚创建的 format 调用，枚举值合法。
    unsafe {
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetTextAlignment(align)?;
        format.SetParagraphAlignment(paragraph)?;
    }
    Ok(format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_family_is_not_null() {
        assert!(!FONT_FAMILY.is_null());
        assert!(!LOCALE.is_null());
    }

    #[test]
    fn ellipsis_char_is_single_glyph() {
        assert_eq!(ELLIPSIS.chars().count(), 1);
    }
}
