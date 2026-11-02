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
    CreateWindowExW, GetWindowRect, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST,
    SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_APP, WINDOW_EX_STYLE, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// 小组件窗口类名。单实例唤起时会 `FindWindowW` 它（`platform::single_instance`）。
pub const WIDGET_CLASS_NAME: PCWSTR = w!("BarlineWidgetWnd");
/// 窗口标题（不出现在任务栏 / Alt+Tab，仅用于诊断）。
pub const WIDGET_TITLE: PCWSTR = w!("曲栏");

/// `WM_APP` 之后的第一个自定义消息，留给小组件窗口内部使用。
pub const WM_BARLINE_WIDGET_BASE: u32 = WM_APP + 16;

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

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// 幂等地把窗口挪到新位置 / 新尺寸，并重新置顶。
    ///
    /// 用 `SWP_NOACTIVATE` 保证移动不会激活窗口（否则会打断用户当前输入）。
    pub fn apply_geometry(&mut self, target: RECT) -> Result<()> {
        let w = (target.right - target.left).max(1);
        let h = (target.bottom - target.top).max(1);
        // SAFETY: hwnd 有效；HWND_TOPMOST + SWP_NOACTIVATE 的组合不会改变焦点。
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
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
