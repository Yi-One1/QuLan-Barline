//! 绘制（`ARCHITECTURE.md` §11.1 渲染管线的第 3–12 步）。
//!
//! 绘制顺序**不能变**：
//! ```text
//! 1. BeginDraw
//! 2. Clear(全透明)                 ← 不 clear 会残留上一帧（风险 R3）
//! 3. 命中区占位 alpha = 1           ← 视觉不可见但整块可点（§10.7）
//! 4. 悬停背景（idle→hover 两态插值，v1.1.12 起常规态也可半透明）
//! 5. 封面（真实位图 or 渐变占位 + 音符；v1.6.0 悬停叠加变暗蒙层 + 双箭头）
//! 6. 歌名 / 歌手（读 WidgetLayout 里缓存的截断结果）
//! 7. 三个按钮（含 disabled / 悬停光晕 / 按下下沉+缩放，v1.6.0 / G4）
//! 7.5 右上角关闭 ×（v1.5.0 / G3；v1.6.0 起与三键共用同一套悬停/按下反馈）
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

    // G4：位图分支不再 early return —— 悬停蒙层必须同时盖住真实位图与占位图。
    // 用 Option 承接位图，本函数内不留任何朝向布尔字面量：
    // 两次 fill_arrow 的朝向一律由 cover_arrow_specs 派生。
    let bitmap = if let Some(cover) = state.playing.cover.as_ref() {
        let rt_clone = frame.rt.clone();
        let wic = frame.wic.clone();
        covers.get(&rt_clone, &wic, slot, state.playing.track_id, cover)
    } else {
        None
    };

    if let Some(bmp) = bitmap {
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
    } else {
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
            rt.FillEllipse(
                &ellipse(cx - 4.0 * unit, cy + 5.0 * unit, 4.2 * unit, 3.2 * unit),
                &fg,
            );
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
    }

    // ---- G4：封面悬停 = 恒暗蒙层 + 恒亮双箭头 ----
    // `hover_target == Cover` 由 `Interaction::on_move` 维护（G3 起就进 frame_hash），
    // 点击整块封面仍是 `ClickCover`（打开播放器），不新增交互语义。
    //
    // 颜色边界（与 R2「按钮光晕同源」不冲突）：按钮光晕是**全窗 hover_bg 的延伸**，
    // 所以内层取 hover_bg 的 RGB；封面蒙层是**主动叠暗一层**的独立视觉操作，
    // 前景色由这个固定操作决定 —— 恒暗蒙层 + 恒亮白箭头。若改用 `theme.text`，
    // 浅壁纸下深色字叠在黑蒙层上会看不见；恒亮箭头在深浅两种壁纸下都成立。
    if ctx.state.hover_target == Some(HitTarget::Cover) {
        let (dr, dg, db) = limits::COVER_DIM_RGB;
        let dim = frame.brush(Rgba::new(dr, dg, db, limits::COVER_HOVER_DIM))?;
        // SAFETY: 栈上圆角矩形；半径与封面一致，不会溢出封面。
        unsafe {
            rt.FillRoundedRectangle(&rounded(c.x, c.y, c.w, c.h, radius), &dim);
        }
        let (ar, ag, ab) = limits::COVER_ARROW_RGB;
        let fg = frame.brush(Rgba::new(ar, ag, ab, limits::COVER_ARROW_ALPHA))?;
        let size = limits::COVER_ARROW_SIZE * s;
        let pad = limits::COVER_ARROW_PAD * s;
        let line = limits::COVER_ARROW_LINE * s;
        // Q4：朝向只由 COVER_ARROW_ORIENTATION 一处常量决定，本函数不留方向字面量。
        let specs = cover_arrow_specs(c, pad);
        fill_arrow(rt, &fg, specs[0].0, specs[0].1, size, line, specs[0].2);
        fill_arrow(rt, &fg, specs[1].0, specs[1].1, size, line, specs[1].2);
    }
    Ok(())
}

/// Q4：封面双箭头的两个 `(tip_x, tip_y, down_right)`，**纯函数**、无 D2D 依赖。
///
/// 朝向只由 [`limits::COVER_ARROW_ORIENTATION`] 这一处具名常量决定：
/// - `0`：左上 ↖ + 右下 ↘（对角外扩，当前拍板值）；
/// - `1`：左下 ↙ + 右上 ↗（另一组对角）；
/// - 其它值**兜底为 0**。
///
/// 返回固定长度 2 的数组，让 [`draw_cover`] 里两次 [`fill_arrow`] 调用完全
/// 从派生结果取值，不留任何方向字面量；真人验收后改常量一处即整体换向。
fn cover_arrow_specs(c: RectF, pad: f32) -> [(f32, f32, bool); 2] {
    cover_arrow_specs_for(c, pad, limits::COVER_ARROW_ORIENTATION)
}

/// [`cover_arrow_specs`] 的可测内核：`orientation` 由调用方显式给出，
/// 便于单测穷举 `0 / 1 / 其它值兜底`，而不必改全局常量。
fn cover_arrow_specs_for(c: RectF, pad: f32, orientation: u8) -> [(f32, f32, bool); 2] {
    match orientation {
        1 => [
            (c.x + pad, c.y + c.h - pad, true), // 左下 tip → ↙（斜杆向右上延伸）
            (c.x + c.w - pad, c.y + pad, false), // 右上 tip → ↗（斜杆向左下延伸）
        ],
        _ => [
            (c.x + pad, c.y + pad, false), // 左上 tip → ↖
            (c.x + c.w - pad, c.y + c.h - pad, true), // 右下 tip → ↘
        ],
    }
}

/// Q4：由「唯一朝向常量 + 本支箭头的选择位」推出斜杆从 tip 延伸的方向 `(sx, sy)`；
/// 箭头指向恒为 `(-sx, -sy)`。`0` 走主对角线（↖ + ↘），`1` 走另一条对角线
/// （↗ + ↙），其它值兜底为 `0`。
fn arrow_axis(orientation: u8, down_right: bool) -> (f32, f32) {
    match orientation {
        1 => {
            if down_right {
                (1.0, -1.0)
            } else {
                (-1.0, 1.0)
            }
        }
        _ => {
            if down_right {
                (-1.0, -1.0)
            } else {
                (1.0, 1.0)
            }
        }
    }
}

/// 画一个小 45° 箭头（G4 封面悬停用）：斜杆用逐行矩形模拟，端点两翼是直角短线。
///
/// `tip` 是箭头尖端；`down_right` 是 [`cover_arrow_specs`] 给出的「本支箭头选择位」，
/// 与 [`limits::COVER_ARROW_ORIENTATION`] 一起经 [`arrow_axis`] 决定斜杆方向
/// （`0` 档 ↖/↘，`1` 档 ↗/↙）。与 [`fill_cross`] / [`fill_triangle`]
/// 保持同一种「纯 `FillRectangle`」风格，不引入 `ID2D1PathGeometry`。
fn fill_arrow(
    rt: &ID2D1RenderTarget,
    brush: &ID2D1SolidColorBrush,
    tip_x: f32,
    tip_y: f32,
    size: f32,
    line: f32,
    down_right: bool,
) {
    let (sx, sy) = arrow_axis(limits::COVER_ARROW_ORIENTATION, down_right);
    // 0.55（翼长比例）/ 0.6（行间重叠消 AA 接缝）/ max(2.0)（最少行数）与
    // fill_triangle / fill_cross 是同款 AA 实现细节，**非可调视觉参数**。
    let rows = size.round().max(2.0) as i32;
    let step = size / rows as f32;
    let wing = size * 0.55;
    // SAFETY: 全部是栈上矩形，坐标由有限输入推导。
    unsafe {
        for i in 0..rows {
            let t = (i as f32 + 0.5) / rows as f32;
            let xx = tip_x + sx * t * size;
            let yy = tip_y + sy * t * size;
            rt.FillRectangle(
                &d2d_rect(xx - line * 0.5, yy - line * 0.5, line, step + 0.6),
                brush,
            );
        }
        // 两翼构成 "L" 形箭头头：沿斜杆的两个轴向各从 tip 伸出一支。
        // sx/sy = (1,1) 时逐像素等价于 WIP 的 ↖ 分支，(-1,-1) 等价于 ↘ 分支。
        let hx = if sx > 0.0 { tip_x } else { tip_x - wing };
        let hy = if sy > 0.0 { tip_y } else { tip_y - line };
        let vx = if sx > 0.0 { tip_x } else { tip_x - line };
        let vy = if sy > 0.0 { tip_y } else { tip_y - wing };
        rt.FillRectangle(&d2d_rect(hx, hy, wing, line), brush);
        rt.FillRectangle(&d2d_rect(vx, vy, line, wing), brush);
    }
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

// ------------------------------------------------------------------ 按钮（v1.6.0 / G4 统一反馈）

/// 单个按钮（播控三键或关闭 ×）的交互视觉状态。
///
/// G4 要求「× 与四按钮共用同一套悬停/按下状态机与绘制函数」：三键的
/// `draw_buttons` 与 × 的 `draw_dismiss` 都只经 [`ButtonVisual::new`] 读状态、
/// 都只经 [`draw_button_feedback`] 画光晕，不再各写一份 `if hovering`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ButtonVisual {
    hovering: bool,
    pressed: bool,
}

impl ButtonVisual {
    fn new(state: &AppState, target: HitTarget) -> Self {
        Self {
            hovering: state.hover_target == Some(target),
            pressed: state.pressed == Some(target),
        }
    }

    /// 是否要画光晕（悬停或按下；Q1 的极淡保底由 [`glow_params`] 统一给出，
    /// 这里不读背景档位、也不按 target 分支）。
    fn glowing(self) -> bool {
        self.hovering || self.pressed
    }

    /// 按下时图标整体下沉量（物理像素）。
    fn sink(self, s: f32) -> f32 {
        if self.pressed {
            limits::BUTTON_PRESSED_SINK * s
        } else {
            0.0
        }
    }

    /// 按下时图标等比缩放系数（1.0 = 原尺寸）。
    fn icon_scale(self) -> f32 {
        if self.pressed {
            limits::BUTTON_PRESSED_SCALE
        } else {
            1.0
        }
    }
}

/// 双层光晕的绘制参数（把原先的三元组具名化，避免调用点靠位置猜语义）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct GlowParams {
    inner_alpha: u8,
    outer_alpha: u8,
    grow_factor: f32,
}

impl GlowParams {
    const fn new(inner_alpha: u8, outer_alpha: u8, grow_factor: f32) -> Self {
        Self {
            inner_alpha,
            outer_alpha,
            grow_factor,
        }
    }
}

/// 纯函数：把「悬停 / 按下 + 全窗悬停背景 alpha」换算成双层光晕参数。
///
/// **唯一强度输入是 `theme.hover_bg.a`**（不读 `BackgroundMode` 枚举）；
/// 色源天然同源：透明档只把 alpha 归零、`hover_bg` 的 RGB 基色完整保留
/// （见 `theme::apply_bg_config`），所以只需修 alpha 这一个维度。
///
/// Q1 用户拍板「保留极淡保底」：逐分量 `.max(GLOW_FLOOR_ALPHA)`，即强度曲线
/// 在 `a → 0` 处的**非零下限截距** —— 始终透明档下四 target（含 ×）仍有极淡
/// 光晕，**不是**「仅在 a == 0 时整体覆盖」，也**不会**归零；`.min(nom)`
/// 保证满档不超过常量上限。无态恒 `(0, 0, 1.0)`，保底不作用于无态（否则常亮）。
///
/// 函数签名与函数体都不含 `HitTarget`：四 target 同值是**结构性保证**，
/// 不是靠调用方自觉。
fn glow_params(hovering: bool, pressed: bool, hover_bg_a: u8) -> GlowParams {
    if !hovering && !pressed {
        return GlowParams::new(0, 0, 1.0);
    }
    let (inner_nom, outer_nom) = if pressed {
        (limits::GLOW_INNER_PRESSED, limits::GLOW_OUTER_PRESSED)
    } else {
        (limits::GLOW_INNER_HOVER, limits::GLOW_OUTER_HOVER)
    };
    let k = f32::from(hover_bg_a) / 255.0;
    let scaled = |nom: u8| -> u8 {
        ((f32::from(nom) * k).round() as u8)
            .max(limits::GLOW_FLOOR_ALPHA)
            .min(nom)
    };
    GlowParams::new(
        scaled(inner_nom),
        scaled(outer_nom),
        if pressed {
            limits::GLOW_GROW_PRESSED_FACTOR
        } else {
            1.0
        },
    )
}

/// 双层光晕的颜色：内层「底」取全窗悬停背景的 RGB（同源），
/// 外层「影 / 光」取 `on_light` 的对比色 —— 浅壁纸＝白底黑影、深壁纸＝黑底白光。
///
/// 透明档 `hover_bg.a == 0` 时 RGB 仍完整取自 `hover_bg`（基色保留），故
/// 同色源结论在两档下都成立；alpha 才由 [`glow_params`] 单独缩放。
fn glow_colors(theme: &Theme) -> (Rgba, Rgba) {
    let base = Rgba::new(theme.hover_bg.r, theme.hover_bg.g, theme.hover_bg.b, 0xFF);
    let halo = if theme.on_light {
        Rgba::new(0x00, 0x00, 0x00, 0xFF)
    } else {
        Rgba::new(0xFF, 0xFF, 0xFF, 0xFF)
    };
    (base, halo)
}

/// 画统一的双层柔和光晕（三键与 × 共用），并返回该按钮的交互态。
///
/// `center` 取图标中心（`(cx, cy)` 合成一个元组，让参数数守住 clippy 的 7 参上限，
/// 几何含义与拆开写完全一致）；`radius` / `grow` 是**几何参数**（28×28 三键 vs
/// 14×14 × 命中区各自收敛），**非反馈强度**：alpha 与颜色一律不按 `target` 分支，
/// 两者只由 [`glow_params`] / [`glow_colors`] 决定。
/// 光晕是椭圆而非命中区矩形，因此 ×（14×14）与三键（28×28）能按各自尺寸
/// 取半径、又共用同一套画法。
fn draw_button_feedback(
    rt: &ID2D1RenderTarget,
    frame: &mut Frame,
    ctx: &Ctx<'_>,
    target: HitTarget,
    center: (f32, f32),
    radius: f32,
    grow: f32,
) -> Result<ButtonVisual> {
    let (cx, cy) = center;
    let v = ButtonVisual::new(ctx.state, target);
    if !v.glowing() {
        return Ok(v);
    }
    let GlowParams {
        inner_alpha: inner_a,
        outer_alpha: outer_a,
        grow_factor: grow_k,
    } = glow_params(v.hovering, v.pressed, ctx.theme.hover_bg.a);
    if inner_a == 0 && outer_a == 0 {
        // 仅当常量被误配（GLOW_FLOOR_ALPHA == 0 且四个 nom 全为 0）时才可能到达；
        // Q1 保底后正常路径不经过此处。保留它是为了明确「保底逻辑不在这里」。
        return Ok(v);
    }
    let (base, halo) = glow_colors(&ctx.theme);
    let r = radius * ctx.scale;
    let outer_r = r + grow * ctx.scale * grow_k;
    // SAFETY: 椭圆与笔刷都是栈上值 / 本帧持有；半径恒为正且有限。
    unsafe {
        if outer_a > 0 {
            let brush = frame.brush(Rgba { a: outer_a, ..halo })?;
            rt.FillEllipse(&ellipse(cx, cy, outer_r, outer_r), &brush);
        }
        if inner_a > 0 {
            let brush = frame.brush(Rgba { a: inner_a, ..base })?;
            rt.FillEllipse(&ellipse(cx, cy, r, r), &brush);
        }
    }
    Ok(v)
}

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
        let (cx, cy) = rect.center();
        // G4：三键与 × 共用同一套悬停/按下反馈（双层光晕 + 下沉 + 等比缩放）。
        // 几何边界（架构师已算，写进注释备查）：三键外圈 (11+3)*s = 14*s 恰为
        // 28×28 命中区半边；光晕左缘与歌名/歌手右缘净距 8*s、与进度条垂直
        // 间隔 4px，不盖文字、不盖进度条。alpha / 颜色不按 target 分支。
        let v = draw_button_feedback(
            rt,
            frame,
            ctx,
            target,
            (cx, cy),
            limits::GLOW_RADIUS_BUTTON,
            limits::GLOW_GROW_BUTTON,
        )?;
        let color = if on { enabled } else { disabled };
        let brush = frame.brush(color)?;
        let sink = v.sink(s);
        // 按下等比缩到 94%：内部所有硬编码偏移同乘 k，图标形状不变；
        // k == 1 时与 v1.5.6 的逐像素输出完全一致（不引入 DPI 行为变化）。
        let k = v.icon_scale();
        let icon = limits::BTN_ICON * s * k;
        let x = cx - icon * 0.5;
        let y = cy - icon * 0.5 + sink;

        // SAFETY: 全部是几何图元；brush 由本帧持有。
        unsafe {
            match target {
                HitTarget::Prev => {
                    // 竖条 + 左向三角
                    rt.FillRectangle(
                        &d2d_rect(x + 1.0 * k, y + 1.5 * k, 2.2 * k, icon - 3.0 * k),
                        &brush,
                    );
                    fill_triangle(
                        rt,
                        &brush,
                        x + 4.2 * k,
                        y + 1.5 * k,
                        icon - 5.6 * k,
                        icon - 3.0 * k,
                        false,
                    );
                }
                HitTarget::Next => {
                    fill_triangle(
                        rt,
                        &brush,
                        x + 1.4 * k,
                        y + 1.5 * k,
                        icon - 5.6 * k,
                        icon - 3.0 * k,
                        true,
                    );
                    rt.FillRectangle(
                        &d2d_rect(
                            x + icon - 3.2 * k,
                            y + 1.5 * k,
                            2.2 * k,
                            icon - 3.0 * k,
                        ),
                        &brush,
                    );
                }
                _ => {
                    if playing {
                        // 暂停：两根竖条
                        rt.FillRectangle(
                            &d2d_rect(x + 4.0 * k, y + 1.5 * k, 3.0 * k, icon - 3.0 * k),
                            &brush,
                        );
                        rt.FillRectangle(
                            &d2d_rect(x + 9.0 * k, y + 1.5 * k, 3.0 * k, icon - 3.0 * k),
                            &brush,
                        );
                    } else {
                        // 播放：右向三角
                        fill_triangle(
                            rt,
                            &brush,
                            x + 4.6 * k,
                            y + 1.5 * k,
                            icon - 6.0 * k,
                            icon - 3.0 * k,
                            true,
                        );
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
/// ## 视觉规格（v1.5.1：缩小 + 换一种悬停表达；v1.5.4：字形 6px + 笔画加粗）
///
/// - 字形常态就是 `0xFF` 满不透明（用户选 100%，且 8px 小字形再半透明会发虚）；
///   v1.5.4 用户再次要求「× 的不透明度与"下一首"一致」—— 查证结果是**本来就同值**，
///   所以这里**直接写字面量 `0xFF`**（与 `draw_buttons` 的 enabled 同源），
///   让「两个值可能漂移」在代码层面也不可能发生。用户看到的「淡」是**笔画太细**
///   造成的覆盖率损失，由 `limits::DISMISS_LINE = 1.2` 解决；
/// - **v1.6.0 / G4**：v1.5.1 那层 `theme.text @ 0x14` 极淡圆角底已废弃 —— 它与
///   全窗 `hover_bg` / `on_light` 不同源，透明档下会出现「全局悬停消失但 × 仍亮」
///   （QA 遗留）。现在 × 与三键共用 [`draw_button_feedback`] 的双层柔和光晕：
///   内层「底」取 `hover_bg` 的 RGB、外层「影/光」按 `on_light` 取对比色，
///   两层 alpha 都随 `hover_bg.a` 缩放**且有保底下限**（Q1：透明档下仍有
///   `GLOW_FLOOR_ALPHA` 的极淡反馈，不是归零淡出）；半径按 14×14 热区收敛到
///   [`limits::GLOW_RADIUS_DISMISS`]；
/// - 按下：光晕外圈收紧到一半、字形整体下沉 `1 * scale` 并等比缩到 94%，
///   与三键同一套 [`ButtonVisual`] 状态机；
/// - 几何边界（架构师已算）：× 外圈 `(5+1.5)*s = 6.5*s` < 14×14 半边 `7*s`，
///   与「下一首」光晕净距 `2.5*s` 不重叠（100% DPI 是否视觉粘连留真机验收）；
/// - 单独保留本函数只是因为 × 的字形（[`fill_cross`]）与播控图标形状不同，
///   **交互反馈本身没有第二套实现**。
fn draw_dismiss(rt: &ID2D1RenderTarget, frame: &mut Frame, ctx: &Ctx<'_>) -> Result<()> {
    let (layout, theme, s) = (ctx.layout, &ctx.theme, ctx.scale);
    let glyph = layout.dismiss;
    if glyph.is_empty() {
        return Ok(());
    }

    let (gcx, gcy) = glyph.center();
    // G4：与三键共用同一套悬停/按下状态机与绘制函数；半径/扩张按 14×14 热区收敛。
    let v = draw_button_feedback(
        rt,
        frame,
        ctx,
        HitTarget::Dismiss,
        (gcx, gcy),
        limits::GLOW_RADIUS_DISMISS,
        limits::GLOW_GROW_DISMISS,
    )?;

    // 字形：常态即满不透明（与 draw_buttons 的 enabled 态同一个字面量 0xFF）。
    // 用户曾觉得 × 更淡，真正原因是笔画宽度（见 limits::DISMISS_LINE），不是 alpha。
    let brush = frame.brush(Rgba::from_rgb(theme.text, 0xFF))?;

    // 按下：整体下沉 1px + 等比缩到 94%，以字形中心为锚点保持居中。
    let k = v.icon_scale();
    let sink = v.sink(s);
    let side = glyph.w.min(glyph.h) * k;
    let x = gcx - side * 0.5;
    let y = gcy - side * 0.5 + sink;
    fill_cross(rt, &brush, x, y, side, limits::DISMISS_LINE * s * k);
    Ok(())
}

/// 用**逐行矩形**填一个 45° 的 ×（避免依赖 `ID2D1PathGeometry` / 变换矩阵，
/// 与 [`fill_triangle`] 保持同一种"纯 `FillRectangle`"实现风格）。
///
/// 每一行画两个短横条：`run` 是笔画在**水平方向**的截面宽度，
/// 45° 斜线的垂直粗细 `lw` 对应 `run = lw * √2`。
/// 两端各留 `run / 2` 的内缩，笔画不会顶出 `side × side` 的字形框。
/// 行间多画 0.6px 消除抗锯齿接缝（同 [`fill_triangle`]）。
///
/// ## 当前参数的实际几何（v1.5.4：`side = 6*s`、`lw = 1.2*s`，100% DPI）
///
/// - `rows = round(6) = 6` 行，`step = 1.0`；
/// - `run = 1.2 × √2 ≈ 1.697px`（每行水平墨量，三键的实心条是 2.2px - 见
///   `limits::DISMISS_LINE` 的注释：这就是「墨色浓度对齐」的来源）；
/// - `inset = run / 2 ≈ 0.849`，`span = 6 − 2×0.849 ≈ 4.303`；
/// - 第 i 行（0-based）两条斜线的水平间距 = `side − 2·inset − 2·t·span`：
///   | i | t      | 间距 (px) | 与 run(1.70) 比 |
///   |---|--------|-----------|-----------------|
///   | 0 | 0.0833 | 3.59      | 分离            |
///   | 1 | 0.2500 | 2.15      | 分离            |
///   | 2 | 0.4167 | 0.72      | **重叠**（交叉点）|
///   | 3 | 0.5833 | 0.72      | **重叠**（交叉点）|
///   | 4 | 0.7500 | 2.15      | 分离            |
///   | 5 | 0.9167 | 3.59      | 分离            |
///   中间 2 行（占总高 1/3）合并成一段，正是「×」的交叉；再粗（≥1.6）会让
///   4 行都并起来、变成实心团，「叉」的形态消失 —— 所以 1.2 是上限附近的取值，
///   下限由 `dismiss_ink_density_matches_buttons` 把守。
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
    use crate::render::layout::{compute, TaskbarEdge, WidgetForm};
    use crate::render::theme::BackgroundMode;

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

    // ------------------------------------------- v1.6.0 / G4 悬停反馈统一

    /// 双层光晕参数真值表（A5 五条）：① 保底常量 ② 透明档保底 ③ 无态不常亮
    /// ④ 四 target 一致性 ⑤ `glow_colors` 恒定；并钉住 `a=0xFF` / `a=0xB3` 期望值。
    #[test]
    fn glow_params_truth_table() {
        let p = limits::GLOW_FLOOR_ALPHA;
        // ① Q1 保底常量必须为正，且不超过硬上界 0x06
        assert!(p > 0 && p <= 0x06, "Q1 保底必须为正且 ≤ 0x06");
        assert_eq!(p, 0x06, "本轮拍板值；真人验收微调请同步此断言");

        // ③ 无态：任意档位恒不画，保底不作用于无态（否则按钮常亮）
        for a in [0u8, 0x40, 0xB3, 0xFF] {
            assert_eq!(
                glow_params(false, false, a),
                GlowParams::new(0, 0, 1.0),
                "无态 a={a}"
            );
        }
        assert_eq!(glow_params(false, false, 0), GlowParams::new(0, 0, 1.0));

        // ② 透明档 Q1 保底：inner == outer == P（双层靠 RGB + 半径区分），
        //    光晕始终保留极淡反馈、强度仍随全窗档位缩放。
        assert_eq!(glow_params(true, false, 0), GlowParams::new(p, p, 1.0));
        assert_eq!(glow_params(true, true, 0), GlowParams::new(p, p, 0.5));
        assert_eq!(glow_params(false, true, 0), GlowParams::new(p, p, 0.5));

        // 普通档 70% → hover_bg.a = alpha_pct(70) = 0xB3 = 179
        //
        // v1.6.1：用户拍板悬停强度由中等档提升，标称值 0x20/0x0C → 0x42/0x1C
        // （按下 0x2E/0x14 → 0x60/0x2E），期望值按新常量重算：
        // round(66*179/255)=46=0x2E / round(28*179/255)=20=0x14；
        // round(96*179/255)=67=0x43 / round(46*179/255)=32=0x20。
        assert_eq!(glow_params(true, false, 0xB3), GlowParams::new(0x2E, 0x14, 1.0));
        assert_eq!(glow_params(true, true, 0xB3), GlowParams::new(0x43, 0x20, 0.5));

        // 满档：悬停 / 按下 / 按下但指针拖走仍按着
        // v1.6.1：标称值上调后满档期望同步变为 0x42/0x1C（悬停）、0x60/0x2E（按下）。
        assert_eq!(glow_params(true, false, 0xFF), GlowParams::new(0x42, 0x1C, 1.0));
        assert_eq!(glow_params(true, true, 0xFF), GlowParams::new(0x60, 0x2E, 0.5));
        assert_eq!(glow_params(false, true, 0xFF), GlowParams::new(0x60, 0x2E, 0.5));
        assert_eq!(
            glow_params(true, false, 0xFF),
            GlowParams::new(limits::GLOW_INNER_HOVER, limits::GLOW_OUTER_HOVER, 1.0)
        );
        assert_eq!(
            glow_params(true, true, 0xFF),
            GlowParams::new(
                limits::GLOW_INNER_PRESSED,
                limits::GLOW_OUTER_PRESSED,
                limits::GLOW_GROW_PRESSED_FACTOR,
            )
        );

        // 半透明档按曲线缩放，恒落在 [P, nom] 内
        let mid = glow_params(true, false, 0x80);
        assert!(mid.inner_alpha >= p && mid.inner_alpha < limits::GLOW_INNER_HOVER);
        assert!(mid.outer_alpha >= p && mid.outer_alpha <= limits::GLOW_OUTER_HOVER);
        assert_eq!(mid.grow_factor, 1.0);

        // ④ 四 target 一致性：经 ButtonVisual::new 走真实状态通道
        let targets = [
            HitTarget::Prev,
            HitTarget::Toggle,
            HitTarget::Next,
            HitTarget::Dismiss,
        ];
        for &t in &targets {
            let mut st = AppState::default();
            st.hover_target = Some(t);
            let v = ButtonVisual::new(&st, t);
            assert_eq!(
                glow_params(v.hovering, v.pressed, 0),
                glow_params(true, false, 0),
                "{t:?} 悬停"
            );
            st.pressed = Some(t);
            let v = ButtonVisual::new(&st, t);
            assert_eq!(
                glow_params(v.hovering, v.pressed, 0xFF),
                glow_params(true, true, 0xFF),
                "{t:?} 按下"
            );
        }

        // ⑤ glow_colors 对两壁纸 / 两档 / 三态恒定，且 base 恒取 hover_bg 的 RGB
        let accent = crate::media::Rgb::SYSTEM_ACCENT_FALLBACK;
        for (on_light, luma) in [(true, 0.9f32), (false, 0.02f32)] {
            for mode in [BackgroundMode::Translucent, BackgroundMode::Transparent] {
                let theme = Theme::from_backdrop_on(
                    on_light,
                    luma,
                    accent,
                    mode,
                    BackgroundMode::Transparent,
                    70,
                    50,
                );
                let expected = glow_colors(&theme);
                for (h, pr) in [(false, false), (true, false), (true, true)] {
                    let _ = glow_params(h, pr, theme.hover_bg.a);
                    assert_eq!(glow_colors(&theme), expected, "glow_colors 与三态无关");
                }
                assert_eq!(
                    (expected.0.r, expected.0.g, expected.0.b),
                    (theme.hover_bg.r, theme.hover_bg.g, theme.hover_bg.b),
                    "内层底恒取 hover_bg 的 RGB（含透明档）"
                );
            }
        }
    }

    /// 光晕颜色与全窗 `hover_bg` / `on_light` 同源（QA v1.5.1 遗留的正面守卫）。
    #[test]
    fn glow_colors_follow_hover_bg_and_on_light() {
        let accent = crate::media::Rgb::SYSTEM_ACCENT_FALLBACK;
        let light = Theme::from_backdrop_on(
            true,
            0.9,
            accent,
            BackgroundMode::Translucent,
            BackgroundMode::Transparent,
            70,
            50,
        );
        let dark = Theme::from_backdrop_on(
            false,
            0.02,
            accent,
            BackgroundMode::Translucent,
            BackgroundMode::Transparent,
            70,
            50,
        );
        for (theme, want_halo) in [
            (light, Rgba::new(0, 0, 0, 0xFF)),
            (dark, Rgba::new(0xFF, 0xFF, 0xFF, 0xFF)),
        ] {
            let (base, halo) = glow_colors(&theme);
            assert_eq!(
                (base.r, base.g, base.b),
                (theme.hover_bg.r, theme.hover_bg.g, theme.hover_bg.b),
                "内层底必须取全窗 hover_bg 的 RGB"
            );
            assert_eq!(halo, want_halo, "外层影/光必须由 on_light 决定");
        }
        // 浅壁纸 = 白底黑影、深壁纸 = 黑底白光
        let (lb, _) = glow_colors(&light);
        let (db, _) = glow_colors(&dark);
        assert!(lb.r > 0xE0 && db.r < 0x40, "浅底应近白、深底应近黑");

        // Q1 补强：透明档 alpha 归零但 RGB 基色完整保留，故 base 仍取 hover_bg 的 RGB，
        // 「四 target 与全窗同源」在透明档下同样成立（不是黑色 / 不是 text 色）。
        for (on_light, luma, want_rgb) in [
            (true, 0.9f32, (0xF2u8, 0xF2u8, 0xF4u8)),
            (false, 0.02f32, (0x1Au8, 0x1Au8, 0x1Eu8)),
        ] {
            let t = Theme::from_backdrop_on(
                on_light,
                luma,
                accent,
                BackgroundMode::Transparent,
                BackgroundMode::Transparent,
                70,
                50,
            );
            assert_eq!(t.hover_bg.a, 0, "透明档 alpha 必须为 0");
            let (base, _) = glow_colors(&t);
            assert_eq!(
                (base.r, base.g, base.b),
                want_rgb,
                "透明档 RGB 基色必须完整保留"
            );
            assert_eq!(
                (base.r, base.g, base.b),
                (t.hover_bg.r, t.hover_bg.g, t.hover_bg.b),
                "透明档 base 仍取 hover_bg 的 RGB"
            );
        }
    }

    /// 按钮交互态：只有按下才下沉 / 缩放，悬停本身不改图标几何。
    #[test]
    fn button_visual_sink_and_scale_truth_table() {
        for (hovering, pressed) in [(false, false), (true, false), (false, true), (true, true)] {
            let v = ButtonVisual { hovering, pressed };
            assert_eq!(v.glowing(), hovering || pressed);
            assert_eq!(
                v.icon_scale(),
                if pressed { limits::BUTTON_PRESSED_SCALE } else { 1.0 }
            );
            assert_eq!(
                v.sink(2.0),
                if pressed { limits::BUTTON_PRESSED_SINK * 2.0 } else { 0.0 }
            );
        }
    }

    /// 常量自洽：外层恒淡于内层、按下加深并收紧、Q1 保底 / 椭圆几何 / Q4 / Q5。
    #[test]
    fn g4_constants_are_self_consistent() {
        assert!(limits::GLOW_OUTER_HOVER < limits::GLOW_INNER_HOVER);
        assert!(limits::GLOW_OUTER_PRESSED < limits::GLOW_INNER_PRESSED);
        assert!(limits::GLOW_INNER_PRESSED > limits::GLOW_INNER_HOVER);
        assert!(limits::GLOW_GROW_BUTTON > 0.0 && limits::GLOW_GROW_DISMISS > 0.0);
        // Q1：保底 ∈ (0, 0x06]，0x06 是硬上界；按下扩张比例 ∈ (0, 1)
        assert!(limits::GLOW_FLOOR_ALPHA > 0 && limits::GLOW_FLOOR_ALPHA <= 0x06);
        assert!(
            limits::GLOW_GROW_PRESSED_FACTOR > 0.0 && limits::GLOW_GROW_PRESSED_FACTOR < 1.0
        );
        assert_eq!(limits::GLOW_GROW_PRESSED_FACTOR, 0.5);
        // 椭圆几何边界：三键命中区 28×28（半边 14），× 命中区 14×14（半边 7）
        assert!(limits::GLOW_RADIUS_BUTTON + limits::GLOW_GROW_BUTTON <= 14.0);
        assert!(limits::GLOW_RADIUS_DISMISS + limits::GLOW_GROW_DISMISS <= 7.0);
        assert!((0.90..1.0).contains(&limits::BUTTON_PRESSED_SCALE));
        assert!(limits::BUTTON_PRESSED_SINK > 0.0);
        // Q4：本轮只实现 0/1 两个对角朝向；Q5：蒙层暗度微调区间 [0x40, 0x80]
        assert!(limits::COVER_ARROW_ORIENTATION <= 1, "只做 0/1 两个对角朝向");
        assert!((0x40..=0x80).contains(&limits::COVER_HOVER_DIM), "Q5 区间");
        assert_eq!(limits::COVER_DIM_RGB, (0x00, 0x00, 0x00));
        assert_eq!(limits::COVER_ARROW_RGB, (0xFF, 0xFF, 0xFF));
        assert!(limits::COVER_ARROW_ALPHA > 0);
        assert!(limits::COVER_HOVER_DIM > 0 && limits::COVER_HOVER_DIM < 0xC0);
        assert!(limits::COVER_ARROW_SIZE > 0.0 && limits::COVER_ARROW_PAD > 0.0);
    }

    /// alpha 缩放单调不减、Q1 下界恒为 P、满档不超过常量上限。
    #[test]
    fn glow_alpha_scaling_is_monotonic_and_bounded() {
        let p = limits::GLOW_FLOOR_ALPHA;
        // 显式钉住 a=0 的下界为 P，消除「prev=0 隐含归零」的旧假设
        let first = glow_params(true, false, 0);
        assert_eq!(first.inner_alpha, p);
        assert_eq!(first.outer_alpha, p);
        assert_eq!(first.grow_factor, 1.0);

        let (mut prev_i, mut prev_o) = (p, p);
        for a in 0..=255u8 {
            let g = glow_params(true, false, a);
            assert!(g.inner_alpha >= prev_i && g.outer_alpha >= prev_o, "a={a} 不能回退");
            assert!(g.inner_alpha >= p && g.outer_alpha >= p, "a={a} 不得跌破 Q1 保底");
            assert!(
                g.inner_alpha <= limits::GLOW_INNER_HOVER
                    && g.outer_alpha <= limits::GLOW_OUTER_HOVER
            );
            (prev_i, prev_o) = (g.inner_alpha, g.outer_alpha);
        }
        assert_eq!(prev_i, limits::GLOW_INNER_HOVER);
        assert_eq!(prev_o, limits::GLOW_OUTER_HOVER);

        // 按下态走同一条曲线：下界仍是 P、满档不超上限、外圈恒定收紧
        let (mut prev_i, mut prev_o) = (p, p);
        for a in 0..=255u8 {
            let g = glow_params(true, true, a);
            assert!(g.inner_alpha >= prev_i && g.outer_alpha >= prev_o, "a={a} 不能回退");
            assert!(g.inner_alpha >= p && g.outer_alpha >= p, "a={a} 不得跌破 Q1 保底");
            assert!(
                g.inner_alpha <= limits::GLOW_INNER_PRESSED
                    && g.outer_alpha <= limits::GLOW_OUTER_PRESSED
            );
            assert_eq!(g.grow_factor, limits::GLOW_GROW_PRESSED_FACTOR);
            (prev_i, prev_o) = (g.inner_alpha, g.outer_alpha);
        }
        assert_eq!(prev_i, limits::GLOW_INNER_PRESSED);
        assert_eq!(prev_o, limits::GLOW_OUTER_PRESSED);
    }

    /// 4 target × 3 态 × 2 壁纸 × 2 档 = 48 组合的穷举矩阵。
    #[test]
    fn g4_glow_matrix_four_targets_two_wallpapers_two_modes() {
        let p = limits::GLOW_FLOOR_ALPHA;
        let targets = [
            HitTarget::Prev,
            HitTarget::Toggle,
            HitTarget::Next,
            HitTarget::Dismiss,
        ];
        let states: [(bool, bool); 3] = [(false, false), (true, false), (true, true)];
        let accent = crate::media::Rgb::SYSTEM_ACCENT_FALLBACK;
        let mut combos = 0usize;

        for (on_light, luma) in [(true, 0.9f32), (false, 0.02f32)] {
            for mode in [BackgroundMode::Translucent, BackgroundMode::Transparent] {
                let theme = Theme::from_backdrop_on(
                    on_light,
                    luma,
                    accent,
                    mode,
                    BackgroundMode::Transparent,
                    70,
                    50,
                );
                let a = theme.hover_bg.a;
                assert_eq!(
                    a == 0,
                    mode == BackgroundMode::Transparent,
                    "档位与 hover_bg.a 必须一致"
                );
                let (base, halo) = glow_colors(&theme);
                let want_halo = if on_light {
                    Rgba::new(0, 0, 0, 0xFF)
                } else {
                    Rgba::new(0xFF, 0xFF, 0xFF, 0xFF)
                };
                assert_eq!(halo, want_halo, "两壁纸 halo 必须翻转");
                assert_eq!(
                    (base.r, base.g, base.b),
                    (theme.hover_bg.r, theme.hover_bg.g, theme.hover_bg.b),
                    "base 恒取 hover_bg.rgb（含透明档）"
                );

                for &(hovering, pressed) in &states {
                    let reference = glow_params(hovering, pressed, a);
                    if !hovering && !pressed {
                        assert_eq!(reference, GlowParams::new(0, 0, 1.0), "无态全 0");
                    } else if mode == BackgroundMode::Transparent {
                        assert_eq!(reference.inner_alpha, p, "透明档 inner 保底");
                        assert_eq!(reference.outer_alpha, p, "透明档 outer 保底");
                        assert!(p > 0 && p <= 0x06);
                    }
                    // glow_colors 对三态 / 两档恒定
                    assert_eq!(glow_colors(&theme), (base, halo), "颜色与三态/档位无关");

                    for &t in &targets {
                        let mut st = AppState::default();
                        if hovering {
                            st.hover_target = Some(t);
                        }
                        if pressed {
                            st.pressed = Some(t);
                        }
                        let v = ButtonVisual::new(&st, t);
                        assert_eq!(v.hovering, hovering, "{t:?} hovering");
                        assert_eq!(v.pressed, pressed, "{t:?} pressed");
                        let g = glow_params(v.hovering, v.pressed, a);
                        assert_eq!(g, reference, "{t:?} 必须与其它 target 同值");
                        combos += 1;
                    }
                }
            }
        }
        assert_eq!(combos, 48, "4 target × 3 态 × 2 壁纸 × 2 档 = 48");
    }

    /// 结构性保证：函数签名 / 函数体不含 `HitTarget` 分支，四 target 组合恒同值。
    #[test]
    fn glow_params_is_target_agnostic() {
        // 说明：`glow_params(bool, bool, u8) -> GlowParams` 无 HitTarget 参数，
        // 函数体也无 `match target`；四 target 同值靠签名结构保证，不靠调用方自觉。
        let targets = [
            HitTarget::Prev,
            HitTarget::Toggle,
            HitTarget::Next,
            HitTarget::Dismiss,
        ];
        for a in [0u8, 0x40, 0xB3, 0xFF] {
            for (hovering, pressed) in [(false, false), (true, false), (true, true), (false, true)] {
                let want = glow_params(hovering, pressed, a);
                for &t in &targets {
                    let mut st = AppState::default();
                    st.hover_target = if hovering { Some(t) } else { None };
                    st.pressed = if pressed { Some(t) } else { None };
                    let v = ButtonVisual::new(&st, t);
                    assert_eq!(
                        glow_params(v.hovering, v.pressed, a),
                        want,
                        "{t:?} @ a={a} 必须与 target 无关"
                    );
                }
            }
        }
    }

    /// 用 `layout::compute` 实算椭圆几何：不越出各自命中区半边、与「下一首」不重叠。
    #[test]
    fn g4_glow_geometry_stays_inside_hit_and_clear_of_next() {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            for form in [WidgetForm::Compact, WidgetForm::Lyric] {
                let l = compute(TaskbarEdge::Bottom, form, scale, (48.0 * scale).round() as i32);
                let s = l.effective_scale();
                let outer_button = (limits::GLOW_RADIUS_BUTTON + limits::GLOW_GROW_BUTTON) * s;
                let outer_dismiss = (limits::GLOW_RADIUS_DISMISS + limits::GLOW_GROW_DISMISS) * s;
                assert!(outer_button <= 14.0 * s + 1e-3, "三键外圈不得越出 28×28 半边");
                assert!(outer_dismiss <= 7.0 * s + 1e-3, "× 外圈不得越出 14×14 半边");

                // 按下态外圈收紧到一半，同样不越界
                let pressed_button = (limits::GLOW_RADIUS_BUTTON
                    + limits::GLOW_GROW_BUTTON * limits::GLOW_GROW_PRESSED_FACTOR)
                    * s;
                let pressed_dismiss = (limits::GLOW_RADIUS_DISMISS
                    + limits::GLOW_GROW_DISMISS * limits::GLOW_GROW_PRESSED_FACTOR)
                    * s;
                assert!(pressed_button <= 14.0 * s + 1e-3);
                assert!(pressed_dismiss <= 7.0 * s + 1e-3);

                // painter 用的两个中心：三键取 btn_next.center()，× 取 dismiss.center()
                let (nx, ny) = l.btn_next.center();
                let (dx, dy) = l.dismiss.center();
                let gap_x = (dx - nx).abs() - outer_button - outer_dismiss;
                assert!(gap_x > 0.0, "与「下一首」光晕水平净距必须为正：{gap_x}");
                assert!(
                    (gap_x - 2.5 * s).abs() < 0.01,
                    "净距应恰为 2.5*s，实得 {gap_x}"
                );
                let dist = ((dx - nx).powi(2) + (dy - ny).powi(2)).sqrt();
                assert!(dist - outer_button - outer_dismiss > 0.0, "两椭圆不得相交");
            }
        }
    }

    /// Q4：`0` 返回 ↖/↘ 主对角、`1` 返回另一组对角、其它值兜底 0；两 tip 不重合。
    #[test]
    fn cover_arrow_specs_returns_diagonal_pairs() {
        let c = RectF::new(10.0, 20.0, 32.0, 32.0);
        let pad = 3.0;
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;

        let main = cover_arrow_specs_for(c, pad, 0);
        assert!(close(main[0].0, c.x + pad) && close(main[0].1, c.y + pad), "0: 左上 ↖ tip");
        assert!(
            close(main[1].0, c.x + c.w - pad) && close(main[1].1, c.y + c.h - pad),
            "0: 右下 ↘ tip"
        );
        assert!(!main[0].2 && main[1].2, "0 档两支朝向必须相反");

        let other = cover_arrow_specs_for(c, pad, 1);
        assert!(
            close(other[0].0, c.x + pad) && close(other[0].1, c.y + c.h - pad),
            "1: 左下 ↙ tip"
        );
        assert!(
            close(other[1].0, c.x + c.w - pad) && close(other[1].1, c.y + pad),
            "1: 右上 ↗ tip"
        );
        assert_ne!(other[0].2, other[1].2, "1 档两支朝向必须相反");
        assert!(!(close(other[0].0, main[0].0) && close(other[0].1, main[0].1)), "另一组对角");
        assert!(!(close(other[1].0, main[1].0) && close(other[1].1, main[1].1)), "另一组对角");

        // 其它值兜底为 0；公共入口固定取唯一那处具名常量
        assert_eq!(cover_arrow_specs_for(c, pad, 2), main);
        assert_eq!(cover_arrow_specs_for(c, pad, 255), main);
        assert_eq!(
            cover_arrow_specs(c, pad),
            cover_arrow_specs_for(c, pad, limits::COVER_ARROW_ORIENTATION)
        );

        // 两支 tip 不重合
        assert!(!(close(main[0].0, main[1].0) && close(main[0].1, main[1].1)));
        assert!(!(close(other[0].0, other[1].0) && close(other[0].1, other[1].1)));

        // 斜杆轴向：0 = 主对角线 ↖/↘，1 = 另一组 ↗/↙，其它兜底 0
        assert_eq!(arrow_axis(0, false), (1.0, 1.0));
        assert_eq!(arrow_axis(0, true), (-1.0, -1.0));
        assert_eq!(arrow_axis(1, false), (-1.0, 1.0));
        assert_eq!(arrow_axis(1, true), (1.0, -1.0));
        assert_eq!(arrow_axis(99, false), arrow_axis(0, false));
        assert_eq!(arrow_axis(99, true), arrow_axis(0, true));
    }
}
