//! 状态层（`ARCHITECTURE.md` §3.3 / §7.7）。
//!
//! `AppState` 是**唯一真相源**，里面没有任何 `HWND` / COM 指针，因此可单测。
//! `App::reduce(AppEvent) -> Vec<Effect>` 是纯函数；`App::apply(Vec<Effect>)` 才碰句柄。
//!
//! ## M2 的处理范围
//!
//! - 完整：`NowPlaying` / `SessionCleared` / `BackdropReady` / `CommandResult` /
//!   `WidgetInput` 全部 15 个变体 / `TaskbarGeometry` / `TaskbarCreated` /
//!   `DisplayChanged` / `ConfigChanged` / `Tick` / `Quit`
//! - 预留：`Hotkey` / `TrayCommand` / `SettingsOpenRequest` / `SettingsClosed`
//!   / `SecondInstanceLaunched`（M3/M4 落地，这里 `log_debug!` 后忽略，不 panic）
//!
//! ## 与 M1 的三处行为变化
//!
//! 1. **可见性门控真的生效了**（M1 恒显）。`NoSession` / 用户隐藏 / 任务栏收起 → 隐藏。
//! 2. **静止态背景完全透明**：不再常驻画 `CAPSULE_BG`，只有悬停才画背景层。
//! 3. **主题不再每次重排都被重置**（M1 的 `state.theme = Theme::default()` 会抹掉采样结果）。

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, MessageBoxW, PostQuitMessage, SetTimer, IDYES, MB_ICONWARNING, MB_ICONINFORMATION,
    MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
};

use crate::config::Config;
use crate::event::{AppEvent, Effect, LyricRequest, MediaCommand, TrayCommand, WidgetInput};
use crate::i18n::{self, Lang};
use crate::lyric::service::LyricService;
use crate::lyric::{LyricDraw, LyricStatus, LyricView};
use crate::media::{self, MediaService, Ms, NowPlaying, PlaybackState};
use crate::platform::dpi::scale_for;
use crate::platform::wake::{EventSink, WakeHwnd};
use crate::platform::wide::to_wide;
use crate::render::layout::{self, WidgetForm, WidgetLayout};
use crate::render::sampler::{CoverageMap, SampleRequest, Sampler};
use crate::render::text::TextKind;
use crate::render::theme::{BackgroundMode, Theme};
use crate::render::Renderer;
use crate::taskbar::geometry::{self, TaskbarGeometry};
use crate::taskbar::host::TaskbarHost;
use crate::ui::hit::HitTarget;
use crate::ui::interaction::Interaction;
use crate::ui::menu::{self, MenuCommand};
use crate::ui::widget_wnd::{self, TIMER_ANIM, TIMER_CLICK, TIMER_LYRIC, TIMER_RESUME};

/// 交互修饰态（PRD 机器 C）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HoverState {
    #[default]
    Idle,
    Hover,
    Seeking,
    VolumeHud,
}

/// 乐观更新的临时覆盖（1s 内未收到会话更新则回滚）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptimisticOverride {
    pub kind: OptimisticKind,
    /// 相对于启动的单调毫秒
    pub until_ms: u64,
}

/// 乐观更新的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimisticKind {
    Playing,
    Paused,
    NextTrack,
    PrevTrack,
}

/// 应用状态。**纯数据，无 Win32 句柄**。
#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub playing: NowPlaying,
    pub lyrics: LyricView,
    pub theme: Theme,
    pub layout: WidgetLayout,
    /// 交互修饰态（PRD 机器 C）
    pub hover: HoverState,
    /// 悬停背景不透明度 0.0..=1.0（M2 新增：淡入 120ms / 淡出 200ms）
    pub hover_alpha: f32,
    /// 歌词行不透明度 0.0..=1.0（v1.1.1：歌词开关宽度过渡期间同步淡入/淡出），
    /// 由 `App` 按 `lyric_w / LYRIC_W` 推导后写入，painter 只读
    pub lyric_alpha: f32,
    /// 长歌词滚动开关（v1.1.2，由 `config.lyrics.scroll` 在 relayout 时同步）
    pub lyric_scroll_enabled: bool,
    /// 跑马灯本帧是否激活（开关开 + 非占位 + 自然宽超出视口 > 0.5px）
    pub lyric_marquee: bool,
    /// 跑马灯当前偏移（文本左移量 0..=excess；`sync_lyric_scroll` 写入，painter 只读）
    pub lyric_scroll_offset: f32,
    /// 当前按下的按钮（M2 新增：画按下态）
    pub pressed: Option<HitTarget>,
    /// 当前指针悬停的目标（v1.5.0 / G3：关闭 × 悬停淡底；v1.5.1 起底色取代"变亮"；
    /// G4 的光晕 / 缩放复用）
    ///
    /// 与 [`AppState::pressed`] 走**同一条通道**：由 `Interaction` 维护，
    /// `sync_interaction` 每帧拷进来，painter 只读。`None` = 指针不在任何控件上。
    pub hover_target: Option<HitTarget>,
    /// 用户是否隐藏（PRD 机器 B 门控 1）
    pub user_hidden: bool,
    /// **本次运行**内被右上角 × 收起（v1.5.0 / G3）
    ///
    /// 与 [`general.widget_hidden`](crate::config::GeneralConfig::widget_hidden) 的区别：
    /// 那个是**持久化**的用户设置（托盘「显示小组件」勾选项），这个是**运行期**标记。
    /// 点 × 只置它，**绝不写配置**——用户要的语义是「这次收起来，下次启动照常显示」。
    /// 有效隐藏态 = `widget_hidden || dismissed`（见 [`App::sync_visibility`]）。
    pub dismissed: bool,
    /// 任务栏自动隐藏且已收起（门控 2）
    pub taskbar_collapsed: bool,
    /// 最终可见性
    pub visible: bool,
    /// 拖动中的临时位置（毫秒），仅 Seeking 态有意义
    pub seek_preview_ms: Ms,
    /// 音量 HUD 当前值 0..=1
    pub volume_hud: f32,
    /// 音量 HUD 剩余毫秒
    pub volume_hud_left_ms: u32,
    /// 乐观更新的临时覆盖
    pub optimistic: Option<OptimisticOverride>,
    /// 几何代次，尺寸 / 位置变化时 +1，用于丢弃过期背景
    pub geometry_epoch: u64,
    /// 切歌瞬间的旧歌词转场：`(当前行, 开始时刻)`，150ms 后清空。
    /// 转场 = 120ms 淡出 + 150ms 上移（M3 遗留 P2）。v1.1.1 起没有下一行。
    ///
    /// 存的是**字符串副本**而不是引用：切歌时 `lyrics` 会被换成 Loading，
    /// 旧那首的 `Arc<Lyrics>` 随时可能被释放。
    pub lyric_fade: Option<(String, Instant)>,
    /// 前台窗口是否**全屏**（v1.4.4 / G2-B2）。
    ///
    /// 由 `sync_capture_freeze` 每秒顺带算出（与截图态探测共用**同一次**
    /// `foreground_info()` 查询，零新增跨进程调用），心跳采样据此跳过：
    /// 全屏视频 / 游戏时读桌面既贵又没意义。
    ///
    /// 只影响「静止态心跳」这一条路径——悬停进入 / 换壁纸 / 几何变化 / 退出截图态
    /// 仍然立即采样（用户能感知的路径一条都不许变慢）。
    ///
    /// **必须配合 [`AppState::foreground_monitor`] 一起看**：只有全屏窗口就在
    /// 小组件那块屏上时，跳过心跳才有意义（见 [`same_monitor`]）。
    pub foreground_fullscreen: bool,
    /// 前台窗口所在显示器的**整块**矩形（v1.4.4 补丁，物理像素）。
    ///
    /// 来自 `ForegroundInfo::monitor`（同一次 `foreground_info()` 查询就有，零新增调用）。
    /// 与 [`App::widget_monitor`] 比较即可知道「全屏的那块屏是不是小组件所在的那块」：
    /// 副屏全屏视频不该让主屏的字色自适应停摆。拿不到时全 0。
    pub foreground_monitor: RECT,
    /// **全屏冻结**（v1.5.5 / G3.5）：同屏的前台窗口全屏时，整个小组件静止。
    ///
    /// 由 1s 心跳 [`App::sync_fullscreen_freeze`] 根据 `foreground_info()` 翻转。
    /// `true` 期间：窗口隐藏、动画 / 歌词定时器停摆且不许再挂、媒体线程暂停
    /// 分层轮询、`redraw` / `ensure_on_top` / `request_backdrop` 全部跳过。
    /// 只保留 `TIMER_GEOM`（1s）里的 `foreground_info()` 作为发现「退出全屏」的途径。
    pub frozen: bool,
    /// **解冻等待期**（v1.5.5 / G3.5）：已退出全屏、媒体线程已恢复，但还没收到
    /// 解冻后第一份 `NowPlaying`。窗口仍隐藏，等 `reduce` 把隐藏窗口刷成最新内容
    /// 再显示，避免用户看到冻结前的旧帧；400ms 一次性兜底定时器到点也强制结束。
    pub resume_pending: bool,
}

impl AppState {
    /// PRD 5.2 可见性门控，优先级从高到低。
    pub fn compute_visible(&self) -> bool {
        if self.user_hidden {
            return false;
        }
        if self.taskbar_collapsed {
            return false;
        }
        // v1.5.5 / G3.5：冻结放在「用户主动隐藏 / 任务栏收起」之后、「无会话」之前。
        // 前两者是明确的长期状态，冻结是临时状态；放在 NoSession 之前才能保证
        // `frozen == true` 时恒为 false，且解冻后由真实会话态无缝接管。
        if self.frozen {
            return false;
        }
        if self.playing.state == PlaybackState::NoSession {
            return false;
        }
        true
    }

    /// 叠加乐观更新后的「有效播放态」。
    ///
    /// 按钮图标与进度推进都读它，这样点下 ⏸ 后 200ms 内图标就会变成 ▶（PRD P0-04），
    /// 而不用等媒体线程下一轮轮询。
    pub fn effective_state(&self) -> PlaybackState {
        match self.optimistic.map(|o| o.kind) {
            Some(OptimisticKind::Playing) => PlaybackState::Playing,
            Some(OptimisticKind::Paused) => PlaybackState::Paused,
            _ => self.playing.state,
        }
    }

    /// 进度条应显示的比例（0..=1）。拖动中优先用预览值。
    pub fn progress_fraction(&self) -> f32 {
        let dur = self.playing.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        if dur <= 0 {
            return 0.0;
        }
        let pos = if self.hover == HoverState::Seeking {
            self.seek_preview_ms
        } else {
            self.playing.position_now()
        };
        (pos as f32 / dur as f32).clamp(0.0, 1.0)
    }

    /// 歌词应该用的时间轴（PRD P0-09 / T12 第 6 条）。
    ///
    /// 拖动进度条时用预览位置——否则用户拖到 3:00 而歌词还停在 1:20，
    /// 松手瞬间才跳变。暂停时 `position_now()` 返回采样值，扫光自然停住。
    pub fn lyric_time(&self) -> Ms {
        if self.hover == HoverState::Seeking {
            self.seek_preview_ms
        } else {
            self.playing.position_now()
        }
    }

    /// 这一帧要画的歌词。渲染层与 UI 层共用同一份推导，避免两边算法漂移。
    pub fn lyric_draw(&self, lang: Lang) -> LyricDraw<'_> {
        self.lyrics.draw(self.lyric_time(), lang)
    }
}

/// 应用主体。持有句柄与副作用执行器。
pub struct App {
    pub state: AppState,
    pub config: Config,
    lang: Lang,
    sink: EventSink<AppEvent>,
    rx: Receiver<AppEvent>,
    msg_hwnd: HWND,
    widget: Option<TaskbarHost>,
    /// 托盘图标（T14）。`None` 表示创建失败（不影响小组件）
    tray: Option<crate::ui::tray::Tray>,
    /// 全局快捷键（T15）
    hotkeys: crate::hotkey::Hotkeys,
    /// 设置页（T13）
    settings: crate::settings::SettingsService,
    /// 模块句柄（建设置窗口 / 加载图标用）
    hinstance: HINSTANCE,
    renderer: Option<Renderer>,
    media: Option<MediaService>,
    lyric: Option<LyricService>,
    sampler: Option<Sampler>,
    geometry: Option<TaskbarGeometry>,
    /// 小组件当前的屏幕矩形（由 `relayout` 算好，`redraw` 直接用）
    target_rect: RECT,
    interact: Interaction,
    started: Instant,
    /// 上一帧的 alpha 覆盖率图（交给采样线程剔除自遮挡）
    last_coverage: Option<Arc<CoverageMap>>,
    /// 动画定时器当前是否已挂
    anim_timer_on: bool,
    /// 歌词推进定时器当前是否已挂（仅在「播放中 + 歌词就绪」时存在）
    lyric_timer_on: bool,
    /// 歌词区当前宽度（逻辑像素，**动画中间值** 0..=160）
    lyric_w: f32,
    /// 歌词区宽度过渡（PRD P0-06 的 200ms 收合，v1.1 起为宽度而非高度）
    lyric_anim: Option<WidthAnim>,
    /// 上一帧的歌词绘制指纹，用来跳过无变化的重绘
    last_lyric_key: Option<(Option<usize>, i32)>,
    /// 上一帧的**像素指纹**（`frame_hash`）。相同就完全不碰 D2D / UpdateLayeredWindow
    last_frame_hash: Option<u64>,
    /// 被指纹去重跳过的帧数（诊断用）
    skipped_frames: u64,
    /// 上一轮几何探测是否失败（避免日志刷屏）
    geom_failed: bool,
    /// 右键菜单正在打开（v1.1.14 问题 12）：期间暂停 `ensure_on_top`——
    /// 1s 心跳把 topmost 的小组件抬到菜单之上会遮住菜单内容
    menu_open: bool,
    /// 截图态（v1.2.0 / G5 问题 2）：前景窗口是截图工具时冻结「跟屏幕变」的两件事
    /// （背景采样 + 任务栏几何重排），其余刷新（进度 / 歌词 / 封面 / 动画）照常
    capture_frozen: bool,
    /// 上一次记录的 `target_rect`（只在诊断日志里对比用，v1.2.0）
    logged_rect: RECT,
    /// 已经为当前这次「窗口实际位置漂移」记过日志（避免每秒刷屏，v1.2.0）
    drift_logged: bool,
    /// 上一次**真的**投递了背景采样请求的时刻（v1.4.4 / G2-B1）。
    ///
    /// 心跳从 1s 放宽到 `limits::BACKDROP_IDLE_MS`（3s）后，靠它算「离上次采样
    /// 够不够久」。所有立即采样入口（悬停进入 / 换壁纸 / 几何变化 / 退出截图态）
    /// 都走 `request_backdrop`，那里会刷新这个时刻——所以立即采样之后不会马上
    /// 又补一次心跳，也不会因为刚补过心跳就推迟用户操作引起的采样。
    last_backdrop: Instant,
    /// **小组件自己**所在显示器的整块矩形（v1.4.4 补丁，物理像素）。
    ///
    /// 在 `relayout_with` 里随 `target_rect` 一起刷新（几何 / DPI / 显示器变化
    /// 本来就会驱动重排，时机正好），取不到就是全 0。
    /// 用途：判断「前台全屏的那块屏」是不是「小组件这块屏」——只有同屏才跳过心跳，
    /// 否则副屏全屏视频会让主屏的字色自适应白白停摆。
    widget_monitor: RECT,
}

/// 小组件窗口矩形变化的**来源**（诊断日志用，v1.2.0 / G5 问题 2）。
///
/// 「截图时左移」这条缺陷唯一的线索就是：究竟哪条路径在遮罩期间改了窗口矩形。
/// 每个影响到 `target_rect` / 窗口位置的入口都必须显式带一个原因，
/// 不允许再出现 "reason=Unknown" 之外的模糊日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RectChangeReason {
    /// 任务栏几何探测结果变了（`on_timer` 心跳 / `TaskbarCreated`）
    TaskbarGeometry,
    /// 置顶轮询时发现**窗口实际位置**已经和 `target_rect` 不一致
    EnsureOnTop,
    /// 尺寸类变化：歌词开关宽度动画、配置变更、标题宽度变化
    Resize,
    /// `WM_DISPLAYCHANGE`（分辨率 / DPI / 显示器增删）
    DpiChange,
    /// 兜底：`Effect::Relayout` 之外的纯刷新入口（第二实例唤起等）。
    ///
    /// 日志里出现 `reason=Unknown` 不代表有 bug，但它**不提供线索**——
    /// 定位「左移」时应优先看 `TaskbarGeometry` / `EnsureOnTop`。
    Unknown,
}

impl RectChangeReason {
    /// 日志友好名（**稳定字符串**，日志检索依赖它）。
    pub fn as_str(self) -> &'static str {
        match self {
            RectChangeReason::TaskbarGeometry => "TaskbarGeometry",
            RectChangeReason::EnsureOnTop => "EnsureOnTop",
            RectChangeReason::Resize => "Resize",
            RectChangeReason::DpiChange => "DpiChange",
            RectChangeReason::Unknown => "Unknown",
        }
    }
}

/// 歌词区宽度过渡动画（v1.1：歌词区移入条带后，收合动画从高度改为宽度）。
#[derive(Debug, Clone, Copy)]
struct WidthAnim {
    from: f32,
    to: f32,
    start: Instant,
}

/// 宽度过渡时长（PRD P0-06：200ms）。
const LYRIC_ANIM_MS: f32 = 200.0;
/// 切歌转场总时长（M3 遗留 P2：120ms 淡出 + 150ms 上移，窗口取并集 150ms）。
pub const LYRIC_FADE_SECS: f32 = 0.150;
/// 转场里旧歌词的淡出段时长（T12 第 4 条原口径，保持观感不变）。
pub const LYRIC_FADE_OUT_SECS: f32 = 0.120;
/// 转场里旧歌词的上移总位移（逻辑像素，随 DPI 缩放；ease-out cubic）。
pub const LYRIC_SLIDE_PX: f32 = 6.0;

/// 解冻兜底超时（毫秒，v1.5.5 / G3.5）。**一次性**定时器，挂在消息窗上。
///
/// 为什么是 400ms：
/// - **下界 200ms**：媒体线程解冻后第一轮全量读要跨进程访问 SMTC，正常 1~3ms、
///   慢调用阈值 8ms，200ms 足够覆盖一次异常慢的读取并让 `NowPlaying` 回到 UI；
///   再短就会在状态到达前提前显示旧帧；
/// - **上界 1000ms**：`TIMER_GEOM` 心跳就是 1s，用户退出全屏后最多再等一个
///   心跳周期就必须看见小组件，不能更慢（也不许为此把心跳调快，见禁改清单）；
/// - 400ms 落在中间：即使媒体线程异常 / 当前无会话（永远不会有 `NowPlaying`），
///   也不会出现「小组件再也不显示」。
///
/// 它不新增任何轮询频率：只 `SetTimer` 一次，触发或被 `finish_resume` 取消后即结束。
pub const RESUME_FALLBACK_MS: u32 = 400;

impl WidthAnim {
    /// 当前时刻的插值宽度（ease-out cubic：收合时先快后慢，观感更自然）。
    fn value(&self, now: Instant) -> (f32, bool) {
        let t = now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / LYRIC_ANIM_MS;
        if t >= 1.0 {
            return (self.to, true);
        }
        let t = t.clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - t).powi(3);
        (self.from + (self.to - self.from) * e, false)
    }
}

impl App {
    /// 启动期组装：配置 → 窗口类 / 消息窗 → 事件通道 → 渲染器 → 采样线程
    /// → 小组件窗口 → 媒体线程。
    ///
    /// 返回裸指针（不是 `Box`）：WndProc 需要在 `GWLP_USERDATA` 里存它，
    /// 而 `Box` 一旦移动地址就变。生命周期由 `main` 通过 [`App::shutdown`] 收尾。
    pub fn bootstrap() -> windows::core::Result<*mut App> {
        let mut config = Config::load();
        // ---- 数据目录（v1.1.5 问题 3）：必须在任何缓存 / 歌词模块使用之前。
        // 从上次生效目录向新目录净搬旧缓存 / 歌词目录（含日志搬迁）。
        crate::storage::init_from_config(&mut config);
        let lang = Lang::from_serde(config.general.language);
        crate::log_info!(
            "启动：语言={:?} 歌词={}({:?}) 背景={:?}",
            lang,
            config.lyrics.enabled,
            config.lyrics.mode,
            config.appearance.background_mode
        );

        // SAFETY: GetModuleHandleW(None) 取当前 exe 的模块句柄，永不失败（除非句柄无效）。
        let hmodule = unsafe {
            windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::PCWSTR::null())
        }?;
        let hinstance = HINSTANCE(hmodule.0);

        let msg_hwnd = crate::ui::install(hinstance)?;
        let (sink, rx) = EventSink::new(WakeHwnd(msg_hwnd));

        let geometry = probe_geometry(&config);
        match geometry.as_ref() {
            Some(g) => crate::log_info!("初始几何：{}", g.summary()),
            None => crate::log_warn!("初始几何探测失败（任务栏不可用），等待 1s 定时器重试"),
        }
        let dpi = geometry.as_ref().map(|g| g.dpi).unwrap_or(96);

        let renderer = match Renderer::new(dpi) {
            Ok(r) => Some(r),
            Err(e) => {
                crate::log_err!("渲染器创建失败（{e}），小组件将不可见");
                None
            }
        };

        let sampler = Sampler::spawn(sink.clone());

        let widget = match geometry.as_ref() {
            Some(g) => match TaskbarHost::attach(hinstance, g.target, 0) {
                Ok(h) => Some(h),
                Err(e) => {
                    crate::log_err!("小组件窗口创建失败：{e}");
                    None
                }
            },
            None => None,
        };

        let target_rect = geometry.as_ref().map(|g| g.target).unwrap_or_default();

        // 歌词区初始宽度 = 配置对应的信息区最小宽（不做入场动画）
        let lyric_w = lyric_w_logical(&config);

        let mut app = Box::new(App {
            state: AppState {
                theme: Theme::default(),
                ..Default::default()
            },
            config,
            lang,
            sink: sink.clone(),
            rx,
            msg_hwnd,
            widget,
            tray: None,
            hotkeys: crate::hotkey::Hotkeys::default(),
            settings: crate::settings::SettingsService::new(),
            hinstance,
            renderer,
            media: None,
            lyric: None,
            sampler,
            geometry,
            target_rect,
            interact: Interaction::new(),
            started: Instant::now(),
            last_coverage: None,
            anim_timer_on: false,
            lyric_timer_on: false,
            lyric_w,
            lyric_anim: None,
            last_lyric_key: None,
            last_frame_hash: None,
            skipped_frames: 0,
            geom_failed: false,
            menu_open: false,
            capture_frozen: false,
            logged_rect: target_rect,
            drift_logged: false,
            // 启动时先当作「刚采过」：首帧背景由随后的 `relayout` → `request_backdrop`
            // 立即投递，不必再叠一次心跳。
            last_backdrop: Instant::now(),
            // 由紧随其后的 `relayout()` 填真值；全 0 时 `same_monitor` 判 false，
            // 也就是「继续采样」——安全方向。
            widget_monitor: RECT::default(),
        });

        // 歌词行不透明度与初始宽度同步（歌词默认开启时首帧就是实色）
        app.sync_lyric_alpha();
        // 长歌词滚动开关初始同步
        app.state.lyric_scroll_enabled = app.config.lyrics.scroll;

        let ptr = Box::into_raw(app);

        // 托盘图标（T14）：失败只记日志，不影响小组件
        unsafe {
            match crate::ui::tray::Tray::install(hinstance, 0) {
                Ok(t) => {
                    (*ptr).tray = Some(t);
                }
                Err(e) => crate::log_err!("托盘窗口创建失败：{e}"),
            }
        }

        // 绑定 App* 到三个窗口的 GWLP_USERDATA
        crate::ui::widget_wnd::bind_app(msg_hwnd, ptr);
        // SAFETY: ptr 由上一行的 Box::into_raw 产生，此刻还没有第二个持有者；
        // 只读地借用其中的 widget 字段（`as_ref` 不移动、不释放）。
        if let Some(w) = unsafe { (*ptr).widget.as_ref() } {
            crate::ui::widget_wnd::bind_app(w.hwnd(), ptr);
        }
        if let Some(t) = unsafe { (*ptr).tray.as_ref() } {
            crate::ui::widget_wnd::bind_app(t.hwnd(), ptr);
        }

        // 启动媒体 / 歌词线程（MTA / 普通线程）。失败不阻塞启动。
        // SAFETY: 同上——`ptr` 是刚 `Box::into_raw` 出来的独占指针，且本函数还没返回，
        // 没有任何其它线程或窗口过程持有它（GWLP_USERDATA 刚写入，消息泵尚未启动）。
        unsafe {
            (*ptr).media = MediaService::spawn(sink.clone());
            (*ptr).lyric = LyricService::spawn(sink);
            if let Some(t) = (*ptr).tray.as_mut() {
                t.add((*ptr).lang);
            }
            let cfg = (*ptr).config.hotkeys;
            (*ptr).hotkeys.register_all((*ptr).msg_hwnd, &cfg);
            // 首启发现快捷键冲突 → 托盘气泡提示一次（不打断用户）
            if !(*ptr).hotkeys.conflicts.is_empty() && !(*ptr).config.hotkeys.conflict_notified {
                if let Some(t) = (*ptr).tray.as_ref() {
                    let title = i18n::t((*ptr).lang, "tray.conflict_title");
                    let text = i18n::t((*ptr).lang, "tray.conflict_text");
                    t.show_balloon((*ptr).lang, title, text, true);
                }
                (*ptr).config.hotkeys.conflict_notified = true;
                let cfg = (*ptr).config.clone();
                (*ptr).apply(vec![Effect::SaveConfig(Box::new(cfg))]);
            }
            (*ptr).relayout();
        }

        crate::log_info!("启动完成，进入消息泵");
        Ok(ptr)
    }

    /// 收尾：停媒体线程 → 卸载钩子 → 停采样线程 → 销毁窗口 → 释放 `App`。
    ///
    /// # Safety
    /// 必须由 `main` 在消息泵退出后、UI 线程上调用**恰好一次**，
    /// 且此后不得再使用该指针。
    pub unsafe fn shutdown(ptr: *mut App) {
        if ptr.is_null() {
            return;
        }
        // SAFETY: 调用方保证独占所有权且只调用一次。
        let mut app = Box::from_raw(ptr);
        widget_wnd::uninstall_wheel_hook();
        if let Some(media) = app.media.as_mut() {
            media.shutdown();
        }
        if let Some(lyric) = app.lyric.as_mut() {
            lyric.shutdown();
        }
        app.hotkeys.unregister_all(app.msg_hwnd);
        if let Some(sampler) = app.sampler.as_mut() {
            sampler.shutdown();
        }
        if let Some(host) = app.widget.take() {
            host.destroy();
        }
        crate::log_info!("已收尾（运行 {:?}）", app.started.elapsed());
        drop(app);
    }

    // ---------------------------------------------------------------- 事件入口

    /// `WM_BARLINE_WAKE`：把队列里的事件全部 drain 掉。
    pub fn on_wake(&mut self) {
        // 防御性上限：一次唤醒最多处理 512 条，避免异常情况下卡住消息泵。
        let mut budget = 512;
        while budget > 0 {
            let Ok(evt) = self.rx.try_recv() else {
                break;
            };
            budget -= 1;
            let effects = self.reduce(evt);
            self.apply(effects);
        }
        self.sync_lyric_timer();
    }

    /// 消息窗的 `WM_TIMER`：`TIMER_GEOM`（1000ms）走几何心跳，
    /// `TIMER_RESUME`（一次性 400ms）走解冻兜底（v1.5.5 / G3.5）。
    pub fn on_timer(&mut self, id: usize) {
        // 解冻兜底：无论媒体线程有没有回状态，到点都结束 resume_pending。
        // 若第一份 `NowPlaying` 已先到，`finish_resume` 的守卫保证不会重复显示。
        if id == TIMER_RESUME {
            self.kill_msg_timer(TIMER_RESUME);
            if self.state.resume_pending {
                self.finish_resume("兜底 400ms");
            }
            return;
        }

        self.tick_optimistic();
        // G2 诊断：截图态探测要碰**前台进程**（OpenProcess + 查询镜像名），
        // 它有被安全软件挂钩拖慢的可能，单独计时。
        // v1.5.5：冻结态下 `sync_capture_freeze` 只跑纯本地的 `foreground_info()`，
        // 这正是「发现退出全屏」的唯一途径（1s 一次，不新增频率、不调快）。
        {
            let _sp = crate::platform::perf::Span::new("tick.capture");
            self.sync_capture_freeze();
        }

        // ---- v1.5.5 / G3.5：全屏冻结 ----
        // 冻结期间这一 tick 只做上面那一次 `foreground_info()`（纯本地、微秒级）。
        // 任务栏几何探测（FindWindow / 托盘遍历 / SHAppBarMessage）、ensure_on_top
        // （SetWindowPos）、静止态背景采样、任何重绘全部跳过。
        if self.state.frozen {
            return;
        }

        // ---- 截图态：冻结「跟屏幕变」的两件事（v1.2.0 / G5 问题 2）----
        //
        // 截图工具的置顶遮罩会改变抓屏内容（整屏压暗 / 十字线），也会让任务栏几何
        // 短暂抖动。这两样一旦跟着变，用户就会看到「截图时文字闪一下 / 小组件左移」。
        // 所以截图态期间：**不探测几何、不置顶、不采样**。
        // 播放进度、歌词、封面、动画帧全部照常（它们在 `on_tick` / `TIMER_ANIM` 里）。
        if !self.capture_frozen {
            // G2 诊断：probe 里有 FindWindowW / GetWindowRect / **SHAppBarMessage**
            // （后者是跨进程调用），是「随机卡一下」的头号嫌疑段。
            let probe_result = {
                let _sp = crate::platform::perf::Span::new("tick.probe");
                geometry::probe()
            };
            match probe_result {
                Some(g) => {
                    let changed = match self.geometry.as_ref() {
                        Some(old) => !old.same_shape(&g),
                        None => true,
                    };
                    let epoch_changed = self.geometry.as_ref().map(|o| o.epoch) != Some(g.epoch);
                    // 诊断（v1.2.0）：只说"几何变化"没法定位「左移」，必须列出变了的字段
                    let fields = self.geometry.as_ref().map(|old| old.diff(&g)).unwrap_or_default();

                    crate::log_debug!("geometry: {}", g.summary());
                    self.geometry = Some(g);
                    self.geom_failed = false;

                    if changed {
                        crate::log_info!(
                            "几何变化（字段={}）→ 重新布局：{}",
                            if fields.is_empty() { "?" } else { fields.as_str() },
                            g.summary()
                        );
                        self.relayout_with(RectChangeReason::TaskbarGeometry);
                    } else if epoch_changed {
                        self.state.geometry_epoch = g.epoch;
                    }
                }
                None => {
                    if !self.geom_failed {
                        crate::log_warn!("geometry: probe 失败（任务栏不可用，Explorer 可能在重启）");
                        self.geom_failed = true;
                    }
                }
            }

            // 风险 R1 的最小缓解：任务栏与小组件同为 WS_EX_TOPMOST，同层窗口按「最近置顶」排序。
            // 我们从不激活，所以任何被后来置顶的 topmost 窗口都会压住小组件。
            // v1.1.14 问题 12：右键菜单打开期间必须暂停——TrackPopupMenuEx 的模态
            // 循环会照常派发 WM_TIMER，每秒把小组件抬到菜单之上（菜单非 topmost）。
            if !self.menu_open {
                // G2 诊断：ensure_on_top 里有 GetWindowRect + **SetWindowPos**（每秒一次），
                // 后者会触发 z-order 重排与 DWM 重合成，是「整屏顿一下」的嫌疑段。
                let _sp = crate::platform::perf::Span::new("tick.ontop");
                self.ensure_on_top();
            }
        }

        // 背景兜底重采样（PRD P0-07「窗口移动时 300ms 内刷新」的近似）。
        //
        // 壁纸 / 主题变化有 `WM_SETTINGCHANGE` 精确通知，但**用户把窗口拖到小组件后面**
        // 不会有任何通知；只靠悬停才采样的话，静止态会一直用着旧背景算出来的文字颜色
        // （深色窗口滑到浅色壁纸后面 → 白字压在浅色上，直接看不清）。
        //
        // v1.4.4（G2-B1 / B2）：这条**只是兜底心跳**，所以两处降载——
        //   1. 间隔 1s → `limits::BACKDROP_IDLE_MS`（3s）：静止态晚 2 秒才更新字色，
        //      用户看不出来；抓屏次数直接降到 1/3（`BitBlt` 是残余卡顿的头号嫌疑）；
        //   2. 前台全屏应用（视频 / 游戏）时整个跳过：读桌面既贵又没意义。
        // **能感知的路径一律不受影响**：悬停进入、换壁纸 / 主题、几何变化、
        // 退出截图态、切播放器导致的几何重排都直接调 `request_backdrop()`，不等心跳。
        //
        // v1.4.4 补丁：跳过的前提是**全屏的那块屏就是小组件这块屏**。
        // 副屏全屏打游戏时主屏小组件仍要正常跟壁纸变字色——那条心跳存在的唯一理由
        // 就是「用户把窗口拖到小组件后面」，与别的显示器上的全屏毫无关系。
        if self.state.visible && !self.is_hovering() && !self.capture_frozen {
            let elapsed_ms = self.last_backdrop.elapsed().as_millis();
            let skip = should_skip_idle_backdrop(
                self.state.foreground_fullscreen,
                self.state.foreground_monitor,
                self.widget_monitor,
            );
            if idle_backdrop_due(elapsed_ms, skip) {
                // G2 诊断：这里只做「拷 Shared 状态 + mpsc 发送」，正常应是微秒级；
                // 一旦慢就说明 Arc clone / 采样请求槽成了瓶颈（对应采样线程的 capture 段）。
                let _sp = crate::platform::perf::Span::new("tick.backdrop");
                self.request_backdrop();
            }
        }
    }

    /// 截图态探测 + 进入/退出日志（v1.2.0 / G5 问题 2）。
    ///
    /// 只在**状态真的翻转**时记日志（1s 心跳下不能每秒刷一条）；
    /// 退出时立刻补一次 `request_backdrop()`——否则用户会看到字色慢一拍，
    /// 要等下一个心跳才恢复。
    ///
    /// v1.4.4（G2-B2）：**同一次** `foreground_info()` 查询顺带刷新「前台是否全屏」，
    /// 供心跳采样判断复用——全屏判定因此**没有新增任何跨进程调用**。
    ///
    /// v1.4.4 补丁：同一次结果里的 `monitor` 也一并存下来（零额外调用）。
    /// 它必须**无条件**刷新（不能只在全屏翻转时更新）：全屏窗口被拖到另一块屏时
    /// `fullscreen` 一直是 `true`，但「在哪块屏」变了——不更新就会误判成同屏。
    fn sync_capture_freeze(&mut self) {
        let info = crate::platform::capture::foreground_info();
        self.state.foreground_monitor = info.monitor;
        // 全屏标记先落地（下面可能因「状态没变」提前 return，但标记必须是最新的）
        if info.fullscreen != self.state.foreground_fullscreen {
            self.state.foreground_fullscreen = info.fullscreen;
            if info.fullscreen {
                crate::log_info!(
                    "前台全屏 {}×{}（rect=({},{},{},{}) monitor=({},{},{},{})）→ 同屏时暂停静止态心跳采样（悬停/换壁纸仍立即采样）",
                    info.monitor.right - info.monitor.left,
                    info.monitor.bottom - info.monitor.top,
                    info.rect.left,
                    info.rect.top,
                    info.rect.right,
                    info.rect.bottom,
                    info.monitor.left,
                    info.monitor.top,
                    info.monitor.right,
                    info.monitor.bottom
                );
            } else {
                crate::log_info!("退出前台全屏 → 恢复 3s 心跳背景采样");
            }
        }

        // v1.5.5（G3.5）：全屏冻结判定与心跳共用**同一次** `foreground_info()`，
        // 零新增跨进程调用。必须放在下面「截图态状态没变就提前 return」之前——
        // 冻结与否不依赖截图态是否翻转。
        self.sync_fullscreen_freeze(&info);

        // 冻结期间连截图态探测（OpenProcess + 查进程镜像名，跨进程）也跳过：
        // 这一 tick 只允许留下纯本地的 `foreground_info()`，它是发现「退出全屏」
        // 的唯一途径。解冻后的下一个 tick 会照常重新判定截图态。
        if self.state.frozen {
            return;
        }

        let now = self.detect_capture_with(&info);
        if now == self.capture_frozen {
            return;
        }
        self.capture_frozen = now;
        if now {
            crate::log_info!("进入截图态：冻结背景采样与几何重排（媒体 / 歌词 / 动画照常）");
        } else {
            crate::log_info!("退出截图态：恢复刷新，立即补一次背景采样");
            self.request_backdrop();
        }
    }

    /// 全屏冻结的**进入 / 退出**（v1.5.5 / G3.5）。
    ///
    /// 每秒一次，与心跳共用 [`crate::platform::capture::foreground_info`] 的结果。
    /// 判据是纯函数 [`frozen_decision`]：前台全屏 **且** 那块屏就是小组件所在屏。
    /// 只在状态真的翻转时执行动作——冻结期间不重复隐藏、不重复发命令。
    fn sync_fullscreen_freeze(&mut self, info: &crate::platform::capture::ForegroundInfo) {
        let want = frozen_decision(info.fullscreen, info.monitor, self.widget_monitor);
        if want == self.state.frozen {
            return;
        }
        if want {
            self.enter_fullscreen_freeze(info);
        } else {
            self.exit_fullscreen_freeze();
        }
    }

    /// 进入冻结（false→true，只执行一次）。
    fn enter_fullscreen_freeze(&mut self, info: &crate::platform::capture::ForegroundInfo) {
        // 1) 立刻隐藏（不绕 sync_visibility：那边会按门控重算，没必要）
        if let Some(h) = self.widget.as_mut() {
            h.set_visible(false);
        }
        // 2) 停掉两个会持续唤醒 UI 的定时器。`start_widget_timer` 里另有 frozen
        //    守卫，冻结期间谁都别想再把它们挂回来。
        self.kill_widget_timer(TIMER_ANIM);
        self.kill_widget_timer(TIMER_LYRIC);
        self.last_lyric_key = None;
        // 3) 媒体线程完全停下：只 send，绝不等待媒体线程（不 join / 不 SendMessage）
        self.send_media(MediaCommand::SetPaused(true));
        // 4) 置位：compute_visible / redraw / request_backdrop / ensure_on_top /
        //    on_timer 全靠它跳过；state.visible 与实际隐藏保持一致。
        self.state.frozen = true;
        self.state.visible = false;
        let w = info.monitor.right - info.monitor.left;
        let h = info.monitor.bottom - info.monitor.top;
        crate::log_info!("进入全屏冻结：{w}×{h} 媒体轮询暂停、渲染停止");
    }

    /// 退出冻结（true→false，只执行一次）。
    ///
    /// 顺序是「不闪旧内容」的关键：**先不显示**，等 `reduce` 收到解冻后第一份
    /// `NowPlaying`（或 400ms 兜底）把隐藏窗口刷成最新内容，再 `set_visible(true)`。
    fn exit_fullscreen_freeze(&mut self) {
        // 1) 恢复媒体线程：SetPaused(false) 让第一轮立即全量读；
        //    Refresh 再兜一次「强制重读」。两者共用命令通道，只 send 不 join。
        self.state.frozen = false;
        self.send_media(MediaCommand::SetPaused(false));
        self.send_media(MediaCommand::Refresh);
        // 2) 置等待标志；sync_visibility 会因此保持隐藏（见其中的 resume 门控）
        self.state.resume_pending = true;
        self.sync_visibility();
        // 3) 一次性兜底定时器（消息窗；不是新的轮询频率）
        self.start_msg_timer(TIMER_RESUME, RESUME_FALLBACK_MS);
        crate::log_info!("退出全屏 → 解冻（{}ms 后恢复显示）", RESUME_FALLBACK_MS);
    }

    /// 结束解冻等待期：先把隐藏窗口刷成最新内容，再显示。
    ///
    /// `source` 只进日志（`"NowPlaying"` / `"兜底 400ms"`），便于确认走的是
    /// 「状态先到」还是「超时兜底」。
    fn finish_resume(&mut self, source: &str) {
        if !self.state.resume_pending {
            return;
        }
        // 1) 窗口仍隐藏时刷新内容：`relayout_with` 内部顺序是 apply_geometry →
        //    `redraw()` → `sync_visibility()`；此时 resume_pending 仍为 true，
        //    sync_visibility 保持隐藏，绝不会提前露出旧帧。
        //    （`redraw` 对隐藏的 layered window 有效：UpdateLayeredWindow 只是提交位图。）
        self.relayout_with(RectChangeReason::Unknown);
        // 2) 内容已画好 → 显示。几何不可用时 relayout 已隐藏窗口，不强行显示，
        //    等下次几何就绪自然恢复；其余可见性门控（user_hidden / NoSession 等）照旧。
        let host = if self.geometry.is_some() && self.state.compute_visible() {
            self.widget.as_mut()
        } else {
            None
        };
        if let Some(h) = host {
            h.set_visible(true);
        }
        // 3) 清标志 + 停兜底定时器 + 让 state.visible / 歌词定时器回到常规门控
        self.state.resume_pending = false;
        self.kill_msg_timer(TIMER_RESUME);
        self.sync_visibility();
        self.sync_lyric_timer();
        crate::log_info!("解冻完成（{source}），恢复显示");
    }

    /// 前景窗口是不是已知的截图工具（用**已经取好**的前台信息，不重复查询）。
    fn detect_capture_with(&self, info: &crate::platform::capture::ForegroundInfo) -> bool {
        if info.pid == 0 {
            return false;
        }
        // 平台层不解析进程名（避免反向依赖 media），这里补上最后一步
        let process = crate::media::launch::process_image_name(info.pid);
        crate::platform::capture::is_capture_tool(&process, &info.class)
    }

    /// `WM_SETTINGCHANGE` / `WM_THEMECHANGED`：壁纸或主题变了 → 立即重新采样。
    ///
    /// PRD P0-07 要求「切换壁纸后 300ms 内完成黑白切换」，采样线程本身有 300ms 节流，
    /// 所以这里必须**立刻**投递请求，不能等兜底心跳（`limits::BACKDROP_IDLE_MS` = 3s）。
    pub fn on_setting_changed(&mut self) {
        crate::log_debug!("系统设置 / 主题变化 → 重新采样背景");
        self.request_backdrop();
    }

    /// `TaskbarCreated` 广播（Explorer 重启）。
    pub fn on_taskbar_created(&mut self) {
        crate::log_info!("收到 TaskbarCreated（Explorer 已重启），重新挂载");
        if self.widget.is_none() {
            if let Some(g) = probe_geometry(&self.config) {
                let hinstance = current_hinstance();
                match TaskbarHost::attach(hinstance, g.target, self as *mut App as isize) {
                    Ok(h) => self.widget = Some(h),
                    Err(e) => crate::log_err!("重新创建小组件窗口失败：{e}"),
                }
                self.geometry = Some(g);
            }
        }
        // 托盘图标在 Explorer 重启后会消失，必须重新 NIM_ADD（T14 第 4 条）
        if let Some(t) = self.tray.as_mut() {
            t.add(self.lang);
        }
        self.relayout_with(RectChangeReason::TaskbarGeometry);
        self.ensure_on_top();
    }

    /// `WM_DISPLAYCHANGE`：分辨率 / DPI / 显示器增删。
    pub fn on_display_changed(&mut self) {
        crate::log_info!("WM_DISPLAYCHANGE：重建渲染目标");
        self.geometry = None;
        self.last_coverage = None;
        self.relayout_with(RectChangeReason::DpiChange);
    }

    /// `WM_HOTKEY`：全局快捷键按下。
    pub fn on_hotkey(&mut self, wparam: usize) {
        let Some(action) = crate::hotkey::action_of_wparam(wparam) else {
            return;
        };
        crate::log_info!("热键触发：{action:?}");
        let effects = self.reduce(AppEvent::Hotkey(action));
        self.apply(effects);
    }

    /// 托盘图标的回调消息（`WM_BARLINE_TRAY`）。
    ///
    /// 右键抬起 → 弹菜单；左键双击 → 打开设置（PRD P0-12）。
    pub fn on_tray_message(&mut self, lparam: LPARAM) -> Option<LRESULT> {
        if crate::ui::tray::mouse_event_of(lparam) == 0 {
            return Some(LRESULT(0));
        }
        if crate::ui::tray::is_right_up(lparam) {
            crate::log_info!("托盘回调：右键抬起，弹菜单");
            // v1.3.1：勾选态 = 「小组件正在显示」。换算只能经过 tray::toggle_checked，
            // 历史 bug 就是这里把 `widget_hidden` 直接当勾选态传进去（勾选态反了）。
            // v1.5.0（G3）：传**有效隐藏态**（配置 ∨ 本次运行的 × 收起），
            // 否则点了 × 收起后托盘里仍然是打钩的，用户会以为没生效。
            let widget_visible = crate::ui::tray::toggle_checked(
                self.config.general.widget_hidden || self.state.dismissed,
            );
            let cmd = self
                .tray
                .as_ref()
                .and_then(|t| t.show_menu(self.lang, widget_visible));
            if let Some(cmd) = cmd {
                let effects = self.reduce(AppEvent::TrayCommand(cmd));
                self.apply(effects);
            }
        } else if crate::ui::tray::is_double_click(lparam) {
            crate::log_info!("托盘图标双击 → 打开设置");
            let effects = self.reduce(AppEvent::TrayCommand(TrayCommand::OpenSettings));
            self.apply(effects);
        }
        Some(LRESULT(0))
    }

    /// 第二个实例请求唤起：打开（或前置）设置页。
    pub fn on_second_instance(&mut self) {
        crate::log_info!("第二个实例请求唤起 → 打开设置页");
        // 纯刷新（几何没变、窗口不该动）：用兜底原因，日志里不会被误当成位移来源
        self.relayout_with(RectChangeReason::Unknown);
        let effects = self.reduce(AppEvent::SettingsOpenRequest);
        self.apply(effects);
    }

    // ---------------------------------------------------------------- 小组件输入

    /// 小组件窗口的全部消息。返回 `Some` 表示已处理（不再交给 `DefWindowProcW`）。
    pub fn on_widget_message(
        &mut self,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        use windows::Win32::UI::WindowsAndMessaging as wam;

        // G2 诊断：只给**鼠标消息**计时（键盘 / 广播消息不在排查范围）。
        // 覆盖范围含消息处理里同步触发的重绘（`dispatch` → `redraw`），
        // 所以它与 `paint.frame` 会重叠：`msg.mouse` 明显更大就说明「重绘之外」
        // 还有别的活儿（命中测试 / 钩子安装 / 日志）。
        //
        // v1.4.4（G2-A1）：右键会同步进 `TrackPopupMenuEx` 的**模态消息循环**，
        // 菜单开着多久就被记成多慢（历史上那条 `450001ms` 是用户开着菜单 7.5 分钟，
        // 不是卡顿）。所以 `WM_RBUTTONUP` 分支在弹菜单前显式 `finish()`，
        // 见下面的 `sp.take()`。
        let mut sp = is_mouse_message(msg).then(|| crate::platform::perf::Span::new("msg.mouse"));

        let layout = self.state.layout.clone();
        match msg {
            wam::WM_MOUSEMOVE => {
                let (x, y) = client_xy(lparam);
                self.request_mouse_leave();
                let inputs = self.interact.on_move(&layout, x, y);
                self.dispatch(inputs);
                self.ensure_wheel_hook();
                Some(LRESULT(0))
            }
            widget_wnd::WM_MOUSELEAVE => {
                let inputs = self.interact.on_leave();
                self.dispatch(inputs);
                widget_wnd::uninstall_wheel_hook();
                self.interact.wheel_hooked = false;
                Some(LRESULT(0))
            }
            wam::WM_LBUTTONDOWN => {
                let (x, y) = client_xy(lparam);
                let (inputs, capture) = self.interact.on_down(&layout, x, y);
                if capture {
                    if let Some(h) = self.widget.as_ref() {
                        // SAFETY: hwnd 有效；SetCapture 只影响鼠标路由，不抢焦点。
                        unsafe { SetCapture(h.hwnd()) };
                    }
                }
                self.dispatch(inputs);
                Some(LRESULT(0))
            }
            wam::WM_LBUTTONUP => {
                let (x, y) = client_xy(lparam);
                let (inputs, release) = self.interact.on_up(&layout, x, y);
                if release {
                    // SAFETY: 与 SetCapture 配对。
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                self.dispatch(inputs);
                Some(LRESULT(0))
            }
            wam::WM_CAPTURECHANGED => {
                let inputs = self.interact.on_capture_lost();
                self.dispatch(inputs);
                Some(LRESULT(0))
            }
            wam::WM_RBUTTONUP => {
                // v1.4.4（G2-A1）：先把 span 结算掉再进菜单的模态循环——
                // 否则「用户开着菜单看了 7.5 分钟」会被记成 `msg.mouse 耗时=450001ms`，
                // 让之后每一次看日志的人（包括我自己）都误判成卡顿。
                // 这里量到的是「弹菜单之前那点活儿」，菜单期间的耗时不再计入。
                if let Some(sp) = sp.take() {
                    sp.finish();
                }
                self.show_menu(lparam);
                Some(LRESULT(0))
            }
            wam::WM_MOUSEWHEEL => {
                // 理论上 WS_EX_NOACTIVATE 收不到，这里是双保险（万一某天路由变了）
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let hud_ms = self.config.volume.hud_ms;
                let inputs = self.interact.on_wheel(delta, hud_ms);
                self.dispatch(inputs);
                Some(LRESULT(0))
            }
            crate::platform::wake::WM_BARLINE_WHEEL => {
                let delta = wparam.0 as i32;
                let hud_ms = self.config.volume.hud_ms;
                let inputs = self.interact.on_wheel(delta, hud_ms);
                self.dispatch(inputs);
                Some(LRESULT(0))
            }
            // 壁纸 / 主题 / 系统强调色变化 → 立即重新采样（PRD P0-07：300ms 内完成黑白切换）。
            //
            // **必须在这里接**：`WM_SETTINGCHANGE` 是广播消息，只发给 top-level 窗口。
            // 消息窗是 `HWND_MESSAGE`（message-only），永远收不到广播——只挂在
            // `msg_proc` 上等于没接。小组件窗口是 `WS_POPUP` top-level，才收得到。
            wam::WM_SETTINGCHANGE | wam::WM_THEMECHANGED => {
                self.on_setting_changed();
                Some(LRESULT(0))
            }
            wam::WM_TIMER => {
                let id = wparam.0;
                if id == TIMER_ANIM {
                    self.on_anim_timer();
                } else if id == TIMER_CLICK {
                    self.kill_widget_timer(TIMER_CLICK);
                    let inputs = self.interact.on_click_timer();
                    self.dispatch(inputs);
                } else if id == TIMER_LYRIC {
                    self.on_lyric_tick();
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// 16ms 动画定时器。
    fn on_anim_timer(&mut self) {
        // 歌词区宽度过渡（v1.1：歌词开关在条带内收合/展开）
        let width_changed = self.advance_width_anim();
        // 切歌转场：150ms 到点就清空并补一帧
        let fade_done = self.expire_lyric_fade();

        let inputs = self
            .interact
            .on_anim_tick(crate::config::defaults::limits::ANIM_TIMER_MS);
        let mut redraw = false;
        for input in inputs {
            let effects = self.reduce(AppEvent::WidgetInput(input));
            for e in effects {
                if matches!(e, Effect::Redraw) {
                    redraw = true;
                } else {
                    self.apply(vec![e]);
                }
            }
        }
        self.sync_interaction();
        if width_changed {
            // 宽度变了必须整条重排（窗口尺寸 + 渲染目标 + 采样区域都要跟着变）
            self.relayout_with(RectChangeReason::Resize);
            return;
        }
        if redraw || fade_done || self.interact.animating() || self.lyric_fade_active() {
            self.redraw();
        }
    }

    /// 切歌转场是否还在进行（150ms 内）。
    fn lyric_fade_active(&self) -> bool {
        self.state
            .lyric_fade
            .as_ref()
            .map(|(_, t)| t.elapsed().as_secs_f32() < LYRIC_FADE_SECS)
            .unwrap_or(false)
    }

    /// 淡出到点就清空。返回 `true` 表示刚清空（需要补一帧）。
    fn expire_lyric_fade(&mut self) -> bool {
        if self.state.lyric_fade.is_some() && !self.lyric_fade_active() {
            self.state.lyric_fade = None;
            return true;
        }
        false
    }

    /// 推进歌词区宽度动画。返回 `true` 表示**取整后的宽度**变了，需要重排。
    fn advance_width_anim(&mut self) -> bool {
        let Some(anim) = self.lyric_anim else {
            return false;
        };
        let (value, done) = anim.value(Instant::now());
        let before = (self.lyric_w * 1000.0) as i32;
        self.lyric_w = value;
        self.sync_lyric_alpha();
        // 按物理像素取整比较：0.5px 以内的变化重排没有意义，只会抖
        let after = (self.lyric_w * 1000.0) as i32;
        if done {
            self.lyric_anim = None;
            self.lyric_w = anim.to;
            self.sync_lyric_alpha();
        }
        before != after
    }

    /// 由当前动画宽度推导歌词行不透明度（宽度展开越多，歌词越实）。
    fn sync_lyric_alpha(&mut self) {
        self.state.lyric_alpha = (self.lyric_w / crate::config::defaults::limits::LYRIC_W)
            .clamp(0.0, 1.0);
    }

    /// 推导本帧的跑马灯状态（v1.1.4：行时间窗口驱动 + 演唱结束前提前滚完）。
    ///
    /// 在每次 `redraw()` 开头调用——frame_hash 会把偏移量化进去，偏移不变
    /// （暂停 / 长窗口慢滚不足 0.5px）自动被去重跳过。
    ///
    /// 时间基准与扫光完全一致（`lyric_time()` 播放位置外推）：暂停冻结、
    /// 拖动跳转跟随、行切换天然从头开始——无需 v1.1.2 的行指纹归零机制。
    fn sync_lyric_scroll(&mut self) {
        let t = self.state.lyric_time();
        let d = self.state.lyrics.draw(t, self.lang);
        let viewport_w = self.state.layout.lyric_cur.w;
        let lyric_on = self.state.layout.form == WidgetForm::Lyric;
        // 歌词行自然宽来自渲染器缓存（上一帧建的 TextLayout——宽度只在换行时变，够用）
        let text_w = self.renderer.as_ref().and_then(|r| r.lyric_text_width());
        // 行内已播时长（行开始前钳 0）与滚动窗口（= 演唱窗口 − 提前量，见下）
        let elapsed_ms = (t - d.line_start_ms).max(0) as u32;
        let sing_window_ms = (d.line_sing_end_ms - d.line_start_ms).max(0);
        let window_ms = marquee_scroll_window_ms(sing_window_ms);
        let (marquee, offset) = marquee_frame(MarqueeInput {
            enabled: self.state.lyric_scroll_enabled,
            placeholder: d.placeholder,
            cur_empty: d.cur.is_empty(),
            lyric_on,
            text_w,
            viewport_w,
            elapsed_ms,
            window_ms,
        });
        self.state.lyric_marquee = marquee;
        self.state.lyric_scroll_offset = offset;
    }

    /// 让歌词区宽度平滑过渡到配置对应的目标值（v1.1：宽度而非高度）。
    fn animate_lyric_width(&mut self) {
        let target = lyric_w_logical(&self.config);
        if (target - self.lyric_w).abs() < 0.01 {
            self.lyric_anim = None;
            return;
        }
        crate::log_debug!(
            "歌词区宽度过渡：{:.0}px → {:.0}px（{}ms）",
            self.lyric_w,
            target,
            LYRIC_ANIM_MS as u32
        );
        self.lyric_anim = Some(WidthAnim {
            from: self.lyric_w,
            to: target,
            start: Instant::now(),
        });
        if !self.anim_timer_on {
            self.start_widget_timer(TIMER_ANIM, crate::config::defaults::limits::ANIM_TIMER_MS);
        }
    }

    /// 歌词推进定时器（40ms ≈ 25fps）的开关判定。
    ///
    /// 只在「小组件可见 + 歌词就绪 + 正在播放」时存在。暂停时立刻停掉：
    /// 扫光本来就该停住（PRD P0-09），继续 25fps 重绘纯属浪费 CPU。
    fn sync_lyric_timer(&mut self) {
        let want = self.state.visible
            && self.config.lyrics.enabled
            && self.state.lyrics.status == LyricStatus::Ready
            && self.state.effective_state() == PlaybackState::Playing;
        if want {
            if !self.lyric_timer_on {
                self.start_widget_timer(TIMER_LYRIC, widget_wnd::TIMER_LYRIC_MS);
            }
        } else if self.lyric_timer_on {
            self.kill_widget_timer(TIMER_LYRIC);
            self.last_lyric_key = None;
        }
    }

    /// 25fps 歌词推进：只有「行号或扫光位置真的变了」才重绘。
    ///
    /// 扫光位置量化到 1/4 码元（约 3px @13px 字号）再比较——比这更细的变化
    /// 人眼看不出来，却会让每一帧都重绘。
    fn on_lyric_tick(&mut self) {
        // v1.1.3：跑马灯偏移直接由播放位置推导（sync_lyric_scroll），tick 只负责
        // 25fps 重绘节拍；暂停时定时器已停，偏移自然冻结，与扫光一致。
        let prev_offset = self.state.lyric_scroll_offset;
        self.sync_lyric_scroll();
        // 偏移按 0.5px 量化比较：窗口驱动下长窗口每帧位移不足 0.5px 时自动去重，
        // 不产生无谓重绘
        let offset_moved = (self.state.lyric_scroll_offset * 2.0) as i32
            != (prev_offset * 2.0) as i32;
        // 先把要比较的量拷出来，结束对 `self.state` 的不可变借用
        let (key, preview) = {
            let draw = self.state.lyric_draw(self.lang);
            let key = (draw.line, (draw.sweep * 4.0) as i32);
            let cut = draw.cur.char_indices().nth(24).map(|(i, _)| i).unwrap_or(draw.cur.len());
            (key, draw.cur[..cut].to_string())
        };
        if self.last_lyric_key == Some(key) && !offset_moved {
            return;
        }
        let line_changed = self.last_lyric_key.map(|k| k.0) != Some(key.0);
        self.last_lyric_key = Some(key);
        self.state.lyrics.line_idx = key.0;
        if line_changed {
            crate::log_debug!("歌词行切换 → {:?}：{preview}", key.0);
        }
        self.redraw();
    }

    /// 组装一次歌词请求。
    fn lyric_request(&self, retry: bool) -> LyricRequest {
        LyricRequest {
            track_id: self.state.playing.track_id,
            track: self.state.playing.track.clone().unwrap_or_default(),
            local_dirs: self.config.lyrics.local_dirs.clone(),
            online_enabled: self.config.lyrics.online_enabled,
            retry,
        }
    }

    // ---------------------------------------------------------------- reduce / apply

    /// 纯函数：事件 → 副作用列表。
    pub fn reduce(&mut self, evt: AppEvent) -> Vec<Effect> {
        match evt {
            AppEvent::NowPlaying(np) => {
                let track_changed = self.state.playing.track_id != np.track_id;
                if track_changed && self.state.lyrics.status == LyricStatus::Ready {
                    // 旧歌词转场 150ms（120ms 淡出 + 上移，M3 遗留 P2），避免"啪"一下消失
                    let draw = self.state.lyric_draw(self.lang);
                    if !draw.cur.is_empty() {
                        self.state.lyric_fade = Some((draw.cur.to_string(), Instant::now()));
                    }
                }
                self.state.playing = *np;
                self.state.optimistic = None;
                if track_changed {
                    // PRD §5.3：切歌强制退出 Seeking / VolumeHUD
                    self.interact.reset();
                    self.sync_interaction();
                }
                // v1.5.5 / G3.5：解冻后的第一份 NowPlaying 不直接 Relayout——
                // 交给 apply 走 `finish_resume`（先刷隐藏窗口的内容，再显示），
                // 否则用户退出全屏瞬间会看到冻结前的旧帧。
                let mut effects = if self.state.resume_pending {
                    vec![Effect::ResumeFromFreeze]
                } else {
                    vec![Effect::Relayout]
                };
                if track_changed {
                    // 换歌 → 立刻发歌词请求（旧歌词由 `LyricLoading` 覆盖，不残留）
                    effects.push(Effect::Lyric(self.lyric_request(false)));
                }
                effects
            }
            AppEvent::SessionCleared => {
                crate::log_info!("会话已清空，小组件隐藏");
                self.state.playing = NowPlaying::default();
                self.state.lyrics = LyricView::default();
                self.interact.reset();
                self.sync_interaction();
                vec![Effect::Relayout]
            }
            AppEvent::BackdropReady { epoch, theme } => {
                // 代次不匹配 → 丢弃（尺寸 / 位置已变，采样对不上）
                if epoch != self.state.geometry_epoch {
                    crate::log_debug!(
                        "丢弃过期背景（epoch {epoch} ≠ {}）",
                        self.state.geometry_epoch
                    );
                    return Vec::new();
                }
                crate::log_debug!(
                    "主题更新：luma={:.3} on_light={} contrast={:.2}",
                    theme.backdrop_luma,
                    theme.on_light,
                    theme.text_contrast()
                );
                // 只有主题**真的变了**才重绘。
                //
                // 静止态心跳会周期性重新采样（`limits::BACKDROP_IDLE_MS` = 3s，
                // 见 `on_timer`），背景静止时结果完全一样；无脑重绘等于每 3 秒白跑
                // 一帧 D2D + WIC 回读 + UpdateLayeredWindow，空闲 CPU 会从 ~0 涨到
                // 0.5%（PRD P0-14 的硬指标就是 0.5%）。
                let changed = !theme.same_paint(&self.state.theme);
                self.state.theme = theme;
                if changed { vec![Effect::Redraw] } else { Vec::new() }
            }
            AppEvent::CommandResult { kind, ok } => {
                if !ok {
                    crate::log_info!("命令 {kind:?} 未被播放器接受，回滚乐观更新");
                    self.state.optimistic = None;
                    return vec![Effect::Redraw];
                }
                Vec::new()
            }
            AppEvent::WidgetInput(input) => self.reduce_input(input),
            AppEvent::SettingsOpenRequest => {
                vec![Effect::OpenSettings]
            }
            AppEvent::SettingsClosed => {
                crate::log_info!("设置页已关闭");
                Vec::new()
            }
            AppEvent::SettingsWebViewReady => {
                self.settings.on_webview_ready();
                Vec::new()
            }
            AppEvent::SettingsWebViewFailed(reason) => {
                crate::log_err!("设置页不可用：{reason}");
                self.show_webview_missing();
                Vec::new()
            }
            AppEvent::SettingsMessage(msg) => self.reduce_settings_message(&msg),
            AppEvent::Hotkey(action) => self.reduce_hotkey(action),
            AppEvent::TrayCommand(cmd) => match cmd {
                TrayCommand::OpenSettings => {
                    self.on_second_instance();
                    Vec::new()
                }
                TrayCommand::ToggleWidget => self.toggle_widget_visible(),
                TrayCommand::Quit => {
                    crate::log_info!("托盘：退出");
                    vec![Effect::Quit]
                }
            },
            // ---------------- 歌词（T2 → T0） ----------------
            AppEvent::LyricLoading { track_id } => {
                // 代次不符 = 用户已经切走了，丢弃（避免旧歌词覆盖新歌）
                if track_id != self.state.playing.track_id {
                    crate::log_debug!("丢弃过期 LyricLoading（track {track_id}）");
                    return Vec::new();
                }
                self.state.lyrics.status = LyricStatus::Loading;
                self.state.lyrics.track_id = track_id;
                self.state.lyrics.lyrics = None;
                self.state.lyrics.line_idx = None;
                vec![Effect::Redraw]
            }
            AppEvent::LyricReady { track_id, lyrics } => {
                if track_id != self.state.playing.track_id {
                    crate::log_debug!("丢弃过期 LyricReady（track {track_id}）");
                    return Vec::new();
                }
                crate::log_info!(
                    "歌词就绪：{} 行，kind={:?}",
                    lyrics.lines.len(),
                    lyrics.kind
                );
                self.state.lyrics.status = LyricStatus::Ready;
                self.state.lyrics.track_id = track_id;
                self.state.lyrics.lyrics = Some(lyrics);
                self.state.lyrics.retry = 0;
                self.state.lyrics.line_idx = None;
                self.sync_lyric_timer();
                vec![Effect::Redraw]
            }
            AppEvent::LyricMissing { track_id, reason } => {
                if track_id != self.state.playing.track_id {
                    return Vec::new();
                }
                crate::log_info!("歌词缺失：{reason:?}");
                self.state.lyrics.status = match reason {
                    crate::lyric::LyricMissReason::NetworkError => LyricStatus::NetworkError,
                    _ => LyricStatus::NotFound,
                };
                self.state.lyrics.track_id = track_id;
                self.state.lyrics.lyrics = None;
                self.state.lyrics.line_idx = None;
                self.sync_lyric_timer();
                vec![Effect::Redraw]
            }
            AppEvent::LyricRetryDue { track_id } => {
                if track_id != self.state.playing.track_id {
                    return Vec::new();
                }
                vec![Effect::Lyric(self.lyric_request(true))]
            }
            AppEvent::TaskbarGeometry(g) => {
                self.geometry = Some(g);
                vec![Effect::Relayout]
            }
            AppEvent::TaskbarCreated => {
                self.on_taskbar_created();
                Vec::new()
            }
            AppEvent::DisplayChanged => {
                self.on_display_changed();
                Vec::new()
            }
            AppEvent::SecondInstanceLaunched => {
                self.on_second_instance();
                Vec::new()
            }
            AppEvent::ConfigChanged(cfg) => {
                crate::log_info!("配置变更（version={}）", cfg.version);
                self.lang = Lang::from_serde(cfg.general.language);
                self.config = *cfg;
                self.animate_lyric_width();
                self.relayout_with(RectChangeReason::Resize);
                let json = self.settings_state_json();
                self.settings.push(&json);
                vec![Effect::RefreshTrayMenu]
            }
            AppEvent::Tick => {
                self.tick_optimistic();
                Vec::new()
            }
            AppEvent::Quit => vec![Effect::Quit],

            // ---- M3–M5 的接口预留：记录后忽略，绝不 panic ----
            other => {
                crate::log_debug!("reduce: 暂未处理的 AppEvent（M2 范围外）：{other:?}");
                Vec::new()
            }
        }
    }

    /// 设置页 JS 消息。
    fn reduce_settings_message(&mut self, msg: &str) -> Vec<Effect> {
        use crate::settings::bridge::{self, Incoming};
        let Some(inc) = bridge::parse(msg) else {
            crate::log_debug!("设置页消息无法解析：{}", &msg[..msg.len().min(80)]);
            return Vec::new();
        };
        match inc {
            Incoming::Record(start) => {
                // 录制期间挂起全局热键（PRD P1-02），否则按旧键会直接触发动作
                let cfg = self.config.hotkeys;
                if start {
                    self.hotkeys.suspend(self.msg_hwnd);
                } else {
                    self.hotkeys.resume(self.msg_hwnd, &cfg);
                }
                Vec::new()
            }
            Incoming::Action(a) => {
                self.run_settings_action(&a);
                Vec::new()
            }
            Incoming::Set { path, value } => {
                if !bridge::apply_set(&mut self.config, &path, &value) {
                    return Vec::new();
                }
                // 歌词开关要走宽度过渡；快捷键变化要重绑
                let hotkey_changed = path.starts_with("hotkeys.");
                if path == "lyrics.enabled" {
                    self.animate_lyric_width();
                }
                // 外观（背景模式/透明度）变了 → 立即重采样拿新主题 + 重绘，
                // 不能等兜底心跳（3s；v1.1.12：idle_mode/hover_alpha/idle_alpha 同样走这里）
                let appearance_changed = path.starts_with("appearance.");
                let mut effects = vec![
                    Effect::Relayout,
                    Effect::SaveConfig(Box::new(self.config.clone())),
                ];
                if hotkey_changed {
                    effects.push(Effect::RebindHotkeys);
                }
                if appearance_changed {
                    // 就地同步两态背景色：滑条 / 模式改动即时可见；
                    // 随后采样回来的主题按同一配置重算，same_paint 相等不闪
                    let a = &self.config.appearance;
                    self.state.theme.apply_bg_config(
                        a.background_mode,
                        a.idle_mode,
                        a.hover_alpha,
                        a.idle_alpha,
                    );
                    self.request_backdrop();
                    effects.push(Effect::Redraw);
                }
                // 改项成功后立刻把全量 state 回推给页面（M4 QA 回归缺陷）：
                // 语言切换等改动要当窗生效，不能等重开。push 同时更新 last_state，
                // 窗口重建后 on_webview_ready 也能补推到最新值。
                let json = self.settings_state_json();
                self.settings.push(&json);
                effects
            }
        }
    }

    /// 设置页里的「动作」按钮。
    fn run_settings_action(&mut self, action: &str) {
        // 歌词目录逐行「更改…」：browseLyricsDir:<idx>
        if let Some(idx) = action
            .strip_prefix("browseLyricsDir:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            self.relocate_lyrics_dir(idx);
            return;
        }
        // 歌词目录路径链接：openLyricsDir:<idx> → 资源管理器打开该目录
        if let Some(idx) = action
            .strip_prefix("openLyricsDir:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if let Some(dir) = self.config.lyrics.local_dirs.get(idx) {
                let w = to_wide(dir);
                // SAFETY: w 以 NUL 结尾且在本调用期间存活。
                unsafe {
                    windows::Win32::UI::Shell::ShellExecuteW(
                        None,
                        windows::core::w!("open"),
                        windows::core::PCWSTR(w.as_ptr()),
                        windows::core::PCWSTR::null(),
                        windows::core::PCWSTR::null(),
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                    );
                }
            }
            return;
        }
        match action {
            "openLogDir" => {
                let dir = crate::platform::data_dir();
                let w = to_wide(&dir.to_string_lossy());
                // SAFETY: w 以 NUL 结尾且在本调用期间存活。
                unsafe {
                    windows::Win32::UI::Shell::ShellExecuteW(
                        None,
                        windows::core::w!("open"),
                        windows::core::PCWSTR(w.as_ptr()),
                        windows::core::PCWSTR::null(),
                        windows::core::PCWSTR::null(),
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                    );
                }
                crate::log_info!("设置页：打开数据目录 {}", dir.display());
            }
            // v1.1.5 问题 3 / v1.1.7：选数据目录 → 弹窗问「迁移？删除原文件？」
            // → 选项记进配置，重启后由 storage::init_from_config 执行
            // （WebView2 / 日志句柄占用，运行中搬会损坏，必须延迟到启动）。
            "browseStorageDir" => {
                let parent = self.settings.hwnd().unwrap_or(self.msg_hwnd);
                let (caption, pick_t, q_migrate, q_delete, done, same) = match self.lang {
                    Lang::Zh => (
                        "数据目录",
                        "选择数据目录",
                        "是否把原目录中的文件同步迁移到新目录？\n（选「否」则新目录从空开始）",
                        "是否删除原目录整个文件夹？\n（删干净；config.json 等配置文件永远保留）",
                        "已保存数据目录，重启曲栏后生效",
                        "数据目录未变化",
                    ),
                    Lang::En => (
                        "Data folder",
                        "Choose data folder",
                        "Migrate files from the old folder to the new one?\n(No = start empty)",
                        "Delete the old folder entirely?\n(config.json is always kept)",
                        "Data folder saved. Restart Barline to apply",
                        "Data folder unchanged",
                    ),
                };
                match crate::platform::pick_folder(parent, pick_t) {
                    Some(p) => {
                        let v = serde_json::json!(p.to_string_lossy().to_string());
                        if crate::settings::bridge::apply_set(
                            &mut self.config,
                            "storage.data_dir",
                            &v,
                        ) {
                            let (migrate, delete) =
                                self.ask_migrate_delete(caption, q_migrate, q_delete);
                            self.config.storage.pending_migrate = migrate;
                            self.config.storage.pending_delete_old = delete;
                            if let Err(e) = self.config.save() {
                                crate::log_err!("保存数据目录配置失败：{e}");
                            }
                            let toast = crate::settings::bridge::toast_json("info", done);
                            self.settings.push(&toast);
                            let state = self.settings_state_json();
                            self.settings.push(&state);
                            crate::log_info!(
                                "数据目录已改为 {}（迁移={migrate} 删除原文件={delete}）",
                                p.display()
                            );
                        } else {
                            let toast = crate::settings::bridge::toast_json("info", same);
                            self.settings.push(&toast);
                        }
                    }
                    None => crate::log_debug!("设置页：取消选择数据目录"),
                }
            }
            // 恢复默认 = 把目录改回默认根，走同一套弹窗（迁移回默认 / 删除旧自定义目录）
            "resetStorageDir" => {
                if self.config.storage.data_dir.is_empty() {
                    let t = match self.lang {
                        Lang::Zh => "数据目录未变化",
                        Lang::En => "Data folder unchanged",
                    };
                    let toast = crate::settings::bridge::toast_json("info", t);
                    self.settings.push(&toast);
                    return;
                }
                let (caption, q_migrate, q_delete, done) = match self.lang {
                    Lang::Zh => (
                        "数据目录",
                        "是否把原目录中的文件同步迁移回默认目录？\n（选「否」则默认目录从空开始）",
                        "是否删除原目录整个文件夹？\n（删干净；config.json 等配置文件永远保留）",
                        "已恢复默认数据目录，重启曲栏后生效",
                    ),
                    Lang::En => (
                        "Data folder",
                        "Migrate files back to the default folder?\n(No = default starts empty)",
                        "Delete the old folder entirely?\n(config.json is always kept)",
                        "Data folder reset. Restart Barline to apply",
                    ),
                };
                let (migrate, delete) = self.ask_migrate_delete(caption, q_migrate, q_delete);
                self.config.storage.data_dir = String::new();
                self.config.storage.pending_migrate = migrate;
                self.config.storage.pending_delete_old = delete;
                if let Err(e) = self.config.save() {
                    crate::log_err!("保存数据目录配置失败：{e}");
                }
                let toast = crate::settings::bridge::toast_json("info", done);
                self.settings.push(&toast);
                let state = self.settings_state_json();
                self.settings.push(&state);
            }
            // v1.1.7 问题 4：「添加」改为系统目录选择对话框（新增目录无迁移语义）
            "addLyricsDirBrowse" => {
                let parent = self.settings.hwnd().unwrap_or(self.msg_hwnd);
                let (pick_t, dup) = match self.lang {
                    Lang::Zh => ("选择歌词目录", "该目录已在列表中"),
                    Lang::En => ("Choose lyrics folder", "Folder is already in the list"),
                };
                match crate::platform::pick_folder(parent, pick_t) {
                    Some(p) => {
                        let s = p.to_string_lossy().to_string();
                        if self.config.lyrics.local_dirs.iter().any(|d| d == &s) {
                            let toast = crate::settings::bridge::toast_json("info", dup);
                            self.settings.push(&toast);
                        } else {
                            self.config.lyrics.local_dirs.push(s);
                            if let Err(e) = self.config.save() {
                                crate::log_err!("保存歌词目录失败：{e}");
                            }
                            let state = self.settings_state_json();
                            self.settings.push(&state);
                        }
                    }
                    None => crate::log_debug!("设置页：取消选择歌词目录"),
                }
            }
            "openLrclib" => {
                let w = to_wide("https://lrclib.net");
                // SAFETY: 同上。
                unsafe {
                    windows::Win32::UI::Shell::ShellExecuteW(
                        None,
                        windows::core::w!("open"),
                        windows::core::PCWSTR(w.as_ptr()),
                        windows::core::PCWSTR::null(),
                        windows::core::PCWSTR::null(),
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                    );
                }
            }
            // v1.3.0（G6）：歌词来源现在有两个，「关于」页各给一个链接
            "openNetease" => {
                let w = to_wide("https://music.163.com");
                // SAFETY: 同上。
                unsafe {
                    windows::Win32::UI::Shell::ShellExecuteW(
                        None,
                        windows::core::w!("open"),
                        windows::core::PCWSTR(w.as_ptr()),
                        windows::core::PCWSTR::null(),
                        windows::core::PCWSTR::null(),
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                    );
                }
            }
            "ready" => {
                // 页面加载完了：补推一次全量状态
                self.settings.on_webview_ready();
            }
            other => crate::log_debug!("设置页未知动作：{other}"),
        }
    }

    /// 「迁移？删除原文件？」两个确认弹窗。返回 `(迁移, 删除)`——用户各自决定。
    fn ask_migrate_delete(&self, caption: &str, q_migrate: &str, q_delete: &str) -> (bool, bool) {
        let parent = self.settings.hwnd().unwrap_or(self.msg_hwnd);
        let migrate = crate::platform::confirm_dialog(parent, caption, q_migrate);
        let delete = crate::platform::confirm_dialog(parent, caption, q_delete);
        (migrate, delete)
    }

    /// 歌词目录逐行「更改…」：选新目录 → 弹窗问「迁移 .lrc？删除原 .lrc？」
    /// → **立即执行**（用户自己的文件，无句柄占用）→ 更新配置项。
    fn relocate_lyrics_dir(&mut self, idx: usize) {
        let Some(old) = self.config.lyrics.local_dirs.get(idx).cloned() else {
            return;
        };
        let parent = self.settings.hwnd().unwrap_or(self.msg_hwnd);
        let (pick_t, dup, q_migrate, q_delete, done) = match self.lang {
            Lang::Zh => (
                "更改歌词目录",
                "新目录已在列表中",
                "是否把原目录中的 .lrc 歌词文件迁移到新目录？\n（只移动 .lrc，其他文件不动）",
                "是否删除原目录整个文件夹？\n（含未迁移的其他文件，删干净）",
                "歌词目录已更改",
            ),
            Lang::En => (
                "Change lyrics folder",
                "Folder is already in the list",
                "Move .lrc files from the old folder to the new one?\n(Only .lrc files are touched)",
                "Delete the old folder entirely?\n(including any non-migrated files)",
                "Lyrics folder changed",
            ),
        };
        match crate::platform::pick_folder(parent, pick_t) {
            Some(p) => {
                let new = p.to_string_lossy().to_string();
                if new == old
                    || self
                        .config
                        .lyrics
                        .local_dirs
                        .iter()
                        .any(|d| d == &new)
                {
                    let toast = crate::settings::bridge::toast_json("info", dup);
                    self.settings.push(&toast);
                    return;
                }
                let (migrate, delete) = self.ask_migrate_delete(pick_t, q_migrate, q_delete);
                let old_path = std::path::PathBuf::from(&old);
                if migrate {
                    match crate::storage::move_lrc_files(&old_path, &p) {
                        Ok(n) if n > 0 => crate::log_info!(
                            "歌词目录迁移：{} → {}（{n} 个 .lrc）",
                            old,
                            new
                        ),
                        Ok(_) => crate::log_info!("歌词目录迁移：原目录没有 .lrc 文件"),
                        Err(e) => crate::log_err!("歌词目录迁移失败：{e}"),
                    }
                }
                if delete {
                    // 用户明确选择：原目录整个文件夹删干净
                    match std::fs::remove_dir_all(&old_path) {
                        Ok(()) => crate::log_info!("歌词原目录已删除：{old}"),
                        Err(e) => crate::log_err!("歌词原目录删除失败：{e}"),
                    }
                }
                self.config.lyrics.local_dirs[idx] = new;
                if let Err(e) = self.config.save() {
                    crate::log_err!("保存歌词目录失败：{e}");
                }
                let toast = crate::settings::bridge::toast_json("info", done);
                self.settings.push(&toast);
                let state = self.settings_state_json();
                self.settings.push(&state);
            }
            None => crate::log_debug!("设置页：取消更改歌词目录"),
        }
    }

    /// 组装给设置页的全量状态 JSON。
    fn settings_state_json(&self) -> String {
        let conflicts: Vec<(&str, u32)> = self
            .hotkeys
            .conflicts
            .iter()
            .map(|c| (crate::settings::bridge::action_name(c.action), c.code))
            .collect();
        crate::settings::bridge::state_json(&self.config, env!("CARGO_PKG_VERSION"), &conflicts)
    }

    /// 设置窗口尺寸变化 → 同步 WebView2。
    pub fn on_settings_resize(&mut self, hwnd: HWND) {
        self.settings.resize(hwnd);
    }

    /// 设置窗口收到 WM_CLOSE：销毁窗口（释放 WebView2），进程继续跑。
    pub fn on_settings_close_request(&mut self) {
        self.settings.close();
    }

    /// 设置窗口 WM_DESTROY：兜底释放。
    pub fn on_settings_destroyed(&mut self) {
        self.settings.close();
        let effects = self.reduce(AppEvent::SettingsClosed);
        self.apply(effects);
    }

    /// WebView2 不可用时的原生兜底提示（不静默失败）。
    ///
    /// 顺带把 `webview::runtime_version` / `open_runtime_download` 两个兜底工具
    /// 接上（QA 复核发现它们原本是无调用点的死代码）：版本号进日志便于定位
    /// 「装了但创建失败」的情况；对话框问一句「是否打开下载页」。
    fn show_webview_missing(&self) {
        match crate::settings::webview::runtime_version() {
            Some(v) => crate::log_info!("WebView2 运行时存在（{v}）但创建失败，可能被策略拦截"),
            None => crate::log_warn!("未检测到 WebView2 运行时"),
        }
        let title = to_wide(i18n::t(self.lang, "settings.webview_missing_title"));
        let body = to_wide(i18n::t(self.lang, "settings.webview_missing_body"));
        // SAFETY: 两个缓冲都是 NUL 结尾且在本调用期间存活。
        let choice = unsafe {
            MessageBoxW(
                None,
                windows::core::PCWSTR(body.as_ptr()),
                windows::core::PCWSTR(title.as_ptr()),
                MB_ICONWARNING | MB_SETFOREGROUND | MB_YESNO,
            )
        };
        if choice == IDYES {
            crate::settings::webview::open_runtime_download();
        }
    }

    /// 全局快捷键 → 与小组件按钮完全相同的语义（复用 `reduce_input`，避免两套逻辑）。
    fn reduce_hotkey(&mut self, action: crate::event::HotkeyAction) -> Vec<Effect> {
        use crate::event::HotkeyAction as A;
        match action {
            A::PlayPause => self.reduce_input(WidgetInput::ClickPlayPause),
            A::Prev => self.reduce_input(WidgetInput::ClickPrev),
            A::Next => self.reduce_input(WidgetInput::ClickNext),
            A::ToggleLyrics => self.reduce_input(WidgetInput::ToggleLyricsFromMenu),
            A::ToggleWidget => {
                crate::log_info!("热键：显隐小组件");
                self.toggle_widget_visible()
            }
            A::Mute => {
                // 静音走 WASAPI：SMTC 的 Mute 只有部分播放器实现
                let muted = crate::media::wasapi::toggle_system_mute();
                crate::log_info!("热键：静音切换 → {muted}");
                Vec::new()
            }
        }
    }

    /// `WidgetInput` 的 15 个分支。
    fn reduce_input(&mut self, input: WidgetInput) -> Vec<Effect> {
        match input {
            WidgetInput::HoverEnter => {
                self.request_backdrop();
                vec![Effect::Redraw]
            }
            WidgetInput::HoverLeave => vec![Effect::Redraw],
            WidgetInput::ClickPlayPause => {
                let playing = self.state.effective_state() == PlaybackState::Playing;
                let kind = if playing {
                    OptimisticKind::Paused
                } else {
                    OptimisticKind::Playing
                };
                self.set_optimistic(kind);
                vec![Effect::Media(MediaCommand::PlayPause), Effect::Redraw]
            }
            WidgetInput::ClickPrev => {
                self.set_optimistic(OptimisticKind::PrevTrack);
                vec![Effect::Media(MediaCommand::Prev), Effect::Redraw]
            }
            WidgetInput::ClickNext => {
                self.set_optimistic(OptimisticKind::NextTrack);
                vec![Effect::Media(MediaCommand::Next), Effect::Redraw]
            }
            WidgetInput::ClickCover => {
                let activation = media::launch::activate_player(&self.state.playing.session);
                crate::log_info!("点击封面 → 激活结果={}", activation.as_str());
                Vec::new()
            }
            WidgetInput::ClickDismiss => {
                // v1.5.0（G3）：右上角 × = 隐藏到托盘，**仅本次运行生效**。
                // 关键：只置运行期标记，**不碰配置、不产生 SaveConfig**——
                // 「下次启动照常显示」全靠这一点（写盘了就变成永久隐藏）。
                self.state.dismissed = true;
                self.sync_visibility();
                crate::log_info!("× 关闭：隐藏到托盘（仅本次运行）");
                vec![Effect::Redraw]
            }
            WidgetInput::SeekBegin => {
                self.state.seek_preview_ms = self.state.playing.position_now();
                vec![Effect::Redraw]
            }
            WidgetInput::SeekPreview(frac) => {
                let dur = self.state.playing.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
                self.state.seek_preview_ms = (frac as f64 * dur as f64).round() as Ms;
                vec![Effect::Redraw]
            }
            WidgetInput::SeekCommit(frac) => {
                let dur = self.state.playing.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
                let target = (frac as f64 * dur as f64).round() as Ms;
                self.state.seek_preview_ms = target;
                if dur <= 0 {
                    crate::log_debug!("无总时长，忽略跳转请求");
                    return vec![Effect::Redraw];
                }
                crate::log_info!("跳转 → {target}ms（{:.0}%）", frac * 100.0);
                vec![Effect::Media(MediaCommand::Seek(target)), Effect::Redraw]
            }
            WidgetInput::SeekCancel => vec![Effect::Redraw],
            WidgetInput::Wheel { delta } => self.on_wheel(delta),
            WidgetInput::VolumeHudExpired => vec![Effect::Redraw],
            WidgetInput::ToggleLyricsFromMenu => {
                self.config.lyrics.enabled = !self.config.lyrics.enabled;
                self.animate_lyric_width();
                self.commit_config()
            }
            WidgetInput::SetBackgroundMode(mode) => {
                self.config.appearance.background_mode = mode;
                // 就地同步主题（悬停底色即时切换）+ 立即重采样拿新亮度主题
                self.state.theme.apply_bg_config(
                    mode,
                    self.config.appearance.idle_mode,
                    self.config.appearance.hover_alpha,
                    self.config.appearance.idle_alpha,
                );
                self.request_backdrop();
                let mut effects = self.commit_config();
                effects.push(Effect::Redraw);
                effects
            }
            WidgetInput::ShowAbout => {
                self.show_about();
                Vec::new()
            }
        }
    }

    /// 滚轮：音量 ±1 格 + HUD。
    ///
    /// v1.1.15（问题 6）：范围「当前播放器」走 `ISimpleAudioVolume` 会话音量，
    /// **不再碰系统主音量**；找不到匹配会话（播放器没出声 / 不认识）时不动作。
    fn on_wheel(&mut self, delta: i32) -> Vec<Effect> {
        if !self.config.volume.wheel_enabled {
            return Vec::new();
        }
        // 桥接层写入口径 clamp(0.01, 0.2)：这里必须同口径，否则设置页选的
        // 10% 会被静默钳成 5%（v1.1.10 修复；此前 0.01..=0.05 是笔误）。
        let step = self.config.volume.step.clamp(0.01, 0.2);
        let dir = if delta > 0 { 1.0 } else { -1.0 };

        // v1.1.17（用户裁决）：会话音量暂禁用——config 加载与桥接层都已把
        // scope 归一化为 System，此分支理论上不可达；实现保留（含 v1.1.16 的
        // GUID/匹配修复与限流诊断），待根因查明后随「当前播放器」范围一并恢复。
        let current_player = self.config.volume.scope
            == crate::config::VolumeScope::CurrentPlayer;
        let next = if current_player {
            let s = self.state.playing.session.clone();
            match crate::media::wasapi::step_session_volume(&s.process_name, &s.aumid, dir * step)
            {
                Some(v) => Some(v),
                None => {
                    // HUD 已在 interaction 层弹出（on_wheel 入口先置可见），
                    // 会话不可用时必须撤掉，否则会拿上一次（往往是系统音量）
                    // 的旧值一直显示（v1.1.16 修复）。
                    self.interact.cancel_volume_hud();
                    return Vec::new();
                }
            }
        } else {
            let base = media::wasapi::system_volume().unwrap_or(self.state.volume_hud);
            let next = (base + dir * step).clamp(0.0, 1.0);
            let ok = media::wasapi::set_system_volume(next);
            if !ok {
                return Vec::new();
            }
            Some(next)
        };

        if let Some(next) = next {
            self.state.volume_hud = next;
            crate::log_debug!(
                "滚轮 delta={delta} → {}音量 {:.0}%",
                if current_player { "会话" } else { "系统" },
                next * 100.0
            );
        }
        vec![Effect::Redraw]
    }

    /// 执行副作用。
    pub fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Relayout => self.relayout_with(RectChangeReason::Resize),
                Effect::Redraw => self.redraw(),
                // v1.5.5 / G3.5：解冻后第一份 NowPlaying 已写进 state，
                // 此时才刷新隐藏窗口并恢复显示（不闪旧内容）。
                Effect::ResumeFromFreeze => self.finish_resume("NowPlaying"),
                Effect::SetVisible(v) => {
                    if let Some(h) = self.widget.as_mut() {
                        h.set_visible(v);
                    }
                }
                Effect::Media(cmd) => match self.media.as_ref() {
                    Some(m) => m.send(cmd),
                    None => crate::log_warn!("媒体线程未启动，命令 {cmd:?} 丢弃"),
                },
                Effect::OpenSettings => {
                    let json = self.settings_state_json();
                    // 标题栏明暗必须与页面内容同源（QA-M4 缺陷 #3）：页面 CSS 跟随
                    // 系统 prefers-color-scheme，所以这里读系统设置而不是小组件的
                    // 壁纸采样结果（浅色系统 + 深壁纸时两者会不一致）。
                    let dark = crate::platform::system_app_theme_dark();
                    self.settings.open(
                        self.hinstance,
                        self.sink.clone(),
                        json,
                        dark,
                    );
                    // 设置窗口刚建好，把 App* 绑上去（WndProc 要用）
                    if let Some(h) = self.settings.hwnd() {
                        crate::settings::bind_app(h, self as *mut App);
                    }
                }
                Effect::CloseSettings => {
                    self.settings.close();
                }
                Effect::RebindHotkeys => {
                    let cfg = self.config.hotkeys;
                    self.hotkeys.register_all(self.msg_hwnd, &cfg);
                }
                Effect::RefreshTrayMenu => {
                    if let Some(t) = self.tray.as_mut() {
                        t.refresh(self.lang);
                    }
                }
                Effect::Lyric(req) => match self.lyric.as_ref() {
                    Some(l) => l.send(req),
                    None => crate::log_warn!("歌词线程未启动，请求被丢弃"),
                },
                Effect::SaveConfig(cfg) => {
                    if let Err(e) = cfg.save() {
                        crate::log_err!("保存配置失败：{e}");
                    }
                }
                Effect::RequestBackdrop { .. } => self.request_backdrop(),
                Effect::Quit => {
                    crate::log_info!("收到 Quit，准备退出");
                    // SAFETY: PostQuitMessage 只投递一条 WM_QUIT，可在任意线程调用。
                    unsafe { PostQuitMessage(0) };
                }
                Effect::MoveWindow { x, y, w, h } => {
                    let r = RECT {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    };
                    if let Some(h) = self.widget.as_mut() {
                        if let Err(e) = h.apply_geometry(r) {
                            crate::log_warn!("应用窗口几何失败：{e}");
                        }
                    }
                }
                other => crate::log_debug!("apply: 暂未实现的 Effect（M3+ 范围外）：{other:?}"),
            }
        }
    }

    // ---------------------------------------------------------------- 渲染

    /// 重算布局 → 应用几何 → 重建目标（如需）→ 填文本缓存 → 重绘 → 同步可见性。
    ///
    /// 不知道原因时用它（等价于 [`App::relayout_with`] + [`RectChangeReason::Unknown`]）；
    /// 有明确来源的入口请用 `relayout_with`，否则诊断日志里分不清是谁挪的窗口。
    pub fn relayout(&mut self) {
        self.relayout_with(RectChangeReason::Unknown);
    }

    /// 同 [`App::relayout`]，但显式声明**为什么**要重排（v1.2.0 诊断用）。
    pub fn relayout_with(&mut self, reason: RectChangeReason) {
        let Some(geo) = self.geometry else {
            // 没有几何信息（Explorer 未就绪）→ 隐藏，等下一次探测
            if let Some(h) = self.widget.as_mut() {
                h.set_visible(false);
            }
            return;
        };

        let scale = scale_for(geo.dpi);
        let bar_thickness = (geo.bar_rect.bottom - geo.bar_rect.top).max(1);
        // v1.1.1：行数固定单行，config 里的旧 two_line 字段只读不用于布局
        // 竖排任务栏（左右）在 M4 一律降级为紧凑态：内容旋转（§10.4）尚未实现，
        // 不降级的话两行信息区会沿条带厚度方向展开，直接压到桌面上。
        let form = if geo.edge.is_vertical() {
            WidgetForm::Compact
        } else {
            WidgetForm::from_lyric_enabled(self.config.lyrics.enabled)
        };

        // 文本自然宽度（歌名 / 歌手分开量）→ 决定信息区宽
        let lang = self.lang;
        let title = self
            .state
            .playing
            .title_or(i18n::t(lang, "state.unknown_track"))
            .to_string();
        let artist = self
            .state
            .playing
            .artist_or(i18n::t(lang, "state.unknown_artist"))
            .to_string();

        let (title_w, artist_w) = match self.renderer.as_ref() {
            Some(r) => (
                r.measure(TextKind::Title, &title),
                r.measure(TextKind::Artist, &artist),
            ),
            None => (0.0, 0.0),
        };

        let mut lay =
            layout::compute_full(geo.edge, form, scale, bar_thickness, title_w, artist_w, self.lyric_w);
        if let Some(r) = self.renderer.as_mut() {
            r.prepare_text(&mut lay, &title, &artist);
        }

        let (pw, ph) = if geo.edge.is_vertical() {
            (lay.height, lay.width)
        } else {
            (lay.width, lay.height)
        };

        self.state.layout = lay;
        self.state.geometry_epoch = geo.epoch;
        // v1.1.2：长歌词滚动开关随配置同步
        self.state.lyric_scroll_enabled = self.config.lyrics.scroll;
        self.state.visible = self.state.compute_visible();

        if let Some(r) = self.renderer.as_mut() {
            if let Err(e) = r.ensure(pw, ph, geo.dpi) {
                crate::log_err!("渲染目标尺寸调整失败：{e}");
            }
        }

        self.target_rect = geometry::place_widget(
            geo.tray_rect,
            geo.bar_rect,
            geo.edge,
            scale,
            pw,
            ph,
        );
        // v1.4.4 补丁：记住「小组件在哪块屏」——心跳要不要因前台全屏而跳过，靠它判断。
        // `dpi::monitor_rect` 是 `MonitorFromWindow` + `GetMonitorInfoW`，纯本地查询，
        // 而且这里本来就在重排（几何 / DPI / 显示器变化驱动），不是新增热路径。
        //
        // **必须放在 `apply_geometry` 之后**：窗口搬到另一块屏后（任务栏换屏 / 多显示器
        // 重排），`MonitorFromWindow` 读的是窗口**当前**位置，先取会拿到旧屏。
        // 注意：`relayout_with` 在开头就按 `self.geometry` 提前 return 的路径不会刷新它，
        // 那种情况下 `widget_monitor` 保持旧值/全 0 —— 全 0 时 `same_monitor` 判 false，
        // 也就是「继续采样」，安全方向。
        // v1.2.0 诊断：窗口矩形每次变化都留痕（old / new / reason）。
        // 「截图时左移」只能靠这条日志定位是哪条路径在遮罩期间改了 rect。
        self.log_rect_change(reason);
        if let Some(h) = self.widget.as_mut() {
            if let Err(e) = h.apply_geometry(self.target_rect) {
                crate::log_warn!("应用窗口几何失败：{e}");
            }
        }
        self.widget_monitor = self
            .widget
            .as_ref()
            .and_then(|h| crate::platform::dpi::monitor_rect(h.hwnd()))
            .map(|m| m.monitor)
            .unwrap_or_default();

        crate::log_debug!(
            "布局：{}×{} form={:?} 文本宽={:.0}/{:.0} target=({},{} {}×{})",
            self.state.layout.width,
            self.state.layout.height,
            form,
            title_w,
            artist_w,
            self.target_rect.left,
            self.target_rect.top,
            self.target_rect.right - self.target_rect.left,
            self.target_rect.bottom - self.target_rect.top
        );

        self.redraw();
        self.sync_visibility();
        self.sync_lyric_timer();
        self.request_backdrop();
    }

    /// 只重绘（布局不变）。
    pub fn redraw(&mut self) {
        // v1.5.5 / G3.5：冻结期间任何绘制都跳过——窗口已隐藏，也没有内容变化。
        // 解冻等待期（resume_pending）frozen 已是 false，这里的 redraw 照常生效，
        // 由 finish_resume 在隐藏状态下先把内容刷成最新。
        if self.state.frozen {
            return;
        }
        if self.geometry.is_none() {
            return;
        }
        // ---- 像素指纹去重（架构 §11.1）----
        // 同一份指纹意味着「这一帧画出来和上一帧一模一样」，直接跳过
        // D2D 重绘 + WIC 回读 + UpdateLayeredWindow 三件套。
        // 这是「播放中空闲 CPU」能被压住的关键：进度条每秒才动一次、
        // 扫光在暂停时不动、悬停淡出结束后完全静止，这些情况都不该重绘。
        self.sync_lyric_scroll();
        let hash = self.frame_hash();
        if self.last_frame_hash == Some(hash) {
            self.skipped_frames = self.skipped_frames.saturating_add(1);
            return;
        }
        let Some(host) = self.widget.as_ref() else {
            return;
        };
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let t = self.target_rect;
        // G2 诊断：「D2D 绘制 + 位图上传 + UpdateLayeredWindow」整帧耗时。
        // UpdateLayeredWindow 是**同步**提交给 DWM 的，DWM 忙（播放视频 / 合成抖动）
        // 时这一句会阻塞，所以它和「鼠标同时卡」高度相关。
        let _sp = crate::platform::perf::Span::new("paint.frame");
        match renderer.render(&self.state, self.lang) {
            Ok((hdc, w, h, coverage)) => {
                if let Err(e) = host.present(hdc, t.left, t.top, w, h) {
                    crate::log_warn!("UpdateLayeredWindow 失败：{e}");
                }
                self.last_coverage = coverage;
                self.last_frame_hash = Some(hash);
            }
            Err(e) => crate::log_err!("绘制失败：{e}"),
        }
    }

    /// 本帧的「像素指纹」。
    ///
    /// 只喂**会影响像素**的量，而且全部先量化：
    /// - 进度按 1 秒量化（进度条宽度一秒才动 ~1px）；
    /// - 扫光按 1/60 行量化（13px 字号下 ~0.2px，肉眼不可辨）；
    /// - 悬停 alpha 按 1/32 量化（120ms 淡入在 16ms 定时器下也就 7 帧）；
    /// - 音量 HUD 按 1% 量化。
    ///
    /// 任何"多算了"的量都只会让去重失效（退化成每帧重绘），不会画错；
    /// 反过来漏算就会画错——所以宁可多算。
    fn frame_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        /// `Rgb` 没有 `Hash`，打包成 u32 再喂。
        fn rgb_key(c: crate::media::Rgb) -> u32 {
            ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
        }
        fn hover_key(h: HoverState) -> u8 {
            match h {
                HoverState::Idle => 0,
                HoverState::Hover => 1,
                HoverState::Seeking => 2,
                HoverState::VolumeHud => 3,
            }
        }
        fn target_key(t: HitTarget) -> u8 {
            match t {
                HitTarget::None => 0,
                HitTarget::Cover => 1,
                HitTarget::Prev => 2,
                HitTarget::Toggle => 3,
                HitTarget::Next => 4,
                HitTarget::Progress => 5,
                HitTarget::Dismiss => 6,
            }
        }

        let mut h = std::collections::hash_map::DefaultHasher::new();
        let st = &self.state;
        let lay = &st.layout;

        // ---- 几何 / 布局 ----
        let geo = [
            self.target_rect.left,
            self.target_rect.top,
            self.target_rect.right,
            self.target_rect.bottom,
            lay.width,
            lay.height,
        ];
        geo.hash(&mut h);
        (lay.text_key as i32).hash(&mut h);
        // 歌词区矩形（宽度动画期间每帧都变；v1.1 起是横向块）
        let lyric_rect = [
            lay.lyric_cur.x as i32,
            lay.lyric_cur.w as i32,
            lay.lyric_cur.h as i32,
        ];
        lyric_rect.hash(&mut h);

        // ---- 主题（只取影响像素的字段，忽略 luma 抖动）----
        // v1.2.0：`needs_outline` 已删除（描边整链移除），指纹里不再有它。
        let theme = [
            rgb_key(st.theme.text),
            rgb_key(st.theme.text_dim),
            rgb_key(st.theme.accent),
            st.theme.hover_bg.packed(),
            st.theme.idle_bg.packed(),
            st.theme.track_bg.packed(),
            st.theme.dim_alpha as u32,
        ];
        theme.hash(&mut h);

        // ---- 交互修饰态 ----
        hover_key(st.hover).hash(&mut h);
        ((st.hover_alpha.clamp(0.0, 1.0) * 32.0) as u32).hash(&mut h);
        st.pressed.map(target_key).hash(&mut h);
        // v1.5.0：× 的悬停反馈必须进指纹，否则「悬停 ×」那一帧会被去重吞掉
        // （v1.5.1 起这个反馈是极淡圆角底而不是变亮，结论不变）
        st.hover_target.map(target_key).hash(&mut h);

        // ---- 播放态 ----
        st.playing.track_id.hash(&mut h);
        (st.effective_state() as u8).hash(&mut h);
        (st.playing.can_prev as u8).hash(&mut h);
        (st.playing.can_next as u8).hash(&mut h);
        (st.playing.cover.is_some() as u8).hash(&mut h);
        let duration = st.playing.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        ((duration > 0) as u8).hash(&mut h);
        // 进度按 1 秒量化；拖动中按预览位置
        let pos = if st.hover == HoverState::Seeking {
            st.seek_preview_ms
        } else {
            st.playing.position_now()
        };
        (pos / 1000).hash(&mut h);
        // 拖动气泡里的 mm:ss 也按秒
        if st.hover == HoverState::Seeking {
            (st.seek_preview_ms / 1000).hash(&mut h);
        }

        // ---- 音量 HUD ----
        (st.hover == HoverState::VolumeHud).hash(&mut h);
        ((st.volume_hud.clamp(0.0, 1.0) * 100.0) as u32).hash(&mut h);

        // ---- 歌词 ----
        let draw = st.lyric_draw(self.lang);
        draw.line.hash(&mut h);
        ((draw.sweep * 60.0) as u32).hash(&mut h);
        draw.placeholder.hash(&mut h);
        draw.cur.hash(&mut h);
        // 跑马灯偏移按 0.5px 量化（30px/s @25fps 每帧 1.2px，量化后逐帧进指纹）
        ((st.lyric_scroll_offset * 2.0) as i32).hash(&mut h);
        // 切歌转场：按 1/16 量化（150ms / 16 ≈ 9.4ms，仍在 25fps 步长内）
        if let Some((old_cur, started)) = &st.lyric_fade {
            let t = (started.elapsed().as_secs_f32() / LYRIC_FADE_SECS * 16.0) as u32;
            t.hash(&mut h);
            old_cur.hash(&mut h);
        }

        h.finish()
    }

    /// 显示 / 隐藏小组件（**托盘菜单「显示小组件」与 `Ctrl+Alt+H` 共用这一份**）。
    ///
    /// v1.5.0（G3）起隐藏有**两个**来源：
    /// - `config.general.widget_hidden` —— 持久化的用户设置（托盘勾选项）；
    /// - `state.dismissed` —— 本次运行点了右上角 ×，**只活在内存里**。
    ///
    /// 判定必须按**有效隐藏态**（两者取或）来做，否则「点了 × 收起后按热键」会把
    /// 已经显示的窗口又"隐藏"一次（看到的是没反应）。语义：
    /// - 当前**隐藏** → 显示：两者一起清（`dismissed` 必须清，否则清完配置仍不可见）；
    /// - 当前**显示** → 隐藏：只写配置（保持既有「托盘隐藏会被记住」的行为，
    ///   与 × 的「仅本次运行」区分开）。
    fn toggle_widget_visible(&mut self) -> Vec<Effect> {
        let hidden = self.config.general.widget_hidden || self.state.dismissed;
        if hidden {
            self.config.general.widget_hidden = false;
            self.state.dismissed = false;
            crate::log_info!("小组件：显示（配置隐藏与 × 收起一并复位）");
        } else {
            self.config.general.widget_hidden = true;
            crate::log_info!("小组件：隐藏（写入配置，下次启动仍隐藏）");
        }
        self.sync_visibility();
        vec![Effect::SaveConfig(Box::new(self.config.clone()))]
    }

    /// 可见性同步（PRD 5.2 三道门控，M2 起真的生效）。
    fn sync_visibility(&mut self) {
        self.state.taskbar_collapsed = self.geometry.map(|g| g.collapsed).unwrap_or(false);
        // v1.5.0（G3）：有效隐藏态 = 「配置里隐藏」∨「本次运行点了 ×」。
        // `dismissed` 只在内存里，从不写盘 —— 这就是「重启后照常显示」。
        self.state.user_hidden = self.config.general.widget_hidden || self.state.dismissed;
        // v1.5.5 / G3.5：解冻等待第一份 `NowPlaying` 期间，即使门控算出「该显示」
        // 也必须保持隐藏——先让隐藏窗口刷成最新内容，再由 `finish_resume` 显示。
        // 否则 `LyricReady` / `ConfigChanged` 等事件顺路调到这里就会露出旧帧。
        self.state.visible = self.state.compute_visible() && !self.state.resume_pending;
        let vis = self.state.visible;
        if let Some(h) = self.widget.as_mut() {
            h.set_visible(vis);
        }
        if !vis {
            // PRD §5.3：隐藏时所有修饰态强制复位到 Idle
            self.interact.reset();
            self.sync_interaction();
            self.kill_widget_timer(TIMER_ANIM);
            widget_wnd::uninstall_wheel_hook();
            self.interact.wheel_hooked = false;
            self.kill_widget_timer(TIMER_LYRIC);
            self.last_lyric_key = None;
        }
    }

    /// 请求背景采样（亮度 → 文字对比度主题）。
    ///
    /// **所有采样入口都走这里**（悬停进入 / 换壁纸 / 几何变化 / 退出截图态 / 配置变更），
    /// 所以「上次采样时刻」只需在这里刷新一处（v1.4.4 / G2-B1 的心跳节流依赖它）。
    fn request_backdrop(&mut self) {
        // v1.5.5（G3.5）：全屏冻结期间不抓屏——窗口已隐藏，读桌面毫无意义，
        // BitBlt 还正是「后台占用」的大头。退出冻结后由 relayout 自然补采。
        if self.state.frozen {
            return;
        }
        // v1.2.0（G5 问题 2）：截图态**不抓屏**。截图工具的置顶遮罩会把整屏压暗 /
        // 打上十字线，抓到的是被污染的像素 → 亮度突变 → 文字色可能翻一下。
        // 退出截图态时由 `sync_capture_freeze` 立刻补一次，不会漏采样。
        if self.capture_frozen {
            return;
        }
        if self.geometry.is_none() {
            return;
        }
        let Some(sampler) = self.sampler.as_ref() else {
            return;
        };
        let scale = self.state.layout.effective_scale();
        let req = SampleRequest {
            epoch: self.state.geometry_epoch,
            rect: self.target_rect,
            scale,
            mode: self.config.appearance.background_mode,
            idle_mode: self.config.appearance.idle_mode,
            hover_alpha: self.config.appearance.hover_alpha,
            idle_alpha: self.config.appearance.idle_alpha,
            accent: self.state.theme.accent,
            coverage: self.last_coverage.clone(),
            text_rects: vec![
                (
                    self.state.layout.title,
                    crate::config::defaults::limits::SAMPLE_TEXT_WEIGHT,
                ),
                (
                    self.state.layout.artist,
                    crate::config::defaults::limits::SAMPLE_TEXT_WEIGHT,
                ),
            ],
        };
        sampler.request(req);
        // 真的投递了才刷新（上面三条 return 都没发请求，心跳下次还会再试）
        self.last_backdrop = Instant::now();
    }

    /// 记一条「窗口矩形变化」诊断日志（v1.2.0 / G5 问题 2）。
    ///
    /// 格式固定为 `old=(l,t,r,b) new=(l,t,r,b) reason=...`，便于 grep
    /// 「截图前后到底是哪条路径改了 rect」。矩形没变则不记。
    fn log_rect_change(&mut self, reason: RectChangeReason) {
        let old = self.logged_rect;
        let new = self.target_rect;
        if old == new {
            return;
        }
        self.logged_rect = new;
        // 位置变了就必须允许下一次漂移再记一条（否则漂移只报一次就静默）
        self.drift_logged = false;
        crate::log_info!(
            "widget rect 变化：old=({},{},{},{}) new=({},{},{},{}) reason={}",
            old.left,
            old.top,
            old.right,
            old.bottom,
            new.left,
            new.top,
            new.right,
            new.bottom,
            reason.as_str()
        );
    }

    // ---------------------------------------------------------------- 交互辅助

    fn is_hovering(&self) -> bool {
        self.interact.hover != HoverState::Idle
    }

    /// 把 `Interaction` 的内部状态同步进 `AppState`，并按需启停定时器。
    fn sync_interaction(&mut self) {
        self.state.hover = self.interact.hover;
        self.state.hover_alpha = self.interact.hover_alpha;
        self.state.pressed = self.interact.pressed;
        self.state.hover_target = self.interact.hover_target;
        self.state.volume_hud_left_ms = self.interact.hud_left_ms;

        // 16ms 动画定时器：只在真的需要推进时存在（悬停淡入淡出 / 歌词高度过渡）
        if self.interact.animating() || self.lyric_anim.is_some() || self.lyric_fade_active() {
            if !self.anim_timer_on {
                self.start_widget_timer(TIMER_ANIM, crate::config::defaults::limits::ANIM_TIMER_MS);
            }
        } else if self.anim_timer_on {
            self.kill_widget_timer(TIMER_ANIM);
        }

        // 250ms 单击判定定时器
        if self.interact.take_click_timer_wanted() {
            self.start_widget_timer(
                TIMER_CLICK,
                crate::config::defaults::limits::CLICK_DELAY_MS,
            );
        }
    }

    fn start_widget_timer(&mut self, id: usize, ms: u32) {
        // v1.5.5 / G3.5：冻结期间 ANIM / LYRIC 一律不许再挂起（0 唤醒）。
        // 单点守卫而不是改每个调用点：`sync_interaction` / `sync_lyric_timer` /
        // `animate_lyric_width` / `on_wake` 全都从这里过。
        if self.state.frozen && (id == TIMER_ANIM || id == TIMER_LYRIC) {
            return;
        }
        let Some(h) = self.widget.as_ref() else {
            return;
        };
        // SAFETY: hwnd 有效；SetTimer 在窗口销毁时会自动清理。
        unsafe {
            let _ = SetTimer(Some(h.hwnd()), id, ms, None);
        }
        match id {
            TIMER_ANIM => self.anim_timer_on = true,
            TIMER_LYRIC => self.lyric_timer_on = true,
            _ => {}
        }
    }

    fn kill_widget_timer(&mut self, id: usize) {
        if let Some(h) = self.widget.as_ref() {
            // SAFETY: 与 SetTimer 配对。
            unsafe {
                let _ = KillTimer(Some(h.hwnd()), id);
            }
        }
        match id {
            TIMER_ANIM => self.anim_timer_on = false,
            TIMER_LYRIC => self.lyric_timer_on = false,
            _ => {}
        }
    }

    /// 在**消息窗**上挂一次性定时器（v1.5.5 / G3.5 解冻兜底）。
    ///
    /// 消息窗是 `HWND_MESSAGE`，进程期常驻；`TIMER_GEOM`（1s）也在它上面，
    /// 两者 id 不冲突。同 id 重复 `SetTimer` 只是重置周期，不会叠加。
    fn start_msg_timer(&self, id: usize, ms: u32) {
        // SAFETY: msg_hwnd 由 bootstrap 创建且进程期有效。
        unsafe {
            let _ = SetTimer(Some(self.msg_hwnd), id, ms, None);
        }
    }

    /// 结束消息窗上的一次性定时器（幂等；WM_TIMER 已入队也无害，
    /// `on_timer` 里的 `resume_pending` 守卫会把它变成 no-op）。
    fn kill_msg_timer(&self, id: usize) {
        // SAFETY: 与 start_msg_timer 配对。
        unsafe {
            let _ = KillTimer(Some(self.msg_hwnd), id);
        }
    }

    /// 首次进入时注册 `TME_LEAVE`，这样离开窗口能收到 `WM_MOUSELEAVE`。
    fn request_mouse_leave(&self) {
        let Some(h) = self.widget.as_ref() else {
            return;
        };
        let mut tme = TRACKMOUSEEVENT {
            cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: h.hwnd(),
            dwHoverTime: 0,
        };
        // SAFETY: tme 是本地结构体，cbSize 已填。
        unsafe {
            let _ = TrackMouseEvent(&mut tme);
        }
    }

    /// 悬停期间挂 `WH_MOUSE_LL` 钩子（`WS_EX_NOACTIVATE` 窗口收不到滚轮）。
    fn ensure_wheel_hook(&mut self) {
        if self.interact.wheel_hooked || !self.config.volume.wheel_enabled {
            return;
        }
        if let Some(h) = self.widget.as_ref() {
            widget_wnd::install_wheel_hook(h.hwnd(), self as *mut App);
            self.interact.wheel_hooked = true;
        }
    }

    /// 把一批语义化输入送进 reduce，并在结束后同步交互态 + 重绘一次。
    fn dispatch(&mut self, inputs: Vec<WidgetInput>) {
        // v1.5.0（G3）：悬停目标变化本身不产生 `WidgetInput`，但它改变像素
        // （× 的悬停变亮）——所以在这里对比一次，变化就补一帧重绘。
        let prev_hover_target = self.state.hover_target;
        if inputs.is_empty() {
            self.sync_interaction();
            if self.state.hover_target != prev_hover_target {
                self.redraw();
            }
            return;
        }
        let mut needs_redraw = false;
        for input in inputs {
            let effects = self.reduce(AppEvent::WidgetInput(input));
            for e in effects {
                if matches!(e, Effect::Redraw) {
                    needs_redraw = true;
                } else {
                    self.apply(vec![e]);
                }
            }
        }
        self.sync_interaction();
        if needs_redraw || self.state.hover_target != prev_hover_target {
            self.redraw();
        }
    }

    /// 乐观更新：设一个 1s 后到期的覆盖。
    fn set_optimistic(&mut self, kind: OptimisticKind) {
        self.state.optimistic = Some(OptimisticOverride {
            kind,
            until_ms: self.now_ms() + 1_000,
        });
    }

    /// 乐观更新对账：超时未收到会话更新 → 回滚（PRD P0-04）。
    fn tick_optimistic(&mut self) {
        let Some(o) = self.state.optimistic else {
            return;
        };
        if self.now_ms() >= o.until_ms {
            crate::log_info!("乐观更新 {:?} 超时未确认，回滚为真实状态", o.kind);
            self.state.optimistic = None;
            self.redraw();
        }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    // ---------------------------------------------------------------- 菜单 / 对话框

    fn show_menu(&mut self, lparam: LPARAM) {
        let Some(host) = self.widget.as_ref() else {
            return;
        };
        let hwnd = host.hwnd();
        let (cx, cy) = client_xy(lparam);
        let origin = (self.target_rect.left, self.target_rect.top);
        let screen = menu::client_to_screen(origin, (cx as i32, cy as i32));

        // v1.1.14 问题 12：菜单打开期间暂停 1s 心跳的 ensure_on_top（见 on_tick），
        // 关闭后无论是否选中都要复位
        self.menu_open = true;
        let cmd = menu::show(hwnd, screen, &self.config, self.lang);
        self.menu_open = false;
        let Some(cmd) = cmd else {
            return;
        };
        crate::log_info!("右键菜单：{}", menu::command_name(cmd));

        // v1.2.1（G2 问题 6）：背景 4 项统一走一个入口，避免四份复制粘贴。
        // `bg_target()` 是 `ui::menu` 里的纯函数（有单测），这里只负责执行。
        if let Some((slot, mode)) = cmd.bg_target() {
            self.set_background(slot, mode);
            return;
        }

        match cmd {
            MenuCommand::ToggleLyrics => {
                self.config.lyrics.enabled = !self.config.lyrics.enabled;
                self.animate_lyric_width();
                let effects = self.commit_config();
                self.apply(effects);
            }
            // v1.1.16（问题 9）：循环/随机菜单项已删除——SMTC 远程变更只是
            // 「请求」，网易云/汽水等大量播放器声明能力却不响应，功能形同虚设。
            MenuCommand::OpenSettings => {
                // v1.1.14 问题 8：与托盘菜单同链路——设置页已开则前置，未开则创建
                self.on_second_instance();
            }
            MenuCommand::ShowAbout => self.show_about(),
            MenuCommand::Quit => self.apply(vec![Effect::Quit]),
            // 4 个背景项已在上面由 `bg_target()` 分支统一处理
            MenuCommand::IdleTransparent
            | MenuCommand::IdleTranslucent
            | MenuCommand::HoverTransparent
            | MenuCommand::HoverTranslucent => {}
        }
    }

    /// 改一组背景模式（v1.2.1 / G2 问题 6）。
    ///
    /// 顺序很重要，**不能靠异步采样回填**（决策 21：切换后必须立即重绘）：
    /// 1. 写配置字段（`BgSlot::Idle` → `idle_mode`；`BgSlot::Hover` → `background_mode`）；
    /// 2. `apply_bg_config` **就地**更新当前主题的两态背景色（同一帧就生效）；
    /// 3. `request_backdrop()` 补一次采样（背景色没变，但重新对账亮度也无害）；
    /// 4. `commit_config()` → `relayout` + `redraw` + 落盘，`apply` 执行副作用。
    fn set_background(&mut self, slot: crate::ui::menu::BgSlot, mode: BackgroundMode) {
        use crate::ui::menu::BgSlot;
        match slot {
            BgSlot::Idle => self.config.appearance.idle_mode = mode,
            BgSlot::Hover => self.config.appearance.background_mode = mode,
        }
        crate::log_info!(
            "右键菜单：背景 slot={} mode={:?}",
            slot.as_str(),
            mode
        );
        self.state.theme.apply_bg_config(
            self.config.appearance.background_mode,
            self.config.appearance.idle_mode,
            self.config.appearance.hover_alpha,
            self.config.appearance.idle_alpha,
        );
        self.request_backdrop();
        let effects = self.commit_config();
        self.apply(effects);
    }

    /// 配置改动后：重排 + 重绘 + 落盘。返回要执行的 Effect。
    fn commit_config(&mut self) -> Vec<Effect> {
        self.relayout_with(RectChangeReason::Resize);
        vec![Effect::SaveConfig(Box::new(self.config.clone()))]
    }

    fn show_about(&mut self) {
        let title = to_wide(i18n::t(self.lang, "about.title"));
        let body = to_wide(i18n::t(self.lang, "about.text"));
        // SAFETY: 两个缓冲都是 NUL 结尾且在本调用期间存活；无 owner 的模态框加 TOPMOST 防遮挡。
        unsafe {
            MessageBoxW(
                None,
                windows::core::PCWSTR(body.as_ptr()),
                windows::core::PCWSTR(title.as_ptr()),
                MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
            );
        }
    }

    /// 重新把小组件窗口放到 topmost 带的最前面（`SWP_NOACTIVATE`，不改焦点）。
    fn ensure_on_top(&mut self) {
        // v1.5.5 / G3.5：冻结期间绝不 `raise()`——这正是「小组件盖在全屏窗口上方」
        // 的根因（本轮需求的原始动机）。on_timer 已提前 return，这里再兜一层，
        // 保证 TaskbarCreated 等其它入口也不会把它抬上去。
        if self.state.frozen {
            return;
        }
        // v1.2.0 诊断：置顶轮询顺便对账「窗口实际矩形 vs target_rect」。
        // 正常情况下两者恒等（`relayout` 用 `apply_geometry` 写、`redraw` 用
        // `present` 的 pptdst 再写一次，用的都是同一个 target_rect）。一旦不等，
        // 说明有第三方（截图遮罩 / DPI 虚拟化 / 系统贴靠）动过窗口——
        // 这是「截图时左移」唯一能落到日志上的直接证据。
        if let Some(actual) = self.widget.as_ref().and_then(|h| h.window_rect()) {
            if actual != self.target_rect {
                if !self.drift_logged {
                    self.drift_logged = true;
                    crate::log_info!(
                        "widget 实际位置漂移：actual=({},{},{},{}) target=({},{},{},{}) \
                         reason={}（下一次 present 会拉回目标位置）",
                        actual.left,
                        actual.top,
                        actual.right,
                        actual.bottom,
                        self.target_rect.left,
                        self.target_rect.top,
                        self.target_rect.right,
                        self.target_rect.bottom,
                        RectChangeReason::EnsureOnTop.as_str()
                    );
                }
            } else {
                self.drift_logged = false;
            }
        }

        if let Some(h) = self.widget.as_ref() {
            if let Err(e) = h.raise() {
                crate::log_debug!("重新置顶失败：{e}");
            }
        }
    }

    // ---------------------------------------------------------------- 诊断

    /// 诊断用：当前几何 / 布局摘要。
    pub fn debug_summary(&self) -> String {
        let (hits, misses, cached) = match self.renderer.as_ref() {
            Some(r) => r.cover_stats(),
            None => (0, 0, 0),
        };
        format!(
            "layout={}×{} scale={:.3} visible={} hover={:?} alpha={:.2} theme_luma={:.3} media={} cover(h/m/c)={hits}/{misses}/{cached}",
            self.state.layout.width,
            self.state.layout.height,
            self.state.layout.scale,
            self.state.visible,
            self.state.hover,
            self.state.hover_alpha,
            self.state.theme.backdrop_luma,
            self.media.is_some()
        )
    }

    /// 事件投递器（工作线程用）。
    pub fn sink(&self) -> EventSink<AppEvent> {
        self.sink.clone()
    }

    /// 消息窗句柄（诊断用）。
    pub fn msg_hwnd(&self) -> HWND {
        self.msg_hwnd
    }

    /// 向媒体线程发一条命令。
    pub fn send_media(&self, cmd: MediaCommand) {
        if let Some(m) = self.media.as_ref() {
            m.send(cmd);
        }
    }
}

// ------------------------------------------------------------------ 自由函数

/// 按当前配置探测几何。
fn probe_geometry(config: &Config) -> Option<TaskbarGeometry> {
    let _ = config;
    geometry::probe()
}

/// 是不是「鼠标类」窗口消息（G2 诊断：`msg.mouse` 只给这类消息计时）。
///
/// 只挑真正由鼠标输入驱动的消息：定时器 / 广播 / 唤醒消息的耗时另有 `tick.*`
/// 与 `paint.frame` 覆盖，混进来会让「鼠标卡」这条线索失真。
///
/// ## ⚠️ 别被历史记录误导（v1.4.4 / G2-A1）
///
/// v1.4.1~v1.4.3 的日志里 `msg.mouse` 有三条慢记录，**全是假阳性**：
/// `843ms` / `882ms` 与同一毫秒的 `[app] 收到 Quit` / `[app] 右键菜单：Quit` 重合，
/// 而 `450001ms` 那条是**用户把右键菜单开着放了 7.5 分钟**——
/// `WM_RBUTTONUP` 会同步进 `TrackPopupMenuEx` 的**模态消息循环**，
/// 菜单开着的每一毫秒都被算进了这个 span。
///
/// 这不是卡顿。v1.4.4 起 `WM_RBUTTONUP` 分支在弹菜单**之前**就显式
/// `Span::finish()`，菜单模态时间不再计入；看到 `msg.mouse` 慢记录时请先确认
/// 日志里没有菜单相关行，再怀疑真的卡。
fn is_mouse_message(msg: u32) -> bool {
    use windows::Win32::UI::WindowsAndMessaging as wam;
    matches!(
        msg,
        wam::WM_MOUSEMOVE
            | wam::WM_LBUTTONDOWN
            | wam::WM_LBUTTONUP
            | wam::WM_RBUTTONUP
            | wam::WM_MOUSEWHEEL
            | wam::WM_CAPTURECHANGED
            | widget_wnd::WM_MOUSELEAVE
            | crate::platform::wake::WM_BARLINE_WHEEL
    )
}

/// 静止态心跳是否该补一次背景采样（v1.4.4 / G2-B1 + B2）——**纯函数**，可单测。
///
/// * `elapsed_ms`：距上次真的投递采样请求过了多久；
/// * `skip`：这次心跳是否该**整段跳过**（当前唯一的来源是「前台全屏且与小组件同屏」，
///   见 [`same_monitor`]）。
///
/// 只有「静止态心跳」用它；用户能感知的采样路径（悬停进入 / 换壁纸 / 几何变化 /
/// 退出截图态）直接调 `request_backdrop()`，**不经过这个判定**。
fn idle_backdrop_due(elapsed_ms: u128, skip: bool) -> bool {
    if skip {
        return false;
    }
    elapsed_ms >= crate::config::defaults::limits::BACKDROP_IDLE_MS as u128
}

/// 两个显示器矩形是不是**同一块屏**（v1.4.4 补丁）——**纯函数**，可单测。
///
/// 判据：四个字段完全相等。**任一方全 0 一律判 `false`**——
/// 拿不到数据时必须走「继续采样」这个安全方向：宁可多抓一次屏，
/// 也不要因为一次查询失败就让字色自适应永久停摆。
fn same_monitor(a: RECT, b: RECT) -> bool {
    if a == RECT::default() || b == RECT::default() {
        return false;
    }
    a == b
}

/// 这次心跳要不要**因前台全屏而跳过**（v1.4.4 补丁）——**纯函数**，可单测。
///
/// 两个条件必须同时成立：
/// 1. 前台窗口是全屏应用（读桌面既贵又没意义）；
/// 2. **那块全屏的屏就是小组件所在的屏**。
///
/// 第 2 条是本轮补的缺口：副屏全屏打游戏 / 看视频时，主屏小组件的字色自适应
/// 不该跟着停摆——那条心跳存在的唯一理由就是「用户把窗口拖到小组件后面」，
/// 与别的显示器上的全屏毫无关系。
fn should_skip_idle_backdrop(fullscreen: bool, fg_monitor: RECT, widget_monitor: RECT) -> bool {
    fullscreen && same_monitor(fg_monitor, widget_monitor)
}

/// 是否进入「全屏冻结」（v1.5.5 / G3.5）——**纯函数**，可单测。
///
/// 判据与 [`should_skip_idle_backdrop`] **完全同源**（直接复用，不复制逻辑）：
/// 前台窗口全屏 **且** 全屏的那块屏就是小组件所在的屏。于是「同屏全屏」这一个
/// 事实同时驱动两件事：v1.4.4 的静止态心跳跳过、v1.5.5 的整个小组件冻结。
///
/// 安全方向：副屏全屏、任一显示器矩形取不到（全 0）时都判 `false`（不冻结），
/// 与 v1.4.4 多显示器补丁一致——宁可多跑一次心跳，也不能让主屏小组件被误冻。
fn frozen_decision(foreground_fullscreen: bool, fg_monitor: RECT, widget_monitor: RECT) -> bool {
    should_skip_idle_backdrop(foreground_fullscreen, fg_monitor, widget_monitor)
}

/// 从 `LPARAM` 里取客户区坐标（`GET_X_LPARAM` / `GET_Y_LPARAM` 的等价实现）。
fn client_xy(lp: LPARAM) -> (f32, f32) {
    let v = lp.0 as u32;
    let x = (v & 0xFFFF) as u16 as i16 as i32 as f32;
    let y = ((v >> 16) & 0xFFFF) as u16 as i16 as i32 as f32;
    (x, y)
}

fn current_hinstance() -> HINSTANCE {
    // SAFETY: 纯查询；失败时返回无效句柄，CreateWindowExW 会报错并被上层记录。
    match unsafe {
        windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::PCWSTR::null())
    } {
        Ok(h) => HINSTANCE(h.0),
        Err(_) => HINSTANCE::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::layout::TaskbarEdge;

    #[test]
    fn no_session_is_not_visible() {
        let s = AppState::default();
        assert_eq!(s.playing.state, PlaybackState::NoSession);
        assert!(!s.compute_visible());
    }

    #[test]
    fn playing_is_visible_unless_hidden() {
        let mut s = AppState::default();
        s.playing.state = PlaybackState::Playing;
        assert!(s.compute_visible());

        s.user_hidden = true;
        assert!(!s.compute_visible());

        s.user_hidden = false;
        s.taskbar_collapsed = true;
        assert!(!s.compute_visible());
    }

    #[test]
    fn paused_stays_visible() {
        let mut s = AppState::default();
        s.playing.state = PlaybackState::Paused;
        assert!(s.compute_visible(), "暂停必须保持显示（PRD P0-02）");
    }

    #[test]
    fn optimistic_overrides_playback_state() {
        let mut s = AppState::default();
        s.playing.state = PlaybackState::Paused;
        assert_eq!(s.effective_state(), PlaybackState::Paused);

        s.optimistic = Some(OptimisticOverride {
            kind: OptimisticKind::Playing,
            until_ms: u64::MAX,
        });
        assert_eq!(s.effective_state(), PlaybackState::Playing);
    }

    #[test]
    fn layout_rects_are_sane() {
        let l = layout::compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        assert_eq!(l.width, 200);
        assert_eq!(l.height, 48);
        assert!(l.title.x > l.cover.right());
        assert_eq!(crate::render::layout::RectF::EMPTY.w, 0.0);
    }

    #[test]
    fn client_coords_decode_signed() {
        // x = 100, y = 40
        let lp = LPARAM(((40u32 << 16) | 100u32) as isize);
        assert_eq!(client_xy(lp), (100.0, 40.0));
        // 负坐标（拖出窗口左边界）
        let lp = LPARAM(((-5i32 as u32 & 0xFFFF) | ((-3i32 as u32 & 0xFFFF) << 16)) as isize);
        assert_eq!(client_xy(lp), (-5.0, -3.0));
    }

    #[test]
    fn progress_fraction_uses_preview_while_seeking() {
        use crate::media::Track;
        let mut s = AppState::default();
        s.playing.track = Some(Track {
            title: String::from("t"),
            duration_ms: 100_000,
            ..Default::default()
        });
        s.playing.state = PlaybackState::Paused;
        s.playing.position_ms = 10_000;
        assert!((s.progress_fraction() - 0.1).abs() < 1e-4);

        s.hover = HoverState::Seeking;
        s.seek_preview_ms = 50_000;
        assert!((s.progress_fraction() - 0.5).abs() < 1e-4);
    }

    #[test]
    fn progress_fraction_is_zero_without_duration() {
        let s = AppState::default();
        assert_eq!(s.progress_fraction(), 0.0);
    }

    // ================================================ QA（G3 独立验证）补充断言
    //
    // `sync_visibility` / `toggle_widget_visible` / `reduce_input(ClickDismiss)`
    // 都是 `App` 的方法（依赖 HWND、托盘、渲染器），无法在单测里构造。
    // 这里只覆盖它们**依赖的纯数据前提**，方法体的行为用代码走查 + file:line
    // 证据在 QA 报告里说明（见 E 组）。

    /// **E4 组**：`dismissed` 是**运行期**标记，默认必须为 `false`。
    ///
    /// 「重启后照常显示」完全依赖这一点：`dismissed` 不落盘、也没有任何从配置
    /// 恢复它的路径，所以下次启动 `user_hidden` 只由 `config.general.widget_hidden`
    /// 决定。若有人给它加上 `serde`/持久化，这条断言会先失败。
    #[test]
    fn dismissed_defaults_to_false() {
        let s = AppState::default();
        assert!(!s.dismissed, "× 收起是运行期状态，重启后不得残留");
        assert!(!s.user_hidden);
        assert!(!s.visible);
    }

    /// **E 组**：有效隐藏态真值表（2×2），与 `sync_visibility` 的公式同源。
    ///
    /// 依据 `src/app.rs:2267`：
    /// `user_hidden = config.general.widget_hidden || state.dismissed`
    /// 因此：
    /// - `(false, false)` → 可见（常规状态）；
    /// - `(false, true)`  → **隐藏**（刚点了 ×，配置仍是「显示」）；
    /// - `(true,  false)` → 隐藏（托盘里持久化的隐藏）；
    /// - `(true,  true)`  → 隐藏。
    ///
    /// 关键是第 2 行：它保证「点 × 之后按热键 / 勾托盘」走的是
    /// `toggle_widget_visible` 的「当前隐藏 → 显示」分支（`src/app.rs:2249-2253`
    /// 同时清 `widget_hidden` 与 `dismissed`），不会出现"按了没反应"。
    #[test]
    fn effective_hidden_truth_table() {
        for widget_hidden in [false, true] {
            for dismissed in [false, true] {
                let mut s = AppState::default();
                s.playing.state = PlaybackState::Playing;
                s.dismissed = dismissed;
                // 与 `sync_visibility` 完全同一公式
                s.user_hidden = widget_hidden || dismissed;

                let hidden = widget_hidden || dismissed;
                assert_eq!(
                    s.compute_visible(),
                    !hidden,
                    "widget_hidden={widget_hidden} dismissed={dismissed} → \
                     user_hidden={} 时可见性应为 {}",
                    s.user_hidden,
                    !hidden
                );
            }
        }

        // 单独点名第 2 行（本轮最可能出错的组合）：配置说"显示"、但点了 ×
        //
        // 注意：`AppState` 不持有 `Config`，所以这里只能验证"有效隐藏态已生效"，
        // 「× 不写配置」这一半靠走查 `reduce_input(ClickDismiss)` 的返回 effect 确认
        // （`src/app.rs:1762-1770`：只置 `dismissed` + `sync_visibility`，
        // 返回 `vec![Effect::Redraw]`，**没有** `Effect::SaveConfig`）。
        let mut s = AppState::default();
        s.playing.state = PlaybackState::Playing;
        s.dismissed = true;
        s.user_hidden = s.dismissed;
        assert!(
            !s.compute_visible(),
            "点 × 收起（配置仍为 widget_hidden=false）后必须不可见"
        );
    }
}

/// 配置 → 信息区最小宽（**逻辑像素**，未乘 scale；v1.1.1：行数固定单行，
/// 开关只切 0 ↔ 160；config 里的旧 `mode` / `lyrics_two_line` 字段忽略不迁移）。
///
/// 与 `WidgetForm::lyric_w()` 必须保持一致：一个是"当前动画值"，
/// 一个是"形态默认值"，两者在动画结束时相等。
fn lyric_w_logical(config: &Config) -> f32 {
    if !config.lyrics.enabled {
        return 0.0;
    }
    crate::config::defaults::limits::LYRIC_W
}

/// 演唱窗口（毫秒）→ 滚动窗口（毫秒）的**纯函数**（v1.1.4）。
///
/// 滚动在「最后一个字唱完」之前 [`limits::MARQUEE_LEAD_MS`] 提前完成，
/// 提前量同时钳到演唱窗口的 1/4——行尾 + 行间间歇留给用户读最后一个字，
/// 短句也不会因为提前太多而滚得过快。
fn marquee_scroll_window_ms(sing_window_ms: i64) -> u32 {
    if sing_window_ms <= 0 {
        return 0;
    }
    let lead = crate::config::defaults::limits::MARQUEE_LEAD_MS.min(sing_window_ms / 4);
    (sing_window_ms - lead) as u32
}

/// 跑马灯本帧输入（v1.1.3，便于单测）。字段语义见 [`marquee_frame`] 文档。
#[derive(Debug, Clone, Copy)]
struct MarqueeInput {
    /// 长歌词滚动开关（`config.lyrics.scroll`）
    enabled: bool,
    /// 当前行是占位文案
    placeholder: bool,
    /// 当前行文本为空
    cur_empty: bool,
    /// 当前形态是歌词形态
    lyric_on: bool,
    /// 歌词行自然宽（渲染器缓存；`None` 表示尚无布局）
    text_w: Option<f32>,
    /// 歌词行视口宽
    viewport_w: f32,
    /// 行内已播时长（毫秒）
    elapsed_ms: u32,
    /// 行时间窗口（毫秒）
    window_ms: u32,
}

/// 跑马灯本帧判定的**纯函数**（v1.1.3，便于单测）：返回 `(是否激活, 偏移)`。
///
/// 激活条件：开关开 + 歌词形态 + 非占位 + 非空行 + 歌词行自然宽超出视口 > 0.5px，
/// 且行时间窗口不小于 [`limits::MARQUEE_MIN_WINDOW_SECS`]（短窗口滚完一圈快到看不清，只显示行首）。
/// 激活时偏移由 [`crate::render::painter::marquee_offset`] 按**行内进度**给出
/// （行时间窗口驱动：唱完该句时刚好滚到尾）。
fn marquee_frame(i: MarqueeInput) -> (bool, f32) {
    let usable = i.enabled && i.lyric_on && !i.placeholder && !i.cur_empty;
    let excess = if usable {
        i.text_w.map_or(0.0, |tw| (tw - i.viewport_w).max(0.0))
    } else {
        0.0
    };
    let window = i.window_ms as f32 / 1000.0;
    if excess > 0.5 && window >= crate::config::defaults::limits::MARQUEE_MIN_WINDOW_SECS {
        (
            true,
            crate::render::painter::marquee_offset(excess, i.elapsed_ms as f32 / 1000.0, window),
        )
    } else {
        (false, 0.0)
    }
}

#[cfg(test)]
mod marquee_tests {
    use super::*;

    /// 构造输入的便捷函数：默认超宽 400px / 视口 160px / 长窗口 10s。
    fn input(
        enabled: bool,
        placeholder: bool,
        cur_empty: bool,
        lyric_on: bool,
        text_w: Option<f32>,
        elapsed_ms: u32,
        window_ms: u32,
    ) -> MarqueeInput {
        MarqueeInput {
            enabled,
            placeholder,
            cur_empty,
            lyric_on,
            text_w,
            viewport_w: 160.0,
            elapsed_ms,
            window_ms,
        }
    }

    #[test]
    fn marquee_disabled_means_no_scroll() {
        // 开关关：无论文本多宽都不滚
        let (m, o) = marquee_frame(input(false, false, false, true, Some(500.0), 99_999, 10_000));
        assert!(!m);
        assert_eq!(o, 0.0);
        // 占位 / 空行 / 歌词关：同样不滚
        for (ph, empty, on) in [(true, false, true), (false, true, true), (false, false, false)] {
            let (m, o) = marquee_frame(input(true, ph, empty, on, Some(500.0), 50_000, 10_000));
            assert!(!m && o == 0.0, "ph={ph} empty={empty} on={on}");
        }
    }

    #[test]
    fn marquee_active_only_when_overflowing() {
        // 恰好放得下：不滚
        let (m, o) = marquee_frame(input(true, false, false, true, Some(160.0), 5_000, 10_000));
        assert!(!m && o == 0.0);
        // 超出 0.5px 以内：不滚（去抖）
        let (m, _) = marquee_frame(input(true, false, false, true, Some(160.4), 5_000, 10_000));
        assert!(!m);
        // 超宽：激活，行首（进度 0）偏移为 0
        let (m, o) = marquee_frame(input(true, false, false, true, Some(400.0), 0, 10_000));
        assert!(m);
        assert_eq!(o, 0.0);
    }

    #[test]
    fn marquee_offset_tracks_window_progress() {
        let excess = 400.0 - 160.0;
        // 行窗口 10s，行内播到一半 → 滚一半
        let (_, o) = marquee_frame(input(true, false, false, true, Some(400.0), 5_000, 10_000));
        assert!((o - excess * 0.5).abs() < 0.5, "o = {o}");
        // 行唱完（窗口尽头）→ 刚好滚到尾（快歌慢歌都不再截断）
        let (_, o) = marquee_frame(input(true, false, false, true, Some(400.0), 10_000, 10_000));
        assert!((o - excess).abs() < 0.01, "o = {o}");
        // 唱完之后（最后一行常驻屏幕）→ 钳在尾部
        let (_, o) = marquee_frame(input(true, false, false, true, Some(400.0), 30_000, 10_000));
        assert!((o - excess).abs() < 0.01, "o = {o}");
    }

    #[test]
    fn marquee_short_window_falls_back_to_fade() {
        // 行窗口 < MARQUEE_MIN_WINDOW_SECS：不激活（只显示行首），偏移 0
        let min = crate::config::defaults::limits::MARQUEE_MIN_WINDOW_SECS;
        let (m, o) = marquee_frame(input(
            true,
            false,
            false,
            true,
            Some(400.0),
            1_000,
            ((min - 0.1) * 1000.0) as u32,
        ));
        assert!(!m && o == 0.0);
    }

    #[test]
    fn marquee_scroll_window_subtracts_lead() {
        // 长句（10s）：提前 1.2s 滚完，行尾留给读最后一个字
        assert_eq!(marquee_scroll_window_ms(10_000), 8_800);
        // 短句（2s）：提前量钳到窗口 1/4（0.5s），滚动窗口 1.5s
        assert_eq!(marquee_scroll_window_ms(2_000), 1_500);
        // 更短（0.4s）：提前 0.1s；零 / 负窗口：0（不滚）
        assert_eq!(marquee_scroll_window_ms(400), 300);
        assert_eq!(marquee_scroll_window_ms(0), 0);
        assert_eq!(marquee_scroll_window_ms(-5), 0);
    }

    #[test]
    fn marquee_freezes_without_ticks() {
        // 同一播放位置必然同一偏移（暂停冻结）：同一入参必然同一输出
        let a = marquee_frame(input(true, false, false, true, Some(400.0), 7_777, 10_000));
        let b = marquee_frame(input(true, false, false, true, Some(400.0), 7_777, 10_000));
        assert_eq!(a, b);
    }

    // ------------------------------------------------ v1.4.4：心跳降载（B1 / B2）

    /// 心跳间隔常量就是 3s（验收口径，改它等于改「降载 2/3」这个承诺）。
    #[test]
    fn idle_backdrop_interval_is_three_seconds() {
        assert_eq!(crate::config::defaults::limits::BACKDROP_IDLE_MS, 3_000);
        // 采样线程自己的节流更短，所以 3s 心跳不会被它吃掉
        assert!(crate::config::defaults::limits::SAMPLE_THROTTLE_MS < 3_000);
    }

    /// 静止态：不到 3s 不补采，到点才补（边界含等号）。
    #[test]
    fn idle_backdrop_due_only_after_three_seconds() {
        let ms = crate::config::defaults::limits::BACKDROP_IDLE_MS as u128;
        assert!(!idle_backdrop_due(0, false));
        assert!(!idle_backdrop_due(999, false), "1s 心跳时代的那次补采必须被吃掉");
        assert!(!idle_backdrop_due(ms - 1, false));
        assert!(idle_backdrop_due(ms, false), "恰好 3s 就要补（≥ 不是 >）");
        assert!(idle_backdrop_due(10_000, false));
    }

    /// 前台全屏：无论过多久都不补（读桌面既贵又没意义）。
    #[test]
    fn idle_backdrop_never_due_when_foreground_is_fullscreen() {
        assert!(!idle_backdrop_due(0, true));
        assert!(!idle_backdrop_due(3_000, true));
        assert!(!idle_backdrop_due(600_000, true), "全屏 10 分钟也不许补采");
    }

    /// 全屏标记默认必须是 `false`：拿不到前台信息时不能永久停掉采样。
    #[test]
    fn fullscreen_flag_defaults_to_false() {
        assert!(!AppState::default().foreground_fullscreen);
        assert_eq!(
            AppState::default().foreground_monitor,
            RECT::default(),
            "默认必须是全 0（`same_monitor` 会因此判 false → 继续采样）"
        );
    }

    // ------------------------------------------- v1.4.4 补丁：多显示器误伤（B2）

    /// 一台 1920×1080 主显示器 / 一块偏移到右边的副屏。
    fn mon_primary() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    fn mon_secondary() -> RECT {
        RECT {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        }
    }

    /// 完全相同才算同一块屏；差 1px 就不算（矩形是整数物理像素，不该有"近似同屏"）。
    #[test]
    fn same_monitor_requires_exact_match() {
        assert!(same_monitor(mon_primary(), mon_primary()));
        assert!(same_monitor(mon_secondary(), mon_secondary()), "副屏偏移坐标同样成立");
        for other in [
            RECT {
                left: 1,
                ..mon_primary()
            },
            RECT {
                left: -1,
                ..mon_primary()
            },
            RECT {
                right: 1921,
                ..mon_primary()
            },
            RECT {
                top: 1,
                ..mon_primary()
            },
            RECT {
                bottom: 1079,
                ..mon_primary()
            },
        ] {
            assert!(!same_monitor(mon_primary(), other), "差 1px 不算同屏：{other:?}");
        }
        assert!(!same_monitor(mon_primary(), mon_secondary()), "左右两块屏不是同屏");
    }

    /// 任一方全 0 → 一律判 false（安全方向：继续采样）。
    #[test]
    fn same_monitor_is_false_when_either_side_is_unknown() {
        let zero = RECT::default();
        assert!(!same_monitor(zero, zero), "两个都拿不到 ≠ 同一块屏");
        assert!(!same_monitor(zero, mon_primary()));
        assert!(!same_monitor(mon_primary(), zero));
    }

    /// **本轮核心断言**：前台全屏但在**别的**显示器上 → 小组件这块屏必须继续心跳采样。
    #[test]
    fn fullscreen_on_another_monitor_must_not_skip_sampling() {
        let ms = crate::config::defaults::limits::BACKDROP_IDLE_MS as u128;
        // 副屏全屏、小组件在主屏 → 不在同一块屏 → 不跳过 → 到点照常补采
        let skip = should_skip_idle_backdrop(true, mon_secondary(), mon_primary());
        assert!(!skip, "副屏全屏不该让主屏小组件跳过采样");
        assert!(
            idle_backdrop_due(ms, skip),
            "到 3s 必须补采（这是 v1.4.4 补丁的修复点）"
        );

        // 对照：同一块屏全屏 → 跳过 → 永不补采
        let skip_same = should_skip_idle_backdrop(true, mon_primary(), mon_primary());
        assert!(skip_same);
        assert!(!idle_backdrop_due(ms, skip_same));
        assert!(!idle_backdrop_due(600_000, skip_same));
    }

    /// 组合矩阵：`foreground_fullscreen × same_monitor` 四格都要对。
    #[test]
    fn skip_matrix_covers_all_four_cases() {
        let ms = crate::config::defaults::limits::BACKDROP_IDLE_MS as u128;
        // (fullscreen, 前台屏, 小组件屏) → 到点时是否仍该补采
        let cases = [
            (false, mon_primary(), mon_primary(), true, "非全屏：同屏也照常补"),
            (false, mon_secondary(), mon_primary(), true, "非全屏：不同屏照常补"),
            (true, mon_primary(), mon_primary(), false, "同屏全屏：跳过"),
            (true, mon_secondary(), mon_primary(), true, "异屏全屏：照常补"),
        ];
        for (fullscreen, fg, widget, want_due, why) in cases {
            let skip = should_skip_idle_backdrop(fullscreen, fg, widget);
            assert!(
                idle_backdrop_due(ms, skip) == want_due,
                "{why}（skip={skip}）"
            );
        }
    }

    /// `widget_monitor` 还没填（全 0，例如启动早期 / 重排失败）→ 必须继续采样。
    #[test]
    fn unknown_widget_monitor_keeps_sampling() {
        let ms = crate::config::defaults::limits::BACKDROP_IDLE_MS as u128;
        let skip = should_skip_idle_backdrop(true, mon_primary(), RECT::default());
        assert!(!skip);
        assert!(
            idle_backdrop_due(ms, skip),
            "拿不到小组件所在屏时必须继续采样（安全方向）"
        );
    }

    // ---------------------------------------------- v1.5.5 / G3.5：全屏冻结

    /// 两个新标记默认必须是 `false`：启动时不该莫名其妙处于冻结 / 解冻等待态。
    #[test]
    fn freeze_flags_default_to_false() {
        let s = AppState::default();
        assert!(!s.frozen);
        assert!(!s.resume_pending);
    }

    /// **冻结判定真值表**：`(foreground_fullscreen, same_monitor,
    /// taskbar_collapsed, user_hidden)` 共 16 格，逐格断言 `frozen` 与
    /// `compute_visible()`。同时点名两条安全方向：副屏全屏、monitor 全 0。
    #[test]
    fn frozen_decision_matrix() {
        for fullscreen in [false, true] {
            for same in [false, true] {
                for collapsed in [false, true] {
                    for hidden in [false, true] {
                        let fg_mon = if same { mon_primary() } else { mon_secondary() };
                        let frozen = frozen_decision(fullscreen, fg_mon, mon_primary());
                        assert_eq!(
                            frozen,
                            fullscreen && same,
                            "fullscreen={fullscreen} same={same} → frozen 应为 {}",
                            fullscreen && same
                        );

                        let mut s = AppState::default();
                        s.playing.state = PlaybackState::Playing; // 有会话
                        s.user_hidden = hidden;
                        s.taskbar_collapsed = collapsed;
                        s.frozen = frozen;
                        let want_visible = !hidden && !collapsed && !frozen;
                        assert_eq!(
                            s.compute_visible(),
                            want_visible,
                            "fullscreen={fullscreen} same={same} collapsed={collapsed} \
                             hidden={hidden} frozen={frozen}"
                        );
                    }
                }
            }
        }

        // 点名 1：同屏全屏才冻结
        assert!(frozen_decision(true, mon_primary(), mon_primary()));
        assert!(!frozen_decision(false, mon_primary(), mon_primary()));
        // 点名 2：**副屏全屏不得冻结主屏**（v1.4.4 多显示器补丁口径）
        assert!(!frozen_decision(true, mon_secondary(), mon_primary()));
        // 点名 3：任一 monitor 全 0 → 不冻结（安全方向）
        assert!(!frozen_decision(true, RECT::default(), mon_primary()));
        assert!(!frozen_decision(true, mon_primary(), RECT::default()));
        assert!(!frozen_decision(true, RECT::default(), RECT::default()));
    }

    /// `frozen == true` 时 `compute_visible()` **恒** false：不管会话态、
    /// 用户隐藏、任务栏收起怎么组合，也不管是不是刚解冻结。
    #[test]
    fn compute_visible_is_false_whenever_frozen() {
        for hidden in [false, true] {
            for collapsed in [false, true] {
                for state in [
                    PlaybackState::NoSession,
                    PlaybackState::Paused,
                    PlaybackState::Playing,
                ] {
                    let mut s = AppState::default();
                    s.playing.state = state;
                    s.user_hidden = hidden;
                    s.taskbar_collapsed = collapsed;
                    s.frozen = true;
                    assert!(
                        !s.compute_visible(),
                        "frozen 时必须恒不可见：hidden={hidden} collapsed={collapsed} state={state:?}"
                    );
                }
            }
        }
        // 回归：没冻结时既有真值表不许被改坏（不放宽任何既有断言）
        let mut s = AppState::default();
        s.playing.state = PlaybackState::Playing;
        assert!(s.compute_visible());
        s.frozen = true;
        assert!(!s.compute_visible());
    }

    /// 冻结判定与 v1.4.4 心跳跳过判据**同源**（不许两套逻辑漂移）。
    #[test]
    fn frozen_decision_shares_backdrop_skip_criterion() {
        for fullscreen in [false, true] {
            for fg in [mon_primary(), mon_secondary(), RECT::default()] {
                for widget in [mon_primary(), mon_secondary(), RECT::default()] {
                    assert_eq!(
                        frozen_decision(fullscreen, fg, widget),
                        should_skip_idle_backdrop(fullscreen, fg, widget),
                        "fg={fg:?} widget={widget:?}"
                    );
                }
            }
        }
    }

    /// 解冻兜底 400ms：
    /// - **≥200ms**：给媒体线程一次解冻后的全量读（跨进程 SMTC 正常 1~3ms，
    ///   异常慢也不该被提前显示旧帧）留出余量；
    /// - **≤1000ms**：`TIMER_GEOM_MS` 就是 1s，退出全屏后最多再等一个心跳周期
    ///   必须看见小组件，且**不许**为此把 1s 心跳调快（禁改清单）；
    /// - 400ms 取中，媒体线程异常 / 无会话（永远没有 NowPlaying）时也不会永远隐藏。
    #[test]
    fn resume_fallback_timeout_is_within_bounds() {
        assert!(
            RESUME_FALLBACK_MS >= 200,
            "太短会在第一份 NowPlaying 到达前露出冻结前的旧帧"
        );
        assert!(
            RESUME_FALLBACK_MS <= 1000,
            "不得超过 1s 几何心跳周期，也不许为此新增更快的轮询"
        );
        assert_eq!(RESUME_FALLBACK_MS, 400);
        assert_eq!(TIMER_RESUME, 6, "消息窗定时器 id 不得与既有 id 冲突");
        assert_eq!(widget_wnd::TIMER_GEOM_MS, 1_000, "1s 心跳是本轮上限，不许调快");
    }
}

// ============================================================================
// QA 独立回归验证（Edward / G3.5 v1.5.5）
// 只新增测试代码；不复用工程师的 mon_primary / mon_secondary helper，
// 全部数据独立构造，期望值独立推导，避免「测试和实现错到一起去」。
// ============================================================================
#[cfg(test)]
mod qa_g35_regression {
    use super::*;

    /// QA 自造的显示器矩形：主屏（含 0 字段但非全 0）、副屏、差 1px、
    /// 左屏（含 0 字段）、取不到（全 0）。
    fn qa_rects() -> Vec<(&'static str, RECT)> {
        vec![
            ("primary", RECT { left: 0, top: 0, right: 1920, bottom: 1080 }),
            ("secondary", RECT { left: 1920, top: 0, right: 3840, bottom: 1080 }),
            ("primary_1px_off", RECT { left: 0, top: 0, right: 1921, bottom: 1080 }),
            ("left_monitor", RECT { left: -1920, top: 0, right: 0, bottom: 1080 }),
            ("unknown_zero", RECT::default()),
        ]
    }

    /// 独立期望：全屏 + 两块矩形严格相等 + 都不是全 0，才冻结。
    fn qa_expected(fullscreen: bool, fg: RECT, widget: RECT) -> bool {
        let unknown = fg == RECT::default() || widget == RECT::default();
        fullscreen && !unknown && fg == widget
    }

    /// B1 + B2：2 × 5 × 5 = 50 格真值表，逐格对照独立期望；
    /// 并逐格确认 frozen_decision 与 should_skip_idle_backdrop 同值（防判据漂移）。
    #[test]
    fn qa_frozen_decision_full_truth_table_and_no_criterion_drift() {
        let rects = qa_rects();
        let mut cells = 0;
        for fullscreen in [false, true] {
            for (fname, fg) in &rects {
                for (wname, widget) in &rects {
                    let want = qa_expected(fullscreen, *fg, *widget);
                    let got = frozen_decision(fullscreen, *fg, *widget);
                    assert_eq!(
                        got,
                        want,
                        "fullscreen={fullscreen} fg={fname}({fg:?}) widget={wname}({widget:?})"
                    );
                    assert_eq!(
                        got,
                        should_skip_idle_backdrop(fullscreen, *fg, *widget),
                        "判据漂移：fullscreen={fullscreen} fg={fname} widget={wname}"
                    );
                    cells += 1;
                }
            }
        }
        assert_eq!(cells, 50);
    }

    /// B1 点名：同屏才冻结、副屏不冻结主屏、任一侧取不到一律不冻结（安全方向）。
    #[test]
    fn qa_safety_direction_never_freezes_on_unknown_or_secondary_monitor() {
        let p = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let s = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let z = RECT::default();
        assert!(frozen_decision(true, p, p), "同屏全屏必须冻结");
        assert!(!frozen_decision(true, s, p), "副屏全屏不得冻结主屏");
        assert!(!frozen_decision(true, p, s), "主屏全屏不得冻结副屏");
        assert!(!frozen_decision(true, z, p), "fg monitor 取不到 → 不冻结");
        assert!(!frozen_decision(true, p, z), "widget monitor 取不到 → 不冻结");
        assert!(!frozen_decision(true, z, z), "两边都取不到 ≠ 同屏");
        assert!(!frozen_decision(false, p, p), "没全屏一律不冻结");
        // 含 0 字段但不是全 0 的合法显示器仍应能判同屏（别把主屏误当「取不到」）
        let left = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        assert!(frozen_decision(true, left, left), "含 0 字段的合法矩形仍算同屏");
    }

    /// B3：compute_visible 门控全组合（2 × 2 × 2 × 3 = 24 格）。
    /// 工程师的矩阵只设 Playing，这里补上 NoSession × frozen 等组合，
    /// 钉死「frozen 恒 false」以及 user_hidden / taskbar_collapsed / NoSession 的优先级。
    #[test]
    fn qa_compute_visible_gate_order_full_matrix() {
        let states = [
            PlaybackState::NoSession,
            PlaybackState::Paused,
            PlaybackState::Playing,
        ];
        let mut cells = 0;
        for user_hidden in [false, true] {
            for collapsed in [false, true] {
                for frozen in [false, true] {
                    for state in states {
                        let mut st = AppState::default();
                        st.user_hidden = user_hidden;
                        st.taskbar_collapsed = collapsed;
                        st.frozen = frozen;
                        st.playing.state = state;
                        let want = !user_hidden
                            && !collapsed
                            && !frozen
                            && state != PlaybackState::NoSession;
                        assert_eq!(
                            st.compute_visible(),
                            want,
                            "hidden={user_hidden} collapsed={collapsed} frozen={frozen} state={state:?}"
                        );
                        if frozen {
                            assert!(!st.compute_visible(), "frozen 时必须恒不可见");
                        }
                        cells += 1;
                    }
                }
            }
        }
        assert_eq!(cells, 24);
    }

    /// E2/E6：resume_pending **不**参与 compute_visible()——
    /// 隐藏是靠 sync_visibility() 里的 `&& !resume_pending`；
    /// 正因为如此，finish_resume 第 847 行的 compute_visible() 门控才能独立生效。
    #[test]
    fn qa_resume_pending_is_not_part_of_compute_visible_gate() {
        let mut st = AppState::default();
        st.playing.state = PlaybackState::Playing;
        for hidden in [false, true] {
            for collapsed in [false, true] {
                st.user_hidden = hidden;
                st.taskbar_collapsed = collapsed;
                let base = st.compute_visible();
                st.resume_pending = true;
                assert_eq!(
                    st.compute_visible(),
                    base,
                    "resume_pending 不得改变门控结论；保持隐藏靠 sync_visibility 的 && !resume_pending"
                );
                st.resume_pending = false;
            }
        }
    }

    /// E6：钉死 finish_resume 的显示门控 `geometry.is_some() && compute_visible()`。
    /// 冻结期 Ctrl+Alt+H 主动隐藏 / 任务栏收起 / 无会话 / 几何不可用时，
    /// 400ms 兜底都不得强行把窗口露出来。
    #[test]
    fn qa_finish_resume_show_gate_blocks_user_hidden_and_no_session() {
        fn gate(geometry_present: bool, st: &AppState) -> bool {
            geometry_present && st.compute_visible()
        }

        let mut st = AppState::default();
        st.playing.state = PlaybackState::Playing;
        assert!(gate(true, &st), "基线：几何就绪 + 有会话 + 未隐藏 → 允许显示");

        st.user_hidden = true; // Ctrl+Alt+H / × 主动隐藏
        assert!(!gate(true, &st), "用户主动隐藏时兜底不得显示");
        st.user_hidden = false;

        st.taskbar_collapsed = true;
        assert!(!gate(true, &st), "任务栏收起时不得显示");
        st.taskbar_collapsed = false;

        st.playing.state = PlaybackState::NoSession;
        assert!(!gate(true, &st), "无会话时不得显示");
        st.playing.state = PlaybackState::Playing;

        assert!(!gate(false, &st), "几何不可用时不得显示");
    }

    /// D4/E4：5 个定时器 id 互异且值未变；1s 心跳、40ms 歌词周期、400ms 兜底未被改。
    #[test]
    fn qa_timer_ids_and_periods_are_unchanged() {
        let ids = [
            ("GEOM", widget_wnd::TIMER_GEOM),
            ("ANIM", widget_wnd::TIMER_ANIM),
            ("CLICK", widget_wnd::TIMER_CLICK),
            ("LYRIC", widget_wnd::TIMER_LYRIC),
            ("RESUME", widget_wnd::TIMER_RESUME),
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i].1, ids[j].1, "{} 与 {} 定时器 id 冲突", ids[i].0, ids[j].0);
            }
        }
        assert_eq!(widget_wnd::TIMER_GEOM, 1);
        assert_eq!(widget_wnd::TIMER_ANIM, 2);
        assert_eq!(widget_wnd::TIMER_CLICK, 3);
        assert_eq!(widget_wnd::TIMER_LYRIC, 5);
        assert_eq!(widget_wnd::TIMER_RESUME, 6);
        assert_eq!(widget_wnd::TIMER_GEOM_MS, 1_000, "1s 心跳不许调快");
        assert_eq!(widget_wnd::TIMER_LYRIC_MS, 40, "歌词定时器周期不许被改");
        assert_eq!(RESUME_FALLBACK_MS, 400);
        assert!(RESUME_FALLBACK_MS >= 200 && RESUME_FALLBACK_MS <= 1_000);
    }

    /// 两个新标记默认 false 且互不联动（防止某处顺手把另一个也置位）。
    #[test]
    fn qa_freeze_flags_default_false_and_independent() {
        let mut st = AppState::default();
        assert!(!st.frozen);
        assert!(!st.resume_pending);
        st.frozen = true;
        assert!(!st.resume_pending, "置 frozen 不得暗改 resume_pending");
        st.resume_pending = true;
        assert!(st.frozen, "置 resume_pending 不得暗改 frozen");
    }
}
