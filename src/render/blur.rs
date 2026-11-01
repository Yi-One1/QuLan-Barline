//! 自研 box blur（`ARCHITECTURE.md` §11.3 第 4 步）。
//!
//! **为什么不用 DWM acrylic**：`SetWindowCompositionAttribute` 的
//! `ACCENT_ENABLE_ACRYLICBLURBEHIND` 要求窗口有 DWM 背景，
//! 而 `WS_EX_LAYERED` + `UpdateLayeredWindow` 的窗口没有（难点 H2）。
//! 只能在 CPU 上自己做。
//!
//! 实现要点：
//! - **可分离**：先横后纵，把 O(w·h·r) 降到 O(w·h)。
//! - **滑动窗口整数累加**：每个像素只做一次加法一次减法，没有乘法。
//! - **3 次 box ≈ 高斯**：中心极限定理，3 遍已经足够接近 acrylic 的观感。
//! - 边缘用「钳位采样」（把越界坐标夹到边界），避免出现黑边。
//!
//! 数据布局统一为紧密排列的 4 通道字节数组（BGRA），alpha 通道一并模糊
//! （我们的输入是块平均色，alpha 恒为 255，模糊后仍为 255）。

/// 对 `buf`（`w * h * 4` 字节，BGRA）原地做 `passes` 次可分离 box blur。
///
/// `radius` 以像素为单位，必须 ≥ 0。`radius == 0` 时直接返回。
pub fn box_blur(bgra: &mut [u8], w: u32, h: u32, radius: i32, passes: u32) {
    if radius <= 0 || passes == 0 || w == 0 || h == 0 {
        return;
    }
    let (wu, hu) = (w as usize, h as usize);
    if bgra.len() < wu * hu * 4 {
        return;
    }
    let mut scratch = vec![0u8; wu * hu * 4];
    for _ in 0..passes {
        blur_horizontal(bgra, &mut scratch, wu, hu, radius as usize);
        blur_vertical(&scratch, bgra, wu, hu, radius as usize);
    }
}

/// 水平方向滑动窗口。
fn blur_horizontal(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u32;
    // 四舍五入而不是截断：截断在 3 遍 × 2 方向后会累积可见的能量损失
    // （小值像素反复被截成 0），四舍五入是无偏的。
    let half = window / 2;
    for y in 0..h {
        let row = y * w * 4;
        let mut acc = [0u32; 4];
        // 初始化窗口（左边界钳位）
        for i in 0..=(2 * r) {
            let x = i.saturating_sub(r).min(w - 1);
            let p = row + x * 4;
            for c in 0..4 {
                acc[c] += src[p + c] as u32;
            }
        }
        for x in 0..w {
            let o = row + x * 4;
            for c in 0..4 {
                dst[o + c] = ((acc[c] + half) / window) as u8;
            }
            // 滑出 x - r，滑入 x + r + 1（两端钳位）
            let out = (x).saturating_sub(r).min(w - 1);
            let inp = (x + r + 1).min(w - 1);
            let po = row + out * 4;
            let pi = row + inp * 4;
            for c in 0..4 {
                acc[c] = acc[c] + src[pi + c] as u32 - src[po + c] as u32;
            }
        }
    }
}

/// 垂直方向滑动窗口。
fn blur_vertical(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let window = (2 * r + 1) as u32;
    let half = window / 2;
    for x in 0..w {
        let col = x * 4;
        let mut acc = [0u32; 4];
        for i in 0..=(2 * r) {
            let y = i.saturating_sub(r).min(h - 1);
            let p = y * w * 4 + col;
            for c in 0..4 {
                acc[c] += src[p + c] as u32;
            }
        }
        for y in 0..h {
            let o = y * w * 4 + col;
            for c in 0..4 {
                dst[o + c] = ((acc[c] + half) / window) as u8;
            }
            let out = (y).saturating_sub(r).min(h - 1);
            let inp = (y + r + 1).min(h - 1);
            let po = out * w * 4 + col;
            let pi = inp * w * 4 + col;
            for c in 0..4 {
                acc[c] = acc[c] + src[pi + c] as u32 - src[po + c] as u32;
            }
        }
    }
}

/// 双线性放大：把 `sw × sh` 的 BGRA 放大到 `dw × dh`。
///
/// 输出 `dst` 必须已经分配好 `dw * dh * 4` 字节。
pub fn upsample_bilinear(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    if dst.len() < (dw as usize) * (dh as usize) * 4 {
        return;
    }
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    for y in 0..dh {
        // 用像素中心对齐，避免整体偏移半像素
        let fy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).max(0.0);
        let y0 = (fy as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let ty = fy - y0 as f32;
        for x in 0..dw {
            let fx = ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).max(0.0);
            let x0 = (fx as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let tx = fx - x0 as f32;

            let p00 = (y0 * sw + x0) * 4;
            let p10 = (y0 * sw + x1) * 4;
            let p01 = (y1 * sw + x0) * 4;
            let p11 = (y1 * sw + x1) * 4;
            let o = (y * dw + x) * 4;
            for c in 0..4 {
                let top = src[p00 + c] as f32 * (1.0 - tx) + src[p10 + c] as f32 * tx;
                let bot = src[p01 + c] as f32 * (1.0 - tx) + src[p11 + c] as f32 * tx;
                let v = top * (1.0 - ty) + bot * ty;
                dst[o + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// 用「同行 / 同列最近的有效值」填充空洞（§11.3 第 2 步）。
///
/// `valid[i] == false` 的格子会被替换成最近的 `valid` 格子的颜色。
/// 两遍扫描（先横后纵）即可覆盖绝大多数空洞；完全无有效值时保持原样。
pub fn fill_holes(bgra: &mut [u8], w: u32, h: u32, valid: &[bool]) {
    let (wu, hu) = (w as usize, h as usize);
    if wu == 0 || hu == 0 || valid.len() < wu * hu {
        return;
    }
    if !valid.iter().any(|v| *v) {
        return;
    }

    // 第一遍：水平方向，每个空洞取左 / 右最近的有效值（距离近者优先）
    for y in 0..hu {
        let row = y * wu;
        let mut last_valid: Option<usize> = None;
        for x in 0..wu {
            if valid[row + x] {
                last_valid = Some(x);
            } else if let Some(lv) = last_valid {
                let s = (row + lv) * 4;
                let d = (row + x) * 4;
                let px = [bgra[s], bgra[s + 1], bgra[s + 2], bgra[s + 3]];
                bgra[d..d + 4].copy_from_slice(&px);
            }
        }
        // 从右往左补左边没覆盖到的
        let mut next_valid: Option<usize> = None;
        for x in (0..wu).rev() {
            if valid[row + x] {
                next_valid = Some(x);
            } else if last_valid.is_none() {
                if let Some(nv) = next_valid {
                    let s = (row + nv) * 4;
                    let d = (row + x) * 4;
                    let px = [bgra[s], bgra[s + 1], bgra[s + 2], bgra[s + 3]];
                    bgra[d..d + 4].copy_from_slice(&px);
                }
            }
        }
    }

    // 第二遍：垂直方向，处理「整行都没有有效值」的极端情况
    for x in 0..wu {
        let mut last_valid: Option<usize> = None;
        for y in 0..hu {
            let idx = y * wu + x;
            if valid[idx] {
                last_valid = Some(y);
            } else if let Some(ly) = last_valid {
                let s = (ly * wu + x) * 4;
                let d = idx * 4;
                let px = [bgra[s], bgra[s + 1], bgra[s + 2], bgra[s + 3]];
                bgra[d..d + 4].copy_from_slice(&px);
            }
        }
        let mut next_valid: Option<usize> = None;
        for y in (0..hu).rev() {
            let idx = y * wu + x;
            if valid[idx] {
                next_valid = Some(y);
            } else if last_valid.is_none() {
                if let Some(ny) = next_valid {
                    let s = (ny * wu + x) * 4;
                    let d = idx * 4;
                    let px = [bgra[s], bgra[s + 1], bgra[s + 2], bgra[s + 3]];
                    bgra[d..d + 4].copy_from_slice(&px);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        let mut v = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            v[i * 4..i * 4 + 4].copy_from_slice(&px);
        }
        v
    }

    /// 常量图进 → 常量图出（box blur 的归一化必须严格成立）。
    #[test]
    fn constant_image_stays_constant() {
        let mut buf = solid(17, 9, [40, 80, 120, 255]);
        box_blur(&mut buf, 17, 9, 4, 3);
        for i in 0..(17 * 9) as usize {
            assert_eq!(&buf[i * 4..i * 4 + 4], &[40, 80, 120, 255], "第 {i} 个像素被改变");
        }
    }

    /// 单点脉冲的能量必须守恒。
    ///
    /// 用 41×41 的图并把脉冲放在正中心：3 遍半径 3 的模糊只会扩散到 ±9 像素，
    /// 离边界还有 11 像素，因此**边界钳位不会引入额外能量**。
    /// 剩下的误差只来自整数四舍五入（约 ±5%）。
    #[test]
    fn impulse_preserves_total_energy() {
        let (w, h) = (41u32, 41u32);
        let mut buf = solid(w, h, [0, 0, 0, 0]);
        let center = ((20 * w + 20) * 4) as usize;
        buf[center] = 255;
        buf[center + 1] = 255;
        buf[center + 2] = 255;

        box_blur(&mut buf, w, h, 3, 3);

        let total: u64 = (0..(w * h) as usize).map(|i| buf[i * 4] as u64).sum();
        assert!(
            (215..=300).contains(&total),
            "脉冲总能量 {total} 应接近 255（±15%）"
        );
        // 中心必须仍是最大值
        let max = (0..(w * h) as usize).map(|i| buf[i * 4]).max().unwrap_or(0);
        assert_eq!(buf[center], max);
        // 能量确实扩散开了（不是原地不动）
        let nonzero = (0..(w * h) as usize).filter(|i| buf[i * 4] > 0).count();
        assert!(nonzero > 20, "只扩散到 {nonzero} 个像素，模糊没生效");
    }

    #[test]
    fn zero_radius_is_noop() {
        let mut buf = solid(5, 5, [1, 2, 3, 255]);
        let before = buf.clone();
        box_blur(&mut buf, 5, 5, 0, 3);
        assert_eq!(buf, before);
    }

    #[test]
    fn upsample_produces_requested_size() {
        let src = solid(4, 2, [10, 20, 30, 255]);
        let mut dst = vec![0u8; 16 * 8 * 4];
        upsample_bilinear(&src, 4, 2, &mut dst, 16, 8);
        assert_eq!(&dst[0..4], &[10, 20, 30, 255]);
        assert_eq!(&dst[dst.len() - 4..], &[10, 20, 30, 255]);
    }

    #[test]
    fn fill_holes_replaces_invalid_cells() {
        // 5×1，中间一格无效，左右都是有效值 → 被填成相邻颜色
        let mut buf = vec![0u8; 5 * 1 * 4];
        for x in 0..5 {
            let c = if x < 2 { 10 } else { 200 };
            buf[x * 4] = c;
            buf[x * 4 + 3] = 255;
        }
        let valid = [true, true, false, true, true];
        fill_holes(&mut buf, 5, 1, &valid);
        // 中间格取左侧最近有效值（10）
        assert_eq!(buf[2 * 4], 10);
    }

    #[test]
    fn fill_holes_without_any_valid_is_noop() {
        let mut buf = vec![7u8; 3 * 3 * 4];
        let before = buf.clone();
        fill_holes(&mut buf, 3, 3, &[false; 9]);
        assert_eq!(buf, before);
    }
}
