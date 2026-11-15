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
//! ## 造型
//!
//! 逐值照抄用户拍板的「变体 3 · 双音符」SVG（96 单位设计盒）：
//!
//! ```svg
//! <circle cx="27" cy="64" r="9.5"/>  <circle cx="63" cy="57" r="9.5"/>
//! <rect x="33" y="20" width="5" height="44" rx="2.5"/>
//! <rect x="69" y="13" width="5" height="44" rx="2.5"/>
//! <path d="M33 20 L74 13 L74 22 L33 29 Z"/>
//! <rect x="14" y="80" width="68" height="4" rx="2"/>
//! ```
//!
//! ## 居中口径（用户明确要求）
//!
//! 双音符按**质量重心**水平居中，而不是外框居中——外框居中会让图形看起来偏右。
//! 实测质量重心 `x = 50.011`，盒中心 48，故整体左移 `2.011` 设计单位。
//! 下方横线**保持原图的 14..82**（用户已确认接受"右端比左端伸得多"）。
//!
//! ## 小尺寸
//!
//! 16/20/24px 下符干只有 1.1~1.7 像素宽，按比例加粗（位置与比例不变），
//! 32px 及以上用原权重。系数见 [`weight_factor`]。

/// 设计坐标系边长（源 SVG 的 96 单位盒）。
pub const DESIGN: f32 = 96.0;

/// 符头 A（左、低）：`(cx, cy, r)`。
pub const HEAD_A: (f32, f32, f32) = (27.0, 64.0, 9.5);
/// 符头 B（右、高）。
pub const HEAD_B: (f32, f32, f32) = (63.0, 57.0, 9.5);

/// 符干 A：`(x, y, w, h)`，圆角 = w/2（胶囊）。
pub const STEM_A: (f32, f32, f32, f32) = (33.0, 20.0, 5.0, 44.0);
/// 符干 B。
pub const STEM_B: (f32, f32, f32, f32) = (69.0, 13.0, 5.0, 44.0);

/// 符杠中线起点（设计单位）。
pub const BAR_FROM: (f32, f32) = (33.0, 24.5);
/// 符杠中线终点。
pub const BAR_TO: (f32, f32) = (74.0, 17.5);
/// 符杠厚度（垂直于中线）。
pub const BAR_THICK: f32 = 9.0;

/// 下方横线：`(x, y, w, h)`，圆角 = h/2。
pub const RULE: (f32, f32, f32, f32) = (14.0, 80.0, 68.0, 4.0);

/// 字形包围盒在 **k = 1**（≥32px）时的取值 `(x0, y0, x1, y1)`：横线 14..82、音符 13..84。
///
/// 注意：小尺寸加粗后包围盒会变大（16px → 高 72.2），实际缩放一律用
/// [`ink_bounds`] 现算，不要用这组常量。
pub const BBOX_AT_UNIT_WEIGHT: (f32, f32, f32, f32) = (14.0, 13.0, 82.0, 84.0);

/// 双音符（两符头 + 两符干 + 符杠）的质量重心 x，设计单位。
///
/// 横线**不计入**：它是独立的装饰线，且本身关于盒中心对称。
pub fn mass_centroid_x() -> f32 {
    let pi = core::f32::consts::PI;
    let mut total = 0.0f32;
    let mut acc = 0.0f32;

    for (cx, _cy, r) in [HEAD_A, HEAD_B] {
        let a = pi * r * r;
        total += a;
        acc += a * cx;
    }
    for (x, _y, w, h) in [STEM_A, STEM_B] {
        let a = w * h;
        total += a;
        acc += a * (x + w * 0.5);
    }
    // 符杠是平行四边形：面积 = 底 × 竖直高；重心 = 四顶点均值 = 中线中点。
    let dx = BAR_TO.0 - BAR_FROM.0;
    let dy = BAR_TO.1 - BAR_FROM.1;
    let a = (dx * dx + dy * dy).sqrt() * BAR_THICK;
    total += a;
    acc += a * ((BAR_FROM.0 + BAR_TO.0) * 0.5);

    acc / total
}

/// 让双音符质量重心落在设计盒中心所需的水平平移（负值 = 左移）。
pub fn center_shift_x() -> f32 {
    DESIGN * 0.5 - mass_centroid_x()
}

/// 小尺寸加粗系数（用户拍板的 A 方案：同造型，按尺寸微加粗）。
///
/// 断点之间线性插值，区间外取端点值。24px 是小组件图标态的实际尺寸，
/// 32px 及以上回到原权重。
pub fn weight_factor(px: f32) -> f32 {
    const TABLE: [(f32, f32); 4] = [(16.0, 1.60), (20.0, 1.32), (24.0, 1.12), (32.0, 1.00)];
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

/// 一个图元（设计单位，已含质量重心平移）。
///
/// 故意只用三种基元：Direct2D 侧分别对应 `FillEllipse` / `FillRoundedRectangle`
/// / 剪切后的 `FillRectangle`，不需要引入 `ID2D1PathGeometry`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prim {
    /// 圆（符头）。
    Circle { cx: f32, cy: f32, r: f32 },
    /// 圆角矩形（符干 / 横线），圆角半径 = `r`。
    RoundRect { x: f32, y: f32, w: f32, h: f32, r: f32 },
    /// 平行四边形（符杠）：`(x, y)` 是**左上角**，左右两端垂直，上边斜率为 `shear`。
    ///
    /// 为什么不是「旋转矩形」：源图 `M33 20 L74 13 L74 22 L33 29 Z` 的左右两端是
    /// **竖直**的（x 恒为 33 / 74），旋转矩形的两端会垂直于符杠轴线、斜 9.69°，
    /// 128px 下端点偏 2.23px。用剪切矩阵可以逐值复现原图。
    Beam { x: f32, y: f32, w: f32, h: f32, shear: f32 },
}

/// 符杠上边斜率（`(13−20)/(74−33)`）。平行四边形与剪切矩阵共用。
pub fn beam_shear() -> f32 {
    (BAR_TO.1 - BAR_FROM.1) / (BAR_TO.0 - BAR_FROM.0)
}

/// 按目标像素尺寸产出全部图元（设计单位，已含平移与加粗）。
///
/// 顺序固定：两符头 → 两符干 → 符杠 → 横线。顺序不影响并集结果，
/// 但固定下来可以让单测逐项比对。
pub fn prims(px: f32) -> [Prim; 6] {
    let k = weight_factor(px);
    let sh = center_shift_x();

    // 符头半径随加粗同步微增，否则「粗符干 + 小符头」会看着像两根柱子。
    let head_r = |r: f32| r * (1.0 + 0.08 * (k - 1.0));

    let stem = |s: (f32, f32, f32, f32)| {
        let (x, y, w, h) = s;
        let cx = x + w * 0.5 + sh;
        let nw = w * k;
        Prim::RoundRect {
            x: cx - nw * 0.5,
            y,
            w: nw,
            h,
            r: nw * 0.5,
        }
    };

    let (rx, ry, rw, rh) = RULE;

    [
        Prim::Circle {
            cx: HEAD_A.0 + sh,
            cy: HEAD_A.1,
            r: head_r(HEAD_A.2),
        },
        Prim::Circle {
            cx: HEAD_B.0 + sh,
            cy: HEAD_B.1,
            r: head_r(HEAD_B.2),
        },
        stem(STEM_A),
        stem(STEM_B),
        // 符杠：竖直厚度 9 → 9k（每边外扩 4.5(k−1)），左右两端恒在 x=33 / x=74
        Prim::Beam {
            x: BAR_FROM.0 + sh,
            y: BAR_FROM.1 - 4.5 * (k - 1.0),
            w: BAR_TO.0 - BAR_FROM.0,
            h: BAR_THICK * k,
            shear: beam_shear(),
        },
        Prim::RoundRect {
            x: rx,
            y: ry + rh * 0.5 - rh * k * 0.5,
            w: rw,
            h: rh * k,
            r: rh * k * 0.5,
        },
    ]
}

/// 给定尺寸下的**墨迹包围盒** `(x0, y0, x1, y1)`（设计单位）。
///
/// 必须按加粗后的实际图元算，不能写死 k=1 的 `BBOX`：
/// 16px 时 k=1.60，横线底边从 84 撑到 `80+2+(4k)/2 = 85.2`，整体墨迹高从 71
/// 变成 72.2；用 71 当缩放基准会把横线下缘裁掉 0.25px（ICO 上肉眼可见）。
///
/// 顶边**不受加粗影响**：最上端是符干 B 的 `y = 13`（矩形只横向变粗，y 不动），
/// 符杠加粗后左端上边是 `24.5−4.5(k−1) = 21.8`、右端 `14.8`，仍低于 13。
pub fn ink_bounds(px: f32) -> (f32, f32, f32, f32) {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for p in prims(px) {
        let (a, b, c, d) = match p {
            Prim::Circle { cx, cy, r } => (cx - r, cy - r, cx + r, cy + r),
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

/// 设计单位 → 目标矩形的映射（等比，按包围盒高度对齐，中心对齐）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub scale: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Placement {
    /// 把图标放进 `(x, y, w, h)` 这个矩形。
    ///
    /// 缩放基准取**加粗后的墨迹包围盒**（[`ink_bounds`]），而不是写死的 71：
    /// 小尺寸加粗会把墨迹撑到 72.2，用 71 会裁掉横线下缘。
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
        Prim::Circle { cx, cy, r } => {
            let dx = x - cx;
            let dy = y - cy;
            dx * dx + dy * dy <= r * r
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
    out.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // 分辨率 + 调色板数

    // 像素自下而上（rasterize 已是 BGRA 直通 alpha）
    for y in (0..h).rev() {
        let row = &rgba[y * w * 4..(y + 1) * w * 4];
        out.extend_from_slice(row);
    }
    out.extend_from_slice(&vec![0u8; mask_len]);
    out
}

// ------------------------------------------------------------------ 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 质量重心必须落在设计盒中心 —— 这是用户「不许偏右」要求的可反向验证形式。
    #[test]
    fn mass_centroid_sits_at_design_center_after_shift() {
        let shifted = mass_centroid_x() + center_shift_x();
        assert!(
            (shifted - DESIGN * 0.5).abs() < 1e-4,
            "平移后重心 = {shifted}，必须等于 {}",
            DESIGN * 0.5
        );
    }

    /// 重心数值本身钉死：改任何一个形状的坐标，这条必须先红。
    #[test]
    fn mass_centroid_value_is_pinned() {
        let c = mass_centroid_x();
        assert!((c - 50.011).abs() < 0.01, "质量重心 = {c}，应为 50.011");
        assert!(
            (center_shift_x() + 2.011).abs() < 0.01,
            "平移 = {}",
            center_shift_x()
        );
    }

    /// 栅格化后（真实像素）的重心也要落在画布中心附近。
    ///
    /// 容差 0.35px：24px 画布上抗锯齿会带来不到半像素的偏差，超过即说明
    /// 「设计单位对了但映射错了」（例如按包围盒宽度而不是高度缩放）。
    #[test]
    fn raster_mass_centroid_is_centered_at_24px() {
        let size = 24u32;
        let px = rasterize(size, (0, 0, 0), 8);
        let mut total = 0.0f32;
        let mut sx = 0.0f32;
        for y in 0..size {
            for x in 0..size {
                let a = px[((y * size + x) * 4 + 3) as usize] as f32;
                total += a;
                sx += a * (x as f32 + 0.5);
            }
        }
        let cx = sx / total;
        assert!((cx - 12.0).abs() < 0.35, "24px 实际重心 x = {cx}，应接近 12");
    }

    /// 每个尺寸都必须画出东西（防止缩放系数算成 0 → 空图）。
    #[test]
    fn every_ico_size_has_ink() {
        for sz in ICO_SIZES {
            let px = rasterize(sz, (0, 0, 0), 4);
            let covered = px.chunks(4).filter(|p| p[3] > 0).count();
            let ratio = covered as f32 / (sz * sz) as f32;
            assert!(
                ratio > 0.08 && ratio < 0.65,
                "{sz}px 覆盖率 {ratio:.3} 不合理（应 0.08~0.65）"
            );
        }
    }

    /// ICO 结构自检：头、目录项、偏移都能对上。
    #[test]
    fn ico_header_and_offsets_are_consistent() {
        let sizes = [16u32, 24, 32];
        let bytes = ico_bytes(&sizes, (0, 0, 0), 2);
        assert_eq!(&bytes[0..2], &[0, 0]);
        assert_eq!(&bytes[2..4], &[1, 0]);
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]) as usize, sizes.len());

        for (i, sz) in sizes.iter().enumerate() {
            let e = 6 + i * 16;
            assert_eq!(bytes[e] as u32, *sz, "目录项 {i} 宽度");
            let len = u32::from_le_bytes([bytes[e + 8], bytes[e + 9], bytes[e + 10], bytes[e + 11]]);
            let off = u32::from_le_bytes([bytes[e + 12], bytes[e + 13], bytes[e + 14], bytes[e + 15]]);
            assert!(
                off as usize + len as usize <= bytes.len(),
                "目录项 {i} 越界"
            );
            // BMP 负载以 40 字节的 BITMAPINFOHEADER 开头
            assert_eq!(
                u32::from_le_bytes([
                    bytes[off as usize],
                    bytes[off as usize + 1],
                    bytes[off as usize + 2],
                    bytes[off as usize + 3]
                ]),
                40
            );
        }
    }

    /// 加粗只影响小尺寸：32px 起必须回到原权重。
    #[test]
    fn weight_factor_only_thickens_small_sizes() {
        assert!((weight_factor(32.0) - 1.0).abs() < 1e-6);
        assert!((weight_factor(256.0) - 1.0).abs() < 1e-6);
        assert!(weight_factor(16.0) > weight_factor(24.0));
        assert!(weight_factor(24.0) > weight_factor(32.0));
    }

    /// 加粗不能改变位置：16px 与 256px 的符头/符干中心线必须完全相同。
    #[test]
    fn weight_does_not_move_anything() {
        let small = prims(16.0);
        let large = prims(256.0);
        for i in [0usize, 1] {
            let (a, b) = match (small[i], large[i]) {
                (Prim::Circle { cx: a, .. }, Prim::Circle { cx: b, .. }) => (a, b),
                _ => panic!("前两个图元必须是符头"),
            };
            assert!((a - b).abs() < 1e-5, "符头 {i} 被加粗挪动了：{a} vs {b}");
        }
        for i in [2usize, 3] {
            let (a, b) = match (small[i], large[i]) {
                (Prim::RoundRect { x: ax, w: aw, .. }, Prim::RoundRect { x: bx, w: bw, .. }) => {
                    (ax + aw * 0.5, bx + bw * 0.5)
                }
                _ => panic!("中间两个图元必须是符干"),
            };
            assert!((a - b).abs() < 1e-5, "符干 {i} 被加粗挪动了：{a} vs {b}");
        }
    }

    /// 横线必须严格保持原图的 14..82（用户已确认接受左右不对称，禁止"修正"）。
    #[test]
    fn rule_keeps_the_source_geometry() {
        let p = prims(32.0); // 32px 权重 1.0，与设计单位一一对应
        match p[5] {
            Prim::RoundRect { x, w, .. } => {
                assert!((x - RULE.0).abs() < 1e-5, "横线左端 = {x}，应为 {}", RULE.0);
                assert!((w - RULE.2).abs() < 1e-5, "横线宽 = {w}，应为 {}", RULE.2);
            }
            _ => panic!("第 6 个图元必须是横线"),
        }
    }

    /// 符杠必须是平行四边形：左右两端**竖直**（x 恒为 33 / 74），上边斜率 −7/41。
    #[test]
    fn beam_is_a_parallelogram_with_vertical_ends() {
        let p = prims(32.0);
        let sh = center_shift_x();
        match p[4] {
            Prim::Beam { x, y, w, h, shear } => {
                assert!((x - (BAR_FROM.0 + sh)).abs() < 1e-5, "左端 x = {x}");
                assert!((w - (BAR_TO.0 - BAR_FROM.0)).abs() < 1e-5, "宽 = {w}");
                assert!((y - BAR_FROM.1).abs() < 1e-5, "左端上边 y = {y}");
                assert!((h - BAR_THICK).abs() < 1e-5, "32px 竖直厚度应回到 9");
                let top_right = y + w * shear;
                assert!(
                    (top_right - BAR_TO.1).abs() < 1e-3,
                    "右端上边 y = {top_right}，应为 {}",
                    BAR_TO.1
                );
                assert!((shear + 7.0 / 41.0).abs() < 1e-6, "斜率 = {shear}");
            }
            _ => panic!("第 5 个图元必须是符杠"),
        }
    }

    /// k = 1 时墨迹包围盒必须等于原图的 14 / 13 / 82 / 84。
    #[test]
    fn ink_bounds_at_unit_weight_match_the_source() {
        let (x0, y0, x1, y1) = ink_bounds(32.0);
        assert!((x0 - 14.0).abs() < 1e-4, "左 {x0}");
        assert!((y0 - 13.0).abs() < 1e-3, "上 {y0}");
        assert!((x1 - 82.0).abs() < 1e-4, "右 {x1}");
        assert!((y1 - 84.0).abs() < 1e-4, "下 {y1}");
    }

    /// 加粗会把墨迹撑大（16px 高 72.2），缩放必须跟着它走，否则端点被裁。
    ///
    /// 这条是「ICO 上横线下缘少一行像素」的可反向验证形式：
    /// 把 `Placement::fit` 改回用固定的 71，下面 `bounds_stay_inside_the_canvas` 会红。
    #[test]
    fn ink_bounds_grow_with_weight() {
        let (_, y0, _, y1) = ink_bounds(16.0);
        assert!(((y1 - y0) - 72.2).abs() < 0.01, "16px 墨迹高 = {}", y1 - y0);
        let (_, y0, _, y1) = ink_bounds(32.0);
        assert!(((y1 - y0) - 71.0).abs() < 0.01, "32px 墨迹高 = {}", y1 - y0);
    }

    /// 每个 ICO 尺寸下，墨迹映射后必须严格落在画布内（不许裁边）。
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
}
