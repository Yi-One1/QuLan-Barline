//! 纯通知型 appbar（v1.5.6 / G3.5-A）：只发 `ABM_NEW` 收系统通知，不占工作区。
//!
//! **故意不发 `ABM_QUERYPOS` / `ABM_SETPOS`**：我们不申请任务栏边缘、不改工作区矩形、
//! 不让 Explorer 参与布局；注册的唯一目的是收到 `ABN_FULLSCREENAPP`（全屏应用
//! 打开 / 关闭）、`ABN_POSCHANGED` / `ABN_STATECHANGE` 这些通知，从而在用户切换
//! 全屏的**同一帧**调整小组件 Z 序。1s 心跳只作兜底对账，不是主路径。
//!
//! `SHAppBarMessage` 依赖 `Shell_TrayWnd` 存在：Explorer 重启期间会失败（返回 0）。
//! 失败只记 warn、不上抛，靠 `TaskbarCreated` 广播触发
//! [`crate::app::App::on_taskbar_created`] → `appbar_reregister()` 重新注册。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA, ABM_NEW, ABM_REMOVE};

/// 构造一条最小 `APPBARDATA`。
///
/// `cbSize` 的写法逐字照抄 `taskbar::geometry.rs`（`ABM_GETTASKBARPOS` 那处）——
/// **禁止硬编码**：结构体在 x86 / x64 下布局不同，写死会在 32 位构建上静默失败。
fn appbar_data(hwnd: HWND, cb_msg: u32) -> APPBARDATA {
    APPBARDATA {
        cbSize: core::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: cb_msg,
        ..Default::default()
    }
}

/// 注册纯通知型 appbar。返回 `true` = `ABM_NEW` 成功。
///
/// 失败（Explorer 正在重启 / `Shell_TrayWnd` 暂不存在）只记 warn，不 panic、
/// 不把 `Result` 上抛——调用方据此降级为「只靠 1s 心跳兜底」，功能不受影响。
pub fn register(hwnd: HWND, cb_msg: u32) -> bool {
    let mut abd = appbar_data(hwnd, cb_msg);
    // SAFETY: abd 是本地结构体，cbSize / hWnd / uCallbackMessage 已填；
    // SHAppBarMessage(ABM_NEW) 只读写该结构体；返回值非 0 表示成功。
    let ok = unsafe { SHAppBarMessage(ABM_NEW, &mut abd) } != 0;
    if ok {
        crate::log_info!(
            "appbar: ABM_NEW 成功（hwnd=0x{:X} callback=0x{:X}）→ 全屏通知走事件驱动",
            hwnd.0 as usize,
            cb_msg
        );
    } else {
        crate::log_warn!(
            "appbar: ABM_NEW 失败（Shell_TrayWnd 可能暂不存在），降级为 1s 心跳兜底，等待 TaskbarCreated 重注册"
        );
    }
    ok
}

/// 注销 appbar（退出 / 重注册前）。best-effort，失败只记 warn。
pub fn unregister(hwnd: HWND) {
    let mut abd = appbar_data(hwnd, 0);
    // SAFETY: 同上；ABM_REMOVE 不需要回调消息号，句柄来自本进程注册记录。
    let ok = unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) } != 0;
    if ok {
        crate::log_info!("appbar: ABM_REMOVE 成功（hwnd=0x{:X}）", hwnd.0 as usize);
    } else {
        crate::log_warn!("appbar: ABM_REMOVE 失败（Explorer 可能正在重启），忽略");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 结构体构造不触发任何 Win32 调用，可在无 Explorer / 无窗口的测试进程里验证。
    #[test]
    fn appbar_data_fills_size_hwnd_and_callback_message() {
        let hwnd = HWND(0x1234_usize as *mut core::ffi::c_void);
        let abd = appbar_data(hwnd, 0x8123);
        assert_eq!(
            abd.cbSize as usize,
            core::mem::size_of::<APPBARDATA>(),
            "cbSize 必须等于结构体真实大小，否则 SHAppBarMessage 静默失败"
        );
        assert_eq!(abd.hWnd, hwnd);
        assert_eq!(abd.uCallbackMessage, 0x8123);
        // 纯通知型：绝不申请边缘 / 不改工作区矩形 / 不带附加参数。
        assert_eq!(abd.uEdge, 0);
        assert_eq!(abd.rc, windows::Win32::Foundation::RECT::default());
        assert_eq!(abd.lParam.0, 0);

        // 消息号语义：ABM_NEW=0 / ABM_REMOVE=1，别和 ABN_* 通知码搞混。
        assert_eq!(ABM_NEW, 0);
        assert_eq!(ABM_REMOVE, 1);
    }

    /// 不同回调消息号 / 句柄不得串味，且 cbSize 恒定。
    #[test]
    fn appbar_data_is_independent_per_call() {
        let a = appbar_data(HWND(std::ptr::null_mut()), 0x8001);
        let b = appbar_data(HWND(0x55_usize as *mut core::ffi::c_void), 0x8002);
        assert_eq!(a.cbSize, b.cbSize);
        assert_ne!(a.hWnd, b.hWnd);
        assert_ne!(a.uCallbackMessage, b.uCallbackMessage);
        assert_eq!(a.uEdge, b.uEdge);
    }
}
