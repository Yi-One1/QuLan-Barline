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

use std::collections::HashSet;
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
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::core::Interface;

use crate::config::defaults::limits;
use crate::event::{AppEvent, MediaCommand, MediaCommandKind};
use crate::media::blacklist::{self, MediaIndex};
use crate::media::cover::load_cover;
use crate::media::player_map;
use crate::media::{
    CoverImage, IgnoredSessionInfo, MediaAppsSnapshot, MediaSessionInfo, MediaSource, Ms,
    NowPlaying, PlaybackState, Track, TrackId,
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

/// 忽略态下的**身份重查**下限（毫秒）（v1.9.0 / G7，ARCH-G7 §4.3）。
///
/// 被拉黑的应用通常还在放歌 → 会话级事件（MediaPropertiesChanged 等）持续置脏位 →
/// `forced` 每 60ms 就来一次。若每次都重读 AUMID，忽略态反而比正常态更费。
/// 1s 一次足够：解绑后 ≤1s 内发现，仍远优于验收要求的 3s。
/// G8-① 起它也兼作「命中黑名单后的会话枚举」下限：同一把低频闸，零定时器。
const IGNORE_RECHECK_MS: u64 = 1_000;

// ---------------------------------------------------------------- 会话挑选（G8-①）

/// 一次会话枚举里单个候选的**纯数据**画像（G8-① / 验收约束 6）。
///
/// 刻意不含任何 COM 句柄：媒体线程把 `GetSessions()` 的每个会话降维成它，
/// 再交给 [`choose_session`]。于是挑选规则可写确定性单测，无需启动媒体线程或 COM。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCandidate {
    /// SMTC 的 `SourceAppUserModelId`（身份）。
    pub aumid: String,
    /// `player_map::resolve()` 解析出的进程名（身份，可能为空串）。
    pub process_name: String,
    /// `GetPlaybackInfo()?.PlaybackStatus() == Playing`。
    pub playing: bool,
    /// `MediaIndex::matches()` 的结果；`true` = 被拉黑，必须完全忽略、永不选中。
    pub ignored: bool,
}

/// 会话挑选结果（G8-①）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChoice {
    /// 显示候选切片里该下标的正常会话（调用方据此 `GetAt(i)` 取回 COM 会话）。
    Show(usize),
    /// 没有可显示的正常会话 → 保持隐藏（沿用现状空态）。
    Hide,
}

/// 一次会话枚举结果的**状态动作**（G8-①）。
///
/// 把「候选画像」翻译成媒体线程要做的动作，刻意不含 COM 句柄：
/// [`PollState::decide_scan`] 是纯逻辑，整个多会话状态机因此可确定性单测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanDecision {
    /// 改显示该下标的正常会话（调用方取回 COM 句柄并重置单会话缓存）。
    Switch(usize),
    /// 仍在显示同一个正常会话：只刷新句柄，缓存一律不动。
    Keep(usize),
    /// 一个正常会话都没有 → 保持隐藏（沿用现状空态）。
    Hide,
    /// 黑名单会话已全部消失 → 回到常规 `GetCurrentSession()` 路径，停止枚举。
    Release,
}

/// **会话挑选的纯函数**（G8-① / 验收约束 6，可确定性单测、不依赖 COM）。
///
/// 系统「当前会话」命中黑名单时用它从全部候选里挑一个来显示，规则按优先级：
/// 1. 命中黑名单的候选一律跳过（约束 1：被拉黑的应用仍完全忽略）；
/// 2. 存在「正在播放（Playing）」的正常会话 → 取 `GetSessions()` 系统顺序里
///    第一个 Playing 的正常会话；
/// 3. 全都暂停 → 取系统顺序里第一个正常会话；
/// 4. 全部被拉黑 / 没有候选 → [`SessionChoice::Hide`]（沿用现状空态）。
///
/// 返回的是**下标**，让调用方自己把纯数据映射回 COM 会话，函数体保持纯逻辑。
pub fn choose_session(candidates: &[SessionCandidate]) -> SessionChoice {
    if let Some(i) = candidates
        .iter()
        .position(|c| !c.ignored && c.playing)
    {
        return SessionChoice::Show(i);
    }
    match candidates.iter().position(|c| !c.ignored) {
        Some(i) => SessionChoice::Show(i),
        None => SessionChoice::Hide,
    }
}

/// 是否该发起一次低频会话枚举（G8-① / 验收约束 4、6 的测试化）。
///
/// **性能红线**：常规播放路径（`blacklist_hit == false`）永远返回 `false`，
/// 所以 `GetSessions()` 绝不会进入每 50ms 的常规 tick；只有命中黑名单时才按
/// [`IGNORE_RECHECK_MS`] 节流。`last_scan = None`（从未枚举）时首次命中立即放行，
/// 避免用户等 1s 才看到另一个正在播放的播放器。
pub fn session_scan_due(
    blacklist_hit: bool,
    last_scan: Option<Instant>,
    now: Instant,
) -> bool {
    if !blacklist_hit {
        return false;
    }
    match last_scan {
        None => true,
        Some(t) => {
            now.saturating_duration_since(t) >= Duration::from_millis(IGNORE_RECHECK_MS)
        }
    }
}

/// 一次会话枚举 → 媒体线程的状态动作（G8-①，**纯函数**，零 COM / 零 IO）。
///
/// `displayed_aumid` = 小组件当前锁定显示的那个会话（`None` = 常规路径）。
/// - 黑名单会话已全部消失 → [`ScanDecision::Release`]：回到常规路径，枚举停止；
/// - 仍有黑名单会话但没有正常候选 → [`ScanDecision::Hide`]：沿用现状空态；
/// - 选中的正常会话正是当前显示的那一个 → [`ScanDecision::Keep`]：不乱清缓存；
/// - 否则 → [`ScanDecision::Switch`]：换会话（调用方重置缓存防串歌）。
pub fn scan_decision(
    candidates: &[SessionCandidate],
    displayed_aumid: Option<&str>,
) -> ScanDecision {
    if !candidates.iter().any(|c| c.ignored) {
        return ScanDecision::Release;
    }
    match choose_session(candidates) {
        SessionChoice::Hide => ScanDecision::Hide,
        SessionChoice::Show(i) => {
            let shown = displayed_aumid.filter(|s| !s.is_empty());
            let same = candidates
                .get(i)
                .is_some_and(|c| Some(c.aumid.as_str()) == shown);
            if same {
                ScanDecision::Keep(i)
            } else {
                ScanDecision::Switch(i)
            }
        }
    }
}

/// P2-1：回到常规路径（[`ScanDecision::Release`]）时，是否必须广播
/// `AppEvent::IgnoredSession(None)` 清掉 UI 侧 `App.ignored` 的残留。
///
/// 纯数据判定：`decision` 必须是 Release，`had_ignored` 表示媒体线程上一次
/// 广播过 `IgnoredSession(Some)`（托盘此刻还显示「恢复某应用」）。G7 基线靠
/// `tick_ignored()` 的「忽略→正常」分支自愈，G8-① 的 Release 绕过了那条路径，
/// 若不在这里清，就会出现「A 已退出、B 正常显示，托盘却还写着恢复 A」。
/// 只有两者同时成立才发事件——不重复打扰 UI。
pub fn release_needs_ignored_clear(decision: ScanDecision, had_ignored: bool) -> bool {
    matches!(decision, ScanDecision::Release) && had_ignored
}

/// P2-2：**主循环的性能闸**（G8-① / 验收约束 4 的测试化）。
///
/// `ignored_now` = 缓存身份命中黑名单；`has_displayed_session` = 正锁定显示
/// `GetSessions()` 挑出的替代会话。只有这两者之一成立才允许进入低频会话枚举；
/// **`false, false`（常规播放路径）必须返回 `false`** —— 于是 `GetSessions()`
/// 永不进每 50ms 的常规 tick，零新增跨进程调用。抽成纯函数是为了让这条红线
/// 被单测锁住，而不是只靠代码走查。
pub fn scan_gate(ignored_now: bool, has_displayed_session: bool) -> bool {
    ignored_now || has_displayed_session
}

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

    /// **只推进三层时间戳**，不做任何分析（v1.9.0 / G7，ARCH-G7 §4.3）。
    ///
    /// 为什么需要它：忽略态下我们跳过了所有读取，若不推进时间戳，`plan` 会永远
    /// "到点"，主循环每 50ms 就发一次 `GetCurrentSession`。用 `note_*` 代推不行——
    /// 它们会改退避状态机（例如喂 (0,0) 会把网易云式退避推到 5s），是副作用。
    /// 铁律：不碰 `zero_streak` / `timeline_backoff` / `prev_timeline` / `prev_state`。
    fn consume(&mut self, now: Instant, plan: Plan) {
        if plan.playback {
            self.last_playback = Some(now);
        }
        if plan.timeline {
            self.last_timeline = Some(now);
        }
        if plan.props {
            self.last_props = Some(now);
        }
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
    /// UI 线程与媒体线程共享的黑名单 / 真实名索引（v1.9.0 / G7）。
    /// 判定只走它的纯内存方法——这里**绝不**出现 OpenProcess / COM / 文件 IO。
    index: Arc<MediaIndex>,
    /// 上一次广播给 UI 的"被忽略会话"（diff 用；`None` = 当前无被忽略会话）
    last_ignored: Option<IgnoredSessionInfo>,
    /// 上次真正发起身份重查的时刻（忽略态的 `IGNORE_RECHECK_MS` 闸）
    last_ignore_probe: Instant,
    /// 命中日志限流（按进程名分桶；`perf::RateLimiter` 的 key 是 `&'static str`，
    /// 表达不了动态进程名，所以自己实现）
    last_hit: Option<(String, Instant)>,
    /// G8-①：**小组件正在显示的那个会话**。
    ///
    /// `None` = 常规路径，每轮照旧用 `GetCurrentSession()`（与 G7 逐字节一致）；
    /// `Some` = 系统「当前会话」命中黑名单后，从 `GetSessions()` 里挑出的正常会话。
    /// 只要它在，`tick()` 与 `execute()` 都针对它——否则 `GetCurrentSession()` 会
    /// 一直返回被拉黑的那个，小组件就永远看不到正常播放器。
    display_session: Option<GlobalSystemMediaTransportControlsSession>,
    /// 上次因黑名单命中而枚举会话的时刻；`None` = 从未枚举（首次命中立即枚举）。
    /// 与 `last_ignore_probe` 分开记，避免 G7 的身份重查闸被 G8 的枚举互相带偏。
    last_session_scan: Option<Instant>,
}

impl PollState {
    fn new(dirty: Arc<AtomicBool>, index: Arc<MediaIndex>) -> Self {
        Self {
            last: None,
            cover: CoverMemo::default(),
            parts: SessionParts::default(),
            cadence: Cadence::default(),
            stats: PollStats::new(Instant::now()),
            last_tick: Instant::now(),
            dirty,
            session_events: None,
            index,
            last_ignored: None,
            last_ignore_probe: Instant::now(),
            last_hit: None,
            display_session: None,
            last_session_scan: None,
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

    // ------------------------------------------ 忽略态（v1.9.0 / G7，ARCH-G7 §4.3）

    /// 当前会话是否被拉黑。**纯内存**：用上一轮缓存的 `parts` 身份，零跨进程调用。
    /// 每轮主循环（含 50ms 空转轮次）最多调 1 次。
    fn ignored_now(&self) -> bool {
        self.index.matches(&self.parts.process_name, &self.parts.aumid)
    }

    /// 命中日志限流（按进程名 60s 窗口）。返回是否真的要打日志。
    fn hit_log_due(&mut self, name: &str, now: Instant) -> bool {
        match &self.last_hit {
            Some((prev, at))
                if prev == name
                    && now.saturating_duration_since(*at)
                        < Duration::from_millis(limits::HIT_LOG_WINDOW_MS) =>
            {
                false
            }
            _ => {
                self.last_hit = Some((name.to_string(), now));
                true
            }
        }
    }

    /// 进入（或保持）忽略态的**一次性**转移（正常 → 忽略 / 忽略中身份变化复用）。
    ///
    /// 四件事，顺序固定：① `last = None`（解绑后才可能重新广播）
    /// ② `SessionCleared`（复用既有变体：UI 侧复位 playing/lyrics 并隐藏小组件）
    /// ③ 命中 INFO（按进程名 60s 限流）④ `IgnoredSession(Some)`。
    /// **绝不**清 `parts` / `cadence` / `session_events`——`parts` 是唯一身份来源。
    fn enter_ignored(&mut self, sink: &EventSink<AppEvent>, now: Instant) {
        // ① 丢弃上一份快照；G8-①：进入空态就不再锁定任何替代会话
        self.last = None;
        self.cover.done = false;
        self.display_session = None;
        // ② 复用既有 SessionCleared（UI 侧已会把 playing/lyrics 复位并隐藏小组件）
        sink.send(AppEvent::SessionCleared);
        // ③ 命中日志（限流）
        let id = self.index.identity(&self.parts.process_name, &self.parts.aumid);
        let label = if id.key.is_empty() {
            self.parts.aumid.clone()
        } else {
            id.key.clone()
        };
        if self.hit_log_due(&label, now) {
            crate::log_info!("黑名单命中，忽略会话：{}（aumid={}）", label, self.parts.aumid);
        }
        // ④ 广播忽略态
        let info = IgnoredSessionInfo {
            process_name: self.parts.process_name.clone(),
            aumid: self.parts.aumid.clone(),
            app_name: self.parts.app_name.clone(),
        };
        self.last_ignored = Some(info.clone());
        sink.send(AppEvent::IgnoredSession(Some(Box::new(info))));
    }

    // ------------------------------------------ 多会话挑选（v1.9.1 / G8-①）

    /// 取本轮要读的会话：常规路径仍是**恰好 1 次** `GetCurrentSession()`，
    /// 已锁定替代会话时直接复用句柄（零新增跨进程调用）。
    fn acquire_session(
        &self,
        mgr: &Manager,
    ) -> Result<GlobalSystemMediaTransportControlsSession> {
        match &self.display_session {
            Some(s) => Ok(s.clone()),
            None => mgr.GetCurrentSession(),
        }
    }

    /// 低频枚举一次 `GetSessions()`，把每个会话降维成 [`SessionCandidate`]。
    ///
    /// **只在「命中黑名单」且过闸时调用**；正常播放路径永不进入。读取白名单与
    /// G7 的枚举一致：`GetSessions` / `Size` / `GetAt` / `SourceAppUserModelId` /
    /// `GetPlaybackInfo`，绝不碰 `TryGetMediaPropertiesAsync` / 封面 / 时间线。
    /// 返回的第二个 Vec 与候选**下标一一对应**，供调用方 `choose_session` 后取回句柄。
    fn collect_candidates(
        &self,
        mgr: &Manager,
    ) -> (
        Vec<SessionCandidate>,
        Vec<GlobalSystemMediaTransportControlsSession>,
    ) {
        let mut candidates: Vec<SessionCandidate> = Vec::new();
        let mut sessions: Vec<GlobalSystemMediaTransportControlsSession> = Vec::new();
        let Ok(view) = mgr.GetSessions() else {
            crate::log_debug!("G8-①：枚举会话失败，保持现状");
            return (candidates, sessions);
        };
        let count = view.Size().unwrap_or(0);
        for i in 0..count {
            let Ok(s) = view.GetAt(i) else {
                continue;
            };
            let aumid = s
                .SourceAppUserModelId()
                .map(|h| h.to_string())
                .unwrap_or_default();
            let process_name = player_map::resolve(&aumid).process_name.to_string();
            // `MediaIndex::matches` 是纯内存三级回退（缓存 → 静态表 → AUMID 末段），
            // 这里零 IO / 零 COM，与 G7 的判定口径完全同源。
            let ignored = self.index.matches(&process_name, &aumid);
            let playing = s
                .GetPlaybackInfo()
                .and_then(|p| p.PlaybackStatus())
                .map(|st| st == PlaybackStatus::Playing)
                .unwrap_or(false);
            candidates.push(SessionCandidate {
                aumid,
                process_name,
                playing,
                ignored,
            });
            sessions.push(s);
        }
        (candidates, sessions)
    }

    /// **切换到另一个正常会话**：把单会话缓存整体作废，杜绝 A/B 会话串歌。
    ///
    /// `SessionParts`（歌名/歌词锚点/track_id）、`Cadence`（三层节奏与退避）、
    /// `CoverMemo`、`SessionSubscriptions`（跟随 AUMID 的事件订阅）、`last`（快照）
    /// 全部重置——下一轮必然按 `Plan::full()` 重新读新会话并重新广播 NowPlaying，
    /// 旧会话的标题、进度基准、歌词代次一概不带过去。
    fn switch_display_session(
        &mut self,
        session: GlobalSystemMediaTransportControlsSession,
        c: &SessionCandidate,
        sink: &EventSink<AppEvent>,
        now: Instant,
    ) {
        self.parts = SessionParts::default();
        self.parts.aumid = c.aumid.clone();
        self.parts.process_name = c.process_name.clone();
        self.parts.app_name = player_map::resolve(&c.aumid).display_name.to_string();
        self.cadence = Cadence::default();
        self.cover = CoverMemo::default();
        self.session_events = None; // Drop 会反注册旧订阅；按新 AUMID 重新挂
        self.last = None; // 强制下一轮重新广播新会话的快照
        self.last_ignore_probe = now;
        self.display_session = Some(session);
        // 「当前应用」现在就是这个正在显示的会话 → 清掉托盘/设置页的忽略态，
        // 与 app.rs 的 `state.playing.session` 单一口径对齐（G8-① 约束 5）。
        if self.last_ignored.take().is_some() {
            sink.send(AppEvent::IgnoredSession(None));
        }
    }

    /// P2-1：执行 [`ScanDecision::Release`] —— 回到常规 `GetCurrentSession()`
    /// 路径，并**与 Switch 分支一致**清掉 UI 侧残留的忽略态。
    ///
    /// 抽成独立方法是为了让事件接线可用假 `EventSink` 单测（无 COM）：
    /// `rescan_sessions()` 的 Release 分支只调用它，不再内联这些副作用。
    fn apply_release(&mut self, decision: ScanDecision, sink: &EventSink<AppEvent>) {
        // 黑名单独象已不成立：解绑回常规路径，并让缓存身份不再被误判为命中。
        self.display_session = None;
        self.parts = SessionParts::default();
        self.cadence = Cadence::default();
        self.session_events = None;
        self.last = None;
        // 上一次广播过 IgnoredSession(Some) → UI 的 App.ignored 还指向旧应用，
        // 必须发 None 让托盘/设置页口径切回「正在显示的会话」（约束 5）。
        if release_needs_ignored_clear(decision, self.last_ignored.take().is_some()) {
            sink.send(AppEvent::IgnoredSession(None));
        }
        crate::log_debug!("G8-①：黑名单会话已消失，回到系统当前会话");
    }

    /// **多会话状态机的纯决策**（G8-①，测试入口）。
    ///
    /// 输入只有候选画像与 `PollState` 自身的状态，**零 COM、零 IO**：
    /// - 黑名单会话已全部消失 → [`ScanDecision::Release`]（回到常规路径，枚举停止）；
    /// - 仍有黑名单会话且 [`choose_session`] 找不到正常候选 → [`ScanDecision::Hide`]；
    /// - 选中的正常会话就是当前显示的那一个 → [`ScanDecision::Keep`]（不制造每秒重播）；
    /// - 否则 → [`ScanDecision::Switch`]（换会话，调用方负责重置缓存防串歌）。
    fn decide_scan(&self, candidates: &[SessionCandidate]) -> ScanDecision {
        let displayed = if self.display_session.is_some() {
            Some(self.parts.aumid.as_str()).filter(|s| !s.is_empty())
        } else {
            None
        };
        scan_decision(candidates, displayed)
    }

    /// 命中黑名单后的**一次低频会话枚举**（G8-① 核心）。
    ///
    /// 返回 `true` = 已切换到另一个正常会话（调用方本轮应全量读它并发布）；
    /// `false` = 保持现状。动作一律由 [`PollState::decide_scan`] 给出，
    /// 「命中即隐藏」不再是一句 `enter_ignored` 就能走到的捷径。
    ///
    /// 本方法只由 `tick()` 的两个拦截点或主循环在「命中黑名单 / 正显示替代会话」
    /// 时调用；`tick_ignored()` 与 `Cadence::consume` 一行未改（G7 语义不污染）。
    fn rescan_sessions(
        &mut self,
        mgr: &Manager,
        sink: &EventSink<AppEvent>,
        now: Instant,
    ) -> bool {
        if !session_scan_due(true, self.last_session_scan, now) {
            return false;
        }
        self.last_session_scan = Some(now);
        let (candidates, sessions) = self.collect_candidates(mgr);

        match self.decide_scan(&candidates) {
            decision @ ScanDecision::Release => {
                // P2-1：与 Switch 分支一致清掉残留忽略态，否则「A 退出 / B 接管」
                // 后托盘仍显示「恢复 A」，用户点它会误把 A 从黑名单移除。
                self.apply_release(decision, sink);
                false
            }
            ScanDecision::Hide => {
                // 确实一个正常会话都没有 → 沿用现状空态（约束 2 后半句）。
                // 用其中一个被拉黑候选的身份填 parts，让 ignored_now() 成立，
                // 托盘/设置页的「恢复当前应用」仍指向造成隐藏的那个会话。
                if let Some(c) = candidates.iter().find(|c| c.ignored) {
                    self.parts.aumid = c.aumid.clone();
                    self.parts.process_name = c.process_name.clone();
                    self.parts.app_name = player_map::resolve(&c.aumid).display_name.to_string();
                }
                self.display_session = None;
                // 1s 闸下会反复走到这里：只在「忽略身份真的变了 / 刚进入空态」
                // 时广播事件，避免已经隐藏时每秒重复发 SessionCleared 打扰 UI。
                let identity_changed = self
                    .last_ignored
                    .as_ref()
                    .map(|p| p.aumid.as_str())
                    != Some(self.parts.aumid.as_str());
                if identity_changed {
                    self.enter_ignored(sink, now);
                }
                false
            }
            ScanDecision::Keep(i) => {
                // 同一会话：只更新 COM 句柄，避免每秒一次清空/重播。
                if let Some(s) = sessions.get(i) {
                    self.display_session = Some(s.clone());
                }
                false
            }
            ScanDecision::Switch(i) => {
                let (Some(session), Some(c)) = (sessions.get(i), candidates.get(i)) else {
                    return false;
                };
                crate::log_info!(
                    "G8-①：系统当前会话被拉黑，改显示其它正常会话：index={} aumid={} playing={}",
                    i,
                    c.aumid,
                    c.playing
                );
                self.switch_display_session(session.clone(), c, sink, now);
                true
            }
        }
    }

    /// 忽略态轮次：不发 NowPlaying、不读时间线/属性/封面/controls。
    ///
    /// 唯一可能发生的跨进程调用：`forced || plan.props` 且距上次重查 ≥
    /// [`IGNORE_RECHECK_MS`] 时，做 1 次 `GetCurrentSession()` +
    /// 1 次 `SourceAppUserModelId()`，用来判断「身份变了 / 已解绑」。
    /// 其余轮次只推进节奏（[`Cadence::consume`]）后直接 return。
    ///
    /// **红线**：本方法绝不复位 `parts` / `cadence` / `session_events`
    /// （那是正常 tick 里"会话真没了"分支的专属动作）——清了就永远认不出解绑。
    fn tick_ignored(
        &mut self,
        mgr: &Manager,
        sink: &EventSink<AppEvent>,
        now: Instant,
        plan: Plan,
        forced: bool,
    ) {
        self.last_tick = now;
        self.cadence.consume(now, plan);
        if !(forced || plan.props) {
            return;
        }
        if now.saturating_duration_since(self.last_ignore_probe)
            < Duration::from_millis(IGNORE_RECHECK_MS)
        {
            return;
        }
        self.last_ignore_probe = now;

        let session = match mgr.GetCurrentSession() {
            Ok(s) => s,
            Err(_) => {
                // 会话暂时拿不到（可能被拉黑应用刚退出、SMTC 还在过渡）：
                // 保持忽略态不动、不发事件——下一轮重查自会分辨「真没了」还是「换会话」。
                return;
            }
        };
        let aumid = session
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();
        let resolved = player_map::resolve(&aumid);
        // 身份字段照常刷新（ARCH-G7 §4.5：不改 parts 字段语义，
        // process_name 还被 session_matches / has_changed 消费）
        self.parts.aumid = aumid;
        self.parts.app_name = resolved.display_name.to_string();
        self.parts.process_name = resolved.process_name.to_string();

        if self.index.matches(&self.parts.process_name, &self.parts.aumid) {
            // 忽略 → 忽略（身份变了才再广播；进程名变了必然过限流）
            let changed = match &self.last_ignored {
                Some(prev) => prev.aumid != self.parts.aumid,
                None => true,
            };
            if changed {
                self.enter_ignored(sink, now);
            }
            return;
        }
        // 忽略 → 正常（解绑 / 换会话）：什么都不清，主循环下一轮走正常 tick()，
        // 因 `last == None` 必然重新广播 NowPlaying。
        self.last_ignored = None;
        sink.send(AppEvent::IgnoredSession(None));
    }

    /// 按计划做一轮读取，diff 后决定是否广播。
    ///
    /// G8-①：本轮读的会话由 [`PollState::acquire_session`] 决定——常规路径仍是
    /// `GetCurrentSession()`，命中黑名单改选的正常会话则一直锁定到黑名单条件解除。
    fn tick(&mut self, mgr: &Manager, sink: &EventSink<AppEvent>, now: Instant, mut plan: Plan) {
        self.last_tick = now;

        let session = match self.acquire_session(mgr) {
            Ok(s) => s,
            Err(_) => {
                if self.last.is_some() {
                    crate::log_info!("session cleared");
                    self.last = None;
                    self.cover.done = false;
                    // 会话没了 → 分层缓存、节奏、会话级订阅全部作废，
                    // 下次会话从全量读开始（G8-①：替代会话句柄一并作废）
                    self.parts = SessionParts::default();
                    self.cadence = Cadence::default();
                    self.session_events = None;
                    self.display_session = None;
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
            // ---- G7 拦截点①（ARCH-G7 §4.2①）：AUMID 提出 timed 闭包，身份已知即早退 ----
            // 命中则跳过 TryGetMediaPropertiesAsync / 封面解码 / 播控标志读取——
            // 且**不计入** props 统计（60s 统计 props 下降的验收依据）。
            let aumid = session
                .SourceAppUserModelId()
                .map(|h| h.to_string())
                .unwrap_or_default();
            if self.index.matches(player_map::resolve(&aumid).process_name, &aumid) {
                self.parts.aumid = aumid;
                let resolved = player_map::resolve(&self.parts.aumid);
                self.parts.app_name = resolved.display_name.to_string();
                self.parts.process_name = resolved.process_name.to_string();
                // 刚读过一次 AUMID → 重查闸从现在起算
                self.last_ignore_probe = now;
                // G8-①：命中黑名单**不再直接隐藏整个小组件**。低频枚举一次其它会话，
                // 只要存在正常会话就改显示它；只有确实一个都没有时才由
                // `rescan_sessions` 内部沿用现状空态（约束 2）。
                // 切换成功则本轮立即全量读新会话并发布，不留 50ms 空窗、不串歌。
                // 递归深度天然有界：rescan 已把 last_session_scan 推到 now，
                // 1s 闸保证同一轮不可能再切第二次。
                if self.rescan_sessions(mgr, sink, now) {
                    return self.tick(mgr, sink, now, Plan::full());
                }
                return;
            }
            let (props, ms) = timed("smtc.props", || {
                timed_join("TryGetMediaPropertiesAsync", || {
                    session.TryGetMediaPropertiesAsync()?.join()
                })
            });
            self.stats.props += 1;
            self.stats.observe(ms);

            match props {
                Ok(props) => {
                    self.parts.aumid = aumid;
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

        // ---- G7 拦截点②（发布点，ARCH-G7 §4.2②）：命中则**不发** NowPlaying ----
        // 与①同时存在不是冗余：①省成本、②保证语义（防 AUMID 与 metadata
        // 在轮次间不同步的窗口；漏一处就是 bug）。
        // G8-①：与①同一套处理——先低频找其它正常会话，找到了就改显示它。
        if self.index.matches(&self.parts.process_name, &self.parts.aumid) {
            if self.rescan_sessions(mgr, sink, now) {
                return self.tick(mgr, sink, now, Plan::full());
            }
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
///
/// `index` 由 `App::bootstrap` 从 `Config.media` 构建注入（G7）：
/// 黑名单判定与真实名缓存全是它的纯内存方法，主循环零新增跨进程调用。
pub fn run(sink: EventSink<AppEvent>, cmd_rx: Receiver<MediaCommand>, index: Arc<MediaIndex>) {
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
    let mut state = PollState::new(dirty.clone(), index);
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
            // G8-①：只有「命中黑名单」或「正在显示替代会话」时才可能低频枚举一次
            // 会话。常规播放路径两者皆否 → 完全不进这段，零新增跨进程调用、零定时器。
            // 两种情况都复用同一把 `IGNORE_RECHECK_MS` 闸（≤1 次/秒）。
            // P2-2：性能闸抽成纯函数 `scan_gate`，由 4 格真值表单测锁住
            // 「常规播放路径（false,false）绝不枚举」这条红线（约束 4）。
            let blacklist_active =
                scan_gate(state.ignored_now(), state.display_session.is_some());
            let switched = blacklist_active
                && session_scan_due(true, state.last_session_scan, now)
                && state.rescan_sessions(&mgr, &sink, now);
            if switched {
                // 已切到另一个正常会话：本轮全量读它并发布（不留 50ms 空窗）。
                state.tick(&mgr, &sink, now, Plan::full());
            } else if state.ignored_now() {
                // G7：确实一个正常会话都没有 → 沿用 ignored 专用轮次。
                // 本分支一行未改：tick_ignored 与 Cadence::consume 语义原样保留。
                state.tick_ignored(&mgr, &sink, now, plan, forced);
            } else {
                state.tick(&mgr, &sink, now, plan);
            }
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
        // `SetPaused` 由 `run` 在命令入口截获，绝不该走到这里；
        // 显式列出是防御未来有人在别处调用 `execute`。
        MediaCommand::SetPaused(_) => return,
        // v1.9.0 / G7：枚举"检测到的应用"。**直接 return**，不许走
        // GetCurrentSession 之后的任何逻辑（ARCH-G7 §5.4 白名单 / R4）。
        MediaCommand::EnumerateApps => {
            let snap = enumerate_apps(mgr, &state.index); // 内部自带预算与降级，不会 panic
            sink.send(AppEvent::MediaApps(Box::new(snap)));
            return;
        }
        _ => {}
    }

    // ---- G7 拦截点③（ARCH-G7 §4.2③）：播控命令入口拦截 ----
    // 命中则整条丢弃 + log_debug，**不发 CommandResult**——app.rs 收到 ok:false
    // 会打「命令未被播放器接受，回滚乐观更新」，把"我们主动丢弃"说成"播放器拒绝"，
    // 会误导以后看日志的人。parts 里就有身份（process_name / aumid），零新增调用。
    // 若不拦，热键（Ctrl+Alt+P）会把 TryTogglePlayPauseAsync 打到被拉黑的应用上。
    if state.ignored_now() {
        crate::log_debug!("黑名单命中，播控命令 {cmd:?} 丢弃");
        return;
    }

    let kind = match cmd {
        MediaCommand::PlayPause => MediaCommandKind::PlayPause,
        MediaCommand::Next => MediaCommandKind::Next,
        MediaCommand::Prev => MediaCommandKind::Prev,
        MediaCommand::Seek(_) => MediaCommandKind::Seek,
        MediaCommand::Refresh | MediaCommand::Quit => return,
        MediaCommand::SetPaused(_) => return,
        MediaCommand::EnumerateApps => return,
    };

    // G8-①：播控必须打到「小组件正在显示的那个会话」。系统当前会话命中黑名单
    // 时，显示的其实是 `display_session` 里挑出的正常会话；若仍用
    // `GetCurrentSession()`，热键会把命令误发到被拉黑的应用上，同时既没有
    // 控制到显示中的播放器、也破坏「被拉黑应用完全忽略」的语义。
    let session = match state.display_session.as_ref() {
        Some(s) => s.clone(),
        None => match mgr.GetCurrentSession() {
            Ok(s) => s,
            Err(_) => {
                crate::log_warn!("无活动会话，命令 {cmd:?} 丢弃");
                sink.send(AppEvent::CommandResult { kind, ok: false });
                return;
            }
        },
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
        MediaCommand::Refresh | MediaCommand::Quit => return,
        MediaCommand::SetPaused(_) => return,
        MediaCommand::EnumerateApps => return,
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

// ---------------------------------------------------------------- 枚举（v1.9.0 / G7）

/// 检测到的应用（一条，ARCH-G7 §5.1）。
#[derive(Debug, Clone)]
pub struct MediaAppEntry {
    /// 身份键（= `MediaIndex::identity().key`），小写含 `.exe`；解析失败为空串
    pub process: String,
    /// `player_map` 的显示名；未命中为 "未知播放器"
    pub display_name: String,
    /// 能否加入黑名单（`process` 非空 = 能）
    pub playable: bool,
    /// true = 名字是**真实进程名**（来自缓存或静态表）；false = AUMID 末段兜底（可能是猜的）
    pub resolved: bool,
    /// "cache" | "table" | "aumid"（诊断 + 前端软提示用）
    pub source: &'static str,
}

/// 枚举一次（**必须跑在媒体线程**：`Manager` 是 MTA 对象，反查含 `OpenProcess`）。
///
/// 三步（ARCH-G7 §5.1）：① collect（`GetSessions` → 逐会话 AUMID）
/// → ② reverse（只对表未命中且缓存未命中的条目做 WASAPI 反查）
/// → ③ finish（`identity()` 三级回退定 process / resolved / source）。
///
/// **读取白名单（§5.4，超出的都是 bug）**：`GetSessions` / `GetAt` /
/// `SourceAppUserModelId` / WASAPI 会话枚举 + `GetProcessId` +
/// `launch::process_image_name` / `session_matches`（纯内存）。
/// **禁止** `TryGetMediaPropertiesAsync` / `GetTimelineProperties` /
/// `GetPlaybackInfo` / 封面读取 / `Manager::RequestAsync().join()` / 任何文件写。
///
/// G8-①：这里枚举的是**全部**会话，而不是只看 `GetCurrentSession()`——
/// 与「黑名单应用是当前会话时改显示其它正常会话」的多会话口径一致；
/// 设置页「检测到的应用」因此能列出所有候选，用户从列表加入最稳。
fn enumerate_apps(mgr: &Manager, index: &MediaIndex) -> MediaAppsSnapshot {
    let start = Instant::now();
    let _sp = Span::with_threshold("smtc.enumerate", perf::SMTC_SLOW_MS);

    // ---- ① collect：SMTC 会话 → (aumid, display_name, 表是否命中) ----
    let sessions: Vec<String> = match mgr.GetSessions() {
        Ok(view) => {
            // windows-rs 0.62：IVectorView 的取元素方法是 GetAt(i)，
            // 大小属性方法名是 Size()（ARCH 文档里的 GetCount 是笔误口径）
            let count = view.Size().unwrap_or(0);
            let mut out = Vec::new();
            for i in 0..count {
                match view.GetAt(i) {
                    Ok(s) => {
                        let aumid = s
                            .SourceAppUserModelId()
                            .map(|h| h.to_string())
                            .unwrap_or_default();
                        if !aumid.is_empty() {
                            out.push(aumid);
                        }
                    }
                    Err(e) => crate::log_debug!("枚举：GetAt({i}) 失败：{e}"),
                }
            }
            out
        }
        Err(e) => {
            // COM 异常 / manager 失效：发**空快照**（不是不发），UI 空态文案
            crate::log_warn!("枚举媒体会话失败：{e}");
            return MediaAppsSnapshot {
                apps: Vec::new(),
                at_ms: epoch_ms(),
                learned: Vec::new(),
                over_budget: false,
            };
        }
    };

    // 每条的身份要素：表命中与否决定要不要反查（D9：静态表已命中的不反查）
    let mut entries: Vec<(String, bool)> = sessions
        .into_iter()
        .map(|aumid| {
            let table_hit = !player_map::resolve(&aumid).process_name.is_empty();
            (aumid, table_hit)
        })
        .collect();
    // 内存缓存已命中的同样不反查（identity ① 级已是实测真实名）
    let mut over_budget = false;
    let mut learned: Vec<(String, String)> = Vec::new();
    let targets: Vec<(usize, String)> = entries
        .iter()
        .enumerate()
        .filter(|(_, (aumid, table_hit))| {
            !*table_hit && !index.identity("", aumid).key.is_empty()
        })
        .filter(|(_, (aumid, _))| {
            // 缓存命中（source == Cache）说明已有实测名 → 不反查
            index.identity("", aumid).source != blacklist::IdentitySource::Cache
        })
        .map(|(i, (aumid, _))| (i, aumid.clone()))
        .collect();

    // ---- ② reverse：一次 WASAPI 遍历，拿「音频会话 pid → 真实镜像名」----
    if !targets.is_empty() {
        let _lp = Span::with_threshold("smtc.lookup", perf::SMTC_SLOW_MS);
        let (images, budget_hit) = collect_audio_session_images();
        if budget_hit {
            over_budget = true;
        }
        let mut learned_set: HashSet<String> = HashSet::new();
        for (_, want_aumid) in &targets {
            // 对得上音频会话 → 真实进程名；对不上 → 留给 ③ 级兜底（失败不静默）
            if let Some((_, image)) = images
                .iter()
                .find(|(_, image)| crate::media::session_matches(image, "", want_aumid))
            {
                if learned_set.insert(want_aumid.clone()) {
                    learned.push((want_aumid.clone(), image.clone()));
                }
            }
        }
        if !learned.is_empty() {
            // 反查成功后**立刻**写内存副本（本轮 tick 即生效；配置落盘由 UI 线程做）
            index.learn(&learned);
        }
    }

    // ---- ③ finish：identity() 三级回退 → process / resolved / source ----
    // 硬规定：枚举、判定、UI 显示三处只能通过 `MediaIndex::identity()`（R6）。
    let apps: Vec<MediaAppEntry> = entries
        .drain(..)
        .map(|(aumid, _table_hit)| {
            let resolved_info = player_map::resolve(&aumid);
            let id = index.identity(resolved_info.process_name, &aumid);
            let source = match id.source {
                blacklist::IdentitySource::Cache => "cache",
                blacklist::IdentitySource::Table => "table",
                _ => "aumid",
            };
            MediaAppEntry {
                playable: !id.key.is_empty(),
                resolved: matches!(
                    id.source,
                    blacklist::IdentitySource::Cache | blacklist::IdentitySource::Table
                ),
                process: id.key,
                display_name: resolved_info.display_name.to_string(),
                source,
            }
        })
        .collect();

    // 出口复核总预算：超了照发结果 + over_budget，只记日志（ARCH-G7 §5.2 检查点 4）
    let elapsed = start.elapsed();
    if elapsed > Duration::from_millis(limits::ENUM_TOTAL_BUDGET_MS) {
        over_budget = true;
        crate::log_warn!("枚举超出总预算 {}ms", elapsed.as_millis());
    }
    MediaAppsSnapshot {
        apps,
        at_ms: epoch_ms(),
        learned,
        over_budget,
    }
}

/// WASAPI 反查的采集步：**一次** `CoCreateInstance` → `GetDefaultAudioEndpoint`
/// → `Activate(IAudioSessionManager2)` → `GetSessionEnumerator`，遍历音频会话取
/// `(pid, 镜像名)`。三道闸门：会话数 ≤ `limits::ENUM_MAX_AUDIO_SESSIONS`、
/// 进程镜像名查询 ≤ `limits::ENUM_MAX_LOOKUPS` 且每次调用**之前**查
/// `limits::ENUM_LOOKUP_BUDGET_MS` deadline、pid 去重（浏览器每标签一个会话）。
/// 返回 `(去重后的 (pid, 镜像名) 列表, 是否撞了闸门)`。
///
/// **单次失败只跳过、绝不 `?` 提前返回整个枚举**（wasapi.rs 的既有教训）。
/// `OpenProcess` / `QueryFullProcessImageNameW` 是同步系统调用、不可中断——
/// 兜底只能是"调用前查预算 + 失败即降级"，不许写伪超时。
fn collect_audio_session_images() -> (Vec<(u32, String)>, bool) {
    let deadline = Instant::now() + Duration::from_millis(limits::ENUM_LOOKUP_BUDGET_MS);
    let mut over_budget = false;
    let chain: windows::core::Result<IAudioSessionEnumerator> = (|| {
        // SAFETY: 所有 COM 对象在闭包作用域内释放；CLSID 是系统注册的进程内组件。
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
            let mgr: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            mgr.GetSessionEnumerator()
        }
    })();
    let sessions = match chain {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("音频会话不可用，反查跳过：{e}");
            return (Vec::new(), false);
        }
    };
    // SAFETY: sessions 是活的 COM 对象，以下调用全部在作用域内。
    let count = unsafe { sessions.GetCount().unwrap_or(0) };
    let mut images: Vec<(u32, String)> = Vec::new();
    let mut seen_pids: HashSet<u32> = HashSet::new();
    let mut visited = 0usize;
    for i in 0..count {
        if visited >= limits::ENUM_MAX_AUDIO_SESSIONS {
            over_budget = true;
            break;
        }
        visited += 1;
        // SAFETY: 同上。
        let Ok(ctrl) = (unsafe { sessions.GetSession(i) }) else {
            continue;
        };
        let Ok(ctl2) = ctrl.cast::<IAudioSessionControl2>() else {
            continue;
        };
        // SAFETY: ctl2 有效。
        let Ok(pid) = (unsafe { ctl2.GetProcessId() }) else {
            continue;
        };
        if pid == 0 || pid == std::process::id() {
            continue;
        }
        if !seen_pids.insert(pid) {
            continue; // pid 去重：同一进程的多个音频会话只查一次镜像名
        }
        if Instant::now() >= deadline || images.len() >= limits::ENUM_MAX_LOOKUPS {
            over_budget = true;
            crate::log_warn!(
                "枚举反查超上限：会话={visited} 进程={}（上限 {} / {}）",
                images.len(),
                limits::ENUM_MAX_AUDIO_SESSIONS,
                limits::ENUM_MAX_LOOKUPS
            );
            break;
        }
        let name = crate::media::launch::process_image_name(pid);
        if name.is_empty() {
            continue; // 权限 / 进程已退出 → 该进程降级，不影响其它
        }
        images.push((pid, name));
    }
    (images, over_budget)
}

/// 当前时刻的 epoch 毫秒（`at` 字段；JS 用 `new Date(at)` 格式化）。
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

    /// 空黑名单的共享索引（G7：PollState::new 需要注入）。
    fn test_index() -> Arc<MediaIndex> {
        MediaIndex::new(&crate::config::MediaConfig::default())
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
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
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
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
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
            let mut st = PollState::new(
                Arc::new(AtomicBool::new(false)),
                MediaIndex::new(&crate::config::MediaConfig::default()),
            );
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

// ============================================================================
// G7 回归（v1.9.0）—— 忽略态状态机 / Cadence::consume / 命中日志限流
// ============================================================================
#[cfg(test)]
mod g7_blacklist_regression {
    use super::*;

    /// `Instant` 的相对工具：全部由参数注入，**不 sleep**（与上层 tests 同款）。
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// 空黑名单的共享索引。
    fn test_index() -> Arc<MediaIndex> {
        MediaIndex::new(&crate::config::MediaConfig::default())
    }

    /// **关键回归（主理人红线）**：`Cadence::consume` 只推进三层时间戳，
    /// 绝不污染退避状态机（`zero_streak` / `timeline_backoff` / prev_*）——
    /// 被拉黑应用的 ignored 专用轮次靠它免于每 50ms 发一次 GetCurrentSession，
    /// 且解绑后网易云式退避必须还是进入忽略前的那个状态。
    #[test]
    fn cadence_consume_only_advances_timestamps_and_never_touches_backoff() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        // 前置：把退避状态机喂成「已进入 5s 退避 + prev_timeline 有值 + prev_state 有值」
        c.note_playback(t0, PlaybackState::Playing);
        for i in 0..limits::MEDIA_POLL_ZERO_STREAK {
            c.note_timeline(at(t0, i as u64 * 1_000), 0, 0);
        }
        assert!(c.timeline_backoff, "前置：网易云已进入退避");
        assert_eq!(c.zero_streak, limits::MEDIA_POLL_ZERO_STREAK);
        let prev_tl = c.prev_timeline;
        let prev_st = c.prev_state;

        // 模拟忽略态连续 200 轮 consume（≈ 全拉黑 + 高频事件下 10s 的 forced 轮次）
        for i in 0..200u32 {
            let now = at(t0, 10_000 + i as u64 * 50);
            c.consume(now, Plan::full());
            assert_eq!(c.zero_streak, limits::MEDIA_POLL_ZERO_STREAK, "zero_streak 不许被动");
            assert!(c.timeline_backoff, "timeline_backoff 不许被动");
            assert_eq!(c.prev_timeline, prev_tl, "prev_timeline 不许被动");
            assert_eq!(c.prev_state, prev_st, "prev_state 不许被动");
        }

        // 时间戳被推进 → plan 不再「永远到点」
        let last_consume = at(t0, 10_000 + 199 * 50);
        let plan = c.plan(at(t0, 10_400), false);
        assert!(!plan.any(), "consume 之后 plan 必须空闲（否则每 50ms 发一次 GetCurrentSession）");
        let _ = last_consume;

        // 解绑后真实事件到达 → note_timeline 照常恢复（退避状态机未被破坏）
        assert!(
            c.note_timeline(at(t0, 20_000), 5_000, 180_000),
            "非零时间线 → 立即补读属性（正常恢复路径）"
        );
        assert!(!c.timeline_backoff, "解绑后出现非零值 → 退避解除");
    }

    /// `consume(full)` 后紧接的 `plan` 必须三组都不读；`consume` 不产生任何跃迁信号。
    #[test]
    fn cadence_consume_makes_plan_idle_without_refill_signals() {
        let mut c = Cadence::default();
        let t0 = Instant::now();
        c.note_playback(t0, PlaybackState::Playing);
        c.note_timeline(t0, 1_000, 180_000);
        c.note_props(t0);
        let t1 = at(t0, 100);
        // 忽略态一轮：三组全 consume（forced 全量计划）
        c.consume(t1, Plan::full());
        let p = c.plan(t1, false);
        assert!(!p.playback && !p.timeline && !p.props, "consume 后三组都该空闲");
        let t2 = at(t0, 600);
        let p = c.plan(t2, false);
        assert!(p.playback, "500ms 后快层照常到点（节奏推进是它唯一的职责）");
    }

    /// `ignored_now` 在 parts 为空（无会话）时绝不命中；
    /// 黑名单配置变化（index.set）后同一份 parts 立即翻转。
    #[test]
    fn ignored_now_is_empty_parts_safe_and_follows_config() {
        let idx = MediaIndex::new(&crate::config::MediaConfig::default());
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), idx);
        assert!(!st.ignored_now(), "空 parts（无会话）不许命中");

        // 模拟「上一轮缓存的身份」是被拉黑应用
        st.parts.aumid = "NeteaseCloudMusic!App".into();
        st.parts.process_name = "cloudmusic.exe".into();
        assert!(!st.ignored_now(), "黑名单为空 → 不命中");

        // 托盘拉黑 → index.set 整组替换 → 下一轮主循环立即识别
        let mut cfg = crate::config::MediaConfig::default();
        cfg.blacklist = vec!["cloudmusic.exe".into()];
        st.index.set(&cfg);
        assert!(st.ignored_now(), "拉黑后 cached parts 必须立即命中（纯内存）");
        assert!(st.index.matches("cloudmusic.exe", "NeteaseCloudMusic!App"));
        // 解绑（清空）→ 立即恢复
        st.index.set(&crate::config::MediaConfig::default());
        assert!(!st.ignored_now(), "解绑后必须立即不命中（≤1s 恢复的前提）");
    }

    /// 命中日志限流：同进程名 60s 内只打一条；进程名变了必然过限流。
    #[test]
    fn hit_log_is_throttled_by_process_name_for_60s() {
        let idx = MediaIndex::new(&crate::config::MediaConfig::default());
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), idx);
        let t0 = Instant::now();
        assert!(st.hit_log_due("cloudmusic.exe", t0), "首条必打");
        assert!(!st.hit_log_due("cloudmusic.exe", at(t0, 59_999)), "59.9s 内不许重打");
        assert!(st.hit_log_due("cloudmusic.exe", at(t0, 60_000)), "满 60s 重打");
        assert!(st.hit_log_due("msedge.exe", at(t0, 61_000)), "换了进程名必打");
        assert!(!st.hit_log_due("msedge.exe", at(t0, 62_000)));
        assert_eq!(
            limits::HIT_LOG_WINDOW_MS,
            60_000,
            "限流窗口常量钉死（与既有 60s 统计窗口同口径）"
        );
    }

    /// `IGNORE_RECHECK_MS` 与既有轮询常量并列钉死（防止被顺手改掉）。
    #[test]
    fn ignore_recheck_constant_is_pinned() {
        assert_eq!(IGNORE_RECHECK_MS, 1_000);
        assert_eq!(FORCED_FLOOR_MS, 60);
        // 枚举预算常量（数值唯一定义处在 limits）
        assert_eq!(limits::ENUM_LOOKUP_BUDGET_MS, 120);
        assert_eq!(limits::ENUM_TOTAL_BUDGET_MS, 200);
        assert_eq!(limits::ENUM_MAX_LOOKUPS, 16);
        assert_eq!(limits::ENUM_MAX_AUDIO_SESSIONS, 64);
    }
}

// ============================================================================
// G8-①（v1.9.1）—— 黑名单多会话：挑正常会话显示，而不是把整个小组件隐藏
//
// 全部测试都跑在纯数据上：候选画像 + 黑名单判定结果 + 播放状态，零 COM、
// 零 WinRT、零定时器。挑选规则与状态机因此可确定性回归。
// ============================================================================
#[cfg(test)]
mod g8_multi_session {
    use super::*;

    /// 造一个候选：身份（AUMID / 进程名）+ 播放状态 + 黑名单判定结果。
    fn cand(aumid: &str, playing: bool, ignored: bool) -> SessionCandidate {
        SessionCandidate {
            aumid: aumid.to_string(),
            process_name: player_map::resolve(aumid).process_name.to_string(),
            playing,
            ignored,
        }
    }

    /// 带真实黑名单配置的索引：候选的 `ignored` 由 `MediaIndex::matches` 实算，
    /// 保证测试用的是与生产完全同源的三级回退判定，不是手写布尔。
    fn blacklist_index(entries: &[&str]) -> Arc<MediaIndex> {
        let mut cfg = crate::config::MediaConfig::default();
        cfg.blacklist = entries.iter().map(|s| s.to_string()).collect();
        MediaIndex::new(&cfg)
    }

    fn measured_candidates(
        index: &MediaIndex,
        aumids: &[(&str, bool)],
    ) -> Vec<SessionCandidate> {
        aumids
            .iter()
            .map(|(aumid, playing)| {
                let process = player_map::resolve(aumid).process_name;
                SessionCandidate {
                    aumid: (*aumid).to_string(),
                    process_name: process.to_string(),
                    playing: *playing,
                    ignored: index.matches(process, aumid),
                }
            })
            .collect()
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    // ------------------------------------------------ 会话挑选纯函数（约束 6）

    /// ① 系统当前会话被拉黑、同时存在「正在播放」的正常会话 → 选中并显示它。
    #[test]
    fn choose_session_shows_playing_normal_when_current_is_blacklisted() {
        let candidates = vec![
            cand("NeteaseCloudMusic!App", true, true), // 系统当前会话：被拉黑、正在播放
            cand("Spotify.exe", false, false),         // 正常但暂停
            cand("QQMusic.exe", true, false),          // 正常且正在播放
        ];
        assert_eq!(choose_session(&candidates), SessionChoice::Show(2));
        assert_eq!(
            scan_decision(&candidates, None),
            ScanDecision::Switch(2),
            "状态机必须据此切换到那个正常会话"
        );
    }

    /// ② 多个正常会话 → 优先正在播放（Playing）的那个。
    #[test]
    fn choose_session_prefers_playing_among_multiple_normal_sessions() {
        let candidates = vec![
            cand("Spotify.exe", false, false),
            cand("QQMusic.exe", false, false),
            cand("Kugou.exe", true, false),
            cand("Foobar2000.exe", false, false),
        ];
        assert_eq!(choose_session(&candidates), SessionChoice::Show(2));

        // 系统顺序里有两个 Playing 时取第一个（顺序可预测，不随 HashMap 漂移）
        let two_playing = vec![
            cand("Spotify.exe", true, false),
            cand("QQMusic.exe", true, false),
        ];
        assert_eq!(choose_session(&two_playing), SessionChoice::Show(0));
    }

    /// ③ 全部暂停 → 按 `GetSessions()` 的系统返回顺序取第一个正常会话。
    #[test]
    fn choose_session_takes_first_in_system_order_when_all_paused() {
        let candidates = vec![
            cand("Spotify.exe", false, false),
            cand("QQMusic.exe", false, false),
            cand("Kugou.exe", false, false),
        ];
        assert_eq!(choose_session(&candidates), SessionChoice::Show(0));

        // 第一个是暂停、后面也全暂停，但前面插了一个被拉黑的：跳过它
        let with_blacklisted_first = vec![
            cand("NeteaseCloudMusic!App", false, true),
            cand("Spotify.exe", false, false),
            cand("QQMusic.exe", false, false),
        ];
        assert_eq!(
            choose_session(&with_blacklisted_first),
            SessionChoice::Show(1)
        );
    }

    /// ④ 全部候选都被拉黑 / 没有候选 → 保持隐藏（沿用现状空态）。
    #[test]
    fn choose_session_hides_when_all_candidates_blacklisted_or_empty() {
        let all_blacklisted = vec![
            cand("NeteaseCloudMusic!App", true, true),
            cand("Spotify.exe", false, true),
        ];
        assert_eq!(choose_session(&all_blacklisted), SessionChoice::Hide);
        assert_eq!(choose_session(&[]), SessionChoice::Hide);
        assert_eq!(
            scan_decision(&all_blacklisted, None),
            ScanDecision::Hide,
            "确实一个正常会话都没有时才隐藏"
        );
    }

    /// ⑤ 被拉黑的应用仍被完全忽略：即使它在播放、排在系统顺序第一位，
    ///    也绝不会被选中（用真实 `MediaIndex::matches` 判定，不是手写布尔）。
    #[test]
    fn blacklisted_app_is_never_selected_even_while_playing() {
        let index = blacklist_index(&["cloudmusic.exe"]);
        let candidates = measured_candidates(
            &index,
            &[
                ("NeteaseCloudMusic!App", true), // 被拉黑 + 正在播放
                ("Spotify.exe", false),          // 正常 + 暂停
            ],
        );
        assert!(candidates[0].ignored, "前置：网易云确实命中黑名单");
        assert!(!candidates[1].ignored, "前置：Spotify 不在黑名单");
        assert_eq!(choose_session(&candidates), SessionChoice::Show(1));
        assert_ne!(choose_session(&candidates), SessionChoice::Show(0));

        // 只留下被拉黑的应用 → 隐藏，绝不「退而求其次」显示它
        let only_blacklisted = measured_candidates(&index, &[("NeteaseCloudMusic!App", true)]);
        assert_eq!(choose_session(&only_blacklisted), SessionChoice::Hide);
    }

    // ------------------------------------------------ 性能红线（约束 4、6）

    /// ⑥ 正常播放路径不触发枚举：`blacklist_hit == false` 时，无论距上次扫描
    ///    多久、无论是否从未扫描过，都不允许发起 `GetSessions()`。
    #[test]
    fn normal_playback_path_never_triggers_session_enumeration() {
        let t0 = Instant::now();
        assert!(!session_scan_due(false, None, t0), "从未扫描过也不许扫");
        assert!(!session_scan_due(false, Some(t0), at(t0, 999)));
        assert!(!session_scan_due(false, Some(t0), at(t0, 1_000)));
        assert!(!session_scan_due(false, Some(t0), at(t0, 60_000)));
        assert!(!session_scan_due(false, None, at(t0, 3_600_000)));
        // 对照：命中黑名单时首次立即放行
        assert!(session_scan_due(true, None, t0));
    }

    /// 命中后的枚举被 1s 闸限流：每秒最多一次，绝不进每 50ms 的常规 tick。
    #[test]
    fn session_scan_due_throttles_rescans_to_one_second() {
        let t0 = Instant::now();
        let last = Some(t0);
        assert!(!session_scan_due(true, last, at(t0, 50)));
        assert!(!session_scan_due(true, last, at(t0, 999)));
        assert!(session_scan_due(true, last, at(t0, 1_000)));
        assert!(session_scan_due(true, last, at(t0, 5_000)));
    }

    // ------------------------------------------------ 多会话状态机

    /// 命中黑名单 + 存在 Playing 的正常会话 → 状态机决定「切换」而非隐藏。
    #[test]
    fn decide_scan_switches_to_normal_session_instead_of_hiding() {
        let candidates = vec![
            cand("NeteaseCloudMusic!App", true, true),
            cand("Spotify.exe", false, false),
            cand("QQMusic.exe", true, false),
        ];
        assert_eq!(scan_decision(&candidates, None), ScanDecision::Switch(2));
        let st = PollState::new(
            Arc::new(AtomicBool::new(false)),
            blacklist_index(&["cloudmusic.exe"]),
        );
        assert_eq!(
            st.decide_scan(&candidates),
            ScanDecision::Switch(2),
            "PollState 的接线必须走同一份纯决策"
        );
    }

    /// 仍在显示同一个正常会话 → Keep：不清缓存、不制造每秒重播。
    #[test]
    fn decide_scan_keeps_the_same_displayed_session() {
        let candidates = vec![
            cand("NeteaseCloudMusic!App", true, true),
            cand("QQMusic.exe", true, false),
        ];
        assert_eq!(
            scan_decision(&candidates, Some("QQMusic.exe")),
            ScanDecision::Keep(1)
        );
        // 换成另一个正常会话 → 必须切（防串歌）
        assert_eq!(
            scan_decision(&candidates, Some("Spotify.exe")),
            ScanDecision::Switch(1)
        );
    }

    /// 只有确实一个正常会话都没有时才 Hide；有正常会话时绝不 Hide。
    #[test]
    fn decide_scan_hides_only_when_no_normal_session_exists() {
        let all_blacklisted = vec![
            cand("NeteaseCloudMusic!App", true, true),
            cand("Spotify.exe", false, true),
        ];
        assert_eq!(scan_decision(&all_blacklisted, None), ScanDecision::Hide);
        assert_eq!(scan_decision(&[], Some("QQMusic.exe")), ScanDecision::Release);

        let with_normal = vec![
            cand("NeteaseCloudMusic!App", true, true),
            cand("Spotify.exe", false, false),
        ];
        assert_ne!(scan_decision(&with_normal, None), ScanDecision::Hide);
    }

    /// 黑名单会话全部消失 → Release：回到常规 `GetCurrentSession()` 路径，
    /// 枚举彻底停止（正常播放路径零新增跨进程调用）。
    #[test]
    fn decide_scan_releases_to_normal_path_when_blacklisted_session_gone() {
        let candidates = vec![
            cand("QQMusic.exe", true, false),
            cand("Spotify.exe", false, false),
        ];
        assert_eq!(scan_decision(&candidates, Some("QQMusic.exe")), ScanDecision::Release);
        assert_eq!(scan_decision(&candidates, None), ScanDecision::Release);
        // 与被拉黑会话共存的场景不会误判成 Release
        let mixed = vec![
            cand("NeteaseCloudMusic!App", true, true),
            cand("QQMusic.exe", true, false),
        ];
        assert_ne!(scan_decision(&mixed, None), ScanDecision::Release);
    }

    /// 综合真值表：被拉黑应用 + 若干正常候选的组合下，挑选结果始终等于
    /// 「跳过 ignored → 优先 Playing → 否则系统顺序第一个」这一独立期望。
    #[test]
    fn choose_session_matches_independent_reference_over_all_combinations() {
        fn expected(cs: &[SessionCandidate]) -> Option<usize> {
            cs.iter()
                .position(|c| !c.ignored && c.playing)
                .or_else(|| cs.iter().position(|c| !c.ignored))
        }
        let aumids = ["A!App", "B.exe", "C.exe", "D.exe"];
        let mut cells = 0;
        for mask in 0u8..16 {
            for playing_mask in 0u8..16 {
                let cs: Vec<SessionCandidate> = aumids
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        cand(
                            a,
                            playing_mask & (1 << i) != 0,
                            mask & (1 << i) != 0,
                        )
                    })
                    .collect();
                let want = match expected(&cs) {
                    Some(i) => SessionChoice::Show(i),
                    None => SessionChoice::Hide,
                };
                assert_eq!(choose_session(&cs), want, "mask={mask} playing={playing_mask}");
                cells += 1;
            }
        }
        assert_eq!(cells, 256);
    }

    // ------------------------------------------------ P2-1：Release 清残留忽略态

    /// 假 `EventSink`：句柄值不存在，`PostMessageW` 失败被忽略，但事件仍进
    /// mpsc 通道，于是「到底广播了什么」可以在无 COM 的单测里直接断言。
    fn fake_sink() -> (EventSink<AppEvent>, std::sync::mpsc::Receiver<AppEvent>) {
        use crate::platform::wake::WakeHwnd;
        use windows::Win32::Foundation::HWND;
        let hwnd = HWND(0x1234_usize as *mut core::ffi::c_void);
        EventSink::new(WakeHwnd(hwnd))
    }

    fn stale_ignored() -> IgnoredSessionInfo {
        IgnoredSessionInfo {
            process_name: "cloudmusic.exe".to_string(),
            aumid: "NeteaseCloudMusic!App".to_string(),
            app_name: "网易云音乐".to_string(),
        }
    }

    /// 纯数据判定：只有「确实是 Release」且「确有残留忽略态」才需要广播清理；
    /// 其它决策动作一律不许借 Release 的名义发 `IgnoredSession(None)`。
    #[test]
    fn release_needs_ignored_clear_truth_table() {
        assert!(release_needs_ignored_clear(ScanDecision::Release, true));
        assert!(!release_needs_ignored_clear(ScanDecision::Release, false));
        assert!(!release_needs_ignored_clear(ScanDecision::Switch(0), true));
        assert!(!release_needs_ignored_clear(ScanDecision::Keep(0), true));
        assert!(!release_needs_ignored_clear(ScanDecision::Hide, true));
    }

    /// **P2-1 回归**：唯一被拉黑应用 A 退出、正常应用 B 接管 → Release 必须清
    /// `last_ignored` 并广播 `IgnoredSession(None)`，让托盘口径立刻切回显示的 B。
    #[test]
    fn release_clears_stale_ignored_state_and_broadcasts_none() {
        let (sink, rx) = fake_sink();
        let mut st = PollState::new(
            Arc::new(AtomicBool::new(false)),
            blacklist_index(&["cloudmusic.exe"]),
        );
        st.last_ignored = Some(stale_ignored());
        // 模拟 Release 之前的状态：还有快照 / 缓存身份
        st.parts.aumid = "NeteaseCloudMusic!App".to_string();

        st.apply_release(ScanDecision::Release, &sink);

        assert!(st.last_ignored.is_none(), "UI 的 App.ignored 残留必须被清掉");
        assert!(st.display_session.is_none(), "回到常规 GetCurrentSession 路径");
        assert!(st.parts.aumid.is_empty(), "缓存身份不许再被误判为命中");
        assert!(st.last.is_none());

        let events: Vec<AppEvent> = rx.try_iter().collect();
        assert!(
            matches!(events.as_slice(), [AppEvent::IgnoredSession(None)]),
            "必须恰好广播一条 IgnoredSession(None)，实际 {events:?}"
        );
    }

    /// 无残留忽略态时不发事件：Release 每 1s 可能重复走到，不该刷 UI。
    #[test]
    fn release_without_stale_ignored_state_does_not_broadcast() {
        let (sink, rx) = fake_sink();
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), blacklist_index(&[]));
        st.apply_release(ScanDecision::Release, &sink);
        assert!(st.last_ignored.is_none());
        assert!(
            rx.try_iter().next().is_none(),
            "没有残留忽略态时不该广播任何事件"
        );
    }

    /// 非 Release 决策即便有残留忽略态也不该由 Release 路径发清理事件。
    #[test]
    fn release_clear_guard_rejects_non_release_decisions() {
        let (sink, rx) = fake_sink();
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), blacklist_index(&[]));
        st.last_ignored = Some(stale_ignored());
        st.apply_release(ScanDecision::Switch(1), &sink);
        assert!(rx.try_iter().next().is_none());
    }

    // ------------------------------------------------ P2-2：常规播放性能闸

    /// **P2-2 核心断言**：常规播放路径（`false,false`）绝不允许进入会话枚举。
    /// 这格是 QA RV4 把闸改成恒 `true` 后唯一失守、却原本没有单测锁住的格子。
    #[test]
    fn scan_gate_truth_table_locks_normal_playback_path() {
        assert!(
            !scan_gate(false, false),
            "常规播放路径必须零新增跨进程调用（约束 4）"
        );
        assert!(scan_gate(true, false), "命中黑名单 → 允许低频枚举");
        assert!(scan_gate(false, true), "正显示替代会话 → 允许低频复核");
        assert!(scan_gate(true, true));
    }

    /// 闸与 1s 时间闸的联动：常规播放永不枚举；首次命中立即放行；
    /// 命中但 1s 内已扫过则被节流（不进每 50ms 常规 tick）。
    #[test]
    fn scan_gate_composes_with_session_scan_due() {
        let t0 = Instant::now();
        assert!(!(scan_gate(false, false) && session_scan_due(true, None, t0)));
        assert!(
            scan_gate(false, true) && session_scan_due(true, None, t0),
            "锁定替代会话时允许首轮枚举复核"
        );
        assert!(scan_gate(true, false) && session_scan_due(true, None, t0));
        assert!(!(scan_gate(true, false) && session_scan_due(true, Some(t0), at(t0, 50))));
        assert!(scan_gate(true, false) && session_scan_due(true, Some(t0), at(t0, 1_000)));
    }
}
