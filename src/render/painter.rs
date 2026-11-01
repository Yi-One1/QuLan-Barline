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
    ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_CLIP,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL;

use crate::app::{AppState, HoverState};
use crate::config::defaults::limits;
use crate::i18n;
use crate::media::PlaybackState;
use crate::platform::wide::to_utf16;
use crate::render::cover_cache::CoverCache;
use crate::render::d2d::{d2d_rect, ellipse, rounded, Frame, TRANSPARENT};
use crate::render::layout::WidgetLayout;
use crate::render::text::{TextFormats, TextKind};
use crate::render::theme::{Rgba, Theme};
use crate::ui::hit::HitTarget;

/// 把一帧画出来。调用方负责 `upload` + `UpdateLayeredWindow`。
///
/// `slot` 是当前帧的缓冲槽位（0 = A / 1 = B），封面位图按槽位分桶缓存
/// （见 [`crate::render::cover_cache`]）。
pub fn paint(
    text: &TextFormats,
    frame: &mut Frame,
    covers: &mut CoverCache,
    slot: u8,
    layout: &WidgetLayout,
    state: &AppState,
    lang: i18n::Lang,
) -> Result<()> {
    let w = frame.width as f32;
    let h = frame.height as f32;
    let s = layout.effective_scale();
    let theme = state.theme;
    let hover_a = state.hover_alpha.clamp(0.0, 1.0);

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
            draw_background(&rt, frame, state, &theme, hover_a, s)?;
        }

        // ---- 3. 封面 ----
        draw_cover(&rt, frame, covers, slot, layout, state, &theme, s)?;

        // ---- 4. 文字 ----
        draw_texts(&rt, frame, text, layout, state, &theme, lang, s)?;

        // ---- 5. 按钮 ----
        draw_buttons(&rt, frame, layout, state, &theme, s)?;

        // ---- 6. 进度条 / 音量 HUD ----
        draw_progress(&rt, frame, text, layout, state, &theme, lang, s)?;

        // ---- 7. 歌词占位（M3/T12 换成真歌词与扫光）----
        draw_lyric_placeholder(&rt, frame, text, layout, &theme, lang, s)?;

        // ---- 8. Seeking 时间气泡 ----
        draw_seek_bubble(&rt, frame, text, layout, state, &theme, s)?;

        rt.EndDraw(None, None)?;
    }

    Ok(())
}

// ------------------------------------------------------------------ 背景

/// 悬停背景：优先画模糊位图，退化为半透明纯色。
fn draw_background(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    state: &AppState,
    theme: &Theme,
    alpha: f32,
    s: f32,
) -> Result<()> {
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
    layout: &WidgetLayout,
    state: &AppState,
    theme: &Theme,
    s: f32,
) -> Result<()> {
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

fn draw_texts(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    text: &TextFormats,
    layout: &WidgetLayout,
    state: &AppState,
    theme: &Theme,
    lang: i18n::Lang,
    _s: f32,
) -> Result<()> {
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
    let dim = Rgba::from_rgb(theme.text_dim, Theme::DIM_ALPHA);

    draw_line(
        rt,
        frame,
        text,
        TextKind::Title,
        title_src,
        layout.title,
        main,
        theme.needs_outline,
    )?;
    draw_line(
        rt,
        frame,
        text,
        TextKind::Artist,
        artist_src,
        layout.artist,
        dim,
        false,
    )?;
    Ok(())
}

/// 画一行文本。`outline == true` 时先画一圈反色描边（中灰壁纸的兜底，§11.2 第 6 步）。
fn draw_line(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    text: &TextFormats,
    kind: TextKind,
    s: &str,
    rect: crate::render::layout::RectF,
    color: Rgba,
    outline: bool,
) -> Result<()> {
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

fn draw_buttons(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    layout: &WidgetLayout,
    state: &AppState,
    theme: &Theme,
    s: f32,
) -> Result<()> {
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

#[allow(clippy::too_many_arguments)]
fn draw_progress(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    text: &TextFormats,
    layout: &WidgetLayout,
    state: &AppState,
    theme: &Theme,
    lang: i18n::Lang,
    s: f32,
) -> Result<()> {
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
        draw_pill(rt, frame, text, &label, x, y, pill_w, pill_h, theme)?;
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
    text: &TextFormats,
    label: &str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    theme: &Theme,
) -> Result<()> {
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
    let _ = theme;
    Ok(())
}

// ------------------------------------------------------------------ 歌词占位

fn draw_lyric_placeholder(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    text: &TextFormats,
    layout: &WidgetLayout,
    theme: &Theme,
    lang: i18n::Lang,
    _s: f32,
) -> Result<()> {
    if layout.form == crate::render::layout::WidgetForm::Compact || layout.lyric_cur.is_empty() {
        return Ok(());
    }
    // M3/T12 会把这里换成真歌词 + 扫光；M2 只保证「高度按形态变化」这条验收成立。
    let label = i18n::t(lang, "lyric.missing");
    draw_line(
        rt,
        frame,
        text,
        TextKind::Lyric,
        label,
        layout.lyric_cur,
        Rgba::from_rgb(theme.text_dim, 0xB0),
        false,
    )
}

// ------------------------------------------------------------------ Seeking 气泡

fn draw_seek_bubble(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    text: &TextFormats,
    layout: &WidgetLayout,
    state: &AppState,
    theme: &Theme,
    s: f32,
) -> Result<()> {
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
    draw_pill(rt, frame, text, &label, x, y, pill_w, pill_h, theme)
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
