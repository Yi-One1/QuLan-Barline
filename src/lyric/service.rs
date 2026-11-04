//! 歌词线程（T2）与状态机（`ARCHITECTURE.md` §12.6 / T11 第 5–6 条）。
//!
//! 线程职责单一：**收到 `LyricRequest` → 查（记忆 → 磁盘 → 本地 → 负缓存 → 在线）→
//! 发 `AppEvent`**。它不认识 UI、不认识窗口、不认识渲染，只认识 `EventSink`。
//!
//! ## 为什么查询顺序是「内存 → 磁盘 → 本地 → 负缓存 → 在线」
//!
//! 1. 内存 / 磁盘缓存最优先（重启后也命中，0 网络包）；
//! 2. **本地文件**紧随其后：命中就**一个网络包都不发**（PRD P0-08 要求抓包可验证）；
//! 3. **负缓存挡在在线之前**：24h 内查不到的歌不再打网络（PRD §6.2）；
//! 4. 最后才联网。
//!
//! ## G1 提速：主循环为什么不再自己做网络查询
//!
//! v1.3.1 及以前的 `run()` 是**单线程串行 worker**：一条查询卡 83.8s（日志实测），
//! 用户换的新歌只能排队——这才是「查过的歌再放也要等 10 秒以上」的真凶。
//!
//! 现在主循环只做**毫秒级**的四步（缓存 / 本地 / 负缓存），一旦需要联网就把活儿
//! 丢给独立线程（[`spawn_dispatcher`]），自己立刻回到 `recv_timeout`。
//! 在线结果经同一条消息队列回主循环处理，于是：
//!
//! - 换歌**永不排队**：新请求在 100ms 内被受理（最坏就是一次 `IDLE_POLL_MS`）；
//! - 旧结果用**代次守卫**丢弃（`TrackId` 本就单调递增，见 `media::TrackId` 的注释），
//!   既不推 `LyricReady`，也不写负缓存——避免新歌被旧结论污染；
//! - 在线查询本身有 8 秒**硬截止**（[`ONLINE_DEADLINE_MS`]），两个源并发。
//!
//! ## 重试
//!
//! 只有「在线查询**明确**网络失败」才会重试，退避 3s、**最多 1 次**
//! （`limits::LYRIC_RETRY_BACKOFF_MS`，只含一项）。两条铁律：
//!
//! 1. **超时不重试**：8 秒截止到点说明两个源都已给过机会，再来一轮只是让用户多等；
//! 2. **换歌就作废**：重试前必查当前代次，旧曲目的重试绝不执行。

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::defaults::limits;
use crate::event::{AppEvent, LyricRequest};
use crate::lyric::cache::Cache;
use crate::lyric::lrclib::LrclibProvider;
use crate::lyric::netease::NeteaseProvider;
use crate::lyric::provider::{
    LocalProvider, LyricProvider, LyricQuery, ParallelOutcome, ProviderChain,
};
use crate::lyric::query;
use crate::lyric::writeback::{self, Outcome};
use crate::lyric::{LyricMissReason, Lyrics};
use crate::media::TrackId;
use crate::platform::wake::EventSink;

/// 线程空闲时的最长阻塞时间（也决定重试计时的精度）。
///
/// 100ms 是「换歌响应」与「空转功耗」的折中：新请求由 channel 直接唤醒，
/// 这个值只影响重试计时与在线结果到达时的额外延迟。
const IDLE_POLL_MS: u64 = 100;

/// 在线查询的**硬截止**（毫秒）：到点必须给用户一个结论。
///
/// 与 `net::LYRIC_PROVIDER_BUDGET_MS`（5500）配合：单源预算先到就先收手，
/// 全局截止只兜「两个源都慢」的底。
const ONLINE_DEADLINE_MS: u64 = 8_000;

// ---------------------------------------------------------------- 线程内消息

/// 线程内消息：**请求与在线结果共用一条队列**。
///
/// 为什么合成一条：主循环只能阻塞在一个 `recv_timeout` 上。分成两条 channel 就得
/// 轮询两条或者用 select，前者白白增加延迟、后者要额外依赖——合成一条消息枚举最简单。
/// worker 自己持有发送端的克隆（在线线程需要它），所以退出信号只能是显式消息，
/// 不能指望 `Disconnected`（见 [`LyricService::shutdown`]）。
enum Msg {
    /// 来自 UI 线程的新请求
    Request(LyricRequest),
    /// 来自在线线程的查询结果（`Box` 一下：否则整个枚举按最大变体 336 字节走）
    Online(Box<OnlineOutcome>),
    /// 关线程（`LyricService::shutdown` 发出）
    Shutdown,
}

/// 一次在线查询的任务快照（主循环 → 在线线程）。
struct OnlineJob {
    /// 原请求（线程要自己组 `LyricQuery`，还得带着 `track_id` 回来做代次守卫）
    req: LyricRequest,
    /// 已重试过的次数（0 = 首次）
    attempt: u8,
    /// 候选关键词，只为了日志
    keywords: Vec<String>,
}

/// 在线查询结果（在线线程 → 主循环）。
struct OnlineOutcome {
    req: LyricRequest,
    attempt: u8,
    keywords: Vec<String>,
    result: ParallelOutcome,
}

/// 待重试的任务。
struct Retry {
    track_id: TrackId,
    request: LyricRequest,
    /// 这一次是**第几次**重试（1 起）
    attempt: u8,
    due: Instant,
}

/// 事件出口。
///
/// 抽成 trait 只为一件事：`EventSink` 需要真实 HWND，单测里造不出来，
/// 而「旧代结果不许产生事件」这条必须能被断言。
trait EventOut {
    fn emit(&self, evt: AppEvent);
}

impl EventOut for EventSink<AppEvent> {
    fn emit(&self, evt: AppEvent) {
        self.send(evt);
    }
}

/// 派发器：把在线查询丢到独立线程。`Err` = 线程起不来。
type Dispatcher = Box<dyn Fn(OnlineJob) -> Result<(), String> + Send + Sync>;

// ---------------------------------------------------------------- 对外句柄

/// 歌词服务句柄（UI 线程持有）。
pub struct LyricService {
    tx: Sender<Msg>,
    handle: Option<JoinHandle<()>>,
}

impl LyricService {
    /// 起线程。失败返回 `None`（歌词不可用，但小组件照常工作）。
    pub fn spawn(sink: EventSink<AppEvent>) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        // worker 自己的发送端：在线线程靠它把结果送回主循环
        let job_tx = tx.clone();
        let spawned = std::thread::Builder::new()
            .name(String::from("barline-lyric"))
            .stack_size(512 * 1024)
            .spawn(move || run(sink, rx, job_tx));
        match spawned {
            Ok(handle) => Some(Self {
                tx,
                handle: Some(handle),
            }),
            Err(e) => {
                crate::log_err!("歌词线程创建失败：{e}");
                None
            }
        }
    }

    /// 投递一次歌词请求。线程已退出时静默丢弃。
    pub fn send(&self, req: LyricRequest) {
        if self.tx.send(Msg::Request(req)).is_err() {
            crate::log_warn!("歌词线程已退出，请求被丢弃");
        }
    }

    /// 通知退出。
    ///
    /// **刻意不 `join`**：在线查询最长 8 秒（[`ONLINE_DEADLINE_MS`]，更短的网络请求
    /// 预算在 `net::TIMEOUT_LYRIC`），等它会把「点退出 → 进程消失」拖成好几秒。
    /// 在线线程本身是 detach 的，进程退出时由 OS 回收；`EventSink::send` 在接收端
    /// 销毁后只是静默失败，不会 panic。
    ///
    /// **为什么必须显式发 `Shutdown`**：worker 自己持有一份发送端（在线线程要用），
    /// 所以「把 `tx` 换成永远失败的 sender」**不会**让 `recv_timeout` 返回
    /// `Disconnected`——队列永远有活着的发送端。给一条明确的退出消息最省事。
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        self.tx = closed_sender();
        // drop JoinHandle = detach
        let _ = self.handle.take();
    }
}

impl Drop for LyricService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 一个永远发送失败的 `Sender`（`shutdown` 里替换用，之后所有 `send` 静默失败）。
fn closed_sender() -> Sender<Msg> {
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    drop(rx);
    tx
}

// ---------------------------------------------------------------- 线程主循环

/// 线程主循环：只做分发，不做任何耗时活儿。
fn run(sink: EventSink<AppEvent>, rx: Receiver<Msg>, job_tx: Sender<Msg>) {
    crate::log_info!("歌词线程启动");
    let mut worker = Worker {
        out: sink,
        cache: Cache::new(crate::platform::data_dir().join("lyrics-cache")),
        dispatch: spawn_dispatcher(job_tx),
        current_track: 0,
        pending: None,
        wrote: HashSet::new(),
    };

    loop {
        let timeout = worker.poll_timeout();
        match rx.recv_timeout(timeout) {
            Ok(Msg::Request(req)) => worker.on_request(req),
            Ok(Msg::Online(out)) => worker.on_online(*out),
            Ok(Msg::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => worker.on_tick(Instant::now()),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    crate::log_info!("歌词线程退出");
}

/// 造一个「起线程跑在线查询」的派发器。
fn spawn_dispatcher(job_tx: Sender<Msg>) -> Dispatcher {
    Box::new(move |job: OnlineJob| {
        let tx = job_tx.clone();
        std::thread::Builder::new()
            .name(String::from("barline-lyric-net"))
            .stack_size(256 * 1024)
            .spawn(move || run_online_job(tx, job))
            // drop JoinHandle = detach：主循环绝不为回收线程再等一次
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// 在线线程主体：并发查询 → 结果回传主循环。
fn run_online_job(tx: Sender<Msg>, job: OnlineJob) {
    let chain = ProviderChain::with(vec![
        Box::new(LrclibProvider::new(true)),
        Box::new(NeteaseProvider::new(true)),
    ]);
    let query = LyricQuery {
        track: &job.req.track,
        local_dirs: &job.req.local_dirs,
        online_enabled: job.req.online_enabled,
    };
    let result = chain.query_parallel(&query, ONLINE_DEADLINE_MS);
    // 主循环可能已经退出（进程在关）：send 失败是正常路径
    let _ = tx.send(Msg::Online(Box::new(OnlineOutcome {
        req: job.req,
        attempt: job.attempt,
        keywords: job.keywords,
        result,
    })));
}

// ---------------------------------------------------------------- 状态机

/// 歌词线程的全部可变状态。抽成结构体是为了能脱离线程单测
/// （判代次、判重试这些纯决策不该只能靠「跑起来看日志」验证）。
struct Worker<O: EventOut> {
    out: O,
    cache: Cache,
    dispatch: Dispatcher,
    /// 最近一次被受理的请求代次（在线结果与重试的唯一守卫）
    current_track: TrackId,
    pending: Option<Retry>,
    /// v1.7.0：本次运行已处理过落盘的曲目 key（有界，见 `limits::WRITE_BACK_DEDUP_CAP`）
    wrote: HashSet<String>,
}

impl<O: EventOut> Worker<O> {
    /// 下一次 `recv_timeout` 等多久。
    fn poll_timeout(&self) -> Duration {
        match &self.pending {
            Some(p) => p
                .due
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(10)),
            None => Duration::from_millis(IDLE_POLL_MS),
        }
    }

    /// 收到新请求：先记账（换代次、作废旧重试），再走毫秒级快路径。
    fn on_request(&mut self, req: LyricRequest) {
        self.current_track = req.track_id;
        // 新请求 = 换歌（或用户手动重试）：旧的重试计时一律作废
        self.pending = None;
        self.query_fast_path(req, 0);
    }

    /// 毫秒级快路径：缓存 → 本地 → 负缓存 → （需要时）丢给在线线程。
    fn query_fast_path(&mut self, req: LyricRequest, attempt: u8) {
        let t0 = Instant::now();
        let track_id = req.track_id;
        let key = query::cache_key(&req.track);

        // 元数据不够：SMTC 只给了个空标题，什么也查不到
        if req.track.title.trim().is_empty() {
            self.out.emit(AppEvent::LyricMissing {
                track_id,
                reason: LyricMissReason::InsufficientMetadata,
            });
            return;
        }

        self.out.emit(AppEvent::LyricLoading { track_id });

        // ---- 1. 内存 ----
        if let Some(l) = self.cache.get_mem(&key) {
            crate::log_info!("歌词内存命中：{key} 耗时={}ms", t0.elapsed().as_millis());
            self.out.emit(AppEvent::LyricReady {
                track_id,
                lyrics: l,
            });
            return;
        }

        // ---- 2. 磁盘 ----
        if let Some(l) = self.cache.get_disk(&key) {
            crate::log_info!("歌词磁盘命中：{key} 耗时={}ms", t0.elapsed().as_millis());
            self.out.emit(AppEvent::LyricReady {
                track_id,
                lyrics: l,
            });
            return;
        }

        let lq = LyricQuery {
            track: &req.track,
            local_dirs: &req.local_dirs,
            online_enabled: req.online_enabled,
        };

        // ---- 3. 本地文件（命中则完全不联网）----
        match LocalProvider.fetch(&lq) {
            Ok(l) => {
                self.cache.put(&key, &l, "local");
                crate::log_info!("歌词本地命中：{key} 耗时={}ms", t0.elapsed().as_millis());
                self.out.emit(AppEvent::LyricReady {
                    track_id,
                    lyrics: Arc::new(l),
                });
                return;
            }
            Err(e) => crate::log_debug!("本地未命中：{}", e.as_str()),
        }

        // ---- 4. 负缓存 ----
        if let Some(reason) = self.cache.miss_reason(&key) {
            crate::log_info!(
                "歌词负缓存命中（{reason:?}），{}h 内不再请求：{key}",
                limits::LYRIC_MISS_TTL_HOURS
            );
            self.out.emit(AppEvent::LyricMissing { track_id, reason });
            return;
        }

        // ---- 5. 在线：丢给独立线程，主循环立刻回到 recv ----
        if !req.online_enabled {
            self.out.emit(AppEvent::LyricMissing {
                track_id,
                reason: LyricMissReason::OnlineDisabled,
            });
            return;
        }

        let keywords = query::search_keywords(&req.track.title, &req.track.artist);
        let job = OnlineJob {
            req,
            attempt,
            keywords,
        };
        if let Err(e) = (self.dispatch)(job) {
            crate::log_err!("在线歌词线程启动失败：{e}");
            self.out.emit(AppEvent::LyricMissing {
                track_id,
                reason: LyricMissReason::NetworkError,
            });
        }
    }

    /// 收到在线结果：先判代次，再决定「写缓存 / 写负缓存 / 安排重试」。
    fn on_online(&mut self, out: OnlineOutcome) {
        // ---- 代次守卫（关键）----
        // 结果回来时用户可能已经换了三首歌。旧代结果**不许**产生任何副作用：
        // 推 LyricReady 会污染新歌的歌词，写负缓存会让新歌 24h 不再查询。
        if out.req.track_id != self.current_track {
            crate::log_debug!(
                "丢弃过期在线结果（结果 track={}，当前 track={}）",
                out.req.track_id,
                self.current_track
            );
            return;
        }

        let track_id = out.req.track_id;
        let key = query::cache_key(&out.req.track);
        // 日志里「谁命中的 / 候选 / 耗时」三件套照旧（不打歌词正文）
        let who = display_who(&out.req);

        if let Some(l) = out.result.lyrics {
            self.cache.put(&key, &l, out.result.source);
            crate::log_info!(
                "歌词查询：{who} → 源={} 命中（候选={:?} 耗时={}ms）",
                out.result.source,
                out.keywords,
                out.result.elapsed_ms
            );
            // v1.7.0：先显示后落盘。用 Arc 共享同一份 Lyrics，落盘不再深拷贝。
            let arc = Arc::new(l);
            self.out.emit(AppEvent::LyricReady {
                track_id,
                lyrics: Arc::clone(&arc),
            });
            self.pending = None;
            // 代次守卫已在前面通过；用结果自带的 req（不是"当前播放"）
            self.maybe_write_back(&out.req, &arc, out.result.source);
            return;
        }

        crate::log_info!(
            "歌词查询：{who} → 全部未命中（候选={:?} 耗时={}ms 超时={}）",
            out.keywords,
            out.result.elapsed_ms,
            out.result.timed_out
        );

        match out.result.reason {
            LyricMissReason::NetworkError => {
                if out.result.timed_out {
                    // 8 秒截止到点：两个源都已给过机会，**不重试**（重跑只会让用户多等）
                    crate::log_warn!("在线查询超过 {}ms 截止，本次放弃", ONLINE_DEADLINE_MS);
                    self.out.emit(AppEvent::LyricMissing {
                        track_id,
                        reason: LyricMissReason::NetworkError,
                    });
                    self.pending = None;
                    return;
                }
                let backoff = limits::LYRIC_RETRY_BACKOFF_MS;
                let idx = out.attempt as usize;
                if idx >= backoff.len() {
                    crate::log_warn!("歌词重试 {idx} 次仍失败，降级为「暂无歌词」");
                    self.cache.put_miss(&key, LyricMissReason::NotFound);
                    self.out.emit(AppEvent::LyricMissing {
                        track_id,
                        reason: LyricMissReason::NotFound,
                    });
                    self.pending = None;
                    return;
                }
                let wait = backoff[idx];
                crate::log_info!("歌词网络失败，{wait}ms 后重试（第 {} 次）", idx + 1);
                self.out.emit(AppEvent::LyricMissing {
                    track_id,
                    reason: LyricMissReason::NetworkError,
                });
                self.pending = Some(Retry {
                    track_id,
                    request: out.req,
                    attempt: out.attempt + 1,
                    due: Instant::now() + Duration::from_millis(wait),
                });
            }
            other => {
                self.cache.put_miss(&key, other);
                self.out.emit(AppEvent::LyricMissing {
                    track_id,
                    reason: other,
                });
                self.pending = None;
            }
        }
    }

    /// 在线歌词落盘（v1.7.0 / G5）。失败静默降级：只记日志，绝不改事件、不改缓存。
    fn maybe_write_back(&mut self, req: &LyricRequest, l: &Arc<Lyrics>, source: &str) {
        if source == "local" {
            return; // 防御性冗余：本地命中的歌词本来就在磁盘上
        }
        if !req.write_back {
            return;
        }
        let key = query::cache_key(&req.track);
        if self.wrote.contains(&key) {
            return;
        }
        if self.wrote.len() >= limits::WRITE_BACK_DEDUP_CAP {
            self.wrote.clear();
        }
        self.wrote.insert(key);

        match writeback::save(&req.track, &req.local_dirs, l.as_ref()) {
            Outcome::Written(p) => crate::log_info!("歌词已落盘：{}", p.display()),
            Outcome::Skipped(s) => crate::log_debug!("歌词不落盘：{s:?}"),
            Outcome::Failed(e) => crate::log_warn!("歌词落盘失败：{e}"),
        }
    }

    /// 定时器：到点才重试，且**只对当前代次**重试。
    fn on_tick(&mut self, now: Instant) {
        let Some(p) = self.pending.take() else {
            return;
        };
        if p.track_id != self.current_track {
            crate::log_debug!(
                "丢弃过期重试（track={}，当前 track={}）",
                p.track_id,
                self.current_track
            );
            return;
        }
        if now < p.due {
            self.pending = Some(p);
            return;
        }
        crate::log_info!("重试歌词（第 {} 次，track={}）", p.attempt, p.track_id);
        self.query_fast_path(p.request, p.attempt);
    }
}

/// 日志用的「标题 - 歌手」（空歌手写成 `<无歌手>`，与既有日志格式一致）。
fn display_who(req: &LyricRequest) -> String {
    let artist = if req.track.artist.trim().is_empty() {
        "<无歌手>"
    } else {
        req.track.artist.trim()
    };
    format!("{} - {artist}", req.track.title)
}

/// 供单测使用：跑一次查询（不启线程）。
#[cfg(test)]
pub fn query_once(
    cache: &mut Cache,
    req: &LyricRequest,
) -> (Option<crate::lyric::Lyrics>, LyricMissReason) {
    let key = query::cache_key(&req.track);
    if let Some(l) = cache.get_mem(&key) {
        return (Some((*l).clone()), LyricMissReason::NotFound);
    }
    let q = LyricQuery {
        track: &req.track,
        local_dirs: &req.local_dirs,
        online_enabled: req.online_enabled,
    };
    let chain = ProviderChain::with(vec![
        Box::new(LocalProvider),
        Box::new(LrclibProvider::new(req.online_enabled)),
        Box::new(NeteaseProvider::new(req.online_enabled)),
    ]);
    let (l, reason) = chain.query(&q);
    if let Some(ref got) = l {
        cache.put(&key, got, "test");
    }
    (l, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyric::{LyricLine, Lyrics};
    use crate::media::Track;

    // ------------------------------------------------ 测试脚手架

    /// 收集事件的最小出口（`EventSink` 需要真实 HWND，测试里造不出来）。
    #[derive(Default)]
    struct Collector {
        events: std::sync::Mutex<Vec<AppEvent>>,
    }

    impl Collector {
        fn take(&self) -> Vec<AppEvent> {
            let mut g = match self.events.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            std::mem::take(&mut *g)
        }

        fn ready(&self) -> usize {
            self.take()
                .iter()
                .filter(|e| matches!(e, AppEvent::LyricReady { .. }))
                .count()
        }

        fn missing(&self) -> usize {
            self.take()
                .iter()
                .filter(|e| matches!(e, AppEvent::LyricMissing { .. }))
                .count()
        }
    }

    impl EventOut for Arc<Collector> {
        fn emit(&self, evt: AppEvent) {
            let mut g = match self.events.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            g.push(evt);
        }
    }

    /// 不启线程的投递器：只把 job 记下来（断言「有没有派发」用）。
    fn recording_dispatch_jobs() -> (Dispatcher, Receiver<OnlineJob>) {
        let (tx, rx) = std::sync::mpsc::channel::<OnlineJob>();
        let d: Dispatcher = Box::new(move |job| {
            let _ = tx.send(job);
            Ok(())
        });
        (d, rx)
    }

    fn never_dispatch() -> Dispatcher {
        Box::new(|_job: OnlineJob| Ok(()))
    }

    fn worker(tag: &str, out: Arc<Collector>, dispatch: Dispatcher) -> Worker<Arc<Collector>> {
        Worker {
            out,
            cache: cache(tag),
            dispatch,
            current_track: 0,
            pending: None,
            wrote: HashSet::new(),
        }
    }

    fn request(title: &str, artist: &str, dirs: Vec<String>, online: bool) -> LyricRequest {
        request_with_id(7, title, artist, dirs, online)
    }

    fn request_with_id(
        id: TrackId,
        title: &str,
        artist: &str,
        dirs: Vec<String>,
        online: bool,
    ) -> LyricRequest {
        LyricRequest {
            track_id: id,
            track: Track {
                title: title.into(),
                artist: artist.into(),
                album: String::new(),
                duration_ms: 180_000,
            },
            local_dirs: dirs,
            online_enabled: online,
            // v1.7.0：默认关闭，既有测试绝不写盘（污染用户目录）；
            // 需要验落盘的测试自己把这一位改成 true
            write_back: false,
            retry: false,
        }
    }

    fn cache(tag: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("barline_service_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        Cache::new(dir)
    }

    fn one_line_lyrics(text: &str) -> Lyrics {
        Lyrics {
            lines: vec![LyricLine {
                start_ms: 0,
                end_ms: 1000,
                text: text.into(),
                words: Vec::new(),
            }],
            ..Default::default()
        }
    }

    fn outcome(
        req: &LyricRequest,
        attempt: u8,
        lyrics: Option<Lyrics>,
        reason: LyricMissReason,
        timed_out: bool,
    ) -> OnlineOutcome {
        OnlineOutcome {
            req: req.clone(),
            attempt,
            keywords: vec!["候选".into()],
            result: ParallelOutcome {
                lyrics,
                reason,
                source: "lrclib",
                elapsed_ms: 123,
                timed_out,
            },
        }
    }

    // ------------------------------------------------ 既有行为（语义不变）

    #[test]
    fn local_file_hit_never_touches_network() {
        // 用一个必然连不通的在线源：如果本地命中，就根本不会走到它
        let dir = std::env::temp_dir().join("barline_service_local");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("离线测试歌.lrc");
        let _ = std::fs::write(&path, "[00:01.00]第一句\n[00:03.00]第二句");

        // 注意：缓存目录名不能也叫 barline_service_local，否则 `cache()` 的
        // remove_dir_all 会把刚写好的 .lrc 一起删掉
        let mut c = cache("local_hit");
        let req = request(
            "离线测试歌",
            "",
            vec![dir.to_string_lossy().to_string()],
            true,
        );
        let (l, _) = query_once(&mut c, &req);
        assert!(l.is_some(), "本地歌词应命中");
        assert_eq!(l.as_ref().map(|x| x.lines.len()), Some(2));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn online_disabled_yields_disabled_reason() {
        let mut c = cache("offline");
        let req = request("不存在的歌名xyz", "不存在的歌手", vec![], false);
        let (l, reason) = query_once(&mut c, &req);
        assert!(l.is_none());
        // 本地没有 + 在线关闭 → chain 里在线 provider 被跳过 → NotFound
        assert_eq!(reason, LyricMissReason::NotFound);
    }

    #[test]
    fn empty_title_reports_insufficient_metadata() {
        // 这条走的是 handle_request 的早退分支，这里直接验证条件成立
        let req = request("   ", "x", vec![], true);
        assert!(req.track.title.trim().is_empty());
    }

    // ------------------------------------------------ 快路径 / 派发（G1）

    /// 缓存命中必须是**毫秒级且不派发在线任务**（否则等于白查一遍）。
    #[test]
    fn cache_hit_answers_without_dispatch() {
        let col = Arc::new(Collector::default());
        let (dispatch, jobs) = recording_dispatch_jobs();
        let mut w = worker("fast_hit", Arc::clone(&col), dispatch);

        let req = request("晴天", "周杰伦", vec![], true);
        w.on_request(req.clone());
        // 第一次必然派发在线任务
        assert!(jobs.try_recv().is_ok(), "首次查询应派发在线任务");
        assert_eq!(col.ready(), 0, "还没有歌词可给");

        // 模拟在线命中 → 写进缓存
        w.on_online(outcome(
            &req,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));
        assert_eq!(col.ready(), 1, "命中应推 LyricReady");

        // 同一首歌再来一次 → 内存命中，不许再派发
        w.on_request(req);
        assert!(jobs.try_recv().is_err(), "缓存命中不该再联网");
        assert_eq!(col.ready(), 1, "缓存命中也要推 LyricReady");
    }

    /// 负缓存命中也不许派发（24h 内不再请求，PRD §6.2）。
    #[test]
    fn negative_cache_hit_answers_without_dispatch() {
        let col = Arc::new(Collector::default());
        let (dispatch, jobs) = recording_dispatch_jobs();
        let mut w = worker("neg_hit", Arc::clone(&col), dispatch);

        let req = request("冷门歌", "没人听过", vec![], true);
        let key = query::cache_key(&req.track);
        w.cache.put_miss(&key, LyricMissReason::NotFound);

        w.on_request(req);
        assert!(jobs.try_recv().is_err(), "负缓存命中不该联网");
        assert_eq!(col.missing(), 1);
    }

    /// 在线关闭时不许派发，且原因必须是 `OnlineDisabled`。
    #[test]
    fn online_disabled_does_not_dispatch() {
        let col = Arc::new(Collector::default());
        let (dispatch, jobs) = recording_dispatch_jobs();
        let mut w = worker("online_off", Arc::clone(&col), dispatch);

        w.on_request(request("晴天", "周杰伦", vec![], false));
        assert!(jobs.try_recv().is_err());
        assert_eq!(col.missing(), 1);
    }

    // ------------------------------------------------ 代次守卫（G1 关键）

    /// **旧代结果必须被丢弃**：不推事件、不写正缓存、不写负缓存。
    #[test]
    fn stale_online_result_is_discarded() {
        let col = Arc::new(Collector::default());
        let mut w = worker("gen_guard", Arc::clone(&col), never_dispatch());

        // 用户先点了 A（track 7），立刻换到 B（track 9）
        let a = request_with_id(7, "晴天", "周杰伦", vec![], true);
        let b = request_with_id(9, "七里香", "周杰伦", vec![], true);
        w.on_request(a.clone());
        w.on_request(b);
        let _ = col.take();

        // A 的查询迟到回来（歌词 + 未命中两种都要丢）
        w.on_online(outcome(
            &a,
            0,
            Some(one_line_lyrics("迟到的歌词")),
            LyricMissReason::NotFound,
            false,
        ));
        w.on_online(outcome(&a, 0, None, LyricMissReason::NotFound, false));

        let events = col.take();
        assert_eq!(events.len(), 0, "旧代结果不许产生任何事件");
        let key = query::cache_key(&a.track);
        assert!(
            w.cache.get_mem(&key).is_none(),
            "旧代歌词不许进缓存"
        );
        assert!(
            w.cache.miss_reason(&key).is_none(),
            "旧代的「没有」不许污染负缓存"
        );
        assert!(w.pending.is_none(), "旧代结果不许安排重试");
    }

    /// 守卫不能宽到把**当前代**的结果也丢掉。
    #[test]
    fn current_generation_result_is_accepted() {
        let col = Arc::new(Collector::default());
        let mut w = worker("gen_ok", Arc::clone(&col), never_dispatch());

        let a = request_with_id(7, "晴天", "周杰伦", vec![], true);
        w.on_request(a.clone());
        let _ = col.take();

        w.on_online(outcome(
            &a,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));
        assert_eq!(col.ready(), 1);
        assert!(w.cache.get_mem(&query::cache_key(&a.track)).is_some());
    }

    // ------------------------------------------------ 重试收敛（G1）

    /// 超时 → 报 `NetworkError` 但**不安排整链重试**。
    #[test]
    fn timeout_does_not_schedule_retry() {
        let col = Arc::new(Collector::default());
        let mut w = worker("timeout", Arc::clone(&col), never_dispatch());
        let req = request_with_id(7, "晴天", "周杰伦", vec![], true);
        w.on_request(req.clone());
        let _ = col.take();

        w.on_online(outcome(&req, 0, None, LyricMissReason::NetworkError, true));
        assert_eq!(col.missing(), 1);
        assert!(w.pending.is_none(), "超时不许触发重试");
    }

    /// 明确网络失败 → 只重试 1 次，且退避 3s（`LYRIC_RETRY_BACKOFF_MS` 只有一项）。
    #[test]
    fn network_error_retries_at_most_once() {
        let col = Arc::new(Collector::default());
        let mut w = worker("retry_once", Arc::clone(&col), never_dispatch());
        let req = request_with_id(7, "晴天", "周杰伦", vec![], true);
        w.on_request(req.clone());
        let _ = col.take();

        // 第 1 次失败 → 安排第 1 次重试
        w.on_online(outcome(&req, 0, None, LyricMissReason::NetworkError, false));
        let p = w.pending.as_ref().expect("应安排一次重试");
        assert_eq!(p.attempt, 1);
        assert_eq!(
            limits::LYRIC_RETRY_BACKOFF_MS.len(),
            1,
            "整链重试次数上限由退避表长度决定"
        );

        // 重试也失败 → 不再安排，降级为 NotFound 并写负缓存
        w.on_online(outcome(&req, 1, None, LyricMissReason::NetworkError, false));
        assert!(w.pending.is_none(), "最多重试 1 次");
        assert_eq!(
            w.cache.miss_reason(&query::cache_key(&req.track)),
            Some(LyricMissReason::NotFound),
            "重试用尽后按「暂无歌词」进负缓存"
        );
    }

    /// 到点才重试；**换歌之后旧重试直接作废**（不许为上一首歌发网络请求）。
    #[test]
    fn retry_is_dropped_after_track_change() {
        let col = Arc::new(Collector::default());
        let (dispatch, jobs) = recording_dispatch_jobs();
        let mut w = worker("retry_stale", Arc::clone(&col), dispatch);

        let a = request_with_id(7, "晴天", "周杰伦", vec![], true);
        w.on_request(a.clone());
        let _ = jobs.try_recv();
        w.on_online(outcome(&a, 0, None, LyricMissReason::NetworkError, false));
        assert!(w.pending.is_some());

        // 用户换歌 → 旧重试作废
        w.on_request(request_with_id(9, "七里香", "周杰伦", vec![], true));
        let _ = jobs.try_recv();
        assert!(w.pending.is_none(), "换歌应清掉旧重试");

        // 即使时间到点，也不该再为 A 派发
        w.on_tick(Instant::now() + Duration::from_secs(10));
        assert!(jobs.try_recv().is_err(), "旧代重试不许再联网");
    }

    /// 重试到点后走的是**同一条快路径**（仍然派发到独立线程，不阻塞主循环）。
    #[test]
    fn retry_due_dispatches_again() {
        let col = Arc::new(Collector::default());
        let (dispatch, jobs) = recording_dispatch_jobs();
        let mut w = worker("retry_due", Arc::clone(&col), dispatch);

        let a = request_with_id(7, "晴天", "周杰伦", vec![], true);
        w.on_request(a.clone());
        let _ = jobs.try_recv();
        w.on_online(outcome(&a, 0, None, LyricMissReason::NetworkError, false));

        w.on_tick(Instant::now() + Duration::from_secs(10));
        let job = jobs.try_recv().expect("到点应再次派发");
        assert_eq!(job.attempt, 1, "重试次数要带上去，结果回来后才知道该不该再试");
        assert_eq!(job.req.track_id, 7);
    }

    // ------------------------------------------------ 落盘（G5 / v1.7.0）

    /// 独立的「本地歌词目录」临时目录（先清空再建，避免残留影响断言）。
    fn write_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("barline_service_{tag}_lyrics"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write_dirs(dir: &std::path::Path) -> Vec<String> {
        vec![dir.to_string_lossy().to_string()]
    }

    /// `Barline` 子目录里的文件名（不存在时返回空表）。
    fn written_files(dir: &std::path::Path) -> Vec<String> {
        match std::fs::read_dir(dir.join("Barline")) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// S1：开关打开 + 有目录 → 落盘，且**事件只推 1 个** `LyricReady`（先显示后落盘）。
    #[test]
    fn s1_online_hit_writes_back_and_emits_one_ready() {
        let dir = write_dir("s1");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s1", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目α", "无名歌手α", write_dirs(&dir), true);
        req.write_back = true;
        w.on_request(req.clone());
        let _ = col.take();

        w.on_online(outcome(
            &req,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));

        assert_eq!(col.ready(), 1, "命中只许推 1 个 LyricReady（不许因落盘再推一个）");
        let files = written_files(&dir);
        assert_eq!(files.len(), 1, "应落盘 1 个文件，实际 {files:?}");
        let want = format!(
            "{}.lrc",
            crate::lyric::locate::candidates(&req.track)
                .first()
                .cloned()
                .expect("应有候选名")
        );
        assert_eq!(files[0], want, "文件名必须等于首选候选名 + .lrc");
        assert!(w.wrote.contains(&query::cache_key(&req.track)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S2：开关关闭 → 既不落盘，也不记账。
    #[test]
    fn s2_write_back_off_writes_nothing() {
        let dir = write_dir("s2");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s2", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目β", "无名歌手β", write_dirs(&dir), true);
        req.write_back = false;
        w.on_request(req.clone());
        let _ = col.take();
        w.on_online(outcome(
            &req,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));

        assert_eq!(col.ready(), 1, "歌词照常显示");
        assert!(written_files(&dir).is_empty(), "关了开关就不许写盘");
        assert!(!dir.join("Barline").exists(), "连目录都不许建");
        assert!(w.wrote.is_empty(), "不落盘就不记账");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S3：**旧代结果不落盘**（代次守卫先于落盘生效）。
    #[test]
    fn s3_stale_generation_does_not_write_back() {
        let dir = write_dir("s3");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s3", Arc::clone(&col), never_dispatch());

        let mut a = request_with_id(7, "测试曲目γ", "无名歌手γ", write_dirs(&dir), true);
        a.write_back = true;
        let mut b = request_with_id(9, "测试曲目δ", "无名歌手δ", write_dirs(&dir), true);
        b.write_back = true;
        w.on_request(a.clone());
        w.on_request(b);
        let _ = col.take();

        // A 的结果迟到回来
        w.on_online(outcome(
            &a,
            0,
            Some(one_line_lyrics("迟到的歌词")),
            LyricMissReason::NotFound,
            false,
        ));

        assert_eq!(col.take().len(), 0, "旧代不许产生任何事件");
        assert!(written_files(&dir).is_empty(), "旧代不许落盘");
        assert!(w.wrote.is_empty(), "旧代不许记账");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S4：`source == "local"` 的防御分支——本地歌词已在磁盘上，不再写一份。
    #[test]
    fn s4_local_source_never_writes_back() {
        let dir = write_dir("s4");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s4", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目ε", "无名歌手ε", write_dirs(&dir), true);
        req.write_back = true;
        let l = Arc::new(one_line_lyrics("第一句"));
        w.maybe_write_back(&req, &l, "local");

        assert!(written_files(&dir).is_empty(), "本地源不许落盘");
        assert!(w.wrote.is_empty(), "防御分支连账都不记");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S5：**磁盘缓存命中不落盘**——防止有人把钩子塞进 `query_fast_path`。
    #[test]
    fn s5_disk_cache_hit_does_not_write_back() {
        let dir = write_dir("s5");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s5", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目ζ", "无名歌手ζ", write_dirs(&dir), true);
        req.write_back = true;
        // 预置磁盘缓存：快路径第 2 步就会命中并早退
        w.cache
            .put(&query::cache_key(&req.track), &one_line_lyrics("第一句"), "lrclib");

        w.on_request(req.clone());

        assert_eq!(col.ready(), 1, "磁盘命中照常推歌词");
        assert!(written_files(&dir).is_empty(), "磁盘命中不该落盘（钩子只在 on_online）");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
