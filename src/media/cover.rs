//! 封面解码与主色提取（`ARCHITECTURE.md` §9.7 / §11.4 / T06 第 4–5 条）。
//!
//! 完整链路：
//! ```text
//! props.Thumbnail()                        -> IRandomAccessStreamReference
//!   .OpenReadAsync().join()                -> IRandomAccessStreamWithContentType
//!   DataReader::CreateDataReader + LoadAsync(size).join() + ReadBytes
//!                                          -> Vec<u8>（原始图片字节）
//! SHCreateMemStream                        -> IStream
//! wic.CreateDecoderFromStream              -> IWICBitmapDecoder
//!   .GetFrame(0)                           -> IWICBitmapFrameDecode
//!   CreateBitmapScaler -> Initialize(64,64, Fant)
//!   CreateFormatConverter -> GUID_WICPixelFormat32bppPBGRA   ← 必须预乘！
//!   CopyPixels                             -> Vec<u8>（64×64 PBGRA）
//! ```
//!
//! **格式转换必须到 `GUID_WICPixelFormat32bppPBGRA`**：D2D 的渲染目标是预乘 alpha，
//! 喂 straight alpha 的位图会让封面颜色发灰（架构 §9.3 明确警告）。
//!
//! 注意：`OpenReadAsync()` 返回 `IAsyncOperation`，**必须在 MTA 线程 `.join()`**。
//! 本模块只会在媒体线程（T1，MTA）里被调用。
//!
//! ## 圆角在解码期做掉
//!
//! 封面视觉上要 4px 圆角（32px 边长 → 半径比例 1/8）。如果放在 painter 里做，
//! 每帧都要 `PushLayer` + 几何遮罩（`D2D1_LAYER_PARAMETERS` 在 windows 0.62 里是
//! `ManuallyDrop<Option<ID2D1Geometry>>`，手写容易漏 Release）。
//! 改成解码时把 64×64 图的四角 alpha 清零——比例固定，一次做好，渲染期零成本。

use std::sync::Arc;

use windows::core::{Interface, Result};
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties as Props;
use windows::Storage::Streams::{DataReader, IRandomAccessStream};
use windows::Win32::Graphics::Imaging::{
    IWICBitmapDecoder, IWICBitmapFrameDecode, IWICFormatConverter, IWICImagingFactory,
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnLoad,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::SHCreateMemStream;

use crate::config::defaults::limits;
use crate::media::{CoverImage, Rgb};
use crate::platform::com::timed_join;
use crate::render::theme::{normalize_accent, relative_luminance};

/// 缩略图字节数上限。超过视为异常（正常封面 20–800 KB）。
const MAX_THUMB_BYTES: u64 = 8 * 1024 * 1024;

/// 从媒体属性里取封面。任何失败都返回 `None`（渲染层画渐变占位，不阻塞 UI）。
pub fn load_cover(props: &Props) -> Option<Arc<CoverImage>> {
    match decode(props) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            crate::log_debug!("封面解码失败：{e}");
            None
        }
    }
}

fn decode(props: &Props) -> Result<CoverImage> {
    let bytes = read_thumbnail_bytes(props)?;
    if bytes.is_empty() {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_UNEXPECTED,
            "缩略图字节为空",
        ));
    }

    // SAFETY: 所有 COM 对象都在本函数作用域内创建与释放；
    // 传入的字节切片生命周期覆盖调用（SHCreateMemStream 会拷贝）。
    unsafe {
        let stream = SHCreateMemStream(Some(&bytes)).ok_or_else(|| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_OUTOFMEMORY,
                "SHCreateMemStream 失败",
            )
        })?;

        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;

        let decoder: IWICBitmapDecoder =
            wic.CreateDecoderFromStream(&stream, core::ptr::null(), WICDecodeMetadataCacheOnLoad)?;
        let frame: IWICBitmapFrameDecode = decoder.GetFrame(0)?;

        let side = limits::COVER_DECODE_PX;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&frame, side, side, WICBitmapInterpolationModeFant)?;

        let conv: IWICFormatConverter = wic.CreateFormatConverter()?;
        conv.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;

        let stride = side * 4;
        let mut bgra = vec![0u8; (stride * side) as usize];
        conv.CopyPixels(core::ptr::null(), stride, &mut bgra)?;

        // 圆角遮罩（解码期做掉，渲染期零成本）
        apply_rounded_mask(&mut bgra, side, side);

        let dominant = extract_dominant(&bgra, side, side);

        Ok(CoverImage {
            width: side,
            height: side,
            bgra,
            dominant,
        })
    }
}

/// `Thumbnail().OpenReadAsync().join()` → `DataReader` → `Vec<u8>`。
fn read_thumbnail_bytes(props: &Props) -> Result<Vec<u8>> {
    let reference = props.Thumbnail()?;
    let with_type =
        timed_join("Thumbnail.OpenReadAsync", || reference.OpenReadAsync()?.join())?;
    // `IRandomAccessStreamWithContentType` 继承 `IRandomAccessStream`，
    // windows-rs 不把基接口方法挂到派生接口上，必须显式 cast。
    let stream: IRandomAccessStream = with_type.cast()?;

    let size = stream.Size()?;
    if size == 0 || size > MAX_THUMB_BYTES {
        crate::log_debug!("缩略图尺寸异常：{size} 字节，跳过");
        return Ok(Vec::new());
    }

    let reader = DataReader::CreateDataReader(&stream)?;
    // SAFETY: LoadAsync 返回的操作在本线程（MTA）join。
    let loaded = timed_join("DataReader.LoadAsync", || {
        reader.LoadAsync(size as u32)?.join()
    })?;
    let n = loaded.min(size as u32) as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; n];
    reader.ReadBytes(&mut buf)?;
    Ok(buf)
}

/// 把四角 alpha 清零。半径比例 = `COVER_RADIUS / COVER_SIDE`（4/32 = 1/8）。
fn apply_rounded_mask(bgra: &mut [u8], w: u32, h: u32) {
    let ratio = limits::COVER_RADIUS / limits::COVER_SIDE;
    let r = (w as f32 * ratio).max(1.0);
    for y in 0..h {
        for x in 0..w {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if !crate::render::sampler::inside_rounded(px, py, w as f32, h as f32, r) {
                let p = ((y * w + x) * 4) as usize;
                if p + 3 < bgra.len() {
                    bgra[p] = 0;
                    bgra[p + 1] = 0;
                    bgra[p + 2] = 0;
                    bgra[p + 3] = 0;
                }
            }
        }
    }
}

/// 主色提取（§11.4）：12 个色相桶 + 饱和度 / 明度过滤 + 权重 `S*V` + 亮度归一化。
pub fn extract_dominant(bgra: &[u8], w: u32, h: u32) -> Rgb {
    /// 12 个桶：累加权重、加权 RGB
    struct Bucket {
        weight: f32,
        r: f32,
        g: f32,
        b: f32,
    }
    let mut buckets: Vec<Bucket> = (0..12)
        .map(|_| Bucket {
            weight: 0.0,
            r: 0.0,
            g: 0.0,
            b: 0.0,
        })
        .collect();

    let mut total = 0u32;
    let mut accepted = 0u32;

    // 步长 2：64×64 → 32×32 采样，足够取色且快 4 倍（§11.4 第 1 条）
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let p = ((y * w + x) * 4) as usize;
            if p + 3 >= bgra.len() {
                continue;
            }
            total += 1;
            let a = bgra[p + 3] as u32;
            if a < 32 {
                continue;
            }
            // PBGRA → 直通（a < 255 时反预乘）
            let un = |v: u8| -> f32 {
                if a >= 255 {
                    v as f32
                } else {
                    (v as f32 * 255.0 / a as f32).min(255.0)
                }
            };
            let b = un(bgra[p]);
            let g = un(bgra[p + 1]);
            let r = un(bgra[p + 2]);

            let (hh, ss, vv) = rgb_to_hsv(r, g, b);
            // 过滤：近黑 / 近白 / 灰
            if vv < 0.15 || vv > 0.95 || ss < 0.12 {
                continue;
            }
            accepted += 1;
            let idx = ((hh / 30.0) as usize).min(11);
            let weight = ss * vv;
            let bkt = &mut buckets[idx];
            bkt.weight += weight;
            bkt.r += r * weight;
            bkt.g += g * weight;
            bkt.b += b * weight;
        }
    }

    // 有效像素 < 8% → 纯白 / 纯灰封面，回退系统强调色（PRD P1-05）
    if total == 0 || (accepted as f32 / total as f32) < 0.08 {
        return Rgb::SYSTEM_ACCENT_FALLBACK;
    }

    let best = buckets
        .iter()
        .enumerate()
        .max_by(|a, b| {
            a.1.weight
                .partial_cmp(&b.1.weight)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let bkt = &buckets[best];
    if bkt.weight <= 0.0 {
        return Rgb::SYSTEM_ACCENT_FALLBACK;
    }

    let r = (bkt.r / bkt.weight).round().clamp(0.0, 255.0) as u8;
    let g = (bkt.g / bkt.weight).round().clamp(0.0, 255.0) as u8;
    let b = (bkt.b / bkt.weight).round().clamp(0.0, 255.0) as u8;

    let picked = Rgb::new(r, g, b);
    let normalized = normalize_accent(picked);
    crate::log_debug!(
        "封面主色：桶 {best} 权重 {:.1} → #{:02X}{:02X}{:02X}（归一化后 luma {:.3}）",
        bkt.weight,
        normalized.r,
        normalized.g,
        normalized.b,
        relative_luminance(normalized)
    );
    normalized
}

/// RGB（0..255）→ HSV（H 0..360，S/V 0..1）。
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let r = r / 255.0;
    let g = g / 255.0;
    let b = b / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let v = max;
    let s = if max <= 0.0 { 0.0 } else { d / max };
    if d <= f32::EPSILON {
        return (0.0, s, v);
    }
    let h = if (max - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / d) % 6.0)
    } else if (max - g).abs() < f32::EPSILON {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (if h < 0.0 { h + 360.0 } else { h }, s, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_of_pure_red() {
        let (h, s, v) = rgb_to_hsv(255.0, 0.0, 0.0);
        assert!((h - 0.0).abs() < 0.01);
        assert!((s - 1.0).abs() < 0.01);
        assert!((v - 1.0).abs() < 0.01);
    }

    #[test]
    fn hsv_of_gray_has_zero_saturation() {
        let (_, s, v) = rgb_to_hsv(128.0, 128.0, 128.0);
        assert!(s < 0.01);
        assert!((v - 0.502).abs() < 0.01);
    }

    /// 纯灰封面必须回退系统强调色（PRD P1-05）。
    #[test]
    fn gray_cover_falls_back_to_system_accent() {
        let (w, h) = (8u32, 8u32);
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            bgra[i * 4] = 0x80;
            bgra[i * 4 + 1] = 0x80;
            bgra[i * 4 + 2] = 0x80;
            bgra[i * 4 + 3] = 0xFF;
        }
        assert_eq!(extract_dominant(&bgra, w, h), Rgb::SYSTEM_ACCENT_FALLBACK);
    }

    /// 大面积饱和蓝必须被识别为蓝色系。
    #[test]
    fn saturated_blue_is_detected() {
        let (w, h) = (8u32, 8u32);
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            bgra[i * 4] = 0xD4; // B
            bgra[i * 4 + 1] = 0x78; // G
            bgra[i * 4 + 2] = 0x00; // R
            bgra[i * 4 + 3] = 0xFF;
        }
        let c = extract_dominant(&bgra, w, h);
        assert!(c.b > c.r, "应识别为蓝色系，实际 {c:?}");
    }

    #[test]
    fn rounded_mask_clears_corners_only() {
        let (w, h) = (16u32, 16u32);
        let mut bgra = vec![0xFFu8; (w * h * 4) as usize];
        apply_rounded_mask(&mut bgra, w, h);
        // 左上角被清零
        assert_eq!(bgra[3], 0);
        // 中心仍是 255
        let mid = ((8 * w + 8) * 4) as usize;
        assert_eq!(bgra[mid + 3], 0xFF);
    }
}
