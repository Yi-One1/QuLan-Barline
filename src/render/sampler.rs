//! 背景采样线程（T3）：抓屏 → alpha 掩码剔除 → 加权亮度 → 主题。
//!
//! 对应 `ARCHITECTURE.md` §11.2（亮度自适应）。v1.1.11（问题 2）起
//! **不再生产模糊背景**——悬停背景只有半透明/透明两档，本线程只负责
//! 「壁纸亮度 → 文字对比度」的主题推导。
//!
//! ## v1.2.0（G5 问题 3）：亮度稳定化
//!
//! 亮度算出来之后**不再直接喂 `Theme`**，而是先过 [`LumaStabilizer`]
//! （双阈值死区 + 连续确认 + 大幅跳变快速通道；**无 EMA**）。稳定器**跨样本
//! 持有状态**，所以它必须活在本线程的栈上（`run` 里），不能是 `sample()` 的局部变量。
//! 结果通过采样区域指纹感知几何代次：采样区域变了就重置状态（见 [`area_epoch`]）。
//!
//! 「1px 反色描边」兜底已删除（用户明确要求去掉那一圈异常描边）。
//!
//! ## 核心难点 H3 / 风险 R5：抓屏会抓到自己
//!
//! `GetDC(NULL)` + `BitBlt` 拿到的是**已合成**的桌面，包含我们自己画的文字与封面。
//! 直接求平均亮度会被自己的内容污染，而且会形成自我强化的正反馈
//! （文字越白 → 平均亮度越高 → 判定背景越亮 → 文字越黑 …）。
//!
//! **解法**：UI 线程在 `UpdateLayeredWindow` 之后把上一帧的 alpha 图按 4×4 块
//! 统计成 [`CoverageMap`] 共享过来；采样时只统计「我们几乎没画东西」的块
//! （coverage < 10%），文本区域覆盖的块额外加权 ×3。
//!
//! ## 为什么不用 `WDA_EXCLUDEFROMCAPTURE`
//!
//! `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)` 会让我们的窗口在截图里
//! 变成**纯黑**——那比「抓到自己的文字」更糟（整块背景被拉黑）。
//!
//! ## CPU 预算
//!
//! 窗口最大 510×135 物理像素 → 128×34 个块。块级加权亮度统计
//! 合计 < 0.3 ms，且**只在收到请求时**运行；空闲时阻塞在 `recv_timeout` 上，CPU ≈ 0。

use std::sync::mpsc::{Receiver, RecvTimeoutError, SendError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
};

use crate::config::defaults::limits;
use crate::event::AppEvent;
use crate::media::Rgb;
use crate::platform::wake::EventSink;
use crate::render::layout::RectF;
use crate::render::stable::LumaStabilizer;
use crate::render::theme::{srgb8_to_linear, BackgroundMode, Theme};

/// 上一帧的 alpha 覆盖率图（4×4 块粒度）。
///
/// `cov[i]` 是该块内 alpha > 8 的像素个数（0..=16）。
/// 用 `u8` 而不是 `f32` 是为了让 `Arc` 共享的体积最小（510×135 → 128×34 = 4.3 KB）。
#[derive(Debug, Clone)]
pub struct CoverageMap {
    pub bw: u32,
    pub bh: u32,
    pub cov: Vec<u8>,
}

impl CoverageMap {
    /// 从一帧 BGRA（顶朝下）统计覆盖率。
    pub fn from_bgra(bgra: &[u8], w: u32, h: u32) -> Self {
        let bs = limits::SAMPLE_BLOCK;
        let bw = w.div_ceil(bs);
        let bh = h.div_ceil(bs);
        let mut cov = vec![0u8; (bw * bh) as usize];
        let stride = w as usize * 4;

        for by in 0..bh {
            for bx in 0..bw {
                let mut n: u16 = 0;
                let x0 = bx * bs;
                let y0 = by * bs;
                for dy in 0..bs {
                    let y = y0 + dy;
                    if y >= h {
                        break;
                    }
                    for dx in 0..bs {
                        let x = x0 + dx;
                        if x >= w {
                            break;
                        }
                        let p = y as usize * stride + x as usize * 4;
                        if p + 3 < bgra.len() && bgra[p + 3] > 8 {
                            n += 1;
                        }
                    }
                }
                cov[(by * bw + bx) as usize] = n.min(255) as u8;
            }
        }

        Self { bw, bh, cov }
    }

    /// 某块的覆盖比例 0.0..=1.0。
    pub fn ratio(&self, bx: u32, by: u32) -> f32 {
        if bx >= self.bw || by >= self.bh {
            return 1.0;
        }
        let n = self.cov[(by * self.bw + bx) as usize] as f32;
        let total = (limits::SAMPLE_BLOCK * limits::SAMPLE_BLOCK) as f32;
        (n / total).clamp(0.0, 1.0)
    }

    /// 整体覆盖率（诊断用）。
    pub fn overall(&self) -> f32 {
        if self.cov.is_empty() {
            return 0.0;
        }
        let sum: u32 = self.cov.iter().map(|v| *v as u32).sum();
        let total = self.cov.len() as u32 * limits::SAMPLE_BLOCK * limits::SAMPLE_BLOCK;
        (sum as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// 一次采样请求。**最新覆盖槽**语义：300ms 内的多次请求只保留最后一次。
#[derive(Debug, Clone)]
pub struct SampleRequest {
    /// 几何代次（与 `AppState::geometry_epoch` 对应）
    pub epoch: u64,
    /// 窗口在屏幕上的物理矩形
    pub rect: RECT,
    pub scale: f32,
    pub mode: BackgroundMode,
    /// 常规（不悬停）背景模式（v1.1.12）
    pub idle_mode: BackgroundMode,
    /// 悬停半透明不透明度百分比 1..=100（v1.1.12）
    pub hover_alpha: u8,
    /// 常规半透明不透明度百分比 1..=100（v1.1.12）
    pub idle_alpha: u8,
    /// 当前强调色（主题推导用）
    pub accent: Rgb,
    /// 上一帧的覆盖率图；`None` 时退化为「全块平均」
    pub coverage: Option<Arc<CoverageMap>>,
    /// 文本区域（客户区坐标 + 权重倍数），这些块在亮度统计里加权
    pub text_rects: Vec<(RectF, f32)>,
}

/// 采样线程门面。
pub struct Sampler {
    tx: Sender<SampleRequest>,
    handle: Option<JoinHandle<()>>,
}

impl Sampler {
    /// 启动 T3 线程。线程创建失败返回 `None`（小组件退化为固定主题，仍可用）。
    pub fn spawn(sink: EventSink<AppEvent>) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<SampleRequest>();
        let spawned = std::thread::Builder::new()
            .name(String::from("barline-sample"))
            .stack_size(256 * 1024)
            .spawn(move || run(sink, rx));
        match spawned {
            Ok(handle) => Some(Self {
                tx,
                handle: Some(handle),
            }),
            Err(e) => {
                crate::log_err!("采样线程创建失败：{e}（主题固定为深色）");
                None
            }
        }
    }

    /// 投递一次采样请求（非阻塞）。
    pub fn request(&self, req: SampleRequest) {
        match self.tx.send(req) {
            Ok(()) => {}
            Err(SendError(_)) => crate::log_debug!("采样线程已退出，请求丢弃"),
        }
    }

    /// 通知退出并等待结束。
    pub fn shutdown(&mut self) {
        // 丢弃 sender 会让 recv_timeout 返回 Disconnected，线程自然退出。
        if let Some(handle) = self.handle.take() {
            // 先尝试断开：把 tx 换成一个立即丢弃的 channel
            let (dead_tx, _dead_rx) = std::sync::mpsc::channel::<SampleRequest>();
            let old = std::mem::replace(&mut self.tx, dead_tx);
            drop(old);
            let _ = handle.join();
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// T3 线程主循环。
///
/// **节流（§11.2）**：请求进入「最新覆盖槽」，`SAMPLE_THROTTLE_MS`（300ms）内
/// 多次请求只保留最后一次。这样「300ms 内完成黑白切换」与「不会采样风暴」
/// 两个目标同时成立，且没有忙循环。
fn run(sink: EventSink<AppEvent>, rx: Receiver<SampleRequest>) {
    crate::log_info!("采样线程启动（T3）");
    let throttle = Duration::from_millis(limits::SAMPLE_THROTTLE_MS);
    let mut slot: Option<SampleRequest> = None;
    let mut last_done = Instant::now() - throttle;
    let mut sampled: u64 = 0;
    // v1.2.0：亮度稳定器**跨样本持有**（连续确认 / 死区判定都需要上一帧结论），
    // 所以只能放在这里而不是 `sample()` 里面。
    let mut stabilizer = LumaStabilizer::new();

    loop {
        // 有空槽时等到节流窗口结束；无槽时阻塞 200ms（5 次/秒的空转，CPU ≈ 0）
        let wait = if slot.is_some() {
            throttle.saturating_sub(last_done.elapsed())
        } else {
            Duration::from_millis(200)
        };

        match rx.recv_timeout(wait) {
            Ok(req) => slot = Some(req),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if slot.is_some() && last_done.elapsed() >= throttle {
            if let Some(req) = slot.take() {
                if let Some(theme) = sample(&req, &mut stabilizer) {
                    sampled += 1;
                    crate::log_debug!(
                        "采样 #{sampled}：raw={:.3} on_light={} epoch={}",
                        theme.backdrop_luma,
                        theme.on_light,
                        req.epoch
                    );
                    sink.send(AppEvent::BackdropReady {
                        epoch: req.epoch,
                        theme,
                    });
                }
                last_done = Instant::now();
            }
        }
    }

    crate::log_info!("采样线程退出（共采样 {sampled} 次）");
}

/// 抓屏 + 统计 → 主题（经亮度稳定器）。
fn sample(req: &SampleRequest, stabilizer: &mut LumaStabilizer) -> Option<Theme> {
    let w = (req.rect.right - req.rect.left).max(1) as u32;
    let h = (req.rect.bottom - req.rect.top).max(1) as u32;
    // 防御：异常大的矩形直接拒绝，避免一次分配几十 MB
    if w > 4096 || h > 4096 {
        crate::log_warn!("采样矩形异常（{w}×{h}），跳过");
        return None;
    }

    // G2 诊断：抓屏单段计时。`GetDC(NULL)` + CreateDIBSection + **BitBlt** 全在里面，
    // BitBlt 读的是已合成桌面，会和 DWM / GPU 抢锁——这是「音乐卡一下」的头号嫌疑段。
    let pixels = {
        let _sp = crate::platform::perf::Span::new("sampler.capture");
        capture(req.rect.left, req.rect.top, w, h)?
    };
    // G2 诊断：像素统计单段计时（纯 CPU，正常 < 1ms；慢就说明这块真出问题了）
    let _analyze_sp = crate::platform::perf::Span::new("sampler.analyze");
    let bs = limits::SAMPLE_BLOCK;
    let bw = w.div_ceil(bs);
    let bh = h.div_ceil(bs);

    // ---- 1. 块平均色 + 覆盖率 ----
    let mut block = vec![0u8; (bw * bh * 4) as usize];
    let mut valid = vec![false; (bw * bh) as usize];
    let mut block_cov = vec![0f32; (bw * bh) as usize];
    let stride = w as usize * 4;

    for by in 0..bh {
        for bx in 0..bw {
            let mut sum = [0u32; 3];
            let mut n: u32 = 0;
            for dy in 0..bs {
                let y = by * bs + dy;
                if y >= h {
                    break;
                }
                for dx in 0..bs {
                    let x = bx * bs + dx;
                    if x >= w {
                        break;
                    }
                    let p = y as usize * stride + x as usize * 4;
                    if p + 3 >= pixels.len() {
                        continue;
                    }
                    sum[0] += pixels[p + 2] as u32; // R
                    sum[1] += pixels[p + 1] as u32; // G
                    sum[2] += pixels[p] as u32; // B
                    n += 1;
                }
            }
            let i = (by * bw + bx) as usize;
            if n == 0 {
                continue;
            }
            block[i * 4] = (sum[2] / n) as u8;
            block[i * 4 + 1] = (sum[1] / n) as u8;
            block[i * 4 + 2] = (sum[0] / n) as u8;
            block[i * 4 + 3] = 0xFF;

            let cov = req
                .coverage
                .as_ref()
                .map(|m| m.ratio(bx, by))
                .unwrap_or(0.0);
            block_cov[i] = cov;
            valid[i] = cov < limits::SAMPLE_COVERED_RATIO;
        }
    }

    // ---- 2. 加权相对亮度 ----
    let grid = BlockGrid {
        block: &block,
        valid: &valid,
        bw,
        bh,
    };
    Some(compute_theme(&grid, req, stabilizer))
}

/// 一次采样里「块网格 + 请求参数」的聚合体。
///
/// 原先 `compute_theme` 吃 8 个参数，其中 `block / valid / bw / bh` 是同一份
/// 块网格的四个面、`text_rects / scale / accent / mode` 是同一份请求的四个字段。
/// 聚成两个结构体后签名只剩 `(grid, req)`，语义反而更清楚。
struct BlockGrid<'a> {
    /// 每块的平均 BGRA（4 字节/块）
    block: &'a [u8],
    /// 该块是否「有效」（不是我们自己画的）
    valid: &'a [bool],
    bw: u32,
    bh: u32,
}

/// 采样区域指纹（v1.2.0）：**只看矩形**，不看 `AppState::geometry_epoch`。
///
/// 为什么不能用 `SampleRequest::epoch` 当稳定器的代次：UI 线程的几何代次
/// 每次 `geometry::probe()` 都会 +1（`EPOCH.fetch_add`），而 1s 心跳每秒都 probe，
/// 于是 `state.geometry_epoch` 实际上**每秒都在变**。拿它当稳定器的 epoch，
/// 每帧都会被当"首帧"重判，确认计数永远攒不起来（稳定化形同虚设）。
///
/// 稳定器真正关心的只是「这次采的是不是同一块屏幕区域」，所以按矩形打指纹：
/// 位置 / 尺寸任何一个变了 → 指纹变 → 重置（那次变化本来也该重新判定）。
fn area_epoch(rect: RECT) -> u64 {
    let w = (rect.right - rect.left).max(0) as u64;
    let h = (rect.bottom - rect.top).max(0) as u64;
    let mut v = rect.left as u32 as u64;
    v = v.wrapping_mul(1_000_003).wrapping_add(rect.top as u32 as u64);
    v = v.wrapping_mul(1_000_003).wrapping_add(w);
    v = v.wrapping_mul(1_000_003).wrapping_add(h);
    v
}

/// 加权亮度 → 主题。§11.2 第 3/4/5/6 步。
///
/// v1.2.0：亮度不直接决定文字色，先过 `stabilizer`（死区 + 连续确认 + 快速通道）。
/// 真正翻转时记一条 info（死区内不刷日志）。
fn compute_theme(
    grid: &BlockGrid<'_>,
    req: &SampleRequest,
    stabilizer: &mut LumaStabilizer,
) -> Theme {
    let BlockGrid {
        block,
        valid,
        bw,
        bh,
    } = *grid;
    let text_rects = &req.text_rects;
    let (scale, accent) = (req.scale, req.accent);
    let (mode, idle_mode) = (req.mode, req.idle_mode);
    let (hover_alpha, idle_alpha) = (req.hover_alpha, req.idle_alpha);
    let s = if scale > 0.0 { scale } else { 1.0 };
    let bs = limits::SAMPLE_BLOCK as f32;
    let mut sum_w = 0f64;
    let mut sum_l = 0f64;
    let mut valid_count = 0usize;

    for by in 0..bh {
        for bx in 0..bw {
            let i = (by * bw + bx) as usize;
            if !valid[i] {
                continue;
            }
            valid_count += 1;
            let r = block[i * 4 + 2];
            let g = block[i * 4 + 1];
            let b = block[i * 4];

            let luma = 0.2126 * srgb8_to_linear(r)
                + 0.7152 * srgb8_to_linear(g)
                + 0.0722 * srgb8_to_linear(b);

            // 文本区域加权（PRD 要求「按文本区域加权」）
            let cx = (bx as f32 * bs + bs * 0.5) / s;
            let cy = (by as f32 * bs + bs * 0.5) / s;
            let mut w = 1.0f64;
            for (rect, weight) in text_rects {
                if rect.contains(cx, cy) {
                    w = (*weight).max(1.0) as f64;
                    break;
                }
            }

            sum_w += w;
            sum_l += (luma as f64) * w;
        }
    }

    let total = (bw * bh) as f32;
    let valid_ratio = if total > 0.0 {
        valid_count as f32 / total
    } else {
        0.0
    };

    let luma = if sum_w > 0.0 && valid_ratio >= limits::SAMPLE_MIN_VALID_RATIO {
        (sum_l / sum_w) as f32
    } else {
        // 兜底（§11.2 第 4 步）：有效块太少 → 退回全块平均
        let mut all_w = 0f64;
        let mut all_l = 0f64;
        for by in 0..bh {
            for bx in 0..bw {
                let i = (by * bw + bx) as usize;
                let r = block[i * 4 + 2];
                let g = block[i * 4 + 1];
                let b = block[i * 4];
                all_l += (0.2126 * srgb8_to_linear(r)
                    + 0.7152 * srgb8_to_linear(g)
                    + 0.0722 * srgb8_to_linear(b)) as f64;
                all_w += 1.0;
            }
        }
        if all_w > 0.0 {
            (all_l / all_w) as f32
        } else {
            0.0
        }
    };

    // v1.2.0：稳定化之后的方向说了算（首帧立刻判定、死区保持、确认区 2 帧、
    // 大幅跳变 1 帧）。代次用**采样区域指纹**而不是 `req.epoch`
    //（后者每秒都变，见 `area_epoch` 注释）。
    let area = area_epoch(req.rect);
    let before = stabilizer.on_light();
    let on_light = stabilizer.push(luma, area);
    if let Some(prev) = before {
        if prev != on_light {
            crate::log_info!(
                "文字色翻转：on_light={} (was {}) raw={:.3} confirm={} area={:#x}",
                on_light,
                prev,
                luma,
                stabilizer.confirm(),
                area
            );
        }
    }

    Theme::from_backdrop_on(
        on_light,
        luma,
        accent,
        mode,
        idle_mode,
        hover_alpha,
        idle_alpha,
    )
}

/// 点是否落在 `w × h`、圆角 `r` 的矩形内（像素中心采样）。
pub fn inside_rounded(px: f32, py: f32, w: f32, h: f32, r: f32) -> bool {
    let r = r.max(0.0).min(w * 0.5).min(h * 0.5);
    if r <= 0.0 {
        return px >= 0.0 && py >= 0.0 && px < w && py < h;
    }
    if px < 0.0 || py < 0.0 || px >= w || py >= h {
        return false;
    }
    // 四个角做圆判定，其余直接通过
    let cx = if px < r {
        r
    } else if px > w - r {
        w - r
    } else {
        return true;
    };
    let cy = if py < r {
        r
    } else if py > h - r {
        h - r
    } else {
        return true;
    };
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

/// 抓取屏幕上的一块矩形，返回顶朝下的 BGRA。
fn capture(x: i32, y: i32, w: u32, h: u32) -> Option<Vec<u8>> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // 负高度 = 顶朝下
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: w * h * 4,
            ..Default::default()
        },
        ..Default::default()
    };

    // SAFETY: 全部是 GDI 句柄的成对创建/释放；每一条失败路径都会把已创建的句柄清掉。
    // bits 指向 CreateDIBSection 分配的 w*h*4 字节，只要 HBITMAP 未删除就一直有效。
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            let _ = ReleaseDC(None, screen);
            return None;
        }

        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let dib = match CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) => b,
            Err(_) => {
                let _ = DeleteDC(mem);
                let _ = ReleaseDC(None, screen);
                return None;
            }
        };
        let old = SelectObject(mem, HGDIOBJ(dib.0));

        let ok = BitBlt(mem, 0, 0, w as i32, h as i32, Some(screen), x, y, SRCCOPY).is_ok();

        let mut out = Vec::new();
        if ok && !bits.is_null() {
            let len = (w as usize) * (h as usize) * 4;
            // SAFETY: bits 至少可读 len 字节（CreateDIBSection 按 bmi 分配）。
            out.extend_from_slice(core::slice::from_raw_parts(bits as *const u8, len));
        }

        let _ = SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem);
        let _ = ReleaseDC(None, screen);

        if ok {
            Some(out)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的最小 `SampleRequest`（只关心 compute_theme 用到的字段）。
    fn sample_req(text_rects: Vec<(RectF, f32)>) -> SampleRequest {
        SampleRequest {
            epoch: 0,
            rect: RECT::default(),
            scale: 1.0,
            mode: BackgroundMode::Translucent,
            idle_mode: BackgroundMode::Transparent,
            hover_alpha: 70,
            idle_alpha: 50,
            accent: Rgb::SYSTEM_ACCENT_FALLBACK,
            coverage: None,
            text_rects,
        }
    }

    /// 测试里构造块网格 + 请求，直接拿主题（每次新建稳定器 = 首帧规则）。
    fn theme_of(
        block: &[u8],
        valid: &[bool],
        bw: u32,
        bh: u32,
        text_rects: Vec<(RectF, f32)>,
    ) -> Theme {
        let grid = BlockGrid {
            block,
            valid,
            bw,
            bh,
        };
        let mut stab = LumaStabilizer::new();
        compute_theme(&grid, &sample_req(text_rects), &mut stab)
    }

    /// 用同一个稳定器连续采样，返回每次的主题（验证「线程级状态」真的接上了）。
    fn theme_series(colors: &[u8], block_side: u32) -> Vec<Theme> {
        let bw = block_side;
        let bh = block_side;
        let mut out = Vec::new();
        let mut stab = LumaStabilizer::new();
        for c in colors {
            let mut block = vec![0u8; (bw * bh * 4) as usize];
            for i in 0..(bw * bh) as usize {
                block[i * 4] = *c;
                block[i * 4 + 1] = *c;
                block[i * 4 + 2] = *c;
                block[i * 4 + 3] = 0xFF;
            }
            let valid = vec![true; (bw * bh) as usize];
            let grid = BlockGrid {
                block: &block,
                valid: &valid,
                bw,
                bh,
            };
            out.push(compute_theme(&grid, &sample_req(Vec::new()), &mut stab));
        }
        out
    }

    /// **v1.2.0 问题 3 的接线测试**：`compute_theme` 必须把亮度喂给稳定器，
    /// 而不是自己按单帧亮度硬切。
    ///
    /// 串：纯黑（首帧判深色）→ 连续 `0x93`（luma ≈ 0.29，落在**确认区**
    /// 0.25..0.30 之内，正好绕开快速通道）。
    /// 单帧硬切（`pick_on_light(0.29)`）会在**第 1 帧**就判浅色；
    /// 稳定器版本必须**精确 2 帧**才翻（第 1 帧只记确认）。
    #[test]
    fn theme_text_color_follows_stabilizer() {
        let series = theme_series(&[0x00, 0x93, 0x93], 2);
        let got: Vec<bool> = series.iter().map(|t| t.on_light).collect();
        assert!(
            (series[1].backdrop_luma - 0.29).abs() < 0.03,
            "0x93 的 luma 应≈0.29，实际 {}",
            series[1].backdrop_luma
        );
        assert_eq!(
            got,
            vec![false, false, true],
            "确认区必须精确 2 帧翻转（单帧硬切会得到 [false,true,true]）"
        );
        assert_eq!(series[0].text, Theme::LIGHT_TEXT, "深色背景用浅色字");
        assert_eq!(series[2].text, Theme::DARK_TEXT, "最终浅色背景用深色字");
    }

    /// **关键回归（v1.2.0）**：稳定器绝不能用 `req.epoch` 当代次。
    ///
    /// UI 线程的 `AppState::geometry_epoch` 每秒都被 `probe()` 的 `EPOCH` 自增刷一遍，
    /// 拿它当稳定器代次会让状态**每秒归零** —— 稳定化形同虚设。
    /// 这里用同一块矩形 + 每秒变化的 `req.epoch`，喂确认区的 `0x93`（luma≈0.29）：
    /// 若被 epoch 冲掉，第 2 帧就会按首帧规则立刻判浅色（`[false,true,true]`）；
    /// 正确实现是精确 2 帧（`[false,false,true]`）。
    #[test]
    fn epoch_bump_must_not_reset_stabilizer() {
        let bw = 2u32;
        let bh = 2u32;
        let mut stab = LumaStabilizer::new();
        let mut got = Vec::new();
        for (i, c) in [0x00u8, 0x93, 0x93].iter().enumerate() {
            let mut block = vec![0u8; (bw * bh * 4) as usize];
            for k in 0..(bw * bh) as usize {
                block[k * 4] = *c;
                block[k * 4 + 1] = *c;
                block[k * 4 + 2] = *c;
                block[k * 4 + 3] = 0xFF;
            }
            let valid = vec![true; (bw * bh) as usize];
            let grid = BlockGrid {
                block: &block,
                valid: &valid,
                bw,
                bh,
            };
            let mut req = sample_req(Vec::new());
            req.epoch = 100 + i as u64; // 模拟「每秒 probe 都 +1」
            got.push(compute_theme(&grid, &req, &mut stab).on_light);
        }
        assert_eq!(
            got,
            vec![false, false, true],
            "req.epoch 把稳定器冲掉了（每帧都按首帧规则重判 → [false,true,true]）"
        );
    }

    /// 区域指纹：同一矩形稳定、任一分量变化都要变。
    #[test]
    fn area_epoch_tracks_the_sampling_rect_only() {
        let a = RECT {
            left: 100,
            top: 200,
            right: 400,
            bottom: 260,
        };
        assert_eq!(area_epoch(a), area_epoch(a), "同一矩形必须同指纹");

        let moved = RECT { left: 120, ..a };
        let resized = RECT { right: 420, ..a };
        let retopped = RECT { top: 180, ..a };
        for other in [moved, resized, retopped] {
            assert_ne!(
                area_epoch(a),
                area_epoch(other),
                "矩形变了必须换指纹（否则采样区域变了还沿用旧区域的确认计数）"
            );
        }
    }

    #[test]
    fn coverage_map_counts_alpha() {
        // 8×8 全透明 → 覆盖率为 0；左上 4×4 全不透明 → 该块覆盖率 1
        let (w, h) = (8u32, 8u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..4usize {
            for x in 0..4usize {
                px[(y * 8 + x) * 4 + 3] = 255;
            }
        }
        let m = CoverageMap::from_bgra(&px, w, h);
        assert_eq!((m.bw, m.bh), (2, 2));
        assert!((m.ratio(0, 0) - 1.0).abs() < 1e-6);
        assert!((m.ratio(1, 0) - 0.0).abs() < 1e-6);
        assert!((m.ratio(0, 1) - 0.0).abs() < 1e-6);
        assert!(m.overall() > 0.2 && m.overall() < 0.3);
    }

    #[test]
    fn coverage_ratio_out_of_range_is_conservative() {
        let m = CoverageMap {
            bw: 1,
            bh: 1,
            cov: vec![0],
        };
        // 越界索引按「已覆盖」处理，避免误把不存在的块算进亮度
        assert!((m.ratio(5, 5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rounded_corner_rejects_outside_pixels() {
        assert!(inside_rounded(10.0, 10.0, 20.0, 20.0, 6.0));
        // 左上角顶点在圆外
        assert!(!inside_rounded(0.5, 0.5, 20.0, 20.0, 6.0));
        // 无圆角时整块都算内部
        assert!(inside_rounded(0.5, 0.5, 20.0, 20.0, 0.0));
        // 越界
        assert!(!inside_rounded(-1.0, 5.0, 20.0, 20.0, 6.0));
    }

    /// 风险 R5 的专项单测：给定「已知 alpha 掩码 + 已知屏幕图」，
    /// 断言只统计非覆盖块（文字色不会污染背景亮度）。
    #[test]
    fn brightness_ignores_covered_blocks() {
        // 4×1 个块：前 3 块是深灰，最后 1 块是纯白（但被我们完全覆盖 → 必须被剔除）
        let bw = 4u32;
        let bh = 1u32;
        let mut block = vec![0u8; (bw * bh * 4) as usize];
        for i in 0..3 {
            block[i * 4] = 0x20;
            block[i * 4 + 1] = 0x20;
            block[i * 4 + 2] = 0x20;
            block[i * 4 + 3] = 0xFF;
        }
        block[3 * 4] = 0xFF;
        block[3 * 4 + 1] = 0xFF;
        block[3 * 4 + 2] = 0xFF;
        block[3 * 4 + 3] = 0xFF;

        let valid = [true, true, true, false];

        let theme = theme_of(&block, &valid, bw, bh, Vec::new());

        // 深灰（0x20）的相对亮度 ≈ 0.0144，必须选浅色文字
        assert!(!theme.on_light, "被覆盖的白块污染了亮度判定");
        assert!(theme.backdrop_luma < 0.05, "luma = {}", theme.backdrop_luma);
    }

    /// 全部块都被覆盖（有效块 0%）→ 走「全块平均」兜底，不能返回 0 亮度死锁。
    #[test]
    fn all_covered_falls_back_to_full_average() {
        let bw = 2u32;
        let bh = 1u32;
        let mut block = vec![0x80u8; (bw * bh * 4) as usize];
        for i in 0..(bw * bh) as usize {
            block[i * 4 + 3] = 0xFF;
        }
        let valid = [false, false];
        let theme = theme_of(&block, &valid, bw, bh, Vec::new());
        // #808080 的亮度 ≈ 0.216
        assert!(
            (theme.backdrop_luma - 0.216).abs() < 0.02,
            "luma = {}",
            theme.backdrop_luma
        );
    }

    #[test]
    fn text_rects_increase_weight() {
        // 两块：左块纯黑（权重 1），右块纯蓝（权重 3）→ 平均必须偏蓝
        let bw = 2u32;
        let bh = 1u32;
        let mut block = vec![0u8; (bw * bh * 4) as usize];
        // 块 0：BGRA = (0,0,0,255)；块 1：B=0xFF, G=0, R=0, A=0xFF
        block[3] = 0xFF;
        block[4] = 0xFF; // 块 1 的 B 通道
        block[7] = 0xFF;
        let valid = [true, true];

        let plain = theme_of(&block, &valid, bw, bh, Vec::new());
        // 右半（x ∈ [4,8)）加权 ×3
        let weighted = theme_of(&block, &valid, bw, bh, vec![(RectF::new(4.0, 0.0, 4.0, 4.0), 3.0)]);
        assert!(
            weighted.backdrop_luma > plain.backdrop_luma,
            "文本区加权未生效：weighted={} plain={}",
            weighted.backdrop_luma,
            plain.backdrop_luma
        );
        // 蓝的亮度 0.0722；加权后应更接近它而不是 0
        assert!(weighted.backdrop_luma > 0.05, "luma = {}", weighted.backdrop_luma);
    }
}
