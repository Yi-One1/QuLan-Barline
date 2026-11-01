//! Direct2D / DirectWrite / WIC 设备资源（`ARCHITECTURE.md` §9.3 / §11.1）。
//!
//! 三个工厂是进程级单例（单线程 D2D 工厂 + 共享 DWrite 工厂 + WIC 工厂），
//! 只有「渲染目标 + WIC 位图」随尺寸变化重建。
//!
//! **像素格式必须是 `GUID_WICPixelFormat32bppPBGRA` + `D2D1_ALPHA_MODE_PREMULTIPLIED`**
//! 两者同时成立，`UpdateLayeredWindow` 才能拿到正确的逐像素 alpha；
//! 混用（例如 WIC 用 straight alpha）会让整块窗口发灰（风险 R3）。
//!
//! `CreateWicBitmapRenderTarget` 在 windows 0.62.2 里被
//! `#[cfg(all(feature = "Win32_Graphics_Direct2D_Common", feature = "Win32_Graphics_Dxgi_Common"))]`
//! 门控——Cargo.toml 里少开一个，这个方法**整个不存在**（§2.2 第 9 条）。
//!
//! ## M2 的两处改进
//!
//! 1. **笔刷缓存（修 M1 技术债 D3）**：M1 每帧 `CreateSolidColorBrush` 4 次。
//!    现在 [`BrushCache`] 按打包色值缓存，一帧内同一颜色只建一次，
//!    且缓存挂在 `Frame` 上（渲染目标重建时才失效）。
//! 2. **覆盖率图**：`HdcBuf::upload` 顺手把 alpha 按 4×4 块统计成
//!    [`CoverageMap`]，供 T3 采样线程剔除「我们自己画的像素」（§11.2，风险 R5）。

use std::sync::Arc;

use windows::core::{Interface, Result};
use windows_numerics::Vector2;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Brush, ID2D1Factory, ID2D1GradientStopCollection,
    ID2D1LinearGradientBrush, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_PROPERTIES, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::Graphics::Imaging::{
    IWICBitmap, IWICImagingFactory, CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA,
    WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::render::sampler::CoverageMap;
use crate::render::theme::Rgba;

/// 三个图形工厂。进程内只建一份。
pub struct D2d {
    pub factory: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    pub wic: IWICImagingFactory,
}

impl D2d {
    /// 创建三个工厂。调用线程必须已初始化 COM（WIC 是 COM 组件）。
    pub fn new() -> Result<Self> {
        // SAFETY: D2D1CreateFactory 的第一个参数枚举值合法；pfactoryoptions 传 None
        // 表示默认选项；返回的接口由 windows crate 负责 AddRef/Release 管理。
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };

        // SAFETY: 0.62.2 的 DWriteCreateFactory 只接受 factorytype（无 iid 参数），
        // 泛型 T 决定要拿哪个接口。
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        // SAFETY: CLSID_WICImagingFactory 是系统注册的进程内组件；
        // punkouter 传 None 表示不聚合。
        let wic: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };

        Ok(Self {
            factory,
            dwrite,
            wic,
        })
    }

    /// 建一帧：一张 PBGRA 的 WIC 位图 + 指向它的 D2D 渲染目标。
    pub fn create_frame(&self, width: u32, height: u32, dpi: f32) -> Result<Frame> {
        let w = width.max(1);
        let h = height.max(1);
        // SAFETY: 尺寸来自我们自己的布局（≥1），像素格式 GUID 是静态常量，
        // WICBitmapCacheOnLoad 表示立即分配像素内存。
        let bitmap = unsafe {
            self.wic
                .CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?
        };

        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };

        // SAFETY: bitmap 是本函数刚创建的合法 WIC 位图；props 是栈上结构体，
        // 生命周期覆盖调用；WIC 渲染目标必须用 PREMULTIPLIED 才能保住 alpha。
        let rt = unsafe { self.factory.CreateWicBitmapRenderTarget(&bitmap, &props)? };

        // WIC 渲染目标不支持 ClearType（会返回错误），必须显式改成灰度抗锯齿。
        // SAFETY: 两个枚举值都合法，且是对刚创建的 rt 调用。
        unsafe {
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }

        Ok(Frame {
            width: w,
            height: h,
            bitmap,
            rt,
            wic: self.wic.clone(),
            brushes: BrushCache::new(),
            grad: None,
            fade: None,
            blur: None,
        })
    }
}

/// 一帧渲染目标。
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bitmap: IWICBitmap,
    pub rt: ID2D1RenderTarget,
    /// WIC 工厂的引用（建封面位图用）
    pub wic: IWICImagingFactory,
    /// 本帧的实色笔刷缓存（修 M1 技术债 D3）
    pub brushes: BrushCache,
    /// 封面渐变占位笔刷缓存：`(accent 打包值, 笔刷)`
    pub grad: Option<(u32, ID2D1LinearGradientBrush)>,
    /// 歌词右侧渐隐笔刷缓存：`(颜色, 渐变起点 x, 渐变终点 x, 笔刷)`。
    /// x 用 `to_bits()` 做键（同帧内歌词矩形不变，命中率高）。
    pub fade: Option<((u32, u32, u32), ID2D1LinearGradientBrush)>,
    /// 悬停模糊背景位图缓存：`(几何代次, 缓冲地址, 位图)`
    ///
    /// 键里带「缓冲地址」是因为同一个 `geometry_epoch` 下模糊缓冲会每 250ms 换一份
    /// （悬停时背景在变），只用 epoch 会一直画第一帧的模糊图。
    pub blur: Option<(u64, usize, ID2D1Bitmap)>,
}

/// 实色笔刷缓存。
///
/// D2D 的 `ID2D1SolidColorBrush` 与渲染目标绑定，颜色改一次要重建一次。
/// M1 每帧建 4 个（命中区 / 胶囊 / 描边 / 强调条 / 文字），一帧内重复颜色也重复建。
/// 这里按打包色值缓存，容量 24（一帧最多用到约 12 种颜色），满了就整体清空重来
/// ——清空比 LRU 便宜，且颜色种类由主题决定，不会持续增长。
pub struct BrushCache {
    items: Vec<(u32, ID2D1SolidColorBrush)>,
}

impl BrushCache {
    const CAP: usize = 24;

    pub fn new() -> Self {
        Self {
            items: Vec::with_capacity(Self::CAP),
        }
    }

    /// 取（或创建）一个实色笔刷。
    pub fn get(&mut self, rt: &ID2D1RenderTarget, c: Rgba) -> Result<ID2D1SolidColorBrush> {
        let key = c.packed();
        if let Some((_, b)) = self.items.iter().find(|(k, _)| *k == key) {
            return Ok(b.clone());
        }
        let color = color_f(c.r, c.g, c.b, c.a);
        // SAFETY: color 是栈上值，生命周期覆盖调用；brushproperties 传 None = 默认。
        let brush = unsafe { rt.CreateSolidColorBrush(&color, None)? };
        if self.items.len() >= Self::CAP {
            self.items.clear();
        }
        self.items.push((key, brush.clone()));
        Ok(brush)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

impl Default for BrushCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Frame {
    /// 取实色笔刷（语法糖，省去每次写 `frame.brushes.get(&frame.rt, ...)` 的借用噪音）。
    pub fn brush(&mut self, c: Rgba) -> Result<ID2D1SolidColorBrush> {
        let rt = self.rt.clone();
        self.brushes.get(&rt, c)
    }

    /// 取（或创建）封面渐变占位笔刷：从 `accent` 到暗色，左上 → 右下。
    pub fn gradient(&mut self, accent: Rgba) -> Result<ID2D1LinearGradientBrush> {
        let key = accent.packed();
        if let Some((k, b)) = self.grad.as_ref() {
            if *k == key {
                return Ok(b.clone());
            }
        }

        // 终点色：强调色压暗 55%
        let dark = Rgba::new(
            (accent.r as f32 * 0.45) as u8,
            (accent.g as f32 * 0.45) as u8,
            (accent.b as f32 * 0.45) as u8,
            accent.a,
        );
        let stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: color_f(accent.r, accent.g, accent.b, accent.a),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: color_f(dark.r, dark.g, dark.b, dark.a),
            },
        ];

        let rt = self.rt.clone();
        // SAFETY: stops 是栈上数组，生命周期覆盖调用；gamma / extend 都是合法枚举。
        let collection: ID2D1GradientStopCollection = unsafe {
            rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?
        };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 { X: 0.0, Y: 0.0 },
            endPoint: Vector2 { X: 1.0, Y: 1.0 },
        };
        // SAFETY: props 栈上；collection 是本函数刚创建的合法对象。
        let brush: ID2D1LinearGradientBrush =
            unsafe { rt.CreateLinearGradientBrush(&props, None, &collection)? };
        self.grad = Some((key, brush.clone()));
        Ok(brush)
    }

    /// 渐变笔刷转基接口（`FillRoundedRectangle` 需要 `ID2D1Brush`）。
    pub fn gradient_base(&mut self, accent: Rgba) -> Result<ID2D1Brush> {
        let g = self.gradient(accent)?;
        g.cast::<ID2D1Brush>()
    }

    /// 取（或创建）**歌词右侧渐隐笔刷**（M3 遗留 P2：长行右侧渐隐）。
    ///
    /// 水平透明度渐变，坐标在**渲染目标空间**（笔刷默认 transform = identity）：
    /// `fade_start` 之前 CLAMP 到 stop0（`c` 原色），`fade_start..fade_end`
    /// 线性降到全透明，之后保持全透明。文本不超宽时整行都落在 stop0 区，
    /// 视觉与实色笔刷完全一致——所以调用方无需先测量是否超宽，直接用。
    ///
    /// 按键缓存：换行 / 换色 / 窗口改宽才会重建（扫光 25fps 热路径零新建）。
    pub fn fade_brush(
        &mut self,
        c: Rgba,
        fade_start: f32,
        fade_end: f32,
    ) -> Result<ID2D1LinearGradientBrush> {
        let key = (c.packed(), fade_start.to_bits(), fade_end.to_bits());
        if let Some((k, b)) = self.fade.as_ref() {
            if *k == key {
                return Ok(b.clone());
            }
        }
        let stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: color_f(c.r, c.g, c.b, c.a),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: color_f(c.r, c.g, c.b, 0),
            },
        ];
        let rt = self.rt.clone();
        // SAFETY: stops 是栈上数组，生命周期覆盖调用；gamma / extend 都是合法枚举。
        let collection: ID2D1GradientStopCollection =
            unsafe { rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)? };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: Vector2 {
                X: fade_start,
                Y: 0.0,
            },
            endPoint: Vector2 { X: fade_end, Y: 0.0 },
        };
        // SAFETY: props 栈上；collection 是本函数刚创建的合法对象。
        let brush: ID2D1LinearGradientBrush =
            unsafe { rt.CreateLinearGradientBrush(&props, None, &collection)? };
        self.fade = Some((key, brush.clone()));
        Ok(brush)
    }

    /// 把一张外部 BGRA（预乘）缓冲变成 D2D 位图。用于悬停模糊背景。
    pub fn bitmap_from_bgra(&self, bgra: &[u8], w: u32, h: u32) -> Result<ID2D1Bitmap> {
        let stride = w * 4;
        // SAFETY: 缓冲长度由调用方保证（BlurBuffer 与宽高自洽）；GUID 是静态常量。
        let wic_bmp = unsafe {
            self.wic
                .CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, stride, bgra)?
        };
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        // SAFETY: wic_bmp 合法；props 栈上。
        unsafe { self.rt.CreateBitmapFromWicBitmap(&wic_bmp, Some(&props)) }
    }
}

/// 双缓冲：两张 `Frame` 轮换。这样读取 A 的像素去做 `UpdateLayeredWindow` 时，
/// 下一帧可以直接写 B，不会撕裂。
pub struct Targets {
    pub a: Frame,
    pub b: Frame,
    /// 下一次画到 b 吗？
    pub next_is_b: bool,
    pub width: u32,
    pub height: u32,
    pub dpi: f32,
}

impl Targets {
    pub fn new(d2d: &D2d, width: u32, height: u32, dpi: f32) -> Result<Self> {
        Ok(Self {
            a: d2d.create_frame(width, height, dpi)?,
            b: d2d.create_frame(width, height, dpi)?,
            next_is_b: false,
            width,
            height,
            dpi,
        })
    }

    /// 取本轮要画的帧，并翻转轮换标记。
    pub fn take_next(&mut self) -> &mut Frame {
        self.next_is_b = !self.next_is_b;
        if self.next_is_b {
            &mut self.b
        } else {
            &mut self.a
        }
    }

    /// 当前槽位（0 = A，1 = B），用作封面缓存的分桶键。
    pub fn current_slot(&self) -> u8 {
        if self.next_is_b {
            1
        } else {
            0
        }
    }

    pub fn matches(&self, width: u32, height: u32, dpi: f32) -> bool {
        self.width == width && self.height == height && (self.dpi - dpi).abs() < 0.01
    }
}

/// 一块 32bpp 顶朝下的 DIB + 配套内存 DC。
///
/// `UpdateLayeredWindow` 需要一个「内容已经是 BGRA」的 `HDC`。D2D 画在 WIC 位图里，
/// 所以每次呈现前要把 WIC 的像素拷进这块 DIB。
///
/// `upload` 同时统计 alpha 覆盖率（[`CoverageMap`]），供采样线程剔除自遮挡。
pub struct HdcBuf {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
    coverage: Option<Arc<CoverageMap>>,
    /// 已经 upload 过的帧数（用于给覆盖率图降频，见 `upload`）
    frames: u32,
}

// SAFETY: HdcBuf 只在 UI 线程（T0）创建与使用；内部持有的是 GDI 句柄与一块
// 由 CreateDIBSection 分配的像素内存，没有任何跨线程共享。这里显式声明 Send
// 只是为了让 `App`（含 Renderer）能放进 `Box` 后取裸指针，不会被编译器以
// 「含非 Send 字段」为由拒绝——真正的线程约束由代码结构保证。
unsafe impl Send for HdcBuf {}

impl HdcBuf {
    /// 新建（或重建）一块 DIB。宽高必须 ≥ 1。
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let w = width.max(1);
        let h = height.max(1);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // 负高度 = 顶朝下（第 0 行就是屏幕最上面一行），省一次翻转
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: w * h * 4,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        // SAFETY: bmi 是本地结构体；bits 由 CreateDIBSection 写出，指向系统分配的
        // 一块 w*h*4 字节内存，只要 HBITMAP 不删除就一直有效。
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            let bmp = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(e) => {
                    let _ = DeleteDC(dc);
                    return Err(e);
                }
            };
            let old = SelectObject(dc, HGDIOBJ(bmp.0));
            Ok(Self {
                dc,
                bmp,
                old,
                bits: bits as *mut u8,
                width: w,
                height: h,
                coverage: None,
                frames: 0,
            })
        }
    }

    pub fn hdc(&self) -> HDC {
        self.dc
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// 上一帧的 alpha 覆盖率图（供 T3 采样线程做自遮挡剔除）。
    pub fn coverage(&self) -> Option<Arc<CoverageMap>> {
        self.coverage.clone()
    }

    /// 把一帧 WIC 位图的像素拷进 DIB，并顺手统计覆盖率。
    pub fn upload(&mut self, frame: &Frame) -> Result<()> {
        debug_assert_eq!(frame.width, self.width);
        debug_assert_eq!(frame.height, self.height);

        // SAFETY: Lock 传 null rect = 整张位图；flags=1 = WICBitmapLockRead。
        // GetDataPointer 返回的指针在 lock 存活期间有效，我们在同一个作用域内拷完。
        unsafe {
            let lock = frame.bitmap.Lock(core::ptr::null(), 1)?;
            let mut buf_size: u32 = 0;
            let mut src: *mut u8 = core::ptr::null_mut();
            lock.GetDataPointer(&mut buf_size, &mut src)?;
            let src_stride = lock.GetStride()? as usize;
            let dst_stride = (self.width * 4) as usize;
            let row_bytes = dst_stride.min(src_stride);

            if src.is_null() || self.bits.is_null() {
                return Ok(());
            }
            for y in 0..self.height as usize {
                let s = src.add(y * src_stride);
                let d = self.bits.add(y * dst_stride);
                core::ptr::copy_nonoverlapping(s, d, row_bytes);
            }
        }

        // 覆盖率：直接读 DIB（顶朝下，行紧密）。
        //
        // **降频到每 8 帧一次**：覆盖率只给采样线程做「自遮挡剔除」用，而采样
        // 最快也是 300ms 一次；逐字扫光时是 25fps，每帧都统计 2.5 万像素纯属浪费
        // （实测占单帧成本的 ~25%）。布局在两次采样之间不会变，8 帧 ≈ 320ms，
        // 完全够用。
        self.frames = self.frames.wrapping_add(1);
        let need_coverage = self.frames % 8 == 1;
        if need_coverage && !self.bits.is_null() {
            let len = (self.width as usize) * (self.height as usize) * 4;
            // SAFETY: bits 指向 CreateDIBSection 分配的 len 字节，刚写完。
            let px = unsafe { core::slice::from_raw_parts(self.bits as *const u8, len) };
            self.coverage = Some(Arc::new(CoverageMap::from_bgra(
                px,
                self.width,
                self.height,
            )));
        }
        Ok(())
    }
}

impl Drop for HdcBuf {
    fn drop(&mut self) {
        // SAFETY: 句柄都由本结构体独占持有，Drop 只跑一次；
        // 先恢复旧 GDI 对象再删除位图，最后删 DC（顺序不能反）。
        unsafe {
            let _ = SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bmp.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

// ----------------------------------------------------------------- 小工具

/// `Rgba`（直通 alpha）→ `D2D1_COLOR_F`（D2D 内部按目标 alpha 模式自行预乘）。
pub fn color_f(r: u8, g: u8, b: u8, a: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
}

/// 全透明色。`BeginDraw` 后必须用它 `Clear`，否则会残留上一帧（风险 R3）。
pub const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};

/// 矩形 → `D2D_RECT_F`。
pub fn d2d_rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

/// 构造一个圆角矩形。
pub fn rounded(x: f32, y: f32, w: f32, h: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: d2d_rect(x, y, w, h),
        radiusX: radius,
        radiusY: radius,
    }
}

/// 构造一个椭圆。
pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
    windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
        point: Vector2 { X: cx, Y: cy },
        radiusX: rx,
        radiusY: ry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brush_cache_key_is_packed_rgba() {
        assert_eq!(Rgba::new(0x12, 0x34, 0x56, 0x78).packed(), 0x7812_3456);
    }

    #[test]
    fn brush_cache_starts_empty() {
        let c = BrushCache::new();
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn rounded_rect_carries_radius() {
        let r = rounded(1.0, 2.0, 3.0, 4.0, 5.0);
        assert_eq!(r.radiusX, 5.0);
        assert_eq!(r.radiusY, 5.0);
        assert_eq!(r.rect.right, 4.0);
        assert_eq!(r.rect.bottom, 6.0);
    }
}
