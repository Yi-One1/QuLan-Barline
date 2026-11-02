//! 分段耗时埋点（G2 随机卡顿诊断，v1.4.1 诊断版）。
//!
//! ## 为什么需要它
//!
//! 用户报「音乐和鼠标同时卡一下、时机完全随机」。静态审计只能给出**嫌疑**，
//! 落地要靠日志把「卡的那一下究竟是哪一段」指出来——而且**卡顿复发时**还能再抓一次。
//!
//! ## 设计约束（平时必须零成本）
//!
//! - 快路径只有一次 `Instant::now()` 与一次减法比较：**不分配、不格式化、不进日志缓冲区**；
//! - 只有单段 ≥ [`SLOW_MS`] 才写**一条** `log_warn!`，格式固定为
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

use std::time::Instant;

/// 慢阈值（毫秒）：**≥ 它**才写日志。
///
/// 50ms 的取法：60Hz 一帧是 16.7ms，人眼能明确感知「顿一下」大约从 40~50ms 起；
/// 阈值再低只会把日志淹掉，反而看不出随机卡顿的形态。
pub const SLOW_MS: u128 = 50;

/// **跨进程调用**专用的慢阈值（毫秒）。
///
/// 依据（G2 实测）：SMTC 的单次调用打到播放器进程后，正常在 1~3ms；
/// 一旦超过 8ms 就说明对面进程被我们拖住了——而那正是「音乐卡一下」的成因，
/// 所以这类段必须比 50ms 敏感得多，否则等到 50ms 记下来时已经晚了两轮。
pub const SMTC_SLOW_MS: u128 = 8;

/// 是否值得记一条（纯函数，单测直接断言）。
pub fn is_slow(ms: u128) -> bool {
    over(ms, SLOW_MS)
}

/// 「这段耗时算不算慢」的唯一判定（含边界：等于阈值就算）。
pub fn over(ms: u128, threshold_ms: u128) -> bool {
    ms >= threshold_ms
}

/// 一段耗时。RAII：活到作用域结束就自动结算。
#[must_use = "Span 必须在作用域内存活才会计时（立即丢弃 = 0ms，什么也不会记）"]
pub struct Span {
    name: &'static str,
    /// 可选备注（段名之外的补充，只在慢记录里出现）
    note: &'static str,
    start: Instant,
    /// 本段的慢阈值（默认 [`SLOW_MS`]，跨进程调用可用 [`SMTC_SLOW_MS`]）
    threshold: u128,
    /// 已经结算过（保证 Drop 只记一次）
    done: bool,
    /// 单测用：收进线程本地向量而不是写日志文件
    #[cfg(test)]
    collect: bool,
}

impl Span {
    /// 开始计一段（默认 50ms 阈值）。
    pub fn new(name: &'static str) -> Self {
        Self::with_threshold(name, SLOW_MS)
    }

    /// 开始计一段，并在慢记录里附带一句备注。
    pub fn with_note(name: &'static str, note: &'static str) -> Self {
        Self {
            name,
            note,
            start: Instant::now(),
            threshold: SLOW_MS,
            done: false,
            #[cfg(test)]
            collect: false,
        }
    }

    /// 开始计一段，**自带阈值**（不传就永远 50ms）。
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

        if self.note.is_empty() {
            crate::log_warn!("[perf] {} 耗时={}ms", self.name, ms);
        } else {
            crate::log_warn!("[perf] {} 耗时={}ms {}", self.name, ms, self.note);
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
}
