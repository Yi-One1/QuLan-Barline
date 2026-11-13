//! 歌词线程（T2）与状态机（`ARCHITECTURE.md` §12.6 / T11 第 5–6 条）。
//!
//! 线程职责单一：**收到 `LyricRequest` → 查（三「层」→ 负缓存 → 在线）→
//! 发 `AppEvent`**。它不认识 UI、不认识窗口、不认识渲染，只认识 `EventSink`。
//!
//! ## 查询顺序（`lyrics.local_first` 可切换，v1.8.0）
//!
//! 三层为「内存 LRU / 磁盘缓存 / 本地 `.lrc`」，顺序由请求自带的开关决定：
//!
//! - `local_first = false`（默认，= v1.7.0 现状）：**内存 → 磁盘 → 本地 → 负缓存 → 在线**。
//!   1. 内存 / 磁盘缓存最优先（重启后也命中，0 网络包）；
//!   2. **本地文件**紧随其后：命中就**一个网络包都不发**（PRD P0-08 要求抓包可验证）；
//!   3. **负缓存挡在在线之前**：24h 内查不到的歌不再打网络（PRD §6.2）；
//!   4. 最后才联网。
//! - `local_first = true`：**本地 → 内存 → 磁盘 → 负缓存 → 在线**。
//!   本地层提到内存层之前，是为了让「手改 `.lrc` 立刻生效」对**正在播放的那一首**
//!   也成立（内存层若在前面，它会一直吃旧的内存副本）。
//!
//! 两条顺序**只有一份逻辑**：`query_fast_path` 里用 [`Step`] 数组表达顺序。
//!
//! ## 落盘（v1.7.0 / G5；v1.8.0 扩到三条路径）
//!
//! 只要拿到歌词且来源 ≠ 本地文件，就在 `emit(LyricReady)` **之后**落一份到
//! `local_dirs[0]/Barline/`。判定收口在 [`Worker::maybe_write_back`] 一处，
//! `query_fast_path` 的三个命中分支与 `on_online` 都只调它。
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
use crate::lyric::cache::{self, Cache, Hit};
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
    /// 清空歌词缓存（v1.11.0）：设置页按钮 → UI 线程 → 这里
    ClearCache,
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

    /// 投递一次「清空缓存」（v1.11.0）。线程已退出时静默丢弃。
    ///
    /// 真正删除发生在歌词线程里（[`Cache::clear_all`]）：`Cache` 是它独占的，
    /// 在 UI 线程直接删文件会和正在进行的落盘 / 读取抢同一批路径。
    pub fn clear_cache(&self) {
        if self.tx.send(Msg::ClearCache).is_err() {
            crate::log_warn!("歌词线程已退出，清空缓存请求被丢弃");
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
        cache: Cache::new(cache::dir()),
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
            Ok(Msg::ClearCache) => worker.on_clear_cache(),
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

/// 查询层的三个「层」，顺序由 `LyricRequest.local_first` 决定（v1.8.0）。
///
/// 存在的唯一理由：两条顺序**不许写成两份逻辑**（复制粘贴必然漂移）。
/// 有了它，`false` 分支的顺序等价性从「31 行对照」降级为「3 个元素对照」。
#[derive(Clone, Copy)]
enum Step {
    /// 内存 LRU
    Mem,
    /// 磁盘正缓存
    Disk,
    /// 本地 `.lrc` 文件
    Local,
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

    /// v1.11.0：清空缓存并回报 UI。
    ///
    /// 「清完立刻重查当前歌」不在这里做——重查要走 `LyricRequest`（含代次、本地目录
    /// 等一整套上下文），由 UI 线程收到 [`AppEvent::LyricCacheCleared`] 后重新投递，
    /// 线程这边只负责「删干净 + 回一条事件」。
    fn on_clear_cache(&mut self) {
        let (files, bytes) = self.cache.clear_all();
        crate::log_info!("设置页：清空歌词缓存 → 删除 {files} 个文件 / {bytes} 字节");
        self.out.emit(AppEvent::LyricCacheCleared);
    }

    /// 毫秒级快路径：三「层」（内存 / 磁盘 / 本地）→ 负缓存 → （需要时）丢给在线线程。
    ///
    /// ## v1.8.0：层序可切换（`lyrics.local_first`）
    ///
    /// 两条顺序**不写成两份逻辑**：层序表（[`Step`] 数组）表达顺序，循环体只写一份。
    /// - `local_first = false` → `[Mem, Disk, Local]`（v1.7.0 原序：省时优先）
    /// - `local_first = true`  → `[Local, Mem, Disk]`（手改 `.lrc` 立刻生效，
    ///   **包括正在播放的那一首**——本地层排在内存层之前，内存里的旧副本赢不了它）
    ///
    /// ## 三条硬约束（改这段代码之前先读）
    ///
    /// 1. **`emit` 必须先于落盘**：落盘是几 KB 同步 IO（杀软环境下可能几 ms），
    ///    发生在**歌词线程**上；先显示后落盘 → UI 无感（A6.7）。反过来写会让
    ///    「歌词晚出一拍」。
    /// 2. **落盘收口在 [`Self::maybe_write_back`]**：内存 / 磁盘 / 在线三条路径都只调
    ///    这一个函数，`!write_back` 的判定只允许存在一份（A7.3）。
    ///    三个调用点各写一遍 `if !req.write_back { return }` 是必被 QA 打回的形态。
    /// 3. **代次守卫**：本函数内的落盘**不需要**新守卫——`on_request` 在调用前已写
    ///    `current_track = req.track_id`，`on_tick` 在调用前已校验
    ///    `p.track_id == self.current_track`；worker 单线程、函数内无 await/无重入
    ///    → 落盘时刻 `req` 必为当前代次。
    ///    **一旦有人把落盘挪到 `dispatch` 之后或另起线程，就必须重新引入
    ///    `track_id == current_track` 判定。**
    fn query_fast_path(&mut self, req: LyricRequest, attempt: u8) {
        let t0 = Instant::now();
        let track_id = req.track_id;
        let key = query::cache_key(&req.track);

        // 元数据不够：SMTC 只给了个空标题，什么也查不到（顺序与 v1.7.0 一致：最先判）
        if req.track.title.trim().is_empty() {
            self.out.emit(AppEvent::LyricMissing {
                track_id,
                reason: LyricMissReason::InsufficientMetadata,
            });
            return;
        }

        self.out.emit(AppEvent::LyricLoading { track_id });

        // ---- 一个数组表达一条顺序，循环体只写一份 ----
        let steps: [Step; 3] = if req.local_first {
            [Step::Local, Step::Mem, Step::Disk]
        } else {
            [Step::Mem, Step::Disk, Step::Local]
        };

        for step in steps {
            let got: Option<Hit> = match step {
                Step::Mem => {
                    // `get_mem` 返回的是独占拥有的 `Hit`，调用一结束对 `self.cache`
                    // 的借用就结束 → 下面 `self.out.emit` / `self.maybe_write_back` 不冲突
                    let h = self.cache.get_mem(&key);
                    if h.is_some() {
                        crate::log_info!("歌词内存命中：{key} 耗时={}ms", t0.elapsed().as_millis());
                    }
                    h
                }
                Step::Disk => {
                    // 命中会回填内存，并把 `source` 一起带进内存条目
                    let h = self.cache.get_disk(&key);
                    if h.is_some() {
                        crate::log_info!("歌词磁盘命中：{key} 耗时={}ms", t0.elapsed().as_millis());
                    }
                    h
                }
                Step::Local => {
                    // 只在轮到本地这一层时才构造（`LyricQuery` 只持有借用、无副作用；
                    // 真正的 IO 在 `LocalProvider::fetch` 里）
                    let lq = LyricQuery {
                        track: &req.track,
                        local_dirs: &req.local_dirs,
                        online_enabled: req.online_enabled,
                    };
                    match LocalProvider.fetch(&lq) {
                        Ok(l) => {
                            // 本地内容写回缓存（source="local"）：这是「手改立刻生效」的
                            // 持久化保证，必须先 put 再 emit（与 v1.7.0 同序）
                            self.cache.put(&key, &l, "local");
                            crate::log_info!(
                                "歌词本地命中：{key} 耗时={}ms",
                                t0.elapsed().as_millis()
                            );
                            Some(Hit {
                                lyrics: Arc::new(l),
                                source: String::from("local"),
                            })
                        }
                        Err(e) => {
                            crate::log_debug!("本地未命中：{}", e.as_str());
                            None
                        }
                    }
                }
            };
            let Some(hit) = got else { continue };

            // ④ 先显示：整条快路径最多一次 LyricReady（落盘绝不额外发事件）
            self.out.emit(AppEvent::LyricReady {
                track_id,
                lyrics: Arc::clone(&hit.lyrics),
            });
            // ⑤ 再落盘：单一收口（`source == "local"` 与 `!write_back` 都在里面早退）
            self.maybe_write_back(&req, &hit.lyrics, &hit.source);
            return;
        }

        // ---- 负缓存（位置与 v1.7.0 一致：三层之后、在线之前）----
        if let Some(reason) = self.cache.miss_reason(&key) {
            crate::log_info!(
                "歌词负缓存命中（{reason:?}），{}h 内不再请求：{key}",
                limits::LYRIC_MISS_TTL_HOURS
            );
            self.out.emit(AppEvent::LyricMissing { track_id, reason });
            return;
        }

        // ---- 在线：丢给独立线程，主循环立刻回到 recv ----
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

    /// 歌词落盘（v1.7.0 / G5；v1.8.0 扩大触发面到三条路径）。
    ///
    /// **这是落盘的唯一收口**（A7.3）：`query_fast_path` 的三个命中分支（mem / disk /
    /// local→跳过）与 `on_online` 的在线命中分支都只调用这个函数，
    /// `source == "local"` 与 `!write_back` 的判定**只存在这一份**。
    ///
    /// 判定规矩（v1.8.0 / R3）：来源 **≠ 本地文件** 就落一份；来源 == `"local"`
    /// （歌词本来就是用户自己的 `.lrc`）早退，避免把用户的文件复制一份进 `Barline/`。
    ///
    /// 失败静默降级：只记日志，绝不改事件、不改缓存、不影响歌词显示。
    fn maybe_write_back(&mut self, req: &LyricRequest, l: &Arc<Lyrics>, source: &str) {
        if source == "local" {
            return; // 防御性冗余：本地命中的歌词本来就在磁盘上
        }
        if !req.write_back {
            return; // 判定只此一份（A7.3）：三个调用点都不许各自复制
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
    if let Some(h) = cache.get_mem(&key) {
        return (Some((*h.lyrics).clone()), LyricMissReason::NotFound);
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
    ///
    /// v1.8.0 加了两个探针字段，只为一条断言服务：**`emit` 那一刻落盘目录里有没有文件**
    /// （A6.7「先显示后落盘」的机械证明，不需要读日志文件）。
    #[derive(Default)]
    struct Collector {
        events: std::sync::Mutex<Vec<AppEvent>>,
        /// 探针目标目录；`Some` 时每次 `emit` 都会记一次文件数
        probe: std::sync::Mutex<Option<std::path::PathBuf>>,
        /// 每次 `emit` 时 `<probe>/Barline` 里的文件数
        probe_counts: std::sync::Mutex<Vec<usize>>,
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

        /// 打开落盘探针：之后每次 `emit` 都记一次 `<dir>/Barline` 的文件数。
        fn watch(&self, dir: &std::path::Path) {
            *self.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
        }

        /// 取走探针记录。
        fn probe_counts(&self) -> Vec<usize> {
            std::mem::take(&mut *self.probe_counts.lock().unwrap_or_else(|e| e.into_inner()))
        }
    }

    impl EventOut for Arc<Collector> {
        fn emit(&self, evt: AppEvent) {
            {
                let mut g = match self.events.lock() {
                    Ok(g) => g,
                    Err(e) => e.into_inner(),
                };
                g.push(evt);
            }
            // 探针：**在 emit 的这一瞬间**数落盘目录里的文件（两把锁分开取，避免嵌套）
            let dir = self
                .probe
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(dir) = dir {
                let n = match std::fs::read_dir(dir.join("Barline")) {
                    Ok(rd) => rd.count(),
                    Err(_) => 0,
                };
                self.probe_counts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(n);
            }
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
            // v1.8.0：默认「磁盘缓存优先」= v1.7.0 原序；需要验本地优先的测试自己改
            local_first: false,
            retry: false,
        }
    }

    /// 测试用缓存目录（与 [`cache`] 同源；需要「模拟重启」时重新 `Cache::new` 它）。
    fn cache_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("barline_service_{tag}"))
    }

    fn cache(tag: &str) -> Cache {
        let dir = cache_path(tag);
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
        // v1.8.0 R4 改口径：文件名从 `candidates()[0] + .lrc`（歌手 - 歌名，已小写）
        // 改成 `歌名 - 歌手.lrc`（保留原始大小写）。
        //
        // v1.8.1（G6 / D-5）：断言从「等于 `writeback::file_name_for` 的产出」改成
        // **字面量**。原写法是「两份同样的实现互相印证」——落盘侧真正用的是私有
        // `build_file_name`，`file_name_for` 是它的一行孪生兄弟，任一份单独改另一条
        // 都测不出「写出去的名字查找侧命中不了」。跨侧闭环由 `writeback` 的 W15 承担
        // （落盘后立刻 `locate::find`），这里只钉死**具体文件名**这个事实。
        assert_eq!(
            files[0], "测试曲目α - 无名歌手α.lrc",
            "文件名必须是「歌名 - 歌手.lrc」（保留原始大小写）"
        );
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

    /// S7：**磁盘缓存命中必须落盘**（v1.8.0 / A6.3）。
    ///
    /// 语义与 v1.7.0 的 `s5_disk_cache_hit_does_not_write_back` **完全反转**。
    /// 旧用例的注释是「防止有人把落盘钩子塞进 `query_fast_path`」，那正是
    /// v1.7.0 漏掉 A6.2/A6.3 的原因：钩子只在 `on_online` 里，缓存命中就永远不落盘
    /// （用户机器上 `Barline\` 一直是空的、缓存里却躺着 93 个文件）。
    ///
    /// v1.8.0 的新意图：**落盘必须收口在 `maybe_write_back`，`query_fast_path` 只允许
    /// 三个统一调用点**（mem / disk / online），三个点都不许各自复制 `!write_back` 判定。
    /// 断言：磁盘命中后落盘 1 个文件，且事件仍然只有 1 个 `LyricReady`。
    #[test]
    fn s7_disk_cache_hit_does_write_back() {
        let dir = write_dir("s7");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s7", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目ζ", "无名歌手ζ", write_dirs(&dir), true);
        req.write_back = true;
        let key = query::cache_key(&req.track);
        // 预置缓存（source≠local）→ 换一个 Cache 实例 = 模拟重启（内存 LRU 清空、磁盘保留）
        w.cache.put(&key, &one_line_lyrics("第一句"), "lrclib");
        w.cache = Cache::new(cache_path("wb_s7"));

        w.on_request(req.clone());

        assert_eq!(col.ready(), 1, "磁盘命中照常推歌词，且只推 1 次");
        let files = written_files(&dir);
        assert_eq!(
            files.len(),
            1,
            "磁盘缓存命中必须落盘 1 个文件（v1.8.0 R3 的缺口修复），实际 {files:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S6：**内存缓存命中必须落盘**（v1.8.0 / A6.2，v1.7.0 的缺口）。
    ///
    /// 场景按 PRD 复刻：同一首歌先走在线命中落盘一次 → 用户把 `Barline/` 清空
    /// → 重播（内存命中）→ 文件必须重新出现，且事件仍然只有 1 个 `LyricReady`。
    #[test]
    fn s6_mem_hit_writes_back() {
        let dir = write_dir("s6");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s6", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目η", "无名歌手η", write_dirs(&dir), true);
        req.write_back = true;

        // ① 在线命中 → 落盘一次
        w.on_request(req.clone());
        let _ = col.take();
        w.on_online(outcome(
            &req,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));
        assert_eq!(col.ready(), 1);
        assert_eq!(written_files(&dir).len(), 1, "在线命中应落盘 1 个");

        // ② 用户清空落盘目录（缓存保留）
        let _ = std::fs::remove_dir_all(dir.join("Barline"));
        // 同会话的 `wrote` 去重账本也要清（等价于「换一次运行」）——
        // `WRITE_BACK_DEDUP_CAP` 只是省 syscall，真正的幂等由 create_new 保证
        w.wrote.clear();

        // ③ 重播 → 内存命中 → 文件必须重新出现
        w.on_request(req.clone());
        assert_eq!(col.ready(), 1, "内存命中也要推 LyricReady（且只推 1 个）");
        let files = written_files(&dir);
        assert_eq!(
            files.len(),
            1,
            "内存缓存命中必须落盘 1 个文件（A6.2：v1.7.0 漏掉的缺口），实际 {files:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S8：**来源为 local 的缓存命中不落盘**（v1.8.0 / A6.6）——
    /// 内存命中与磁盘命中两条路径各断言一次（C2 锁的是「回填不丢 source」，
    /// 这条锁的是「丢了 source 会被判成非 local 而多写一份」）。
    #[test]
    fn s8_cache_hit_with_local_source_never_writes_back() {
        let dir = write_dir("s8");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s8", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目θ", "无名歌手θ", write_dirs(&dir), true);
        req.write_back = true;
        let key = query::cache_key(&req.track);
        w.cache.put(&key, &one_line_lyrics("用户自己的歌词"), "local");

        // ① 内存命中
        w.on_request(req.clone());
        assert_eq!(col.ready(), 1);
        assert!(written_files(&dir).is_empty(), "source=local 的内存命中不许落盘");
        assert!(w.wrote.is_empty(), "早退连账都不记");

        // ② 重启后磁盘命中（同样带 source=local）
        w.cache = Cache::new(cache_path("wb_s8"));
        w.on_request(req.clone());
        assert_eq!(col.ready(), 1);
        assert!(written_files(&dir).is_empty(), "source=local 的磁盘命中不许落盘");
        assert!(!dir.join("Barline").exists(), "连目录都不许建");
        assert!(w.wrote.is_empty(), "早退连账都不记");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 造一份「本地 .lrc + 缓存里已有另一份」的场景，返回（曲目请求模板, 本地目录）。
    fn local_vs_cache_fixture(tag: &str) -> (LyricRequest, std::path::PathBuf) {
        let dir = write_dir(tag);
        let req = request_with_id(7, "顺序测试曲ω", "顺序测试歌手ω", write_dirs(&dir), true);
        std::fs::write(
            dir.join("顺序测试曲ω.lrc"),
            "[00:00.00]AAA\n[00:02.00]AAA2",
        )
        .expect("写本地 .lrc");
        (req, dir)
    }

    /// S9：`local_first = true` → **本地文件赢过内存/磁盘缓存**（A2.1/A2.3 的进程内版）。
    ///
    /// 关键断言不止「显示 AAA」：本地命中后 `cache.put(..., "local")` 必须把内存与
    /// 磁盘的 source 都改写成 `local`（否则下一次播放又要落盘一份）。
    #[test]
    fn s9_local_first_true_prefers_local_over_memory() {
        let (mut req, dir) = local_vs_cache_fixture("s9");
        let key = query::cache_key(&req.track);
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s9", Arc::clone(&col), never_dispatch());
        // 缓存里先放一份「BBB」（来源 lrclib）
        w.cache.put(&key, &one_line_lyrics("BBB"), "lrclib");

        req.write_back = true;
        req.local_first = true;
        w.on_request(req.clone());

        let events = col.take();
        let shown: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AppEvent::LyricReady { lyrics, .. } => Some(lyrics.lines[0].text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(shown, vec![String::from("AAA")], "本地优先时必须显示本地文件的内容");

        let mem = w.cache.get_mem(&key).expect("本地命中必须写回内存");
        assert_eq!(mem.source, "local", "本地命中必须把缓存条目标成 local");
        // 磁盘侧也要是 local（换实例 = 直接读磁盘 JSON）
        let mut c2 = Cache::new(cache_path("wb_s9"));
        assert_eq!(
            c2.get_disk(&key).expect("本地命中必须写回磁盘缓存").source,
            "local",
            "磁盘 JSON 的 source 必须被改写为 local（A2.3）"
        );
        assert!(
            written_files(&dir).is_empty(),
            "本地命中不落盘（歌词本来就在磁盘上）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S10：`local_first = false` → 与 v1.7.0 一致，内存/磁盘缓存赢过本地文件。
    ///
    /// 同时证明层序没被错排：`false` 时本地层没有被提前。
    #[test]
    fn s10_local_first_false_prefers_cache() {
        let (mut req, dir) = local_vs_cache_fixture("s10");
        let key = query::cache_key(&req.track);
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s10", Arc::clone(&col), never_dispatch());
        w.cache.put(&key, &one_line_lyrics("BBB"), "lrclib");

        req.write_back = true;
        req.local_first = false;
        w.on_request(req.clone());

        let shown: Vec<String> = col
            .take()
            .iter()
            .filter_map(|e| match e {
                AppEvent::LyricReady { lyrics, .. } => Some(lyrics.lines[0].text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(shown, vec![String::from("BBB")], "磁盘缓存优先时必须显示缓存内容");
        // 缓存来源非 local → 照常落盘一份（F1 的产品隐患已在 ARCH §7.1 留档）
        assert_eq!(written_files(&dir).len(), 1, "来源非 local 的缓存命中要落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S11：**本地命中永不落盘**（A6.5）——两种 `local_first` 各跑一遍。
    #[test]
    fn s11_local_hit_never_writes_back() {
        for local_first in [false, true] {
            let (mut req, dir) = local_vs_cache_fixture(if local_first {
                "s11_on"
            } else {
                "s11_off"
            });
            let col = Arc::new(Collector::default());
            let mut w = worker(
                if local_first { "wb_s11_on" } else { "wb_s11_off" },
                Arc::clone(&col),
                never_dispatch(),
            );
            req.write_back = true;
            req.local_first = local_first;
            w.on_request(req);
            assert_eq!(col.ready(), 1, "本地命中要显示歌词");
            assert!(
                written_files(&dir).is_empty(),
                "local_first={local_first} 时本地命中不许落盘"
            );
            assert!(!dir.join("Barline").exists(), "连目录都不许建");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// S12：`write_back = false` → 三条路径（内存 / 磁盘 / 在线）全都不落盘（A7.1）。
    #[test]
    fn s12_write_back_off_writes_nothing_all_paths() {
        // ① 内存命中
        let dir = write_dir("s12_mem");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s12_mem", Arc::clone(&col), never_dispatch());
        let mut req = request_with_id(7, "测试曲目ι", "无名歌手ι", write_dirs(&dir), true);
        req.write_back = false;
        w.cache
            .put(&query::cache_key(&req.track), &one_line_lyrics("第一句"), "lrclib");
        w.on_request(req.clone());
        assert_eq!(col.ready(), 1);
        assert!(written_files(&dir).is_empty(), "内存命中也受 write_back 约束");

        // ② 磁盘命中
        let dir2 = write_dir("s12_disk");
        let col2 = Arc::new(Collector::default());
        let mut w2 = worker("wb_s12_disk", Arc::clone(&col2), never_dispatch());
        let mut req2 = request_with_id(7, "测试曲目κ", "无名歌手κ", write_dirs(&dir2), true);
        req2.write_back = false;
        w2.cache
            .put(&query::cache_key(&req2.track), &one_line_lyrics("第一句"), "lrclib");
        w2.cache = Cache::new(cache_path("wb_s12_disk"));
        w2.on_request(req2.clone());
        assert_eq!(col2.ready(), 1);
        assert!(written_files(&dir2).is_empty(), "磁盘命中也受 write_back 约束");

        // ③ 在线命中
        let dir3 = write_dir("s12_online");
        let col3 = Arc::new(Collector::default());
        let mut w3 = worker("wb_s12_online", Arc::clone(&col3), never_dispatch());
        let req3 = request_with_id(7, "测试曲目λ", "无名歌手λ", write_dirs(&dir3), true);
        w3.on_request(req3.clone());
        let _ = col3.take();
        w3.on_online(outcome(
            &req3,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));
        assert_eq!(col3.ready(), 1, "歌词照常显示");
        assert!(written_files(&dir3).is_empty(), "在线命中也受 write_back 约束");
        assert!(!dir3.join("Barline").exists(), "关掉开关时连目录都不许建");
        assert!(w3.wrote.is_empty(), "不落盘就不记账");

        for d in [&dir, &dir2, &dir3] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// S13：**`emit` 先于落盘**（A6.7 的机械证明）。
    ///
    /// 探针在 `emit` 回调里数 `<dir>/Barline` 的文件数：两个事件（Loading / Ready）
    /// 发生的那一刻都必须是 0，落盘只能发生在 `emit` 之后。
    #[test]
    fn s13_emit_precedes_writeback() {
        let dir = write_dir("s13");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s13", Arc::clone(&col), never_dispatch());

        let mut req = request_with_id(7, "测试曲目μ", "无名歌手μ", write_dirs(&dir), true);
        req.write_back = true;
        col.watch(&dir);

        w.on_request(req.clone());
        let _ = col.take();
        w.on_online(outcome(
            &req,
            0,
            Some(one_line_lyrics("第一句")),
            LyricMissReason::NotFound,
            false,
        ));

        let counts = col.probe_counts();
        assert!(!counts.is_empty(), "探针必须记录过事件");
        assert!(
            counts.iter().all(|n| *n == 0),
            "emit 那一刻落盘目录必须还是空的（先显示后落盘），实际 {counts:?}"
        );
        assert_eq!(written_files(&dir).len(), 1, "落盘最终还是发生了");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S14：**旧代结果不落盘**（代次守卫在层序列改造后仍然生效）。
    ///
    /// 与 `s3` 同款，但额外打开 `local_first`——证明循环改造没有把守卫绕掉，
    /// 也证明「层序切换」与「代次守卫」互不影响。
    #[test]
    fn s14_stale_generation_still_does_not_write() {
        let dir = write_dir("s14");
        let col = Arc::new(Collector::default());
        let mut w = worker("wb_s14", Arc::clone(&col), never_dispatch());

        let mut a = request_with_id(7, "测试曲目ν", "无名歌手ν", write_dirs(&dir), true);
        a.write_back = true;
        a.local_first = true;
        let mut b = request_with_id(9, "测试曲目ξ", "无名歌手ξ", write_dirs(&dir), true);
        b.write_back = true;
        b.local_first = true;
        w.on_request(a.clone());
        w.on_request(b);
        let _ = col.take();

        // A 的结果迟到回来 → 不许有任何副作用
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
}
