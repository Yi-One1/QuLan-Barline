//! SMTC（`Windows.Media.Control`）媒体线程（`ARCHITECTURE.md` §6.4 / T05）。
//!
//! **三个必须照做的坑（§2.2）**：
//! 1. `IAsyncOperation` 在 windows 0.62.2 里没有 `get()`，只有 **`.join()`**；
//!    且 `.join()` 只能在 **MTA** 线程调用——放 STA 会死锁。
//! 2. 事件订阅方法**没有 `Add` 前缀**：`CurrentSessionChanged` / `SessionsChanged`。
//! 3. `PlaybackStatus` 常量是 **PascalCase**（`::Playing` / `::Paused` …），不是 `PLAYING`。
//!    且 `TimeSpan.Duration` 单位是 **100ns**，转毫秒要 `/ 10_000`。
//!
//! 轮询策略：管理器级事件只用来「置脏位 + 唤醒」，真正的读取统一走
//! `poll_once`（最快 60ms 一次、兜底 500ms 一次）。这样既满足 PRD P0-01 的
//! 「1 秒内」要求，也不会被播放器的高频事件拖垮。

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
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
    SessionsChangedEventArgs,
};

use crate::config::defaults::limits;
use crate::event::{AppEvent, MediaCommand, MediaCommandKind};
use crate::media::cover::load_cover;
use crate::media::player_map;
use crate::media::{
    CoverImage, MediaSessionInfo, MediaSource, Ms, NowPlaying, PlaybackState, Track, TrackId,
};
use crate::platform::com::{backoff_ms, timed_join, ComApartment};
use crate::platform::wake::EventSink;

/// 管理器获取失败时的最大重试次数。
const MANAGER_RETRIES: u32 = 5;

/// 封面解码记忆。
///
/// **为什么必须有**：`read_session` 每次轮询（最快 60ms、兜底 500ms）都会调用，
/// 而封面解码要 `OpenReadAsync().join()` + WIC 解码（约 3–10ms）。
/// 不缓存的话每秒会解 2 次同一张图，纯属浪费 CPU 与 GDI/COM 句柄。
/// 只有 `track_id` 变化时才真正解码一次。
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
        self.cover = load_cover(props);
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

/// 媒体线程主函数。**整个函数体都在 MTA 上运行**，因此可以随意 `join()`。
pub fn run(sink: EventSink<AppEvent>, cmd_rx: Receiver<MediaCommand>) {
    let _com = ComApartment::mta();
    crate::log_info!("媒体线程启动（MTA）");

    let Some(mgr) = acquire_manager() else {
        crate::log_err!("SMTC manager 连续 {MANAGER_RETRIES} 次获取失败，媒体功能降级（小组件仍可用）");
        drain_until_quit(&cmd_rx);
        crate::log_info!("媒体线程退出");
        return;
    };

    // 事件回调只做两件事：置脏位 + PostMessage 唤醒 UI。绝不在回调里做重活。
    let dirty = Arc::new(AtomicBool::new(true));
    let _handlers = subscribe(&mgr, &dirty, &sink);

    let mut last: Option<NowPlaying> = None;
    let mut cover_memo = CoverMemo::default();
    let mut last_poll = Instant::now();

    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(MediaCommand::Quit) => break,
            Ok(cmd) => execute(cmd, &mgr, &sink, &mut last, &mut cover_memo),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                crate::log_warn!("命令通道已断开，媒体线程退出");
                break;
            }
        }

        let forced = dirty.swap(false, Ordering::Relaxed);
        let elapsed = last_poll.elapsed();
        let due = elapsed >= Duration::from_millis(limits::MEDIA_POLL_MS);
        // 事件驱动的补采也要有下限，避免播放器高频事件导致采样风暴
        if due || (forced && elapsed >= Duration::from_millis(60)) {
            last_poll = Instant::now();
            poll_once(&mgr, &sink, &mut last, &mut cover_memo);
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

/// 读一次当前会话，做 diff 后决定是否发事件。
fn poll_once(
    mgr: &Manager,
    sink: &EventSink<AppEvent>,
    last: &mut Option<NowPlaying>,
    cover_memo: &mut CoverMemo,
) {
    let session = match mgr.GetCurrentSession() {
        Ok(s) => s,
        Err(_) => {
            if last.is_some() {
                crate::log_info!("session cleared");
                *last = None;
                cover_memo.done = false;
                sink.send(AppEvent::SessionCleared);
            }
            return;
        }
    };

    let snapshot = match read_session(&session, cover_memo) {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("读取会话失败：{e}");
            return;
        }
    };

    if let Some(prev) = last.as_ref() {
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

    *last = Some(snapshot.clone());
    sink.send(AppEvent::NowPlaying(Box::new(snapshot)));
}

/// 从会话读出一份完整快照。
fn read_session(
    session: &GlobalSystemMediaTransportControlsSession,
    cover_memo: &mut CoverMemo,
) -> Result<NowPlaying> {
    let aumid = session
        .SourceAppUserModelId()
        .map(|h| h.to_string())
        .unwrap_or_default();
    let resolved = player_map::resolve(&aumid);

    let playback = session.GetPlaybackInfo().ok();
    let state = match playback.as_ref().and_then(|p| p.PlaybackStatus().ok()) {
        Some(s) if s == PlaybackStatus::Playing => PlaybackState::Playing,
        // Paused / Opened / Changing / Stopped / Closed 一律视为「暂停」：
        // 小组件保持显示、进度与扫光静止（PRD P0-02）。
        Some(_) => PlaybackState::Paused,
        // 拿不到 PlaybackInfo 时按「播放中」处理，避免误隐藏
        None => PlaybackState::Playing,
    };

    let props = timed_join("TryGetMediaPropertiesAsync", || {
        session.TryGetMediaPropertiesAsync()?.join()
    })?;
    let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
    let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
    let album = props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default();

    let (position_ms, duration_ms) = read_timeline(session);

    let controls = playback.as_ref().and_then(|p| p.Controls().ok());
    let can_prev = controls
        .as_ref()
        .and_then(|c| c.IsPreviousEnabled().ok())
        .unwrap_or(false);
    let can_next = controls
        .as_ref()
        .and_then(|c| c.IsNextEnabled().ok())
        .unwrap_or(false);
    // SMTC 的 PlaybackControls 不暴露「能否 seek」，用「有总时长」作为代理判据（风险 R9）
    let can_seek = duration_ms > 0;
    // v1.1.16：can_shuffle/can_repeat/shuffle/repeat 采集已随菜单项删除

    let track = Track {
        title,
        artist,
        album,
        duration_ms,
    };
    let track_id = if track.is_empty() {
        0
    } else {
        fnv1a64(track.key().as_bytes())
    };

    Ok(NowPlaying {
        track_id,
        track: Some(track),
        session: MediaSessionInfo {
            aumid,
            app_name: resolved.display_name.to_string(),
            process_id: 0,
            process_name: resolved.process_name.to_string(),
            source: MediaSource::Smtc,
        },
        state,
        position_ms,
        position_at: Instant::now(),
        can_prev,
        can_next,
        can_seek,
        // 封面按 track_id 记忆：换歌才解码一次（见 CoverMemo）
        cover: cover_memo.get(track_id, &props),
    })
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
/// **刻意不把 `position_ms` 直接纳入 diff**：播放中位置每 500ms 必然前进，
/// 那样会变成 2Hz 的无意义重绘（T05 验收明确要求「空闲 5 分钟日志不再刷屏」）。
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
fn execute(
    cmd: MediaCommand,
    mgr: &Manager,
    sink: &EventSink<AppEvent>,
    last: &mut Option<NowPlaying>,
    cover_memo: &mut CoverMemo,
) {
    match cmd {
        MediaCommand::Quit => return,
        MediaCommand::Refresh => {
            // 强制重读一次并广播（乐观更新超时后的对账）
            poll_once(mgr, sink, last, cover_memo);
            return;
        }
        MediaCommand::Mute => {
            crate::log_debug!("Mute 由 WASAPI 通道实现（T16），当前忽略");
            return;
        }
        _ => {}
    }

    let kind = match cmd {
        MediaCommand::PlayPause => MediaCommandKind::PlayPause,
        MediaCommand::Next => MediaCommandKind::Next,
        MediaCommand::Prev => MediaCommandKind::Prev,
        MediaCommand::Seek(_) => MediaCommandKind::Seek,
        MediaCommand::Refresh | MediaCommand::Mute | MediaCommand::Quit => return,
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
}
