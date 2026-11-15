//! 双音符图标的 Direct2D 绘制。
//!
//! **几何不在这里**：造型、质量重心、小尺寸加粗全部来自
//! [`crate::render::icon_geom`]，那个文件同时被 `build.rs` 用来生成 ICO。
//! 本文件只负责把 `Prim` 翻译成 D2D 调用，这样「小组件里的图标」与
//! 「exe / 托盘 / 设置页里的图标」不可能画成两个样。
//!
//! 三种基元对应三种 D2D 调用：
//! - `Prim::Circle` → `FillEllipse`
//! - `Prim::RoundRect` → `FillRoundedRectangle`
//! - `Prim::Bar` → `SetTransform`（绕中心旋转）+ `FillRectangle` + 复位
//!
//! 符杠用旋转矩阵而不是「逐行 FillRectangle」：它是唯一带斜边的形状，
//! 逐行拼会在斜边上留下台阶。

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::ID2D1RenderTarget;
use windows_numerics::Matrix3x2;

use crate::render::d2d::{d2d_rect, ellipse, rounded, Frame};
use crate::render::icon_geom::{self, Placement, Prim};
use crate::render::layout::RectF;
use crate::render::theme::Rgba;

/// 把图标画进 `rect`（正方形时正好铺满高度，宽度按包围盒比例 68:71 略窄）。
///
/// `color` 由调用方决定：图标态用主题墨色（随壁纸自适应黑/白），
/// 封面占位用固定白色（叠在强调色渐变上）。
pub fn draw(rt: &ID2D1RenderTarget, frame: &mut Frame, rect: RectF, color: Rgba) -> Result<()> {
    if rect.is_empty() || rect.w <= 0.0 || rect.h <= 0.0 {
        return Ok(());
    }
    let brush = frame.brush(color)?;
    let place = Placement::fit(rect.x, rect.y, rect.w, rect.h);
    // 小尺寸加粗的判定基准取**短边**：图标总是画在正方形里，
    // 但调用方传进来的矩形不保证是正方形。
    let target = rect.w.min(rect.h);

    for prim in icon_geom::prims(target) {
        // SAFETY: 全部是几何图元，坐标由布局与设计常量推导，恒为有限值；
        // brush 由本帧持有，生命周期覆盖调用；SetTransform 在 Bar 分支里成对复位。
        unsafe {
            match prim {
                Prim::Circle { cx, cy, r } => {
                    rt.FillEllipse(
                        &ellipse(
                            place.x(cx),
                            place.y(cy),
                            r * place.scale,
                            r * place.scale,
                        ),
                        &brush,
                    );
                }
                Prim::RoundRect { x, y, w, h, r } => {
                    rt.FillRoundedRectangle(
                        &rounded(
                            place.x(x),
                            place.y(y),
                            w * place.scale,
                            h * place.scale,
                            r * place.scale,
                        ),
                        &brush,
                    );
                }
                Prim::Beam { x, y, w, h, shear } => {
                    let (px0, py0) = (place.x(x), place.y(y));
                    let (pw, ph) = (w * place.scale, h * place.scale);
                    rt.SetTransform(&shear_matrix(px0, shear));
                    rt.FillRectangle(&d2d_rect(px0, py0, pw, ph), &brush);
                    rt.SetTransform(&Matrix3x2::identity());
                }
            }
        }
    }
    Ok(())
}

/// 以 `x0` 那条竖线为不动线的竖直剪切矩阵：`y' = y + shear · (x − x0)`。
///
/// 符杠是**平行四边形**（左右两端竖直），不是旋转矩形，所以用剪切而不是旋转。
/// 手写矩阵而不是用 `Matrix3x2::rotation`：后者的角度单位是**度**，传弧度会得到
/// 一个几乎不转的符杠，而这种错误在 D2D 调用里看不出来（单测跑不了渲染），
/// 只能靠这条纯函数把它钉死。
fn shear_matrix(x0: f32, shear: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: 1.0,
        M12: shear,
        M21: 0.0,
        M22: 1.0,
        M31: 0.0,
        M32: -x0 * shear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把矩阵作用到点上（D2D 行向量约定：`[x y 1] × M`）。
    fn apply(m: &Matrix3x2, x: f32, y: f32) -> (f32, f32) {
        (
            x * m.M11 + y * m.M21 + m.M31,
            x * m.M12 + y * m.M22 + m.M32,
        )
    }

    /// 剪切不动线（符杠左端）必须原地不动。
    #[test]
    fn shear_matrix_keeps_its_anchor_line_fixed() {
        let m = shear_matrix(33.0, -7.0 / 41.0);
        let (x, y) = apply(&m, 33.0, 20.0);
        assert!((x - 33.0).abs() < 1e-4 && (y - 20.0).abs() < 1e-4, "锚点被挪到 ({x}, {y})");
    }

    /// 右端（x = 74）必须上移 7 —— 这正是源图 `L74 13` 的位置。
    #[test]
    fn shear_matrix_lifts_the_right_end_by_seven_units() {
        let m = shear_matrix(33.0, -7.0 / 41.0);
        let (x, y) = apply(&m, 74.0, 20.0);
        assert!((x - 74.0).abs() < 1e-4, "x 不该被剪切：{x}");
        assert!((y - 13.0).abs() < 1e-4, "右端 y = {y}，应为 13");
    }

    /// 剪切只改 y，不改 x（平行四边形左右两端竖直）。
    #[test]
    fn shear_matrix_never_moves_x() {
        let m = shear_matrix(10.0, -0.1707);
        for x in [10.0f32, 30.0, 51.0] {
            let (ox, _) = apply(&m, x, 5.0);
            assert!((ox - x).abs() < 1e-5, "x 被改成了 {ox}");
        }
    }

    /// 接线锁：`paint()` 必须在画封面/文字之前分流到图标态。
    ///
    /// 这是**源码静态断言**——把图标态分支删掉时，所有纯函数单测仍然全绿
    /// （布局有图标矩形、几何算得对），只有这条会红。
    #[test]
    fn paint_branches_to_icon_state_before_drawing_the_widget() {
        let src = include_str!("painter.rs");
        let branch = src
            .find("form.is_icon()")
            .expect("painter 必须判图标态，否则图标态画不出来");
        let cover = src
            .find("draw_cover(&rt, frame, covers, slot, ctx)")
            .expect("必须有封面绘制");
        assert!(branch < cover, "图标态分流必须排在封面绘制之前");
        assert!(src.contains("fn draw_icon_state("), "必须有 draw_icon_state");
        assert!(
            src.contains("crate::render::icon::draw"),
            "封面缺图占位必须复用同一份几何，不许另画一套"
        );
    }

    /// 设置页品牌图标必须与 `icon_geom` **同源**：同一组坐标 + 同一个质量重心平移。
    ///
    /// 这是「软件所有地方的双音符风格一致」在设置页那一侧的守卫：
    /// 改了 `icon_geom` 而忘了改 HTML（或反过来），这条立刻红。
    #[test]
    fn settings_brand_svg_matches_the_geometry() {
        let html = include_str!("../../assets/ui/index.html");
        let at = html
            .find("data-icon=\"barline-note\"")
            .expect("品牌图标必须带 data-icon 标记");
        let svg_start = html[..at].rfind("<svg").expect("必须有 svg 开始标签");
        let svg_end = html[at..].find("</svg>").map(|i| at + i).expect("svg 必须闭合");
        let svg = &html[svg_start..svg_end];

        // 平移量必须等于 icon_geom 算出来的值
        let shift = format!(
            "translate({:.3} 0)",
            crate::render::icon_geom::center_shift_x()
        );
        assert!(
            svg.contains(&shift),
            "品牌图标的平移量应为 {shift}（质量重心居中），实际：{svg}"
        );

        // 六个子形状逐值比对（与源 SVG 一字不差）
        for needle in [
            "cx=\"27\" cy=\"64\" r=\"9.5\"",
            "cx=\"63\" cy=\"57\" r=\"9.5\"",
            "x=\"33\" y=\"20\" width=\"5\" height=\"44\" rx=\"2.5\"",
            "x=\"69\" y=\"13\" width=\"5\" height=\"44\" rx=\"2.5\"",
            "M33 20 L74 13 L74 22 L33 29 Z",
            "x=\"14\" y=\"80\" width=\"68\" height=\"4\" rx=\"2\"",
        ] {
            assert!(svg.contains(needle), "品牌图标缺少形状 {needle}");
        }

        // 颜色必须跟随主题，不许写死（用户要求「黑白、随主题」）
        assert!(svg.contains("currentColor"), "品牌图标颜色必须用 currentColor");
        assert!(!svg.contains("#2C2C2A"), "品牌图标不许写死颜色");
    }
}
