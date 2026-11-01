//! 渲染层门面（`ARCHITECTURE.md` §5 / §11.1）。
//!
//! 调用关系：`App` 计算布局 → `Renderer::prepare_text` 填缓存 → `Renderer::render(&AppState)`
//! → 返回一个「内容已经是 BGRA」的 `HDC` + 覆盖率图 → `TaskbarHost::present()` 做
//! `UpdateLayeredWindow`。
//!
//! 渲染层**只读** `AppState`，不持有任何业务状态（§3.3 表现层约束）。

pub mod blur;
pub mod cover_cache;
pub mod d2d;
pub mod layout;
pub mod painter;
pub mod sampler;
pub mod text;
pub mod theme;

use std::sync::Arc;

use windows::core::Result;
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;
use windows::Win32::Graphics::Gdi::HDC;

use crate::app::AppState;
use crate::lyric::LyricDraw;
use cover_cache::CoverCache;
use d2d::{D2d, HdcBuf, Targets};
use layout::WidgetLayout;
use sampler::CoverageMap;
use text::{TextFormats, TextKind};

/// 测量用的「无限宽」上限。取 4096 而不是 f32::MAX，避免 DirectWrite 内部溢出。
const MEASURE_LIMIT: f32 = 4096.0;

/// 渲染器。只在 UI 线程（T0）使用。
pub struct Renderer {
    d2d: D2d,
    text: TextFormats,
    targets: Option<Targets>,
    buf: Option<HdcBuf>,
    covers: CoverCache,
    /// 歌词当前行的 `IDWriteTextLayout` 缓存（扫光要用它把码元位置换成 x）
    lyric: Option<LyricLayout>,
}

/// 歌词行的布局缓存。键里带 `track_id` + 行号 + 文本长度 + 宽度，
/// 只要换行 / 换歌 / 改宽度就会重建。
struct LyricLayout {
    /// `(track_id, line_idx, 文本字节长度, 可用宽度 bits)`
    key: (u64, usize, usize, u32),
    layout: IDWriteTextLayout,
    /// 这行文本的 UTF-16 码元数（`hit_test_x` 需要上界）
    utf16_len: u32,
}

impl Renderer {
    /// 创建工厂与文本格式。此时还没有尺寸，渲染目标按需在 [`Self::ensure`] 里建。
    pub fn new(dpi: u32) -> Result<Self> {
        let d2d = D2d::new()?;
        let scale = crate::platform::dpi::scale_for(dpi);
        let text = TextFormats::new(&d2d.dwrite, scale)?;
        Ok(Self {
            d2d,
            text,
            targets: None,
            buf: None,
            covers: CoverCache::new(),
            lyric: None,
        })
    }

    /// 保证渲染目标与 DIB 的尺寸匹配 `(w, h, dpi)`。尺寸不变则什么都不做。
    ///
    /// 这是**唯一**会分配大块像素内存的地方，所以必须只在尺寸 / DPI 变化时触发。
    pub fn ensure(&mut self, w: i32, h: i32, dpi: u32) -> Result<()> {
        let uw = w.max(1) as u32;
        let uh = h.max(1) as u32;
        let scale = crate::platform::dpi::scale_for(dpi);
        let dpi_f = if dpi == 0 { 96.0 } else { dpi as f32 };

        let need_targets = match &self.targets {
            Some(t) => !t.matches(uw, uh, dpi_f),
            None => true,
        };
        if need_targets {
            self.targets = Some(Targets::new(&self.d2d, uw, uh, dpi_f)?);
            self.buf = Some(HdcBuf::new(uw, uh)?);
            // 尺寸 / DPI 变了 → 文本字号要跟着重算；封面位图归属旧目标 → 必须清空
            self.text = TextFormats::new(&self.d2d.dwrite, scale)?;
            self.covers.clear();
            crate::log_info!("渲染目标已重建：{uw}×{uh} @ {dpi_f}dpi（scale={scale:.3}）");
        } else if self.buf.is_none() {
            self.buf = Some(HdcBuf::new(uw, uh)?);
        }
        Ok(())
    }

    /// 测量文本自然宽度（用于宽度自适应）。只在布局变化时调用。
    pub fn measure(&self, kind: TextKind, s: &str) -> f32 {
        self.text.measure(&self.d2d.dwrite, kind, s, MEASURE_LIMIT)
    }

    /// 把「截断后的歌名 / 歌手」填进布局缓存。
    ///
    /// 指纹没变就直接返回——这是「空闲 CPU ≈ 0」的关键：滚动 60fps 时
    /// painter 只读字符串，一次 `IDWriteTextLayout` 都不建（修 M1 报告 §5.9）。
    pub fn prepare_text(&mut self, layout: &mut WidgetLayout, title: &str, artist: &str) {
        let key = text_key(title, artist, layout.title.w, layout.artist.w, layout.scale);
        if layout.text_key == key {
            return;
        }

        let title_out = self
            .text
            .ellipsis(&self.d2d.dwrite, TextKind::Title, title, layout.title.w);
        let artist_out = self
            .text
            .ellipsis(&self.d2d.dwrite, TextKind::Artist, artist, layout.artist.w);

        if title_out != layout.title_text || artist_out != layout.artist_text {
            crate::log_debug!(
                "文本截断更新：title='{}'（{} 字） artist='{}'（{} 字）",
                title_out,
                title_out.chars().count(),
                artist_out,
                artist_out.chars().count()
            );
        }
        layout.title_text = title_out;
        layout.artist_text = artist_out;
        layout.text_key = key;
    }

    /// 画一帧并把像素搬进 DIB，返回 `(HDC, 宽, 高, 覆盖率图)`。
    ///
    /// 覆盖率图交给采样线程做「自遮挡剔除」（§11.2，风险 R5）。
    pub fn render(
        &mut self,
        state: &AppState,
        lang: crate::i18n::Lang,
    ) -> Result<(HDC, i32, i32, Option<Arc<CoverageMap>>)> {
        // 歌词当前行的布局：只在「换行 / 换歌 / 改宽度」时重建（T12 性能要求）
        let draw = state.lyric_draw(lang);
        self.ensure_lyric_layout(&draw, state);

        let Self {
            text,
            targets,
            buf,
            covers,
            lyric,
            ..
        } = self;

        let Some(targets) = targets.as_mut() else {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_UNEXPECTED,
                "渲染目标尚未创建（先调用 ensure）",
            ));
        };
        let Some(buf) = buf.as_mut() else {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_UNEXPECTED,
                "呈现缓冲尚未创建",
            ));
        };

        // take_next 会翻转轮换标记，所以槽位要**先**算：next_is_b == false → 本轮画 B。
        let slot: u8 = if targets.next_is_b { 0 } else { 1 };
        let frame = targets.take_next();
        let ctx = painter::Ctx {
            text,
            layout: &state.layout,
            state,
            theme: state.theme,
            lang,
            scale: state.layout.effective_scale(),
        };
        painter::paint(
            frame,
            covers,
            slot,
            &ctx,
            &draw,
            lyric.as_ref().map(|l| (&l.layout, l.utf16_len)),
        )?;
        buf.upload(frame)?;

        Ok((buf.hdc(), buf.width() as i32, buf.height() as i32, buf.coverage()))
    }

    /// 保证歌词当前行的 `IDWriteTextLayout` 与内容匹配。
    fn ensure_lyric_layout(&mut self, draw: &LyricDraw<'_>, state: &AppState) {
        // 占位文案 / 空行不需要扫光布局
        if draw.placeholder || draw.cur.is_empty() {
            self.lyric = None;
            return;
        }
        let max_w = state.layout.lyric_cur.w.max(1.0);
        let key = (
            state.playing.track_id,
            draw.line.unwrap_or(usize::MAX),
            draw.cur.len(),
            max_w.to_bits(),
        );
        if self.lyric.as_ref().map(|l| l.key) == Some(key) {
            return;
        }
        let Some(layout) = self
            .text
            .layout(&self.d2d.dwrite, TextKind::Lyric, draw.cur, max_w)
        else {
            self.lyric = None;
            return;
        };
        self.lyric = Some(LyricLayout {
            key,
            layout,
            utf16_len: draw.cur.encode_utf16().count() as u32,
        });
    }

    /// 当前渲染目标尺寸（诊断用）。
    pub fn size(&self) -> Option<(u32, u32)> {
        self.targets.as_ref().map(|t| (t.width, t.height))
    }

    /// 歌词当前行的自然宽度（跑马灯判断是否超宽用；无缓存行返回 `None`）。
    pub fn lyric_text_width(&self) -> Option<f32> {
        self.lyric
            .as_ref()
            .map(|l| crate::render::text::metrics_width(&l.layout))
            .filter(|w| w.is_finite() && *w > 0.0)
    }

    /// 封面缓存统计（诊断用）。
    pub fn cover_stats(&self) -> (u64, u64, usize) {
        (self.covers.hits, self.covers.misses, self.covers.len())
    }
}

/// 文本缓存指纹：文本内容 + 可用宽度 + 缩放。
///
/// 用 FNV-1a 而不是 `DefaultHasher`：前者无随机种子、结果稳定，
/// 且不依赖 `std::collections::hash_map` 的 SipHash 实现细节。
fn text_key(title: &str, artist: &str, title_w: f32, artist_w: f32, scale: f32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(title.as_bytes());
    feed(b"|");
    feed(artist.as_bytes());
    feed(b"|");
    feed(&title_w.to_bits().to_le_bytes());
    feed(&artist_w.to_bits().to_le_bytes());
    feed(&scale.to_bits().to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_key_is_stable_and_sensitive() {
        let a = text_key("歌", "手", 100.0, 80.0, 1.0);
        let b = text_key("歌", "手", 100.0, 80.0, 1.0);
        let c = text_key("歌", "手", 101.0, 80.0, 1.0);
        let d = text_key("歌2", "手", 100.0, 80.0, 1.0);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn text_key_changes_with_scale() {
        assert_ne!(
            text_key("t", "a", 10.0, 10.0, 1.0),
            text_key("t", "a", 10.0, 10.0, 1.25)
        );
    }
}
