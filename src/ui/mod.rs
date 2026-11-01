//! UI 层：窗口过程、命中测试、交互状态机、右键菜单、消息泵。

pub mod hit;
pub mod interaction;
pub mod menu;
pub mod widget_wnd;

use windows::core::Result;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, TranslateMessage, MSG,
};

/// 标准 Win32 消息泵。返回退出码。
///
/// `GetMessageW` 的三个返回值：`> 0` 正常取到消息、`0` 收到 `WM_QUIT`、`< 0` 出错。
/// 只有 `0` 是正常退出。
pub fn run_message_loop() -> i32 {
    let mut msg = MSG::default();
    loop {
        // SAFETY: msg 是本地栈结构体；hwnd 传 None 表示接收本线程所有窗口的消息。
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            crate::log_info!("收到 WM_QUIT，消息泵退出");
            return msg.wParam.0 as i32;
        }
        if r.0 < 0 {
            crate::log_err!("GetMessageW 返回错误（{}），消息泵退出", r.0);
            return -1;
        }
        // SAFETY: msg 由 GetMessageW 填充，可直接交给这两个 API。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 注册窗口类并创建消息窗。返回消息窗句柄。
pub fn install(
    hinstance: windows::Win32::Foundation::HINSTANCE,
) -> Result<windows::Win32::Foundation::HWND> {
    widget_wnd::register_classes(hinstance)?;
    widget_wnd::create_msg_window(hinstance, 0)
}
