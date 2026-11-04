//! SMTC（`Windows.Media.Control`）媒体线程（`ARCHITECTURE.md` §6.4 / T05）。
//!
//! **三个必须照做的坑（§2.2）**：
//! 1. `IAsyncOperation` 在 windows 0.62.2 里没有 `get()`，只有 **`.join()`**；
//!    且 `.join()` 只能在 **MTA** 线程调用——放 STA 会死锁。
//! 2. 事件订阅方法**没有 `Add` 前缀**：`CurrentSessionChanged` / `SessionsChanged`。
//! 3. `PlaybackStatus` 常量是 **PascalCase**（`::Playing` / `::Paused` …），不是 `PLAYING`。
//!    且 `TimeSpan.Duration` 单位是 **100ns**，转毫秒要 `/ 10_000`。
//!
//! ## 轮询策略（G2 修复版：分层降频）
//!
//! 病因（用户两阶段实测）：旧实现每 500ms 全量读一次 ≈ 每次 6 个跨进程调用，
//! **约 12 次/秒打进播放器进程**。网易云扛不住，表现为「音乐和鼠标同时卡一下」；
//! 同样节奏下 Edge（B站）与汽水都不卡。所以读取按成本分三层，各走各的节奏：
//!
//! | 层 | 周期 | 内容 | 为什么 |
//! |---|---|---|---|
//! | 快 | 500ms | `GetPlaybackInfo`（播放状态） | 最轻，且状态变化要即时（暂停/播放决定扫光停不停） |
//! | 中 | 1000ms | `GetTimelineProperties`（时长+位置） | UI 侧本来就靠 `position_at` 本地外推，1s 校一次够 |
//! | 慢 | 2000ms | AUMID + `TryGetMediaPropertiesAsync` + 播控标志 + 封面 | 最重的一组，标题/歌手/按钮可用性 2s 内跟上即可 |
//!
//! 另有**立即补读**（当轮同时读时间线与属性，绕过节奏）：播放状态跃迁、
//! 时间线位置回退 > [`limits::MEDIA_TIMELINE_REGRESS_MS`]、时长变化、`forced`
//! （管理器事件脏位 / UI 强制刷新）。这四条覆盖了绝大多数真实切歌。
//!
//! **网易云式退避**：网易云恒报 `(position=0, duration=0)`（日志实证），
//! 连续 [`limits::MEDIA_POLL_ZERO_STREAK`] 次就把时间线拉长到
//! [`limits::MEDIA_POLL_TIMELINE_BACKOFF_MS`]，出现非零值或状态跃迁再恢复——
//! 别拿我们用不上的数据每秒去烦它。
//!
//! ## 事件驱动（G2 修复版第二轮：轮询降级为兜底）
//!
//! 用户实测 v1.4.2：卡顿消失（方向对了），但**切歌不跟手**（最坏等 2s 慢层）。
//! 主通道改成会话级事件：`MediaPropertiesChanged`（标题/歌手/专辑 → 切歌）、
//! `PlaybackInfoChanged`（状态跃迁）、`TimelinePropertiesChanged`（位置/时长）。
//! 回调只置脏位 + 唤醒 UI（它们跑在 WinRT 线程池上，**严禁**在里面做 COM 重活），
//! 脏位并入 `dirty`，下一轮按 `forced` 全量读（60ms 节流）。
//! 订阅跟随 AUMID：换了播放器就重挂；token 保存在 [`SessionSubscriptions`]，
//! Drop 时 `remove_*`（线程退出与会话清空都会走到）。某播放器不发事件时，
//! 行为自动退化为纯轮询（= 上一版，不会更差）。
//!
//! ## 锚点规则（`position_at` 怎么取）
//!
//! `position_at` 是 UI 外推的基准，取错 = 歌词/进度漂移。v1.4.2 曾把它绑到
//! `timeline_at`：对网易云（恒 0/0、时间线退避 5s、切歌由 2s 慢层发现）等于把
//! 歌词钟拨快最多 5s。现在按 [`anchor_of`] 的三规则取值，细节见该函数注释。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackInfo,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs,
    SessionsChangedEventArgs, TimelinePropertiesChangedEventArgs,
};

use crate::config::defaults::limits;
use crate::event::{AppEvent, MediaCommand, MediaCommandKind};
use crate::media::cover::load_cover;
use crate::media::player_map;
use crate::media::{
    CoverImage, MediaSessionInfo, MediaSource, Ms, NowPlaying, PlaybackState, Track, TrackId,
};
use crate::platform::com::{backoff_ms, timed_join, ComApartment};
use crate::platform::perf::{self, Span};
use crate::platform::wake::EventSink;

/// 管理器获取失败时的最大重试次数。
const MANAGER_RETRIES: u32 = 5;
/// 统计窗口（秒）：到点记一条「这一分钟实际调了多少次」。
const STATS_WINDOW_SECS: u64 = 60;
/// 事件驱动的强制补读下限：避免播放器高频事件造成读取风暴（沿用旧值）。
const FORCED_FLOOR_MS: u64 = 60;

/// 封面解码记忆。
///
/// **为什么必须有**：封面解码要 `OpenReadAsync().join()` + WIC 解码（约 3–10ms），
/// 不缓存的话慢层每 2s 就会解一次同一张图，纯属浪费 CPU 与 GDI/COM 句柄。
/// 只有 `track_id` 变化时才真正解码一次（G2 修复版起慢层 2s 才读一次属性，
/// 天然把这条路径又拉慢了一倍，但记忆仍不可省——换歌瞬间会连读两次）。
#[derive(Default)]
struct CoverMemo {
    track_id: TrackId,
    cover: Option<Arc<CoverImage>>,
    done: bool,
}

impl CoverMemo {
    fn get(
        &mut self,
        track_id: TrackId,
        props: &GlobalSystemMediaTransportControlsSessionMediaProperties,
    ) -> Option<Arc<CoverImage>> {
        if self.done && self.track_id == track_id {
            return self.cover.clone();
        }
        self.track_id = track_id;
        // G2 诊断：封面解码 = 跨进程读缩略图流 + WIC 全解码，只在换歌时发生，
        // 但很容易上百毫秒（媒体线程被它占住 → 进度/歌词/播控状态停更）。
        self.cover = {
            let _sp = Span::new("cover.decode");
            load_cover(props)
        };
        self.done = true;
        match self.cover.as_ref() {
            Some(c) => crate::log_info!(
                "封面已解码：{}×{} 主色 #{:02X}{:02X}{:02X}",
                c.width,
                c.height,
                c.dominant.r,
                c.dominant.g,
                c.dominant.b
            ),
            None => crate::log_debug!("该曲目没有可用封面（渲染层用渐变占位）"),
        }
        self.cover.clone()
    }
}

// ---------------------------------------------------------------- 分层节奏状态机

/// 本轮要做的三组跨进程调用。
///
/// 三组的成本差一个数量级，所以**不许**再合并成一次全量读取（那正是病因）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Plan {
    /// 快层：播放状态
    playback: bool,
    /// 中层：时间线（时长 + 位置）
    timeline: bool,
    /// 慢层：AUMID + 元数据 + 播控标志 + 封面
    props: bool,
}

impl Plan {
    fn full() -> Self {
        Self {
            playback: true,
            timeline: true,
            props: true,
        }
    }

    /// 一组都不读就整个跳过本轮（连 `GetCurrentSession` 都不发）。
    fn any(&self) -> bool {
        self.playback || self.timeline || self.props
    }
}

/// 分层节奏 + 「立即补读」判定的状态机。
///
/// **纯逻辑**：时间全部由参数注入（`now`），单测不需要 sleep。
#[derive(Debug, Default)]
struct Cadence {
    last_playback: Option<Instant>,
    last_timeline: Option<Instant>,
    last_props: Option<Instant>,
    /// 连续读到 `(0, 0)` 的次数（网易云特征）
    zero_streak: u8,
    /// 时间线已退避（streak 触发后为真，见 `timeline_interval_ms`）
    timeline_backoff: bool,
    /// 上次读到的时间线（判「位置回退 / 时长变化」）
    prev_timeline: Option<(Ms, Ms)>,
    /// 上一轮的播放状态（判「状态跃迁」）
    prev_state: Option<PlaybackState>,
}

impl Cadence {
    /// 本轮读什么。`forced` = 管理器事件脏位 / UI 强制刷新 → 三组全读。
    fn plan(&self, now: Instant, forced: bool) -> Plan {
        if forced {
            return Plan::full();
        }
        Plan {
            playback: due(self.last_playback, now, limits::MEDIA_POLL_MS),
            timeline: due(self.last_timeline, now, self.timeline_interval_ms()),
            props: due(self.last_props, now, limits::MEDIA_POLL_SLOW_MS),
        }
    }

    /// 时间线当前的有效间隔（退避期拉长到 5s）。
    fn timeline_interval_ms(&self) -> u64 {
        if self.timeline_backoff {
            limits::MEDIA_POLL_TIMELINE_BACKOFF_MS
        } else {
            limits::MEDIA_POLL_MID_MS
        }
    }

    /// 记录一次播放状态读取。返回「是否发生状态跃迁」（→ 立即补读）。
    fn note_playback(&mut self, now: Instant, state: PlaybackState) -> bool {
        self.last_playback = Some(now);
        let jumped = matches!(self.prev_state, Some(p) if p != state);
        self.prev_state = Some(state);
        if jumped {
            // 状态跃迁同时是「恢复时间线轮询」的信号（需求 5）
            self.zero_streak = 0;
            self.timeline_backoff = false;
        }
        jumped
    }

    /// 记录一次时间线读取。返回「是否需要立即补读属性」（切歌 / 重播的信号）。
    fn note_timeline(&mut self, now: Instant, position: Ms, duration: Ms) -> bool {
        self.last_timeline = Some(now);

        // ---- 网易云式退避 ----
        if position == 0 && duration == 0 {
            self.zero_streak = self.zero_streak.saturating_add(1);
            if !self.timeline_backoff && self.zero_streak >= limits::MEDIA_POLL_ZERO_STREAK {
                self.timeline_backoff = true;
                crate::log_debug!(
                    "时间线连续 {} 次为 (0,0)（网易云特征），轮询退避到 {}ms",
                    self.zero_streak,
                    limits::MEDIA_POLL_TIMELINE_BACKOFF_MS
                );
            }
        } else {
            if self.timeline_backoff {
                crate::log_debug!(
                    "时间线恢复非零值（{}ms/{}ms），轮询回到 {}ms",
                    position,
                    duration,
                    limits::MEDIA_POLL_MID_MS
                );
            }
            self.zero_streak = 0;
            self.timeline_backoff = false;
        }

        // ---- 立即补读判定 ----
        let mut refill = false;
        if let Some((prev_pos, prev_dur)) = self.prev_timeline {
            // 位置大幅倒退 = 切歌 / 重播 / 大幅后退 seek。
            // 用 saturating_sub：异常数据（位置暴涨再暴跌）也不许 panic。
            if prev_pos.saturating_sub(position) > limits::MEDIA_TIMELINE_REGRESS_MS {
                refill = true;
            }
            if duration != prev_dur {
                refill = true;
            }
        }
        self.prev_timeline = Some((position, duration));
        refill
    }

    /// 记录一次属性组读取。
    fn note_props(&mut self, now: Instant) {
        self.last_props = Some(now);
    }
}

/// 「到点了吗」。`None` = 从没读过 → 必读（首轮）。
fn due(last: Option<Instant>, now: Instant, interval_ms: u64) -> bool {
    match last {
        None => true,
        Some(t) => now.saturating_duration_since(t) >= Duration::from_millis(interval_ms),
    }
}

/// **锚点规则**：这份快照的 `position_at`（UI 外推的基准）取哪个时刻。
///
/// 取错就是「歌词/进度与音频漂移」，三种情况（按顺序判）：
/// 1. 本轮**读过时间线** → 用该次读取的时刻（`timeline_at`，恰好就是本轮 `now`）。
///    位置是新鲜的，外推从读取时刻起算；
/// 2. 本轮没读时间线，但 `track_id` 相对上一份快照**变了**（切歌）→ 用**检测时刻**。
///    网易云恒报 `(0,0)`、时间线还在退避里，切歌只能由慢层发现——旧版歌词能同步，
///    靠的正是「位置恒 0、歌词钟从检测时刻起走表」，这条规则把它显式化了；
/// 3. 其余 → **沿用上一份快照的锚点**。绝不许退回陈旧的 `timeline_at`
///    （v1.4.2 的 bug：退避下它是切歌之前 5s 的时刻，歌词直接快 5s）。
///
/// `prev` 是上一份快照的 `position_at`；首轮没有上一份时兜底用 `now`。
fn anchor_of(
    read_timeline_this_tick: bool,
    timeline_at: Option<Instant>,
    track_changed: bool,
    now: Instant,
    prev: Option<Instant>,
) -> Instant {
    if read_timeline_this_tick {
        return timeline_at.unwrap_or(now);
    }
    if track_changed {
        return now;
    }
    prev.unwrap_or(now)
}

// ---------------------------------------------------------------- 分层读数缓存

/// 各层最近一次读到的数据。
///
/// 为什么必须缓存：分层后一次 tick 只补缺的层，而 `NowPlaying` 是**整体**快照，
/// 没读到的那层就用上一次的值（它们的时效由各自的节奏保证）。
#[derive(Default)]
struct SessionParts {
    aumid: String,
    app_name: String,
    process_name: String,
    /// 标题 / 歌手 / 专辑。**时长不存这里**（唯一来源是 [`SessionParts::duration_ms`]，
    /// 拼 Track 时再填，避免同一数据有两份导致 `track_id` 漂移）。
    track: Track,
    state: PlaybackState,
    position_ms: Ms,
    duration_ms: Ms,
    /// 这份位置是**什么时候**读到的（外推基准，见 `snapshot`）
    timeline_at: Option<Instant>,
    can_prev: bool,
    can_next: bool,
    cover: Option<Arc<CoverImage>>,
    /// 属性组至少成功读过一次。成功之前不推事件——
    /// 与旧实现「`read_session` 失败就整轮放弃」的语义一致。
    props_ok: bool,
}

/// 拼快照用的最终 `Track`（时长只来自时间线层）。
fn effective_track(parts: &SessionParts) -> Track {
    Track {
        duration_ms: parts.duration_ms,
        ..parts.track.clone()
    }
}

/// `track_id` 的唯一计算点（公式与旧实现完全一致：`fnv1a64(track.key())`）。
fn parts_track_id(parts: &SessionParts) -> TrackId {
    let track = effective_track(parts);
    if track.is_empty() {
        0
    } else {
        fnv1a64(track.key().as_bytes())
    }
}

impl SessionParts {
    /// 拼一份完整快照。`anchor` 由 [`anchor_of`] 的三规则决定（见其注释）。
    fn snapshot(&self, anchor: Instant) -> NowPlaying {
        let track = effective_track(self);
        NowPlaying {
            track_id: parts_track_id(self),
            track: Some(track),
            session: MediaSessionInfo {
                aumid: self.aumid.clone(),
                app_name: self.app_name.clone(),
                process_id: 0,
                process_name: self.process_name.clone(),
                source: MediaSource::Smtc,
            },
            state: self.state,
            position_ms: self.position_ms,
            // 锚点绝不许就地拍脑袋取 `Instant::now()`：分层后位置可能来自
            // 上一轮（1s~5s 前），外推基准错了进度条/歌词钟就漂了。
            position_at: anchor,
            can_prev: self.can_prev,
            can_next: self.can_next,
            can_seek: self.duration_ms > 0,
            cover: self.cover.clone(),
        }
    }
}

// ---------------------------------------------------------------- 每分钟统计

/// 一分钟窗口统计（验收用：证明降频真的生效）。
///
/// 平时只是几个计数器；到点才格式化一次日志。
struct PollStats {
    playback: u32,
    props: u32,
    timeline: u32,
    /// 四段里最慢的一次（含没到 8ms 阈值、没写日志的那些）
    max_ms: u128,
    window_start: Instant,
}

impl PollStats {
    fn new(window_start: Instant) -> Self {
        Self {
            playback: 0,
            props: 0,
            timeline: 0,
            max_ms: 0,
            window_start,
        }
    }

    fn observe(&mut self, ms: u128) {
        self.max_ms = self.max_ms.max(ms);
    }

    /// 窗口到点记一条并清零。返回是否记了（单测断言用）。
    ///
    /// v1.4.4（G2-A4）：同一行**追加**采样线程的抓屏统计（`capture=N 慢M 最慢=Xms`）。
    /// 采样线程用无锁原子累计，这里「取走即清零」，与其他字段同语义（都是本窗口的数）。
    ///
    /// 段名仍写 `smtc 轮询统计`：它是 `progress.md` 里登记的日志检索关键字，
    /// 改名会让所有排查手册失效——宁可名字略窄，也不要让下一次接手的人 grep 不到。
    fn flush_if_due(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.window_start) < Duration::from_secs(STATS_WINDOW_SECS)
        {
            return false;
        }
        let cap = perf::take_capture_window();
        crate::log_info!(
            "[perf] smtc 轮询统计 {}s：playback={} props={} timeline={} 最慢={}ms capture={} 慢={} capture最慢={}ms",
            STATS_WINDOW_SECS,
            self.playback,
            self.props,
            self.timeline,
            self.max_ms,
            cap.count,
            cap.slow,
            cap.max_ms
        );
        *self = Self::new(now);
        true
    }
}

// ---------------------------------------------------------------- 媒体线程状态

/// 媒体线程的全部可变状态。
struct PollState {
    /// 上次广播的快照（`has_changed` 判重用；**也是锚点规则 3 的来源**）
    last: Option<NowPlaying>,
    cover: CoverMemo,
    /// 各层缓存
    parts: SessionParts,
    cadence: Cadence,
    stats: PollStats,
    /// 上次轮询时刻（`forced` 的 60ms 下限用）
    last_tick: Instant,
    /// 事件脏位（管理器级 + 会话级事件共用；回调只置位，主循环消费）
    dirty: Arc<AtomicBool>,
    /// 会话级事件订阅（AUMID 变了就重挂；`None` = 还没挂过 / 会话已清空）
    session_events: Option<SessionSubscriptions>,
}

impl PollState {
    fn new(dirty: Arc<AtomicBool>) -> Self {
        Self {
            last: None,
            cover: CoverMemo::default(),
            parts: SessionParts::default(),
            cadence: Cadence::default(),
            stats: PollStats::new(Instant::now()),
            last_tick: Instant::now(),
            dirty,
            session_events: None,
        }
    }

    /// 事件脏位是否已过 60ms 下限（不到点就先攒着，下一轮再读）。
    fn forced_due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_tick) >= Duration::from_millis(FORCED_FLOOR_MS)
    }

    /// v1.5.5 / G3.5：**解冻后的第一轮必须是全量读**。
    ///
    /// 不许等 500 / 1000 / 2000ms 的常规节奏，也不许被
    /// [`FORCED_FLOOR_MS`]（60ms）挡住。做法是三管齐下，全部只改状态、不改常量：
    /// 1. `Cadence::default()` → 三层 `last_*` 全为 `None`，[`due`] 恒真；
    /// 2. `last_tick` 回拨一个下限 → `forced_due` 立即成立；
    /// 3. 置脏位 → 主循环的 `schedule` 拿到 `dirty_set = true`。
    ///
    /// 于是无论本轮是「收到 `Refresh`」还是「50ms 超时」，`schedule` 都返回
    /// `(Plan::full(), true)`；UI 连发 `SetPaused(false) + Refresh` 也只会有
    /// **一次**全量读（两者都只置状态，由本轮末尾统一的 `tick` 执行）。
    fn reset_for_resume(&mut self, now: Instant) {
        self.cadence = Cadence::default();
        self.last_tick = now
            .checked_sub(Duration::from_millis(FORCED_FLOOR_MS))
            .unwrap_or(now);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// **主循环每轮（50ms）的调度**：统计落地 + forced 判定 + 分层计划。
    ///
    /// 与 COM 无关的部分集中在这里，`run` 只负责收发——这样「统计确实每轮都刷」
    /// 和「脏位 → forced 的映射」才能被单测覆盖。
    /// 返回 `(计划, 是否消费了 forced)`；落脏位由调用方做。
    fn schedule(&mut self, now: Instant, dirty_set: bool) -> (Plan, bool) {
        // 统计日志必须在**主循环**里落地，而不是藏在 tick 里——
        // 否则「一组都没到点」的轮次会把统计窗口拖长，数字就不可信了。
        self.stats.flush_if_due(now);
        let forced = dirty_set && self.forced_due(now);
        let plan = self.cadence.plan(now, forced);
        (plan, forced)
    }

    /// 事件驱动的主通道：AUMID 变了（或还没挂过）就重挂会话级事件。
    ///
    /// 为什么跟着 AUMID 走：订阅挂在**具体会话对象**上，播放器切换后旧订阅
    /// 不会再响；而「会话对象换了但 AUMID 相同」的场景由管理器级
    /// `SessionsChanged` 事件兜底（它也会置脏位走 forced 全量读）。
    fn ensure_session_events(
        &mut self,
        session: &GlobalSystemMediaTransportControlsSession,
        sink: &EventSink<AppEvent>,
    ) {
        if self.parts.aumid.is_empty() {
            return;
        }
        let need = match &self.session_events {
            Some(s) => s.aumid != self.parts.aumid,
            None => true,
        };
        if !need {
            return;
        }
        self.session_events = Some(subscribe_session(
            session,
            &self.dirty,
            sink,
            self.parts.aumid.clone(),
        ));
    }

    /// 按计划做一轮读取，diff 后决定是否广播。
    fn tick(&mut self, mgr: &Manager, sink: &EventSink<AppEvent>, now: Instant, mut plan: Plan) {
        self.last_tick = now;

        let session = match mgr.GetCurrentSession() {
            Ok(s) => s,
            Err(_) => {
                if self.last.is_some() {
                    crate::log_info!("session cleared");
                    self.last = None;
                    self.cover.done = false;
                    // 会话没了 → 分层缓存、节奏、会话级订阅全部作废，
                    // 下次会话从全量读开始
                    self.parts = SessionParts::default();
                    self.cadence = Cadence::default();
                    self.session_events = None;
                    sink.send(AppEvent::SessionCleared);
                }
                return;
            }
        };

        // ---- 快层：播放状态（最轻，必须即时）----
        // `read_timeline_now` 供锚点规则 1 使用
        let mut read_timeline_now = false;
        let mut playback_info = None;
        if plan.playback {
            let (info, ms) = timed("smtc.playback", || session.GetPlaybackInfo().ok());
            self.stats.playback += 1;
            self.stats.observe(ms);
            let state = playback_state_of(info.as_ref());
            // 状态跃迁 → 时间线与属性都在本轮立即补读，
            // 这样事件里带的是**新状态下**的真实位置，而不是最多 1s 前的旧值。
            if self.cadence.note_playback(now, state) {
                plan.timeline = true;
                plan.props = true;
            }
            self.parts.state = state;
            playback_info = info;
        }

        // ---- 中层：时间线（时长 + 位置）----
        if plan.timeline {
            let ((position, duration), ms) = timed("smtc.timeline", || read_timeline(&session));
            self.stats.timeline += 1;
            self.stats.observe(ms);
            // 位置回退 / 时长变化 ≈ 切歌：立刻补读属性，别让新标题等 2s
            if self.cadence.note_timeline(now, position, duration) {
                plan.props = true;
            }
            self.parts.position_ms = position;
            self.parts.duration_ms = duration;
            // **只有这里**许更新 timeline_at（锚点规则的依据，别处一律不碰）
            self.parts.timeline_at = Some(now);
            read_timeline_now = true;
        }

        // ---- 慢层：AUMID + 元数据 + 播控标志 + 封面 ----
        if plan.props {
            let (meta, ms) = timed("smtc.props", || {
                let aumid = session
                    .SourceAppUserModelId()
                    .map(|h| h.to_string())
                    .unwrap_or_default();
                let props = timed_join("TryGetMediaPropertiesAsync", || {
                    session.TryGetMediaPropertiesAsync()?.join()
                });
                (aumid, props)
            });
            self.stats.props += 1;
            self.stats.observe(ms);

            match meta.1 {
                Ok(props) => {
                    self.parts.aumid = meta.0;
                    let resolved = player_map::resolve(&self.parts.aumid);
                    self.parts.app_name = resolved.display_name.to_string();
                    self.parts.process_name = resolved.process_name.to_string();
                    self.parts.track = Track {
                        title: props.Title().map(|h| h.to_string()).unwrap_or_default(),
                        artist: props.Artist().map(|h| h.to_string()).unwrap_or_default(),
                        album: props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default(),
                        // 时长不在这里填（唯一来源是时间线层，见 effective_track）
                        duration_ms: 0,
                    };
                    // 封面按 track_id 记忆：换歌才解码一次（见 CoverMemo）
                    self.parts.cover = self.cover.get(parts_track_id(&self.parts), &props);
                    self.parts.props_ok = true;
                    // 属性里带 AUMID → 在这里挂/重挂会话级事件（事件驱动主通道）
                    self.ensure_session_events(&session, sink);
                }
                Err(e) => {
                    // 属性读失败：保留旧值继续用（分层的好处之一——
                    // 一层失败不至于把整份快照丢掉），下一轮慢层会再试
                    crate::log_debug!("读取媒体属性失败：{e}");
                }
            }

            // 播控标志挂在 PlaybackInfo 上：本轮快层读过就复用，没读过才补一次
            if playback_info.is_none() {
                playback_info = session.GetPlaybackInfo().ok();
            }
            let ((can_prev, can_next), ms) = timed("smtc.controls", || {
                let controls = playback_info.as_ref().and_then(|p| p.Controls().ok());
                let prev = controls
                    .as_ref()
                    .and_then(|c| c.IsPreviousEnabled().ok())
                    .unwrap_or(false);
                let next = controls
                    .as_ref()
                    .and_then(|c| c.IsNextEnabled().ok())
                    .unwrap_or(false);
                (prev, next)
            });
            self.stats.observe(ms);
            self.parts.can_prev = can_prev;
            self.parts.can_next = can_next;
            self.cadence.note_props(now);
        }

        // 属性一次都没成功读过 → 组不出有意义的快照（与旧行为一致）
        if !self.parts.props_ok {
            return;
        }

        // ---- 锚点（见 anchor_of 的三规则）----
        let track_id = parts_track_id(&self.parts);
        let track_changed = self.last.as_ref().map(|p| p.track_id) != Some(track_id);
        let anchor = anchor_of(
            read_timeline_now,
            self.parts.timeline_at,
            track_changed,
            now,
            self.last.as_ref().map(|p| p.position_at),
        );

        let snapshot = self.parts.snapshot(anchor);
        if let Some(prev) = self.last.as_ref() {
            if !has_changed(prev, &snapshot) {
                return;
            }
        }

        let (title, artist, duration) = match snapshot.track.as_ref() {
            Some(t) => (t.title.as_str(), t.artist.as_str(), t.duration_ms),
            None => ("", "", 0),
        };
        crate::log_info!(
            "session changed: aumid={} app={} title={} artist={} duration={} state={} position={}",
            snapshot.session.aumid,
            snapshot.session.app_name,
            title,
            artist,
            duration,
            snapshot.state.as_str(),
            snapshot.position_ms
        );

        self.last = Some(snapshot.clone());
        sink.send(AppEvent::NowPlaying(Box::new(snapshot)));
    }
}

/// 跑一次跨进程读，返回 `(结果, 耗时毫秒)`。
///
/// 慢于 [`perf::SMTC_SLOW_MS`]（8ms）由 `Span` 自己记日志；耗时同时进
/// 每分钟统计的「最慢」字段。快路径（正常 1~3ms）只有一次 `Instant::now` 对比。
fn timed<T>(name: &'static str, f: impl FnOnce() -> T) -> (T, u128) {
    let sp = Span::with_threshold(name, perf::SMTC_SLOW_MS);
    let value = f();
    let ms = sp.elapsed_ms();
    sp.finish();
    (value, ms)
}

// ---------------------------------------------------------------- 线程主体

/// 一次「等命令」的结果（v1.5.5 / G3.5：抽成纯通道语义，便于单测）。
///
/// `Timeout` 只在**非冻结**态出现（50ms 轮询节拍）；paused 态是阻塞 `recv()`，
/// 永远不会产生 Timeout——这正是「0 唤醒」的含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecvOutcome {
    Command(MediaCommand),
    Timeout,
    Disconnected,
}

/// 取一条命令：冻结（paused）时**阻塞** `recv()`，非冻结时 50ms `recv_timeout()`。
///
/// 这条分叉是 G3.5 的核心：冻结期间媒体线程完全不醒来、不发任何跨进程调用，
/// 唯一的唤醒来源是命令通道——`Quit`（退出）、播控命令（热键切歌）、
/// `SetPaused(false)`（解冻）。`Quit` 因此在 paused 态天然可投递，不会死锁。
fn recv_command(rx: &Receiver<MediaCommand>, paused: bool) -> RecvOutcome {
    if paused {
        match rx.recv() {
            Ok(cmd) => RecvOutcome::Command(cmd),
            Err(_) => RecvOutcome::Disconnected,
        }
    } else {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(cmd) => RecvOutcome::Command(cmd),
            Err(RecvTimeoutError::Timeout) => RecvOutcome::Timeout,
            Err(RecvTimeoutError::Disconnected) => RecvOutcome::Disconnected,
        }
    }
}

/// 媒体线程主函数。**整个函数体都在 MTA 上运行**，因此可以随意 `join()`。
pub fn run(sink: EventSink<AppEvent>, cmd_rx: Receiver<MediaCommand>) {
    let _com = ComApartment::mta();
    crate::log_info!("媒体线程启动（MTA）");

    let Some(mgr) = acquire_manager() else {
        crate::log_err!(
            "SMTC manager 连续 {MANAGER_RETRIES} 次获取失败，媒体功能降级（小组件仍可用）"
        );
        drain_until_quit(&cmd_rx);
        crate::log_info!("媒体线程退出");
        return;
    };

    // 事件回调只做两件事：置脏位 + PostMessage 唤醒 UI。绝不在回调里做重活。
    let dirty = Arc::new(AtomicBool::new(true));
    let mut state = PollState::new(dirty.clone());
    let _handlers = subscribe(&mgr, &dirty, &sink);

    // v1.5.5 / G3.5：全屏冻结时整个轮询停摆，只留命令通道（阻塞 recv）。
    // 分层间隔 500 / 1000 / 2000ms 与 FORCED_FLOOR_MS 一律不动，只整体跳过。
    let mut paused = false;
    loop {
        match recv_command(&cmd_rx, paused) {
            RecvOutcome::Command(MediaCommand::Quit) => break,
            RecvOutcome::Command(MediaCommand::SetPaused(true)) => {
                if !paused {
                    crate::log_info!("媒体线程：全屏冻结，暂停分层轮询（仅等待命令）");
                }
                paused = true;
                // 冻结期间完全跳过 schedule / tick
                continue;
            }
            RecvOutcome::Command(MediaCommand::SetPaused(false)) => {
                paused = false;
                // 解冻后第一轮必须全量读：三层节奏时间戳归零 + 置脏位 +
                // 让 forced 的 60ms 下限立即成立（见 `PollState::reset_for_resume`）。
                state.reset_for_resume(Instant::now());
                crate::log_info!("媒体线程：解冻，恢复分层轮询（第一轮全量读）");
                continue;
            }
            RecvOutcome::Command(cmd) => {
                // 播控命令在冻结期间照常执行（用户在全屏游戏里按热键切歌）。
                // 只有「轮询」被暂停，命令通路不冻结；执行完立即回到阻塞 recv。
                execute(cmd, &mgr, &sink, &mut state);
                if paused {
                    continue;
                }
            }
            RecvOutcome::Timeout => {
                // 非冻结态：50ms 到点，进入本轮分层轮询
            }
            RecvOutcome::Disconnected => {
                crate::log_warn!("命令通道已断开，媒体线程退出");
                break;
            }
        }

        // 分层降频（G2 修复版）：每组调用各看各的节奏，一组都不到点就整轮跳过
        // （连 GetCurrentSession 都不发）。事件驱动（G2 二轮）只是把脏位置上，
        // 节奏判定统一走 `schedule`，轮询永远作兜底。
        let now = Instant::now();
        let (plan, forced) = state.schedule(now, state.dirty.load(Ordering::Relaxed));
        if forced {
            // 消费脏位（schedule 判定过 60ms 下限才到这里）
            state.dirty.store(false, Ordering::Relaxed);
        }
        if plan.any() {
            state.tick(&mgr, &sink, now, plan);
        }
    }

    crate::log_info!("媒体线程退出");
}

/// 启动期拿 manager：最多 5 次指数退避。
fn acquire_manager() -> Option<Manager> {
    for attempt in 0..MANAGER_RETRIES {
        let r = timed_join("RequestAsync", || Manager::RequestAsync()?.join());
        match r {
            Ok(m) => {
                crate::log_info!("SMTC manager 获取成功（第 {} 次尝试）", attempt + 1);
                return Some(m);
            }
            Err(e) => {
                let wait = backoff_ms(attempt);
                crate::log_warn!(
                    "SMTC manager 获取失败（第 {}/{} 次）：{e}，{}ms 后重试",
                    attempt + 1,
                    MANAGER_RETRIES,
                    wait
                );
                std::thread::sleep(Duration::from_millis(wait));
            }
        }
    }
    None
}

/// 事件订阅句柄的保活容器。
///
/// **为什么必须保活**：`TypedEventHandler` 是 COM 对象，一旦我们 drop 掉唯一引用，
/// 事件源虽然仍持有引用（回调依然会触发），但 token 失效后无法反注册，
/// 且语义上容易误判。显式持有到线程退出最省心。
struct Subscriptions {
    _current: TypedEventHandler<Manager, CurrentSessionChangedEventArgs>,
    _sessions: TypedEventHandler<Manager, SessionsChangedEventArgs>,
}

fn subscribe(
    mgr: &Manager,
    dirty: &Arc<AtomicBool>,
    sink: &EventSink<AppEvent>,
) -> Subscriptions {
    let d1 = dirty.clone();
    let s1 = sink.clone();
    let current = TypedEventHandler::new(move |_sender, _args| {
        d1.store(true, Ordering::Relaxed);
        s1.poke();
        Ok(())
    });

    let d2 = dirty.clone();
    let s2 = sink.clone();
    let sessions = TypedEventHandler::new(move |_sender, _args| {
        d2.store(true, Ordering::Relaxed);
        s2.poke();
        Ok(())
    });

    match mgr.CurrentSessionChanged(&current) {
        Ok(token) => crate::log_debug!("已订阅 CurrentSessionChanged（token={token}）"),
        Err(e) => crate::log_warn!("订阅 CurrentSessionChanged 失败：{e}"),
    }
    match mgr.SessionsChanged(&sessions) {
        Ok(token) => crate::log_debug!("已订阅 SessionsChanged（token={token}）"),
        Err(e) => crate::log_warn!("订阅 SessionsChanged 失败：{e}"),
    }

    Subscriptions {
        _current: current,
        _sessions: sessions,
    }
}

// ---------------------------------------------------------------- 会话级事件（G2 二轮）

/// 会话级事件订阅（切歌 / 状态跃迁 / 时间线变化的**主通道**）。
///
/// 与管理器级订阅的差别：它挂在**具体会话对象**上，所以必须跟着 AUMID 走——
/// 换播放器就重挂；token 连同会话对象一起保存在这里，[`Drop::drop`] 时
/// `Remove*`（媒体线程退出、会话清空都会走到；Drop 时 COM 还活着，
/// 见 `run` 的构造顺序：`_com` 最先创建、最后销毁）。
///
/// 注意 windows-rs 0.62 的命名：args 类型不带会话前缀（`MediaPropertiesChangedEventArgs`），
/// 反注册方法是 **PascalCase** 的 `Remove*`（与事件订阅的 `MediaPropertiesChanged` 不同）。
struct SessionSubscriptions {
    /// 会话对象：`Remove*` 需要它，不能只存 token
    session: GlobalSystemMediaTransportControlsSession,
    /// 订阅时的 AUMID（变了才重挂）
    aumid: String,
    props: TypedEventHandler<GlobalSystemMediaTransportControlsSession, MediaPropertiesChangedEventArgs>,
    playback: TypedEventHandler<GlobalSystemMediaTransportControlsSession, PlaybackInfoChangedEventArgs>,
    timeline: TypedEventHandler<
        GlobalSystemMediaTransportControlsSession,
        TimelinePropertiesChangedEventArgs,
    >,
    props_token: i64,
    playback_token: i64,
    timeline_token: i64,
}

impl Drop for SessionSubscriptions {
    fn drop(&mut self) {
        // token 无效时返回 Err，忽略即可（不 panic；Drop 里也不许 panic）。
        let _ = self.session.RemoveMediaPropertiesChanged(self.props_token);
        let _ = self.session.RemovePlaybackInfoChanged(self.playback_token);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.timeline_token);
        crate::log_debug!("会话级事件订阅已移除（aumid={}）", self.aumid);
    }
}

/// 给一个会话挂上三个事件。**回调里只许置脏位 + 唤醒**——
/// 它们跑在 WinRT 线程池上，做 COM 重活会把线程池拖垮（风险与收益完全不对等）。
fn subscribe_session(
    session: &GlobalSystemMediaTransportControlsSession,
    dirty: &Arc<AtomicBool>,
    sink: &EventSink<AppEvent>,
    aumid: String,
) -> SessionSubscriptions {
    let d1 = dirty.clone();
    let s1 = sink.clone();
    let props = TypedEventHandler::new(move |_sender, _args| {
        d1.store(true, Ordering::Relaxed);
        s1.poke();
        Ok(())
    });

    let d2 = dirty.clone();
    let s2 = sink.clone();
    let playback = TypedEventHandler::new(move |_sender, _args| {
        d2.store(true, Ordering::Relaxed);
        s2.poke();
        Ok(())
    });

    let d3 = dirty.clone();
    let s3 = sink.clone();
    let timeline = TypedEventHandler::new(move |_sender, _args| {
        d3.store(true, Ordering::Relaxed);
        s3.poke();
        Ok(())
    });

    // 订阅失败只记日志（token=0），轮询兜底仍然完整可用
    let props_token = session.MediaPropertiesChanged(&props).unwrap_or(0);
    let playback_token = session.PlaybackInfoChanged(&playback).unwrap_or(0);
    let timeline_token = session.TimelinePropertiesChanged(&timeline).unwrap_or(0);
    crate::log_info!(
        "已订阅会话级事件（aumid={}）：MediaPropertiesChanged / PlaybackInfoChanged / TimelinePropertiesChanged",
        aumid
    );
    SessionSubscriptions {
        session: session.clone(),
        aumid,
        props,
        playback,
        timeline,
        props_token,
        playback_token,
        timeline_token,
    }
}

/// 从 `PlaybackInfo` 推播放状态。拿不到时按「播放中」处理，避免误隐藏。
fn playback_state_of(
    info: Option<&GlobalSystemMediaTransportControlsSessionPlaybackInfo>,
) -> PlaybackState {
    match info.and_then(|p| p.PlaybackStatus().ok()) {
        Some(s) if s == PlaybackStatus::Playing => PlaybackState::Playing,
        // Paused / Opened / Changing / Stopped / Closed 一律视为「暂停」：
        // 小组件保持显示、进度与扫光静止（PRD P0-02）。
        Some(_) => PlaybackState::Paused,
        None => PlaybackState::Playing,
    }
}

/// `TimeSpan.Duration` 是 100ns 单位 → 毫秒要 `/ 10_000`。
fn read_timeline(session: &GlobalSystemMediaTransportControlsSession) -> (Ms, Ms) {
    let Ok(t) = session.GetTimelineProperties() else {
        return (0, 0);
    };
    let start = t.StartTime().map(|ts| ts.Duration).unwrap_or(0);
    let end = t.EndTime().map(|ts| ts.Duration).unwrap_or(0);
    let pos = t.Position().map(|ts| ts.Duration).unwrap_or(0);
    const TICKS_PER_MS: i64 = 10_000;
    let position = ((pos - start) / TICKS_PER_MS).max(0);
    let duration = ((end - start) / TICKS_PER_MS).max(0);
    (position, duration)
}

/// 判断快照是否「值得发事件」。
///
/// **刻意不把 `position_ms` 直接纳入 diff**：播放中位置必然前进，
/// 那样会变成每秒的无意义重绘（T05 验收明确要求「空闲 5 分钟日志不再刷屏」）。
/// UI 侧靠 `position_at` 线性外推（`NowPlaying::position_now`）拿实时进度。
///
/// seek 检测用**非对称窗口**，而不是「外推值 vs 上报值」：
/// 有些播放器（实测 Edge 播 B 站视频）会把 SMTC 的 position 长时间停在同一个值，
/// 此时「外推值 - 上报值」会随 elapsed 线性增大，用对称阈值会每 2 秒误报一次。
/// 改成比较「上报位置的增量 delta」与「两次快照的时间差 elapsed」：
/// - `delta < -1000`：位置倒退 → 后退 seek
/// - `delta > elapsed + 3000`：位置跳得比时间还快 → 前进 seek
/// - 其余（包括 delta == 0 的陈旧上报、delta ≈ elapsed 的正常推进）→ 不算变化
fn has_changed(prev: &NowPlaying, cur: &NowPlaying) -> bool {
    if prev.state != cur.state
        || prev.track_id != cur.track_id
        || prev.session.aumid != cur.session.aumid
    {
        return true;
    }
    if prev.can_prev != cur.can_prev
        || prev.can_next != cur.can_next
        || prev.can_seek != cur.can_seek
    {
        return true;
    }
    let delta = cur.position_ms - prev.position_ms;
    let elapsed = prev.position_at.elapsed().as_millis() as Ms;
    delta < -1_000 || delta > elapsed + 3_000
}

/// 执行一条播控命令。全部 `.join()` 都在 MTA 上，安全。
///
/// 无论成功失败都会回一条 `AppEvent::CommandResult`，UI 侧据此决定
/// 「保留乐观更新」还是「回滚」（PRD P0-04）。
fn execute(cmd: MediaCommand, mgr: &Manager, sink: &EventSink<AppEvent>, state: &mut PollState) {
    match cmd {
        MediaCommand::Quit => return,
        MediaCommand::Refresh => {
            // 强制重读一次（乐观更新回滚校验 / v1.5.5 解冻）。
            // 不在 `execute` 里直接 `tick`，而是置「立即全量读」状态，交给本轮
            // 末尾的 `schedule` 统一执行：解冻时 UI 连发 `SetPaused(false) + Refresh`
            // 也只会读一次；冻结期间收到它则自然推迟到解冻后（0 跨进程调用）。
            state.reset_for_resume(Instant::now());
            return;
        }
        MediaCommand::Mute => {
            crate::log_debug!("Mute 由 WASAPI 通道实现（T16），当前忽略");
            return;
        }
        // `SetPaused` 由 `run` 在命令入口截获，绝不该走到这里；
        // 显式列出是防御未来有人在别处调用 `execute`。
        MediaCommand::SetPaused(_) => return,
        _ => {}
    }

    let kind = match cmd {
        MediaCommand::PlayPause => MediaCommandKind::PlayPause,
        MediaCommand::Next => MediaCommandKind::Next,
        MediaCommand::Prev => MediaCommandKind::Prev,
        MediaCommand::Seek(_) => MediaCommandKind::Seek,
        MediaCommand::Refresh | MediaCommand::Mute | MediaCommand::Quit => return,
        MediaCommand::SetPaused(_) => return,
    };

    let session = match mgr.GetCurrentSession() {
        Ok(s) => s,
        Err(_) => {
            crate::log_warn!("无活动会话，命令 {cmd:?} 丢弃");
            sink.send(AppEvent::CommandResult { kind, ok: false });
            return;
        }
    };

    let result: Result<bool> = match cmd {
        MediaCommand::PlayPause => timed_join("TryTogglePlayPauseAsync", || {
            session.TryTogglePlayPauseAsync()?.join()
        }),
        MediaCommand::Next => {
            timed_join("TrySkipNextAsync", || session.TrySkipNextAsync()?.join())
        }
        MediaCommand::Prev => timed_join("TrySkipPreviousAsync", || {
            session.TrySkipPreviousAsync()?.join()
        }),
        MediaCommand::Seek(ms) => timed_join("TryChangePlaybackPositionAsync", || {
            // 毫秒 → 100ns
            session
                .TryChangePlaybackPositionAsync(ms.saturating_mul(10_000))?
                .join()
        }),
        MediaCommand::Refresh | MediaCommand::Mute | MediaCommand::Quit => return,
        MediaCommand::SetPaused(_) => return,
    };

    match result {
        Ok(accepted) => {
            if accepted {
                crate::log_info!("命令 {cmd:?} 已下发，播放器接受");
            } else {
                crate::log_warn!("命令 {cmd:?} 被播放器拒绝");
            }
            sink.send(AppEvent::CommandResult {
                kind,
                ok: accepted,
            });
        }
        Err(e) => {
            crate::log_warn!("命令 {cmd:?} 执行失败：{e}");
            sink.send(AppEvent::CommandResult { kind, ok: false });
        }
    }
}

/// manager 不可用时的降级循环：只消费命令，保证发送端不会因为通道满而阻塞。
fn drain_until_quit(cmd_rx: &Receiver<MediaCommand>) {
    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(MediaCommand::Quit) => return,
            Ok(cmd) => crate::log_debug!("媒体降级态，忽略命令 {cmd:?}"),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// FNV-1a 64 位。用于把曲目 key 映射成稳定的 `TrackId`。
pub fn fnv1a64(bytes: &[u8]) -> TrackId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(title: &str, state: PlaybackState, position: Ms) -> NowPlaying {
        let track = Track {
            title: String::from(title),
            artist: String::from("a"),
            album: String::new(),
            duration_ms: 180_000,
        };
        NowPlaying {
            track_id: fnv1a64(track.key().as_bytes()),
            track: Some(track),
            session: MediaSessionInfo {
                aumid: String::from("CloudMusic.exe"),
                ..Default::default()
            },
            state,
            position_ms: position,
            ..Default::default()
        }
    }

    #[test]
    fn fnv_is_stable_and_distinct() {
        assert_eq!(fnv1a64(b"abc"), fnv1a64(b"abc"));
        assert_ne!(fnv1a64(b"abc"), fnv1a64(b"abd"));
    }

    #[test]
    fn identical_snapshots_do_not_trigger_event() {
        let a = snapshot("t", PlaybackState::Paused, 1000);
        let b = snapshot("t", PlaybackState::Paused, 1000);
        assert!(!has_changed(&a, &b));
    }

    #[test]
    fn track_change_triggers_event() {
        let a = snapshot("t1", PlaybackState::Playing, 0);
        let b = snapshot("t2", PlaybackState::Playing, 0);
        assert!(has_changed(&a, &b));
    }

    #[test]
    fn state_change_triggers_event() {
        let a = snapshot("t", PlaybackState::Playing, 0);
        let b = snapshot("t", PlaybackState::Paused, 0);
        assert!(has_changed(&a, &b));
    }

    #[test]
    fn small_position_drift_does_not_trigger_event() {
        // 暂停态不外推，位置差 300ms → 不该发事件
        let a = snapshot("t", PlaybackState::Paused, 1000);
        let b = snapshot("t", PlaybackState::Paused, 1300);
        assert!(!has_changed(&a, &b));
    }

    #[test]
    fn seek_triggers_event() {
        // 暂停态下跳到 30s 处，偏差远超 2s 阈值
        let a = snapshot("t", PlaybackState::Paused, 1000);
        let b = snapshot("t", PlaybackState::Paused, 30_000);
        assert!(has_changed(&a, &b));
    }

    // ------------------------------------------------ 分层节奏（G2 修复版）

    /// `Instant` 的相对工具：全部由参数注入，**不 sleep**。
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// 三层各走各的节奏；499ms 内一层都不该动。
    #[test]
    fn tiers_fire_at_their_own_cadence() {
        let mut c = Cadence::default();
        let t0 = Instant::now();

        // 首轮：三组全读（都从没读过）
        let p = c.plan(t0, false);
        assert!(p.playback && p.timeline && p.props, "首轮必须全量");
        c.note_playback(t0, PlaybackState::Playing);
        c.note_timeline(t0, 1_000, 180_000);
        c.note_props(t0);

        let p = c.plan(at(t0, 499), false);
        assert!(
            !p.playback && !p.timeline && !p.props,
            "499ms 内不许有任何跨进程调用"
        );
        let p = c.plan(at(t0, 500), false);
        assert!(p.playback && !p.timeline && !p.props, "500ms 只该读快层");
        let p = c.plan(at(t0, 1_000), false);
        assert!(p.playback && p.timeline && !p.props, "1s 读快+中");
        let p = c.plan(at(t0, 2_000), false);
        assert!(p.playback && p.timeline && p.props, "2s 三层全到点");
    }

    /// `forced`（事件脏位 / UI 强制刷新）绕过所有节奏。
    #[test]
    fn forced_plan_reads_everything() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_playback(t0, PlaybackState::Playing);
        c.note_timeline(t0, 1, 180_000);
        c.note_props(t0);
        let p = c.plan(at(t0, 10), true);
        assert!(p.playback && p.timeline && p.props);
    }

    /// 需求①：连续 3 次 (0,0) → 退避 5s；出现非零值 → 回 1s。
    #[test]
    fn timeline_backs_off_after_three_zero_reads() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_playback(t0, PlaybackState::Playing);
        c.note_props(t0);

        c.note_timeline(t0, 0, 0);
        assert!(!c.timeline_backoff, "第 1 次不许退避");
        c.note_timeline(at(t0, 1_000), 0, 0);
        assert!(!c.timeline_backoff, "第 2 次还不许退避");
        c.note_timeline(at(t0, 2_000), 0, 0);
        assert!(c.timeline_backoff, "连续 3 次必须退避");

        // 退避期：last_timeline = t0+2000，下一次到点在 t0+7000
        let p = c.plan(at(t0, 6_999), false);
        assert!(p.playback && !p.timeline, "退避期 1s 不该到点");
        let p = c.plan(at(t0, 7_000), false);
        assert!(p.timeline, "退避期满 5s 该读了");

        // 出现非零值 → 立即恢复 1s
        c.note_timeline(at(t0, 7_000), 5_000, 180_000);
        assert!(!c.timeline_backoff);
        let p = c.plan(at(t0, 8_000), false);
        assert!(p.timeline, "恢复后 1s 就该到点");
    }

    /// 需求①（后半）：状态跃迁也要恢复时间线轮询。
    #[test]
    fn playback_jump_resets_timeline_backoff() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        // 先建立状态基线：跃迁是「边沿触发」，没有上一拍就谈不上跃迁
        c.note_playback(t0, PlaybackState::Playing);
        for i in 0..3 {
            c.note_timeline(at(t0, i * 1_000), 0, 0);
        }
        assert!(c.timeline_backoff);
        c.note_playback(at(t0, 3_000), PlaybackState::Paused);
        assert!(!c.timeline_backoff, "状态跃迁必须清退避");
        assert!(
            c.plan(at(t0, 3_000), false).timeline,
            "跃迁当轮就该补读时间线"
        );
    }

    /// 需求④：状态跃迁只报一次（同状态不算）。
    #[test]
    fn playback_jump_is_edge_triggered() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_playback(t0, PlaybackState::Playing);
        assert!(!c.note_playback(at(t0, 500), PlaybackState::Playing));
        assert!(c.note_playback(at(t0, 1_000), PlaybackState::Paused));
        assert!(!c.note_playback(at(t0, 1_500), PlaybackState::Paused));
    }

    /// 需求②：位置回退 >1500ms → 立即补读属性（切歌 / 重播）。
    #[test]
    fn position_regression_beyond_1500ms_triggers_refill() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_timeline(t0, 60_000, 180_000);
        // 恰好倒退 1500ms → 不算（阈值是「>」）
        assert!(!c.note_timeline(at(t0, 1_000), 58_500, 180_000));
        // 倒退 1501ms → 算
        assert!(c.note_timeline(at(t0, 2_000), 1_000, 180_000));
        // 正常前进再多也不算
        assert!(!c.note_timeline(at(t0, 3_000), 2_000, 180_000));
    }

    /// 需求④：时长变化也算「切歌信号」；位置前进不算。
    #[test]
    fn duration_change_triggers_refill() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_timeline(t0, 1_000, 180_000);
        assert!(!c.note_timeline(at(t0, 1_000), 2_000, 180_000), "前进不算");
        assert!(c.note_timeline(at(t0, 2_000), 2_000, 90_000), "时长变了算");
    }

    /// 网易云恒 (0,0) 不许被误判成「切歌」（否则每 2s 白读一次属性）。
    #[test]
    fn zero_streak_player_does_not_look_like_a_track_change() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_timeline(t0, 0, 0);
        assert!(!c.note_timeline(at(t0, 1_000), 0, 0));
        assert!(!c.note_timeline(at(t0, 2_000), 0, 0));
    }

    /// 需求③的另一半：统计窗口到点才清零。
    #[test]
    fn stats_window_flushes_and_resets() {
        let t0 = Instant::now();
        let mut s = PollStats::new(t0);
        s.playback = 120;
        s.props = 30;
        s.timeline = 60;
        s.observe(42);
        assert!(!s.flush_if_due(at(t0, 59_999)), "没到点不许清零");
        assert_eq!(s.playback, 120);
        assert!(s.flush_if_due(at(t0, 60_000)), "满 60s 必须记一条");
        assert_eq!(s.playback, 0, "到点必须清零重新统计");
        assert_eq!(s.max_ms, 0);
    }

    /// 快照拼装：时长只来自时间线层，`track_id` 公式与旧实现一致。
    #[test]
    fn snapshot_takes_duration_from_timeline_layer_only() {
        let mut parts = SessionParts::default();
        parts.track = Track {
            title: "晴天".into(),
            artist: "周杰伦".into(),
            album: String::new(),
            duration_ms: 0, // 属性层永远填 0
        };
        parts.duration_ms = 269_000; // 时间线层读到的
        let t = effective_track(&parts);
        assert_eq!(t.duration_ms, 269_000);
        let expect = fnv1a64(t.key().as_bytes());
        assert_eq!(parts_track_id(&parts), expect);

        // 时长没读到（0）时 track_id 用含 0 时长的 key —— 与旧实现逐字节一致
        parts.duration_ms = 0;
        let t0 = effective_track(&parts);
        assert_eq!(parts_track_id(&parts), fnv1a64(t0.key().as_bytes()));
    }

    /// 时长为 0 → `can_seek` 必须是 false（风险 R9 的代理判据不许变）。
    #[test]
    fn snapshot_seek_flag_follows_duration() {
        let mut parts = SessionParts::default();
        parts.track = Track {
            title: "t".into(),
            ..Default::default()
        };
        parts.props_ok = true;
        // 锚点传「发事件时刻」：这条测试只关心 can_seek，不关心外推基准
        let now = Instant::now();
        assert!(!parts.snapshot(now).can_seek);
        parts.duration_ms = 180_000;
        assert!(parts.snapshot(now).can_seek);
    }

    // ------------------------------------------------ 锚点 + 事件驱动（G2 二轮）

    /// 规则 1：本轮读过时间线 → 锚点 = 该次读取时刻。
    #[test]
    fn anchor_rule_1_timeline_read_this_tick() {
        let t0 = Instant::now();
        let tl_at = at(t0, 100);
        let now = at(t0, 150);
        let a = anchor_of(true, Some(tl_at), false, now, Some(at(t0, 50)));
        assert_eq!(a, tl_at, "锚点必须是时间线读取时刻，不是发事件的时刻");
        // timeline_at 缺席（理论不可达）时退到 now，不许 panic
        assert_eq!(anchor_of(true, None, false, now, Some(at(t0, 50))), now);
    }

    /// 规则 2（核心回归）：`(0,0)` 播放器在 5s 退避下切歌 ——
    /// 本轮没读时间线，锚点必须是「检测时刻」，绝不许退回陈旧的 timeline_at。
    #[test]
    fn anchor_rule_2_track_change_without_timeline_uses_detection_moment() {
        let t0 = Instant::now();
        // 时间线上次读取在 5s 前（网易云退避期），此刻慢层发现切歌
        let stale_timeline_at = Some(t0);
        let detection = at(t0, 5_000);
        let a = anchor_of(false, stale_timeline_at, true, detection, Some(at(t0, 4_000)));
        assert_eq!(
            a, detection,
            "退回 timeline_at = 歌词钟快 5s（v1.4.2 的漂移 bug）"
        );
    }

    /// 规则 3：没读时间线、也没切歌（普通事件，如播控标志变化）→
    /// 锚点沿用上一份快照，位置与基准必须同进退。
    #[test]
    fn anchor_rule_3_plain_event_keeps_previous_anchor() {
        let t0 = Instant::now();
        let prev_anchor = at(t0, 500);
        let now = at(t0, 3_000);
        let a = anchor_of(false, Some(at(t0, 500)), false, now, Some(prev_anchor));
        assert_eq!(a, prev_anchor, "锚点漂了，位置与基准就错位了");
        // 首份快照没有上一份 → 用 now（此时 props_ok 门刚开，必然刚读过）
        assert_eq!(anchor_of(false, Some(at(t0, 500)), false, now, None), now);
    }

    /// 锚点规则与节奏状态机的**串联**：`(0,0)` 播放器在退避期切歌，
    /// cadence 判定「本轮不读时间线」+ anchor 判定「用检测时刻」——
    /// 两个判定合起来才是 v1.4.2 漂移 bug 的完整复现。
    #[test]
    fn zero_player_backoff_track_change_gets_detection_anchor() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_playback(t0, PlaybackState::Playing);
        c.note_props(t0);
        for i in 0..3 {
            c.note_timeline(at(t0, i * 1_000), 0, 0);
        }
        assert!(c.timeline_backoff, "前置：网易云已进入退避");

        // 退避期内（last_timeline = t0+2000，下次到点 t0+7000）慢层发现切歌
        let detection = at(t0, 3_000);
        let p = c.plan(detection, false);
        assert!(!p.timeline, "退避期本轮不读时间线");

        let anchor = anchor_of(
            p.timeline,
            Some(at(t0, 2_000)), // 退避前最后读时间线的时刻
            true,                // 慢层发现 track_id 变了
            detection,
            Some(at(t0, 2_000)),
        );
        assert_eq!(
            anchor, detection,
            "锚点必须是检测时刻：网易云的歌词钟从这里起走表"
        );
    }

    /// 需求③：主循环每轮都调 `schedule` → 统计窗口到点**必须**落地。
    /// 这条测的是「接线」本身：`schedule` 里包含 `flush_if_due`。
    #[test]
    fn loop_schedule_flushes_stats_and_maps_dirty_to_forced() {
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)));
        let t0 = st.last_tick;
        // 先「演」一轮全量读（真实主循环首轮就是全量）：三层计时起点都落在 t0
        st.cadence.note_playback(t0, PlaybackState::Playing);
        st.cadence.note_timeline(t0, 1_000, 180_000);
        st.cadence.note_props(t0);

        // 脏位 + 60ms 未到 → 不消费、不下全量（防事件风暴）
        let (plan, forced) = st.schedule(at(t0, 10), true);
        assert!(!forced, "60ms 内不许因事件全量读");
        assert!(!plan.any(), "三层都没到点就该整轮跳过（连 GetCurrentSession 都不发）");

        // 60ms 到 → forced 全量读
        let (plan, forced) = st.schedule(at(t0, 60), true);
        assert!(forced, "过 60ms 下限的事件必须映射成 forced");
        assert!(plan.playback && plan.timeline && plan.props);

        // 无脏位的普通轮次：走三层节奏，不触发 forced
        let (_, forced) = st.schedule(at(t0, 100), false);
        assert!(!forced);

        // 统计接线：60s 后的普通轮次必须把窗口清零（flush 在 schedule 里）
        st.stats.playback = 7;
        st.stats.timeline = 3;
        st.stats.props = 2;
        let _ = st.schedule(at(t0, 60_000), false);
        assert_eq!(st.stats.playback, 0, "主循环必须每轮落地统计（flush_if_due）");
        assert_eq!(st.stats.timeline, 0);
        assert_eq!(st.stats.props, 0);
    }

    // ------------------------------------------- v1.5.5 / G3.5：全屏冻结（paused）

    /// paused 态用阻塞 `recv()`：通道开着、命令还没到时必须一直等，而不是
    /// 每 50ms 醒一次。**同时证明 `Quit` 在 paused 语义下仍可投递**（退出不死锁）。
    #[test]
    fn paused_recv_waits_for_command_and_quit_is_deliverable() {
        let (tx, rx) = std::sync::mpsc::channel();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            tx.send(MediaCommand::SetPaused(true)).unwrap();
            tx.send(MediaCommand::Quit).unwrap();
        });
        assert_eq!(
            recv_command(&rx, true),
            RecvOutcome::Command(MediaCommand::SetPaused(true)),
            "paused 必须阻塞到命令到达，不许返回 Timeout"
        );
        assert_eq!(
            recv_command(&rx, true),
            RecvOutcome::Command(MediaCommand::Quit),
            "Quit 必须能在 paused 态收到，否则 shutdown 会挂住"
        );
        sender.join().unwrap();
    }

    /// 非 paused 用 `recv_timeout`：空闲时返回 Timeout，主循环据此推进分层轮询。
    #[test]
    fn running_recv_uses_timeout_when_idle() {
        // `_tx` 必须活着，否则会直接 Disconnected
        let (_tx, rx) = std::sync::mpsc::channel::<MediaCommand>();
        assert_eq!(recv_command(&rx, false), RecvOutcome::Timeout);
    }

    /// paused 态**不可能**产生 Timeout（0 唤醒）；通道断开则两种态都判 Disconnected。
    #[test]
    fn paused_recv_never_times_out_and_detects_disconnect() {
        let (tx, rx) = std::sync::mpsc::channel::<MediaCommand>();
        drop(tx);
        assert_eq!(recv_command(&rx, true), RecvOutcome::Disconnected);
        assert_eq!(recv_command(&rx, false), RecvOutcome::Disconnected);
    }

    /// 解冻命令序列 `SetPaused(false) + Refresh` 按 FIFO 到达，paused 的 recv 接得住。
    #[test]
    fn resume_command_sequence_is_delivered_in_order() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(MediaCommand::SetPaused(true)).unwrap();
        tx.send(MediaCommand::SetPaused(false)).unwrap();
        tx.send(MediaCommand::Refresh).unwrap();
        assert_eq!(
            recv_command(&rx, true),
            RecvOutcome::Command(MediaCommand::SetPaused(true))
        );
        assert_eq!(
            recv_command(&rx, true),
            RecvOutcome::Command(MediaCommand::SetPaused(false))
        );
        assert_eq!(
            recv_command(&rx, false),
            RecvOutcome::Command(MediaCommand::Refresh)
        );
    }

    /// **解冻后第一轮就是全量读**的证据：冻结前三层都刚读过（常规节奏整轮跳过），
    /// `reset_for_resume` 之后第一次 `schedule` 必须 `forced` + `Plan::full()`。
    #[test]
    fn resume_forces_full_read_on_first_schedule() {
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)));
        let t0 = st.last_tick;
        st.cadence.note_playback(t0, PlaybackState::Playing);
        st.cadence.note_timeline(t0, 1_000, 180_000);
        st.cadence.note_props(t0);
        assert!(
            !st.cadence.plan(t0, false).any(),
            "前置：三层都刚读过，常规节奏这一轮一组都不该读"
        );

        st.reset_for_resume(t0);
        assert!(st.dirty.load(Ordering::Relaxed), "脏位必须置上");
        let (plan, forced) = st.schedule(t0, st.dirty.load(Ordering::Relaxed));
        assert!(forced, "解冻第一轮必须绕过 60ms 下限成为 forced");
        assert_eq!(
            plan,
            Plan::full(),
            "解冻第一轮必须三组全读，不许等 500/1000/2000ms"
        );
    }
}

// ============================================================================
// QA 独立回归验证（Edward / G3.5 v1.5.5）—— 只新增测试代码。
// ============================================================================
#[cfg(test)]
mod qa_g35_regression {
    use super::*;
    use std::sync::mpsc::channel;

    /// C1/C2：paused 必须是**阻塞 recv()**，不是 50ms 轮询。
    /// 工程师的用例只 sleep 30ms（< 50ms）——即使实现错用 recv_timeout(50ms)，
    /// 命令也已经到达，recv_timeout 会立刻返回 Ok，测试照样通过，**无法证伪**。
    /// 这里故意让命令 150ms 后才到：若实现仍是 50ms 超时，第一次必然返回 Timeout。
    #[test]
    fn qa_paused_recv_is_truly_blocking_not_50ms_polling() {
        let (tx, rx) = channel::<MediaCommand>();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            tx.send(MediaCommand::PlayPause).unwrap();
            tx.send(MediaCommand::Quit).unwrap();
        });

        let t0 = Instant::now();
        let first = recv_command(&rx, true);
        let elapsed = t0.elapsed();
        assert_eq!(
            first,
            RecvOutcome::Command(MediaCommand::PlayPause),
            "paused 期间不得产生 Timeout / 50ms 唤醒；实际拿到 {first:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(120),
            "阻塞仅 {elapsed:?}，说明存在 50ms 级唤醒，不是阻塞 recv()"
        );
        assert_eq!(
            recv_command(&rx, true),
            RecvOutcome::Command(MediaCommand::Quit),
            "Quit 在 paused 下必须可投递，否则 shutdown 死锁"
        );
        sender.join().unwrap();
    }

    /// C3：paused 下所有播控命令（含 Refresh / SetPaused / Quit）原样送达，
    /// 不产生 Timeout；通道断开后 paused 也必须识别 Disconnected。
    #[test]
    fn qa_all_commands_are_deliverable_while_paused_without_timeout() {
        let seq = [
            MediaCommand::PlayPause,
            MediaCommand::Next,
            MediaCommand::Prev,
            MediaCommand::Seek(42_000),
            MediaCommand::Mute,
            MediaCommand::Refresh,
            MediaCommand::SetPaused(true),
            MediaCommand::SetPaused(false),
            MediaCommand::Quit,
        ];
        let (tx, rx) = channel::<MediaCommand>();
        for cmd in seq {
            tx.send(cmd).unwrap();
        }
        drop(tx); // 队列读空后应得到 Disconnected，而不是 Timeout

        for cmd in seq {
            assert_eq!(
                recv_command(&rx, true),
                RecvOutcome::Command(cmd),
                "paused 下命令 {cmd:?} 必须原样送达"
            );
        }
        assert_eq!(
            recv_command(&rx, true),
            RecvOutcome::Disconnected,
            "通道断开在 paused 下也必须被识别（run() 据此退出）"
        );
    }

    /// 非冻结态仍走 50ms recv_timeout：空闲返回 Timeout，且不许被改快。
    #[test]
    fn qa_running_recv_keeps_50ms_cadence() {
        let (_tx, rx) = channel::<MediaCommand>();
        let t0 = Instant::now();
        assert_eq!(recv_command(&rx, false), RecvOutcome::Timeout);
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= Duration::from_millis(35),
            "非冻结态 50ms 节拍被改快：{elapsed:?}"
        );
    }

    /// C4：解冻后第一轮在任意时刻都必须是 (Plan::full(), true)；
    /// 连续两个解冻信号（SetPaused(false) + Refresh 都只置状态）只产生一次全量读。
    #[test]
    fn qa_reset_for_resume_forces_full_read_at_any_now_and_is_idempotent() {
        for step_ms in [0u64, 1, 60, 137, 2_000] {
            let mut st = PollState::new(Arc::new(AtomicBool::new(false)));
            let base = st.last_tick;
            // 前置：三层都「刚读过」，常规节奏这一轮一组都不该读
            st.cadence.note_playback(base, PlaybackState::Playing);
            st.cadence.note_timeline(base, 1_000, 180_000);
            st.cadence.note_props(base);
            assert!(!st.cadence.plan(base, false).any(), "前置：常规节奏整轮跳过");

            let now = base + Duration::from_millis(step_ms);
            st.reset_for_resume(now);
            st.reset_for_resume(now); // 模拟 UI 连发 SetPaused(false) + Refresh

            assert!(st.dirty.load(Ordering::Relaxed), "脏位必须置上");
            assert!(st.cadence.last_playback.is_none(), "三层节奏必须归零");
            assert!(st.cadence.last_timeline.is_none(), "三层节奏必须归零");
            assert!(st.cadence.last_props.is_none(), "三层节奏必须归零");

            let (plan, forced) = st.schedule(now, st.dirty.load(Ordering::Relaxed));
            assert!(forced, "step={step_ms}ms 解冻第一轮必须绕过 60ms 下限");
            assert_eq!(plan, Plan::full(), "step={step_ms}ms 解冻第一轮必须三组全读");

            // 消费脏位后同一轮内不会再出现第二次 forced 全量读
            st.dirty.store(false, Ordering::Relaxed);
            let (_, forced_again) = st.schedule(now, st.dirty.load(Ordering::Relaxed));
            assert!(!forced_again, "两个解冻信号只该产生一次全量读");
        }
    }

    /// C6：分层间隔与 60ms 下限一个都没被改（只在 paused 时整体跳过）。
    #[test]
    fn qa_polling_intervals_and_forced_floor_are_unchanged() {
        assert_eq!(limits::MEDIA_POLL_MS, 500);
        assert_eq!(limits::MEDIA_POLL_MID_MS, 1_000);
        assert_eq!(limits::MEDIA_POLL_SLOW_MS, 2_000);
        assert_eq!(limits::MEDIA_POLL_TIMELINE_BACKOFF_MS, 5_000);
        assert_eq!(limits::MEDIA_POLL_ZERO_STREAK, 3);
        assert_eq!(FORCED_FLOOR_MS, 60);
        assert_eq!(limits::BACKDROP_IDLE_MS, 3_000);
    }
}
