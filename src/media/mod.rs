//! 媒体层：数据模型 + 媒体线程入口。
//!
//! 数据模型（`Track` / `NowPlaying` / `MediaSessionInfo` / `CoverImage` / `Rgb`）是
//! **跨模块契约**，逐字段照抄 `ARCHITECTURE.md` §7.1，不得改名改类型。

pub mod cover;
pub mod launch;
pub mod player_map;
pub mod smtc;
pub mod wasapi;

use std::sync::mpsc::{SendError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use crate::event::{AppEvent, MediaCommand};
use crate::platform::wake::EventSink;

/// 毫秒。全项目时间统一用 i64 毫秒，禁止混用秒 / f32。
pub type Ms = i64;

/// 曲目代次。单调递增，用于丢弃过期的歌词 / 封面异步结果。
pub type TrackId = u64;

/// 播放状态。`NoSession` 时小组件应完全隐藏（M2 起生效，M1 恒显）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackState {
    #[default]
    NoSession,
    Paused,
    Playing,
    Fallback,
}

impl PlaybackState {
    /// 日志友好名。
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackState::NoSession => "NoSession",
            PlaybackState::Paused => "Paused",
            PlaybackState::Playing => "Playing",
            PlaybackState::Fallback => "Fallback",
        }
    }
}

/// 元数据来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaSource {
    #[default]
    None,
    Smtc,
    Wasapi,
}

/// 循环模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    None,
    Track,
    List,
}

/// 曲目元数据。同时作为歌词缓存 key。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 总时长；0 表示播放器未上报
    pub duration_ms: Ms,
}

impl Track {
    /// 缓存键：时长按 3 秒粒度取整，避免不同播放器上报 ±1s 造成缓存击穿。
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.title.trim().to_lowercase(),
            self.artist.trim().to_lowercase(),
            self.album.trim().to_lowercase(),
            self.duration_ms / 3000
        )
    }

    pub fn is_empty(&self) -> bool {
        self.title.is_empty()
    }
}

/// 会话来源信息（用于「来自网易云音乐」与点击封面激活）。
#[derive(Debug, Clone, Default)]
pub struct MediaSessionInfo {
    /// SMTC 的 SourceAppUserModelId，例如 "CloudMusic.exe"
    pub aumid: String,
    /// 由 player_map 解析出的显示名，例如 "网易云音乐"
    pub app_name: String,
    /// 进程 id（WASAPI 兜底时必填；SMTC 时由 aumid 反查，可能为 0）
    pub process_id: u32,
    /// 进程名，例如 "cloudmusic.exe"（小写）
    pub process_name: String,
    pub source: MediaSource,
}

/// 封面位图（预乘 alpha 的 BGRA，行紧密排列）。
#[derive(Debug, Clone)]
pub struct CoverImage {
    pub width: u32,
    pub height: u32,
    /// len == width * height * 4，预乘 alpha，BGRA 顺序
    pub bgra: Vec<u8>,
    /// 从封面提取的主色（用于进度条与悬停背景）
    pub dominant: Rgb,
}

/// 24 位颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// 系统强调色兜底值（Win11 默认蓝）。
    pub const SYSTEM_ACCENT_FALLBACK: Rgb = Rgb::new(0x00, 0x78, 0xD4);

    /// 解析 `#RRGGBB` / `RRGGBB`。失败返回 `None`。
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Rgb::new(r, g, b))
    }
}

/// 小组件渲染所需的完整「当前播放」快照。UI 线程只读它，不再访问 COM。
#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub track_id: TrackId,
    /// Fallback 态为 None
    pub track: Option<Track>,
    pub session: MediaSessionInfo,
    pub state: PlaybackState,
    /// 采样时刻的播放位置
    pub position_ms: Ms,
    /// 采样时刻（单调时钟），用于 UI 线程线性外推
    pub position_at: Instant,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_seek: bool,
    pub can_shuffle: bool,
    pub can_repeat: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub cover: Option<Arc<CoverImage>>,
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self {
            track_id: 0,
            track: None,
            session: MediaSessionInfo::default(),
            state: PlaybackState::NoSession,
            position_ms: 0,
            position_at: Instant::now(),
            can_prev: false,
            can_next: false,
            can_seek: false,
            can_shuffle: false,
            can_repeat: false,
            shuffle: false,
            repeat: RepeatMode::None,
            cover: None,
        }
    }
}

impl NowPlaying {
    /// UI 线程使用：按单调时钟外推到「此刻」的播放位置。
    /// 暂停 / 兜底态直接返回采样值，保证扫光停住（PRD P0-09）。
    pub fn position_now(&self) -> Ms {
        if self.state != PlaybackState::Playing {
            return self.position_ms;
        }
        let dur = self.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        let elapsed = self.position_at.elapsed().as_millis() as Ms;
        let p = self.position_ms + elapsed;
        if dur > 0 {
            p.min(dur)
        } else {
            p
        }
    }

    /// 标题（无会话时给占位文案）。渲染热路径调用，避免每帧 `format!`。
    pub fn title_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        match self.track.as_ref() {
            Some(t) if !t.title.is_empty() => t.title.as_str(),
            _ => fallback,
        }
    }

    /// 歌手。
    pub fn artist_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        match self.track.as_ref() {
            Some(t) if !t.artist.is_empty() => t.artist.as_str(),
            _ => fallback,
        }
    }
}

/// WASAPI 兜底会话信息（`ARCHITECTURE.md` §8.1）。
///
/// 放在 media 层而不是 event 层，是因为它是「采集层的事实」，
/// `AppEvent::FallbackSession(Box<FallbackInfo>)` 只是把它搬过线程边界。
#[derive(Debug, Clone)]
pub struct FallbackInfo {
    pub process_id: u32,
    pub process_name: String,
    pub display_name: String,
    /// 该会话的 peak 值，用于判断是否真的在出声
    pub peak: f32,
}

/// 媒体线程门面：持有命令发送端与 join 句柄。
pub struct MediaService {
    tx: Sender<MediaCommand>,
    handle: Option<JoinHandle<()>>,
}

impl MediaService {
    /// 启动媒体线程（MTA）。失败（线程创建失败）返回 `None`，不影响小组件运行。
    pub fn spawn(sink: EventSink<AppEvent>) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<MediaCommand>();
        let spawned = std::thread::Builder::new()
            .name(String::from("barline-media"))
            .stack_size(512 * 1024)
            .spawn(move || smtc::run(sink, rx));

        match spawned {
            Ok(handle) => Some(Self {
                tx,
                handle: Some(handle),
            }),
            Err(e) => {
                crate::log_err!("媒体线程创建失败：{e}");
                None
            }
        }
    }

    /// 投递一条播控命令。UI 线程已退出 / 线程已结束时静默丢弃。
    pub fn send(&self, cmd: MediaCommand) {
        match self.tx.send(cmd) {
            Ok(()) => {}
            Err(SendError(_)) => {
                crate::log_warn!("媒体线程已退出，命令被丢弃");
            }
        }
    }

    /// 通知媒体线程退出并等待它结束（最多 2s，避免拖住关机）。
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(MediaCommand::Quit);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MediaService {
    fn drop(&mut self) {
        self.shutdown();
    }
}
