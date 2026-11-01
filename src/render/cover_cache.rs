//! 封面位图缓存（`ARCHITECTURE.md` §7.6 / T06 第 3 条）。
//!
//! ## 为什么要按「帧槽位」分桶
//!
//! 渲染层是双缓冲（两张 `IWICBitmap` + 两个 `ID2D1RenderTarget`）。
//! `CreateBitmapFromWicBitmap` 产出的 `ID2D1Bitmap` 归属于创建它的渲染目标，
//! 跨目标使用行为未定义（实测会画出空白或颜色错乱）。
//!
//! 因此缓存 key 是 `(TrackId, 槽位)`：槽位 0 = A 帧，槽位 1 = B 帧。
//! 两个槽位各缓存一份，轮换绘制时永远命中。
//!
//! 容量写死 `limits::COVER_CACHE_CAP`（8），LRU 淘汰，绝不动态增长（风险 R11）。

use std::sync::Arc;

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1RenderTarget, D2D1_BITMAP_PROPERTIES,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{IWICImagingFactory, GUID_WICPixelFormat32bppPBGRA};

use crate::config::defaults::limits;
use crate::media::CoverImage;

/// 一条缓存记录。
struct Entry {
    track_id: u64,
    slot: u8,
    bitmap: ID2D1Bitmap,
    /// 最近一次命中的逻辑时钟（单调递增）
    used: u64,
}

/// 封面位图 LRU。
pub struct CoverCache {
    entries: Vec<Entry>,
    tick: u64,
    /// 统计信息（日志用）
    pub hits: u64,
    pub misses: u64,
}

impl Default for CoverCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CoverCache {
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(limits::COVER_CACHE_CAP),
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// 渲染目标重建时清空（旧位图属于已释放的目标）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.tick = 0;
    }

    /// 取（或创建）`track_id` 在 `slot` 上的封面位图。
    ///
    /// 失败返回 `None`（渲染层退回渐变占位，不影响其它元素）。
    pub fn get(
        &mut self,
        rt: &ID2D1RenderTarget,
        wic: &IWICImagingFactory,
        slot: u8,
        track_id: u64,
        cover: &Arc<CoverImage>,
    ) -> Option<ID2D1Bitmap> {
        self.tick += 1;
        let now = self.tick;

        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.track_id == track_id && e.slot == slot)
        {
            e.used = now;
            self.hits += 1;
            return Some(e.bitmap.clone());
        }

        self.misses += 1;
        let bitmap = create_bitmap(rt, wic, cover)?;

        // LRU 淘汰
        if self.entries.len() >= limits::COVER_CACHE_CAP {
            if let Some((idx, _)) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.used)
            {
                self.entries.remove(idx);
            }
        }
        self.entries.push(Entry {
            track_id,
            slot,
            bitmap: bitmap.clone(),
            used: now,
        });
        Some(bitmap)
    }

    /// 当前缓存条数（诊断用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 从 `CoverImage` 的原始 BGRA 造一个 D2D 位图。
fn create_bitmap(
    rt: &ID2D1RenderTarget,
    wic: &IWICImagingFactory,
    cover: &CoverImage,
) -> Option<ID2D1Bitmap> {
    if cover.width == 0 || cover.height == 0 {
        return None;
    }
    let stride = cover.width * 4;
    let need = (stride as usize) * (cover.height as usize);
    if cover.bgra.len() < need {
        return None;
    }

    // SAFETY: 像素缓冲长度已校验；GUID 是静态常量；WIC 位图在 D2D 位图创建完成后即可释放
    // （CreateBitmapFromWicBitmap 会拷贝像素）。
    let wic_bitmap = unsafe {
        wic.CreateBitmapFromMemory(
            cover.width,
            cover.height,
            &GUID_WICPixelFormat32bppPBGRA,
            stride,
            &cover.bgra[..need],
        )
    }
    .ok()?;

    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };

    // SAFETY: wic_bitmap 是本函数刚创建的合法源；props 是栈上值。
    unsafe { rt.CreateBitmapFromWicBitmap(&wic_bitmap, Some(&props)) }.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_starts_empty() {
        let c = CoverCache::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn clear_resets_entries() {
        let mut c = CoverCache::new();
        c.tick = 42;
        c.clear();
        assert_eq!(c.tick, 0);
        assert!(c.is_empty());
    }
}
