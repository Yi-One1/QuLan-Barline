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
//!
//! ## 歌词钟连续性（v1.9.3 / G8-④）
//!
//! 网易云恒报 `(position=0, duration=0)`，歌词钟完全靠「锚点 + 1:1 走表」
//! 自维护，所以**钟的连续性**是这一层的核心不变量，三条铁律：
//! 1. **位置与锚点严格配对**：`parts.timeline_at` 只能是
//!    `parts.position_ms` 被采到的时刻。`(0,0)` 读数不许刷新它
//!    （否则就是「陈旧位置 @ 新鲜时刻」，任一次并发发布都会把钟整段拽回去）；
//! 2. **跃迁锚在跃迁时刻**，不是「检测到它的那一轮」（`TickInputs::jumped_at`
//!    来自 `PlaybackInfoChanged` 的到达时刻，见 [`transition_at`]）——暂停与
//!    继续两个方向同源，单轮偏差相等才能逐轮抵消；
//! 3. **离开一首歌要把钟记下来**（[`LyricClockMemory`]）：跨应用 / 跨会话 /
//!    同 App 换歌再换回来时，只要本轮拿不到可信位置就续接记忆里的位置，
//!    而不是从 0 重唱；真正没听过的歌仍然 `Reset` 归零。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
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
    /// `MediaIndex::is_filtered()` 的结果；`true` = 不该接管（拉黑 **或** 白名单外），
    /// 必须完全忽略、永不选中。
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
/// **规则 3 只是冷启动兜底**（G8-⑥）：真正调用本函数的 [`scan_decision`] 会在
/// 进入规则 3 之前先用 [`sticky_index`] 粘住上次显示的会话——否则「用户按下暂停」
/// 会被误判成「全都暂停，随便挑一个」，小组件漂移到别的应用上。所以本函数的规则 3
/// 只在上次显示的会话**已经消失**（应用退出 / 会话注销）时才会被用到。
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

/// 平局时「粘住上次显示的那个正常会话」的下标（G8-⑥ / v1.14.0）。
///
/// **为什么必须有**：`choose_session` 的规则③在「全都暂停」时取 `GetSessions()`
/// 系统顺序里第一个正常会话，而它**无法区分**「用户刚按下暂停」和「这首歌早就播完
/// 了、只是会话还挂着」。暂停当前播放器的那一刻恰好落进规则③，于是小组件漂移到
/// 一个从未播放的应用上（真机日志：汽水暂停后 0.5s 切到 `index=1` 的网易云，
/// `playing=false`），连播控也跟着错位——按播放键恢复不了刚才暂停的那首歌。
///
/// 判据只认「同一 AUMID **且未被过滤**」：
/// - `!c.ignored` 前置不可省——否则等于给黑名单开后门（被拉黑的会话绝不许借粘性复活）；
/// - 上次显示的会话已消失 → 返回 `None`，由 `choose_session` 兜底，
///   于是「关掉汽水」仍然会正常跟随离开，不会粘在幽灵上。
pub fn sticky_index(
    candidates: &[SessionCandidate],
    displayed_aumid: Option<&str>,
) -> Option<usize> {
    let shown = displayed_aumid.filter(|s| !s.is_empty())?;
    candidates
        .iter()
        .position(|c| !c.ignored && c.aumid == shown)
}

/// 一次会话枚举 → 媒体线程的状态动作（G8-①，**纯函数**，零 COM / 零 IO）。
///
/// `displayed_aumid` = 小组件当前锁定显示的那个会话（`None` = 常规路径）。
///
/// **优先级次序**（G8-⑥ 修正，本身就是这个 bug 的本质——原来缺了第 3 条）：
/// 1. 黑名单会话已全部消失 → [`ScanDecision::Release`]：回到常规路径，枚举停止；
/// 2. 存在正常会话正在播放 → 走 [`choose_session`] 的规则②（合法切换必须保留：
///    用户真在网易云点了播放，就该切过去）；
/// 3. **没有正常会话在播 + 上次显示的那个仍在候选里 → [`ScanDecision::Keep`]**：
///    平局不漂移，`Keep` 只刷新 COM 句柄，不清缓存、不重播、不动歌词钟；
/// 4. 否则 → 走 [`choose_session`] 剩下的规则（系统顺序第一个正常会话 /
///    全被过滤则 [`ScanDecision::Hide`]）。规则③退化为**冷启动兜底**：
///    只在上次显示的会话已经消失时才会用到。
pub fn scan_decision(
    candidates: &[SessionCandidate],
    displayed_aumid: Option<&str>,
) -> ScanDecision {
    if !candidates.iter().any(|c| c.ignored) {
        return ScanDecision::Release;
    }
    // 优先级的**中段**：没有正常会话在播时，不许离开上次显示的那个。
    // 有正常会话在播时一律放行给 `choose_session`（规则②），
    // 否则「用户真的切到另一个播放器」会被粘性永久拒绝。
    let has_playing_normal = candidates.iter().any(|c| !c.ignored && c.playing);
    if !has_playing_normal {
        if let Some(i) = sticky_index(candidates, displayed_aumid) {
            return ScanDecision::Keep(i);
        }
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
/// `filtered` = 缓存身份命中过滤判据（拉黑 ∪ 白名单外）；`has_displayed_session` = 正锁定显示
/// `GetSessions()` 挑出的替代会话。只有这两者之一成立才允许进入低频会话枚举；
/// **`false, false`（常规播放路径）必须返回 `false`** —— 于是 `GetSessions()`
/// 永不进每 50ms 的常规 tick，零新增跨进程调用。抽成纯函数是为了让这条红线
/// 被单测锁住，而不是只靠代码走查。
pub fn scan_gate(filtered: bool, has_displayed_session: bool) -> bool {
    filtered || has_displayed_session
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

    /// 上一次快层观测的时刻（G8-④：跃迁时刻的陈旧性判据）。
    ///
    /// 必须在调用 [`Cadence::note_playback`] **之前**取：`note_playback` 会把它
    /// 推到 `now`，之后就拿不到「上一次确认旧状态是什么时候」了。
    fn last_playback_obs(&self) -> Option<Instant> {
        self.last_playback
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
/// 1. 本轮**读过时间线** → 用该位置**被采到的时刻**（`timeline_at`）。
///    ⚠️ v1.9.3 起它不再等于「本轮 `now`」：`(0,0)` 读数不许刷新它
///    （见 [`PollState::apply_timeline_read`]），所以它可能早于 `now`。
///    规则本身没变，变的只是「它装的是什么」——现在它永远与
///    `parts.position_ms` 严格配对，不会出现「陈旧位置 @ 新鲜时刻」；
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

// ---------------------------------------------------------------- 歌词钟续接（G8-②/G8-④）

/// G8-②：一次快照发布前，对「歌词钟」（`position_ms` / `position_at`）的动作。
///
/// 全部是纯数据，不含 COM 句柄：`PollState::build_snapshot` 据此改写快照，
/// 于是「暂停→继续不归零」的接线可以被确定性单测锁住。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricClockAction {
    /// 维持 `parts` / [`anchor_of`] 的结果（正常播放器、首帧、稳态轮次）。
    Keep,
    /// 切歌：恒报 `(0,0)` 的播放器歌词钟归零，锚点仍交给 [`anchor_of`] 规则 2/3。
    Reset,
    /// 状态跃迁：从上一份已广播快照续接歌词钟，不归零、不重载歌词。
    Carry,
}

/// G8-②：`tick()` 交给 [`PollState::build_snapshot`] 的**全部本轮输入**（纯数据）。
///
/// 把这些输入收拢成一个结构，是为了让「`tick()` 里怎么赋值 / 怎么传参」这段
/// 无法用 COM 单测覆盖的接线只留**一个**入口：`build_snapshot` 只接受
/// [`TickInputs`]，测试既可以用 [`tick_inputs`]（生产同款构造）也可以用字面量
/// 直接构造（QA 的 RV-C3 教训：把 `state_jumped = true` 改成 `false` 后
/// 592 条测试全绿 —— 输入散落成三个实参时无人锁定赋值与传参）。
///
/// `read_timeline_now` **由 `timeline_read.is_some()` 派生**（`tick_inputs` 是
/// 唯一的生产构造入口），不再单独传递，避免「本轮读过时间线」出现两个来源。
/// 字段设为 `pub` 只为测试能用字面量精确构造不一致组合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickInputs {
    /// [`Cadence::note_playback`] 判出的播放状态跃迁。
    pub state_jumped: bool,
    /// 本轮时间线读数（`None` = 本轮没读中层）。
    pub timeline_read: Option<(Ms, Ms)>,
    /// 本轮是否读过时间线（`anchor_of` 规则 1 的依据）。恒等于
    /// `timeline_read.is_some()`；由 [`tick_inputs`] 派生，测试可直接给。
    pub read_timeline_now: bool,
    /// G8-④：本轮跃迁**真正发生**的时刻（见 [`transition_at`]）。
    /// `None` = 不是跃迁轮次（或没有事件时刻可用，退回本轮 `now`）。
    pub jumped_at: Option<Instant>,
}

/// G8-②：`TickInputs` 的**唯一生产构造入口**（纯函数）。
///
/// 生产代码只经这里构造，`read_timeline_now` 一律由 `timeline_read.is_some()`
/// 派生 —— 真实读数与 `(0,0)` 都算「这轮读过时间线」（锚点规则 1 依赖它：
/// 读过就以该次读取时刻为锚点，切歌则由规则 2 覆盖）。
///
/// G8-④：跃迁时刻另走 [`TickInputs::at`]，**不塞进这里的第二个实参**——
/// `tick_wires_state_jump_into_snapshot_inputs` 逐字断言 `tick()` 里出现
/// `tick_inputs(state_jumped, timeline_read)`，把 `jumped_at` 塞进签名会让那条
/// 接线断言失效。
pub fn tick_inputs(state_jumped: bool, timeline_read: Option<(Ms, Ms)>) -> TickInputs {
    TickInputs {
        state_jumped,
        timeline_read,
        read_timeline_now: timeline_read.is_some(),
        jumped_at: None,
    }
}

impl TickInputs {
    /// G8-④：补上「跃迁真正发生的时刻」（唯一生产调用点在 `tick()` 里）。
    ///
    /// 传 `None` 等价于旧行为（用本轮 `now` 当跃迁时刻）。
    pub fn at(mut self, jumped_at: Option<Instant>) -> Self {
        self.jumped_at = jumped_at;
        self
    }
}

/// G8-②：本轮要不要改写新快照的 `(position_ms, position_at)`（**纯函数**）。
///
/// `timeline` = 本轮时间线读数（`None` = 本轮没读）；`zero_duration` =
/// `parts.duration_ms == 0`（播放器从不报时长，网易云式特征）；`state_jumped` =
/// [`Cadence::note_playback`] 判出的播放状态跃迁；`track_changed` = `track_id`
/// 相对上一份快照变了；`has_prev` = 已有上一份已广播快照。
///
/// 「没拿到可信位置」= **本轮读到的位置是 0**（`(0,0)` 与 `(0,真实时长)` 都算：
/// 时长有值不代表位置已上报，判据必须是位置本身），或本轮压根没读时间线且
/// 播放器从不报时长（5s 退避里由慢层发现切歌的场景）。后者若漏掉，换歌时会把
/// 上一首续接的歌词钟带进新歌。
///
/// 优先级（背景：网易云恒报 `(0,0)`，那个读数**不是**有效位置，程序实际靠
/// `position_at` 线性外推维持歌词钟。旧实现状态跃迁时立即补读时间线，把
/// `position_ms` 写成 0、锚点挪到 `now`，新快照一发布歌词就跳回第一句）：
/// 1. 拿到可信的真实位置读数 → [`LyricClockAction::Keep`]（汽水等原样）；
/// 2. 切歌 → [`LyricClockAction::Reset`]（新歌从头走，绝不被续接误伤）；
/// 3. 首帧无上一份快照 → [`LyricClockAction::Keep`]（沿用 `anchor_of` 兜底）；
/// 4. 状态跃迁 → [`LyricClockAction::Carry`]；
/// 5. 其余 → [`LyricClockAction::Keep`]。
pub fn lyric_clock_action(
    timeline: Option<(Ms, Ms)>,
    zero_duration: bool,
    state_jumped: bool,
    track_changed: bool,
    has_prev: bool,
) -> LyricClockAction {
    let no_usable_position = match timeline {
        // v1.9.3 / G8-⑤：判据是「位置是否为 0」，不是「时长有没有值」——
        // `(0, 真实时长)` 是「时长已知、位置还没上报」，不是可信位置。
        Some((position, _)) => position == 0,
        None => zero_duration,
    };
    if !no_usable_position {
        return LyricClockAction::Keep;
    }
    if track_changed {
        return LyricClockAction::Reset;
    }
    if !has_prev {
        return LyricClockAction::Keep;
    }
    if state_jumped {
        LyricClockAction::Carry
    } else {
        LyricClockAction::Keep
    }
}

/// G8-②：上一份已广播快照在 `now` 时刻的歌词钟位置（**纯函数**，零 COM）。
///
/// 与 [`NowPlaying::position_now`] 同口径，但时间由参数注入，单测不需 sleep：
/// - `Playing` → `position_ms + (now - position_at)`（线性外推）；
/// - 其它态（暂停 / 兜底）→ 冻结的 `position_ms`（暂停期间歌词原地停住）。
///
/// `saturating_*` 保证时钟回拨 / 异常数值都不 panic。
pub fn lyric_clock_at(prev: &NowPlaying, now: Instant) -> Ms {
    if prev.state != PlaybackState::Playing {
        return prev.position_ms;
    }
    let elapsed = now.saturating_duration_since(prev.position_at).as_millis() as Ms;
    prev.position_ms.saturating_add(elapsed)
}

// ------------------------------------------------- G8-④：跃迁时刻（v1.9.3）

/// `PlaybackInfoChanged` 的**到达时刻**（媒体线程 ↔ WinRT 回调共享）。
///
/// 为什么需要它：歌词钟对「状态跃迁发生在何时」只能猜。旧实现用「检测到的那
/// 一轮的 `now`」，于是
/// - 暂停方向：时钟会**多走**「真暂停 → 被检测到」这一段（最多一个快层间隔
///   500ms，实测事件驱动下约 60ms）；
/// - 继续方向：时钟**丢掉**同样一段。
///
/// 两者用同一条规则、同一套延迟时逐轮精确抵消（`g8_lyric_clock` 的 N 轮仿真
/// 与用户真机日志双向印证：d≈r≈60ms 时误差恒为 ±2ms）；但只要两个方向的延迟
/// 不等（例如某一个方向的事件没到、只能等轮询），误差就会按 (d−r)/轮 **线性
/// 累加**，表现为「暂停几次之后歌词越来越靠后」。
///
/// 解法是把两个方向都锚到**同一个来源**——事件到达时刻：它是真实的跃迁时刻
/// 加上「事件延迟」，而事件延迟对暂停/继续是同一套回调、同一个量级，逐轮误差
/// 相等 → 逐轮抵消，不再随轮询相位漂。
///
/// 实现代价：回调里只做一次 `Instant::now()` + 一次极短加锁（**零 COM、零
/// 分配、零日志**）；拿不到就退回本轮 `now`，即与旧行为完全一致。
#[derive(Clone, Default)]
struct PlaybackEventClock {
    at: Arc<Mutex<Option<Instant>>>,
}

impl PlaybackEventClock {
    /// 回调侧：记下「此刻刚收到一次 `PlaybackInfoChanged`」。
    fn stamp(&self) {
        self.stamp_at(Instant::now());
    }

    /// 记下指定时刻（单测注入用；生产只经 [`PlaybackEventClock::stamp`]）。
    fn stamp_at(&self, at: Instant) {
        if let Ok(mut slot) = self.at.lock() {
            *slot = Some(at);
        }
    }

    /// 主循环侧：取走「上一次快层观测之后」的跃迁时刻。
    ///
    /// 早于 `after` 的时间戳一律丢弃（那是更早一次跃迁的残留，用它会把时钟
    /// 往错误的方向挪）；无论取舍都消费掉，避免陈旧时间戳反复被误用。
    fn take_after(&self, after: Option<Instant>) -> Option<Instant> {
        let Ok(mut slot) = self.at.lock() else {
            return None;
        };
        let taken = slot.take();
        match (taken, after) {
            // 严格早于上一次观测 = 更早一次跃迁的残留；「恰好相等」要收下：
            // 事件触发的强制读本身会把 `last_playback` 推到自己这个时刻。
            (Some(t), Some(a)) if t < a => None,
            (t, _) => t,
        }
    }
}

/// G8-④：跃迁时刻的取值（**纯函数**）。
///
/// 有事件时刻且不晚于本轮 `now` → 用它；否则退回本轮 `now`（旧行为）。
/// 晚于 `now` 的时间戳只可能来自时钟异常，直接丢弃，绝不让时钟倒退。
fn transition_at(now: Instant, event_at: Option<Instant>) -> Instant {
    match event_at {
        Some(e) if e <= now => e,
        _ => now,
    }
}

// ------------------------------------------------- G8-④：歌词钟记忆（v1.9.3）

/// 一条「这首歌离开时的歌词钟」。
struct ClockMemoryEntry {
    aumid: String,
    track_id: TrackId,
    /// 离开那一刻的歌词钟位置（已按离开时刻外推，**不再按离开时长续推**）
    position_ms: Ms,
    /// 记账时刻（TTL 判据；也是「淘汰最老一条」的判据）
    sampled_at: Instant,
    /// 离开那一刻的播放态：`Paused` = 原样续接；`Playing` = 只是**停车值**。
    state: PlaybackState,
}

/// 这条记忆能不能用来续接（**纯函数**，v1.9.3 / G8-⑤）。
///
/// **只有「离开时暂停」才续接。** 判据来自「宁可落后，绝不超前」这条红线：
///
/// | 离开时 | 再出现最可能是什么 | 续接的后果 |
/// |---|---|---|
/// | `Paused` | 用户把这首歌停在那儿，回头接着听 | 精确（暂停值就是真值）→ **续接** |
/// | `Playing` | ① 被别的应用顶掉 ② 用户切走后**在列表里重新点了这首歌**（网易云点列表 = 从头播）③ 顺序播完后再点（也是从头） | ②③ 会把歌词拨到**还没唱到**的句子，而网易云读不到真实位置、**没有任何自愈机会**，错误会持续整首 |
///
/// ②③ 与 ① 在 `(0,0)` 播放器上**无法区分**（没有任何位置信息），所以只能按
/// 最坏情况处理：`Playing` 离开的记忆一律不续接，退回 `Reset 0`（落后）。
/// 代价写在 `docs`/报告里：**「在播时切走、再切回同一首」这一类丢钟没有修复**
/// （用户日志 02:21:26 那次就属于这一类）。
///
/// 另外 `NoSession` / `Fallback` 也不续接：那不是「这首歌被暂停」，是状态未知。
fn restorable(leave_state: PlaybackState) -> bool {
    leave_state == PlaybackState::Paused
}

/// 一次命中的记忆（只带出必要字段，保持纯数据、可单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoredClock {
    pub position_ms: Ms,
    /// 离开时的播放态（决定「原样续接」还是「按停车值续接」，见 [`LyricClockMemory::restore`]）
    pub leave_state: PlaybackState,
}

/// G8-④：**按 (aumid, track_id) 记住「这首歌离开时的歌词钟」**。
///
/// 恒报 `(0,0)` 的播放器（网易云）读不到真实位置，歌词钟完全靠
/// `position_at` 线性自维护。身份一变（跨应用切走再切回 / 同 App 换歌再换回 /
/// 会话被系统短暂丢弃），`track_changed` 就会走 `Reset` 把钟归零 ——
/// 用户真机日志实证：02:25:19 暂停在 `position=141223`，切到汽水再切回来，
/// 02:26:50 变成 `position=0`，歌词从第一句重唱。
///
/// 存取口径（保守优先，宁慢不快）：
/// 1. **只在「本轮没有任何可信位置」时才被查**：真实上报位置的播放器（汽水）
///    走 `Keep`，一个字节都不经过这里；
/// 2. **绝不按离开时长向前外推，且只在「离开时暂停」时续接**（[`restorable`]）：
///    歌曲被别的应用顶掉 / 用户切走后重新点播时它到底在哪，我们无从得知。
///    宁可停在「离开时的位置」（歌词宁可慢一点点），也不把歌词往前拨——
///    往前拨是「唱到还没播的句子」，比落后更刺眼，而且 `(0,0)` 播放器没有
///    任何自愈机会；
/// 3. 有时效与容量，过期/超容即视为不可信，退回 `Reset`（真新歌仍然从 0 开始）。
#[derive(Default)]
struct LyricClockMemory {
    entries: Vec<ClockMemoryEntry>,
}

impl LyricClockMemory {
    /// 记下 `snapshot`（**即将被替换或丢弃的那一份**）的歌词钟。
    ///
    /// 位置按 `now` 外推（`Playing` 的曲子「离开那一刻」就是 `now`），
    /// 此后 **只读不推**。
    fn remember(&mut self, snapshot: &NowPlaying, now: Instant) {
        if snapshot.track_id == 0 || snapshot.session.aumid.is_empty() {
            return;
        }
        let position_ms = lyric_clock_at(snapshot, now);
        self.prune(now);
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.track_id == snapshot.track_id && e.aumid == snapshot.session.aumid)
        {
            e.position_ms = position_ms;
            e.sampled_at = now;
            e.state = snapshot.state;
            return;
        }
        if self.entries.len() >= limits::LYRIC_CLOCK_MEMORY_CAP {
            // 淘汰 sampled_at 最老的一条：确定性（同刻取靠前者），且不需要
            // 额外维护 LRU 访问序——记住的语义本来就是「按时间新旧」。
            let mut oldest = 0usize;
            for (i, e) in self.entries.iter().enumerate() {
                if e.sampled_at < self.entries[oldest].sampled_at {
                    oldest = i;
                }
            }
            self.entries.remove(oldest);
        }
        self.entries.push(ClockMemoryEntry {
            aumid: snapshot.session.aumid.clone(),
            track_id: snapshot.track_id,
            position_ms,
            sampled_at: now,
            state: snapshot.state,
        });
    }

    /// 查「这首歌离开时的钟」。`None` = 没记过 / 已过期 / 不可续接
    /// （见 [`restorable`]）/ 位置无意义（≤0）。
    ///
    /// 命中时返回的值**原样使用**（不按 `now - sampled_at` 外推），理由见类型
    /// 注释第 2 条。
    fn restore(&self, aumid: &str, track_id: TrackId, now: Instant) -> Option<RestoredClock> {
        if track_id == 0 || aumid.is_empty() {
            return None;
        }
        let e = self
            .entries
            .iter()
            .find(|e| e.track_id == track_id && e.aumid == aumid)?;
        let age_ms = now.saturating_duration_since(e.sampled_at).as_millis() as u64;
        if age_ms > limits::LYRIC_CLOCK_MEMORY_TTL_MS {
            return None;
        }
        if e.position_ms <= 0 {
            return None;
        }
        if !restorable(e.state) {
            crate::log_debug!(
                "歌词钟记忆不可续接：{} track={} 离开时在播（可能是重播/被顶掉）",
                aumid,
                track_id
            );
            return None;
        }
        Some(RestoredClock {
            position_ms: e.position_ms,
            leave_state: e.state,
        })
    }

    /// 丢掉过期条目（只影响超时的那几条，不影响「刚离开」的记忆）。
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|e| {
            let age_ms = now.saturating_duration_since(e.sampled_at).as_millis() as u64;
            age_ms <= limits::LYRIC_CLOCK_MEMORY_TTL_MS
        });
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
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
    /// G8-④：`PlaybackInfoChanged` 的到达时刻（跨线程，回调只写不读）。
    playback_events: PlaybackEventClock,
    /// G8-④：按 (aumid, track_id) 记住的「离开时歌词钟」。
    memory: LyricClockMemory,
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
            playback_events: PlaybackEventClock::default(),
            memory: LyricClockMemory::default(),
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
            self.playback_events.clone(),
        ));
    }

    // ------------------------------------------ 忽略态（v1.9.0 / G7，ARCH-G7 §4.3）

    /// 当前会话是否**不该被接管**（拉黑 ∪ 白名单外）。**纯内存**：用上一轮缓存的
    /// `parts` 身份，零跨进程调用。每轮主循环（含 50ms 空转轮次）最多调 1 次。
    ///
    /// v1.12.0：判据从「命中黑名单」改为 `MediaIndex::is_filtered()`——白名单非空时，
    /// 名单外的应用同样不接管，但它并没有被拉黑（设置页的"已拉黑"标记仍走 `matches`）。
    /// 下面所有「命中 → 进入忽略态」的路径共用这一处判据，不许各写一份。
    fn filtered_now(&self) -> bool {
        self.index.is_filtered(&self.parts.process_name, &self.parts.aumid)
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
        // G8-④：先记住歌词钟（被拉黑的应用解绑后，同一首歌不该从 0 重唱）
        self.remember_leaving(None, now);
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
            // v1.12.0：`ignored` 的语义是「永不选中」——白名单非空时名单外同样
            // 不选中，所以走 `is_filtered()`（设置页的"已拉黑"标记仍走 `matches`）。
            let ignored = self.index.is_filtered(&process_name, &aumid);
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
        // G8-④：切换前把旧会话的歌词钟记下来（切回来还能续上）
        self.remember_leaving(None, now);
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
    fn apply_release(&mut self, decision: ScanDecision, sink: &EventSink<AppEvent>, now: Instant) {
        // 黑名单独象已不成立：解绑回常规路径，并让缓存身份不再被误判为命中。
        self.display_session = None;
        // G8-④：丢弃快照前先记住歌词钟（解绑后重新看到同一首歌不该从 0 起）
        self.remember_leaving(None, now);
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
    /// 输入只有候选画像与 `PollState` 自身的状态，**零 COM、零 IO**。
    /// 真正的判定次序在 [`scan_decision`]（G8-⑥ 补齐了第 3 条），这里只负责
    /// 把「当前显示的是谁」从状态里取出来交过去：
    /// `displayed = display_session.is_some() ? parts.aumid（非空） : None`。
    ///
    /// 于是 `Keep` 有**两个来源**，别把它们混成一件事：
    /// 1. **粘性**：没有正常会话在播、且上次显示的会话仍在候选里 → 平局不漂移
    ///    （暂停不是换人）；
    /// 2. **判同**：`choose_session` 选中的正是当前显示的那个 → 不制造每秒重播。
    ///
    /// 两者的共同点是「只刷新 COM 句柄、不清缓存」；`Switch` 才会整份作废。
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
                self.apply_release(decision, sink, now);
                false
            }
            ScanDecision::Hide => {
                // 确实一个正常会话都没有 → 沿用现状空态（约束 2 后半句）。
                // 用其中一个被过滤候选的身份填 parts，让 filtered_now() 成立，
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

        if self.filtered_now() {
            // 不该接管（拉黑 / 白名单外）→ 进入忽略态（身份变了才再广播）
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

    /// G8-②/G8-④：把一次时间线读数写进分层缓存（**无 COM，可单测**）。
    ///
    /// **判据是「位置是否为 0」，不是「时长有没有值」**（v1.9.3 / G8-⑤ 收紧）。
    ///
    /// `(0,0)` 是「播放器根本没上报位置」，不是「位置等于 0」。网易云恒报它；
    /// 若照旧把 `parts.position_ms` 盖成 0，那么状态跃迁（或播控标志变化）
    /// 引发的任意一次发布都会把歌词钟归零。
    ///
    /// **写入条件 = 「这条读数里至少有一个字段是真值」**（`position != 0 ||
    /// duration != 0`），且**位置与采样时刻永远成对写**。
    ///
    /// 为什么 `(0,D)` 要写：真实上报位置的播放器（汽水 / Edge）把进度**拖回 0**
    /// 时读到的就是 `(0,D)`。若不写，`parts.position_ms` 会停在旧值，`Keep` 拼出
    /// 旧位置、`has_changed` 判 `delta ≈ 0` 静默丢弃 ⇒ **连发布都不发生**，歌词与
    /// 进度卡在旧位置。真机日志里 17 次 `(0,D)` 全部是换曲瞬间（真实位置确实 ≈0），
    /// 而换曲路径由 `Reset` 覆写，两者结果相同，所以这条写入是安全且必要的。
    ///
    /// 为什么它不会把网易云的钟砸到 0（G8-⑤ 的另一半在 `lyric_clock_action`）：
    /// 1. 网易云 `position` 与 `duration` **恒为 0** ⇒ 走不进这个分支（两个都 0）；
    /// 2. 换曲瞬间的 `(0,D)` 会被下一步 `Reset` 无条件覆写成 0，写与不写等价；
    /// 3. 状态跃迁那一拍的 `(0,D)`：判据层把「位置 == 0」判成**不可信位置** ⇒
    ///    走 `Carry` 续接，覆盖掉这里写入的 0。**两处必须同时成立**：只改判据层、
    ///    写入层仍一字不写 → 暂停态 seek 回 0 卡死在旧位置；只改写入层、判据层
    ///    仍把 `Some(_)` 当可信 → 跃迁那一拍绕过 Carry，钟被砸到 0。
    ///
    /// `timeline_at` 的唯一语义是「`parts.position_ms` 是**什么时候**采到的」，是
    /// `anchor_of` 规则 1 的外推基准。它必须与位置**同生同死**：以前它会随每条读数
    /// 刷成 `now`、却保留旧位置，于是构造出「陈旧位置 @ 新鲜时刻」——只要同一轮里
    /// 还有别的字段变化（播控标志 / 封面 / 标题 / 播放态）而真的广播出去，UI 就会
    /// 把时钟按陈旧位置重新起表，**整段已播时间被悄悄吃掉**（旧实现全靠
    /// `has_changed` 的 `delta == 0` 兜住，属侥幸）。现在写位置才写时刻；只有
    /// 「首份读数两个字段都是 0」（0 就是此刻建立的基准）才补一次 `timeline_at`。
    /// 注意 `note_timeline()` 的退避状态机仍照常吃 `(0,0)`，语义未动。
    fn apply_timeline_read(&mut self, position: Ms, duration: Ms, now: Instant) {
        if position != 0 || duration != 0 {
            self.parts.position_ms = position;
            // 位置与采样时刻**严格配对**（锚点规则 1 的依据）
            self.parts.timeline_at = Some(now);
        } else if self.parts.timeline_at.is_none() {
            // 首份读数就是 (0,0)：位置 0 本来就是这个时刻建立的基准，补一次。
            self.parts.timeline_at = Some(now);
        }
        self.parts.duration_ms = duration;
    }

    /// G8-②/G8-④：按本轮读数、状态跃迁与切歌情况拼出待发布的快照（**无 COM，可单测**）。
    ///
    /// `anchor_of` 的三规则对**切歌**的语义逐字保留；本方法只在两种情况下改写
    /// 歌词钟：
    /// - 切歌（含 5s 退避里没读时间线的换歌）：**先查 [`LyricClockMemory`]**
    ///   —— 这首歌「离开时的钟」还在记忆里且新鲜就用它续接（跨应用 / 跨会话
    ///   切回同一首歌不再从第一句重唱），查不到才是真正的 `Reset` 归零
    ///   （新歌仍然从头走，切歌语义不破）；
    /// - 状态跃迁：把上一份快照外推到**跃迁真正发生的时刻**
    ///   （`inputs.jumped_at`，见 [`transition_at`]；拿不到就退回 `now`，
    ///   与 v1.9.2 完全一致），并把同样的基准写回 `parts`，避免下一次 `(0,0)`
    ///   读数让 `has_changed` 的「位置大幅回退」判据误判成 seek。
    fn build_snapshot(&mut self, inputs: TickInputs, now: Instant) -> NowPlaying {
        // 只从 TickInputs 取值（单入口），下方 `anchor_of` / `lyric_clock_action`
        // 的判定逻辑与调用形式逐字未改。
        let TickInputs {
            state_jumped,
            timeline_read,
            read_timeline_now,
            jumped_at,
        } = inputs;
        let track_id = parts_track_id(&self.parts);
        let track_changed = self.last.as_ref().map(|p| p.track_id) != Some(track_id);
        let anchor = anchor_of(
            read_timeline_now,
            self.parts.timeline_at,
            track_changed,
            now,
            self.last.as_ref().map(|p| p.position_at),
        );

        let mut snapshot = self.parts.snapshot(anchor);
        let action = lyric_clock_action(
            timeline_read,
            self.parts.duration_ms == 0,
            state_jumped,
            track_changed,
            self.last.is_some(),
        );
        match action {
            LyricClockAction::Keep => {}
            LyricClockAction::Reset => {
                // 新歌从头走；但**这首歌自己**离开时的钟还在记忆里就用它续接。
                // 锚点一律是 now：钟是「此刻起表」的，与 anchor_of 的规则 1/2
                // 在本分支给出的值等价（规则 1 的 timeline_at 现在可能早于 now
                // —— 见 apply_timeline_read 的 G8-④ 修正，绝不能拿它当锚点）。
                let restored = self
                    .memory
                    .restore(&self.parts.aumid, track_id, now);
                let position_ms = restored.map(|r| r.position_ms).unwrap_or(0);
                snapshot.position_ms = position_ms;
                snapshot.position_at = now;
                self.parts.position_ms = position_ms;
                self.parts.timeline_at = Some(now);
                if let Some(r) = restored {
                    crate::log_info!(
                        "歌词钟续接：{} track={} → {}ms（离开时状态={}）",
                        snapshot.session.aumid,
                        track_id,
                        r.position_ms,
                        r.leave_state.as_str()
                    );
                }
            }
            LyricClockAction::Carry => {
                // G8-④：跃迁时刻优先用事件到达时刻（两个方向同源 → 逐轮抵消），
                // 拿不到就是本轮 now（v1.9.2 行为）。先结束对 self.last 的
                // 不可变借用，再写回 parts。
                let at = transition_at(now, jumped_at);
                let carried = self.last.as_ref().map(|p| {
                    // 事件时刻理论上不会早于上一份快照的锚点；万一早于（跨会话
                    // 陈旧时间戳），钳住它，绝不让钟倒退。
                    let base = if at < p.position_at { p.position_at } else { at };
                    (lyric_clock_at(p, base), base)
                });
                if let Some((carried, base)) = carried {
                    snapshot.position_ms = carried;
                    snapshot.position_at = base;
                    self.parts.position_ms = carried;
                    self.parts.timeline_at = Some(base);
                }
            }
        }
        snapshot
    }

    /// `tick()` 尾部「读完 → 这份快照值不值得广播」的**唯一判定点**（无 COM）。
    ///
    /// 抽成独立方法有两个理由：
    /// 1. `tick()` 的前半段（跨进程读取）无法单测，但**发布判定**可以——
    ///    歌词钟漂移是「每段都对、组合起来漂」，必须让多轮仿真跑在与生产
    ///    完全相同的判定代码上（`g8_lyric_clock` 的 N 轮仿真就是这么驱动的）；
    /// 2. 「先 `build_snapshot`、再 `has_changed` 门控、最后才推进 `last`」
    ///    这个顺序只存在一份，避免以后有人把 `last` 提前推进。
    fn publish_point(&mut self, inputs: TickInputs, now: Instant) -> Option<NowPlaying> {
        let snapshot = self.build_snapshot(inputs, now);
        if let Some(prev) = self.last.as_ref() {
            if !has_changed(prev, &snapshot) {
                return None;
            }
        }
        Some(snapshot)
    }

    /// G8-④：`last` 即将被替换成 `next`（或整个丢弃）时，把它的歌词钟记进记忆。
    ///
    /// **只在「离开这首歌」时记**（`next == None` 或 aumid / track_id 变了）：
    /// 同一首歌的稳态发布（播控标志翻转等）不刷新记忆的采样时刻，否则 TTL
    /// 会被无限续期，陈旧位置永远不过期，反而会把歌词拨回很久以前。
    fn remember_leaving(&mut self, next: Option<&NowPlaying>, now: Instant) {
        let Some(prev) = self.last.as_ref() else {
            return;
        };
        let leaving = match next {
            None => true,
            Some(n) => n.track_id != prev.track_id || n.session.aumid != prev.session.aumid,
        };
        if leaving {
            self.memory.remember(prev, now);
        }
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
                    // G8-④：会话消失前先把这首歌的歌词钟记下来
                    // （跨应用切走后切回来才能续上，而不是从第一句重唱）。
                    self.remember_leaving(None, now);
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
        // G8-②：本轮交给 `build_snapshot` 的纯数据输入——`state_jumped` 由
        // `note_playback()` 的返回值赋值，`timeline_read` 由中层记录；两者最后
        // 经 `tick_inputs()` 收拢成 `TickInputs`（`read_timeline_now` 由它派生，
        // 不再单独立局部变量，避免「两个来源可能不一致」）。
        let mut timeline_read: Option<(Ms, Ms)> = None;
        let mut state_jumped = false;
        let mut jumped_at: Option<Instant> = None;
        let mut playback_info = None;
        if plan.playback {
            // G8-④：必须在 note_playback() **之前**取「上一次观测时刻」——
            // 它是判定事件时间戳是否陈旧（早于上一次观测 = 更早的跃迁残留）
            // 的唯一依据。
            let prev_obs = self.cadence.last_playback_obs();
            let (info, ms) = timed("smtc.playback", || session.GetPlaybackInfo().ok());
            self.stats.playback += 1;
            self.stats.observe(ms);
            let state = playback_state_of(info.as_ref());
            // 状态跃迁 → 时间线与属性都在本轮立即补读，
            // 这样事件里带的是**新状态下**的真实位置，而不是最多 1s 前的旧值。
            if self.cadence.note_playback(now, state) {
                plan.timeline = true;
                plan.props = true;
                state_jumped = true;
                // G8-④：跃迁时刻 = PlaybackInfoChanged 的到达时刻（暂停/继续
                // 两个方向同源 → 逐轮误差相等 → 逐轮抵消）；拿不到退回 now。
                jumped_at = Some(transition_at(now, self.playback_events.take_after(prev_obs)));
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
            timeline_read = Some((position, duration));
            self.apply_timeline_read(position, duration, now);
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
            if self
                .index
                .is_filtered(player_map::resolve(&aumid).process_name, &aumid)
            {
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
        if self.filtered_now() {
            if self.rescan_sessions(mgr, sink, now) {
                return self.tick(mgr, sink, now, Plan::full());
            }
            return;
        }

        // ---- 锚点 + G8-②/G8-④ 歌词钟续接（三规则与切歌语义见 anchor_of / build_snapshot）----
        let inputs = tick_inputs(state_jumped, timeline_read).at(jumped_at);
        let Some(snapshot) = self.publish_point(inputs, now) else {
            return;
        };

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

        // G8-④：先记下「即将被替换的那一份」的歌词钟（只在离开这首歌时记），
        // 再推进 last —— 顺序反了就等于没记。
        self.remember_leaving(Some(&snapshot), now);
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
                scan_gate(state.filtered_now(), state.display_session.is_some());
            let switched = blacklist_active
                && session_scan_due(true, state.last_session_scan, now)
                && state.rescan_sessions(&mgr, &sink, now);
            if switched {
                // 已切到另一个正常会话：本轮全量读它并发布（不留 50ms 空窗）。
                state.tick(&mgr, &sink, now, Plan::full());
            } else if state.filtered_now() {
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
///
/// G8-④：`PlaybackInfoChanged` 额外写一次到达时刻（[`PlaybackEventClock`]）。
/// 那是一次 `Instant::now()` + 一次极短加锁，**零 COM、零分配、零日志**，
/// 与「只许置脏位」的纪律不冲突；拿到它，歌词钟才知道状态跃迁真正发生在何时。
fn subscribe_session(
    session: &GlobalSystemMediaTransportControlsSession,
    dirty: &Arc<AtomicBool>,
    sink: &EventSink<AppEvent>,
    aumid: String,
    events: PlaybackEventClock,
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
        // G8-④：状态跃迁的真实时刻（媒体线程据此锚定歌词钟）
        events.stamp();
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
    if state.filtered_now() {
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

    /// `filtered_now` 在 parts 为空（无会话）时绝不命中；
    /// 黑名单配置变化（index.set）后同一份 parts 立即翻转。
    #[test]
    fn filtered_now_is_empty_parts_safe_and_follows_config() {
        let idx = MediaIndex::new(&crate::config::MediaConfig::default());
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), idx);
        assert!(!st.filtered_now(), "空 parts（无会话）不许命中");

        // 模拟「上一轮缓存的身份」是被拉黑应用
        st.parts.aumid = "NeteaseCloudMusic!App".into();
        st.parts.process_name = "cloudmusic.exe".into();
        assert!(!st.filtered_now(), "黑名单为空 → 不命中");

        // 托盘拉黑 → index.set 整组替换 → 下一轮主循环立即识别
        let mut cfg = crate::config::MediaConfig::default();
        cfg.blacklist = vec!["cloudmusic.exe".into()];
        st.index.set(&cfg);
        assert!(st.filtered_now(), "拉黑后 cached parts 必须立即命中（纯内存）");
        assert!(st.index.is_filtered("cloudmusic.exe", "NeteaseCloudMusic!App"));
        // 解绑（清空）→ 立即恢复
        st.index.set(&crate::config::MediaConfig::default());
        assert!(!st.filtered_now(), "解绑后必须立即不命中（≤1s 恢复的前提）");
    }

    /// **v1.12.0**：白名单同样驱动 `filtered_now`——改配置后同一份 parts 立即翻转。
    #[test]
    fn filtered_now_follows_whitelist_config() {
        let idx = MediaIndex::new(&crate::config::MediaConfig::default());
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), idx);
        st.parts.aumid = "Spotify.exe".into();
        st.parts.process_name = "spotify.exe".into();
        assert!(!st.filtered_now(), "白名单为空（未启用）→ 放行");

        // 切到白名单模式，名单里只有网易云 → 正在缓存的 Spotify 立刻变「不该接管」
        let mut cfg = crate::config::MediaConfig::default();
        cfg.mode = crate::config::FilterMode::Whitelist;
        cfg.whitelist = vec!["cloudmusic.exe".into()];
        st.index.set(&cfg);
        assert!(st.filtered_now(), "白名单模式 + 名单外 → 必须过滤");
        // 它**并没有被拉黑**：切回黑名单模式就立刻放行。这个区分在 UI 上仍要用
        // （设置页标的是「已在名单内」，不是「已拉黑」），所以单独钉一条。
        st.index.set(&crate::config::MediaConfig::default());
        assert!(!st.filtered_now(), "黑名单模式（空表）→ 放行，说明它没被拉黑");
        st.index.set(&cfg); // 恢复白名单模式，继续验证"加入白名单后恢复"

        // 把 Spotify 加入白名单 → 恢复
        cfg.whitelist.push("spotify.exe".into());
        st.index.set(&cfg);
        assert!(!st.filtered_now(), "加入白名单后立即恢复接管");
    }

    /// **v1.12.0 接线锁**：媒体主循环的「要不要接管」只能走 `is_filtered`
    /// （拉黑 ∪ 白名单外）。谁把它改回只看黑名单，白名单就静默失效——
    /// 而所有纯函数测试仍然全绿（`is_filtered` 自己测得过）。
    #[test]
    fn media_loop_filters_through_is_filtered_not_matches() {
        let src = include_str!("smtc.rs");
        // 只看**生产区**：这条测试自己就写着 `self.index.matches(&self.parts`，
        // 全文扫描会被自身满足（假绿 → 真绿的反面：断言永远失败）。
        let cut = src.find("#[cfg(test)]\nmod tests").expect("找不到测试模块起点");
        let prod = &src[..cut];
        // 判据函数体内部必须是 is_filtered，不许是 matches
        let start = prod.find("fn filtered_now(&self) -> bool {").expect("找不到 filtered_now");
        let body_end = prod[start..].find("\n    }").expect("filtered_now 没有闭合");
        let body = &prod[start..start + body_end];
        assert!(
            body.contains("index.is_filtered"),
            "filtered_now 必须调用 MediaIndex::is_filtered"
        );
        assert!(
            !body.contains("index.matches("),
            "matches 只回答'是否被拉黑'，不能当过滤判据"
        );
        // 生产代码里不许再出现直接的 parts 级 matches 判定（全部收拢到 filtered_now）
        assert_eq!(
            prod.matches("self.index.matches(&self.parts").count(),
            0,
            "parts 级判定必须统一走 filtered_now，否则会漏掉白名单"
        );
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

    /// 带真实黑名单配置的索引：候选的 `ignored` 由 `MediaIndex::is_filtered` 实算，
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
                    ignored: index.is_filtered(process, aumid),
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

    // ------------------------------------------------ G8-⑥ 平局不漂移（v1.14.0）

    /// 复刻真机现场（2026-11-14 日志）：黑名单里有 `msedge.exe`，所以锁定模式
    /// 长期生效、每 1s 枚举一次；用户暂停汽水的那一刻，候选画像变成
    /// `[Edge(ignored), 网易云(暂停, 幽灵会话), 汽水(暂停)]`。
    ///
    /// 修复前 → `Switch(1)`：小组件漂到一个从未播放的网易云上，连播控都错位；
    /// 修复后 → `Keep(2)`：停在用户刚暂停的汽水上，按播放能直接恢复。
    #[test]
    fn scan_decision_keeps_displayed_session_when_it_pauses() {
        let index = blacklist_index(&["msedge.exe"]);
        let candidates = measured_candidates(
            &index,
            &[
                ("MSEdge", false),         // [0] 被拉黑（用户黑名单里的 Edge）
                ("cloudmusic.exe", false), // [1] 正常但暂停：启动过、没播、会话还挂着
                ("汽水音乐", false),        // [2] 正常、用户刚按下暂停
            ],
        );
        assert!(candidates[0].ignored, "前置：Edge 确实命中黑名单");
        assert_eq!(
            sticky_index(&candidates, Some("汽水音乐")),
            Some(2),
            "前置：上次显示的汽水仍在候选里且未被过滤"
        );
        // 修复前这里是 Switch(1)（漂到网易云），删掉粘性分支必须变红
        assert_eq!(
            scan_decision(&candidates, Some("汽水音乐")),
            ScanDecision::Keep(2),
            "暂停不是换人：必须停在用户刚暂停的那个会话上"
        );
    }

    /// 反向：用户真的在另一个播放器点了播放 → 必须切过去。
    /// 防「过度粘性把合法切换也拒了」（这是粘性最危险的副作用）。
    #[test]
    fn scan_decision_still_switches_to_a_playing_session() {
        let index = blacklist_index(&["msedge.exe"]);
        let candidates = measured_candidates(
            &index,
            &[
                ("MSEdge", false),
                ("cloudmusic.exe", true), // 用户真的在网易云点了播放
                ("汽水音乐", false),
            ],
        );
        assert_eq!(
            scan_decision(&candidates, Some("汽水音乐")),
            ScanDecision::Switch(1),
            "有正常会话在播时，粘性必须让路"
        );
    }

    /// 粘性绝不能给黑名单开后门：上次「显示」的会话若已被拉黑，不许 Keep。
    /// （锁定模式只在系统当前会话被拉黑时才有 `display_session`，但用户可能在
    /// 显示期间把该应用加进黑名单，这条边界必须守住 G7 语义。）
    #[test]
    fn scan_decision_does_not_keep_a_displayed_session_that_became_ignored() {
        let index = blacklist_index(&["msedge.exe"]);
        let candidates = measured_candidates(
            &index,
            &[("MSEdge", true), ("cloudmusic.exe", false)],
        );
        assert!(candidates[0].ignored);
        let decision = scan_decision(&candidates, Some("MSEdge"));
        assert_ne!(
            decision,
            ScanDecision::Keep(0),
            "被拉黑的会话绝不许借粘性复活"
        );
        assert_eq!(decision, ScanDecision::Switch(1), "应改显示唯一正常的会话");
        // 纯函数层面也不许为 ignored 候选给出下标
        assert_eq!(sticky_index(&candidates, Some("MSEdge")), None);
    }

    /// 上次显示的会话已消失（用户关掉汽水）→ 不粘幽灵，顺着系统顺序走。
    #[test]
    fn scan_decision_falls_back_to_first_normal_when_displayed_vanished() {
        let index = blacklist_index(&["msedge.exe"]);
        let candidates = measured_candidates(
            &index,
            &[("MSEdge", false), ("cloudmusic.exe", false)],
        );
        assert_eq!(
            sticky_index(&candidates, Some("汽水音乐")),
            None,
            "汽水已经不在候选里，没有可粘的对象"
        );
        assert_eq!(
            scan_decision(&candidates, Some("汽水音乐")),
            ScanDecision::Switch(1),
            "应用关掉后必须正常跟随，不能停在幽灵上"
        );
    }

    /// 优先级真值表：`Release > 合法切换(Playing) > 粘性 > 系统顺序第一个/Hide`。
    ///
    /// 本次事故的本质是**次序缺失**，单点断言守不住，所以把四个维度的组合钉死：
    /// `has_ignored × has_playing_normal × displayed_present × displayed_playing`。
    #[test]
    fn scan_decision_priority_truth_table() {
        // ① 没有 ignored 候选 → Release（最高优先级，压过一切）
        let no_ignored = vec![cand("Spotify.exe", false, false)];
        assert_eq!(
            scan_decision(&no_ignored, Some("Spotify.exe")),
            ScanDecision::Release
        );

        // ② 有 ignored + 有正常会话在播 → Switch 到在播的那个，即使上次显示的
        //    是另一个也照样切（合法切换优先于粘性）
        let playing_wins = vec![
            cand("MSEdge", false, true),
            cand("cloudmusic.exe", false, false),
            cand("汽水音乐", true, false),
        ];
        assert_eq!(
            scan_decision(&playing_wins, Some("cloudmusic.exe")),
            ScanDecision::Switch(2)
        );

        // ③ 有 ignored + 全暂停 + 上次显示的仍在 → Keep（平局不漂移）
        //    这两种子情形（上次显示的是 playing 还是 paused）都必须 Keep
        let all_paused = vec![
            cand("MSEdge", false, true),
            cand("cloudmusic.exe", false, false),
            cand("汽水音乐", false, false),
        ];
        assert_eq!(
            scan_decision(&all_paused, Some("汽水音乐")),
            ScanDecision::Keep(2)
        );
        assert_eq!(
            scan_decision(&all_paused, Some("cloudmusic.exe")),
            ScanDecision::Keep(1),
            "粘的是上次显示的那个，不是系统顺序第一个"
        );

        // ④ 有 ignored + 全暂停 + 上次显示的缺失 → 系统顺序第一个正常会话
        assert_eq!(
            scan_decision(&all_paused, None),
            ScanDecision::Switch(1),
            "冷启动没有任何历史 → 仍按系统顺序兜底"
        );
        assert_eq!(scan_decision(&all_paused, Some("")), ScanDecision::Switch(1));

        // ⑤ 有 ignored + 无正常候选 → Hide（不管有没有 displayed）
        let only_ignored = vec![cand("MSEdge", true, true)];
        assert_eq!(scan_decision(&only_ignored, Some("汽水音乐")), ScanDecision::Hide);
    }

    /// `sticky_index` 的边界：空串与 `None` 一视同仁，绝不把空身份当成匹配。
    #[test]
    fn sticky_index_ignores_empty_identity() {
        let candidates = vec![
            cand("", false, false),
            cand("汽水音乐", false, false),
        ];
        assert_eq!(sticky_index(&candidates, None), None);
        assert_eq!(sticky_index(&candidates, Some("")), None, "空串不是身份");
        assert_eq!(sticky_index(&candidates, Some("汽水音乐")), Some(1));
        assert_eq!(sticky_index(&candidates, Some("不存在的应用")), None);
    }

    /// **接线锁**：粘性能生效的前提是 `decide_scan` 确实把
    /// `display_session.is_some()` 时的 `parts.aumid` 交给 `scan_decision`。
    /// 若有人把它改成常量 `None`，纯函数单测仍会全绿，但线上粘性完全不生效
    /// ——「测试全绿但功能没做」的经典坑，所以直接对源码做静态断言。
    #[test]
    fn decide_scan_wires_displayed_aumid_into_scan_decision() {
        const SRC: &str = include_str!("smtc.rs");
        let start = SRC
            .find("    fn decide_scan(&self, candidates: &[SessionCandidate])")
            .expect("decide_scan 必须存在");
        let body = &SRC[start..];
        let end = body.find("\n    }\n").expect("decide_scan 函数体必须闭合");
        let body = &body[..end];
        assert!(
            body.contains("self.display_session.is_some()"),
            "displayed 必须来自 display_session 状态，不能是常量"
        );
        assert!(
            body.contains("self.parts.aumid.as_str()"),
            "displayed 必须是当前显示会话的 AUMID"
        );
        assert!(
            body.contains("scan_decision(candidates, displayed)"),
            "必须把 displayed 交给纯函数，否则粘性永远拿不到身份"
        );
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

        st.apply_release(ScanDecision::Release, &sink, Instant::now());

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
        st.apply_release(ScanDecision::Release, &sink, Instant::now());
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
        st.apply_release(ScanDecision::Switch(1), &sink, Instant::now());
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

// ============================================================================
// G8-②（v1.9.2）—— 网易云「暂停→继续」歌词钟不归零
//
// 全部测试跑在纯数据上：候选读数 / 状态跃迁标志 / 已广播快照 / 注入的时刻。
// 其中 `apply_timeline_read` / `build_snapshot` 两条直接调用 PollState 的接线，
// 保证「只改纯函数不改接线」也会被反向验证打红。
// ============================================================================
#[cfg(test)]
mod g8_lyric_clock {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// 全部时刻都取「过去」，`position_at.elapsed()` / `position_now()` 才有意义。
    fn past_base() -> Instant {
        Instant::now() - Duration::from_millis(200_000)
    }

    fn test_index() -> Arc<MediaIndex> {
        MediaIndex::new(&crate::config::MediaConfig::default())
    }

    /// 网易云式 parts：有曲目、属性已成功读过、恒报 (0,0)、歌词钟从 t0 起走表。
    fn zero_player(parts: &mut SessionParts, title: &str, t0: Instant) {
        parts.track = Track {
            title: title.to_string(),
            artist: "周杰伦".to_string(),
            album: String::new(),
            duration_ms: 0,
        };
        parts.props_ok = true;
        parts.state = PlaybackState::Playing;
        parts.position_ms = 0;
        parts.duration_ms = 0;
        parts.timeline_at = Some(t0);
    }

    fn state_with(title: &str, t0: Instant) -> PollState {
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, title, t0);
        st
    }

    /// 造一份「已广播」快照：track_id 与 parts 一致（不触发切歌）。
    fn published(
        parts: &SessionParts,
        state: PlaybackState,
        position: Ms,
        position_at: Instant,
    ) -> NowPlaying {
        NowPlaying {
            track_id: parts_track_id(parts),
            track: Some(effective_track(parts)),
            session: MediaSessionInfo {
                aumid: parts.aumid.clone(),
                app_name: parts.app_name.clone(),
                process_name: parts.process_name.clone(),
                ..Default::default()
            },
            state,
            position_ms: position,
            position_at,
            ..Default::default()
        }
    }

    /// 测试专用 `TickInputs` 构造：**字段全部显式给出**（含 `read_timeline_now`），
    /// 不依赖被测的 [`tick_inputs`] 推导 —— `build_snapshot` 的单测与
    /// `tick_inputs()` 的真值表因此互相独立，任一处写坏都能被单独抓出来。
    ///
    /// G8-④：`jumped_at`（跃迁时刻）默认 `None` = 本轮的 `now`，即 v1.9.2 行为；
    /// 要测「事件时刻」的用例用 [`ti_at`]。
    fn ti(
        state_jumped: bool,
        timeline_read: Option<(Ms, Ms)>,
        read_timeline_now: bool,
    ) -> TickInputs {
        ti_at(state_jumped, timeline_read, read_timeline_now, None)
    }

    /// 带跃迁时刻的 `TickInputs`（G8-④）。
    fn ti_at(
        state_jumped: bool,
        timeline_read: Option<(Ms, Ms)>,
        read_timeline_now: bool,
        jumped_at: Option<Instant>,
    ) -> TickInputs {
        TickInputs {
            state_jumped,
            timeline_read,
            read_timeline_now,
            jumped_at,
        }
    }

    // ------------------------------------------------ 纯判定 / 纯计算

    /// 4 种时间线读数 × 零时长标志 × 状态跃迁 × 切歌 × 有无上一份 = 64 格真值表，
    /// 逐格对照独立期望（可信读数一律 Keep、切歌 Reset、跃迁 Carry）。
    #[test]
    fn lyric_clock_action_truth_table() {
        fn want(
            timeline: Option<(Ms, Ms)>,
            zero_duration: bool,
            jumped: bool,
            track_changed: bool,
            has_prev: bool,
        ) -> LyricClockAction {
            // v1.9.3 / G8-⑤：`(0, D)` 同样不是可信位置（判据 = 位置是否为 0）。
            // 旧版这里写的是 `Some(_) => true`，即「时长有值就算位置可信」——
            // 那正是「状态跃迁那一拍撞上 (0,D) 就把钟砸到 0」的成因。
            let usable = match timeline {
                Some((position, _)) => position != 0,
                None => !zero_duration,
            };
            if usable {
                return LyricClockAction::Keep;
            }
            if track_changed {
                return LyricClockAction::Reset;
            }
            if !has_prev {
                return LyricClockAction::Keep;
            }
            if jumped {
                LyricClockAction::Carry
            } else {
                LyricClockAction::Keep
            }
        }
        let timelines = [
            None,
            Some((0, 0)),
            Some((12_000, 180_000)),
            Some((0, 180_000)),
        ];
        let mut cells = 0;
        for timeline in timelines {
            for zero_duration in [false, true] {
                for jumped in [false, true] {
                    for track_changed in [false, true] {
                        for has_prev in [false, true] {
                            assert_eq!(
                                lyric_clock_action(
                                    timeline,
                                    zero_duration,
                                    jumped,
                                    track_changed,
                                    has_prev
                                ),
                                want(timeline, zero_duration, jumped, track_changed, has_prev),
                                "timeline={timeline:?} zero_duration={zero_duration} jumped={jumped} track_changed={track_changed} has_prev={has_prev}"
                            );
                            cells += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(cells, 64);
    }

    /// 真实位置播放器（汽水）无论状态是否跃迁都不该被续接逻辑改写。
    #[test]
    fn lyric_clock_action_never_carries_for_real_position_players() {
        // 真实位置读数（position > 0）→ 一律 Keep：汽水这类播放器的路径不许变。
        for timeline in [Some((30_000, 180_000)), None] {
            for jumped in [false, true] {
                assert_eq!(
                    lyric_clock_action(timeline, false, jumped, false, true),
                    LyricClockAction::Keep
                );
            }
        }
        // `(0, 真实时长)` **不是**可信位置（v1.9.3 / G8-⑤），所以它和 `(0,0)`
        // 同档：跃迁那一拍要走 Carry 受保护，不许 Keep 把位置盖成 0。
        // ⚠️ 旧断言给的是 Keep —— 那正是「时长有值就当位置可信」的病因
        // （真机日志 17 次 `(0,D)` 全在换曲瞬间，一旦撞上暂停/继续那一拍，
        // 钟就被砸到 0）。这里按新契约改写，属收紧。
        for timeline in [Some((0, 0)), Some((0, 180_000))] {
            assert_eq!(
                lyric_clock_action(timeline, false, false, false, true),
                LyricClockAction::Keep,
                "非跃迁轮次：保持现状（位置由上一份有效值继续外推）"
            );
            assert_eq!(
                lyric_clock_action(timeline, false, true, false, true),
                LyricClockAction::Carry,
                "跃迁轮次必须 Carry 保护，不许把钟砸到 0"
            );
        }
        // 首帧还没有上一份快照 → 仍然 Keep（`anchor_of` 兜底），不会凭空 Carry。
        assert_eq!(
            lyric_clock_action(Some((0, 180_000)), false, true, false, false),
            LyricClockAction::Keep
        );
    }

    /// 5s 退避里没读时间线的零时长播放器，切歌也必须 Reset（不带旧歌词钟进新歌）。
    #[test]
    fn lyric_clock_action_resets_on_backoff_track_change_without_timeline_read() {
        assert_eq!(
            lyric_clock_action(None, true, false, true, true),
            LyricClockAction::Reset
        );
        // 同样没读时间线但播放器报真实时长 → 汽水原样，不回归
        assert_eq!(
            lyric_clock_action(None, false, false, true, true),
            LyricClockAction::Keep
        );
    }

    /// `lyric_clock_at` 与 `position_now` 同口径；暂停冻结、播放线性外推。
    #[test]
    fn lyric_clock_at_extrapolates_playing_and_freezes_paused() {
        let t0 = past_base();
        let mut playing = NowPlaying::default();
        playing.state = PlaybackState::Playing;
        playing.position_ms = 5_000;
        playing.position_at = t0;
        assert_eq!(lyric_clock_at(&playing, t0), 5_000);
        assert_eq!(lyric_clock_at(&playing, at(t0, 3_000)), 8_000);

        let mut paused = playing.clone();
        paused.state = PlaybackState::Paused;
        paused.position_ms = 42_000;
        assert_eq!(
            lyric_clock_at(&paused, at(t0, 60_000)),
            42_000,
            "暂停期间歌词原地停住"
        );
        assert_eq!(paused.position_now(), 42_000, "暂停的 position_now 也冻结");
    }

    /// 时钟回拨 / 极端数值不许 panic、不许把位置倒推成负数。
    #[test]
    fn lyric_clock_at_is_clock_skew_safe() {
        let t0 = past_base();
        let mut s = NowPlaying::default();
        s.state = PlaybackState::Playing;
        s.position_ms = Ms::MAX - 10;
        s.position_at = t0;
        assert_eq!(lyric_clock_at(&s, t0 - Duration::from_millis(1_000)), Ms::MAX - 10);
        assert_eq!(lyric_clock_at(&s, at(t0, 1_000)), Ms::MAX);
    }

    // ------------------------------------------------ 接线：时间线层

    /// `(0,0)` 不是有效位置读数：不许把已续接的歌词钟盖回 0，**也不许刷新
    /// `timeline_at`**（v1.9.3 / G8-④ 收紧）；真实读数照常写，且位置与采样
    /// 时刻同进退（`anchor_of` 规则 1 的外推基准必须与位置**配对**）。
    ///
    /// ⚠️ v1.9.2 的这条断言写的是 `timeline_at == Some(at(t0, 1_000))`，即
    /// **要求** `(0,0)` 刷新采样时刻——那正是本轮的病因：位置留在几秒前、
    /// 基准却跳到此刻，只要同一轮里还有别的字段变化（播控标志 / 封面 / 标题 /
    /// 播放态）而真的广播出去，UI 就把时钟按陈旧位置重新起表，整段已播时间
    /// 被悄悄吃掉。旧实现只是靠 `has_changed` 的 `delta == 0` 侥幸没露头。
    /// 这条断言因此按新契约改写（更严：位置与时刻必须配对），不是放宽。
    #[test]
    fn apply_timeline_read_ignores_zero_zero_but_writes_real_position() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        st.parts.position_ms = 42_000;
        st.apply_timeline_read(0, 0, at(t0, 1_000));
        assert_eq!(st.parts.position_ms, 42_000, "(0,0) 不许把歌词钟归零");
        assert_eq!(
            st.parts.timeline_at,
            Some(t0),
            "(0,0) 不许刷新采样时刻：位置与基准必须配对，否则陈旧位置配上新鲜锚点"
        );
        assert_eq!(st.parts.duration_ms, 0);

        st.apply_timeline_read(30_000, 180_000, at(t0, 2_000));
        assert_eq!(st.parts.position_ms, 30_000, "汽水式真实读数照常写入");
        assert_eq!(
            st.parts.timeline_at,
            Some(at(t0, 2_000)),
            "真实读数必须把采样时刻一起推进"
        );
        assert_eq!(st.parts.duration_ms, 180_000);

        // 首份读数就是 (0,0)：位置 0 本来就是这个时刻建立的基准 → 补上采样时刻。
        let mut fresh = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        assert_eq!(fresh.parts.timeline_at, None);
        fresh.apply_timeline_read(0, 0, at(t0, 500));
        assert_eq!(fresh.parts.timeline_at, Some(at(t0, 500)));
        // 再来一次 (0,0) 就不许再往后续了
        fresh.apply_timeline_read(0, 0, at(t0, 6_000));
        assert_eq!(fresh.parts.timeline_at, Some(at(t0, 500)));
    }

    // ------------------------------------------------ 接线：发布层（核心）

    /// ① 核心：`Playing→Paused→Playing` 两次状态跃迁 + `(0,0)` 读数，
    /// 位置与锚点都不归零；暂停冻结在那一句，继续后接着外推。
    #[test]
    fn state_jump_on_zero_player_carries_lyric_clock_across_pause_and_resume() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);

        // ---- 暂停（T1 = T0 + 42s）----
        let t1 = at(t0, 42_000);
        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(0, 0, t1);
        let paused = st.build_snapshot(ti(true, Some((0, 0)), true), t1);
        assert_eq!(paused.position_ms, 42_000, "暂停冻结在第 42s 那一句，不是 0");
        assert_eq!(paused.position_at, t1);
        assert_eq!(st.parts.position_ms, 42_000, "写回 parts 防下次 (0,0) 归零");
        assert_eq!(st.parts.timeline_at, Some(t1));
        assert_eq!(paused.position_now(), 42_000, "暂停期间歌词原地停住");

        // ---- 继续（T2 = T1 + 10s）----
        st.last = Some(paused);
        let t2 = at(t0, 52_000);
        st.parts.state = PlaybackState::Playing;
        st.apply_timeline_read(0, 0, t2);
        let resumed = st.build_snapshot(ti(true, Some((0, 0)), true), t2);
        assert_eq!(resumed.position_ms, 42_000, "继续后从暂停那一句接着走");
        assert_eq!(resumed.position_at, t2, "锚点取继续时刻，从冻结值重新外推");
        assert_eq!(
            lyric_clock_at(&resumed, at(t2, 2_000)),
            44_000,
            "继续后按继续时刻线性外推"
        );
    }

    /// ② 暂停冻结用真实 `position_now()` 复核，继续后的续接值不等于 0。
    #[test]
    fn paused_snapshot_freezes_position_now_and_resume_continues() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);
        let t1 = at(t0, 63_000);
        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(0, 0, t1);
        let paused = st.build_snapshot(ti(true, Some((0, 0)), true), t1);
        assert_eq!(paused.position_now(), 63_000);
        assert_ne!(paused.position_now(), 0, "暂停不许把歌词拨回第一句");

        st.last = Some(paused);
        let t2 = at(t0, 70_000);
        st.parts.state = PlaybackState::Playing;
        st.apply_timeline_read(0, 0, t2);
        let resumed = st.build_snapshot(ti(true, Some((0, 0)), true), t2);
        assert_eq!(resumed.position_ms, 63_000);
        assert_eq!(lyric_clock_at(&resumed, at(t2, 1_000)), 64_000);
    }

    /// ③ 汽水式（真实 position/duration）状态跃迁仍采用播放器上报的真实位置。
    #[test]
    fn real_position_player_state_jump_keeps_reported_position() {
        let t0 = past_base();
        let mut st = state_with("汽水", t0);
        st.parts.duration_ms = 180_000;
        st.parts.position_ms = 0;
        st.parts.timeline_at = Some(t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);

        let t1 = at(t0, 30_000);
        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(30_000, 180_000, t1);
        let paused = st.build_snapshot(ti(true, Some((30_000, 180_000)), true), t1);
        assert_eq!(paused.position_ms, 30_000, "真实位置原样采用，不被续接改写");
        assert_eq!(paused.position_at, t1);
        assert_eq!(paused.position_now(), 30_000);
        assert_eq!(st.parts.position_ms, 30_000);
        // 播放器的真实时长参与 track_id，行为不变
        assert_eq!(paused.track.as_ref().unwrap().duration_ms, 180_000);
    }

    /// ④ 切歌必须归零：即便同时发生状态跃迁、且已有 42s 歌词钟，也不许续接过去。
    #[test]
    fn zero_player_track_change_resets_position_even_with_state_jump() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);

        let t1 = at(t0, 42_000);
        st.parts.track.title = "七里香".to_string();
        st.apply_timeline_read(0, 0, t1);
        let new_song = st.build_snapshot(ti(true, Some((0, 0)), true), t1);
        assert_eq!(new_song.position_ms, 0, "切歌归零，不被续接误伤");
        assert_eq!(new_song.position_at, t1);
        assert_eq!(st.parts.position_ms, 0, "写回 parts，下一首从 0 起走表");
        assert_ne!(
            new_song.track_id,
            st.last.as_ref().unwrap().track_id,
            "切歌必须换 track_id（触发歌词请求与代次守卫）"
        );
    }

    /// ④ 切歌锚点规则 2 未被改动：本轮没读时间线时仍用检测时刻，
    /// 不退回陈旧的 `timeline_at`（v1.4.2 漂移 bug 的回归守卫）。
    #[test]
    fn track_change_without_timeline_read_keeps_detection_anchor() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);
        let detection = at(t0, 5_000);
        st.parts.track.title = "七里香".to_string();
        st.parts.timeline_at = Some(t0); // 陈旧锚点，绝不许被采用
        let new_song = st.build_snapshot(ti(false, None, false), detection);
        assert_eq!(new_song.position_at, detection, "锚点必须是检测时刻");
        assert_ne!(
            new_song.track_id,
            st.last.as_ref().unwrap().track_id
        );
        assert!(has_changed(st.last.as_ref().unwrap(), &new_song));
    }

    /// ④ 5s 退避里由慢层发现切歌（本轮没读时间线）：即便 parts 里还留着
    /// 上一首续接的 42s 歌词钟，新歌也必须归零，锚点仍是检测时刻。
    #[test]
    fn backoff_track_change_without_timeline_read_drops_carried_clock() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);
        let detection = at(t0, 5_000);
        st.parts.track.title = "七里香".to_string();
        let new_song = st.build_snapshot(ti(false, None, false), detection);
        assert_eq!(new_song.position_ms, 0, "换歌不许把上一首的歌词钟带过来");
        assert_eq!(new_song.position_at, detection);
        assert_eq!(st.parts.position_ms, 0);
    }

    /// ⑤ 首帧 / 无上一份快照：沿用 `anchor_of` 的 now 兜底，位置取 parts 的值。
    #[test]
    fn first_frame_without_prev_uses_now_anchor_and_parts_position() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        st.last = None;
        st.parts.timeline_at = None;
        st.apply_timeline_read(0, 0, t0);
        let first = st.build_snapshot(ti(false, Some((0, 0)), true), t0);
        assert_eq!(first.position_ms, 0);
        assert_eq!(first.position_at, t0);
        assert_eq!(st.parts.position_ms, 0);
    }

    /// ⑥ + 约束 3：状态跃迁不改 `track_id`，app.rs 因此不会发 `Effect::Lyric`
    /// / `LyricLoading`，歌词高亮连续、不重载、不闪一下。
    #[test]
    fn state_jump_keeps_track_id_so_ui_does_not_reload_lyrics() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        let prev_tid = s0.track_id;
        st.last = Some(s0);
        let t1 = at(t0, 20_000);
        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(0, 0, t1);
        let paused = st.build_snapshot(ti(true, Some((0, 0)), true), t1);
        assert_eq!(paused.track_id, prev_tid);
        // app.rs 的 `track_changed = state.playing.track_id != np.track_id` 必须为 false：
        // 只有它为 true 才会 push(Effect::Lyric) → LyricLoading 覆盖态
        assert_eq!(st.last.as_ref().unwrap().track_id, paused.track_id);
        assert!(
            has_changed(st.last.as_ref().unwrap(), &paused),
            "只因 state 变化而发布，不需要重新加载歌词"
        );
    }

    /// ⑥ 稳态回归：继续之后的常规 `(0,0)` 读数不得让 `has_changed` 的
    /// 「位置大幅回退」判据误判成 seek，进而再发布一份归零快照。
    #[test]
    fn zero_player_steady_state_after_resume_never_publishes_a_reset() {
        let t0 = past_base();
        let mut st = state_with("晴天", t0);
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);

        let t1 = at(t0, 42_000);
        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(0, 0, t1);
        let paused = st.build_snapshot(ti(true, Some((0, 0)), true), t1);
        st.last = Some(paused);

        let t2 = at(t0, 52_000);
        st.parts.state = PlaybackState::Playing;
        st.apply_timeline_read(0, 0, t2);
        let resumed = st.build_snapshot(ti(true, Some((0, 0)), true), t2);
        assert_eq!(resumed.position_ms, 42_000);
        st.last = Some(resumed.clone());

        // 继续后 1s 的稳态 (0,0) 读数
        let t3 = at(t0, 53_000);
        st.apply_timeline_read(0, 0, t3);
        let steady = st.build_snapshot(ti(false, Some((0, 0)), true), t3);
        assert_eq!(steady.position_ms, 42_000, "稳态不得归零");
        assert!(
            !has_changed(&resumed, &steady),
            "稳态不该发布新快照，歌词高亮连续"
        );
    }

    /// ⑥ 代次守卫：不同曲目仍有不同 `track_id`，`has_changed` 仍会触发换歌发布。
    #[test]
    fn generation_guard_still_distinguishes_tracks() {
        let t0 = past_base();
        let mut parts = SessionParts::default();
        zero_player(&mut parts, "晴天", t0);
        let a = parts_track_id(&parts);
        parts.track.title = "七里香".to_string();
        let b = parts_track_id(&parts);
        assert_ne!(a, b);
        let sa = published(&parts, PlaybackState::Playing, 0, t0);
        let mut sb = sa.clone();
        sb.track_id = a;
        assert!(has_changed(&sa, &sb), "track_id 变化仍必须触发发布");
    }

    // ------------------------------------------------ TickInputs 收拢 + tick() 接线

    /// `tick_inputs()` 真值表：两个输入原样带出，`read_timeline_now` 由
    /// `timeline_read.is_some()` 派生 —— **`(0,0)` 也算「读过时间线」**
    /// （锚点规则 1 以该次读取时刻为锚点，切歌再由规则 2 覆盖）。
    #[test]
    fn tick_inputs_truth_table() {
        // (state_jumped, timeline_read, 期望的 read_timeline_now)
        let cases: [(bool, Option<(Ms, Ms)>, bool); 4] = [
            (false, None, false),
            (false, Some((0, 0)), true),
            (true, None, false),
            (true, Some((0, 0)), true),
        ];
        for (jumped, timeline, want_read) in cases {
            let got = tick_inputs(jumped, timeline);
            assert_eq!(got.state_jumped, jumped, "state_jumped 必须原样带出");
            assert_eq!(got.timeline_read, timeline, "timeline_read 必须原样带出");
            assert_eq!(
                got.read_timeline_now, want_read,
                "read_timeline_now 由 timeline_read.is_some() 派生（(0,0) 也算读过）"
            );
        }
        // 真实读数（非 (0,0)）同样算「读过时间线」
        assert!(tick_inputs(false, Some((12_000, 180_000))).read_timeline_now);
    }

    /// 端到端：`build_snapshot` 必须真的读 `TickInputs.state_jumped`。
    /// 同一组读数下，`state_jumped=true` 要走 `Carry`、`false` 要走 `Keep`；
    /// RV-C3 同类改坏（让该字段恒 false）会让本用例变红。
    #[test]
    fn build_snapshot_reads_state_jumped_from_tick_inputs() {
        let t0 = past_base();
        let t1 = at(t0, 42_000);

        // 上半：state_jumped = true → Carry
        let mut jumped = state_with("晴天", t0);
        let s0 = published(&jumped.parts, PlaybackState::Playing, 0, t0);
        jumped.last = Some(s0);
        jumped.parts.state = PlaybackState::Paused;
        jumped.apply_timeline_read(0, 0, t1);
        let carried = jumped.build_snapshot(
            TickInputs {
                state_jumped: true,
                timeline_read: Some((0, 0)),
                read_timeline_now: true,
                jumped_at: None,
            },
            t1,
        );
        assert_eq!(carried.position_ms, 42_000, "state_jumped=true 必须触发 Carry");
        assert_eq!(carried.position_at, t1);
        assert_eq!(jumped.parts.position_ms, 42_000, "Carry 基准必须写回 parts");

        // 下半：只有 state_jumped 不同（false）→ Keep，证明确实读了该字段
        let mut kept_state = state_with("晴天", t0);
        let s0b = published(&kept_state.parts, PlaybackState::Playing, 0, t0);
        kept_state.last = Some(s0b);
        kept_state.parts.state = PlaybackState::Paused;
        kept_state.apply_timeline_read(0, 0, t1);
        let kept = kept_state.build_snapshot(
            TickInputs {
                state_jumped: false,
                timeline_read: Some((0, 0)),
                read_timeline_now: true,
                jumped_at: None,
            },
            t1,
        );
        assert_eq!(kept.position_ms, 0, "state_jumped=false → Keep，不续接");
        assert_ne!(carried.position_ms, kept.position_ms, "两个分支必须可区分");
    }

    /// **静态结构断言**：`tick()` 必须把 `note_playback()` 判出的跃迁写进
    /// `state_jumped`，并经 `tick_inputs()` 交给 `build_snapshot`；G8-④ 起还要
    /// 把「跃迁真正发生的时刻」一起交进去，并在离开一首歌时把钟记进记忆。
    ///
    /// 这是唯一能抓住「赋值/传参被写死」的手段（无 COM 无法真正跑 `tick()`）：
    /// QA 的 RV-C3 把 `state_jumped = true` 改成 `= false` 时，它必须变红。
    /// v1.9.3 同理：把 `.at(jumped_at)` 改成 `.at(None)`、或删掉
    /// `remember_leaving(...)`，仿真测试全绿而线上修复静默失效 —— 必须由这里拦住。
    #[test]
    fn tick_wires_state_jump_into_snapshot_inputs() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/media/smtc.rs"
        ))
        .expect("必须能读到 smtc.rs 自身源码");
        let start = src
            .find("    fn tick(&mut self")
            .expect("tick 函数必须存在");
        let rest = &src[start..];
        // 方法体语句在 8 空格缩进，故第一个 4 空格缩进的 `}` 就是 tick 的收尾
        // （已人工核对：`tick` 内部没有 4 空格缩进的独立行）。
        let end = rest
            .find("\n    }\n")
            .map(|i| i + "\n    }\n".len())
            .unwrap_or(rest.len());
        let body = &rest[..end];

        assert!(
            body.contains("state_jumped = true"),
            "tick 必须在 note_playback() 判出跃迁的分支里置 state_jumped = true"
        );
        assert!(
            body.contains("tick_inputs(state_jumped, timeline_read)"),
            "tick 必须把真实输入交给 tick_inputs()，不许绕过"
        );
        assert!(
            !body.contains("tick_inputs(false"),
            "禁止把 state_jumped 写死成 false（G8-② 修复会静默失效）"
        );
        assert!(
            !body.contains("read_timeline_now ="),
            "read_timeline_now 必须由 tick_inputs() 派生，不许再立局部变量"
        );

        // ---- G8-④（v1.9.3）：Bug 1 / Bug 2 的接线 ----
        assert!(
            body.contains("transition_at(now, self.playback_events.take_after"),
            "跃迁时刻必须来自 PlaybackInfoChanged 的到达时刻（Bug 1 的抵消机制）"
        );
        assert!(
            body.contains(".at(jumped_at)"),
            "跃迁时刻必须交给 tick_inputs().at()，不许绕过（写死 None 会让 Bug 1 修复静默失效）"
        );
        assert!(
            !body.contains(".at(None)"),
            "禁止把跃迁时刻写死成 None"
        );
        assert!(
            body.contains("self.remember_leaving(Some(&snapshot), now)"),
            "tick 必须在推进 last 之前 remember_leaving（顺序反了等于没记，Bug 2 复发）"
        );
        assert!(
            body.find("remember_leaving").unwrap() < body.find("self.last = Some").unwrap(),
            "remember_leaving 必须排在 `self.last = Some(...)` 之前"
        );

        // `build_snapshot` 的 Reset 分支必须查记忆（否则切回同一首歌仍然归零）
        let bs = src
            .find("    fn build_snapshot(&mut self")
            .expect("build_snapshot 必须存在");
        let bs_rest = &src[bs..];
        let bs_end = bs_rest
            .find("\n    }\n")
            .map(|i| i + "\n    }\n".len())
            .unwrap_or(bs_rest.len());
        let bs_body = &bs_rest[..bs_end];
        assert!(
            bs_body.contains(".restore(&self.parts.aumid, track_id, now)"),
            "Reset 分支必须先查歌词钟记忆（跨应用/跨会话切回同一首歌不许归零）"
        );
        assert!(
            bs_body.contains("self.parts.timeline_at = Some(now)"),
            "Reset / Carry 写位置时必须同时写 timeline_at（位置与锚点严格配对）"
        );
        assert!(
            bs_body.contains("transition_at(now, jumped_at)"),
            "Carry 必须用 tick 交进来的跃迁时刻，不许自己退回 now"
        );
    }
}

// ============================================ G8-④（v1.9.3）歌词钟连续性回归
//
// 为什么单独立一个模块：这两个 bug 都是「**每个纯函数单看都对，串起来才漂**」
// ——`anchor_of` / `lyric_clock_action` / `lyric_clock_at` 全部有单元测试且全绿，
// 漂移是在「50ms 主循环 × 分层轮询 × 事件驱动 × 门控发布」组合出来时才出现的。
// 所以这里不重测纯函数，而是搭一台**确定性仿真**：用**生产同款**的
// `PollState::schedule` / `Cadence::note_*` / `apply_timeline_read` /
// `publish_point` / `remember_leaving` / `PlaybackEventClock` 把主循环跑起来，
// 时间全部注入（`Instant` 只做加法），**不 sleep、不碰 COM、不碰真实时钟**。
// 只有「跨进程读」这一步被替换成场景闭包（`obs_state` / `props` / `timeline`）。
//
// 这样仿真与生产走的是同一份判定代码：把修复改坏（例如让 `tick()` 不再传
// `jumped_at`、或让 `Reset` 不查记忆）时，这些测试会真正变红。
#[cfg(test)]
mod g8_lyric_clock_continuity {
    use super::*;

    const CLOUD: &str = "cloudmusic.exe";
    const SODA: &str = "汽水音乐";

    // ---- 本模块自带的脚手架（不跨模块借用别人模块里的私有 helper）----

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn test_index() -> Arc<MediaIndex> {
        MediaIndex::new(&crate::config::MediaConfig::default())
    }

    /// 网易云式 parts：有曲目、属性已成功读过、恒报 (0,0)。
    fn zero_player(parts: &mut SessionParts, title: &str, t0: Instant) {
        parts.track = Track {
            title: title.to_string(),
            artist: "周杰伦".to_string(),
            album: String::new(),
            duration_ms: 0,
        };
        parts.aumid = CLOUD.to_string();
        parts.process_name = CLOUD.to_string();
        parts.app_name = CLOUD.to_string();
        parts.props_ok = true;
        parts.state = PlaybackState::Playing;
        parts.position_ms = 0;
        parts.duration_ms = 0;
        parts.timeline_at = Some(t0);
    }

    /// 造一份「已广播」快照：`track_id` 与 parts 一致（不触发切歌）。
    fn published(
        parts: &SessionParts,
        state: PlaybackState,
        position: Ms,
        position_at: Instant,
    ) -> NowPlaying {
        NowPlaying {
            track_id: parts_track_id(parts),
            track: Some(effective_track(parts)),
            session: MediaSessionInfo {
                aumid: parts.aumid.clone(),
                app_name: parts.app_name.clone(),
                process_name: parts.process_name.clone(),
                ..Default::default()
            },
            state,
            position_ms: position,
            position_at,
            ..Default::default()
        }
    }

    /// `TickInputs` 构造（本模块内联，不跨模块借）。
    fn ti(state_jumped: bool, timeline_read: Option<(Ms, Ms)>, read_timeline_now: bool) -> TickInputs {
        TickInputs {
            state_jumped,
            timeline_read,
            read_timeline_now,
            jumped_at: None,
        }
    }

    /// 带完整曲目的快照（曲目不同 → `track_id` 不同）。
    fn snapshot(title: &str, state: PlaybackState, position: Ms) -> NowPlaying {
        let track = Track {
            title: title.to_string(),
            artist: String::from("a"),
            album: String::new(),
            duration_ms: 180_000,
        };
        NowPlaying {
            track_id: fnv1a64(track.key().as_bytes()),
            track: Some(track),
            session: MediaSessionInfo {
                aumid: CLOUD.to_string(),
                ..Default::default()
            },
            state,
            position_ms: position,
            ..Default::default()
        }
    }

    /// 一轮真实节奏的**驱动输入**（全部是纯数据/闭包，零 IO）。
    struct Scene<'a> {
        /// 快层读到的播放状态（真实状态 + 检测链路都被场景吸收进这个函数）
        obs_state: &'a dyn Fn(u64) -> PlaybackState,
        /// 慢层读到的 (aumid, 标题, 时长)
        props: &'a dyn Fn(u64) -> (&'static str, &'static str, Ms),
        /// 中层读到的 (位置, 时长)。网易云恒 `(0, 0)`。
        timeline: &'a dyn Fn(u64) -> (Ms, Ms),
        /// `PlaybackInfoChanged` 的到达时刻（空表 = 这条链路发不出事件，
        /// 退化为纯轮询；`u64::MAX` 之类的哨兵值一律不可达）。
        events: Vec<u64>,
        /// 播控标志（`can_next`）。翻它可以在**不换歌、不换状态**的前提下制造一次
        /// 「只有播控标志变化」的发布——`has_changed` 会放行，于是 `Keep` 分支
        /// 拼出来的位置/锚点会被真的送到 UI。这是「陈旧位置 @ 新鲜锚点」的唯一
        /// 可观测入口（v1.9.3 之前它是条静默的整段回退路径）。
        can_next: &'a dyn Fn(u64) -> bool,
    }

    /// 确定性主循环仿真（与 `run` + `tick` 的**同款顺序**，只省掉 COM 读）。
    struct ClockSim {
        st: PollState,
        base: Instant,
        /// 已推进到第几毫秒（50ms 节拍）
        cursor: u64,
        /// 已消费的事件下标
        next_event: usize,
        /// 本场景真实「累计播放时长」的积分器（真值，用来量误差）
        truth: PlayingIntegral,
    }

    /// 真实累计播放时长（按场景的真实状态时间轴积分，与歌词钟无关）。
    #[derive(Default)]
    struct PlayingIntegral {
        /// (时刻, 状态)：真实状态时间轴
        marks: Vec<(u64, PlaybackState)>,
    }

    impl PlayingIntegral {
        fn mark(&mut self, t: u64, state: PlaybackState) {
            self.marks.push((t, state));
        }
        fn at(&self, t: u64) -> Ms {
            let mut total = 0i64;
            for i in 0..self.marks.len() {
                let (start, state) = self.marks[i];
                if start >= t {
                    break;
                }
                let end = if i + 1 < self.marks.len() {
                    self.marks[i + 1].0.min(t)
                } else {
                    t
                };
                if state == PlaybackState::Playing && end > start {
                    total += (end - start) as i64;
                }
            }
            total
        }
    }

    impl ClockSim {
        fn new(base: Instant) -> Self {
            Self {
                st: PollState::new(Arc::new(AtomicBool::new(false)), test_index()),
                base,
                cursor: 0,
                next_event: 0,
                truth: PlayingIntegral::default(),
            }
        }

        fn now(&self, ms: u64) -> Instant {
            self.base + Duration::from_millis(ms)
        }

        /// 换场景时重置事件游标（各场景的事件表是独立的，绝对时刻可能不连续）。
        fn begin_scene(&mut self) {
            self.next_event = 0;
        }

        /// 跑到 `to_ms`（含），每 50ms 一拍，**与 `run` 的循环体逐句对应**。
        fn run_to(&mut self, to_ms: u64, scene: &Scene<'_>) {
            while self.cursor + 50 <= to_ms {
                self.cursor += 50;
                let t = self.cursor;
                let now = self.now(t);

                // ---- 事件到达（真实回调：置脏位 + 记到达时刻）----
                while let Some(&at) = scene.events.get(self.next_event) {
                    if at > t {
                        break;
                    }
                    self.st.playback_events.stamp_at(self.now(at));
                    self.st.dirty.store(true, Ordering::Relaxed);
                    self.next_event += 1;
                }

                let (plan, forced) =
                    self.st
                        .schedule(now, self.st.dirty.load(Ordering::Relaxed));
                if forced {
                    self.st.dirty.store(false, Ordering::Relaxed);
                }
                if plan.any() {
                    self.tick_like(plan, now, t, scene);
                }
            }
        }

        /// `tick()` 去掉 COM 读之后的等价体（顺序逐句照抄）。
        fn tick_like(&mut self, mut plan: Plan, now: Instant, t: u64, scene: &Scene<'_>) {
            self.st.last_tick = now;

            // ---- 快层 ----
            let mut timeline_read: Option<(Ms, Ms)> = None;
            let mut state_jumped = false;
            let mut jumped_at: Option<Instant> = None;
            if plan.playback {
                let prev_obs = self.st.cadence.last_playback_obs();
                let state = (scene.obs_state)(t);
                if self.st.cadence.note_playback(now, state) {
                    plan.timeline = true;
                    plan.props = true;
                    state_jumped = true;
                    jumped_at =
                        Some(transition_at(now, self.st.playback_events.take_after(prev_obs)));
                }
                self.st.parts.state = state;
            }

            // ---- 中层 ----
            if plan.timeline {
                let (position, duration) = (scene.timeline)(t);
                self.st.cadence.note_timeline(now, position, duration);
                self.st.apply_timeline_read(position, duration, now);
                timeline_read = Some((position, duration));
            }

            // ---- 慢层 ----
            if plan.props {
                let (aumid, title, duration) = (scene.props)(t);
                self.st.parts.aumid = aumid.to_string();
                self.st.parts.app_name = aumid.to_string();
                self.st.parts.process_name = aumid.to_string();
                self.st.parts.track = Track {
                    title: title.to_string(),
                    artist: "a".to_string(),
                    album: String::new(),
                    duration_ms: 0,
                };
                self.st.parts.props_ok = true;
                self.st.parts.can_next = (scene.can_next)(t);
                self.st.cadence.note_props(now);
                // 时长唯一来源是中层；场景里 props 的时长只用于「汽水式」场景
                if duration > 0 {
                    self.st.parts.duration_ms = duration;
                }
            }
            if !self.st.parts.props_ok {
                return;
            }

            // ---- 发布点（生产同一个函数）----
            let inputs = tick_inputs(state_jumped, timeline_read).at(jumped_at);
            if let Some(snapshot) = self.st.publish_point(inputs, now) {
                self.st.remember_leaving(Some(&snapshot), now);
                self.st.last = Some(snapshot);
            }
        }

        /// UI 侧 `lyric_time()` 的口径：已发布快照外推到 `t`。
        fn clock(&self, t: u64) -> Ms {
            match self.st.last.as_ref() {
                Some(last) => lyric_clock_at(last, self.now(t)),
                None => 0,
            }
        }

        /// 「会话被系统短暂丢弃」：与 `tick` 的 session-cleared 分支等价。
        fn drop_session(&mut self, now: Instant) {
            self.st.remember_leaving(None, now);
            self.st.last = None;
            self.st.parts = SessionParts::default();
            self.st.cadence = Cadence::default();
        }
    }

    // ------------------------------------------------------------ 场景辅助

    /// 网易云式：恒 `(0,0)`、标题固定、aumid 固定。
    fn cloud_props(_: u64) -> (&'static str, &'static str, Ms) {
        (CLOUD, "晴天", 0)
    }
    fn cloud_timeline(_: u64) -> (Ms, Ms) {
        (0, 0)
    }

    /// 由「真实状态时间轴 + 每条跃迁的检测延迟」生成快层读数闭包。
    ///
    /// `delays[i]` 是第 i 条跃迁的**检测延迟**（真实时刻 → 被快层读到的时刻），
    /// 与真实世界的「事件驱动 ~60ms / 纯轮询最坏 500ms」对应。
    fn observed_timeline(
        marks: &[(u64, PlaybackState)],
        delays: &[u64],
    ) -> Vec<(u64, PlaybackState)> {
        observed_timeline_from(marks[0].1, marks, delays)
    }

    /// 同上，但显式给出「本场景开始前播放器处于什么状态」——换场景时必须用，
    /// 否则 `(0, Playing)` 会在场景起点凭空造出一次「暂停→播放」跃迁。
    fn observed_timeline_from(
        initial: PlaybackState,
        marks: &[(u64, PlaybackState)],
        delays: &[u64],
    ) -> Vec<(u64, PlaybackState)> {
        let mut out = vec![(0u64, initial)];
        for (i, &(at, state)) in marks.iter().enumerate() {
            out.push((at + delays.get(i).copied().unwrap_or(0), state));
        }
        out
    }

    fn state_of(timeline: &[(u64, PlaybackState)], t: u64) -> PlaybackState {
        let mut s = timeline[0].1;
        for (at, st) in timeline {
            if *at <= t {
                s = *st;
            }
        }
        s
    }

    // ------------------------------------------------------------ Bug 1

    /// **Bug 1：暂停/继续 N 轮之后，歌词钟的滞后不许随 N 累积。**
    ///
    /// 场景（用户真机节奏）：网易云恒 `(0,0)`；每轮「播一会 → 暂停 → 再播」，
    /// 暂停时长逐轮不同（所以请求相位逐轮不同）。每轮的检测延迟**可参数化**，
    /// 含最坏情况 500ms；同时每条跃迁都会产生一次 `PlaybackInfoChanged`（
    /// 用户日志实证网易云会发，检测约 60ms），事件到达时刻随延迟一起给出。
    ///
    /// 断言：
    /// 1. `|clock(end) - 真实累计播放时长| ≤ 一个轮询间隔`；
    /// 2. N=10 与 N=20 的误差**同阶**（不许线性增长）。
    fn run_pause_resume_rounds(rounds: usize, detect: u64, event_latency: u64) -> Ms {
        let base = Instant::now() - Duration::from_millis(2_000_000);
        let mut sim = ClockSim::new(base);
        // 真实状态时间轴：先播 5s，然后每轮 7s 播 / 变长暂停
        let pauses = [3_000u64, 1_000, 4_000, 2_000, 6_000, 1_500, 5_000, 800, 3_500, 2_500];
        let mut marks: Vec<(u64, PlaybackState)> = vec![(0, PlaybackState::Playing)];
        let mut events: Vec<u64> = vec![0];
        let mut t = 5_000u64;
        for i in 0..rounds {
            let tp = t;
            let tr = tp + pauses[i % pauses.len()];
            marks.push((tp, PlaybackState::Paused));
            marks.push((tr, PlaybackState::Playing));
            // 事件到达 = 真实跃迁 + 事件延迟；dirty 位一律置上（用户日志：网易云会发）
            events.push(tp.saturating_add(event_latency));
            events.push(tr.saturating_add(event_latency));
            t = tr + 7_000;
        }
        marks.push((t, PlaybackState::Paused));
        events.push(t.saturating_add(event_latency));
        for &(at, state) in marks.iter() {
            sim.truth.mark(at, state);
        }
        let delays = vec![detect; marks.len() - 1];
        let obs = observed_timeline(&marks, &delays);
        let end = t + 3_000;
        let scene = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events,
            can_next: &|_| true,
        };
        sim.run_to(end, &scene);
        sim.clock(end) - sim.truth.at(end)
    }

    /// ① 有事件（用户真机常态，检测 ~60ms）：误差恒定在 1 个检测延迟量级，
    ///    **N=10 与 N=20 完全同阶**。
    #[test]
    fn pause_resume_lag_does_not_accumulate_with_events() {
        // 事件延迟取最坏情况的 500ms 也不能让误差累积：两个方向都用同一个
        // 事件时刻锚定，逐轮误差相等 → 逐轮抵消。
        for &(detect, latency) in &[(60u64, 0u64), (60, 120), (500, 0), (500, 250)] {
            let e10 = run_pause_resume_rounds(10, detect, latency);
            let e20 = run_pause_resume_rounds(20, detect, latency);
            assert!(
                e10.abs() <= limits::MEDIA_POLL_MS as Ms,
                "detect={detect} latency={latency}: N=10 误差 {} 超过一个轮询间隔",
                e10
            );
            assert!(
                e20.abs() <= limits::MEDIA_POLL_MS as Ms,
                "detect={detect} latency={latency}: N=20 误差 {} 超过一个轮询间隔",
                e20
            );
            assert!(
                (e20 - e10).abs() <= 100,
                "detect={detect} latency={latency}: 误差随轮数增长（N=10 {} → N=20 {}），\
                 说明跃迁时刻没锚到事件上（Bug 1 复发）",
                e10,
                e20
            );
        }
    }

    /// ② 没有任何事件（纯轮询兜底）：单轮误差 ≤ 一个轮询间隔；这里让两个方向
    ///    的检测延迟**相等**（同一条规则），误差必须逐轮抵消、绝不随 N 增长。
    ///
    /// 注：两个方向延迟**不等**时误差是 Σ(d−r)（信息论上限，未知道跃迁时刻
    /// 时无法消除），这一项的处置见报告「风险与遗留」。
    #[test]
    fn pause_resume_lag_is_stable_without_events_when_delays_match() {
        for &detect in &[0u64, 60, 250, 500] {
            let e10 = run_pause_resume_rounds(10, detect, u64::MAX);
            let e20 = run_pause_resume_rounds(20, detect, u64::MAX);
            // 纯轮询下每一拍的检测相位都可能不同，单轮残差是零均值的随机量，
            // 实测漂移落在「两个轮询间隔」以内且不随轮数增长
            // （实测：detect=0 → 100/100ms，60 → 100/-400ms，250 → 600/100ms）。
            assert!(
                e10.abs() <= 2 * limits::MEDIA_POLL_MS as Ms
                    && e20.abs() <= 2 * limits::MEDIA_POLL_MS as Ms,
                "detect={detect}: 误差超过两个轮询间隔（{} / {}）",
                e10,
                e20
            );
            // 再多跑 10 轮也只许再漂一个轮询间隔；绝不允许 (d−r)×N 那种线性
            // 增长（那会让 20 轮的误差是 10 轮的两倍以上，比如 -500 → -1000）。
            assert!(
                e20.abs() <= e10.abs() + limits::MEDIA_POLL_MS as Ms,
                "detect={detect}: 误差随轮数线性增长（N=10 {} → N=20 {}）",
                e10,
                e20
            );
        }
    }

    /// ③ **Carry 必须锚在「跃迁真正发生的时刻」，不是「检测到它的那一刻」。**
    ///
    /// 这一格是 Bug 1 的记账核心：暂停/继续两个方向用**同一个**时刻来源时，
    /// 「多算的检测延迟」与「少算的检测延迟」逐轮相等 → 逐轮抵消；用检测时刻
    /// 则退化成「谁先被读到」，误差按 (d−r)/轮 累加。
    #[test]
    fn carry_anchors_at_the_transition_instant_not_the_detection_tick() {
        let t0 = Instant::now() - Duration::from_millis(200_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);
        st.last = Some(published(&st.parts, PlaybackState::Playing, 42_000, t0));

        // 真暂停发生在 t0+42s，直到 t0+48s 才被读到（检测延迟 6s 的极端例子）
        let jumped_at = at(t0, 42_000);
        let now = at(t0, 48_000);
        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(0, 0, now);
        let paused = st.build_snapshot(
            TickInputs {
                state_jumped: true,
                timeline_read: Some((0, 0)),
                read_timeline_now: true,
                jumped_at: Some(jumped_at),
            },
            now,
        );
        assert_eq!(
            paused.position_at, jumped_at,
            "Carry 的锚点必须是跃迁时刻（暂停/继续同源才能逐轮抵消）"
        );
        assert_eq!(
            paused.position_ms, 84_000,
            "位置要外推到跃迁时刻（42s 起点 + 42s 播放），不是检测时刻的 90s"
        );

        // 恢复方向同理：锚在跃迁时刻，UI 从那一刻继续外推
        st.last = Some(paused.clone());
        let resume_at = at(t0, 50_000);
        let detect = at(t0, 50_060);
        st.parts.state = PlaybackState::Playing;
        st.apply_timeline_read(0, 0, detect);
        let resumed = st.build_snapshot(
            TickInputs {
                state_jumped: true,
                timeline_read: Some((0, 0)),
                read_timeline_now: true,
                jumped_at: Some(resume_at),
            },
            detect,
        );
        assert_eq!(resumed.position_ms, 84_000);
        assert_eq!(resumed.position_at, resume_at, "恢复锚点 = 跃迁时刻");
        assert_eq!(
            lyric_clock_at(&resumed, at(t0, 51_000)),
            85_000,
            "从跃迁时刻起算，检测延迟那 60ms 不再被丢掉"
        );
    }

    /// ③ **Bug 1 的判定性测试（陈旧位置 @ 新鲜锚点）**。
    ///
    /// 播放中同一拍里既有 `(0,0)` 时间线读数、又有播控标志变化时，`Keep` 分支
    /// 拼出来的快照必须让「位置 ↔ 锚点」严格配对：锚点只能是那个位置的采样
    /// 时刻，**绝不能**被 `(0,0)` 读数刷成此刻。v1.9.2 会刷新它，于是
    /// 「42s 的位置 @ 10s 后的锚点」被真的广播出去 —— UI 按 42s 重新起表，
    /// 已播的 10s 被整段吃掉（`has_changed` 的 `delta == 0` 只是侥幸没让它
    /// 常态复现）。每发生一次就永久少一段，累积起来正是「歌词越来越靠后」。
    #[test]
    fn stale_zero_read_cannot_rewind_the_clock() {
        let t0 = Instant::now() - Duration::from_millis(200_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        // 已续接到 42s：位置 42_000，采样时刻 t0，can_next = false
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);
        st.parts.can_next = false;
        st.last = Some(published(&st.parts, PlaybackState::Playing, 42_000, t0));

        // 10s 后的一次稳态 (0,0) 读数，同一拍里 can_next 翻成 true（只有它变了）
        let t1 = at(t0, 10_000);
        st.apply_timeline_read(0, 0, t1);
        st.parts.can_next = true;
        let snap = st
            .publish_point(ti(false, Some((0, 0)), true), t1)
            .expect("can_next 变了 → 必须发布（否则这条路径不可观测）");

        assert_eq!(snap.position_ms, 42_000, "(0,0) 不许改位置");
        assert_eq!(
            snap.position_at, t0,
            "锚点必须仍是 42_000 的采样时刻，不许被 (0,0) 读数刷成 t1"
        );
        assert_eq!(
            lyric_clock_at(&snap, t1),
            52_000,
            "外推出的钟必须是 42s + 10s，任何回退都是 Bug 1"
        );
        // 下一拍真的发布出去（同一个锚点），钟继续前进
        let t2 = at(t0, 11_000);
        st.apply_timeline_read(0, 0, t2);
        st.parts.can_next = false;
        st.last = Some(snap);
        let snap2 = st
            .publish_point(ti(false, Some((0, 0)), true), t2)
            .expect("can_next 再次变化 → 发布");
        assert_eq!(
            lyric_clock_at(&snap2, t2),
            53_000,
            "连续两次发布都不许吃掉时间"
        );
    }

    /// ③ 跃迁时刻的选择本身：有事件用事件、没事件用 `now`、晚于 `now` 丢弃。
    #[test]
    fn transition_at_prefers_event_and_rejects_future() {
        let t0 = Instant::now();
        assert_eq!(
            transition_at(at(t0, 1_000), Some(at(t0, 940))),
            at(t0, 940),
            "有事件时刻就必须用它（暂停/继续两个方向同源）"
        );
        assert_eq!(
            transition_at(at(t0, 1_000), None),
            at(t0, 1_000),
            "没有事件时刻 → 退回本轮 now（v1.9.2 行为）"
        );
        assert_eq!(
            transition_at(at(t0, 1_000), Some(at(t0, 1_050))),
            at(t0, 1_000),
            "晚于 now 的异常时间戳必须丢弃，绝不让钟倒退"
        );
    }

    /// ④ 事件时钟：陈旧事件（早于上一次快层观测）必须被丢掉，且只消费一次。
    #[test]
    fn playback_event_clock_drops_stale_stamps_and_consumes_once() {
        let clock = PlaybackEventClock::default();
        let t0 = Instant::now();
        assert!(clock.take_after(None).is_none(), "没事件时不许凭空造时刻");
        clock.stamp_at(at(t0, 500));
        assert_eq!(
            clock.take_after(Some(at(t0, 600))),
            None,
            "早于上一次观测的时刻是更早一次跃迁的残留，必须丢弃"
        );
        assert!(clock.take_after(None).is_none(), "取走一次就必须消费掉");
        clock.stamp_at(at(t0, 900));
        assert_eq!(clock.take_after(Some(at(t0, 600))), Some(at(t0, 900)));
    }

    // ------------------------------------------------------------ Bug 2

    /// **Bug 2：跨应用返回同一首歌，必须接着离开时的位置。**
    ///
    /// 完整 tick 序列（用户日志 02:25:19 → 02:26:50 的复刻）：
    /// 网易云播到 63s → 暂停 → 切到汽水（真实上报位置）→ 汽水暂停 →
    /// 切回网易云（时间线又是 `(0,0)`）。
    #[test]
    fn cross_app_return_restores_lyric_clock_instead_of_zero() {
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);

        // 汽水：真实位置 = 从 40s 起按 1:1 走表（只在被读时给出）
        let soda_timeline = |t: u64| -> (Ms, Ms) {
            let pos = 40_000 + (t.saturating_sub(20_000)) as Ms;
            (pos.min(220_000), 220_000)
        };
        let soda_props = |_: u64| -> (&'static str, &'static str, Ms) { (SODA, "月半", 220_000) };

        // 阶段 1：网易云播放（恒 (0,0)）到 20s，然后暂停到 40s
        let cloud_marks = vec![
            (0u64, PlaybackState::Playing),
            (20_000, PlaybackState::Paused),
        ];
        let cloud_obs = observed_timeline(&cloud_marks, &[60, 60]);
        for &(at, st) in &cloud_marks {
            sim.truth.mark(at, st);
        }
        let phase1 = Scene {
            obs_state: &|x| state_of(&cloud_obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![0, 20_060],
            can_next: &|_| true,
        };
        sim.run_to(20_500, &phase1);
        let paused_at = sim.clock(20_500);
        assert!(
            (paused_at - 20_000).abs() <= 100,
            "前置：网易云应暂停在 20s 附近，实际 {paused_at}"
        );

        // 阶段 2：汽水成为当前会话（真实位置），随后暂停
        let soda_marks = vec![
            (40_000u64, PlaybackState::Playing),
            (50_000, PlaybackState::Paused),
        ];
        let soda_obs = observed_timeline_from(
            PlaybackState::Paused,
            &soda_marks,
            &[60, 60],
        );
        for &(at, st) in &soda_marks {
            sim.truth.mark(at, st);
        }
        let phase2 = Scene {
            obs_state: &|x| state_of(&soda_obs, x),
            props: &soda_props,
            timeline: &soda_timeline,
            events: vec![40_060, 50_060],
            can_next: &|_| true,
        };
        // 汽水不参与「网易云那首歌」的真值积分，只跑时钟
        sim.truth.mark(40_000, PlaybackState::Paused);
        sim.begin_scene();
        sim.run_to(50_500, &phase2);
        assert_eq!(
            sim.st.last.as_ref().unwrap().session.aumid,
            SODA,
            "前置：此刻显示的是汽水"
        );

        // 阶段 3：切回网易云继续播放（时间线回到 (0,0)）
        let back_marks = vec![
            (60_000u64, PlaybackState::Playing),
            (100_000, PlaybackState::Paused),
        ];
        let back_obs = observed_timeline_from(
            PlaybackState::Paused,
            &back_marks,
            &[60, 60],
        );
        sim.truth.mark(60_000, PlaybackState::Playing);
        let phase3 = Scene {
            obs_state: &|x| state_of(&back_obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![60_060, 100_060],
            can_next: &|_| true,
        };
        sim.begin_scene();
        sim.run_to(60_500, &phase3);
        let resumed = sim.clock(60_500);
        // 复原点是「切回来那一拍」，之后钟继续正常走表，所以允许一个轮询间隔的
        // 正向偏差；但绝不许低于离开时的位置，更不许归零。
        assert!(
            resumed >= paused_at - 100,
            "切回网易云不许比离开时的 {paused_at}ms 更靠后（归零 = Bug 2）：实际 {resumed}ms"
        );
        assert!(
            resumed - paused_at <= limits::MEDIA_POLL_MS as Ms + 100,
            "切回网易云必须接着离开时的 {paused_at}ms，实际 {resumed}ms（差了 {}ms）",
            resumed - paused_at
        );
        assert!(resumed > 19_000, "绝不能回到 0（歌词从头开始）");

        // 继续播 40s：钟必须在「离开时的位置」之上精确走 40s，
        // 不许被记忆再拽回去，也不许多走。
        sim.run_to(100_500, &phase3);
        let after = sim.clock(100_500);
        assert!(
            (after - (paused_at + 40_000)).abs() <= 200,
            "续接之后必须精确走表：期望 {}，实际 {after}",
            paused_at + 40_000
        );
    }

    /// **Bug 2 的另一半：真正的新歌仍然从 0 开始（Reset 语义不许被记忆破坏）。**
    #[test]
    fn brand_new_track_still_starts_at_zero() {
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);
        let marks = vec![(0u64, PlaybackState::Playing)];
        let obs = observed_timeline(&marks, &[]);
        let p1 = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![0],
            can_next: &|_| true,
        };
        sim.run_to(30_000, &p1);
        assert!((sim.clock(30_000) - 30_000).abs() <= 100);

        // 同 App 内切到一首从没听过的歌（不在记忆里）→ 必须从 0 起
        let other = |_: u64| -> (&'static str, &'static str, Ms) { (CLOUD, "七里香", 0) };
        let p2 = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &other,
            timeline: &cloud_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.run_to(31_000, &p2);
        assert_eq!(
            sim.st.last.as_ref().unwrap().position_ms,
            0,
            "没听过的歌必须 Reset 归零（切歌语义不破）"
        );
        // 关键不变量：Reset 的锚点必须在「切歌被慢层发现」之后，
        // 不许把切歌前的走表时间算进新歌（那正是「从第一句重唱」的反面——
        // 时钟凭空领先）。
        let reset = sim.st.last.as_ref().unwrap();
        assert!(
            reset.position_at >= sim.now(30_000),
            "Reset 锚点不能早于切歌时刻"
        );
        assert!(
            sim.clock(31_000) <= (31_000 - 30_000) as Ms,
            "Reset 之后钟最多只走「切歌被发现的时刻 → 现在」这一段：实际 {}",
            sim.clock(31_000)
        );
    }

    /// 同 App 内「切走再切回**同一首**」：不许再丢掉之前的进度
    /// （用户日志原始症状：141s 的曲子切回来变成 0，之后一直慢 141s）。
    #[test]
    fn returning_to_same_track_within_app_keeps_progress() {
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);
        let marks = vec![(0u64, PlaybackState::Playing), (60_000, PlaybackState::Paused)];
        let obs = observed_timeline(&marks, &[60, 60]);
        let p1 = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![0, 60_060],
            can_next: &|_| true,
        };
        sim.run_to(61_000, &p1);
        let left_at = sim.clock(61_000);
        assert!((left_at - 60_000).abs() <= 100, "前置：暂停在 60s 附近");

        // 之后全程「暂停」：时钟不再自走，读数就是复原值本身。
        let frozen = |_: u64| PlaybackState::Paused;
        // 切到另一首（同 App、同 (0,0)，读不到位置）→ 记进记忆后归零
        let other = |_: u64| -> (&'static str, &'static str, Ms) { (CLOUD, "七里香", 0) };
        let p2 = Scene {
            obs_state: &frozen,
            props: &other,
            timeline: &cloud_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        // 窗口必须长过慢层（2s）才一定读得到新曲目
        sim.run_to(64_000, &p2);
        assert_eq!(
            sim.st.last.as_ref().unwrap().position_ms,
            0,
            "前置：另一首没听过 → 归零"
        );
        // 切回同一首
        let back = Scene {
            obs_state: &frozen,
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.run_to(67_000, &back);
        let restored = sim.st.last.as_ref().unwrap().position_ms;
        assert!(
            (restored - left_at).abs() <= 100,
            "切回同一首歌必须复原 {left_at}ms，实际 {restored}ms"
        );
    }

    /// 会话被系统短暂丢弃（`session cleared`）后重新出现：同样必须复原。
    #[test]
    fn session_drop_and_return_restores_lyric_clock() {
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);
        let marks = vec![(0u64, PlaybackState::Playing), (40_000, PlaybackState::Paused)];
        let obs = observed_timeline(&marks, &[60, 60]);
        let scene = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![0, 40_060],
            can_next: &|_| true,
        };
        sim.run_to(41_000, &scene);
        let left_at = sim.clock(41_000);
        assert!((left_at - 40_000).abs() <= 100);

        sim.drop_session(sim.now(41_500));
        assert!(sim.st.last.is_none(), "前置：会话确实被丢弃");
        sim.run_to(44_000, &scene);
        let restored = sim.st.last.as_ref().unwrap().position_ms;
        assert!(
            (restored - left_at).abs() <= 100,
            "会话回来后必须复原 {left_at}ms，实际 {restored}ms（归零 = 歌词从头）"
        );
    }

    // ------------------------------------------- G8-⑤：(0, 真实时长) 不是位置

    /// **`(0, 真实时长)` 不是可信位置（真机日志实证 17 次）。**
    ///
    /// 时长有值只说明播放器知道这首歌多长，**不代表位置已经上报**。旧判据是
    /// 「时长有没有值」（`Some(_) => false`），于是一条 `(0,D)` 读数撞在
    /// **状态跃迁那一拍**就会绕过 Carry、把钟砸到 0。
    ///
    /// 真机统计（`session changed:` 行）：`position=0 && duration>0` 共 17 次
    /// —— 汽水 15 次（`duration=60000` 11 次，其余 4 次）、Edge 2 次；
    /// 网易云 716 次 `position=0` 的 duration **恒为 0**（0 次），本次没触发。
    /// 那 17 次都发生在换曲瞬间（后续读数证明真实位置确实 ≈0），属合法场景，
    /// 所以修复必须**不改变它们的最终结果**（下面第三段就锁这个）。
    #[test]
    fn zero_position_with_real_duration_is_not_a_trusted_position() {
        // ① 缓存层：`(0,D)` 会写入位置 0（播放器说位置是 0，就得照写——真实位置
        //    播放器把进度拖回开头时靠它把钟拉回 0），但**必须与采样时刻配对**：
        //    绝不允许出现「旧位置 @ 新时刻」（G8-④ 的病）或「新位置 @ 旧时刻」。
        let t0 = Instant::now() - Duration::from_millis(200_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);
        let t9 = at(t0, 9_000);
        st.apply_timeline_read(0, 180_000, t9);
        assert_eq!(
            st.parts.position_ms, 0,
            "(0,D) 写 0：seek 回开头/重播到 0 必须能把钟拉回 0"
        );
        assert_eq!(
            st.parts.timeline_at,
            Some(t9),
            "(0,D) 写位置就必须同时写采样时刻（严格配对）"
        );
        assert_eq!(st.parts.duration_ms, 180_000, "时长是真信息，必须收下");

        // ② 判定层：跃迁那一拍必须 Carry（旧判据会走 Keep 把钟砸到 0）
        assert_eq!(
            lyric_clock_action(Some((0, 180_000)), false, true, false, true),
            LyricClockAction::Carry
        );
        let mut jumped = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut jumped.parts, "晴天", t0);
        // 时长先落定：`Track::key()` 含 duration/3000，时长一变 `track_id` 就变，
        // 那会走 `Reset` 把本用例的意图（状态跃迁 Carry）掩盖掉。
        jumped.parts.duration_ms = 180_000;
        jumped.parts.position_ms = 42_000;
        jumped.parts.timeline_at = Some(t0);
        jumped.last = Some(published(&jumped.parts, PlaybackState::Playing, 42_000, t0));
        jumped.parts.state = PlaybackState::Paused;
        let t1 = at(t0, 10_000);
        jumped.apply_timeline_read(0, 180_000, t1);
        let snap = jumped
            .publish_point(ti(true, Some((0, 180_000)), true), t1)
            .expect("状态跃迁必须发布");
        assert_eq!(
            snap.position_ms, 52_000,
            "(0,D) + 状态跃迁：必须续接到 42s+10s，绝不许砸到 0"
        );

        // ③ 合法场景不受影响：新曲目第一份读数就是 `(0, D)`（第一句在 0ms）
        let mut fresh = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        fresh.parts.props_ok = true;
        fresh.parts.aumid = CLOUD.to_string();
        fresh.parts.process_name = CLOUD.to_string();
        fresh.parts.app_name = CLOUD.to_string();
        fresh.parts.track = Track {
            title: "晴天".to_string(),
            artist: "a".to_string(),
            album: String::new(),
            duration_ms: 0,
        };
        fresh.parts.state = PlaybackState::Playing;
        fresh.apply_timeline_read(0, 180_000, t0);
        let first = fresh
            .publish_point(ti(false, Some((0, 180_000)), true), t0)
            .expect("首帧必须发布");
        assert_eq!(first.position_ms, 0, "新歌本来就该从 0 起表");
        assert!(first.can_seek, "时长到了 → 进度条可拖（can_seek）");
        // 紧接着的真实位置读数照常生效（稳步前进不发布，靠 UI 外推；
        // 这里断言「外推出来的钟 == 真实位置」，这才是播放器真正的口径）
        fresh.last = Some(first);
        fresh.apply_timeline_read(600, 180_000, at(t0, 600));
        let next = fresh.build_snapshot(ti(false, Some((600, 180_000)), true), at(t0, 600));
        assert_eq!(next.position_ms, 600, "真实读数照常写入");
        assert_eq!(
            lyric_clock_at(&next, at(t0, 600)),
            600,
            "外推钟 == 真实位置"
        );
    }

    /// 换曲瞬间的 `(0,D)`（真机那 17 次的形态）：新曲从 0 起表、后续读数正常接上，
    /// 旧曲的位置绝不许被带进新曲。
    #[test]
    fn zero_duration_boundary_read_does_not_leak_old_position() {
        let t0 = Instant::now() - Duration::from_millis(200_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);
        st.last = Some(published(&st.parts, PlaybackState::Playing, 42_000, t0));

        // 切到另一首（真实上报位置的播放器）：读数就是 (0, 180_000)
        st.parts.track.title = "七里香".to_string();
        st.parts.duration_ms = 180_000;
        let t1 = at(t0, 43_000);
        st.apply_timeline_read(0, 180_000, t1);
        let snap = st
            .publish_point(ti(false, Some((0, 180_000)), true), t1)
            .expect("换曲必须发布");
        assert_eq!(snap.position_ms, 0, "新曲必须从 0 起表");
        assert_eq!(snap.position_at, t1, "锚点 = 新曲的采样时刻");

        // 2.2s 后真实位置 2286（真机日志里就是这个形态）→ 正常跟上。
        // 注意这里**不该发布**：位置稳步前进被刻意排除在 `has_changed` 之外
        // （否则每秒重绘），所以断言 `build_snapshot` 的结果。
        let t2 = at(t0, 45_200);
        st.last = Some(snap);
        st.apply_timeline_read(2_286, 180_000, t2);
        assert!(
            st.publish_point(ti(false, Some((2_286, 180_000)), true), t2)
                .is_none(),
            "稳步前进不许发布（每秒重绘）"
        );
        let next = st.build_snapshot(ti(false, Some((2_286, 180_000)), true), t2);
        assert_eq!(next.position_ms, 2_286);
        assert_eq!(lyric_clock_at(&next, t2), 2_286);
    }

    // ------------------------------------------- G8-⑤：重播保护

    /// **反向场景：同一首歌真的从头重播，不许被旧记忆带偏。**
    ///
    /// 判据是 [`restorable`]：只有「离开时**暂停**」的记忆才续接。
    #[test]
    fn genuine_replay_is_not_dragged_by_memory() {
        let t0 = Instant::now() - Duration::from_millis(200_000);
        // ① 纯函数真值表：只有 Paused 可续接
        for state in [
            PlaybackState::Playing,
            PlaybackState::Paused,
            PlaybackState::NoSession,
            PlaybackState::Fallback,
        ] {
            assert_eq!(
                restorable(state),
                state == PlaybackState::Paused,
                "只有「离开时暂停」才可续接（{}）",
                state.as_str()
            );
        }

        // ② 记忆层：在播离开 → restore 必须拒绝
        let mut mem = LyricClockMemory::default();
        let mut playing = snapshot("晴天", PlaybackState::Playing, 60_000);
        playing.session.aumid = CLOUD.to_string();
        mem.remember(&playing, t0);
        assert!(
            mem.restore(CLOUD, playing.track_id, at(t0, 5_000)).is_none(),
            "离开时在播 ⇒ 不续接（可能是在列表里重新点播 = 从头播）"
        );
        // 换成暂停离开 → 可续接
        let mut paused = playing.clone();
        paused.state = PlaybackState::Paused;
        mem.remember(&paused, t0);
        assert!(mem.restore(CLOUD, paused.track_id, at(t0, 5_000)).is_some());

        // ③ 端到端：在播时切走、再切回来（用户可能点了重播）→ 必须归零，
        //    绝不许被记忆拖到 60s（那会让歌词唱到还没播的句子）
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);
        let marks = vec![(0u64, PlaybackState::Playing)];
        let obs = observed_timeline(&marks, &[]);
        let scene = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![0],
            can_next: &|_| true,
        };
        sim.run_to(60_000, &scene);
        assert!((sim.clock(60_000) - 60_000).abs() <= 100, "前置：播到 60s");
        // 切走（此时仍在播 → last.state = Playing）
        let other = |_: u64| -> (&'static str, &'static str, Ms) { (CLOUD, "七里香", 0) };
        let away = Scene {
            obs_state: &|_| PlaybackState::Playing,
            props: &other,
            timeline: &cloud_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.begin_scene();
        sim.run_to(64_000, &away);
        // 切回来
        let back = Scene {
            obs_state: &|_| PlaybackState::Playing,
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.begin_scene();
        sim.run_to(67_000, &back);
        let restored = sim.st.last.as_ref().unwrap().position_ms;
        assert_eq!(
            restored, 0,
            "在播时切走的记忆不可续接 → 必须归零（宁可落后，绝不超前）"
        );
    }

    // ------------------------------------------- G8-④：N 轮切走切回不丢钟

    /// **N 轮「播放 → 暂停 →（会话被别的播放器接走）→ 切回继续」不许丢钟。**
    ///
    /// 这是 Bug 1 改口径后的判定性测试：真机上每轮丢掉的不是「几十毫秒的检测
    /// 延迟」，而是**整首歌的进度**（日志实证 79,550ms / 141,223ms 两次）。
    /// 断言用「每轮平均损失 ≤ 一个轮询间隔」表达「不丢钟」——旧实现每轮丢掉整首
    /// 歌，平均损失 ~7000ms/轮，必红。
    fn run_cutaway_rounds(rounds: usize) -> Ms {
        let base = Instant::now() - Duration::from_millis(3_000_000);
        let mut sim = ClockSim::new(base);
        let play = 7_000u64;
        let away = 1_000u64;
        let soda_timeline = |t: u64| -> (Ms, Ms) { (t as Ms % 90_000 + 1, 200_000) };
        let soda_props = |_: u64| -> (&'static str, &'static str, Ms) { (SODA, "月半", 200_000) };
        let soda = Scene {
            obs_state: &|_| PlaybackState::Playing,
            props: &soda_props,
            timeline: &soda_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        let mut t = 0u64;
        for _ in 0..rounds {
            // 网易云播 play ms → 用户按暂停（检测到之后才算数）
            let pause_at = t + play;
            let obs = move |x: u64| {
                if x < pause_at {
                    PlaybackState::Playing
                } else {
                    PlaybackState::Paused
                }
            };
            let cloud = Scene {
                obs_state: &obs,
                props: &cloud_props,
                timeline: &cloud_timeline,
                events: vec![0],
                can_next: &|_| true,
            };
            sim.run_to(pause_at + 500, &cloud);
            // 会话被汽水接走 1s
            sim.drop_session(sim.now(pause_at + 500));
            sim.begin_scene();
            sim.run_to(pause_at + 500 + away, &soda);
            t = pause_at + 500 + away;
            // 切回网易云（用户点了播放）—— 模拟 CurrentSessionChanged 脏位
            sim.st.dirty.store(true, Ordering::Relaxed);
            sim.begin_scene();
        }
        // 最后一段：切回网易云再播 play ms
        let tail = Scene {
            obs_state: &|_| PlaybackState::Playing,
            props: &cloud_props,
            timeline: &cloud_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.run_to(t + play, &tail);
        sim.clock(t + play) - ((rounds + 1) as Ms * play as Ms)
    }

    #[test]
    fn repeated_cutaways_do_not_lose_the_lyric_clock() {
        for rounds in [10usize, 20] {
            let err = run_cutaway_rounds(rounds);
            let budget = rounds as Ms * limits::MEDIA_POLL_MS as Ms;
            assert!(
                err.abs() <= budget,
                "N={rounds}：累计误差 {err}ms 超过「每轮一个轮询间隔」的预算 {budget}ms \
                 （旧实现每轮丢掉整首歌 ⇒ 这里会是 -{}ms 量级）",
                rounds as Ms * 7_000
            );
        }
        // N=20 与 N=10 的误差都必须落在各自的预算内 ⇒ 不随 N 线性增长到整首歌
        let e10 = run_cutaway_rounds(10);
        let e20 = run_cutaway_rounds(20);
        assert!(
            e20.abs() <= e10.abs() + 10 * limits::MEDIA_POLL_MS as Ms,
            "误差随轮数暴涨（N=10 {e10} → N=20 {e20}）：钟又被丢掉了"
        );
    }

    /// 记忆的时效与容量：过期的记忆不许被使用（宁可从头），容量满则淘汰最老。
    #[test]
    fn clock_memory_respects_ttl_and_capacity() {
        let t0 = Instant::now();
        let mut mem = LyricClockMemory::default();
        let mut np = snapshot("晴天", PlaybackState::Paused, 42_000);
        np.session.aumid = CLOUD.to_string();
        mem.remember(&np, t0);
        assert_eq!(
            mem.restore(CLOUD, np.track_id, at(t0, 1_000)),
            Some(RestoredClock {
                position_ms: 42_000,
                leave_state: PlaybackState::Paused
            })
        );
        // TTL 边界：不过期 / 刚过期
        let ttl = limits::LYRIC_CLOCK_MEMORY_TTL_MS;
        assert!(mem
            .restore(CLOUD, np.track_id, at(t0, ttl.saturating_sub(1)))
            .is_some());
        assert!(
            mem.restore(CLOUD, np.track_id, at(t0, ttl + 1)).is_none(),
            "过期记忆必须被拒（不许把很久以前的位置硬塞回来）"
        );
        // 位置 0 的记忆没有意义（等同 Reset）
        let mut zero = snapshot("雨天", PlaybackState::Paused, 0);
        zero.session.aumid = CLOUD.to_string();
        mem.remember(&zero, t0);
        assert!(mem.restore(CLOUD, zero.track_id, t0).is_none());

        // 容量上限：超过 CAP 之后淘汰最老的一条
        for i in 0..(limits::LYRIC_CLOCK_MEMORY_CAP + 4) as u64 {
            let mut s = snapshot(&format!("曲目{i}"), PlaybackState::Paused, 1_000 + i as Ms);
            s.session.aumid = CLOUD.to_string();
            mem.remember(&s, at(t0, i));
        }
        assert_eq!(mem.len(), limits::LYRIC_CLOCK_MEMORY_CAP);
        // 最早那条（"晴天"）已被淘汰
        assert!(mem.restore(CLOUD, np.track_id, at(t0, 10)).is_none());
    }

    /// 记忆只记「离开时」的那一份：同一首歌的稳态发布不许刷新它的时间戳，
    /// 否则 TTL 会被无限续期。
    #[test]
    fn stable_publishes_do_not_refresh_memory_timestamp() {
        let t0 = Instant::now();
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        st.parts.aumid = CLOUD.to_string();
        let s0 = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.last = Some(s0);
        // 同一首歌、同一 aumid → 不算「离开」
        st.remember_leaving(Some(&published(&st.parts, PlaybackState::Playing, 0, t0)), at(t0, 5_000));
        assert_eq!(st.memory.len(), 0, "稳态发布不许写记忆");
        // 换歌 → 才算离开
        st.parts.track.title = "七里香".to_string();
        let next = published(&st.parts, PlaybackState::Playing, 0, t0);
        st.remember_leaving(Some(&next), at(t0, 5_000));
        assert_eq!(st.memory.len(), 1, "离开这首歌必须写记忆");
    }

    // ------------------------------------------- 汽水这类真实上报位置的播放器

    /// **守护测试：真实上报位置的播放器行为一个字节都不许变。**
    ///
    /// 汽水的读数一直是 `(position, duration)` 且 `position != 0` →
    /// `no_usable_position == false` → 永远走 `Keep`：快照里的位置就是本轮读到
    /// 的位置、锚点就是本轮读取时刻。记忆与 Carry 都不许插一脚。
    #[test]
    fn real_position_player_snapshot_is_untouched() {
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);
        let soda_timeline = |t: u64| -> (Ms, Ms) {
            let pos = 30_000 + t as Ms;
            (pos, 300_000)
        };
        let soda_props = |_: u64| -> (&'static str, &'static str, Ms) { (SODA, "月半", 300_000) };
        let marks = vec![(0u64, PlaybackState::Playing), (12_000, PlaybackState::Paused)];
        let obs = observed_timeline(&marks, &[60, 60]);
        let scene = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &soda_props,
            timeline: &soda_timeline,
            events: vec![0, 12_060],
            can_next: &|_| true,
        };
        // 每一拍都检查：快照位置必须等于「本轮读到的位置」，不许被外推/记忆改写
        let mut checked = 0usize;
        while sim.cursor + 50 <= 20_000 {
            sim.run_to(sim.cursor + 50, &scene);
            let t = sim.cursor;
            let last = sim.st.last.as_ref().expect("汽水必须一直有快照");
            let reported = (soda_timeline)(t).0;
            if last.state == PlaybackState::Playing {
                // 播放中：UI 外推出的钟必须等于真实读数（采样值 + 线性外推 = 真值）
                let clock = lyric_clock_at(last, sim.now(t));
                assert!(
                    (clock - reported).abs() <= 100,
                    "t={t} 汽水的外推钟必须等于真实读数：钟 {clock}，读数 {reported}"
                );
            }
            assert_eq!(last.session.aumid, SODA);
            checked += 1;
        }
        assert!(checked > 100, "守护测试必须真的跑过足够多拍");
        // 暂停轮次：位置就是暂停那一刻读到的真实值（不许被 Carry 外推改写）
        sim.run_to(20_500, &scene);
        let paused = sim.st.last.as_ref().unwrap();
        assert_eq!(paused.state, PlaybackState::Paused);
        let reported_at_pause = (soda_timeline)(12_060).0;
        assert!(
            (paused.position_ms - reported_at_pause).abs() <= 1_200,
            "汽水暂停位置必须是真实读数（{}），实际 {}",
            reported_at_pause,
            paused.position_ms
        );
    }

    /// 汽水切歌再切回（真实读数）：必须用**真实位置**，绝不许用记忆里的旧值。
    #[test]
    fn real_position_player_ignores_clock_memory_on_track_change() {
        let base = Instant::now() - Duration::from_millis(1_000_000);
        let mut sim = ClockSim::new(base);
        let soda_timeline = |t: u64| -> (Ms, Ms) { (t as Ms + 1_000, 300_000) };
        let marks = vec![(0u64, PlaybackState::Playing)];
        let obs = observed_timeline(&marks, &[]);
        let a = |_: u64| -> (&'static str, &'static str, Ms) { (SODA, "月半", 300_000) };
        let p1 = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &a,
            timeline: &soda_timeline,
            events: vec![0],
            can_next: &|_| true,
        };
        sim.run_to(30_000, &p1);
        // B 歌：真实位置 5s
        let b = |_: u64| -> (&'static str, &'static str, Ms) { (SODA, "隔岸", 300_000) };
        let b_timeline = |_: u64| -> (Ms, Ms) { (5_000, 300_000) };
        let p2 = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &b,
            timeline: &b_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.run_to(31_000, &p2);
        assert_eq!(
            sim.st.last.as_ref().unwrap().position_ms,
            5_000,
            "真实读数优先：不许用记忆里的旧值覆盖它"
        );
        assert_eq!(
            sim.st.last.as_ref().unwrap().state,
            PlaybackState::Playing,
            "切歌后仍在播放"
        );
        // 切回 A：真实读数（此刻 33s）仍然优先，不许用记忆里的 30s
        let p3 = Scene {
            obs_state: &|x| state_of(&obs, x),
            props: &a,
            timeline: &soda_timeline,
            events: vec![],
            can_next: &|_| true,
        };
        sim.run_to(33_000, &p3);
        assert_eq!(
            lyric_clock_at(sim.st.last.as_ref().unwrap(), sim.now(33_000)),
            34_000,
            "切回 A 必须用真实读数（此刻 34s），不许用记忆里的 30s"
        );
    }

    // ================================================ QA 补测（v1.9.3 / G8-④）
    //
    // 下面 4 条是 QA 独立审计后补的**盲区覆盖**：把对应实现改坏时，原有 612 条
    // 测试**全部仍然通过**（已实测），而用户会看到「暂停几次后歌词越来越靠后」
    // 或「被拉黑/解绑后同一首歌从第一句重唱」。每条都已在改坏实验里验证过会变红。

    /// **QA 补测①：主循环停顿（系统休眠 / UI 卡死）后恢复时，Carry 必须锚到
    /// 事件到达时刻，而不是「醒来那一刻」。**
    ///
    /// 真机形态：合盖 25s 再打开（或主循环被卡住），期间 `PlaybackInfoChanged`
    /// 已经到达并被 [`PlaybackEventClock`] 记下真实时刻。这时
    /// - 锚到事件时刻 → 钟落在暂停真正发生的位置（42s + 5.06s）；
    /// - 锚到本轮 `now`（`jumped_at = None`）→ 钟被多拨 25s，**永久超前**，
    ///   歌词直接跳到还没播的句子，而 `(0,0)` 播放器没有任何自愈机会。
    ///
    /// 现有 N 轮仿真都是「50ms 连续跑」的形态，碰不到这一格；下面的接线三行与
    /// `tick()` 逐字同款（`last_playback_obs` → `note_playback` → `take_after`）。
    #[test]
    fn carry_anchors_at_event_stamp_after_a_stalled_loop() {
        let t0 = Instant::now() - Duration::from_millis(500_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        st.parts.position_ms = 42_000;
        st.parts.timeline_at = Some(t0);
        st.last = Some(published(&st.parts, PlaybackState::Playing, 42_000, t0));
        // 上一次快层观测就在 t0，此后主循环停了 25s
        st.cadence.note_playback(t0, PlaybackState::Playing);
        // 真实暂停在 t0+5s；PlaybackInfoChanged 在 t0+5.06s 到达
        st.playback_events.stamp_at(at(t0, 5_060));

        // ---- 以下 3 行与 `tick()` 的接线逐字同款 ----
        let now = at(t0, 30_000);
        let prev_obs = st.cadence.last_playback_obs();
        let jumped = st.cadence.note_playback(now, PlaybackState::Paused);
        let jumped_at = Some(transition_at(now, st.playback_events.take_after(prev_obs)));

        st.parts.state = PlaybackState::Paused;
        st.apply_timeline_read(0, 0, now);
        let inputs = tick_inputs(jumped, Some((0, 0))).at(jumped_at);
        let snap = st.publish_point(inputs, now).expect("状态跃迁必须发布");

        assert!(jumped, "前置：必须判出状态跃迁");
        assert_eq!(
            snap.position_ms, 47_060,
            "停摆 25s 后恢复，钟只许走「真实播放过的 5.06s」，不许把整段停摆算进去"
        );
        assert_eq!(
            snap.position_at,
            at(t0, 5_060),
            "Carry 的锚点必须是事件到达时刻，不是醒来那一刻（否则永久超前 25s）"
        );
        assert_eq!(lyric_clock_at(&snap, now), 47_060, "暂停态钟原地停住");
    }

    /// **QA 补测②：事件时间戳恰好等于「上一次快层观测」时必须收下。**
    ///
    /// 边界方向写反（`t < a` → `t <= a`）时原有 612 条测试全绿（已实测 M5），
    /// 后果是把「恰好落在上一次观测那一刻的这次跃迁」当成更早一次的残留丢掉，
    /// Carry 退回 `now`，那一轮白白少算一个轮询间隔。
    #[test]
    fn event_stamp_exactly_at_the_last_observation_is_accepted() {
        let clock = PlaybackEventClock::default();
        let t0 = Instant::now();
        clock.stamp_at(at(t0, 600));
        assert_eq!(
            clock.take_after(Some(at(t0, 600))),
            Some(at(t0, 600)),
            "「恰好相等」必须收下：那是本轮要检测的这次跃迁，不是更早一次的残留"
        );
        assert!(
            clock.take_after(None).is_none(),
            "时间戳只许被消费一次"
        );
    }

    fn fake_sink() -> (EventSink<AppEvent>, std::sync::mpsc::Receiver<AppEvent>) {
        use crate::platform::wake::WakeHwnd;
        use windows::Win32::Foundation::HWND;
        let hwnd = HWND(0x1234_usize as *mut core::ffi::c_void);
        EventSink::new(WakeHwnd(hwnd))
    }

    /// 造一份「网易云暂停在 141.223s」的已广播状态（`last` 已就位）。
    fn paused_cloud_state(t0: Instant) -> (PollState, NowPlaying) {
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        zero_player(&mut st.parts, "晴天", t0);
        st.parts.position_ms = 141_223;
        st.parts.timeline_at = Some(t0);
        st.parts.state = PlaybackState::Paused;
        let snap = published(&st.parts, PlaybackState::Paused, 141_223, t0);
        st.last = Some(snap.clone());
        (st, snap)
    }

    /// **QA 补测③：被拉黑 / 解绑释放这两条路径也必须记下歌词钟。**
    ///
    /// 真机场景：网易云暂停在 141s → 用户把它加进黑名单（或从黑名单移除）→
    /// 小组件隐藏/恢复 → 同一首歌再出现。这两处漏挂 `remember_leaving` 时原有
    /// 612 条测试全绿（已实测 M6a / M6b），而用户会看到歌词从第一句重唱。
    #[test]
    fn blacklist_paths_remember_the_lyric_clock() {
        let t0 = Instant::now() - Duration::from_millis(200_000);
        let expect = Some(RestoredClock {
            position_ms: 141_223,
            leave_state: PlaybackState::Paused,
        });

        // ① 进入忽略态（命中黑名单）：`tick` 的两个拦截点与 `rescan_sessions`
        //    的 Hide 分支都走这里
        let (mut st, snap) = paused_cloud_state(t0);
        let (sink, _rx) = fake_sink();
        st.enter_ignored(&sink, at(t0, 1_000));
        assert!(st.last.is_none(), "前置：进入忽略态必须丢弃快照");
        assert_eq!(st.memory.len(), 1, "进入忽略态必须先记住歌词钟");
        assert_eq!(
            st.memory.restore(CLOUD, snap.track_id, at(t0, 2_000)),
            expect,
            "记下来的必须是「141.223s + 暂停态」，否则续接会拿错值"
        );

        // ② 解绑释放（黑名单条件不再成立 → 回到常规路径）
        let (mut st2, snap2) = paused_cloud_state(t0);
        let (sink2, _rx2) = fake_sink();
        st2.apply_release(ScanDecision::Release, &sink2, at(t0, 1_000));
        assert!(st2.last.is_none(), "前置：释放必须丢弃快照");
        assert_eq!(st2.memory.len(), 1, "解绑释放必须先记住歌词钟");
        assert_eq!(st2.memory.restore(CLOUD, snap2.track_id, at(t0, 2_000)), expect);
    }

    /// **QA 补测⑤：真实播放器「换曲首帧就是 `(0, D)`」不许被上一首的旧位置污染。**
    ///
    /// G8-⑤ 的核心风险：`apply_timeline_read` 现在只在 `position != 0` 时写
    /// `parts.position_ms`，所以 `(0,D)` 读完后 `parts.position_ms` **还是上一首的
    /// 值**。这条用例证明它不会外泄：`track_id` 变了 → `lyric_clock_action` 判
    /// `Reset`（因为 G8-⑤ 的判据是「位置是否为 0」）→ `Reset` 无条件把位置写成
    /// 0 / 记忆值。**判据一旦退回 `Some(_) => false`，本用例立刻变红（钟残留 200s）。**
    #[test]
    fn zero_position_first_read_of_a_new_track_is_not_polluted_by_the_previous_track() {
        let t0 = Instant::now() - Duration::from_millis(300_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        // 汽水 A 歌播到 200s 并已广播
        st.parts.track = Track {
            title: "月半".to_string(),
            artist: "黄龄".to_string(),
            album: String::new(),
            duration_ms: 0,
        };
        st.parts.aumid = SODA.to_string();
        st.parts.process_name = SODA.to_string();
        st.parts.app_name = SODA.to_string();
        st.parts.props_ok = true;
        st.parts.state = PlaybackState::Playing;
        st.parts.position_ms = 200_000;
        st.parts.duration_ms = 200_000;
        st.parts.timeline_at = Some(t0);
        st.last = Some(published(&st.parts, PlaybackState::Playing, 200_000, t0));

        // 换到 B 歌：真实播放器的首帧读数就是 `(0, 223_000)`
        st.parts.track = Track {
            title: "隔岸".to_string(),
            artist: "x".to_string(),
            album: String::new(),
            duration_ms: 0,
        };
        let t1 = at(t0, 1_000);
        st.apply_timeline_read(0, 223_000, t1);
        let snap = st
            .publish_point(ti(false, Some((0, 223_000)), true), t1)
            .expect("换曲必须发布");
        assert_eq!(
            snap.position_ms, 0,
            "换曲首帧 (0,D)：钟必须是 0，绝不许残留上一首的 200s"
        );
        assert_eq!(snap.position_at, t1, "锚点必须是这次采样的时刻");
        assert_eq!(
            st.parts.position_ms, 0,
            "parts 也必须归零，否则下一轮 (0,D) 会继续沿用 200s"
        );
        assert_eq!(st.parts.timeline_at, Some(t1), "位置与锚点必须配对");
    }

    /// **QA 审判项（v1.9.3 已知回归，**故意留红时不得出包**）：真实播放器在
    /// 暂停态把进度拖到最开头 / 单曲循环重播到 0 时，钟必须跟着回 0。**
    ///
    /// 现行实现里 `apply_timeline_read` 只在 `position != 0` 时写位置，于是
    /// `(0, D)` 读完后 `parts.position_ms` 仍是旧值（120s），`Keep` 分支把这份
    /// 旧值原样拼进快照，`has_changed` 又判成「没变化」→ **连发布都不发生**，
    /// 进度/歌词卡在旧位置，直到播放器报出第一个非零位置（≤1 个时间线轮隔）。
    #[test]
    fn seeking_to_zero_on_a_real_position_player_pulls_the_clock_back_to_zero() {
        let t0 = Instant::now() - Duration::from_millis(300_000);
        let mut st = PollState::new(Arc::new(AtomicBool::new(false)), test_index());
        st.parts.track = Track {
            title: "月半".to_string(),
            artist: "黄龄".to_string(),
            album: String::new(),
            duration_ms: 0,
        };
        st.parts.aumid = SODA.to_string();
        st.parts.process_name = SODA.to_string();
        st.parts.app_name = SODA.to_string();
        st.parts.props_ok = true;
        st.parts.state = PlaybackState::Paused;
        st.parts.position_ms = 120_000;
        st.parts.duration_ms = 200_000;
        st.parts.timeline_at = Some(t0);
        st.last = Some(published(&st.parts, PlaybackState::Paused, 120_000, t0));

        // 用户在暂停态把进度拖到 0 → 真实读数 `(0, 200_000)`
        let t1 = at(t0, 1_000);
        st.apply_timeline_read(0, 200_000, t1);
        let published_now = st.publish_point(ti(false, Some((0, 200_000)), true), t1);
        let snap = published_now.expect("拖到 0 后必须发布（位置大幅回退，UI 要立刻跟上）");
        assert_eq!(
            snap.position_ms, 0,
            "暂停态拖到 0：钟必须回 0，不许停在 120s"
        );
        assert_eq!(snap.position_at, t1);
    }

    /// **QA 补测④：五个丢弃点的接线静态锁。**
    ///
    /// `switch_display_session` 需要真实 COM 会话句柄，单测构造不出来；它漏挂
    /// `remember_leaving` 时原有 612 条测试同样全绿（已实测 M6c），而「黑名单
    /// 命中 → 改显示其它会话 → 切回来」正是 G8-① 引入的高频路径。这里按方法体
    /// 逐个断言，把这条路径钉死。
    #[test]
    fn every_snapshot_discard_point_calls_remember_leaving() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/media/smtc.rs"
        ))
        .expect("必须能读到自己");
        for sig in [
            "fn enter_ignored(&mut self",
            "fn switch_display_session(",
            "fn apply_release(&mut self",
            "fn tick(&mut self",
        ] {
            let start = src.find(sig).unwrap_or_else(|| panic!("{sig} 必须存在"));
            let rest = &src[start..];
            let end = rest.find("\n    }\n").map(|i| i + 7).unwrap_or(rest.len());
            let body = &rest[..end];
            assert!(
                body.contains("remember_leaving"),
                "{sig} 丢弃快照前必须 remember_leaving（漏一处 = 这首歌的位置仍然被丢）"
            );
        }
    }
}
