//! 分段耗时埋点（G2 随机卡顿诊断，v1.4.1 诊断版；v1.4.4 分级阈值 + 同段限流）。
//!
//! ## 为什么需要它
//!
//! 用户报「音乐和鼠标同时卡一下、时机完全随机」。静态审计只能给出**嫌疑**，
//! 落地要靠日志把「卡的那一下究竟是哪一段」指出来——而且**卡顿复发时**还能再抓一次。
//!
//! ## 设计约束（平时必须零成本）
//!
//! - 快路径只有一次 `Instant::now()` 与一次减法比较：**不分配、不格式化、不进日志缓冲区**；
//! - 只有单段 ≥ 该段的阈值才写**一条** `log_warn!`，格式固定为
//!   `[perf] <段名> 耗时=NNNms <可选备注>`（便于 grep 与脚本统计）；
//! - [`Span`] 是 RAII：`Drop` 时结算，任何 `?` / 提前 `return` 都不会漏记；
//!   显式调用 [`Span::finish`] 之后 `Drop` **不会**重复记。
//!
//! ## 为什么不做「全局 span 栈 / 父子嵌套」
//!
//! 嵌套要在每条记录上多两次栈操作，还要把父 span 的地址跨函数传递；而这里要回答的
//! 问题只是「哪一段慢」。每段自带 `Instant` 就够了，父子关系从段名
//! （`tick.probe` / `paint.frame` / `msg.mouse`）本身就能读出来。
//! 代价是嵌套段的耗时**会重叠**（`msg.mouse` 里包含 `paint.frame`），解读时注意即可。
//!
//! ## v1.4.4（G2 残余卡顿续查）加了什么
//!
//! 1. **分级阈值**（[`threshold_for`]）：`tick.*` 的 4 段与 `sampler.capture` 改按
//!    [`UI_SLOW_MS`]（16ms，一帧 16.7ms）判定，其余段保持 [`SLOW_MS`]（50ms）/
//!    [`SMTC_SLOW_MS`]（8ms）。原因是旧口径有**结构性盲区**：每秒执行一次的
//!    `tick.*` 单次即便 20~40ms 也永远越不过 50ms，日志里「一条都没有」并不等于「不慢」。
//! 2. **同段限流**（[`RateLimiter`]）：阈值降到 16ms 后单段可能刷屏，所以同一段名
//!    60s 内最多记 [`LIMIT_MAX_PER_WINDOW`] 条，超出的静默丢弃，并**每窗口只提示一次**。
//! 3. **采样线程统计**（[`record_capture`] / [`take_capture_window`]）：抓屏次数 /
//!    慢次数 / 最慢 ms 用无锁原子累计，由每 60s 的统计行汇总——采样线程绝不加锁。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// 慢阈值（毫秒）：**≥ 它**才写日志。
///
/// 50ms 的取法：60Hz 一帧是 16.7ms，人眼能明确感知「顿一下」大约从 40~50ms 起；
/// 阈值再低只会把日志淹掉，反而看不出随机卡顿的形态。
///
/// v1.4.4 起它只覆盖**非 UI 心跳**段（`paint.frame` / `cover.decode` / 采样统计等）；
/// UI 线程上每秒跑一次的那几段改用更敏感的 [`UI_SLOW_MS`]，见 [`threshold_for`]。
pub const SLOW_MS: u128 = 50;

/// **跨进程调用**专用的慢阈值（毫秒）。
///
/// 依据（G2 实测）：SMTC 的单次调用打到播放器进程后，正常在 1~3ms；
/// 一旦超过 8ms 就说明对面进程被我们拖住了——而那正是「音乐卡一下」的成因，
/// 所以这类段必须比 50ms 敏感得多，否则等到 50ms 记下来时已经晚了两轮。
pub const SMTC_SLOW_MS: u128 = 8;

/// **UI 心跳段**的慢阈值（毫秒）。
///
/// 取 16ms 的理由：一帧 16.7ms，UI 线程上 16ms 的停顿已经**可感知**（掉一帧）；
/// 而 `tick.capture / tick.probe / tick.ontop / tick.backdrop` 每秒只跑一次，
/// 单次 20~40ms 在 50ms 口径下永远静默 —— 那正是 G2 日志里的结构性盲区。
pub const UI_SLOW_MS: u128 = 16;

/// **采样线程抓屏段**的慢阈值（毫秒），与 [`UI_SLOW_MS`] 同口径。
///
/// `sampler.capture` 走 `GetDC(NULL)` + `BitBlt`，会强制 GPU→CPU 同步、
/// 和 DWM 抢锁；16ms 就足以让播放器掉一帧，所以它不能等 50ms。
pub const SAMPLER_SLOW_MS: u128 = 16;

/// 走 16ms 口径的段名（唯一清单，[`threshold_for`] 与单测共用）。
///
/// 只有这 5 段用 16ms：**其余段（`paint.frame` / `cover.decode` / `smtc.*` …）
/// 一律保持原阈值**，避免把「正常的一帧」全部记成慢。
pub const UI_SLOW_SEGMENTS: [&str; 5] = [
    "tick.capture",
    "tick.probe",
    "tick.ontop",
    "tick.backdrop",
    "sampler.capture",
];

/// 该段该用哪个阈值（纯函数，单测直接断言）。
///
/// 段名 → 阈值是**唯一口径**：`Span::new` 会自动套用它，所以调用点不需要
/// 记住「这段该不该敏感」——漏写就等于没埋点，那种错误在日志里看不出来。
pub fn threshold_for(name: &str) -> u128 {
    if UI_SLOW_SEGMENTS.contains(&name) {
        UI_SLOW_MS
    } else {
        SLOW_MS
    }
}

/// 是否值得记一条（纯函数，单测直接断言）。
pub fn is_slow(ms: u128) -> bool {
    over(ms, SLOW_MS)
}

/// 「这段耗时算不算慢」的唯一判定（含边界：等于阈值就算）。
pub fn over(ms: u128, threshold_ms: u128) -> bool {
    ms >= threshold_ms
}

// ---------------------------------------------------------------- 同段限流

/// 同一个段名在一个窗口内最多记多少条。
pub const LIMIT_MAX_PER_WINDOW: u32 = 10;

/// 限流窗口长度（毫秒）。与 60s 统计行同周期，方便对照。
pub const LIMIT_WINDOW_MS: u64 = 60_000;

/// 限流表的槽位数。段总数远小于它，线性探测足够；满了就退化为「共享额度」。
const LIMIT_SLOTS: usize = 32;

/// 一次记录请求的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 额度内，正常记一条。
    Allow,
    /// 本窗口刚记满：**这一条不记**，只发一行「后续丢弃」的提示（每窗口仅一次）。
    Notice,
    /// 超额度且提示已发过：静默丢弃。
    Drop,
}

/// 一个段名在当前窗口里的记账。
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// 段名；空串表示空槽
    name: &'static str,
    /// 当前窗口的起点（相对进程启动的毫秒）
    window_start_ms: u64,
    /// 本窗口已记条数
    count: u32,
    /// 本窗口的「已记满」提示是否发过
    noticed: bool,
}

/// 空槽的常量值（`[Slot; N]` 的静态初始化用）。
const EMPTY_SLOT: Slot = Slot {
    name: "",
    window_start_ms: 0,
    count: 0,
    noticed: false,
};

/// 按段名的限流器（纯逻辑，时间由调用方传入 → 可单测，不用 sleep）。
///
/// 只在**确实要写日志**时才被调用（[`Span::report`] 里 `over()` 之后），
/// 所以快路径既不碰它也不拿锁。
#[derive(Debug)]
pub struct RateLimiter {
    slots: [Slot; LIMIT_SLOTS],
}

impl RateLimiter {
    /// 全空表。
    pub const fn new() -> Self {
        Self {
            slots: [EMPTY_SLOT; LIMIT_SLOTS],
        }
    }

    /// 申请一条记录额度。`now_ms` 是单调毫秒（相对任意固定起点）。
    pub fn admit(&mut self, name: &'static str, now_ms: u64) -> Verdict {
        let start = (hash_name(name) % LIMIT_SLOTS as u64) as usize;
        let mut free: Option<usize> = None;
        let mut idx = start;
        for _ in 0..LIMIT_SLOTS {
            let slot = &mut self.slots[idx];
            if slot.name.is_empty() {
                if free.is_none() {
                    free = Some(idx);
                }
            } else if slot.name == name {
                return Self::touch(slot, now_ms);
            }
            idx = (idx + 1) % LIMIT_SLOTS;
        }
        // 没有同名槽：占第一个空槽；表满则退回哈希槽（与别的段共享额度，
        // 宁可少记也不刷屏——日志总量有上限才是这里的目的）。
        let idx = free.unwrap_or(start);
        self.slots[idx] = Slot {
            name,
            window_start_ms: now_ms,
            count: 0,
            noticed: false,
        };
        Self::touch(&mut self.slots[idx], now_ms)
    }

    /// 滚动窗口 + 计数。窗口过期先清零，再判额度。
    fn touch(slot: &mut Slot, now_ms: u64) -> Verdict {
        if now_ms.saturating_sub(slot.window_start_ms) >= LIMIT_WINDOW_MS {
            slot.window_start_ms = now_ms;
            slot.count = 0;
            slot.noticed = false;
        }
        if slot.count < LIMIT_MAX_PER_WINDOW {
            slot.count += 1;
            Verdict::Allow
        } else if !slot.noticed {
            slot.noticed = true;
            Verdict::Notice
        } else {
            Verdict::Drop
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局限流表。**只有「要写日志」的路径才会拿这把锁**。
static LIMITER: Mutex<RateLimiter> = Mutex::new(RateLimiter::new());

/// 进程启动后的单调毫秒（限流窗口用；首次调用时定起点）。
fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 向全局限流表申请额度（日志路径专用）。
fn admit_record(name: &'static str) -> Verdict {
    // 毒化不该让埋点失效：拿回内部数据继续用（埋点比一致性更重要）。
    let mut guard = match LIMITER.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.admit(name, now_ms())
}

/// 段名 → 槽位的哈希（FNV-1a 64，够散且不分配）。
fn hash_name(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---------------------------------------------------------------- 采样线程统计

/// 采样线程抓屏段在一个窗口内的累计（A4）。
///
/// 用无锁原子而不是 Mutex / 通道：采样线程在 `BitBlt` 前后各一次 `fetch_add`
/// 就是全部开销，**绝不会**因为「UI 线程正在读统计」而阻塞。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureWindow {
    /// 抓屏次数（含失败）
    pub count: u64,
    /// 其中耗时 ≥ [`SAMPLER_SLOW_MS`] 的次数
    pub slow: u64,
    /// 窗口内最慢的一次（毫秒）
    pub max_ms: u64,
}

/// 抓屏计数器（无锁）。全局只用一个实例，单测另建实例以免互相串味。
#[derive(Debug)]
pub struct CaptureStats {
    count: AtomicU64,
    slow: AtomicU64,
    max_ms: AtomicU64,
}

impl CaptureStats {
    /// 全零。
    pub const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            max_ms: AtomicU64::new(0),
        }
    }

    /// 记一次抓屏耗时（采样线程调用，无锁、无分配、无格式化）。
    pub fn record(&self, ms: u128) {
        let ms64 = ms.min(u64::MAX as u128) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        if over(ms, SAMPLER_SLOW_MS) {
            self.slow.fetch_add(1, Ordering::Relaxed);
        }
        self.max_ms.fetch_max(ms64, Ordering::Relaxed);
    }

    /// 取走本窗口的统计并清零（每 60s 统计行调用一次）。
    ///
    /// 「取走即清零」让这一行和其他字段（`playback` / `props` / `timeline`）同语义：
    /// 都是**本窗口**的数字，不是开机以来的累计。
    pub fn take(&self) -> CaptureWindow {
        CaptureWindow {
            count: self.count.swap(0, Ordering::Relaxed),
            slow: self.slow.swap(0, Ordering::Relaxed),
            max_ms: self.max_ms.swap(0, Ordering::Relaxed),
        }
    }
}

impl Default for CaptureStats {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局抓屏计数器（采样线程写、媒体线程每 60s 读走）。
static CAPTURE_STATS: CaptureStats = CaptureStats::new();

/// 记一次抓屏耗时（采样线程调用）。
pub fn record_capture(ms: u128) {
    CAPTURE_STATS.record(ms);
}

/// 取走本窗口的抓屏统计并清零（60s 统计行调用）。
pub fn take_capture_window() -> CaptureWindow {
    CAPTURE_STATS.take()
}

// ---------------------------------------------------------------- Span

/// 一段耗时。RAII：活到作用域结束就自动结算。
#[must_use = "Span 必须在作用域内存活才会计时（立即丢弃 = 0ms，什么也不会记）"]
pub struct Span {
    name: &'static str,
    /// 可选备注（段名之外的补充，只在慢记录里出现）
    note: &'static str,
    start: Instant,
    /// 本段的慢阈值（由 [`threshold_for`] 按段名自动选，或调用方显式指定）
    threshold: u128,
    /// 已经结算过（保证 Drop 只记一次）
    done: bool,
    /// 单测用：收进线程本地向量而不是写日志文件
    #[cfg(test)]
    collect: bool,
}

impl Span {
    /// 开始计一段。阈值由 [`threshold_for`] 按段名自动选择。
    pub fn new(name: &'static str) -> Self {
        Self::with_threshold(name, threshold_for(name))
    }

    /// 开始计一段，并在慢记录里附带一句备注。
    pub fn with_note(name: &'static str, note: &'static str) -> Self {
        Self {
            name,
            note,
            start: Instant::now(),
            threshold: threshold_for(name),
            done: false,
            #[cfg(test)]
            collect: false,
        }
    }

    /// 开始计一段，**自带阈值**（绕过 [`threshold_for`]）。
    ///
    /// 为什么需要：同一套埋点里，跨进程调用的「慢」与一帧绘制的「慢」不是一个量级，
    /// 用同一个阈值必然二选一地失真（见 [`SMTC_SLOW_MS`]）。
    pub fn with_threshold(name: &'static str, threshold_ms: u128) -> Self {
        Self {
            name,
            note: "",
            start: Instant::now(),
            threshold: threshold_ms,
            done: false,
            #[cfg(test)]
            collect: false,
        }
    }

    /// 显式结算（等价于「提前结束」）。消费 `self`，之后的 `Drop` 不再记。
    pub fn finish(mut self) {
        self.report();
    }

    /// 丢弃这一段（不计、不记）。场景：中途发现「这段其实不该测」。
    pub fn cancel(mut self) {
        self.done = true;
    }

    /// 本段已耗时（毫秒）。诊断用，也可以在不结束的情况下读。
    pub fn elapsed_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }

    /// 本段阈值（单测与诊断用）。
    pub fn threshold_ms(&self) -> u128 {
        self.threshold
    }

    /// 结算：幂等，只有第一次生效。
    fn report(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let ms = self.elapsed_ms();
        if !over(ms, self.threshold) {
            return;
        }

        #[cfg(test)]
        {
            if self.collect {
                record_for_test(self.name, ms, self.note);
                return;
            }
        }

        // 限流只在「确实要写日志」时才发生（快路径到此为止，永不拿锁）。
        match admit_record(self.name) {
            Verdict::Allow => {
                if self.note.is_empty() {
                    crate::log_warn!("[perf] {} 耗时={}ms", self.name, ms);
                } else {
                    crate::log_warn!("[perf] {} 耗时={}ms {}", self.name, ms, self.note);
                }
            }
            Verdict::Notice => crate::log_warn!(
                "[perf] {} 60s 内已记满 {} 条，后续丢弃",
                self.name,
                LIMIT_MAX_PER_WINDOW
            ),
            Verdict::Drop => {}
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.report();
    }
}

// ---------------------------------------------------------------- 单测支撑

#[cfg(test)]
thread_local! {
    /// 线程本地记录：`Span` 都在单测的同一根栈上创建/销毁，
    /// 不进全局状态就不会被并行跑的其他用例串味。
    static RECORDED: std::cell::RefCell<Vec<(&'static str, u128, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn record_for_test(name: &'static str, ms: u128, note: &'static str) {
    RECORDED.with(|v| {
        if let Ok(mut g) = v.try_borrow_mut() {
            g.push((name, ms, note));
        }
    });
}

#[cfg(test)]
fn take_recorded() -> Vec<(&'static str, u128, &'static str)> {
    RECORDED.with(|v| {
        let mut g = match v.try_borrow_mut() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        std::mem::take(&mut *g)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 单测里只记录、不写日志。
    fn collecting(name: &'static str) -> Span {
        Span {
            collect: true,
            ..Span::new(name)
        }
    }

    /// 单测里只记录、不写日志，但用**自定义阈值**。
    fn collecting_with(name: &'static str, threshold_ms: u128) -> Span {
        Span {
            collect: true,
            ..Span::with_threshold(name, threshold_ms)
        }
    }

    /// 睡过阈值（+ 一点余量，避免 scheduler 抖动卡在边界上）。
    fn sleep_slow() {
        std::thread::sleep(Duration::from_millis(SLOW_MS as u64 + 15));
    }

    #[test]
    fn threshold_is_inclusive_at_50ms() {
        assert_eq!(SLOW_MS, 50, "阈值口径固定，改它等于改验收标准");
        assert!(!is_slow(0));
        assert!(!is_slow(49));
        assert!(is_slow(50), "恰好 50ms 就要记（≥ 不是 >）");
        assert!(is_slow(51));
    }

    /// 短的段什么都不记（否则 25fps 下日志会被淹掉）。
    #[test]
    fn fast_span_records_nothing() {
        let _ = take_recorded();
        {
            let _s = collecting("t.fast");
        }
        assert!(take_recorded().is_empty());
    }

    /// 慢的段记一条，带段名与毫秒数。
    #[test]
    fn slow_span_records_once() {
        let _ = take_recorded();
        {
            let _s = collecting("t.slow");
            sleep_slow();
        }
        let got = take_recorded();
        assert_eq!(got.len(), 1, "一段只该记一条");
        assert_eq!(got[0].0, "t.slow");
        assert!(got[0].1 >= SLOW_MS, "记录的耗时要过阈值：{}", got[0].1);
        assert_eq!(got[0].2, "", "无备注时备注为空串");
    }

    /// 显式 `finish()` 之后再 `Drop` **不许**重复记。
    #[test]
    fn finish_makes_drop_a_noop() {
        let _ = take_recorded();
        let s = collecting("t.finish");
        sleep_slow();
        s.finish();
        assert_eq!(take_recorded().len(), 1, "finish 已结算，Drop 不许再记");
    }

    /// `cancel()` 的段即使超过阈值也不记。
    #[test]
    fn cancelled_span_is_never_recorded() {
        let _ = take_recorded();
        let s = collecting("t.cancel");
        sleep_slow();
        s.cancel();
        assert!(take_recorded().is_empty());
    }

    /// 嵌套不串味：内层只算自己那段，外层覆盖两段。
    #[test]
    fn nested_spans_do_not_crosstalk() {
        let _ = take_recorded();
        let outer = collecting("t.outer");
        sleep_slow();
        {
            let _inner = collecting("t.inner");
            sleep_slow();
        }
        outer.finish();

        let got = take_recorded();
        assert_eq!(got.len(), 2, "内外各一条：{got:?}");
        let inner = got.iter().find(|r| r.0 == "t.inner").expect("缺内层记录");
        let outer = got.iter().find(|r| r.0 == "t.outer").expect("缺外层记录");
        assert!(
            inner.1 < outer.1,
            "内层 {}ms 不小于外层 {}ms → 计时起点串了",
            inner.1,
            outer.1
        );
        assert!(
            outer.1 >= 2 * SLOW_MS,
            "外层应覆盖内外两段，实际 {}ms",
            outer.1
        );
    }

    /// `elapsed_ms` 能在不结束的情况下读（诊断分支会用到）。
    #[test]
    fn elapsed_is_readable_without_finishing() {
        let _ = take_recorded();
        let s = collecting("t.peek");
        sleep_slow();
        assert!(s.elapsed_ms() >= SLOW_MS);
        s.cancel();
        assert!(take_recorded().is_empty(), "cancel 后不许残留记录");
    }

    // ------------------------------------------------ 自定义阈值（G2 修复版）

    /// 阈值判定**含边界**：恰好等于就算慢（`smtc.*` 的 8ms 也走同一个函数）。
    #[test]
    fn over_threshold_is_inclusive_for_any_threshold() {
        assert!(over(8, SMTC_SLOW_MS), "恰好 8ms 就要记（≥ 不是 >）");
        assert!(!over(7, SMTC_SLOW_MS));
        assert!(over(50, SLOW_MS));
        assert!(!over(49, SLOW_MS));
        // is_slow 是 50ms 的便捷入口，不许悄悄变语义
        assert_eq!(SLOW_MS, 50);
        assert_eq!(SMTC_SLOW_MS, 8);
        assert_eq!(is_slow(50), true);
        assert_eq!(is_slow(49), false);
    }

    /// 同样的耗时，自定义阈值（8ms）要记、默认阈值（50ms）**不许**记。
    #[test]
    fn custom_threshold_records_earlier_than_default() {
        let _ = take_recorded();
        // 12ms：够 8ms 阈值，不够 50ms 默认阈值
        {
            let _s = collecting_with("t.smtc", SMTC_SLOW_MS);
            std::thread::sleep(Duration::from_millis(SMTC_SLOW_MS as u64 + 4));
        }
        {
            let _s = collecting("t.default");
            std::thread::sleep(Duration::from_millis(SMTC_SLOW_MS as u64 + 4));
        }
        let got = take_recorded();
        assert_eq!(got.len(), 1, "只有自定义阈值那段该记：{got:?}");
        assert_eq!(got[0].0, "t.smtc");
    }

    /// 自定义阈值的段同样受「Drop 只记一次」约束。
    #[test]
    fn custom_threshold_finish_is_single_shot() {
        let _ = take_recorded();
        let s = collecting_with("t.smtc.finish", SMTC_SLOW_MS);
        std::thread::sleep(Duration::from_millis(SMTC_SLOW_MS as u64 + 4));
        s.finish();
        assert_eq!(take_recorded().len(), 1);
    }

    // ------------------------------------------------ v1.4.4：分级阈值（A2）

    /// 16ms 口径**只**给这 5 段；其余段一个都不许跟着变敏感。
    #[test]
    fn only_ui_segments_use_16ms() {
        assert_eq!(UI_SLOW_MS, 16, "16ms ≈ 一帧 16.7ms，改它等于改验收口径");
        assert_eq!(SAMPLER_SLOW_MS, 16);
        for name in UI_SLOW_SEGMENTS {
            assert_eq!(threshold_for(name), UI_SLOW_MS, "{name} 必须用 16ms");
        }
        // 相邻段一个都不许误伤：sampler.analyze 是纯 CPU 统计，paint.frame 是整帧绘制
        for name in [
            "paint.frame",
            "cover.decode",
            "sampler.analyze",
            "msg.mouse",
            "smtc.playback",
            "",
        ] {
            assert_eq!(threshold_for(name), SLOW_MS, "{name} 必须保持 50ms");
        }
    }

    /// `Span::new` 会自动套用分级阈值（调用点漏写也不会掉回 50ms）。
    #[test]
    fn span_new_picks_threshold_by_name() {
        let ui = Span::new("tick.probe");
        assert_eq!(ui.threshold_ms(), UI_SLOW_MS);
        ui.cancel();
        let other = Span::new("paint.frame");
        assert_eq!(other.threshold_ms(), SLOW_MS);
        other.cancel();
        // 显式阈值优先于段名
        let forced = Span::with_threshold("tick.probe", SMTC_SLOW_MS);
        assert_eq!(forced.threshold_ms(), SMTC_SLOW_MS);
        forced.cancel();
    }

    /// 16ms 口径下 20ms 的段要记，50ms 口径下同长度不记（这就是盲区被补上的证明）。
    #[test]
    fn ui_threshold_catches_what_50ms_missed() {
        let _ = take_recorded();
        {
            let _s = collecting("tick.probe"); // 自动 16ms
            std::thread::sleep(Duration::from_millis(24));
        }
        let got = take_recorded();
        assert_eq!(got.len(), 1, "20~30ms 的 tick 段必须留下记录：{got:?}");
        assert_eq!(got[0].0, "tick.probe");
    }

    // ------------------------------------------------ v1.4.4：同段限流（A3）

    /// 同一段名 60s 内第 11 条只发一次提示，之后静默丢弃。
    #[test]
    fn limiter_caps_ten_per_window() {
        let mut l = RateLimiter::new();
        for i in 0..LIMIT_MAX_PER_WINDOW {
            assert_eq!(l.admit("tick.probe", 0), Verdict::Allow, "第 {i} 条该放行");
        }
        assert_eq!(l.admit("tick.probe", 0), Verdict::Notice, "第 11 条只发提示");
        for _ in 0..5 {
            assert_eq!(l.admit("tick.probe", 1_000), Verdict::Drop, "提示只许发一次");
        }
    }

    /// 窗口滚动：满 60s 之后重新给满额度。
    #[test]
    fn limiter_window_rolls_over_after_60s() {
        let mut l = RateLimiter::new();
        for _ in 0..LIMIT_MAX_PER_WINDOW {
            assert_eq!(l.admit("tick.ontop", 5_000), Verdict::Allow);
        }
        assert_eq!(l.admit("tick.ontop", 5_000), Verdict::Notice);
        // 差 1ms 还没滚
        assert_eq!(
            l.admit("tick.ontop", 5_000 + LIMIT_WINDOW_MS - 1),
            Verdict::Drop
        );
        // 刚好满窗口 → 新窗口，重新放行
        assert_eq!(
            l.admit("tick.ontop", 5_000 + LIMIT_WINDOW_MS),
            Verdict::Allow
        );
    }

    /// 不同段名各自独立记账（一段刷屏不许拖累别的段）。
    #[test]
    fn limiter_is_per_segment() {
        let mut l = RateLimiter::new();
        for _ in 0..LIMIT_MAX_PER_WINDOW {
            assert_eq!(l.admit("tick.probe", 0), Verdict::Allow);
        }
        assert_eq!(l.admit("tick.probe", 0), Verdict::Notice);
        assert_eq!(l.admit("tick.ontop", 0), Verdict::Allow, "别的段额度独立");
        assert_eq!(l.admit("sampler.capture", 0), Verdict::Allow);
    }

    /// 表满也不能死循环 / 丢记账（超过槽位数量的段名照样能记）。
    #[test]
    fn limiter_survives_more_segments_than_slots() {
        let mut l = RateLimiter::new();
        let names: Vec<&'static str> = (0..LIMIT_SLOTS + 8)
            .map(|i| &*Box::leak(format!("t.overflow.{i}").into_boxed_str()))
            .collect();
        for name in &names {
            assert_eq!(l.admit(name, 0), Verdict::Allow, "{name} 必须放行");
        }
    }

    /// 5 个 16ms 段各自有独立额度（一段刷屏不许把别的段挤掉）。
    ///
    /// 这条同时兜住「哈希退化 / 线性探测串味」：只要槽位选错，某段就会提前收到
    /// `Notice`。
    #[test]
    fn every_ui_segment_has_its_own_budget() {
        let mut l = RateLimiter::new();
        for _ in 0..LIMIT_MAX_PER_WINDOW {
            for name in UI_SLOW_SEGMENTS {
                assert_eq!(l.admit(name, 0), Verdict::Allow, "{name} 额度被别的段吃了");
            }
        }
        for name in UI_SLOW_SEGMENTS {
            assert_eq!(l.admit(name, 0), Verdict::Notice, "{name} 该到限了");
        }
    }

    // ------------------------------------------------ v1.4.4：抓屏统计（A4）

    /// 抓屏统计：次数 / 慢次数 / 最慢，且「取走即清零」。
    ///
    /// 用**独立实例**而不是全局那个：全局计数器会被 60s 统计行的用例读走，
    /// 并行跑测试时会互相串味（这是本文件唯一允许「另建实例」的地方）。
    #[test]
    fn capture_window_accumulates_and_resets() {
        let stats = CaptureStats::new();
        stats.record(3);
        stats.record(SAMPLER_SLOW_MS); // 恰好 16ms 就算慢（≥）
        stats.record(80);
        let w = stats.take();
        assert_eq!(w.count, 3);
        assert_eq!(w.slow, 2, "16ms 与 80ms 各算一次慢");
        assert_eq!(w.max_ms, 80);
        assert_eq!(stats.take(), CaptureWindow::default(), "取走之后必须清零");
    }

    /// 最慢值必须靠 `fetch_max` 累加，**与到达顺序无关**。
    ///
    /// 真实采样是乱序的：先来一次 80ms 的慢抓屏，后面接着一串 1~3ms 的快抓屏。
    /// 如果实现写成了「后写覆盖」（`store`），窗口末端的 `capture最慢=` 会变成 1ms，
    /// 统计行就会把唯一一次真卡顿抹掉。这条用例专门钉住「先大后小」这个方向。
    #[test]
    fn capture_stats_max_survives_out_of_order() {
        let stats = CaptureStats::new();
        stats.record(80);
        stats.record(1);
        stats.record(3);
        let w = stats.take();
        assert_eq!(w.count, 3);
        assert_eq!(w.max_ms, 80, "最慢值不许被后面的快抓屏覆盖");
        assert_eq!(w.slow, 1, "只有 80ms 那条算慢");
    }

    /// 全局入口只是把请求转给计数器（接线测试：漏接的话统计行永远是 0）。
    ///
    /// **必须重试**：`CAPTURE_STATS` 是全局的，`smtc::PollStats::flush_if_due`
    /// 在别的用例（`stats_window_flushes_and_resets`）里也会 `take()` 一次；
    /// 并行跑时它可能正好插在「记一条」与「取走」之间，把这一条取走。
    /// 这不是被测实现的缺陷（生产里只有一个读者），但会让本用例随机变红。
    #[test]
    fn global_capture_stats_is_wired() {
        for _ in 0..8 {
            record_capture(1);
            let w = take_capture_window();
            if w.count >= 1 {
                assert!(w.max_ms >= 1, "记了次数却没记最慢值");
                return;
            }
        }
        panic!("record_capture 没接到计数器上（连续 8 次都被别的用例取走，不可能是巧合）");
    }
}
