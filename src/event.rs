//! 事件枚举 —— 模块之间**唯一**的耦合点（`ARCHITECTURE.md` §8）。
//!
//! 分层单向数据流：
//! ```text
//! 采集层(T1/T2/T3) --AppEvent--> 状态层(T0: App::reduce) --Effect--> 表现层(T0)
//! ```
//! `reduce()` 是纯函数，只产出 `Vec<Effect>`；副作用由 `App::apply()` 执行。
//!
//! M1 阶段只有 `NowPlaying` / `SessionCleared` / `Tick` / `Quit` / `ConfigChanged` /
//! `TaskbarGeometry` / `SecondInstanceLaunched` 会被真正处理，其余变体是
//! M2–M5 的接口预留——`reduce()` 里统一 `log_debug!` 后忽略。

#![allow(dead_code)]

use std::sync::Arc;

use crate::config::Config;
use crate::lyric::{LyricMissReason, Lyrics};
use crate::media::{FallbackInfo, Ms, NowPlaying, Track, TrackId};
use crate::platform::wake::EventSink;
use crate::render::theme::{BackgroundMode, Theme};
use crate::taskbar::geometry::TaskbarGeometry;

/// 工作线程 → UI 线程的事件。
#[derive(Debug, Clone)]
pub enum AppEvent {
    // ---------------- 媒体（T1 → T0） ----------------
    /// 当前播放快照变化（已做 diff，字段无变化不会发）
    NowPlaying(Box<NowPlaying>),
    /// 会话全部消失
    SessionCleared,
    /// WASAPI 兜底会话出现
    FallbackSession(Box<FallbackInfo>),
    /// 播控命令执行结果（用于回滚乐观更新）
    CommandResult { kind: MediaCommandKind, ok: bool },

    // ---------------- 歌词（T2 → T0） ----------------
    LyricLoading {
        track_id: TrackId,
    },
    LyricReady {
        track_id: TrackId,
        lyrics: Arc<Lyrics>,
    },
    LyricMissing {
        track_id: TrackId,
        reason: LyricMissReason,
    },
    /// 重试计时到点，需要重新请求
    LyricRetryDue {
        track_id: TrackId,
    },

    // ---------------- 背景（T3 → T0） ----------------
    BackdropReady {
        epoch: u64,
        theme: Theme,
    },

    // ---------------- 任务栏（watcher → T0） ----------------
    TaskbarGeometry(TaskbarGeometry),
    /// 收到 TaskbarCreated 广播（Explorer 重启），需要重新挂载
    TaskbarCreated,
    /// 显示配置变化（分辨率 / DPI / 显示器增删）
    DisplayChanged,

    // ---------------- 用户输入 ----------------
    Hotkey(HotkeyAction),
    TrayCommand(TrayCommand),
    /// 小组件窗口内产生的交互（由 widget_wnd 内部状态机产出）
    WidgetInput(WidgetInput),

    // ---------------- 配置 / 设置页 ----------------
    ConfigChanged(Box<Config>),
    /// 设置页请求打开 / 关闭
    SettingsOpenRequest,
    SettingsClosed,
    /// WebView2 就绪（controller 已拿到）
    SettingsWebViewReady,
    /// WebView2 创建失败（未安装运行时等）
    SettingsWebViewFailed(String),
    /// 设置页 JS 发来的一条 JSON
    SettingsMessage(String),

    // ---------------- 生命周期 ----------------
    /// 第二个实例启动，要求唤起设置页
    SecondInstanceLaunched,
    /// 低频心跳（500ms），驱动进度外推与歌词推进的脏标记
    Tick,
    Quit,
}

/// 全局快捷键动作（6 组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    PlayPause,
    Prev,
    Next,
    ToggleLyrics,
    Mute,
    ToggleWidget,
}

impl HotkeyAction {
    /// 动作名（**稳定字符串**）：设置页的 `<action>`、配置路径、日志都用它。
    ///
    /// v1.2.3（G4）：从 `settings::bridge::action_name` 上提到这里，
    /// 让「设置页写入 / 逐行恢复默认 / 日志」共用同一张名字表。
    pub fn as_str(self) -> &'static str {
        match self {
            HotkeyAction::PlayPause => "play_pause",
            HotkeyAction::Prev => "prev",
            HotkeyAction::Next => "next",
            HotkeyAction::ToggleLyrics => "toggle_lyrics",
            HotkeyAction::Mute => "mute",
            HotkeyAction::ToggleWidget => "toggle_widget",
        }
    }

    /// 动作名 → 动作。不认识返回 `None`（**不 panic、不静默成功**）。
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "play_pause" => HotkeyAction::PlayPause,
            "prev" => HotkeyAction::Prev,
            "next" => HotkeyAction::Next,
            "toggle_lyrics" => HotkeyAction::ToggleLyrics,
            "mute" => HotkeyAction::Mute,
            "toggle_widget" => HotkeyAction::ToggleWidget,
            _ => return None,
        })
    }

    /// 全部 6 个动作（测试 / 遍历用）。
    pub const ALL: [HotkeyAction; 6] = [
        HotkeyAction::PlayPause,
        HotkeyAction::Prev,
        HotkeyAction::Next,
        HotkeyAction::ToggleLyrics,
        HotkeyAction::Mute,
        HotkeyAction::ToggleWidget,
    ];
}

/// 托盘菜单命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    OpenSettings,
    ToggleWidget,
    Quit,
}

/// 小组件窗口内部状态机产出、需要进入 `reduce` 的语义化输入。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WidgetInput {
    HoverEnter,
    HoverLeave,
    ClickPlayPause,
    ClickPrev,
    ClickNext,
    ClickCover,
    /// 右上角关闭 ×（v1.5.0 / G3）：隐藏到托盘，**仅本次运行**生效（不写配置）
    ClickDismiss,
    SeekBegin,
    /// 0.0..=1.0
    SeekPreview(f32),
    SeekCommit(f32),
    SeekCancel,
    Wheel { delta: i32 },
    VolumeHudExpired,
    ToggleLyricsFromMenu,
    SetBackgroundMode(BackgroundMode),
    ShowAbout,
}

/// 播控命令种类（用于 `CommandResult` 回滚）。v1.1.16 起没有 Shuffle/Repeat。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommandKind {
    PlayPause,
    Next,
    Prev,
    Seek,
}

/// UI 线程需要产生的副作用。`reduce()` 保持纯函数，`App::apply()` 执行这些。
#[derive(Debug, Clone)]
pub enum Effect {
    /// 重新计算并应用布局 + 重绘
    Relayout,
    /// 只重绘（布局不变）
    Redraw,
    /// 解冻（v1.5.5 / G3.5）：收到解冻后第一份 `NowPlaying` 时，
    /// 由 `apply` 先把隐藏窗口刷成最新内容，再恢复显示（不闪旧帧）。
    ResumeFromFreeze,
    /// 应用窗口可见性
    SetVisible(bool),
    /// 把窗口移到新位置 / 新尺寸
    MoveWindow { x: i32, y: i32, w: i32, h: i32 },
    /// 重建 D2D 目标（尺寸或 DPI 变化）
    ResizeTarget { w: i32, h: i32, dpi: u32 },
    /// 请求背景采样
    RequestBackdrop { epoch: u64 },
    /// 向媒体线程发命令
    Media(MediaCommand),
    /// 向歌词线程发请求
    Lyric(LyricRequest),
    /// 保存配置（带 300ms 去抖）
    SaveConfig(Box<Config>),
    /// 向设置页推送
    SettingsPush(HostMsg),
    /// 打开 / 关闭设置页
    OpenSettings,
    CloseSettings,
    /// 重新注册热键
    RebindHotkeys,
    /// 更新托盘菜单语言
    RefreshTrayMenu,
    /// 退出进程
    Quit,
}

/// 向媒体线程下发的播控命令。
///
/// v1.1.16 起**没有** `Shuffle` / `Repeat`——SMTC 远程变更只是「请求」，
/// 网易云/汽水等大量播放器声明能力却不响应，右键菜单两项已删除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    PlayPause,
    Next,
    Prev,
    Seek(Ms),
    /// 强制重新读取一次会话（用于乐观更新回滚校验）
    Refresh,
    Mute,
    /// **全屏冻结**（v1.5.5 / G3.5）：暂停 / 恢复媒体线程的「分层轮询」。
    ///
    /// - `true`：线程阻塞在 `cmd_rx.recv()` 上，0 唤醒、0 跨进程调用；
    ///   只有命令（热键播控 / 解冻 / Quit）能叫醒它。
    /// - `false`：恢复，并让**解冻后第一轮就是全量读**（重置三层节奏时间戳，
    ///   不等 500 / 1000 / 2000ms 常规节奏）。
    ///
    /// 播控命令在 paused 期间照常执行，不受此开关影响；UI 线程只 `send`、绝不等待。
    SetPaused(bool),
    Quit,
}

/// 歌词请求。
#[derive(Debug, Clone)]
pub struct LyricRequest {
    pub track_id: TrackId,
    pub track: Track,
    /// 已知的本地歌词目录（来自 Config）
    pub local_dirs: Vec<String>,
    pub online_enabled: bool,
    /// 是否为重试
    pub retry: bool,
}

/// Rust → 设置页的消息（`ARCHITECTURE.md` §13.2）。
///
/// **M1 占位**：真正的协议实现与 WebView2 桥接在 T13（M3），
/// 届时本类型会整体搬到 `settings::bridge`，`Effect::SettingsPush` 的签名不变。
#[derive(Debug, Clone)]
pub enum HostMsg {
    /// 首次全量下发（配置 + 运行时信息 + i18n 字典）
    Init,
    /// 单字段变更回推
    Patch { path: String },
    /// 轻提示
    Toast { level: ToastLevel, text: String },
}

/// 提示级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Warn,
    Error,
}

/// 便捷别名，减少调用点的类型噪音。
pub type Sink = EventSink<AppEvent>;

#[cfg(test)]
mod tests {
    use super::*;

    /// v1.2.3（G4）：动作名是配置路径 / 前端 `<action>` / 日志的共同约定，
    /// 必须**双向一致**——写错一个字符，逐行「恢复默认」就会静默失败。
    #[test]
    fn hotkey_action_names_roundtrip() {
        for a in HotkeyAction::ALL {
            assert_eq!(HotkeyAction::parse(a.as_str()), Some(a), "{} 双向不一致", a.as_str());
        }
        assert_eq!(HotkeyAction::ALL.len(), 6);
        assert_eq!(HotkeyAction::parse("bogus"), None);
        assert_eq!(HotkeyAction::parse(""), None);
        assert_eq!(HotkeyAction::parse("PlayPause"), None, "必须是小写蛇形名");
    }
}
