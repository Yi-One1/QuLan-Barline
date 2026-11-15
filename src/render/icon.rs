//! 双音符图标的 Direct2D 绘制。
//!
//! **几何不在这里**：造型、质量重心、小尺寸加粗全部来自
//! [`crate::render::icon_geom`]，那个文件同时被 `build.rs` 用来生成 ICO。
//! 本文件只负责把 `Prim` 翻译成 D2D 调用，这样「小组件里的图标」与
//! 「exe / 托盘 / 设置页里的图标」不可能画成两个样。
//!
//! 三种基元对应三种 D2D 调用：
//! - `Prim::Ellipse` → `SetTransform`（绕中心旋转）+ `FillEllipse` + 复位
//! - `Prim::RoundRect` → `FillRoundedRectangle`
//! - `Prim::Beam` → `SetTransform`（竖直剪切）+ `FillRectangle` + 复位
//!
//! 符杠用剪切矩阵而不是「逐行 FillRectangle」：它是唯一带斜边的形状，
//! 逐行拼会在斜边上留下台阶。符头是**旋转椭圆**（长短轴比 1.34、倾角 −45°），
//! 用旋转矩阵而不是把椭圆硬掰成轴对齐——后者在 128px 下轮廓差 3px 以上。

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
                Prim::Ellipse { cx, cy, rx, ry, rot } => {
                    let (dcx, dcy) = (place.x(cx), place.y(cy));
                    rt.SetTransform(&rotation_matrix(dcx, dcy, rot));
                    rt.FillEllipse(
                        &ellipse(dcx, dcy, rx * place.scale, ry * place.scale),
                        &brush,
                    );
                    rt.SetTransform(&Matrix3x2::identity());
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

/// 以 `(cx, cy)` 为中心、旋转 `deg` 度（正值顺时针）的矩阵。
///
/// 符头是旋转椭圆（长短轴比 1.34、倾角 −45°）。手写矩阵而不是用
/// `Matrix3x2::rotation` 之外的东西：D2D 的变换是行向量约定 `[x y 1] × M`，
/// 绕**原点**旋转后再补平移，平移量取「中心 − 旋转后的中心」，
/// 这样中心点严格不动（下面的单测直接验证这一点）。
fn rotation_matrix(cx: f32, cy: f32, deg: f32) -> Matrix3x2 {
    let t = deg.to_radians();
    let (s, c) = (t.sin(), t.cos());
    Matrix3x2 {
        M11: c,
        M12: s,
        M21: -s,
        M22: c,
        M31: cx - (cx * c - cy * s),
        M32: cy - (cx * s + cy * c),
    }
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
        use crate::render::icon_geom::{BAR_SHEAR, BAR_TOP_LEFT, BAR_X0};
        let m = shear_matrix(BAR_X0, BAR_SHEAR);
        let (x, y) = apply(&m, BAR_X0, BAR_TOP_LEFT);
        assert!(
            (x - BAR_X0).abs() < 1e-4 && (y - BAR_TOP_LEFT).abs() < 1e-4,
            "锚点被挪到 ({x}, {y})"
        );
    }

    /// 右端必须上移到符杠上边在该处的 y —— 这就是「两端竖直、上边是斜线」。
    #[test]
    fn shear_matrix_lifts_the_right_end_to_the_beam_top() {
        use crate::render::icon_geom::{bar_top_at, BAR_SHEAR, BAR_TOP_LEFT, BAR_X0, BAR_X1};
        let m = shear_matrix(BAR_X0, BAR_SHEAR);
        let (x, y) = apply(&m, BAR_X1, BAR_TOP_LEFT);
        assert!((x - BAR_X1).abs() < 1e-4, "x 不该被剪切：{x}");
        let want = bar_top_at(BAR_X1);
        assert!((y - want).abs() < 1e-4, "右端 y = {y}，应为 {want}");
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

    /// 旋转矩阵必须让椭圆中心严格不动，且长轴方向被转过去。
    #[test]
    fn rotation_matrix_keeps_the_centre_and_turns_the_major_axis() {
        let m = rotation_matrix(100.0, 50.0, -45.0);
        let (x, y) = apply(&m, 100.0, 50.0);
        assert!((x - 100.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3, "中心被挪到 ({x}, {y})");

        // 长轴（+x 方向）转 −45° 后应指向右上（屏幕坐标 y 向下，所以 y 变小）
        let (ux, uy) = apply(&m, 110.0, 50.0);
        assert!(ux > 100.0, "长轴应指向右侧");
        assert!(uy < 50.0, "长轴应指向右上，实际 y = {uy}");
        // 与 x 轴的夹角应接近 45°
        let ang = (uy - 50.0).atan2(ux - 100.0).to_degrees();
        assert!((ang + 45.0).abs() < 0.5, "长轴倾角 = {ang}°，应为 −45°");
    }

    /// 0° 旋转必须等价于恒等变换（防止矩阵写反）。
    #[test]
    fn rotation_matrix_at_zero_is_identity() {
        let m = rotation_matrix(37.0, 11.0, 0.0);
        for (x, y) in [(0.0f32, 0.0f32), (37.0, 11.0), (90.0, -4.0)] {
            let (ox, oy) = apply(&m, x, y);
            assert!((ox - x).abs() < 1e-4 && (oy - y).abs() < 1e-4, "({x},{y}) -> ({ox},{oy})");
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

    /// 从 `svg` 里找到含 `tag` 的那一段，读它的数值属性。
    fn attr(svg: &str, tag: &str, name: &str) -> f32 {
        let start = svg.find(tag).unwrap_or_else(|| panic!("缺少 {tag}"));
        let seg = &svg[start..];
        let key = format!("{name}=\"");
        let i = seg
            .find(&key)
            .unwrap_or_else(|| panic!("{tag} 缺少属性 {name}"));
        let rest = &seg[i + key.len()..];
        let j = rest.find('"').expect("属性值未闭合");
        rest[..j]
            .parse()
            .unwrap_or_else(|_| panic!("{tag} 的 {name} 不是数字：{}", &rest[..j]))
    }

    /// 把一段字符串里的所有数字抽出来（用于解析 `d` / `rotate(...)`）。
    fn nums(s: &str) -> Vec<f32> {
        s.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter(|t| !t.is_empty() && *t != "-" && *t != ".")
            .filter_map(|t| t.parse().ok())
            .collect()
    }

    fn close(a: f32, b: f32, tol: f32, what: &str) {
        assert!((a - b).abs() <= tol, "{what}：HTML={a} 几何={b}");
    }

    /// 设置页品牌图标必须与 `icon_geom` **同源**。
    ///
    /// 这是「软件所有地方的双音符风格一致」在设置页那一侧的守卫：
    /// 改了 `icon_geom` 而忘了改 HTML（或反过来），这条立刻红。
    #[test]
    fn settings_brand_svg_matches_the_geometry() {
        use crate::render::icon_geom as g;

        let html = include_str!("../../assets/ui/index.html");
        let at = html
            .find("data-icon=\"barline-note\"")
            .expect("品牌图标必须带 data-icon 标记");
        let svg_start = html[..at].rfind("<svg").expect("必须有 svg 开始标签");
        let svg_end = html[at..].find("</svg>").map(|i| at + i).expect("svg 必须闭合");
        let svg = &html[svg_start..svg_end];

        // 1) 两个符头必须是**旋转椭圆**，且半轴/倾角与几何一致
        //    （cx 含 v1.14.2 的视觉居中平移 NOTE_SHIFT_X）
        let first = svg.find("<ellipse").expect("必须有椭圆符头");
        let second = first + svg[first + 1..].find("<ellipse").expect("必须有两个符头") + 1;
        for (i, (tag, head)) in [(&svg[first..], g::HEAD_A), (&svg[second..], g::HEAD_B)]
            .iter()
            .enumerate()
        {
            close(attr(tag, "<ellipse", "cx"), head.0 + g::NOTE_SHIFT_X, 0.02, "符头 cx");
            close(attr(tag, "<ellipse", "cy"), head.1, 0.02, "符头 cy");
            close(attr(tag, "<ellipse", "rx"), head.2, 0.02, "符头 rx");
            close(attr(tag, "<ellipse", "ry"), head.3, 0.02, "符头 ry");
            let rot = nums(&tag[tag.find("rotate(").expect("椭圆必须带 rotate")..]);
            close(rot[0], g::HEAD_ROT_DEG, 0.5, &format!("符头 {} 倾角", i + 1));
        }

        // 2) 符干：顶端必须等于符杠上边（用户指出的「小尖尖」在设置页也不许出现）
        let stems: Vec<&str> = svg
            .match_indices("<rect")
            .map(|(i, _)| &svg[i..])
            .filter(|s| attr(s, "<rect", "rx") < 2.5)
            .collect();
        assert!(stems.len() >= 2, "必须有两条符干");
        for (tag, x, head_cy) in [
            (stems[0], g::STEM_A_X, g::HEAD_A.1),
            (stems[1], g::STEM_B_X, g::HEAD_B.1),
        ] {
            close(attr(tag, "<rect", "x"), x + g::NOTE_SHIFT_X, 0.02, "符干左缘");
            close(attr(tag, "<rect", "width"), g::STEM_W, 0.02, "符干宽");
            let top = attr(tag, "<rect", "y");
            // 平移是横向的，符杠上边在**未平移坐标系**里查表
            let cx = x + g::STEM_W * 0.5;
            close(top, g::bar_top_at(cx), 0.02, "符干顶端必须贴在符杠上边");
            close(top + attr(tag, "<rect", "height"), head_cy, 0.05, "符干底端应到符头中心");
        }

        // 3) 符杠：平行四边形，四个角与几何一致
        let d_start = svg.find("<path d=\"").expect("必须有符杠 path") + 9;
        let d = &svg[d_start..d_start + svg[d_start..].find('"').unwrap()];
        let p = nums(d);
        assert_eq!(p.len(), 8, "符杠路径应有 4 个点");
        let bx0 = g::BAR_X0 + g::NOTE_SHIFT_X;
        let bx1 = g::BAR_X1 + g::NOTE_SHIFT_X;
        let (bt, tr) = (g::BAR_TOP_LEFT, g::bar_top_at(g::BAR_X1));
        for (got, want, what) in [
            (p[0], bx0, "左上 x"),
            (p[1], bt, "左上 y"),
            (p[2], bx1, "右上 x"),
            (p[3], tr, "右上 y"),
            (p[4], bx1, "右下 x"),
            (p[5], tr + g::BAR_THICK, "右下 y"),
            (p[6], bx0, "左下 x"),
            (p[7], bt + g::BAR_THICK, "左下 y"),
        ] {
            close(got, want, 0.05, what);
        }

        // 4) 横线
        let rule = svg
            .match_indices("<rect")
            .map(|(i, _)| &svg[i..])
            .find(|s| attr(s, "<rect", "y") > 80.0)
            .expect("必须有横线");
        close(attr(rule, "<rect", "x"), g::RULE.0, 0.02, "横线 x");
        close(attr(rule, "<rect", "y"), g::RULE.1, 0.02, "横线 y");
        close(attr(rule, "<rect", "width"), g::RULE.2, 0.02, "横线宽");
        close(attr(rule, "<rect", "height"), g::RULE.3, 0.02, "横线厚");

        // 5) viewBox 必须紧贴墨迹包围盒
        let vb = nums(&svg[svg.find("viewBox=\"").expect("必须有 viewBox") + 9..]);
        let (ix0, iy0, ix1, iy1) = g::ink_bounds(32.0);
        close(vb[0], ix0, 0.05, "viewBox x");
        close(vb[1], iy0, 0.05, "viewBox y");
        close(vb[2], ix1 - ix0, 0.05, "viewBox 宽");
        close(vb[3], iy1 - iy0, 0.05, "viewBox 高");

        // 6) 颜色必须跟随主题，不许写死
        assert!(svg.contains("currentColor"), "品牌图标颜色必须用 currentColor");
        assert!(!svg.contains("#2C2C2A"), "品牌图标不许写死颜色");
    }
}
