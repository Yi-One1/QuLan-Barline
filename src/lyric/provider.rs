//! `LyricProvider` 抽象与有序链（`ARCHITECTURE.md` §12.4，风险 R1 拍板）。
//!
//! 为什么要抽象：在线源只有 lrclib 一个（风险 R1），但**本地文件**也是一路 provider；
//! 未来若加备用源，只需往链里插一个 `Box<dyn LyricProvider>`，`service` 一行不用改。
//!
//! ## G1 提速：为什么多了 [`ProviderChain::query_parallel`]
//!
//! 串行链（[`ProviderChain::query_with_source`]）的语义是「本地 → 在线」的**优先级**，
//! 必须保留——本地文件命中就一个网络包都不该发。但**两个在线源**之间没有优先级，
//! 只有「谁先答出来」：用户日志里 5 次查询 >30s，全部是 lrclib 精确超时 → lrclib
//! 模糊 → 网易云这么一路串下来累积的。所以在线这一层改成并发 + 硬截止，
//! 谁能给就立刻用谁的。两条路径并存，互不影响。

use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::lyric::locate;
use crate::lyric::parser;
use crate::lyric::LyricKind;
use crate::lyric::LyricMissReason;
use crate::lyric::Lyrics;
use crate::media::Track;

/// 一次查询的输入。
pub struct LyricQuery<'a> {
    pub track: &'a Track,
    /// 用户在设置里添加的歌词目录
    pub local_dirs: &'a [String],
    /// 是否允许联网（关闭时链里的在线 provider 直接跳过）
    pub online_enabled: bool,
}

/// provider 的错误分类。**决定重试策略**，不要合并。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 明确没有（404 / 本地没找到）→ 负缓存 24h，不重试
    NotFound,
    /// 网络层失败（超时 / 连不上 / 5xx）→ 指数退避重试 3 次
    Network(String),
    /// 拿到了但解析不出歌词 → 视为 NotFound
    Parse(String),
    /// 用户关闭了该源
    Disabled,
}

impl ProviderError {
    /// 日志友好名。
    pub fn as_str(&self) -> &str {
        match self {
            ProviderError::NotFound => "NotFound",
            ProviderError::Network(s) => s,
            ProviderError::Parse(s) => s,
            ProviderError::Disabled => "Disabled",
        }
    }
}

/// 一个歌词来源。
pub trait LyricProvider: Send + Sync {
    /// provider 名字，用于日志与缓存分区。
    fn name(&self) -> &'static str;
    /// 是否可用（在线源检查"用户已关闭在线歌词"）。
    fn is_enabled(&self) -> bool;
    /// 同步查询。实现内部自己做超时（WinHTTP 5s）。
    fn fetch(&self, q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError>;
}

// ---------------------------------------------------------------- 本地文件

/// 本地 `.lrc` 文件 provider（链首，**命中就不联网**）。
pub struct LocalProvider;

impl LyricProvider for LocalProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn fetch(&self, q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError> {
        let hit = locate::find(q.track, q.local_dirs).ok_or(ProviderError::NotFound)?;
        let text = locate::read_text(&hit.path)
            .ok_or_else(|| ProviderError::Parse(format!("读取失败：{}", hit.path.display())))?;
        let lyrics = parser::parse(&text, LyricKind::Line);
        if lyrics.lines.is_empty() {
            return Err(ProviderError::Parse(format!(
                "{} 解析不出歌词行",
                hit.path.display()
            )));
        }
        crate::log_info!(
            "本地歌词命中：{}（{} 行，kind={:?}）",
            hit.source,
            lyrics.lines.len(),
            lyrics.kind
        );
        Ok(lyrics)
    }
}

// ---------------------------------------------------------------- 并发查询结果

/// [`ProviderChain::query_parallel`] 的结果。
///
/// 相比串行路径多两个字段，都是为了**让 service 能做出正确的重试决定**：
/// - `timed_out`：是「截止时间到了、不知道有没有歌词」，还是「真的都答了没有」。
///   前者**不该**写负缓存（会把一次超时记成 24h 没有），也不该整链重跑；
/// - `elapsed_ms`：验收时对数用（日志里必须能看出这次快了多少）。
#[derive(Debug, Clone)]
pub struct ParallelOutcome {
    pub lyrics: Option<Lyrics>,
    pub reason: LyricMissReason,
    /// 命中的源名；未命中为空串
    pub source: &'static str,
    pub elapsed_ms: u128,
    /// `true` = 因截止时间放弃（此时 `reason` 一定是 `NetworkError`）
    pub timed_out: bool,
}

/// 并发查询时传给每个线程的**自有**查询参数。
///
/// 为什么需要它：`LyricQuery` 借的是 `&Track` / `&[String]`，而 detach 的线程
/// 要求闭包 `'static`。拷一份进 `Arc`，N 个线程共用一份（`Track` 就几个短字符串，
/// 拷贝成本远小于一次网络请求）。
struct OwnedQuery {
    track: Track,
    local_dirs: Vec<String>,
    online_enabled: bool,
}

impl OwnedQuery {
    fn as_query(&self) -> LyricQuery<'_> {
        LyricQuery {
            track: &self.track,
            local_dirs: &self.local_dirs,
            online_enabled: self.online_enabled,
        }
    }
}

// ---------------------------------------------------------------- 有序链

/// 有序 provider 链。任一成功即返回。
pub struct ProviderChain {
    /// 用 `Arc` 而不是 `Box`：并发路径要给每个线程各一条 provider 的**引用**，
    /// `Arc` 才能安全地跨线程共享（`LyricProvider` 本身已要求 `Send + Sync`）。
    providers: Vec<Arc<dyn LyricProvider>>,
}

impl ProviderChain {
    /// 用任意 provider 列表组链（`service` 需要把"在线"单独成链，
    /// 好在本地与在线之间插入负缓存判断）。
    pub fn with(providers: Vec<Box<dyn LyricProvider>>) -> Self {
        Self {
            providers: providers.into_iter().map(Arc::from).collect(),
        }
    }

    /// 依次查询。返回 `(Some(歌词), _)` 或 `(None, 失败原因)`。
    ///
    /// 语义细节：
    /// - `NotFound` / `Parse` **不短路**，继续问下一个源（本地没有就该去网上找）；
    /// - `Network` **也不短路**，但会记下来——链走完还没有结果时，
    ///   优先报 `NetworkError`（可重试）而不是 `NotFound`（不可重试）。
    pub fn query(&self, q: &LyricQuery<'_>) -> (Option<Lyrics>, LyricMissReason) {
        let (l, reason, _) = self.query_with_source(q);
        (l, reason)
    }

    /// 同 [`ProviderChain::query`]，但额外返回**命中的源名**（v1.3.0）。
    ///
    /// 存在的唯一理由是可观测性：现在链上有 lrclib 与网易云两个在线源，
    /// 用户日志必须能看出「到底是谁命中的」（否则调覆盖率时两眼一抹黑）。
    /// 未命中时源名为空串。
    pub fn query_with_source(
        &self,
        q: &LyricQuery<'_>,
    ) -> (Option<Lyrics>, LyricMissReason, &'static str) {
        let mut saw_network = false;
        for p in &self.providers {
            if !p.is_enabled() {
                continue;
            }
            match p.fetch(q) {
                Ok(l) if !l.lines.is_empty() => return (Some(l), LyricMissReason::NotFound, p.name()),
                Ok(_) => continue,
                Err(ProviderError::NotFound) => continue,
                Err(ProviderError::Parse(e)) => {
                    crate::log_debug!("provider {} 解析失败：{e}", p.name());
                    continue;
                }
                Err(ProviderError::Network(e)) => {
                    crate::log_warn!("provider {} 网络失败：{e}", p.name());
                    saw_network = true;
                    continue;
                }
                Err(ProviderError::Disabled) => continue,
            }
        }
        if saw_network {
            (None, LyricMissReason::NetworkError, "")
        } else {
            (None, LyricMissReason::NotFound, "")
        }
    }

    /// **并发**查询：每个 enabled provider 各起一个线程，首个非空命中立即返回。
    ///
    /// `deadline_ms` 是**硬截止**：到点无论还有一个源在跑都先给结论
    /// （`timed_out = true`，按 `NetworkError` 归类）。
    ///
    /// 语义细节（与串行 [`ProviderChain::query_with_source`] 的差异必须记住）：
    /// - **不做跨源打分**：谁先给出「自己认为达标」的结果就用谁的（每个 provider
    ///   内部仍然走 `pick_best` + `MIN_PICK_SCORE` + 时长淘汰，「不串歌」的三条铁律
    ///   一条没松）。代价是 lrclib 的优质结果可能输给网易云的快结果——这是拿
    ///   「慢但可能更好」换「8 秒内一定有结论」的自觉取舍；
    /// - 线程**detach**（不 join）：主线程绝不为了回收线程再等一次；
    ///   迟到线程的 `send` 会静默失败（接收端已 drop），不会 panic；
    /// - provider 内部 panic 被 `catch_unwind` 兜住并记成网络失败，
    ///   绝不让一个源的崩溃把整条链带走。
    pub fn query_parallel(&self, q: &LyricQuery<'_>, deadline_ms: u64) -> ParallelOutcome {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(deadline_ms);
        let shared = Arc::new(OwnedQuery {
            track: q.track.clone(),
            local_dirs: q.local_dirs.to_vec(),
            online_enabled: q.online_enabled,
        });

        let (tx, rx) = channel::<(&'static str, Result<Lyrics, ProviderError>)>();
        let mut remaining: usize = 0;
        // 线程都起不来也算网络失败（否则会被误记成「24h 内不用再查」）
        let mut saw_network = false;

        for p in &self.providers {
            if !p.is_enabled() {
                continue;
            }
            let provider = Arc::clone(p);
            let shared = Arc::clone(&shared);
            let sender = tx.clone();
            let name = provider.name();
            let spawned = std::thread::Builder::new()
                .name(format!("barline-lyric-{name}"))
                .stack_size(256 * 1024)
                .spawn(move || {
                    // AssertUnwindSafe：provider 内部只有局部状态，panic 后线程即退出，
                    // 不会留下被污染的可观察状态；这里唯一的要求是「别把 panic 传出去」。
                    let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let query = shared.as_query();
                        provider.fetch(&query)
                    }));
                    let result = match got {
                        Ok(r) => r,
                        Err(_) => {
                            crate::log_err!("provider {name} 线程 panic，按网络失败处理");
                            Err(ProviderError::Network(format!("{name} 内部错误")))
                        }
                    };
                    // 主线程可能已经按截止时间返回了 —— send 失败是正常路径
                    let _ = sender.send((name, result));
                });
            match spawned {
                // drop JoinHandle = detach
                Ok(_) => remaining += 1,
                Err(e) => {
                    crate::log_warn!("provider {name} 线程创建失败：{e}");
                    saw_network = true;
                }
            }
        }
        // 主线程自己那份 sender 必须 drop，否则 `recv_timeout` 永远等不到 Disconnected
        drop(tx);

        let mut timed_out = false;
        while remaining > 0 {
            let now = Instant::now();
            if now >= deadline {
                timed_out = true;
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok((name, Ok(l))) if !l.lines.is_empty() => {
                    return ParallelOutcome {
                        lyrics: Some(l),
                        reason: LyricMissReason::NotFound,
                        source: name,
                        elapsed_ms: started.elapsed().as_millis(),
                        timed_out: false,
                    };
                }
                Ok((_, Ok(_empty))) => remaining -= 1,
                Ok((name, Err(ProviderError::NotFound)))
                | Ok((name, Err(ProviderError::Disabled))) => {
                    crate::log_debug!("provider {name} 未命中");
                    remaining -= 1;
                }
                Ok((name, Err(ProviderError::Parse(e)))) => {
                    crate::log_debug!("provider {name} 解析失败：{e}");
                    remaining -= 1;
                }
                Ok((name, Err(ProviderError::Network(e)))) => {
                    crate::log_warn!("provider {name} 网络失败：{e}");
                    saw_network = true;
                    remaining -= 1;
                }
                Err(RecvTimeoutError::Timeout) => {
                    timed_out = true;
                    break;
                }
                // 还有线程没回话却已经没人持有发送端 = 它们崩了
                Err(RecvTimeoutError::Disconnected) => {
                    saw_network = true;
                    break;
                }
            }
        }

        let reason = if timed_out || saw_network {
            LyricMissReason::NetworkError
        } else {
            LyricMissReason::NotFound
        };
        ParallelOutcome {
            lyrics: None,
            reason,
            source: "",
            elapsed_ms: started.elapsed().as_millis(),
            timed_out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        name: &'static str,
        enabled: bool,
        result: Result<Vec<&'static str>, ProviderError>,
        /// 故意拖多久才回（模拟慢源）
        delay: Duration,
    }

    impl LyricProvider for Stub {
        fn name(&self) -> &'static str {
            self.name
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        fn fetch(&self, _q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError> {
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            match &self.result {
                Ok(lines) => Ok(Lyrics {
                    lines: lines
                        .iter()
                        .map(|t| crate::lyric::LyricLine {
                            start_ms: 0,
                            end_ms: 1000,
                            text: (*t).to_string(),
                            words: Vec::new(),
                        })
                        .collect(),
                    ..Default::default()
                }),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn q(track: &Track) -> LyricQuery<'_> {
        LyricQuery {
            track,
            local_dirs: &[],
            online_enabled: true,
        }
    }

    fn chain(list: Vec<Stub>) -> ProviderChain {
        ProviderChain::with(
            list.into_iter()
                .map(|s| Box::new(s) as Box<dyn LyricProvider>)
                .collect(),
        )
    }

    #[test]
    fn first_success_wins() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Ok(vec!["A"]),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
                delay: Duration::ZERO,
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert_eq!(l.and_then(|x| x.lines.first().map(|q| q.text.clone())), Some("A".to_string()));
    }

    #[test]
    fn not_found_falls_through_to_next_provider() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Err(ProviderError::NotFound),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
                delay: Duration::ZERO,
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert!(l.is_some());
    }

    #[test]
    fn disabled_provider_is_skipped() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: false,
                result: Ok(vec!["A"]),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
                delay: Duration::ZERO,
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert_eq!(l.and_then(|x| x.lines.first().map(|q| q.text.clone())), Some("B".to_string()));
    }

    #[test]
    fn network_error_is_reported_over_not_found() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Err(ProviderError::Network("timeout".into())),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Err(ProviderError::NotFound),
                delay: Duration::ZERO,
            },
        ]);
        let (l, reason) = c.query(&q(&t));
        assert!(l.is_none());
        assert_eq!(reason, LyricMissReason::NetworkError);
    }

    #[test]
    fn all_not_found_reports_not_found() {
        let t = Track::default();
        let c = chain(vec![Stub {
            name: "a",
            enabled: true,
            result: Err(ProviderError::NotFound),
            delay: Duration::ZERO,
        }]);
        let (l, reason) = c.query(&q(&t));
        assert!(l.is_none());
        assert_eq!(reason, LyricMissReason::NotFound);
    }

    #[test]
    fn empty_lyrics_is_treated_as_miss() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Ok(vec![]),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
                delay: Duration::ZERO,
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert_eq!(l.and_then(|x| x.lines.first().map(|q| q.text.clone())), Some("B".to_string()));
    }

    // ------------------------------------------------ 并发路径（G1 提速）

    fn parallel_chain(list: Vec<Stub>) -> ProviderChain {
        ProviderChain::with(
            list.into_iter()
                .map(|s| Box::new(s) as Box<dyn LyricProvider>)
                .collect(),
        )
    }

    /// **G1 核心收益**：快源先答出来就必须立刻用，不许等慢源。
    #[test]
    fn parallel_returns_fast_source_without_waiting_for_slow_one() {
        let t = Track::default();
        let c = parallel_chain(vec![
            Stub {
                name: "slow",
                enabled: true,
                result: Ok(vec!["慢"]),
                delay: Duration::from_millis(600),
            },
            Stub {
                name: "fast",
                enabled: true,
                result: Ok(vec!["快"]),
                delay: Duration::from_millis(40),
            },
        ]);
        let t0 = Instant::now();
        let out = c.query_parallel(&q(&t), 3_000);
        let took = t0.elapsed();

        assert_eq!(out.source, "fast", "必须用先答出来的源");
        assert!(!out.timed_out);
        assert_eq!(
            out.lyrics.and_then(|l| l.lines.first().map(|x| x.text.clone())),
            Some("快".to_string())
        );
        assert!(
            took < Duration::from_millis(400),
            "不该等慢源（600ms），实际 {took:?}"
        );
    }

    /// 截止到点 → 未命中，且**必须**归类为 `NetworkError`
    /// （`NotFound` 会被写进 24h 负缓存，一次超时不能变成一天没有歌词）。
    #[test]
    fn parallel_deadline_reports_network_error() {
        let t = Track::default();
        let c = parallel_chain(vec![Stub {
            name: "slow",
            enabled: true,
            result: Ok(vec!["慢"]),
            delay: Duration::from_millis(500),
        }]);
        let t0 = Instant::now();
        let out = c.query_parallel(&q(&t), 50);
        let took = t0.elapsed();

        assert!(out.lyrics.is_none());
        assert!(out.timed_out, "必须标记为超时");
        assert_eq!(out.reason, LyricMissReason::NetworkError);
        assert!(
            took < Duration::from_millis(300),
            "截止应立刻生效，实际 {took:?}"
        );
    }

    /// 所有源都**明确**答了「没有」→ `NotFound`（可安全进负缓存），不是网络失败。
    #[test]
    fn parallel_all_not_found_reports_not_found() {
        let t = Track::default();
        let c = parallel_chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Err(ProviderError::NotFound),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Err(ProviderError::NotFound),
                delay: Duration::ZERO,
            },
        ]);
        let out = c.query_parallel(&q(&t), 1_000);
        assert!(out.lyrics.is_none());
        assert!(!out.timed_out);
        assert_eq!(out.reason, LyricMissReason::NotFound);
    }

    /// 汇总规则与串行链保持同一套：有 `Network` 就报 `NetworkError`。
    #[test]
    fn parallel_network_error_wins_over_not_found() {
        let t = Track::default();
        let c = parallel_chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Err(ProviderError::Network("timeout".into())),
                delay: Duration::ZERO,
            },
            Stub {
                name: "b",
                enabled: true,
                result: Err(ProviderError::NotFound),
                delay: Duration::ZERO,
            },
        ]);
        let out = c.query_parallel(&q(&t), 1_000);
        assert!(out.lyrics.is_none());
        assert_eq!(out.reason, LyricMissReason::NetworkError);
    }

    /// disabled 的源连线程都不该起（也不该影响汇总结果）。
    #[test]
    fn parallel_skips_disabled_provider() {
        let t = Track::default();
        let c = parallel_chain(vec![
            Stub {
                name: "off",
                enabled: false,
                result: Ok(vec!["不该被用"]),
                delay: Duration::from_millis(200),
            },
            Stub {
                name: "on",
                enabled: true,
                result: Ok(vec!["B"]),
                delay: Duration::ZERO,
            },
        ]);
        let t0 = Instant::now();
        let out = c.query_parallel(&q(&t), 1_000);
        assert_eq!(out.source, "on");
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "disabled 源不该被等待"
        );
    }
}
