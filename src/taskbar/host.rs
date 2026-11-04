//! 分层窗口宿主（`ARCHITECTURE.md` §10.1 / §9.2）。
//!
//! 为什么必须是 **top-level** 窗口而不是 `SetParent(taskbar)` 的子窗口：
//! 1. 子窗口会被 Explorer 的布局逻辑重排位置；
//! 2. 子窗口收不到 `TaskbarCreated` 广播（该广播只发给 top-level 窗口）；
//! 3. `WS_EX_LAYERED` + `UpdateLayeredWindow` 的逐像素透明只对 top-level / popup 生效；
//! 4. 子窗口会被任务栏裁剪，动画时不稳。
//!
//! 为什么不是 deskband：deskband 是 IE 时代的 `IBandSite` COM 组件，Win11 已完全移除，
//! 且需要注册 COM、由 Explorer 加载 DLL。本方案零注册、零安装。

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BLENDFUNCTION, HDC, AC_SRC_ALPHA, AC_SRC_OVER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetWindowRect, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_BOTTOM,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WM_APP, WINDOW_EX_STYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

/// 小组件窗口类名。单实例唤起时会 `FindWindowW` 它（`platform::single_instance`）。
pub const WIDGET_CLASS_NAME: PCWSTR = w!("BarlineWidgetWnd");
/// 窗口标题（不出现在任务栏 / Alt+Tab，仅用于诊断）。
pub const WIDGET_TITLE: PCWSTR = w!("曲栏");

/// `WM_APP` 之后的第一个自定义消息，留给小组件窗口内部使用。
pub const WM_BARLINE_WIDGET_BASE: u32 = WM_APP + 16;

/// 按「全屏遮挡态」选 Z 序插入锚点（v1.5.6 / G3.5-A）——**纯函数**，可单测。
///
/// - `demoted = true` → `HWND_BOTTOM`：一次调用同时清除 `WS_EX_TOPMOST` 状态并落到
///   Z 序最底。绝不用 `HWND_NOTOPMOST`——它只是「置于所有**非** topmost 窗口之上」，
///   而 Chrome F11 / 电影和电视 / 无边框全屏游戏都是非 topmost，仍会被小组件盖住。
/// - `demoted = false` → `HWND_TOPMOST`：常态，恢复到与任务栏同层。
///
/// 返回 `*mut c_void` 的指针值（`isize`）而不是 `HWND`，是为了让这张真值表不依赖
/// 任何窗口 / 系统状态；调用方用 [`insert_after_hwnd`] 还原成 `HWND`。
fn z_insert_after(demoted: bool) -> isize {
    if demoted {
        HWND_BOTTOM.0 as isize
    } else {
        HWND_TOPMOST.0 as isize
    }
}

/// 把 [`z_insert_after`] 的指针值还原成 `SetWindowPos` 需要的 `HWND`。
fn insert_after_hwnd(demoted: bool) -> HWND {
    HWND(z_insert_after(demoted) as *mut core::ffi::c_void)
}

/// 窗口样式组合（§10.1 逐字照抄）。
pub fn widget_ex_style() -> WINDOW_EX_STYLE {
    WS_EX_LAYERED      // 逐像素 alpha + UpdateLayeredWindow
        | WS_EX_TOOLWINDOW // 不出现在 Alt+Tab / 任务视图
        | WS_EX_NOACTIVATE // 点击不抢焦点
        | WS_EX_TOPMOST // 与任务栏同层，保证在上
}

/// 小组件窗口的宿主。
pub struct TaskbarHost {
    hwnd: HWND,
    visible: bool,
    /// 全屏遮挡态：窗口仍 visible，只是 Z 序被压到 `HWND_BOTTOM`（v1.5.6 / G3.5-A）。
    z_demoted: bool,
}

impl TaskbarHost {
    /// 创建窗口（初始隐藏）。`user_data` 会被写进 `GWLP_USERDATA`，供 WndProc 取回 `App*`。
    pub fn attach(hinstance: HINSTANCE, target: RECT, user_data: isize) -> Result<Self> {
        let x = target.left;
        let y = target.top;
        let w = (target.right - target.left).max(1);
        let h = (target.bottom - target.top).max(1);

        // SAFETY: 类名已由 ui::widget_wnd::register_classes 注册；父窗口传 None
        // 保证是 top-level（见模块注释）；lpparam 传 None，user_data 在创建后用
        // SetWindowLongPtrW 补写，避免依赖 WM_NCCREATE 的时序。
        let hwnd = unsafe {
            CreateWindowExW(
                widget_ex_style(),
                WIDGET_CLASS_NAME,
                WIDGET_TITLE,
                WS_POPUP,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(hinstance),
                None,
            )?
        };

        let host = Self {
            hwnd,
            visible: false,
            z_demoted: false,
        };

        if user_data != 0 {
            // SAFETY: hwnd 刚创建且有效；GWLP_USERDATA 只存一个指针值，不解引用。
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                    hwnd,
                    windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    user_data,
                );
            }
        }

        crate::log_info!(
            "小组件窗口已创建：hwnd=0x{:X} 位置=({x},{y}) 尺寸={w}×{h}",
            hwnd.0 as usize
        );
        Ok(host)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// 当前是否处于「Z 序已降级」的全屏遮挡态。
    pub fn z_demoted(&self) -> bool {
        self.z_demoted
    }

    /// 幂等地切换 Z 序（v1.5.6 / G3.5-A）。
    ///
    /// **永不隐藏窗口**：只调一次 `SetWindowPos`，`SWP_NOMOVE | SWP_NOSIZE |
    /// SWP_NOACTIVATE` 保证位置、尺寸、焦点都不变。状态相同直接返回 `Ok(())`，
    /// 因此 `App::apply_fullscreen_occlusion` 可以每帧重复调用而不产生重复副作用。
    pub fn set_z_demoted(&mut self, demoted: bool) -> Result<()> {
        if self.z_demoted == demoted {
            return Ok(());
        }
        let target = insert_after_hwnd(demoted);
        // SAFETY: hwnd 有效且由本结构体独占；目标锚点是系统预定义常量
        // （HWND_BOTTOM / HWND_TOPMOST），三个 SWP_* 保证不移动、不改尺寸、不激活。
        let switched = unsafe {
            SetWindowPos(
                self.hwnd,
                Some(target),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
        // v1.5.6 补丁：**只有 SetWindowPos 成功后**才翻转 `self.z_demoted`。
        // 失败只记 debug 并保持旧值，留给 1s 只读 EXSTYLE 对账 / 下一次
        // `apply_fullscreen_occlusion` 重试——绝不出现「状态说已降级、窗口其实还在 topmost」。
        match switched {
            Ok(()) => {
                self.z_demoted = demoted;
                crate::log_info!(
                    "小组件 Z 序切换 → {}（hwnd=0x{:X}）",
                    if demoted { "BOTTOM" } else { "TOPMOST" },
                    self.hwnd.0 as usize
                );
                Ok(())
            }
            Err(e) => {
                crate::log_debug!(
                    "小组件 Z 序切换失败，保持旧值 z_demoted={}：{e}",
                    self.z_demoted
                );
                Err(e)
            }
        }
    }

    /// 无条件按当前 `z_demoted` **再发一次** Z 序（v1.5.6 / G3.5-A）。
    ///
    /// 供 Explorer 重启、appbar 的 `ABN_POSCHANGED` / `ABN_STATECHANGE`、
    /// 冻结期只读对账发现「被外部重新置顶」时使用。不做几何探测、不重绘。
    pub fn reassert_z_order(&self) -> Result<()> {
        let target = insert_after_hwnd(self.z_demoted);
        // SAFETY: 同 set_z_demoted；只读 self 的布尔状态，不改变宿主内部状态。
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(target),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )?;
        }
        Ok(())
    }

    /// 幂等地把窗口挪到新位置 / 新尺寸，并重新置顶。
    ///
    /// 用 `SWP_NOACTIVATE` 保证移动不会激活窗口（否则会打断用户当前输入）。
    pub fn apply_geometry(&mut self, target: RECT) -> Result<()> {
        let w = (target.right - target.left).max(1);
        let h = (target.bottom - target.top).max(1);
        // v1.5.6 / G3.5-A：所有 relayout / 可见性变更路径的 Z 序总闸——
        // 遮挡态下任何几何应用都必须继续用 HWND_BOTTOM，否则一次重排就会把小组件
        // 重新抬回全屏窗口上方。其余参数与 flags 一字不改。
        // SAFETY: hwnd 有效；选中锚点 + SWP_NOACTIVATE 的组合不会改变焦点。
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(insert_after_hwnd(self.z_demoted)),
                target.left,
                target.top,
                w,
                h,
                SWP_NOACTIVATE,
            )?;
        }
        Ok(())
    }

    /// 读窗口**实际**的屏幕矩形（诊断用，v1.2.0）。
    ///
    /// 为什么需要它：`target_rect` 是我们「以为」窗口在哪，
    /// 而 `UpdateLayeredWindow` 的 `pptdst` 会改位置——两条路径一旦不一致，
    /// 小组件就会「莫名位移」。G5 的「截图时左移」就是靠对比这两个矩形定位的。
    pub fn window_rect(&self) -> Option<RECT> {
        let mut r = RECT::default();
        // SAFETY: hwnd 有效；r 是本地栈变量，GetWindowRect 只写它。
        match unsafe { GetWindowRect(self.hwnd, &mut r) } {
            Ok(()) => Some(r),
            Err(_) => None,
        }
    }

    /// 仅重新置顶（z-order 维持，§10.5）。
    pub fn raise(&self) -> Result<()> {
        // v1.5.6 / G3.5-A：遮挡态下绝不把小组件抬回 topmost 带（双保险，
        // App::ensure_on_top 里另有一层门控）。
        if self.z_demoted {
            return Ok(());
        }
        // SAFETY: 同上；尺寸位置都不变，只调整 z-order。
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
            )?;
        }
        Ok(())
    }

    /// 呈现一帧。
    ///
    /// `hdc` 里的内容必须是**预乘 alpha 的 BGRA**，且与 `(w, h)` 一致。
    /// `crkey` 在 `ULW_ALPHA` 模式下被忽略，但签名要求传值（不是 `Option`）。
    pub fn present(&self, hdc: HDC, x: i32, y: i32, w: i32, h: i32) -> Result<()> {
        let dst = POINT { x, y };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        // SAFETY: hdcdst 传 None + pptdst 传 Some → 窗口位置由 pptdst 决定（两者不能混用，
        // 混用会让窗口跑到 (0,0)，见 T04 易错点）。hdcsrc 是有效的内存 DC。
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&dst),
                Some(&size),
                Some(hdc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }

    /// 显示 / 隐藏。显示用 `SW_SHOWNOACTIVATE`，绝不激活窗口。
    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        // SAFETY: hwnd 有效；两个 show 命令都不改变焦点。
        unsafe {
            let _ = ShowWindow(
                self.hwnd,
                if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
            );
        }
        self.visible = visible;
        // v1.5.6 / G3.5-A：遮挡态下 ShowWindow 之后补一次 Z 序对账（防御性、幂等）。
        // 绝不在这里 SW_HIDE 全屏遮挡——「只调 Z 序、永不隐藏」是本轮铁律。
        if visible && self.z_demoted {
            let _ = self.reassert_z_order();
        }
        crate::log_debug!("小组件窗口可见性 → {visible}");
    }

    /// 销毁窗口（退出路径）。`Drop` 里不自动做，避免和 `PostQuitMessage` 抢时序。
    pub fn destroy(self) {
        // SAFETY: hwnd 由本结构体独占；DestroyWindow 后句柄失效，self 随即被消耗。
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v1.5.6 / G3.5-A：Z 序锚点常量映射真值表。
    /// 关键回归：**降级必须是 `HWND_BOTTOM` 而不是 `HWND_NOTOPMOST`**——
    /// 后者仍会盖在「非 topmost 的全屏窗口」上方。
    #[test]
    fn z_insert_after_maps_demoted_to_bottom_and_normal_to_topmost() {
        assert_eq!(z_insert_after(false), HWND_TOPMOST.0 as isize);
        assert_eq!(z_insert_after(true), HWND_BOTTOM.0 as isize);
        assert_ne!(z_insert_after(true), z_insert_after(false));

        // 常量本身的取值（windows 0.62.2）：BOTTOM=1 / TOPMOST=-1 / NOTOPMOST=-2。
        assert_eq!(HWND_BOTTOM.0 as isize, 1);
        assert_eq!(HWND_TOPMOST.0 as isize, -1);
        assert_eq!(
            windows::Win32::UI::WindowsAndMessaging::HWND_NOTOPMOST.0 as isize,
            -2
        );
        assert_ne!(
            z_insert_after(true),
            windows::Win32::UI::WindowsAndMessaging::HWND_NOTOPMOST.0 as isize,
            "HWND_NOTOPMOST 不能用于降级"
        );

        // 往返还原成 HWND 后必须逐位一致。
        assert_eq!(insert_after_hwnd(false), HWND_TOPMOST);
        assert_eq!(insert_after_hwnd(true), HWND_BOTTOM);
    }

    /// 幂等性：同一个 want 反复调用得到同一个锚点，不会漂移。
    #[test]
    fn z_insert_after_is_stable_under_repeated_calls() {
        for want in [false, true, true, false, false] {
            let first = z_insert_after(want);
            for _ in 0..8 {
                assert_eq!(z_insert_after(want), first);
                assert_eq!(insert_after_hwnd(want), HWND(first as *mut core::ffi::c_void));
            }
        }
    }
}
