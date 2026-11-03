//! 绘制（`ARCHITECTURE.md` §11.1 渲染管线的第 3–12 步）。
//!
//! 绘制顺序**不能变**：
//! ```text
//! 1. BeginDraw
//! 2. Clear(全透明)                 ← 不 clear 会残留上一帧（风险 R3）
//! 3. 命中区占位 alpha = 1           ← 视觉不可见但整块可点（§10.7）
//! 4. 悬停背景（idle→hover 两态插值，v1.1.12 起常规态也可半透明）
//! 5. 封面（真实位图 or 渐变占位 + 音符）
//! 6. 歌名 / 歌手（读 WidgetLayout 里缓存的截断结果）
//! 7. 三个按钮（含 disabled / pressed 态）
//! 7.5 右上角关闭 ×（v1.5.0 / G3；v1.5.1 悬停淡底 / 按下加深并下沉 1px）
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

        // ---- 2. 背景（常规 idle_bg / 悬停 hover_bg 插值）----
        // v1.1.12：常规态也可能半透明——只有「不悬停 且 常规透明」才整块跳过
        // （此前守卫只看 hover_a，把常规半透明整帧吞掉 = 问题 1/2 的根因）
        if hover_a > 0.004 || ctx.theme.idle_bg.a > 0 {
            draw_background(&rt, frame, ctx, hover_a)?;
        }

        // ---- 3. 封面 ----
        draw_cover(&rt, frame, covers, slot, ctx)?;

        // ---- 4. 文字 ----
        draw_texts(&rt, frame, ctx)?;

        // ---- 5. 按钮 ----
        draw_buttons(&rt, frame, ctx)?;

        // ---- 5.5 右上角关闭 ×（v1.5.0 / G3）----
        // 必须在按钮之后、进度条之前：进度条贴在条带底部、整宽，画在它后面
        // 就会被压住（× 在右上角，实际不重叠，但顺序上先画更安全）。
        draw_dismiss(&rt, frame, ctx)?;

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

/// 悬停背景：`idle_bg → hover_bg` 按 `t`（悬停淡入进度）线性插值。
///
/// v1.1.12（背景体系扩展）：常规态也可以是半透明（`idle_bg`），悬停淡入
/// 就是两色之间的过渡；任一端 alpha = 0（透明档）时该端画不出来，
/// 全程 alpha = 0 则整帧跳过背景绘制。
fn draw_background(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    alpha: f32,
) -> Result<()> {
    let (theme, s) = (&ctx.theme, ctx.scale);
    let w = frame.width as f32;
    let h = frame.height as f32;

    let bg = crate::render::theme::mix_rgba(theme.idle_bg, theme.hover_bg, alpha);
    if bg.a == 0 {
        return Ok(());
    }

    let radius = (8.0 * s).min(h * 0.5);
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
        },
    )?;
    Ok(())
}

/// 画一行文本。
///
/// v1.2.0：**取消了 1px 反色描边兜底**（G5 问题 3）。那条兜底只在
/// luma ≈ 0.174~0.189 的窄带触发，画出来是一圈"异常描边"，用户明确要求去掉；
/// 中灰背景的可读性由「按对比度择优 + 亮度稳定器」保证（`render::stable`）。
fn draw_line(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    d: &TextDraw<'_>,
) -> Result<()> {
    let text = ctx.text;
    let (kind, s, rect, color) = (d.kind, d.text, d.rect, d.color);
    if s.is_empty() || rect.is_empty() {
        return Ok(());
    }
    let utf16 = to_utf16(s);
    if utf16.is_empty() {
        return Ok(());
    }
    let rect_d = d2d_rect(rect.x, rect.y, rect.w, rect.h);
    let format = text.get(kind);

    let brush = frame.brush(color)?;
    // SAFETY: 栈上矩形；brush 由本帧持有；utf16 生命周期覆盖调用。
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

// ------------------------------------------------------------------ 关闭 ×（v1.5.0 / G3）

/// 右上角常驻的关闭 ×（点击 = 隐藏到托盘，仅本次运行）。
///
/// ## 为什么单独一个函数而不是塞进 `draw_buttons`
///
/// `draw_buttons` 的循环吃「媒体播控三键 + 启用/禁用态」，× 既不属于播控、
/// 也没有禁用态（任何播放状态都可收起）；硬塞进去会让那个循环多出两处
/// `if target == Dismiss` 的分叉，反而更难读。
///
/// ## 视觉规格（v1.5.1：缩小 + 换一种悬停表达）
///
/// - 字形常态就是 `0xFF` 满不透明（用户选 100%，且 8px 小字形再半透明会发虚）；
/// - **因此不能再靠"变亮"做悬停反馈**——已经满不透明就没有余量。改为在
///   `hit_dismiss` 那一格里铺一层**极淡圆角底**：悬停 `theme.text @ 0x14`（≈8%）、
///   按下 `0x22`（≈13%），圆角 `4 * scale`。这是 Win11 任务栏图标悬停的常规做法：
///   **不是**描边、**不是**光晕、**不**向外扩（底矩形取自 `dismiss_bg_rect()`，
///   语义上恒等于 `hit_dismiss`，几何契约由 `layout::tests` 锁住）；
/// - 按下除了加深底色，字形整体下沉 `1 * scale`（与三键同一套手感）；
/// - 完整的光晕 / 缩放留 G4 —— 布局里的 `dismiss` / `hit_dismiss` 与
///   `hover_target` / `pressed` 字段已经建好，G4 直接复用，不再改数据流。
fn draw_dismiss(rt: &ID2D1RenderTarget, frame: &mut Frame, ctx: &Ctx<'_>) -> Result<()> {
    let (layout, state, theme, s) = (ctx.layout, ctx.state, &ctx.theme, ctx.scale);
    let glyph = layout.dismiss;
    if glyph.is_empty() {
        return Ok(());
    }

    let hovering = state.hover_target == Some(HitTarget::Dismiss);
    let pressed = state.pressed == Some(HitTarget::Dismiss);

    // ---- 1. 悬停 / 按下的极淡圆角底（v1.5.1）----
    // 只在 `hit_dismiss` 那一格里画：底矩形 = `dismiss_bg_rect()`，绝不外扩，
    // 因此天然与 `hit_next` 保持 DISMISS_GAP 的净距。
    let bg_alpha = if pressed {
        limits::DISMISS_BG_PRESSED
    } else if hovering {
        limits::DISMISS_BG_HOVER
    } else {
        0
    };
    if bg_alpha > 0 {
        let bg = layout.dismiss_bg_rect();
        if !bg.is_empty() {
            // 圆角钳到短边一半：D2D 对 radius > 半边长 的圆角矩形会画成胶囊/异常。
            let radius = (limits::DISMISS_BG_RADIUS * s).min(bg.w.min(bg.h) * 0.5);
            let bg_brush = frame.brush(Rgba::from_rgb(theme.text, bg_alpha))?;
            // SAFETY: 栈上圆角矩形；坐标全部有限；brush 由本帧持有。
            unsafe {
                rt.FillRoundedRectangle(&rounded(bg.x, bg.y, bg.w, bg.h, radius), &bg_brush);
            }
        }
    }

    // ---- 2. 字形：常态即满不透明（不再有 0xCC → 0xFF 的变亮）----
    let brush = frame.brush(Rgba::from_rgb(theme.text, limits::DISMISS_ALPHA))?;

    // 按下：整体下沉 1px（与三键同一口径：`1.0 * scale`）
    let sink = if pressed { 1.0 * s } else { 0.0 };

    let side = glyph.w.min(glyph.h);
    fill_cross(
        rt,
        &brush,
        glyph.x,
        glyph.y + sink,
        side,
        limits::DISMISS_LINE * s,
    );
    Ok(())
}

/// 用**逐行矩形**填一个 45° 的 ×（避免依赖 `ID2D1PathGeometry` / 变换矩阵，
/// 与 [`fill_triangle`] 保持同一种"纯 `FillRectangle`"实现风格）。
///
/// 每一行画两个短横条：`run` 是笔画在**水平方向**的截面宽度，
/// 45° 斜线的垂直粗细 `lw` 对应 `run = lw * √2`。
/// 两端各留 `run / 2` 的内缩，笔画不会顶出 `side × side` 的字形框。
/// 行间多画 0.6px 消除抗锯齿接缝（同 [`fill_triangle`]）。
fn fill_cross(
    rt: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    x: f32,
    y: f32,
    side: f32,
    line_width: f32,
) {
    let side = side.max(1.0);
    let rows = side.round().max(1.0) as i32;
    let step = side / rows as f32;
    let run = (line_width.max(0.4) * std::f32::consts::SQRT_2).min(side * 0.5);
    let inset = run * 0.5;
    let span = (side - 2.0 * inset).max(0.0);

    for i in 0..rows {
        // 行中心相对字形框的归一化位置 0..1
        let t = (i as f32 + 0.5) / rows as f32;
        let yy = y + i as f32 * step;
        // 左上→右下：中心从 inset 走到 side-inset
        let cx1 = x + inset + t * span;
        // 右上→左下：镜像
        let cx2 = x + side - inset - t * span;
        // SAFETY: 栈上矩形；坐标全部有限；brush 由本帧持有。
        unsafe {
            rt.FillRectangle(&d2d_rect(cx1 - run * 0.5, yy, run, step + 0.6), brush);
            rt.FillRectangle(&d2d_rect(cx2 - run * 0.5, yy, run, step + 0.6), brush);
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

// ---------------------------------------------------------------- 跑马灯（v1.1.3）

/// 长歌词滚动的**纯函数**（v1.1.3：行时间窗口驱动，取代 v1.1.2 固定速度跑马灯）。
///
/// 滚动行程按当前歌词行的时间窗口线性分配：窗口内匀速滚完全程，唱完该句时
/// 刚好滚到尾——快歌慢歌都不会出现「句子切走了还没滚完」。时间基准与扫光
/// 一致（播放位置外推），暂停自然冻结、拖动跳转天然跟随，无需行指纹归零。
///
/// 窗口短于 [`limits::MARQUEE_MIN_WINDOW_SECS`] 的行返回 0，由调用方判定
/// 不激活（窗口太短滚完全程会快到看不清，只显示行首）。
///
/// 返回值是「文本相对视口的左移量」`0.0..=excess`。
pub fn marquee_offset(excess: f32, elapsed_secs: f32, window_secs: f32) -> f32 {
    if excess <= 0.5 || window_secs < limits::MARQUEE_MIN_WINDOW_SECS {
        return 0.0;
    }
    // 行内进度 0..=1（行开始前 / 唱完后——如最后一行常驻屏幕——都钳在两端）
    let p = (elapsed_secs / window_secs).clamp(0.0, 1.0);
    excess * p
}

/// 画歌词区（T12）：当前行**逐字扫光**。
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
/// 长行处理：v1.1.2 起超宽行走跑马灯（视口裁剪 + 左移偏移）；v1.1.4 删除
/// 右缘渐隐带——普通路径实色硬裁（放不下的部分不显示），不再有渐变吃淡
/// 最后一个字的问题。文本仍整行进布局，扫光码元映射保持正确。
fn draw_lyrics(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    lyric: &LyricDraw<'_>,
    lyric_layout: Option<(&IDWriteTextLayout, u32)>,
) -> Result<()> {
    let (layout, state, theme) = (ctx.layout, ctx.state, &ctx.theme);
    // v1.1.1：歌词行是歌名/歌手行下方的单行——关闭（Compact）或宽度动画未展开时跳过
    if layout.lyric_cur.is_empty() || lyric.is_empty() {
        return Ok(());
    }
    // 开关宽度过渡期间的淡入/淡出系数（App 按动画进度写入，稳定态为 1.0）
    let fade = state.lyric_alpha.clamp(0.0, 1.0);
    let scale_a = |a: u8| ((a as f32) * fade).round() as u8;

    // ---- 切歌瞬间：旧歌词淡出 + 上移（M3 遗留 P2）----
    // 淡出在前 120ms 内完成（保持原 PRD 观感），上移走满 150ms（ease-out cubic）。
    // 这期间不画新内容。v1.1.1 起没有下一行，转场只涉及当前行。
    if let Some((old_cur, started)) = &state.lyric_fade {
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
                        color: Rgba::from_rgb(theme.text, scale_a(a)),
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
                color: Rgba::from_rgb(theme.text_dim, scale_a(0xB0)),
            },
        );
    }

    if lyric.cur.is_empty() || layout.lyric_cur.is_empty() {
        return Ok(());
    }

    // ---- 当前行 ----
    // v1.1.2 跑马灯路径：歌词超宽且开关打开 → 视口裁剪 + 左移偏移（平色，不用渐隐）。
    // 扫光边界在**文本坐标**里照常映射，位移只发生在绘制层，码元映射不变。
    if state.lyric_marquee {
        let off = state.lyric_scroll_offset.clamp(0.0, 1.0e6);
        let rect = layout.lyric_cur;
        // 绘制矩形随偏移左移并加宽（多留 2px 余量抵消测量舍入，保证尾部完整进布局）
        let wide = RectF::new(
            rect.x - off,
            rect.y,
            rect.w + off + 2.0 * ctx.scale,
            rect.h,
        );
        // SAFETY: 视口裁剪矩形是栈上值；Push/Pop 严格配对（见下方先收结果再 Pop）。
        unsafe {
            rt.PushAxisAlignedClip(&d2d_rect(rect.x, rect.y, rect.w, rect.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        // 未唱部分：整行平色
        let unsung = draw_line(
            rt,
            frame,
            ctx,
            &TextDraw {
                kind: TextKind::Lyric,
                text: lyric.cur,
                rect: wide,
                color: Rgba::from_rgb(theme.text_dim, scale_a(0x8C)),
            },
        );
        // 已唱部分：扫光边界换算到屏幕坐标（文本 x − 偏移），再与视口求交
        let sung: Result<()> = match lyric_layout {
            Some((text_layout, utf16_len)) if lyric.sweep > 0.01 => {
                let boundary = crate::render::text::hit_test_x(text_layout, utf16_len, lyric.sweep);
                let vis = (boundary - off).clamp(0.0, rect.w);
                if vis <= 0.5 {
                    Ok(())
                } else {
                    // SAFETY: 内层裁剪与视口裁剪嵌套；Push/Pop 严格配对。
                    unsafe {
                        rt.PushAxisAlignedClip(&d2d_rect(rect.x, rect.y, vis, rect.h), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    }
                    let r = draw_line(
                        rt,
                        frame,
                        ctx,
                        &TextDraw {
                            kind: TextKind::Lyric,
                            text: lyric.cur,
                            rect: wide,
                            color: Rgba::from_rgb(theme.text, scale_a(0xFF)),
                        },
                    );
                    // SAFETY: 与内层 Push 配对。
                    unsafe {
                        rt.PopAxisAlignedClip();
                    }
                    r
                }
            }
            _ => draw_line(
                rt,
                frame,
                ctx,
                &TextDraw {
                    kind: TextKind::Lyric,
                    text: lyric.cur,
                    rect: wide,
                    color: Rgba::from_rgb(theme.text, scale_a(0xFF)),
                },
            ),
        };
        // SAFETY: 与最外层视口 Push 配对。
        unsafe {
            rt.PopAxisAlignedClip();
        }
        unsung?;
        return sung;
    }

    // ---- 普通路径（未超宽 / 滚动关闭）：未唱部分 ----
    // v1.1.4：右缘渐隐带已删——实色绘制，放不下的部分被视口硬裁。
    let unsung = Rgba::from_rgb(theme.text_dim, scale_a(0x8C));
    draw_line(
        rt,
        frame,
        ctx,
        &TextDraw {
            kind: TextKind::Lyric,
            text: lyric.cur,
            rect: layout.lyric_cur,
            color: unsung,
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
                color: Rgba::from_rgb(theme.text, scale_a(0xFF)),
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
    // 且 `?` 提前返回前已保证没有未配对的 Push——这里 draw_line 失败会
    // 跳过 Pop，所以先用 let 接住结果再 Pop）。
    unsafe {
        rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    }
    let r = draw_line(
        rt,
        frame,
        ctx,
        &TextDraw {
            kind: TextKind::Lyric,
            text: lyric.cur,
            rect: layout.lyric_cur,
            color: Rgba::from_rgb(theme.text, scale_a(0xFF)),
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

    /// 跑马灯窗口驱动：行内进度线性映射滚动量，两端钳位，短行只显示行首。
    #[test]
    fn marquee_offset_tracks_window_linearly() {
        let excess = 60.0f32;
        let window = 10.0f32;

        // 行首：偏移 0
        assert_eq!(marquee_offset(excess, 0.0, window), 0.0);
        // 行内线性：播到一半滚一半
        let mid = marquee_offset(excess, window * 0.5, window);
        assert!((mid - excess * 0.5).abs() < 0.01, "mid = {mid}");
        // 窗口尽头：刚好滚到尾
        assert!((marquee_offset(excess, window, window) - excess).abs() < 1e-4);
        // 唱完之后（最后一行常驻屏幕）：钳在尾部
        assert_eq!(marquee_offset(excess, window + 99.0, window), excess);
        // 行开始之前（理论上不出现）：钳 0
        assert_eq!(marquee_offset(excess, -1.0, window), 0.0);
        // 不超宽：永远 0
        assert_eq!(marquee_offset(0.0, 100.0, window), 0.0);
        assert_eq!(marquee_offset(-5.0, 100.0, window), 0.0);
        // 短窗口（< MARQUEE_MIN_WINDOW_SECS）：不滚，只显示行首
        assert_eq!(marquee_offset(excess, 0.5, limits::MARQUEE_MIN_WINDOW_SECS - 0.1), 0.0);
        // 窗口内单调不减且有界
        let mut prev = 0.0f32;
        for i in 0..=200 {
            let t = window * i as f32 / 200.0;
            let o = marquee_offset(excess, t, window);
            assert!(o >= prev - 1e-6, "回退：t={t} o={o} prev={prev}");
            assert!((0.0..=excess).contains(&o), "越界：t={t} o={o}");
            prev = o;
        }
    }
}
