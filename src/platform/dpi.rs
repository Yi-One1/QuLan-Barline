//! DPI 感知与显示器几何。
//!
//! 关键约束（ARCHITECTURE.md R14）：
//! - 进程级 DPI 感知用 `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`，
//!   **必须在创建任何窗口之前**调用；
//! - 取 DPI 只用 `GetDpiForWindow`，**不用** `GetDeviceCaps(LOGPIXELSX)`
//!   （在 DPI 感知进程里它返回的是系统 DPI，不可靠）。

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// 设置进程 DPI 感知为「每显示器 V2」。返回是否成功。
///
/// 失败通常意味着：清单里已经声明了 dpiAware（我们刻意没写）或已被更早的调用锁定。
/// 失败不致命，继续以系统 DPI 运行。
pub fn set_process_dpi_aware() -> bool {
    // SAFETY: 纯进程级设置，无指针参数；只要在任何窗口创建之前调用即可。
    let r = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    r.is_ok()
}

/// 取窗口所在显示器的 DPI（96 = 100%）。
pub fn get_dpi_for_window(hwnd: HWND) -> u32 {
    // SAFETY: GetDpiForWindow 对任意有效 HWND 安全；hwnd 无效时返回 0，
    // 这里统一兜底为 96。
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 {
        96
    } else {
        dpi
    }
}

/// DPI → 缩放系数（96 → 1.0，120 → 1.25，144 → 1.5）。
pub fn scale_for(dpi: u32) -> f32 {
    let d = if dpi == 0 { 96 } else { dpi };
    d as f32 / 96.0
}

/// 一个显示器的几何信息（物理像素）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MonitorRect {
    /// 显示器整块矩形
    pub monitor: RECT,
    /// 去掉任务栏后的工作区
    pub work: RECT,
}

/// 取窗口所在显示器（最近的）的矩形。
pub fn monitor_rect(hwnd: HWND) -> Option<MonitorRect> {
    // SAFETY: hwnd 由调用方保证有效；MONITORINFO.cbSize 必须由我们填好，
    // 否则 GetMonitorInfoW 直接失败（这是最常见的踩坑点）。
    unsafe {
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if hmon.is_invalid() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: core::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut info).as_bool() {
            return None;
        }
        Some(MonitorRect {
            monitor: info.rcMonitor,
            work: info.rcWork,
        })
    }
}
