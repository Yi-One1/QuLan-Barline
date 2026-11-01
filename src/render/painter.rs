//! 绘制（`ARCHITECTURE.md` §11.1 渲染管线的第 3–12 步）。
//!
//! 绘制顺序**不能变**：
//! ```text
//! 1. BeginDraw
//! 2. Clear(全透明)                 ← 不 clear 会残留上一帧（风险 R3）
//! 3. 命中区占位 alpha = 1           ← 视觉不可见但整块可点（§10.7）
//! 4. 悬停背景（模糊位图 / 半透明纯色，alpha = hover_alpha）
//! 5. 封面（真实位图 or 渐变占位 + 音符）
//! 6. 歌名 / 歌手（读 WidgetLayout 里缓存的截断结果）
//! 7. 三个按钮（含 disabled / pressed 态）
//! 8. 进度条（静止 2px / 悬停 4px / 拖动时显示气泡）
//! 9. 歌词占位（M3 换成真歌词与扫光）
//! 10. HUD 浮层
//! 11. EndDraw
//! ```
//!
//! ## 命中区技巧（§10.7，必须保留）
//!
//! `WS_EX_LAYERED` + `UpdateLayeredWindow` 的窗口，**鼠标命中与 alpha 绑定**：
//! alpha == 0 的像素让鼠标消息穿透。先在整个客户区填一个 `alpha = 1` 的矩形
//! （人眼阈值约 alpha = 2，所以视觉上完全不可见），整块区域就可接收鼠标，
//! 而圆角外的像素仍保持「透明」观感。**不要**改成 alpha = 0。
//!
//! ## 静止态真的完全透明（PRD §4.2）
//!
//! M1 常驻画一层 `CAPSULE_BG`（alpha 0xC8），M2 改成「只有悬停才画背景」。
//! 静止态只剩封面 / 文字 / 按钮 / 进度条，背景像素与壁纸完全一致。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::{
    ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_CLIP,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, DWRITE_MEASURING_MODE_NATURAL,
};

use crate::app::{AppState, HoverState};
use crate::config::defaults::limits;
use crate::i18n;
use crate::lyric::LyricDraw;
use crate::media::PlaybackState;
use crate::platform::wide::to_utf16;
use crate::render::cover_cache::CoverCache;
use crate::render::d2d::{d2d_rect, ellipse, rounded, Frame, TRANSPARENT};
use crate::render::layout::{RectF, WidgetLayout};
use crate::render::text::{TextFormats, TextKind};
use crate::render::theme::{Rgba, Theme};
use crate::ui::hit::HitTarget;

/// 一帧的绘制上下文。
///
/// 为什么要有它：`paint` 一族函数原先各自吃 6–9 个参数，其中
/// `text / layout / state / theme / lang / scale` 是**每一层都要用**的同一份东西。
/// 聚合之后每个绘制函数只剩「本层特有的参数」（矩形、颜色、文本），
/// 签名反而更好读（也自然满足 clippy 的 `too_many_arguments`）。
pub struct Ctx<'a> {
    pub text: &'a TextFormats,
    pub layout: &'a WidgetLayout,
    pub state: &'a AppState,
    /// 主题按值存（`Theme` 是 `Copy` 的小结构体，比多传一层引用清爽）
    pub theme: Theme,
    pub lang: i18n::Lang,
    pub scale: f32,
}

/// 一次单行文本绘制的参数（`draw_line` 的入参）。
struct TextDraw<'a> {
    kind: TextKind,
    text: &'a str,
    rect: crate::render::layout::RectF,
    color: Rgba,
    outline: bool,
}

/// 一个药丸浮层（音量 HUD / 拖动时间气泡）的参数。
struct Pill<'a> {
    label: &'a str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// 把一帧画出来。调用方负责 `upload` + `UpdateLayeredWindow`。
///
/// `slot` 是当前帧的缓冲槽位（0 = A / 1 = B），封面位图按槽位分桶缓存
/// （见 [`crate::render::cover_cache`]）。
///
/// `lyric` 是本帧的歌词内容（纯数据），`lyric_layout` 是当前行的
/// `IDWriteTextLayout` + 码元数——扫光需要它把「第 N 个码元」换成 x 坐标。
pub fn paint(
    frame: &mut Frame,
    covers: &mut CoverCache,
    slot: u8,
    ctx: &Ctx<'_>,
    lyric: &LyricDraw<'_>,
    lyric_layout: Option<(&IDWriteTextLayout, u32)>,
) -> Result<()> {
    let w = frame.width as f32;
    let h = frame.height as f32;
    let hover_a = ctx.state.hover_alpha.clamp(0.0, 1.0);

    let rt = frame.rt.clone();

    // SAFETY: 以下全部是对本帧独占的 ID2D1RenderTarget 的调用；
    // 所有矩形 / 颜色都是栈上值，生命周期覆盖调用；BeginDraw 与 EndDraw 严格配对。
    unsafe {
        rt.BeginDraw();
        rt.Clear(Some(&TRANSPARENT));

        // ---- 1. 命中区占位：alpha = 1，视觉不可见但整块可点（§10.7）----
        let hit_brush = frame.brush(Rgba::new(0, 0, 0, 1))?;
        rt.FillRectangle(&d2d_rect(0.0, 0.0, w, h), &hit_brush);

        // ---- 2. 悬停背景 ----
        if hover_a > 0.004 {
            draw_background(&rt, frame, ctx, hover_a)?;
        }

        // ---- 3. 封面 ----
        draw_cover(&rt, frame, covers, slot, ctx)?;

        // ---- 4. 文字 ----
        draw_texts(&rt, frame, ctx)?;

        // ---- 5. 按钮 ----
        draw_buttons(&rt, frame, ctx)?;

        // ---- 6. 进度条 / 音量 HUD ----
        draw_progress(&rt, frame, ctx)?;

        // ---- 7. 歌词（逐字扫光）----
        draw_lyrics(&rt, frame, ctx, lyric, lyric_layout)?;

        // ---- 8. Seeking 时间气泡 ----
        draw_seek_bubble(&rt, frame, ctx)?;

        rt.EndDraw(None, None)?;
    }

    Ok(())
}

// ------------------------------------------------------------------ 背景

/// 悬停背景：优先画模糊位图，退化为半透明纯色。
fn draw_background(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    alpha: f32,
) -> Result<()> {
    let (state, theme, s) = (ctx.state, &ctx.theme, ctx.scale);
    let w = frame.width as f32;
    let h = frame.height as f32;
    let radius = (8.0 * s).min(h * 0.5);

    if theme.use_blur {
        // 只接受「代次与尺寸都对得上」的缓冲（§11.3：geometry_epoch 不匹配直接丢弃）
        let usable = state.blur.as_ref().filter(|b| {
            b.geometry_epoch == state.geometry_epoch
                && b.width == frame.width
                && b.height == frame.height
        });

        if let Some(buf) = usable {
            let key = (buf.geometry_epoch, std::sync::Arc::as_ptr(buf) as usize);
            let need_new = match &frame.blur {
                Some((e, p, _)) => (*e, *p) != key,
                None => true,
            };
            if need_new {
                // SAFETY: 缓冲由采样线程按 (width, height) 生成，长度自洽。
                match frame.bitmap_from_bgra(&buf.bgra, buf.width, buf.height) {
                    Ok(bmp) => frame.blur = Some((key.0, key.1, bmp)),
                    Err(e) => {
                        crate::log_warn!("模糊背景位图创建失败：{e}");
                        frame.blur = None;
                    }
                }
            }
            if let Some((_, _, bmp)) = frame.blur.as_ref() {
                // SAFETY: bmp 属于本帧的渲染目标；destination 覆盖整块客户区。
                unsafe {
                    rt.DrawBitmap(
                        bmp,
                        Some(&d2d_rect(0.0, 0.0, w, h)),
                        alpha,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
                return Ok(());
            }
        }
    }

    // 半透明纯色（或模糊尚未就绪时的兜底，避免悬停时闪一下全透）
    let bg = theme.hover_bg.with_alpha_scale(alpha);
    let brush = frame.brush(bg)?;
    // SAFETY: 栈上圆角矩形；brush 由本帧持有。
    unsafe {
        rt.FillRoundedRectangle(&rounded(0.0, 0.0, w, h, radius), &brush);
    }
    Ok(())
}

// ------------------------------------------------------------------ 封面

/// 画封面：真实位图优先，缺失时渐变占位 + 音符图标（PRD P0-03：不出现白块 / 黑块）。
fn draw_cover(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    covers: &mut CoverCache,
    slot: u8,
    ctx: &Ctx<'_>,
) -> Result<()> {
    let (layout, state, theme, s) = (ctx.layout, ctx.state, &ctx.theme, ctx.scale);
    let c = layout.cover;
    if c.is_empty() {
        return Ok(());
    }
    let radius = limits::COVER_RADIUS * s;

    if let Some(cover) = state.playing.cover.as_ref() {
        let rt_clone = frame.rt.clone();
        let wic = frame.wic.clone();
        if let Some(bmp) = covers.get(&rt_clone, &wic, slot, state.playing.track_id, cover) {
            // 封面在解码阶段就做好的圆角遮罩（见 media::cover），这里直接贴。
            // SAFETY: bmp 属于本帧目标；目标矩形来自布局。
            unsafe {
                rt.DrawBitmap(
                    &bmp,
                    Some(&d2d_rect(c.x, c.y, c.w, c.h)),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
            return Ok(());
        }
    }

    // 渐变占位：强调色 → 暗色
    let grad = frame.gradient_base(Rgba::from_rgb(theme.accent, 0xFF))?;
    // SAFETY: 栈上圆角矩形；grad 由本帧持有。
    unsafe {
        rt.FillRoundedRectangle(&rounded(c.x, c.y, c.w, c.h, radius), &grad);
    }

    // 音符图标（不引入图标字体 / PNG）：符头椭圆 + 符干 + 符尾
    let fg = frame.brush(Rgba::new(0xFF, 0xFF, 0xFF, 0xC8))?;
    let (cx, cy) = c.center();
    let unit = c.w / 32.0;
    // SAFETY: 全部是几何图元，坐标由布局推导，恒为有限值。
    unsafe {
        rt.FillEllipse(&ellipse(cx - 4.0 * unit, cy + 5.0 * unit, 4.2 * unit, 3.2 * unit), &fg);
        rt.FillRectangle(
            &d2d_rect(cx - 0.4 * unit, cy - 9.0 * unit, 1.6 * unit, 14.0 * unit),
            &fg,
        );
        rt.FillRectangle(
            &d2d_rect(cx + 1.0 * unit, cy - 9.0 * unit, 5.0 * unit, 1.5 * unit),
            &fg,
        );
        rt.FillRectangle(
            &d2d_rect(cx + 4.4 * unit, cy - 9.0 * unit, 1.6 * unit, 4.0 * unit),
            &fg,
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ 文字

fn draw_texts(rt: &ID2D1RenderTarget, frame: &mut Frame, ctx: &Ctx<'_>) -> Result<()> {
    let (layout, state, theme, lang) = (ctx.layout, ctx.state, &ctx.theme, ctx.lang);
    // 歌名：优先用布局里缓存的截断结果（M1 报告 §5.9 的技术债解法）
    let title_src: &str = if layout.text_key != 0 {
        layout.title_text.as_str()
    } else {
        state
            .playing
            .title_or(i18n::t(lang, "state.unknown_track"))
    };
    let artist_src: &str = if layout.text_key != 0 {
        layout.artist_text.as_str()
    } else {
        state.playing.artist_or(i18n::t(lang, "state.unknown_artist"))
    };

    let main = Rgba::from_rgb(theme.text, 0xFF);
    // 歌手名：不透明度由主题按背景算过（≥ 4.5:1，下限 78%），不是写死的 60%
    let dim = Rgba::from_rgb(theme.text_dim, theme.dim_alpha);

    draw_line(
        rt,
        frame,
        ctx,
        &TextDraw {
            kind: TextKind::Title,
            text: title_src,
            rect: layout.title,
            color: main,
            outline: theme.needs_outline,
        },
    )?;
    draw_line(
        rt,
        frame,
        ctx,
        &TextDraw {
            kind: TextKind::Artist,
            text: artist_src,
            rect: layout.artist,
            color: dim,
            outline: false,
        },
    )?;
    Ok(())
}

/// 画一行文本。`outline == true` 时先画一圈反色描边（中灰壁纸的兜底，§11.2 第 6 步）。
fn draw_line(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    d: &TextDraw<'_>,
) -> Result<()> {
    let text = ctx.text;
    let (kind, s, rect, color, outline) = (d.kind, d.text, d.rect, d.color, d.outline);
    if s.is_empty() || rect.is_empty() {
        return Ok(());
    }
    let utf16 = to_utf16(s);
    if utf16.is_empty() {
        return Ok(());
    }
    let rect_d = d2d_rect(rect.x, rect.y, rect.w, rect.h);
    let format = text.get(kind);

    if outline {
        let inverse = if color.r > 0x80 {
            Rgba::new(0x00, 0x00, 0x00, 0xB0)
        } else {
            Rgba::new(0xFF, 0xFF, 0xFF, 0xB0)
        };
        let ob = frame.brush(inverse)?;
        for (dx, dy) in [(-1.0f32, 0.0f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            let shifted = d2d_rect(rect.x + dx, rect.y + dy, rect.w, rect.h);
            // SAFETY: 栈上矩形；ob 由本帧持有；utf16 生命周期覆盖调用。
            unsafe {
                rt.DrawText(
                    &utf16,
                    format,
                    &shifted,
                    &ob,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
    }

    let brush = frame.brush(color)?;
    // SAFETY: 同上。
    unsafe {
        rt.DrawText(
            &utf16,
            format,
            &rect_d,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ 按钮

fn draw_buttons(rt: &ID2D1RenderTarget, frame: &mut Frame, ctx: &Ctx<'_>) -> Result<()> {
    let (layout, state, theme, s) = (ctx.layout, ctx.state, &ctx.theme, ctx.scale);
    let playing = state.effective_state() == PlaybackState::Playing;
    let fallback = state.effective_state() == PlaybackState::Fallback;

    let enabled = Rgba::from_rgb(theme.text, 0xFF);
    let disabled = Rgba::from_rgb(theme.text, 0x55);

    let items = [
        (HitTarget::Prev, layout.btn_prev, state.playing.can_prev && !fallback),
        (HitTarget::Toggle, layout.btn_toggle, !fallback),
        (HitTarget::Next, layout.btn_next, state.playing.can_next && !fallback),
    ];

    for (target, rect, on) in items {
        let color = if on { enabled } else { disabled };
        let brush = frame.brush(color)?;
        // 按下态：图标整体下沉 1px，给出即时反馈
        let sink = if state.pressed == Some(target) { 1.0 * s } else { 0.0 };
        let (cx, cy) = rect.center();
        let icon = limits::BTN_ICON * s;
        let x = cx - icon * 0.5;
        let y = cy - icon * 0.5 + sink;

        // SAFETY: 全部是几何图元；brush 由本帧持有。
        unsafe {
            match target {
                HitTarget::Prev => {
                    // 竖条 + 左向三角
                    rt.FillRectangle(&d2d_rect(x + 1.0, y + 1.5, 2.2, icon - 3.0), &brush);
                    fill_triangle(rt, &brush, x + 4.2, y + 1.5, icon - 5.6, icon - 3.0, false);
                }
                HitTarget::Next => {
                    fill_triangle(rt, &brush, x + 1.4, y + 1.5, icon - 5.6, icon - 3.0, true);
                    rt.FillRectangle(
                        &d2d_rect(x + icon - 3.2, y + 1.5, 2.2, icon - 3.0),
                        &brush,
                    );
                }
                _ => {
                    if playing {
                        // 暂停：两根竖条
                        rt.FillRectangle(&d2d_rect(x + 4.0, y + 1.5, 3.0, icon - 3.0), &brush);
                        rt.FillRectangle(
                            &d2d_rect(x + 9.0, y + 1.5, 3.0, icon - 3.0),
                            &brush,
                        );
                    } else {
                        // 播放：右向三角
                        fill_triangle(rt, &brush, x + 4.6, y + 1.5, icon - 6.0, icon - 3.0, true);
                    }
                }
            }
        }
    }
    Ok(())
}

/// 用逐行矩形填一个三角形（避免依赖 `ID2D1PathGeometry` 的接口继承链）。
///
/// 16px 图标只有 ~13 行，成本可以忽略；行间多画 0.6px 消除抗锯齿接缝。
fn fill_triangle(
    rt: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    pointing_right: bool,
) {
    let rows = h.round().max(1.0) as i32;
    let step = h / rows as f32;
    for i in 0..rows {
        let t = (i as f32 + 0.5) / rows as f32;
        let ww = w * (1.0 - (2.0 * t - 1.0).abs());
        if ww <= 0.15 {
            continue;
        }
        let xx = if pointing_right { x } else { x + (w - ww) };
        // SAFETY: 栈上矩形；坐标全部有限。
        unsafe {
            rt.FillRectangle(&d2d_rect(xx, y + i as f32 * step, ww, step + 0.6), brush);
        }
    }
}

// ------------------------------------------------------------------ 进度条 / HUD

fn draw_progress(rt: &ID2D1RenderTarget, frame: &mut Frame, ctx: &Ctx<'_>) -> Result<()> {
    let (layout, state, theme, lang, s) = (ctx.layout, ctx.state, &ctx.theme, ctx.lang, ctx.scale);
    let duration = state.playing.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let hud = state.hover == HoverState::VolumeHud;
    let seeking = state.hover == HoverState::Seeking;

    // duration == 0（播放器未上报 timeline）→ 隐藏进度条（风险 R9）
    if duration <= 0 && !hud {
        return Ok(());
    }

    let hovering = state.hover != HoverState::Idle;
    let ph = layout.progress_height(hovering);
    let pr = layout.progress_rect(ph);
    let radius = ph * 0.5;

    let track = frame.brush(theme.track_bg)?;
    // SAFETY: 栈上圆角矩形；brush 由本帧持有。
    unsafe {
        rt.FillRoundedRectangle(&rounded(pr.x, pr.y, pr.w, ph, radius), &track);
    }

    let frac = if hud {
        state.volume_hud.clamp(0.0, 1.0)
    } else if seeking {
        if duration > 0 {
            (state.seek_preview_ms as f32 / duration as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    } else if duration > 0 {
        (state.playing.position_now() as f32 / duration as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let accent = frame.brush(Rgba::from_rgb(theme.accent, 0xFF))?;
    let fill_w = pr.w * frac;
    if fill_w > 0.5 {
        // SAFETY: 同上。
        unsafe {
            rt.FillRoundedRectangle(&rounded(pr.x, pr.y, fill_w, ph, radius), &accent);
        }
    }

    // 悬停时在填充末端画一个圆点，指示可拖动
    if hovering && !hud && duration > 0 {
        let knob_x = pr.x + fill_w;
        let r = (3.0 * s).max(2.0);
        // SAFETY: 同上。
        unsafe {
            rt.FillEllipse(&ellipse(knob_x, pr.y + ph * 0.5, r, r), &accent);
        }
    }

    // 音量 HUD：右上角一个小药丸显示百分比
    if hud {
        let label = format_hud_volume(lang, state.volume_hud);
        let pill_w = 74.0 * s;
        let pill_h = 16.0 * s;
        let x = (layout.width as f32 - pill_w - limits::CONTENT_PAD * s).max(0.0);
        let y = (pr.y - pill_h - 1.0 * s).max(0.0);
        draw_pill(
            rt,
            frame,
            ctx,
            &Pill {
                label: &label,
                x,
                y,
                w: pill_w,
                h: pill_h,
            },
        )?;
    }

    Ok(())
}

/// 音量 HUD 文案（`音量 42%` / `Volume 42%`）。
fn format_hud_volume(lang: i18n::Lang, v: f32) -> String {
    let pct = (v.clamp(0.0, 1.0) * 100.0).round() as i32;
    let mut out = String::with_capacity(16);
    out.push_str(i18n::t(lang, "hud.volume"));
    out.push(' ');
    out.push_str(&pct.to_string());
    out.push('%');
    out
}

/// 画一个圆角药丸 + 居中文本。
fn draw_pill(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    p: &Pill<'_>,
) -> Result<()> {
    let text = ctx.text;
    let (label, x, y, w, h) = (p.label, p.x, p.y, p.w, p.h);
    let bg = frame.brush(Rgba::new(0x14, 0x14, 0x18, 0xE6))?;
    // SAFETY: 栈上圆角矩形。
    unsafe {
        rt.FillRoundedRectangle(&rounded(x, y, w, h, h * 0.5), &bg);
    }
    let utf16 = to_utf16(label);
    if utf16.is_empty() {
        return Ok(());
    }
    let fg = frame.brush(Rgba::new(0xFF, 0xFF, 0xFF, 0xF0))?;
    let r = d2d_rect(x, y, w, h);
    // SAFETY: utf16 生命周期覆盖调用；hud_center 是居中对齐的格式。
    unsafe {
        rt.DrawText(
            &utf16,
            &text.hud_center,
            &r,
            &fg,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}

// ------------------------------------------------------------------ 歌词

/// 歌词右侧渐隐带宽度（逻辑像素；随 DPI 缩放）。
///
/// 文本超宽时，可用宽度最后这一段从实色渐到全透明（M3 遗留 P2），
/// 取代旧的「右缘硬裁」。不超宽时该带落在文本之外，视觉零差异。
const LYRIC_FADE_W: f32 = 24.0;

/// 计算一行歌词的渐隐带（渲染目标坐标）。
///
/// 返回 `(fade_start, fade_end)`：`fade_end` = 行右缘；可用宽度不足
/// `LYRIC_FADE_W` 时起点钳到行首（整行都在渐变里，不会出现负起点）。
fn fade_band(x: f32, w: f32, fade_w: f32) -> (f32, f32) {
    let end = x + w;
    let start = (end - fade_w).max(x);
    (start, end)
}

/// 画一行歌词（**右侧渐隐版**，M3 遗留 P2）。
///
/// 与 [`draw_line`] 的唯一区别：文本笔刷是水平透明度渐变——行宽最后
/// `LYRIC_FADE_W` 像素从实色渐到全透明。文本不超宽时整行落在渐变起点
/// 之前（CLAMP 延展 = stop0 实色），视觉与实色笔刷一致，因此**无需先测量
/// 是否超宽**，无差别使用即可（省一次测量，保住空闲 CPU 指标）。
///
/// 扫光的码元 → x 映射不受影响：文本仍然整行进 `IDWriteTextLayout`，
/// 裁剪宽度仍由 `hit_test_x` 给出，渐隐只是绘制层面的透明度调制。
fn draw_lyric_line(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    text: &TextFormats,
    s: f32,
    d: &TextDraw<'_>,
) -> Result<()> {
    let rect = d.rect;
    if d.text.is_empty() || rect.is_empty() {
        return Ok(());
    }
    let utf16 = to_utf16(d.text);
    if utf16.is_empty() {
        return Ok(());
    }
    let (fade_start, fade_end) = fade_band(rect.x, rect.w, LYRIC_FADE_W * s);
    let brush = frame.fade_brush(d.color, fade_start, fade_end)?;
    let rect_d = d2d_rect(rect.x, rect.y, rect.w, rect.h);
    let format = text.get(d.kind);
    // SAFETY: utf16 生命周期覆盖调用；brush / format 由 frame / text 持有；rect 是栈上值。
    unsafe {
        rt.DrawText(
            &utf16,
            format,
            &rect_d,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}

/// 画歌词区（T12）：当前行**逐字扫光**，下一行 30% 不透明度。
///
/// 扫光用「两遍绘制 + 裁剪」实现，而不是逐字画不同颜色：
/// 1. 整行先按**未唱**颜色画一遍；
/// 2. `PushAxisAlignedClip` 裁到 `[行首, 扫光边界]`，再用**已唱**颜色画同一行；
/// 3. `PopAxisAlignedClip`。
///
/// 为什么不用 `IDWriteTextLayout::SetDrawingEffect` 分段上色：那需要
/// `IDWriteTextLayout1` + `IDWriteTextRenderer` 自定义渲染器（几百行 COM），
/// 而且每帧都要重设 effect。裁剪法只要一次 `HitTestTextPosition`，成本低一个量级。
///
/// 长行不再硬裁：两遍绘制都走 [`draw_lyric_line`]（右侧渐隐），文本整行
/// 进布局，扫光码元映射保持正确。
fn draw_lyrics(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    lyric: &LyricDraw<'_>,
    lyric_layout: Option<(&IDWriteTextLayout, u32)>,
) -> Result<()> {
    let (layout, state, theme) = (ctx.layout, ctx.state, &ctx.theme);
    if layout.form == crate::render::layout::WidgetForm::Compact || lyric.is_empty() {
        return Ok(());
    }

    // ---- 切歌瞬间：旧歌词淡出 + 上移（M3 遗留 P2）----
    // 淡出在前 120ms 内完成（保持原 PRD 观感），上移走满 150ms（ease-out cubic，
    // 与高度过渡同一族曲线）。这期间不画新内容。
    if let Some((old_cur, old_next, started)) = &state.lyric_fade {
        let t = started.elapsed().as_secs_f32();
        if t < crate::app::LYRIC_FADE_SECS {
            let a = ((1.0 - t / crate::app::LYRIC_FADE_OUT_SECS) * 255.0).clamp(0.0, 255.0) as u8;
            let p = (t / crate::app::LYRIC_FADE_SECS).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - p).powi(3);
            let dy = -eased * crate::app::LYRIC_SLIDE_PX * ctx.scale;
            let shift = |r: &crate::render::layout::RectF| {
                RectF::new(r.x, r.y + dy, r.w, r.h)
            };
            if !old_cur.is_empty() {
                draw_line(
                    rt,
                    frame,
                    ctx,
                    &TextDraw {
                        kind: TextKind::Lyric,
                        text: old_cur,
                        rect: shift(&layout.lyric_cur),
                        color: Rgba::from_rgb(theme.text, a),
                        outline: false,
                    },
                )?;
            }
            if !old_next.is_empty() && !layout.lyric_next.is_empty() {
                draw_line(
                    rt,
                    frame,
                    ctx,
                    &TextDraw {
                        kind: TextKind::Lyric,
                        text: old_next,
                        rect: shift(&layout.lyric_next),
                        color: Rgba::from_rgb(theme.text_dim, ((a as f32) * 0.6) as u8),
                        outline: false,
                    },
                )?;
            }
            return Ok(());
        }
    }

    // ---- 占位文案（加载中 / 暂无歌词）：淡色、无扫光 ----
    if lyric.placeholder {
        return draw_line(
            rt,
            frame,
            ctx,
            &TextDraw {
                kind: TextKind::Lyric,
                text: lyric.cur,
                rect: layout.lyric_cur,
                color: Rgba::from_rgb(theme.text_dim, 0xB0),
                outline: false,
            },
        );
    }

    // ---- 下一行（先画，压在下面也无所谓，两行不重叠）----
    if !lyric.next.is_empty() && !layout.lyric_next.is_empty() {
        draw_lyric_line(
            rt,
            frame,
            ctx.text,
            ctx.scale,
            &TextDraw {
                kind: TextKind::Lyric,
                text: lyric.next,
                rect: layout.lyric_next,
                color: Rgba::from_rgb(theme.text_dim, Theme::DIM_ALPHA),
                outline: false,
            },
        )?;
    }

    if lyric.cur.is_empty() || layout.lyric_cur.is_empty() {
        return Ok(());
    }

    // ---- 当前行：未唱部分 ----
    let unsung = Rgba::from_rgb(theme.text_dim, 0x8C);
    draw_lyric_line(
        rt,
        frame,
        ctx.text,
        ctx.scale,
        &TextDraw {
            kind: TextKind::Lyric,
            text: lyric.cur,
            rect: layout.lyric_cur,
            color: unsung,
            outline: false,
        },
    )?;

    // ---- 当前行：已唱部分（裁剪后重画）----
    let Some((text_layout, utf16_len)) = lyric_layout else {
        // 没有布局（理论上不会发生）→ 整行按已唱色画，至少不会看着像"没歌词"
        return draw_line(
            rt,
            frame,
            ctx,
            &TextDraw {
                kind: TextKind::Lyric,
                text: lyric.cur,
                rect: layout.lyric_cur,
                color: Rgba::from_rgb(theme.text, 0xFF),
                outline: false,
            },
        );
    };
    if lyric.sweep <= 0.01 {
        return Ok(());
    }
    let boundary = crate::render::text::hit_test_x(text_layout, utf16_len, lyric.sweep);
    if boundary <= 0.5 {
        return Ok(());
    }
    let clip = d2d_rect(
        layout.lyric_cur.x,
        layout.lyric_cur.y,
        boundary.min(layout.lyric_cur.w),
        layout.lyric_cur.h,
    );
    // SAFETY: clip 是栈上矩形；Push/Pop 严格配对（中间只有一次 DrawText，
    // 且 `?` 提前返回前已保证没有未配对的 Push——这里 draw_lyric_line 失败会
    // 跳过 Pop，所以先用 let 接住结果再 Pop）。
    unsafe {
        rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    let r = draw_lyric_line(
        rt,
        frame,
        ctx.text,
        ctx.scale,
        &TextDraw {
            kind: TextKind::Lyric,
            text: lyric.cur,
            rect: layout.lyric_cur,
            color: Rgba::from_rgb(theme.text, 0xFF),
            outline: false,
        },
    );
    // SAFETY: 与上面的 PushAxisAlignedClip 配对；无论 draw_line 成功与否都要 Pop。
    unsafe {
        rt.PopAxisAlignedClip();
    }
    r
}

// ------------------------------------------------------------------ Seeking 气泡

fn draw_seek_bubble(rt: &ID2D1RenderTarget, frame: &mut Frame, ctx: &Ctx<'_>) -> Result<()> {
    let (layout, state, s) = (ctx.layout, ctx.state, ctx.scale);
    if state.hover != HoverState::Seeking {
        return Ok(());
    }
    let duration = state.playing.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
    let label = format_time_pair(state.seek_preview_ms, duration);

    let pill_w = 104.0 * s;
    let pill_h = 18.0 * s;
    // 跟随拖动位置，并夹在客户区内
    let frac = if duration > 0 {
        (state.seek_preview_ms as f32 / duration as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let want_x = layout.progress.x + layout.progress.w * frac - pill_w * 0.5;
    let x = want_x
        .max(2.0 * s)
        .min((layout.width as f32 - pill_w - 2.0 * s).max(2.0 * s));
    // 气泡贴在主行顶部（底部任务栏时主行不在客户区顶部，不能写死 0）
    let y = layout.main_row.y + 1.0 * s;
    draw_pill(
        rt,
        frame,
        ctx,
        &Pill {
            label: &label,
            x,
            y,
            w: pill_w,
            h: pill_h,
        },
    )
}

/// `01:12 / 03:45`。**不分配 String 之外的任何东西**，且只在 Seeking 态调用。
fn format_time_pair(pos: i64, dur: i64) -> String {
    let mut out = String::with_capacity(20);
    push_mmss(&mut out, pos);
    out.push_str(" / ");
    push_mmss(&mut out, dur);
    out
}

fn push_mmss(out: &mut String, ms: i64) {
    let total = (ms.max(0) / 1000) as u32;
    let m = total / 60;
    let sec = total % 60;
    if m >= 100 {
        out.push_str(&m.to_string());
    } else {
        if m < 10 {
            out.push('0');
        }
        out.push_str(&m.to_string());
    }
    out.push(':');
    if sec < 10 {
        out.push('0');
    }
    out.push_str(&sec.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_band_stays_inside_rect_and_clamps() {
        // 正常情况：渐隐带 = 右缘前 24px
        let (s, e) = fade_band(100.0, 200.0, 24.0);
        assert_eq!(s, 276.0);
        assert_eq!(e, 300.0);
        // 可用宽度比渐隐带还窄：起点钳到行首，整行都在渐变里
        let (s, e) = fade_band(100.0, 10.0, 24.0);
        assert_eq!(s, 100.0);
        assert_eq!(e, 110.0);
        // 零宽：起终点重合（DrawText 侧有 rect.is_empty 兜底，不会真画）
        let (s, e) = fade_band(50.0, 0.0, 24.0);
        assert_eq!(s, 50.0);
        assert_eq!(e, 50.0);
    }

    #[test]
    fn time_pair_formats_with_leading_zeros() {
        assert_eq!(format_time_pair(72_000, 225_000), "01:12 / 03:45");
        assert_eq!(format_time_pair(0, 0), "00:00 / 00:00");
        assert_eq!(format_time_pair(-500, 5_000), "00:00 / 00:05");
        assert_eq!(format_time_pair(6_000_000, 7_200_000), "100:00 / 120:00");
    }

    #[test]
    fn volume_label_contains_percent() {
        let s = format_hud_volume(i18n::Lang::Zh, 0.42);
        assert!(s.ends_with("42%"), "s = {s}");
        let s = format_hud_volume(i18n::Lang::En, 1.0);
        assert!(s.ends_with("100%"), "s = {s}");
    }
}
