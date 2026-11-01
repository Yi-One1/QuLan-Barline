//! 跨线程唤醒：`mpsc` + `PostMessageW`。
//!
//! `std::sync::mpsc::Receiver` 阻塞在 UI 线程上时**不会**被 `send()` 唤醒——
//! 消息泵和 channel 是两套机制。所以每次投递都要额外 `PostMessageW` 一下，
//! 让 `GetMessageW` 返回并把控制权交回 UI 线程（ARCHITECTURE.md §6.2）。

use std::sync::mpsc::{channel, Receiver, Sender};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// 工作线程 → UI 线程：有新事件待 drain。
pub const WM_BARLINE_WAKE: u32 = WM_APP + 1;
/// 托盘回调（T14 使用，M1 先占位，避免后续改常量值）。
pub const WM_BARLINE_TRAY: u32 = WM_APP + 2;
/// 第二个实例启动 → 唤起已有实例（打开设置页）。
pub const WM_BARLINE_SHOW_SETTINGS: u32 = WM_APP + 3;
/// `WH_MOUSE_LL` 钩子捕获到滚轮 → 投递给小组件窗口。
///
/// `wParam` 里放滚轮增量（有符号，正 = 上滚）。
/// 为什么用消息而不是直接调用：钩子回调虽然跑在 UI 线程的消息循环里，
/// 但用 `PostMessageW` 能让「钩子 → 业务」的路径与其它输入完全一致，
/// 也避免了在回调里重入消息泵的风险。
pub const WM_BARLINE_WHEEL: u32 = WM_APP + 4;

/// `HWND` 不实现 `Send`/`Sync`（`#[repr(transparent)] pub struct HWND(pub *mut c_void)`），
/// 跨线程持有窗口句柄必须自己包一层。
#[derive(Clone, Copy, Debug)]
pub struct WakeHwnd(pub HWND);

// SAFETY: HWND 本质是一个进程内不透明句柄值。Windows 明确允许任意线程对任意窗口调用
// PostMessageW（消息会被投递到该窗口所属线程的队列）；句柄失效时 PostMessageW 返回 Err，
// 调用方忽略即可。我们不通过该句柄解引用任何内存。
unsafe impl Send for WakeHwnd {}
// SAFETY: 同上。`&WakeHwnd` 只用于读取句柄值并调用线程安全的 PostMessageW。
unsafe impl Sync for WakeHwnd {}

impl WakeHwnd {
    /// 投递一个空消息唤醒 UI 线程。永不 panic、永不失败上抛。
    pub fn wake(&self) {
        // SAFETY: 见上方 unsafe impl 的说明；参数全为 0 的空消息，WndProc 只认消息号。
        unsafe {
            let _ = PostMessageW(Some(self.0), WM_BARLINE_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

/// 工作线程持有的事件投递器。
pub struct EventSink<T: Send + 'static> {
    tx: Sender<T>,
    wake: WakeHwnd,
}

impl<T: Send + 'static> EventSink<T> {
    /// 创建一对 (投递器, 接收器)。`wake` 必须是 UI 线程上那个消息窗口的句柄。
    pub fn new(wake: WakeHwnd) -> (Self, Receiver<T>) {
        let (tx, rx) = channel();
        (Self { tx, wake }, rx)
    }

    /// 从任意线程投递事件并唤醒 UI 线程。**永不 panic**。
    pub fn send(&self, evt: T) {
        if self.tx.send(evt).is_err() {
            // UI 线程已退出（接收器被 drop），静默丢弃——这是正常关机路径。
            return;
        }
        self.wake.wake();
    }

    /// 仅唤醒，不投递（用于「已经有数据在共享状态里，去看一眼」的场景）。
    pub fn poke(&self) {
        self.wake.wake();
    }
}

impl<T: Send + 'static> Clone for EventSink<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: self.wake,
        }
    }
}
