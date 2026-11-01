//! COM 套间（apartment）RAII 封装 + WinRT 异步调用的计时外壳。
//!
//! **铁律（ARCHITECTURE.md §6.3）**：
//! 1. `IAsyncOperation::join()` 只能在 **MTA** 线程调用。在 STA（T0/UI 线程）上阻塞等待
//!    一个需要消息泵派发的完成回调 → 死锁。
//! 2. 因此本模块**不提供**在 STA 上 join 的入口；需要异步的 UI 侧代码必须走完成回调。
//! 3. 每次 `join()` 外面都要包一层 [`timed_join`]，把 > 2s 记 WARN、> 5s 记 ERROR，
//!    否则「SMTC 服务卡死」这种故障在日志里完全看不见。

use std::time::Instant;

use windows::core::Result;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
};

/// 一个已初始化的 COM 套间。`Drop` 时自动 `CoUninitialize`。
pub struct ComApartment {
    active: bool,
}

impl ComApartment {
    /// STA：UI 线程（T0）专用。WebView2 的 controller 要求宿主线程是 STA。
    pub fn sta() -> Option<Self> {
        Self::init(COINIT_APARTMENTTHREADED)
    }

    /// MTA：媒体线程（T1）与采样线程（T3）专用。只有 MTA 上才能安全 `join()`。
    pub fn mta() -> Option<Self> {
        Self::init(COINIT_MULTITHREADED)
    }

    fn init(mode: COINIT) -> Option<Self> {
        // SAFETY: CoInitializeEx 对同一线程重复调用是合法的：
        //   S_OK    —— 首次初始化成功
        //   S_FALSE —— 已用同一模式初始化过（HRESULT::is_ok() 也为真）
        //   RPC_E_CHANGED_MODE —— 已用另一种模式初始化，此时我们不该 CoUninitialize
        // 只有 is_ok() 为真才把 active 置 true，保证 Drop 与初始化严格配对。
        let hr = unsafe { CoInitializeEx(None, mode) };
        Some(Self { active: hr.is_ok() })
    }

    /// 套间是否真的初始化成功。
    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: 与 init() 里成功的 CoInitializeEx 严格配对，且每线程只 drop 一次
            // （ComApartment 不是 Clone/Copy，也没有其他构造入口）。
            unsafe { CoUninitialize() };
        }
    }
}

/// 给一次阻塞式 WinRT 调用（`.join()`）套上计时与分级日志。
///
/// 用法：
/// ```ignore
/// let mgr = timed_join("RequestAsync", || {
///     GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.join()
/// })?;
/// ```
pub fn timed_join<T>(label: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let started = Instant::now();
    let result = f();
    let elapsed_ms = started.elapsed().as_millis();
    if elapsed_ms > 5_000 {
        crate::log_err!("join `{label}` 耗时 {elapsed_ms}ms（> 5s，视为异常）");
    } else if elapsed_ms > 2_000 {
        crate::log_warn!("join `{label}` 耗时 {elapsed_ms}ms（> 2s）");
    } else {
        crate::log_debug!("join `{label}` 耗时 {elapsed_ms}ms");
    }
    result
}

/// 指数退避（毫秒），用于启动期重试。`attempt` 从 0 开始。
pub fn backoff_ms(attempt: u32) -> u64 {
    const STEPS: [u64; 5] = [200, 400, 800, 1_600, 3_200];
    STEPS[(attempt as usize).min(STEPS.len() - 1)]
}
