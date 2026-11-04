//! 任务栏几何探测（`ARCHITECTURE.md` §10.3）。
//!
//! 设计原则：**「任何输入变化 → 重新计算 → 幂等应用」**。
//! `probe()` 每次都从头读一遍系统状态，不做增量；`compute_target()` 是纯函数，
//! 所以整条链路可以单测，也可以在 Explorer 重启 / 托盘图标增减 / 四方位 / 自动隐藏 /
//! 多 DPI 的组合爆炸下保持行为一致。
//!
//! M1 只做「探测 + 日志 + 静态摆放」，`target` 宽度固定；
//! M2 起 `target` 会按内容自然宽度自适应（签名不变）。

use std::sync::atomic::{AtomicU64, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, APPBARDATA, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW, GetWindowRect};

use crate::config::defaults::limits;
use crate::render::layout::TaskbarEdge;

/// 几何代次。每次 `probe()` 成功 +1，用于丢弃过期的背景模糊缓冲。
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// 任务栏几何快照（物理像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TaskbarGeometry {
    pub edge: TaskbarEdge,
    /// 任务栏整条带的屏幕矩形
    pub bar_rect: RECT,
    /// 托盘图标区（TrayNotifyWnd）的屏幕矩形
    pub tray_rect: RECT,
    /// 目标显示器的屏幕矩形
    pub monitor_rect: RECT,
    pub dpi: u32,
    /// 任务栏是否设为自动隐藏
    pub auto_hide: bool,
    /// 当前是否已收起（自动隐藏且不可见）
    pub collapsed: bool,
    /// 小组件应放置的屏幕矩形（物理像素）
    pub target: RECT,
    /// 几何代次
    pub epoch: u64,
}

impl TaskbarGeometry {
    /// 单行摘要，用于日志。
    pub fn summary(&self) -> String {
        format!(
            "edge={} bar=({},{} {}x{}) tray_left={} tray=({},{} {}x{}) monitor=({},{} {}x{}) dpi={} auto_hide={} collapsed={} target=({},{} {}x{}) epoch={}",
            self.edge.as_str(),
            self.bar_rect.left,
            self.bar_rect.top,
            self.bar_rect.right - self.bar_rect.left,
            self.bar_rect.bottom - self.bar_rect.top,
            self.tray_rect.left,
            self.tray_rect.left,
            self.tray_rect.top,
            self.tray_rect.right - self.tray_rect.left,
            self.tray_rect.bottom - self.tray_rect.top,
            self.monitor_rect.left,
            self.monitor_rect.top,
            self.monitor_rect.right - self.monitor_rect.left,
            self.monitor_rect.bottom - self.monitor_rect.top,
            self.dpi,
            self.auto_hide,
            self.collapsed,
            self.target.left,
            self.target.top,
            self.target.right - self.target.left,
            self.target.bottom - self.target.top,
            self.epoch,
        )
    }

    /// 列出与 `other` **实质不同**的字段名（诊断用，v1.2.0 / G5 问题 2）。
    ///
    /// 「几何变了 → 重排」的日志只写一句"几何变化"是没用的：G5 的「截图时左移」
    /// 必须知道究竟是 `tray_rect` 变了、`dpi` 变了还是 `auto_hide` 变了。
    /// 两份完全相同时返回空串。
    pub fn diff(&self, other: &Self) -> String {
        let mut out = String::new();
        let mut add = |name: &str| {
            if !out.is_empty() {
                out.push(',');
            }
            out.push_str(name);
        };
        if self.edge != other.edge {
            add("edge");
        }
        if self.bar_rect != other.bar_rect {
            add("bar_rect");
        }
        if self.tray_rect != other.tray_rect {
            add("tray_rect");
        }
        if self.monitor_rect != other.monitor_rect {
            add("monitor_rect");
        }
        if self.dpi != other.dpi {
            add("dpi");
        }
        if self.auto_hide != other.auto_hide {
            add("auto_hide");
        }
        if self.collapsed != other.collapsed {
            add("collapsed");
        }
        if self.target != other.target {
            add("target");
        }
        out
    }

    /// 两份几何是否「实质相同」——**忽略 `epoch`**。
    ///
    /// `probe()` 每次都会 +1 epoch，直接比整个结构体永远不等，
    /// 会导致每秒一次无谓的重排 + 重绘。
    pub fn same_shape(&self, other: &Self) -> bool {
        self.edge == other.edge
            && self.bar_rect == other.bar_rect
            && self.tray_rect == other.tray_rect
            && self.monitor_rect == other.monitor_rect
            && self.dpi == other.dpi
            && self.auto_hide == other.auto_hide
            && self.collapsed == other.collapsed
            && self.target == other.target
    }
}

/// 探测当前任务栏几何。返回 `None` 表示拿不到任务栏
/// （Explorer 正在重启，或运行在没有 shell 的会话里）。
pub fn probe() -> Option<TaskbarGeometry> {
    // ---- 1. 找任务栏 ----
    // SAFETY: FindWindowW 只按类名查找，不涉及我们持有的内存。
    let tray = match unsafe { FindWindowW(w!("Shell_TrayWnd"), None) } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            crate::log_debug!("未找到 Shell_TrayWnd（Explorer 可能正在重启）");
            return None;
        }
    };

    // ---- 2. 找托盘图标区；找不到就退化成任务栏自身 ----
    // SAFETY: 同上；第三个参数是类名，第四个是窗口名（null = 不限定）。
    let notify: HWND = unsafe {
        FindWindowExW(
            Some(tray),
            None,
            w!("TrayNotifyWnd"),
            PCWSTR::null(),
        )
    }
    .unwrap_or(tray);

    // ---- 3. 两个矩形 ----
    let mut bar_rect = RECT::default();
    let mut tray_rect = RECT::default();
    // SAFETY: 两个 RECT 都是本地栈变量，GetWindowRect 只写出这两个结构体。
    unsafe {
        GetWindowRect(tray, &mut bar_rect).ok()?;
        if notify != tray {
            GetWindowRect(notify, &mut tray_rect).ok()?;
        } else {
            // 某些系统版本托盘类名不同：把整条任务栏当成托盘区（右端对齐）
            tray_rect = bar_rect;
        }
    }

    // ---- 4/5. 方位 + 自动隐藏 ----
    let mut abd = APPBARDATA {
        // cbSize 必须填，否则 SHAppBarMessage 静默失败（最常见的坑）
        cbSize: core::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    // SAFETY: abd 是本地结构体且 cbSize 已填；SHAppBarMessage 只读写该结构体。
    let pos_ok = unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) } != 0;
    if !pos_ok {
        crate::log_debug!("SHAppBarMessage(ABM_GETTASKBARPOS) 失败，按 Bottom 处理");
    }
    let edge = if pos_ok {
        TaskbarEdge::from_appbar_edge(abd.uEdge)
    } else {
        TaskbarEdge::Bottom
    };

    let mut state_abd = APPBARDATA {
        cbSize: core::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    // SAFETY: 同上。
    let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut state_abd) } as u32;
    let auto_hide = (state & ABS_AUTOHIDE) != 0;

    // ---- 6. 显示器矩形 ----
    let mut mi = MONITORINFO {
        cbSize: core::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: hmon 由系统返回；mi 是本地结构体且 cbSize 已填。
    let monitor_rect = unsafe {
        let hmon = MonitorFromWindow(tray, MONITOR_DEFAULTTONEAREST);
        if hmon.is_invalid() || !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            bar_rect
        } else {
            mi.rcMonitor
        }
    };

    // ---- 7. DPI（任务栏的 DPI 就是该显示器的 DPI）----
    // SAFETY: GetDpiForWindow 对有效 HWND 安全。
    let dpi = {
        let d = unsafe { GetDpiForWindow(tray) };
        if d == 0 {
            96
        } else {
            d
        }
    };

    // ---- 8. 目标矩形 ----
    let scale = crate::platform::dpi::scale_for(dpi);
    let logical_w = limits::WIDGET_MIN_W;
    let target = compute_target(tray_rect, bar_rect, edge, scale, logical_w);

    // ---- 9. collapsed 判定 ----
    let collapsed = auto_hide && is_collapsed(edge, tray_rect, monitor_rect);

    let epoch = EPOCH.fetch_add(1, Ordering::Relaxed) + 1;

    Some(TaskbarGeometry {
        edge,
        bar_rect,
        tray_rect,
        monitor_rect,
        dpi,
        auto_hide,
        collapsed,
        target,
        epoch,
    })
}

/// 自动隐藏且已收起：按方位比较托盘区与显示器边界（§10.3 第 6 步）。
fn is_collapsed(edge: TaskbarEdge, tray_rect: RECT, monitor_rect: RECT) -> bool {
    const SLOP: i32 = 4;
    match edge {
        TaskbarEdge::Bottom => tray_rect.bottom <= monitor_rect.top + SLOP,
        TaskbarEdge::Top => tray_rect.top >= monitor_rect.bottom - SLOP,
        TaskbarEdge::Left => tray_rect.right <= monitor_rect.left + SLOP,
        TaskbarEdge::Right => tray_rect.left >= monitor_rect.right - SLOP,
    }
}

/// **纯函数**：由托盘区矩形 + 任务栏矩形 + 方位 + 缩放，算出小组件应占的屏幕矩形。
///
/// 语义（§10.4）：
/// - `Bottom` / `Top`：贴托盘区**左侧** `TRAY_GAP * scale`，高度 = 条带厚度；
/// - `Left` / `Right`：贴托盘区**上侧** `TRAY_GAP * scale`，宽度 = 条带厚度；
/// - 空间不足时收缩宽度，最小 120 * scale，再不够就贴到条带起始边。
pub fn compute_target(
    tray_rect: RECT,
    bar_rect: RECT,
    edge: TaskbarEdge,
    scale: f32,
    logical_w: f32,
) -> RECT {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let bar_w = (bar_rect.right - bar_rect.left).max(1);
    let bar_h = (bar_rect.bottom - bar_rect.top).max(1);
    let long = (logical_w * s).round() as i32;
    if edge.is_vertical() {
        place_widget(tray_rect, bar_rect, edge, scale, bar_w, long)
    } else {
        place_widget(tray_rect, bar_rect, edge, scale, long, bar_h)
    }
}

/// **纯函数**：把「已经算好尺寸的窗口」摆到任务栏条带里。
///
/// v1.1 起 `place_widget` 的正常输入高度**恒等于任务栏条带厚度**
/// （歌词区已移入条带内，双行压缩，小组件不再向桌面扩展），此时：
/// - `Bottom`：右边缘贴托盘区左侧（让出 gap），上下与条带完全重合；
/// - `Top`：同上；
/// - `Right`：底边贴托盘区上侧，左右与条带完全重合；
/// - `Left`：同上。
///
/// 函数仍保留对「比条带更厚」输入的通用处理（`Bottom` 时向上扩展等），
/// 作为防御性兜底——但正常路径不会再走到。
///
/// 空间不足时先收缩长边（最小 120*scale），再不够就贴到条带起始边。
pub fn place_widget(
    tray_rect: RECT,
    bar_rect: RECT,
    edge: TaskbarEdge,
    scale: f32,
    want_w: i32,
    want_h: i32,
) -> RECT {
    let s = if scale > 0.0 { scale } else { 1.0 };
    let gap = (limits::TRAY_GAP * s).round() as i32;
    let min_long = (120.0 * s).round() as i32;
    let bar_w = (bar_rect.right - bar_rect.left).max(1);

    match edge {
        TaskbarEdge::Bottom | TaskbarEdge::Top => {
            let right = tray_rect.left - gap;
            let avail = (right - bar_rect.left).max(0);
            let mut w = want_w.max(1);
            if w > avail {
                w = if avail >= min_long { min_long } else { avail };
            }
            let w = w.min(avail).max(0);
            let left = (right - w).max(bar_rect.left);
            let h = want_h.max(1);
            let (top, bottom) = if edge == TaskbarEdge::Bottom {
                (bar_rect.bottom - h, bar_rect.bottom)
            } else {
                (bar_rect.top, bar_rect.top + h)
            };
            RECT {
                left,
                top,
                right: left + w,
                bottom,
            }
        }
        TaskbarEdge::Left | TaskbarEdge::Right => {
            let bottom = tray_rect.top - gap;
            let avail = (bottom - bar_rect.top).max(0);
            let mut h = want_h.max(1);
            if h > avail {
                h = if avail >= min_long { min_long } else { avail };
            }
            let h = h.min(avail).max(0);
            let top = (bottom - h).max(bar_rect.top);
            let w = if want_w > 0 { want_w } else { bar_w };
            let (left, right) = if edge == TaskbarEdge::Right {
                (bar_rect.right - w, bar_rect.right)
            } else {
                (bar_rect.left, bar_rect.left + w)
            };
            RECT {
                left,
                top,
                right,
                bottom: top + h,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    /// 2560×1440 主屏，任务栏在底部，48px 高，托盘区在右侧 380px 宽。
    #[test]
    fn bottom_edge_places_left_of_tray_with_gap() {
        let bar = rect(0, 1392, 2560, 1440);
        let tray = rect(2180, 1392, 2560, 1440);
        let t = compute_target(tray, bar, TaskbarEdge::Bottom, 1.0, 200.0);
        assert_eq!(t.right, tray.left - 8);
        assert_eq!(t.left, tray.left - 8 - 200);
        assert_eq!(t.top, bar.top);
        assert_eq!(t.bottom, bar.bottom);
    }

    #[test]
    fn top_edge_places_left_of_tray_with_gap() {
        let bar = rect(0, 0, 2560, 48);
        let tray = rect(2100, 0, 2560, 48);
        let t = compute_target(tray, bar, TaskbarEdge::Top, 1.0, 200.0);
        assert_eq!(t.right, tray.left - 8);
        assert_eq!(t.top, 0);
        assert_eq!(t.bottom, 48);
    }

    #[test]
    fn right_edge_places_above_tray() {
        // 任务栏在右侧：条带宽 60，托盘区在最下面
        let bar = rect(2500, 0, 2560, 1440);
        let tray = rect(2500, 1200, 2560, 1440);
        let t = compute_target(tray, bar, TaskbarEdge::Right, 1.0, 200.0);
        assert_eq!(t.bottom, tray.top - 8);
        assert_eq!(t.top, tray.top - 8 - 200);
        assert_eq!(t.left, 2500);
        assert_eq!(t.right, 2560);
    }

    #[test]
    fn left_edge_places_above_tray() {
        let bar = rect(0, 0, 60, 1440);
        let tray = rect(0, 1100, 60, 1440);
        let t = compute_target(tray, bar, TaskbarEdge::Left, 1.0, 200.0);
        assert_eq!(t.bottom, tray.top - 8);
        assert_eq!(t.left, 0);
        assert_eq!(t.right, 60);
    }

    #[test]
    fn dpi_scale_multiplies_everything() {
        let bar = rect(0, 1380, 2560, 1440); // 60 高 = 150%
        let tray = rect(2200, 1380, 2560, 1440);
        let t = compute_target(tray, bar, TaskbarEdge::Bottom, 1.25, 200.0);
        assert_eq!(t.right - t.left, 250);
        assert_eq!(t.right, tray.left - 10);
        assert_eq!(t.bottom - t.top, 60);
    }

    #[test]
    fn narrow_bar_shrinks_widget() {
        // 条带只有 300 宽，托盘区吃掉 250 → 只剩 50，不够最小 120
        let bar = rect(0, 1392, 300, 1440);
        let tray = rect(250, 1392, 300, 1440);
        let t = compute_target(tray, bar, TaskbarEdge::Bottom, 1.0, 200.0);
        // 至少不能超出条带
        assert!(t.left >= bar.left);
        assert!(t.right <= tray.left);
        assert!(t.right - t.left >= 0);
    }

    #[test]
    fn collapsed_detection_follows_edge() {
        let monitor = rect(0, 0, 2560, 1440);
        // 底部任务栏收起：托盘区被推到屏幕上方之外
        assert!(is_collapsed(
            TaskbarEdge::Bottom,
            rect(2180, -46, 2560, 2),
            monitor
        ));
        assert!(!is_collapsed(
            TaskbarEdge::Bottom,
            rect(2180, 1392, 2560, 1440),
            monitor
        ));
        assert!(is_collapsed(
            TaskbarEdge::Top,
            rect(2100, 1438, 2560, 1486),
            monitor
        ));
        assert!(is_collapsed(
            TaskbarEdge::Left,
            rect(-58, 0, 2, 1440),
            monitor
        ));
        assert!(is_collapsed(
            TaskbarEdge::Right,
            rect(2558, 0, 2618, 1440),
            monitor
        ));
    }

    /// v1.1：小组件高度恒为条带厚度——底部任务栏时与条带完全重合，不出屏。
    #[test]
    fn bar_height_widget_fits_exactly_on_bottom_taskbar() {
        let bar = rect(0, 1392, 2560, 1440);
        let tray = rect(2194, 1392, 2560, 1440);
        let t = place_widget(tray, bar, TaskbarEdge::Bottom, 1.0, 429, 48);
        assert_eq!(t.top, bar.top, "顶边必须与条带重合");
        assert_eq!(t.bottom, bar.bottom, "底边必须与条带重合");
        assert_eq!(t.right, tray.left - 8);
        assert_eq!(t.right - t.left, 429);
    }

    /// 顶部任务栏同样与条带完全重合。
    #[test]
    fn bar_height_widget_fits_exactly_on_top_taskbar() {
        let bar = rect(0, 0, 2560, 48);
        let tray = rect(2194, 0, 2560, 48);
        let t = place_widget(tray, bar, TaskbarEdge::Top, 1.0, 429, 48);
        assert_eq!(t.top, bar.top);
        assert_eq!(t.bottom, bar.top + 48);
    }

    /// 防御性兜底：万一传入比条带更厚的窗口，底部任务栏仍向上扩展（不出屏）。
    #[test]
    fn taller_widget_grows_upward_on_bottom_taskbar() {
        let bar = rect(0, 1392, 2560, 1440);
        let tray = rect(2194, 1392, 2560, 1440);
        let t = place_widget(tray, bar, TaskbarEdge::Bottom, 1.0, 269, 94);
        assert_eq!(t.bottom, bar.bottom, "底边必须贴任务栏底边");
        assert_eq!(t.top, bar.bottom - 94, "顶边向上扩展");
        assert_eq!(t.right, tray.left - 8);
        assert_eq!(t.right - t.left, 269);
    }

    /// 竖排任务栏：宽 = 条带宽度，高 = 长边，向上扩展。
    #[test]
    fn vertical_taskbar_uses_bar_width() {
        let bar = rect(2500, 0, 2560, 1440);
        let tray = rect(2500, 1200, 2560, 1440);
        // 竖排时窗口宽 = 条带宽度（60），高 = 逻辑长边（269）
        let t = place_widget(tray, bar, TaskbarEdge::Right, 1.0, 60, 269);
        assert_eq!(t.left, 2500);
        assert_eq!(t.right, 2560);
        assert_eq!(t.bottom, tray.top - 8);
        assert_eq!(t.bottom - t.top, 269);
    }
}
