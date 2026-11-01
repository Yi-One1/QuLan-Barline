//! 单实例（ARCHITECTURE.md §14.2 / PRD P0-13）。
//!
//! 做法：`CreateMutexW` + `ERROR_ALREADY_EXISTS`。互斥体名用 `Local\` 前缀，
//! 这样只在当前登录会话内生效——跨用户 / 跨 RDP 会话互不干扰（PRD 要求）。
//!
//! **注意**：互斥体句柄必须在进程生命周期内保持打开。一旦 drop，第二个进程会
//! 误判为「没有实例」，于是出现两个小组件。因此这里把它存进 `OnceLock`，永不释放
//! （进程退出时由内核回收）。

use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Foundation::{GetLastError, HWND, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

use crate::platform::wake::WM_BARLINE_SHOW_SETTINGS;

/// 已持有的互斥体句柄（存成 `isize` 以满足 `OnceLock` 的 `Sync` 要求；
/// `HANDLE` 内部的 `*mut c_void` 不是 `Sync`）。
static MUTEX_HANDLE: OnceLock<isize> = OnceLock::new();

/// 尝试成为主实例。
///
/// - 返回 `true`：本进程是唯一实例，继续启动。
/// - 返回 `false`：已有实例在跑，调用方应 [`wake_existing`] 后立即退出。
///
/// 拿不到互斥体（极罕见，例如权限问题）时**返回 `true`**：宁可多开一个，
/// 也不要因为一个非核心功能把整个程序卡在启动失败。
pub fn acquire() -> bool {
    // SAFETY: CreateMutexW 只创建一个内核对象；名字是静态字面量；
    // 我们刻意不释放返回的句柄（见模块注释），因此不存在 use-after-free。
    let created = unsafe { CreateMutexW(None, false, w!("Local\\Barline.SingleInstance.v1")) };

    match created {
        Ok(handle) => {
            // SAFETY: GetLastError 是纯读取；CreateMutexW 成功返回时
            // windows crate 不会清掉它（ok_or_else 是惰性的）。
            let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            // 句柄值转 isize 存起来，进程退出前不再使用。
            let _ = MUTEX_HANDLE.set(handle.0 as isize);
            if already {
                crate::log_warn!("已有实例在运行（互斥体 Local\\Barline.SingleInstance.v1 已存在）");
                false
            } else {
                crate::log_info!("单实例互斥体创建成功，本进程是主实例");
                true
            }
        }
        Err(e) => {
            crate::log_warn!("CreateMutexW 失败（{e}），跳过单实例检查继续启动");
            true
        }
    }
}

/// 唤起已有实例：找到小组件窗口，投递 `WM_BARLINE_SHOW_SETTINGS`。
///
/// 返回是否成功投递。找不到窗口（例如已有实例还在启动中）返回 `false`，
/// 但调用方仍然应该退出——单实例语义优先。
pub fn wake_existing() -> bool {
    // SAFETY: FindWindowW / PostMessageW 都是只读句柄 + 跨线程安全的投递，
    // 不会解引用任何我们持有的指针。
    unsafe {
        let widget: windows::core::Result<HWND> = FindWindowW(w!("BarlineWidgetWnd"), None);
        let target = match widget {
            Ok(h) if !h.is_invalid() => Some(h),
            _ => FindWindowW(w!("BarlineMsgWnd"), None).ok(),
        };
        match target {
            Some(h) if !h.is_invalid() => {
                let ok = PostMessageW(Some(h), WM_BARLINE_SHOW_SETTINGS, Default::default(), Default::default()).is_ok();
                if ok {
                    crate::log_info!("已有实例，已唤起（已投递 WM_BARLINE_SHOW_SETTINGS）");
                } else {
                    crate::log_warn!("已有实例，但 PostMessageW 投递失败");
                }
                ok
            }
            _ => {
                crate::log_warn!("已有实例，但找不到它的窗口（可能仍在启动中）");
                false
            }
        }
    }
}
