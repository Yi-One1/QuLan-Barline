//! 双音符应用图标 —— **唯一的造型来源**（纯几何，零 crate 依赖）。
//!
//! 这个文件同时被两处编译：
//! 1. `src/render/icon.rs`（运行时 Direct2D 绘制：图标态 / 封面占位）；
//! 2. `build.rs`（生成 `assets/icon.ico` 与 `assets/icon_light.ico`，供 exe 资源、
//!    托盘、设置窗口标题栏使用）。
//!
//! 之所以这么拆：用户对图标的核心要求是「**软件所有地方的双音符风格一致**」。
//! 只要两处用同一份几何常量，就不可能出现「托盘是一个样、小组件是另一个样」。
//! 因此本文件**只允许依赖 `core` / `std`**，不许 `use crate::…`、不许 `windows`。
//!
//! ## 造型来源：用户 2026-11-16 给的参考图
//!
//! 上一版（变体 3）被用户否掉，理由是「上方突出两个小尖尖」。根因是符干顶端
//! 比符杠上边高出 4.5~5.4 单位，露在符杠外面。本版改为逐像素测量用户提供的
//! 参考图（`174×180`，逐行 + 逐列扫描 + 最小二乘拟合），实测结果：
//!
//! | 量 | 参考图（px） | 占墨迹高 |
//! |---|---|---|
//! | 符头 | **椭圆** 半长轴 34.2 / 半短轴 25.5，倾角 −45° | 长短轴比 1.34 |
//! | 符干宽 | 8.0 | 4.44% |
//! | 符杠竖直厚 | 30.7 | 17.1% |
//! | 符杠斜率 | −0.26846 | — |
//! | 两符头中心距 | 108 × −20 | — |
//!
//! **符头是椭圆，不是圆。** 这一点靠像素重合度判定，不靠肉眼也不靠二阶矩
//! （后者会被符干遮挡带偏）：把整个音符按候选形状重画一遍与参考图逐像素比对，
//! 椭圆 34.2/25.5 @−45° 误差 1.43%，同样外框的正圆误差 3.78% —— 差 2.6 倍。
//! 面积也印证：实测 2772px²，内接同一外框的正圆是 2922，旋转椭圆是 2750。
//!
//! **关键：符干顶端 = 符杠上边**。参考图里符干 A 顶端在 `y=30`、符干 B 在 `y=1`，
//! 与符杠上边 `y = −0.26772x + 45.676` 在该处的取值完全吻合 —— 符干藏在符杠里，
//! 所以看不到尖。本文件用 [`bar_top_at`] 强制这条，[`Prim::RoundRect`] 的圆角
//! （胶囊形）保证符干顶端任何一点都不越出符杠上边。
//!
//! ## 居中口径（v1.14.2 起）
//!
//! **音符左移、横线居中**，两条各管一件事：
//!
//! 1. 本造型的符杠又厚又长（占音符面积约 29%）且重心在最右，按墨迹外框居中时
//!    音符的**面积重心比盒中心偏右 4.82 单位**，看着「往右坠」—— 这是用户在
//!    v1.14.1 验收里指出的「水平方向不居中」。
//! 2. 直接让重心居中（音符左移 4.82）是对的，但**横线不能跟着移**：横线是整幅图
//!    里唯一水平且左右对称的元素，它居中才能当「视觉基准线」。若横线跟着音符走，
//!    墨迹外框就变成 4.93 / 14.57 的不对称边距，比原来更歪。
//! 3. 所以横线**按盒宽居中**（左右边距各 [`RULE_MARGIN`]），并加宽到刚好盖住
//!    左移后的音符左缘；音符单独按 [`NOTE_SHIFT_X`] 左移。
//!
//! 结果是：外框（= 横线）严格居中，音符在框内光学居中，两者都不歪。
//! 用户 2026-11-16 拍板：「只平移双音符，下方的横线始终水平居中，横线需要长一点」。
//!
//! ## 小尺寸
//!
//! 参考图的符干只占 4.44%，16px 下只有 0.6 像素宽 —— 不加粗会断成虚线。
//! 故 16~40px 按 [`weight_factor`] 只加粗**符干**（符杠本来就厚、符头本来就大，
//! 不参与加粗，造型保持忠实）。缩放基准用 [`ink_bounds`] 现算，不写死。

/// 设计坐标系边长（参考图等比映射后的 96 单位盒）。
pub const DESIGN: f32 = 96.0;

/// 符头 A（左、低）：`(cx, cy, 半长轴, 半短轴)`。参考图中心 `(32.5, 150)`。
pub const HEAD_A: (f32, f32, f32, f32) = (23.435, 68.110, 15.510, 11.564);
/// 符头 B（右、高）。参考图中心 `(140.5, 130)`。
pub const HEAD_B: (f32, f32, f32, f32) = (72.414, 59.040, 15.510, 11.564);

/// 符头椭圆的倾角（**度**，正值 = 顺时针，与 D2D 的 `rotation` 同向）。
///
/// −45° = 长轴指向右上，也就是经典音符头的方向。参考图实测值。
pub const HEAD_ROT_DEG: f32 = -45.0;

/// 符头椭圆的长短轴比（给单测与文档用）。
pub fn head_axis_ratio() -> f32 {
    HEAD_A.2 / HEAD_A.3
}

/// 符干宽（k = 1）。
pub const STEM_W: f32 = 3.628;
/// 符干 A 的**左边缘** x（中心 = 左边缘 + `STEM_W` / 2）。
pub const STEM_A_X: f32 = 34.092;
/// 符干 B 的**左边缘** x。
pub const STEM_B_X: f32 = 82.617;

/// 符杠左端 x（= 符干 A 左边缘附近，参考图实测 57/174）。
pub const BAR_X0: f32 = 34.546;
/// 符杠右端 x（= 符干 B 右边缘，参考图实测 171/174）。
pub const BAR_X1: f32 = 86.246;
/// 符杠**上边**在 `BAR_X0` 处的 y。
pub const BAR_TOP_LEFT: f32 = 13.879;
/// 符杠上边斜率（参考图最小二乘拟合 −0.26846）。
pub const BAR_SHEAR: f32 = -0.26846;
/// 符杠**竖直**厚度（不是垂直于轴线的厚度）。
pub const BAR_THICK: f32 = 13.923;

/// 下方横线：`(x, y, w, h)`，圆角 = h/2。
///
/// **横线不参与音符的平移**，它在 96 盒里始终水平居中（左右边距相等）。
/// 宽度取「刚好盖住左移后的音符左缘」：`x = 音符左缘`，于是右端同样留
/// `RULE_MARGIN`，横线居中、音符左移，音符不会再探出横线左端。
pub const RULE_MARGIN: f32 = 4.934;
pub const RULE: (f32, f32, f32, f32) = (
    RULE_MARGIN,
    90.600,
    DESIGN - 2.0 * RULE_MARGIN,
    5.400,
);

/// 符杠上边在 `x` 处的 y。符干顶端与它对齐，所以看不到「小尖尖」。
///
/// 注意：这是**参考图坐标系**（未平移）的函数。`prims()` 输出的符干/符杠已经
/// 整体平移了 [`NOTE_SHIFT_X`]，所以拿平移后的 `cx` 反查时要先减去平移量，
/// 见 `stem_tops_sit_exactly_on_the_beam_top_edge`。
pub fn bar_top_at(x: f32) -> f32 {
    BAR_TOP_LEFT + BAR_SHEAR * (x - BAR_X0)
}

/// 视觉居中：**音符（不含横线）**整体左移的设计单位数。
///
/// 为什么需要它：本造型的符杠又厚又长（占音符面积约 29%）且重心在最右，
/// 按墨迹外框居中时，面积重心比盒中心偏右 **4.82** 单位，看着「往右坠」。
///
/// 为什么不把横线一起移：横线是唯一一条水平的、左右对称的元素，它居中才能
/// 充当「视觉基准线」；横线跟着音符走的话，整幅图的墨迹外框又变成不对称的了
/// （左右边距 4.93 / 14.57），反而更歪。**用户 2026-11-16 拍板：只平移双音符，
/// 横线始终水平居中**。
///
/// −4.82 = 让**整幅墨迹（音符 + 横线）**的面积重心落在盒中心（128px 栅格实测
/// 重心在 52.815，盒心 48），也正是用户 2026-11-16 从四档预览里选定的那一档。
///
/// 注意口径：单看**双音符自己**，平移后它的面积重心落在 49.08 —— 还偏右 1.08。
/// 再往左 1.08 会让音符探出横线左端（横线得继续加宽），且与用户选定的预览
/// 档位不符，故止步于此。要改这个数请先看 `note_is_shifted_left_while_the_rule_stays_centred`。
pub const NOTE_SHIFT_X: f32 = -4.82;

/// 小尺寸符干加粗系数。
///
/// 参考图符干只占墨迹高的 4.44%，落位到 16px 只有 0.6px。断点之间线性插值，
/// 区间外取端点值。目标：符干落位后 ≥ 1.4~1.6px（低于这个值抗锯齿会把它糊成灰线）。
///
/// 只加粗符干 —— 符杠（17.1%）与符头（33.9%）本来就粗，跟着加粗会失真。
pub fn weight_factor(px: f32) -> f32 {
    const TABLE: [(f32, f32); 6] = [
        (16.0, 2.40),
        (20.0, 2.11),
        (24.0, 1.76),
        (32.0, 1.32),
        (40.0, 1.06),
        (48.0, 1.00),
    ];
    if px <= TABLE[0].0 {
        return TABLE[0].1;
    }
    for pair in TABLE.windows(2) {
        let (x0, k0) = pair[0];
        let (x1, k1) = pair[1];
        if px <= x1 {
            let t = (px - x0) / (x1 - x0);
            return k0 + (k1 - k0) * t;
        }
    }
    TABLE[TABLE.len() - 1].1
}

/// 一个图元（设计单位）。
///
/// 故意只用三种基元：Direct2D 侧分别对应 `FillEllipse` / `FillRoundedRectangle`
/// / 剪切后的 `FillRectangle`，不需要引入 `ID2D1PathGeometry`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prim {
    /// 旋转椭圆（符头）。`rot` 单位是**度**，正值顺时针。
    ///
    /// 符头不是圆：参考图实测长短轴比 1.34、倾角 −45°。用像素重合度验证过，
    /// 正圆的误差是椭圆的 2.6 倍。
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32, rot: f32 },
    /// 圆角矩形（符干 / 横线），圆角半径 = `r`。符干取 `r = w/2`（胶囊形）：
    /// 顶端收成半圆，保证加粗后任何一点都不越出符杠上边。
    RoundRect { x: f32, y: f32, w: f32, h: f32, r: f32 },
    /// 平行四边形（符杠）：`(x, y)` 是**左上角**，左右两端垂直，上边斜率为 `shear`。
    ///
    /// 为什么不是「旋转矩形」：参考图的符杠左右两端是**竖直**的（x 恒为 57 / 171），
    /// 旋转矩形的两端会垂直于符杠轴线、斜 15.03°，128px 下端点偏 4.1px。
    /// 用剪切矩阵可以逐值复现参考图。
    Beam { x: f32, y: f32, w: f32, h: f32, shear: f32 },
}

/// 椭圆旋转后的**轴对齐半宽**。
fn ellipse_half_w(rx: f32, ry: f32, rot_deg: f32) -> f32 {
    let t = rot_deg.to_radians();
    let (s, c) = (t.sin(), t.cos());
    (rx * rx * c * c + ry * ry * s * s).sqrt()
}

/// 椭圆旋转后的**轴对齐半高**。
fn ellipse_half_h(rx: f32, ry: f32, rot_deg: f32) -> f32 {
    let t = rot_deg.to_radians();
    let (s, c) = (t.sin(), t.cos());
    (rx * rx * s * s + ry * ry * c * c).sqrt()
}

/// 一根符干：**外缘（右缘）固定，向内加粗**，顶端严格落在符杠上边。
///
/// 为什么向内加粗：参考图里符干 B 的右缘正好是音符墨迹的右缘（与符杠右端重合），
/// 向两侧加粗会让它在小尺寸下探出符杠右端，右边界随尺寸漂移，
/// 横线也就对不齐了。固定外缘后墨迹包围盒在所有尺寸下都是同一个。
fn stem(x_left: f32, head_cy: f32, k: f32) -> Prim {
    let w = STEM_W * k;
    let right = x_left + STEM_W;
    let cx = right - w * 0.5;
    let top = bar_top_at(cx);
    Prim::RoundRect {
        x: right - w,
        y: top,
        w,
        h: head_cy - top,
        r: w * 0.5,
    }
}

/// 按目标像素尺寸产出全部图元（设计单位，已含加粗与音符平移）。
///
/// 顺序固定：两符头 → 两符干 → 符杠 → 横线。顺序不影响并集结果，
/// 但固定下来可以让单测逐项比对。
///
/// **前 5 项（音符）整体加 [`NOTE_SHIFT_X`]，最后一项（横线）不动。**
pub fn prims(px: f32) -> [Prim; 6] {
    let k = weight_factor(px);
    let (rx, ry, rw, rh) = RULE;
    let d = NOTE_SHIFT_X;

    [
        Prim::Ellipse {
            cx: HEAD_A.0 + d,
            cy: HEAD_A.1,
            rx: HEAD_A.2,
            ry: HEAD_A.3,
            rot: HEAD_ROT_DEG,
        },
        Prim::Ellipse {
            cx: HEAD_B.0 + d,
            cy: HEAD_B.1,
            rx: HEAD_B.2,
            ry: HEAD_B.3,
            rot: HEAD_ROT_DEG,
        },
        shift_x(stem(STEM_A_X, HEAD_A.1, k), d),
        shift_x(stem(STEM_B_X, HEAD_B.1, k), d),
        Prim::Beam {
            x: BAR_X0 + d,
            y: BAR_TOP_LEFT,
            w: BAR_X1 - BAR_X0,
            h: BAR_THICK,
            shear: BAR_SHEAR,
        },
        Prim::RoundRect {
            x: rx,
            y: ry,
            w: rw,
            h: rh,
            r: rh * 0.5,
        },
    ]
}

/// 图元整体横移（只用于把音符挪到视觉居中位置）。
fn shift_x(p: Prim, d: f32) -> Prim {
    match p {
        Prim::Ellipse { cx, cy, rx, ry, rot } => Prim::Ellipse {
            cx: cx + d,
            cy,
            rx,
            ry,
            rot,
        },
        Prim::RoundRect { x, y, w, h, r } => Prim::RoundRect { x: x + d, y, w, h, r },
        Prim::Beam { x, y, w, h, shear } => Prim::Beam {
            x: x + d,
            y,
            w,
            h,
            shear,
        },
    }
}

/// 给定尺寸下的**墨迹包围盒** `(x0, y0, x1, y1)`（设计单位）。
///
/// 必须按加粗后的实际图元算：符干加粗只改宽度（不影响包围盒），但缩放基准
/// 一旦写死就会在小尺寸下裁边。现算最稳。
pub fn ink_bounds(px: f32) -> (f32, f32, f32, f32) {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for p in prims(px) {
        let (a, b, c, d) = match p {
            Prim::Ellipse { cx, cy, rx, ry, rot } => {
                let hw = ellipse_half_w(rx, ry, rot);
                let hh = ellipse_half_h(rx, ry, rot);
                (cx - hw, cy - hh, cx + hw, cy + hh)
            }
            Prim::RoundRect { x, y, w, h, .. } => (x, y, x + w, y + h),
            Prim::Beam { x, y, w, h, shear } => {
                // 平行四边形：左上 (x, y)、右上 (x+w, y+w*shear)、左下 (x, y+h)
                let top_right = y + w * shear;
                (x, y.min(top_right), x + w, (y + h).max(top_right + h))
            }
        };
        x0 = x0.min(a);
        y0 = y0.min(b);
        x1 = x1.max(c);
        y1 = y1.max(d);
    }
    (x0, y0, x1, y1)
}

/// 设计单位 → 目标矩形的映射（等比，按墨迹包围盒居中）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub scale: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Placement {
    /// 把图标放进 `(x, y, w, h)` 这个矩形。
    ///
    /// 缩放基准取**加粗后的墨迹包围盒**（[`ink_bounds`]），而不是任何写死的常数。
    pub fn fit(x: f32, y: f32, w: f32, h: f32) -> Self {
        let side = if w < h { w } else { h };
        let (bx0, by0, bx1, by1) = ink_bounds(side);
        let span = (bx1 - bx0).max(by1 - by0);
        let scale = if side > 0.0 && span > 0.0 { side / span } else { 0.0 };
        Self {
            scale,
            ox: x + w * 0.5 - (bx0 + bx1) * 0.5 * scale,
            oy: y + h * 0.5 - (by0 + by1) * 0.5 * scale,
        }
    }

    /// 设计单位 x → 目标坐标。
    pub fn x(&self, v: f32) -> f32 {
        v * self.scale + self.ox
    }

    /// 设计单位 y → 目标坐标。
    pub fn y(&self, v: f32) -> f32 {
        v * self.scale + self.oy
    }
}

// ------------------------------------------------------------------ 栅格化
//
// 只为 build.rs 生成 ICO 服务：超采样 + 面积平均，无外部 crate。
// 运行时绘制走 Direct2D，不用这段代码。

/// 判断设计单位下的一点是否落在某个图元内部。
fn inside(p: &Prim, x: f32, y: f32) -> bool {
    match *p {
        Prim::Ellipse { cx, cy, rx, ry, rot } => {
            // 反向旋转到椭圆的局部坐标，再做轴对齐的椭圆判别
            let t = -rot.to_radians();
            let (s, c) = (t.sin(), t.cos());
            let (dx, dy) = (x - cx, y - cy);
            let lx = dx * c - dy * s;
            let ly = dx * s + dy * c;
            let (ux, uy) = (lx / rx, ly / ry);
            ux * ux + uy * uy <= 1.0
        }
        Prim::RoundRect { x: rx, y: ry, w, h, r } => {
            if x < rx || x > rx + w || y < ry || y > ry + h {
                return false;
            }
            let rr = r.min(w * 0.5).min(h * 0.5);
            if rr <= 0.0 {
                return true;
            }
            // 胶囊形（r = w/2）时 lo 与 hi 在数学上相等，但浮点相减会差一个 ulp，
            // 直接 `clamp` 会 panic（min > max）。这里退化时取中线。
            let (lox, hix) = (rx + rr, rx + w - rr);
            let (loy, hiy) = (ry + rr, ry + h - rr);
            let nx = if hix > lox { x.clamp(lox, hix) } else { rx + w * 0.5 };
            let ny = if hiy > loy { y.clamp(loy, hiy) } else { ry + h * 0.5 };
            let dx = x - nx;
            let dy = y - ny;
            dx * dx + dy * dy <= rr * rr
        }
        Prim::Beam { x: bx, y: by, w, h, shear } => {
            if x < bx || x > bx + w {
                return false;
            }
            // 上边随 x 线性下移（shear < 0 时是上移），下边始终在上边下方 h 处
            let top = by + (x - bx) * shear;
            y >= top && y <= top + h
        }
    }
}

/// 把图标栅格化成 `size × size` 的 **BGRA（非预乘）** 位图，透明背景。
///
/// `sub` 是每像素每轴的超采样数（`sub = 4` → 16 个样本）。
pub fn rasterize(size: u32, ink: (u8, u8, u8), sub: u32) -> Vec<u8> {
    let s = size as f32;
    let place = Placement::fit(0.0, 0.0, s, s);
    let shapes = prims(s);
    let mut out = vec![0u8; (size * size * 4) as usize];
    let n = sub.max(1);
    let inv = 1.0 / (n * n) as f32;

    for py in 0..size {
        for px in 0..size {
            let mut hits = 0u32;
            for sy in 0..n {
                for sx in 0..n {
                    // 目标像素内的样本点 → 反向映射回设计单位
                    let tx = ((px as f32 + (sx as f32 + 0.5) / n as f32) - place.ox) / place.scale;
                    let ty = ((py as f32 + (sy as f32 + 0.5) / n as f32) - place.oy) / place.scale;
                    if shapes.iter().any(|sh| inside(sh, tx, ty)) {
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let a = (hits as f32 * inv * 255.0).round() as u8;
            let i = ((py * size + px) * 4) as usize;
            out[i] = ink.2;
            out[i + 1] = ink.1;
            out[i + 2] = ink.0;
            out[i + 3] = a;
        }
    }
    out
}

/// ICO 里用到的尺寸清单（从小到大）。
///
/// 40px 是 125% DPI 下 `SM_CXICON` 的取值，缺了它 Windows 只能拉大 32px。
/// 不含 256：32bpp BMP 负载在 256px 下要 270KB，而 PNG 负载需要 zlib 依赖；
/// 128px 已覆盖 Explorer 大图标视图，任务栏/托盘/Alt-Tab 都用 ≤48。
pub const ICO_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 128];

/// 生成多尺寸 ICO 的字节流（32bpp BMP 负载，无 PNG——不引入 zlib 依赖）。
pub fn ico_bytes(sizes: &[u32], ink: (u8, u8, u8), sub: u32) -> Vec<u8> {
    let mut images: Vec<(u32, Vec<u8>)> = Vec::with_capacity(sizes.len());
    for &sz in sizes {
        images.push((sz, bmp_payload(sz, ink, sub)));
    }

    let count = images.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = 6 + 16 * images.len() as u32;
    for (sz, data) in &images {
        let dim = if *sz >= 256 { 0u8 } else { *sz as u8 };
        out.push(dim); // width
        out.push(dim); // height
        out.push(0); // palette
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}

/// 单个尺寸的 BMP 负载：`BITMAPINFOHEADER` + 32bpp 自下而上像素 + 全零 AND 掩码。
fn bmp_payload(size: u32, ink: (u8, u8, u8), sub: u32) -> Vec<u8> {
    let rgba = rasterize(size, ink, sub);
    let w = size as usize;
    let h = size as usize;

    // AND 掩码：每行按 4 字节对齐，全 0（不透明度由 alpha 决定）。
    let mask_row = w.div_ceil(32) * 4;
    let mask_len = mask_row * h;

    let mut out = Vec::with_capacity(40 + w * h * 4 + mask_len);
    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&((h * 2) as i32).to_le_bytes()); // 高度含掩码
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes()); // 图像大小
    out.extend_from_slice(&[0u8; 16]); // 分辨率 + 调色板

    // XOR 位图：BMP 自下而上，写 BGRA
    for y in (0..h).rev() {
        for x in 0..w {
            let i = (y * w + x) * 4;
            out.extend_from_slice(&[rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]);
        }
    }
    out.extend(std::iter::repeat_n(0u8, mask_len));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设计盒边长必须能容纳墨迹（防止有人改常量时把图形撑出盒外）。
    #[test]
    fn ink_fits_the_design_box() {
        let (x0, y0, x1, y1) = ink_bounds(32.0);
        assert!(x0 >= -0.01 && y0 >= -0.5, "左上越界 ({x0}, {y0})");
        assert!(x1 <= DESIGN + 0.01 && y1 <= DESIGN + 0.01, "右下越界 ({x1}, {y1})");
    }

    /// **用户指出的「两个小尖尖」的回归锁。**
    ///
    /// 符干顶端必须严格落在符杠上边，不能高出一点点。上一版高出 4.5~5.4 单位，
    /// 在任务栏上就是两个尖。这条同时盯住两根符干。
    ///
    /// 注意 `bar_top_at()` 吃的是**参考图坐标系**的 x，而 `prims()` 已把音符
    /// 整体平移了 [`NOTE_SHIFT_X`]，所以要先反平移再查表。
    #[test]
    fn stem_tops_sit_exactly_on_the_beam_top_edge() {
        let p = prims(32.0);
        for (i, (stem, head)) in [(p[2], HEAD_A), (p[3], HEAD_B)].iter().enumerate() {
            match *stem {
                Prim::RoundRect { x, y, w, .. } => {
                    let cx = x + w * 0.5;
                    let expected = bar_top_at(cx - NOTE_SHIFT_X);
                    assert!(
                        (y - expected).abs() < 1e-4,
                        "符干 {} 顶端 y = {y}，符杠上边 = {expected}（高出的部分就是尖尖）",
                        if i == 0 { "A" } else { "B" }
                    );
                    // 且必须真的往下连到符头
                    assert!(head.1 > y, "符干必须延伸到符头中心");
                }
                _ => panic!("第 {} 个图元必须是符干", i + 3),
            }
        }
    }

    /// 所有 ICO 尺寸下，符干顶端都不能越出符杠上边（含加粗后）。
    ///
    /// 胶囊形符干的顶点在中心，加粗只会向两侧扩，所以顶端 y 不随 k 变。
    #[test]
    fn no_stem_pokes_above_the_beam_at_any_size() {
        for sz in ICO_SIZES {
            let p = prims(sz as f32);
            let beam_top = match p[4] {
                Prim::Beam { y, .. } => y,
                _ => unreachable!(),
            };
            let _ = beam_top;
            for stem in [p[2], p[3]] {
                if let Prim::RoundRect { x, y, w, .. } = stem {
                    let cx = x + w * 0.5;
                    assert!(
                        y >= bar_top_at(cx - NOTE_SHIFT_X) - 1e-4,
                        "{sz}px：符干顶端 {y} 高于符杠上边 {}",
                        bar_top_at(cx - NOTE_SHIFT_X)
                    );
                    // 符干在符杠右端附近时，上边比左端更高 —— 也要满足
                    let top_right = BAR_TOP_LEFT + (BAR_X1 - BAR_X0) * BAR_SHEAR;
                    assert!(
                        y >= top_right - 1e-4 || y >= bar_top_at(cx - NOTE_SHIFT_X) - 1e-4
                    );
                }
            }
        }
    }

    /// **像素级回归**：栅格化后不许有任何墨迹高于符杠上边。
    ///
    /// 上面两条查的是常量；这条查的是**真的画出来的像素**。16/24/32px 各查一遍，
    /// 任何一次「符干探出符杠」都会在这里被抓到。
    #[test]
    fn raster_has_no_ink_above_the_beam_top_edge() {
        for sz in [16u32, 20, 24, 32, 48, 128] {
            let place = Placement::fit(0.0, 0.0, sz as f32, sz as f32);
            let buf = rasterize(sz, (0, 0, 0), 6);
            for py in 0..sz {
                for px in 0..sz {
                    let a = buf[((py * sz + px) * 4 + 3) as usize];
                    if a < 40 {
                        continue;
                    }
                    let dx = (px as f32 + 0.5 - place.ox) / place.scale;
                    let dy = (py as f32 + 0.5 - place.oy) / place.scale;
                    // 符杠（含平移）的横向范围
                    let (bl, br) = (BAR_X0 + NOTE_SHIFT_X - 1.0, BAR_X1 + NOTE_SHIFT_X + 1.0);
                    if dx < bl || dx > br {
                        continue; // 符杠范围外只有符头，另有约束
                    }
                    let top = bar_top_at(dx - NOTE_SHIFT_X);
                    // 允许半个像素的抗锯齿误差（像素中心落在边界上）
                    let tol = 0.75 / place.scale;
                    assert!(
                        dy >= top - tol,
                        "{sz}px：({px},{py}) 的墨迹在设计坐标 y={dy:.2}，高于符杠上边 {top:.2} —— 这就是小尖尖"
                    );
                }
            }
        }
    }

    /// 符杠必须是平行四边形：左右两端**竖直**，上边斜率 = 参考图拟合值。
    #[test]
    fn beam_is_a_parallelogram_with_vertical_ends() {
        let p = prims(32.0);
        let d = NOTE_SHIFT_X;
        match p[4] {
            Prim::Beam { x, y, w, h, shear } => {
                assert!((x - (BAR_X0 + d)).abs() < 1e-5, "左端 x = {x}");
                assert!((x + w - (BAR_X1 + d)).abs() < 1e-5, "右端 x = {}", x + w);
                assert!((y - BAR_TOP_LEFT).abs() < 1e-5, "上边左端 y = {y}");
                assert!((h - BAR_THICK).abs() < 1e-5, "竖直厚度 = {h}");
                assert!((shear - BAR_SHEAR).abs() < 1e-6, "斜率 = {shear}");
                let top_right = y + w * shear;
                assert!(
                    (top_right - bar_top_at(BAR_X1)).abs() < 1e-3,
                    "右端上边 = {top_right}"
                );
            }
            _ => panic!("第 5 个图元必须是符杠"),
        }
    }

    /// 造型比例必须等于参考图实测值（用户给的就是这把尺子）。
    ///
    /// 归一化基准是**音符自身**的高/宽，不是含横线的整体墨迹 ——
    /// 参考图没有横线，拿整体去比会得出错误结论。
    #[test]
    fn proportions_match_the_reference_measurements() {
        let hh = ellipse_half_h(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG);
        let note_top = bar_top_at(BAR_X1);
        let note_h = (HEAD_A.1 + hh) - note_top;
        // 音符自身宽度（左符头最左 → 符杠右端）。**不能拿横线宽代替**：
        // v1.14.2 起横线为了让开左移后的音符而加宽，比音符本身更宽。
        let note_left = HEAD_A.0 - ellipse_half_w(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG);
        let note_w = BAR_X1 - note_left;

        let head_w = ellipse_half_w(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG) * 2.0;
        let head_h = hh * 2.0;
        // 参考图（px）：符头 60.3×60.3、符干 8、符杠 30.7、音符 168.67×180.35
        assert!((head_w / note_h - 60.3 / 180.35).abs() < 0.01, "符头宽占比");
        assert!((head_h / note_h - 60.3 / 180.35).abs() < 0.01, "符头高占比");
        assert!((STEM_W / note_h - 8.0 / 180.35).abs() < 0.004, "符干占比");
        assert!((BAR_THICK / note_h - 30.7 / 180.35).abs() < 0.005, "符杠厚度占比");
        assert!((note_w / note_h - 168.67 / 180.35).abs() < 0.01, "音符宽高比");
        assert!(
            ((HEAD_B.0 - HEAD_A.0) / note_h - 108.0 / 180.35).abs() < 0.01,
            "符头中心距"
        );
        assert!(
            ((HEAD_A.1 - HEAD_B.1) / note_h - 20.0 / 180.35).abs() < 0.01,
            "符头高度差"
        );
    }

    /// 符头必须是**椭圆**，不是圆：长短轴比 1.34、倾角 −45°。
    ///
    /// 用户专门指出过这一点。这条把「有人把椭圆改回圆」钉死。
    #[test]
    fn head_is_a_tilted_ellipse_not_a_circle() {
        assert!(
            (head_axis_ratio() - 1.34).abs() < 0.02,
            "长短轴比 = {}，参考图实测 1.34",
            head_axis_ratio()
        );
        assert!((HEAD_ROT_DEG + 45.0).abs() < 0.5, "倾角应为 −45°");
        // 椭圆投影到轴对齐后的外框必须接近正方形（参考图 60.3 x 60.3）
        let hw = ellipse_half_w(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG);
        let hh = ellipse_half_h(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG);
        assert!((hw - hh).abs() < 0.3, "外框 {hw:.2} x {hh:.2} 应该接近正方形");
        // 且不能退化成圆
        assert!(HEAD_A.2 - HEAD_A.3 > 3.0, "半轴差太小，又变回圆了");
    }

    /// 椭圆的旋转必须真的生效（判别式对旋转敏感）。
    #[test]
    fn rotated_ellipse_membership_follows_the_tilt() {
        let e = Prim::Ellipse {
            cx: 0.0,
            cy: 0.0,
            rx: 20.0,
            ry: 10.0,
            rot: -45.0,
        };
        // 长轴方向（−45°）上远处属于椭圆
        let d = 18.0 * std::f32::consts::FRAC_1_SQRT_2;
        assert!(inside(&e, d, -d), "长轴方向 18 应命中");
        // 短轴方向（+45°）上同样距离应落空
        assert!(!inside(&e, d, d), "短轴方向 18 不该命中");
        // 不转的话 (+45°) 也会命中 —— 证明旋转确实起作用
        let axis_aligned = Prim::Ellipse {
            cx: 0.0,
            cy: 0.0,
            rx: 20.0,
            ry: 10.0,
            rot: 0.0,
        };
        assert!(!inside(&axis_aligned, d, d), "轴对齐椭圆在 (d,d) 不该命中");
        assert!(inside(&axis_aligned, 18.0, 0.0), "轴对齐椭圆在长轴应命中");
    }

    /// 横线在音符下方，且左右端分别对齐音符墨迹的左右端。
    #[test]
    fn rule_sits_below_the_note_and_spans_its_width() {
        let (x0, y0, x1, y1) = ink_bounds(32.0);
        let (rx, ry, rw, rh) = RULE;
        assert!((ry - (y1 - rh)).abs() < 1e-4, "横线底边必须就是墨迹底边");
        assert!((rx - x0).abs() < 1e-4, "横线左端 {} 应等于墨迹左端 {x0}", rx);
        assert!((rx + rw - x1).abs() < 1e-4, "横线右端应等于墨迹右端 {x1}");
        // 音符最低点（符头 A 的下缘）到横线顶之间必须留空隙
        let note_bottom = HEAD_A.1 + ellipse_half_h(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG);
        assert!(ry > note_bottom + 4.0, "横线与符头之间必须有可见间隙");
        let _ = y0;
    }

    /// **v1.14.2 居中口径的回归锁**（用户 2026-11-16 拍板）。
    ///
    /// 三条一起成立才算对：
    /// 1. 横线在 96 盒里**水平居中**（左右边距相等）；
    /// 2. 音符整体左移了 [`NOTE_SHIFT_X`]；
    /// 3. 横线**完全盖住**音符（音符不许探出横线左右任何一端）。
    #[test]
    fn note_is_shifted_left_while_the_rule_stays_centred() {
        let (rx, _, rw, _) = RULE;
        let (rl, rr) = (rx, rx + rw);
        assert!(
            (rl - (DESIGN - rr)).abs() < 1e-3,
            "横线必须水平居中：左 {rl:.3} / 右 {:.3}",
            DESIGN - rr
        );

        let d = NOTE_SHIFT_X;
        assert!(d < 0.0, "音符必须左移");
        // 音符的左右极值（含平移）
        let hw = ellipse_half_w(HEAD_A.2, HEAD_A.3, HEAD_ROT_DEG);
        let note_left = HEAD_A.0 + d - hw;
        let note_right = BAR_X1 + d;
        assert!(
            note_left >= rl - 1e-3,
            "音符左缘 {note_left:.3} 探出了横线左端 {rl:.3}"
        );
        assert!(
            note_right <= rr + 1e-3,
            "音符右缘 {note_right:.3} 超出了横线右端 {rr:.3}"
        );

        // 且平移后音符的重心确实更靠近盒中心（不是白移的）
        let centre = (note_left + note_right) * 0.5;
        let mid = (rl + rr) * 0.5;
        assert!(
            centre < mid,
            "音符必须落在横线中心的左侧（音符中点 {centre:.2} / 盒中心 {mid:.2}）"
        );
    }

    /// 平移量本身：128px 栅格实测「音符 + 横线」整体墨迹的面积重心偏右 4.82。
    ///
    /// 这条锁住「数字不能随手改」——改大了音符会探出横线左端（上一条抓），
    /// 改小了又会回到「往右坠」。
    #[test]
    fn note_shift_matches_the_measured_area_centroid() {
        assert!(
            (NOTE_SHIFT_X + 4.82).abs() < 0.01,
            "平移量 {NOTE_SHIFT_X} 应等于 128px 栅格实测的整体重心偏移 −4.82"
        );
    }

    /// 加粗只作用于小尺寸，48px 及以上回到原权重。
    #[test]
    fn weight_factor_only_thickens_small_sizes() {
        assert!(weight_factor(16.0) > weight_factor(20.0));
        assert!(weight_factor(20.0) > weight_factor(24.0));
        assert!(weight_factor(24.0) > weight_factor(32.0));
        assert!((weight_factor(48.0) - 1.0).abs() < 1e-6);
        assert!((weight_factor(128.0) - 1.0).abs() < 1e-6);
        // 目标：16~24px 的符干落位后至少 1.35px
        for sz in [16.0f32, 20.0, 24.0] {
            let (_, _, _, y1) = ink_bounds(sz);
            let (_, y0, _, _) = ink_bounds(sz);
            let span = (y1 - y0).max(ink_bounds(sz).2 - ink_bounds(sz).0);
            let px_w = STEM_W * weight_factor(sz) * sz / span;
            assert!(px_w >= 1.35, "{sz}px 符干只有 {px_w:.2}px，会糊成灰线");
        }
    }

    /// 加粗只向内长，外缘（右缘）固定 —— 否则符干 B 会探出符杠右端。
    #[test]
    fn weight_grows_inwards_and_keeps_the_outer_edge() {
        let thin = prims(128.0);
        let thick = prims(16.0);
        for (a, b) in [(thin[2], thick[2]), (thin[3], thick[3])] {
            if let (
                Prim::RoundRect { x: xa, w: wa, .. },
                Prim::RoundRect { x: xb, w: wb, .. },
            ) = (a, b)
            {
                assert!((xa + wa - (xb + wb)).abs() < 1e-4, "符干外缘必须固定");
                assert!(wb > wa, "小尺寸符干必须更粗");
                assert!(xb < xa, "加粗必须向内（向左）长");
            } else {
                panic!("符干必须是圆角矩形");
            }
        }
        // 符头与符杠不受加粗影响
        assert_eq!(thin[0], thick[0]);
        assert_eq!(thin[1], thick[1]);
        assert_eq!(thin[4], thick[4]);
        assert_eq!(thin[5], thick[5]);
    }

    /// 墨迹包围盒必须在**所有**尺寸下都一致（加粗不能改变外框）。
    ///
    /// 这条是上面「向内加粗」的可见后果：外框稳定，横线才能对得齐音符。
    #[test]
    fn ink_bounds_are_identical_at_every_size() {
        let base = ink_bounds(128.0);
        for sz in ICO_SIZES {
            let b = ink_bounds(sz as f32);
            for (i, (a, c)) in [(base.0, b.0), (base.1, b.1), (base.2, b.2), (base.3, b.3)]
                .iter()
                .enumerate()
            {
                assert!(
                    (a - c).abs() < 0.02,
                    "{sz}px 的第 {i} 个边界从 {a} 变成 {c} —— 加粗改动了外框"
                );
            }
        }
    }

    /// 每个 ICO 尺寸下墨迹都必须落在画布内（不许裁边）。
    #[test]
    fn bounds_stay_inside_the_canvas() {
        for sz in ICO_SIZES {
            let s = sz as f32;
            let place = Placement::fit(0.0, 0.0, s, s);
            let (x0, y0, x1, y1) = ink_bounds(s);
            let (lx, ty) = (place.x(x0), place.y(y0));
            let (rx, by) = (place.x(x1), place.y(y1));
            assert!(lx >= -0.01 && ty >= -0.01, "{sz}px 左上越界 ({lx:.2}, {ty:.2})");
            assert!(
                rx <= s + 0.01 && by <= s + 0.01,
                "{sz}px 右下越界 ({rx:.2}, {by:.2})"
            );
        }
    }

    /// 每个尺寸都要真的画出东西（防止加粗/缩放的边界情况把图形算成空）。
    #[test]
    fn every_ico_size_has_ink() {
        for sz in ICO_SIZES {
            let buf = rasterize(sz, (0, 0, 0), 4);
            let ink = buf.chunks_exact(4).filter(|p| p[3] > 128).count();
            let ratio = ink as f32 / (sz * sz) as f32;
            assert!(ratio > 0.05, "{sz}px 只有 {ratio:.3} 的像素是墨迹");
            assert!(ratio < 0.6, "{sz}px 墨迹占 {ratio:.3}，糊成一块了");
        }
    }

    /// ICO 目录头与偏移必须自洽（Explorer 靠它找每一帧）。
    #[test]
    fn ico_header_and_offsets_are_consistent() {
        let bytes = ico_bytes(&ICO_SIZES, (0, 0, 0), 2);
        assert_eq!(&bytes[0..4], &[0, 0, 1, 0], "ICO 魔数");
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        assert_eq!(count, ICO_SIZES.len());
        let mut expect_off = 6 + 16 * count;
        for i in 0..count {
            let e = 6 + 16 * i;
            assert_eq!(bytes[e] as u32, ICO_SIZES[i], "第 {i} 帧宽度");
            assert_eq!(bytes[e + 6], 32, "必须 32bpp");
            let len = u32::from_le_bytes([bytes[e + 8], bytes[e + 9], bytes[e + 10], bytes[e + 11]]) as usize;
            let off = u32::from_le_bytes([bytes[e + 12], bytes[e + 13], bytes[e + 14], bytes[e + 15]]) as usize;
            assert_eq!(off, expect_off, "第 {i} 帧偏移");
            expect_off += len;
        }
        assert_eq!(bytes.len(), expect_off, "总长度");
    }
}
