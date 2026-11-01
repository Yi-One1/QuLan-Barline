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
    KillTimer, MessageBoxW, PostQuitMessage, SetTimer, MB_ICONINFORMATION, MB_SETFOREGROUND,
    MB_TOPMOST,
};

use crate::config::{Config, LyricModeSerde};
use crate::event::{AppEvent, Effect, MediaCommand, WidgetInput};
use crate::i18n::{self, Lang};
use crate::lyric::LyricView;
use crate::media::{self, MediaService, Ms, NowPlaying, PlaybackState, RepeatMode};
use crate::platform::dpi::scale_for;
use crate::platform::wake::{EventSink, WakeHwnd};
use crate::platform::wide::to_wide;
use crate::render::layout::{self, LyricMode, WidgetForm, WidgetLayout};
use crate::render::sampler::{CoverageMap, SampleRequest, Sampler};
use crate::render::text::TextKind;
use crate::render::theme::{BackgroundMode, BlurBuffer, Theme};
use crate::render::Renderer;
use crate::taskbar::geometry::{self, TaskbarGeometry};
use crate::taskbar::host::TaskbarHost;
use crate::ui::hit::HitTarget;
use crate::ui::interaction::Interaction;
use crate::ui::menu::{self, MenuCommand};
use crate::ui::widget_wnd::{self, TIMER_ANIM, TIMER_BLUR, TIMER_CLICK};

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
    /// 当前按下的按钮（M2 新增：画按下态）
    pub pressed: Option<HitTarget>,
    /// 用户是否隐藏（PRD 机器 B 门控 1）
    pub user_hidden: bool,
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
    /// 模糊背景（仅 Hover 且 mode=Blur 时非 None）
    pub blur: Option<Arc<BlurBuffer>>,
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
    renderer: Option<Renderer>,
    media: Option<MediaService>,
    sampler: Option<Sampler>,
    geometry: Option<TaskbarGeometry>,
    /// 小组件当前的屏幕矩形（由 `relayout` 算好，`redraw` 直接用）
    target_rect: RECT,
    interact: Interaction,
    started: Instant,
    /// 上一帧的 alpha 覆盖率图（交给采样线程剔除自遮挡）
    last_coverage: Option<Arc<CoverageMap>>,
    /// 动画 / 模糊定时器当前是否已挂
    anim_timer_on: bool,
    blur_timer_on: bool,
    /// 系统控件提示是否已弹过（本次进程内）
    /// 上一轮几何探测是否失败（避免日志刷屏）
    geom_failed: bool,
}

impl App {
    /// 启动期组装：配置 → 窗口类 / 消息窗 → 事件通道 → 渲染器 → 采样线程
    /// → 小组件窗口 → 媒体线程。
    ///
    /// 返回裸指针（不是 `Box`）：WndProc 需要在 `GWLP_USERDATA` 里存它，
    /// 而 `Box` 一旦移动地址就变。生命周期由 `main` 通过 [`App::shutdown`] 收尾。
    pub fn bootstrap() -> windows::core::Result<*mut App> {
        let config = Config::load();
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

        let app = Box::new(App {
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
            renderer,
            media: None,
            sampler,
            geometry,
            target_rect,
            interact: Interaction::new(),
            started: Instant::now(),
            last_coverage: None,
            anim_timer_on: false,
            blur_timer_on: false,
            geom_failed: false,
        });

        let ptr = Box::into_raw(app);

        // 绑定 App* 到两个窗口的 GWLP_USERDATA
        crate::ui::widget_wnd::bind_app(msg_hwnd, ptr);
        if let Some(w) = unsafe { (*ptr).widget.as_ref() } {
            crate::ui::widget_wnd::bind_app(w.hwnd(), ptr);
        }

        // 启动媒体线程（MTA）。失败不阻塞启动。
        unsafe {
            (*ptr).media = MediaService::spawn(sink);
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
    }

    /// 1 秒定时器：几何兜底探测 + 乐观更新对账。
    pub fn on_timer(&mut self) {
        self.tick_optimistic();

        match geometry::probe() {
            Some(g) => {
                let changed = match self.geometry.as_ref() {
                    Some(old) => !old.same_shape(&g),
                    None => true,
                };
                let epoch_changed = self.geometry.as_ref().map(|o| o.epoch) != Some(g.epoch);

                crate::log_debug!("geometry: {}", g.summary());
                self.geometry = Some(g);
                self.geom_failed = false;

                if changed {
                    crate::log_info!("几何变化 → 重新布局：{}", g.summary());
                    self.relayout();
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
        self.ensure_on_top();

        // 背景兜底重采样（PRD P0-07「窗口移动时 300ms 内刷新」的近似）。
        //
        // 壁纸 / 主题变化有 `WM_SETTINGCHANGE` 精确通知，但**用户把窗口拖到小组件后面**
        // 不会有任何通知；只靠悬停才采样的话，静止态会一直用着旧背景算出来的文字颜色
        // （深色窗口滑到浅色壁纸后面 → 白字压在浅色上，直接看不清）。
        // 这里按 1s 心跳补一次；采样线程自身有 300ms 节流，抓的只是小组件大小的
        // 一小块像素，空闲 CPU 仍是 0 量级（实测 < 0.3%）。
        if self.state.visible && !self.is_hovering() {
            self.request_backdrop(false);
        }
    }

    /// `WM_SETTINGCHANGE` / `WM_THEMECHANGED`：壁纸或主题变了 → 立即重新采样。
    ///
    /// PRD P0-07 要求「切换壁纸后 300ms 内完成黑白切换」，采样线程本身有 300ms 节流，
    /// 所以这里必须**立刻**投递请求，不能等 1 秒心跳。
    pub fn on_setting_changed(&mut self) {
        crate::log_debug!("系统设置 / 主题变化 → 重新采样背景");
        self.request_backdrop(self.is_hovering());
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
        self.relayout();
        self.ensure_on_top();
    }

    /// `WM_DISPLAYCHANGE`：分辨率 / DPI / 显示器增删。
    pub fn on_display_changed(&mut self) {
        crate::log_info!("WM_DISPLAYCHANGE：重建渲染目标");
        self.geometry = None;
        self.last_coverage = None;
        self.relayout();
    }

    /// 第二个实例请求唤起（M3 起打开设置页）。
    pub fn on_second_instance(&mut self) {
        crate::log_info!("第二个实例请求唤起：设置页在 M3/T13 落地，当前仅重绘小组件");
        self.relayout();
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
                } else if id == TIMER_BLUR {
                    if self.is_hovering() {
                        self.request_backdrop(true);
                    } else {
                        self.kill_widget_timer(TIMER_BLUR);
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// 16ms 动画定时器。
    fn on_anim_timer(&mut self) {
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
        if redraw || self.interact.animating() {
            self.redraw();
        }
    }

    // ---------------------------------------------------------------- reduce / apply

    /// 纯函数：事件 → 副作用列表。
    pub fn reduce(&mut self, evt: AppEvent) -> Vec<Effect> {
        match evt {
            AppEvent::NowPlaying(np) => {
                let track_changed = self.state.playing.track_id != np.track_id;
                self.state.playing = *np;
                self.state.optimistic = None;
                if track_changed {
                    // PRD §5.3：切歌强制退出 Seeking / VolumeHUD
                    self.interact.reset();
                    self.sync_interaction();
                }
                vec![Effect::Relayout]
            }
            AppEvent::SessionCleared => {
                crate::log_info!("会话已清空，小组件隐藏");
                self.state.playing = NowPlaying::default();
                self.state.blur = None;
                self.interact.reset();
                self.sync_interaction();
                vec![Effect::Relayout]
            }
            AppEvent::BackdropReady {
                epoch,
                theme,
                blur,
            } => {
                // 代次不匹配 → 丢弃（尺寸 / 位置已变，模糊图对不上）
                if epoch != self.state.geometry_epoch {
                    crate::log_debug!(
                        "丢弃过期背景（epoch {epoch} ≠ {}）",
                        self.state.geometry_epoch
                    );
                    return Vec::new();
                }
                crate::log_debug!(
                    "主题更新：luma={:.3} on_light={} contrast={:.2} blur={}",
                    theme.backdrop_luma,
                    theme.on_light,
                    theme.text_contrast(),
                    blur.is_some()
                );
                // 只有主题或模糊缓冲**真的变了**才重绘。
                //
                // 1s 心跳会周期性重新采样（见 `on_timer`），背景静止时结果完全一样；
                // 无脑重绘等于每秒白跑一帧 D2D + WIC 回读 + UpdateLayeredWindow，
                // 空闲 CPU 会从 ~0 涨到 0.5%（PRD P0-14 的硬指标就是 0.5%）。
                // 模糊缓冲每次都是新的 `Arc`，所以用指针相等判断内容是否更新。
                let blur_changed = match (blur.as_ref(), self.state.blur.as_ref()) {
                    (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
                    (None, None) => false,
                    _ => true,
                };
                let changed = !theme.same_paint(&self.state.theme) || blur_changed;
                self.state.theme = theme;
                self.state.blur = blur;
                if changed {
                    vec![Effect::Redraw]
                } else {
                    Vec::new()
                }
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
                self.relayout();
                Vec::new()
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

    /// `WidgetInput` 的 15 个分支。
    fn reduce_input(&mut self, input: WidgetInput) -> Vec<Effect> {
        match input {
            WidgetInput::HoverEnter => {
                self.request_backdrop(self.config.appearance.background_mode == BackgroundMode::Blur);
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
                self.commit_config()
            }
            WidgetInput::SetLyricMode(mode) => {
                self.config.lyrics.enabled = mode != LyricMode::Off;
                self.config.lyrics.mode = if mode == LyricMode::OneLine {
                    LyricModeSerde::OneLine
                } else {
                    LyricModeSerde::TwoLine
                };
                self.commit_config()
            }
            WidgetInput::SetBackgroundMode(mode) => {
                self.config.appearance.background_mode = mode;
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
    fn on_wheel(&mut self, delta: i32) -> Vec<Effect> {
        if !self.config.volume.wheel_enabled {
            return Vec::new();
        }
        let step = self.config.volume.step.clamp(0.01, 0.05);
        let dir = if delta > 0 { 1.0 } else { -1.0 };
        let base = media::wasapi::system_volume().unwrap_or(self.state.volume_hud);
        let next = (base + dir * step).clamp(0.0, 1.0);
        let ok = media::wasapi::set_system_volume(next);
        self.state.volume_hud = next;
        crate::log_debug!(
            "滚轮 delta={delta} → 音量 {:.0}%（设备写入={ok}）",
            next * 100.0
        );
        vec![Effect::Redraw]
    }

    /// 执行副作用。
    pub fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Relayout => self.relayout(),
                Effect::Redraw => self.redraw(),
                Effect::SetVisible(v) => {
                    if let Some(h) = self.widget.as_mut() {
                        h.set_visible(v);
                    }
                }
                Effect::Media(cmd) => match self.media.as_ref() {
                    Some(m) => m.send(cmd),
                    None => crate::log_warn!("媒体线程未启动，命令 {cmd:?} 丢弃"),
                },
                Effect::SaveConfig(cfg) => {
                    if let Err(e) = cfg.save() {
                        crate::log_err!("保存配置失败：{e}");
                    }
                }
                Effect::RequestBackdrop { .. } => self.request_backdrop(self.is_hovering()),
                Effect::DropBackdrop => {
                    self.state.blur = None;
                    self.redraw();
                }
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
    pub fn relayout(&mut self) {
        let Some(geo) = self.geometry else {
            // 没有几何信息（Explorer 未就绪）→ 隐藏，等下一次探测
            if let Some(h) = self.widget.as_mut() {
                h.set_visible(false);
            }
            return;
        };

        let scale = scale_for(geo.dpi);
        let bar_thickness = (geo.bar_rect.bottom - geo.bar_rect.top).max(1);
        let lyric_mode = LyricMode::from_config(
            self.config.lyrics.enabled,
            self.config.lyrics.mode == LyricModeSerde::OneLine,
        );
        // 竖排任务栏（左右）在 M2 一律降级为紧凑态：M2 还没有内容旋转
        // （`ARCHITECTURE.md` §10.4 明确「左右分支 M4 最后做，允许降级为紧凑态」）。
        // 不降级的话窗口会为歌词区多出 46px，比任务栏还宽，直接压到桌面上。
        let form = if geo.edge.is_vertical() {
            WidgetForm::Compact
        } else {
            WidgetForm::from_lyric_mode(lyric_mode)
        };

        // 文本自然宽度 → 决定总宽（200..340 * scale）
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

        let text_w = match self.renderer.as_ref() {
            Some(r) => r
                .measure(TextKind::Title, &title)
                .max(r.measure(TextKind::Artist, &artist)),
            None => 0.0,
        };

        let mut lay = layout::compute_ex(geo.edge, form, scale, bar_thickness, text_w);
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
        if let Some(h) = self.widget.as_mut() {
            if let Err(e) = h.apply_geometry(self.target_rect) {
                crate::log_warn!("应用窗口几何失败：{e}");
            }
        }

        crate::log_debug!(
            "布局：{}×{} form={:?} 文本宽={text_w:.0} target=({},{} {}×{})",
            self.state.layout.width,
            self.state.layout.height,
            form,
            self.target_rect.left,
            self.target_rect.top,
            self.target_rect.right - self.target_rect.left,
            self.target_rect.bottom - self.target_rect.top
        );

        self.redraw();
        self.sync_visibility();
        self.request_backdrop(self.is_hovering());
    }

    /// 只重绘（布局不变）。
    pub fn redraw(&mut self) {
        if self.geometry.is_none() {
            return;
        }
        let Some(host) = self.widget.as_ref() else {
            return;
        };
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let t = self.target_rect;
        match renderer.render(&self.state, self.lang) {
            Ok((hdc, w, h, coverage)) => {
                if let Err(e) = host.present(hdc, t.left, t.top, w, h) {
                    crate::log_warn!("UpdateLayeredWindow 失败：{e}");
                }
                self.last_coverage = coverage;
            }
            Err(e) => crate::log_err!("绘制失败：{e}"),
        }
    }

    /// 可见性同步（PRD 5.2 三道门控，M2 起真的生效）。
    fn sync_visibility(&mut self) {
        self.state.taskbar_collapsed = self.geometry.map(|g| g.collapsed).unwrap_or(false);
        self.state.user_hidden = self.config.general.widget_hidden;
        self.state.visible = self.state.compute_visible();
        let vis = self.state.visible;
        if let Some(h) = self.widget.as_mut() {
            h.set_visible(vis);
        }
        if !vis {
            // PRD §5.3：隐藏时所有修饰态强制复位到 Idle
            self.interact.reset();
            self.sync_interaction();
            self.kill_widget_timer(TIMER_ANIM);
            self.kill_widget_timer(TIMER_BLUR);
            widget_wnd::uninstall_wheel_hook();
            self.interact.wheel_hooked = false;
        }
    }

    /// 请求背景采样（亮度 + 可选模糊）。
    fn request_backdrop(&mut self, need_blur: bool) {
        if self.geometry.is_none() {
            return;
        }
        let Some(sampler) = self.sampler.as_ref() else {
            return;
        };
        let scale = self.state.layout.effective_scale();
        let mode = self.config.appearance.background_mode;
        let req = SampleRequest {
            epoch: self.state.geometry_epoch,
            rect: self.target_rect,
            scale,
            mode,
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
            corner_radius: 8.0 * scale,
            need_blur: need_blur && mode == BackgroundMode::Blur,
        };
        sampler.request(req);
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
        self.state.volume_hud_left_ms = self.interact.hud_left_ms;

        // 16ms 动画定时器：只在真的需要推进时存在
        if self.interact.animating() {
            if !self.anim_timer_on {
                self.start_widget_timer(TIMER_ANIM, crate::config::defaults::limits::ANIM_TIMER_MS);
            }
        } else if self.anim_timer_on {
            self.kill_widget_timer(TIMER_ANIM);
        }

        // 250ms 模糊刷新定时器：悬停 + 模糊模式
        let want_blur = self.is_hovering()
            && self.config.appearance.background_mode == BackgroundMode::Blur;
        if want_blur {
            if !self.blur_timer_on {
                self.start_widget_timer(TIMER_BLUR, widget_wnd::TIMER_BLUR_MS);
            }
        } else if self.blur_timer_on {
            self.kill_widget_timer(TIMER_BLUR);
        }

        // 250ms 单击判定定时器
        if self.interact.take_click_timer_wanted() {
            self.start_widget_timer(
                TIMER_CLICK,
                crate::config::defaults::limits::CLICK_DELAY_MS,
            );
        }

        // 淡出结束后释放模糊缓冲（PRD：淡出不重新抓屏，但结束后可以丢弃）
        if self.interact.hover == HoverState::Idle && !self.interact.animating() {
            self.state.blur = None;
        }
    }

    fn start_widget_timer(&mut self, id: usize, ms: u32) {
        let Some(h) = self.widget.as_ref() else {
            return;
        };
        // SAFETY: hwnd 有效；SetTimer 在窗口销毁时会自动清理。
        unsafe {
            let _ = SetTimer(Some(h.hwnd()), id, ms, None);
        }
        match id {
            TIMER_ANIM => self.anim_timer_on = true,
            TIMER_BLUR => self.blur_timer_on = true,
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
            TIMER_BLUR => self.blur_timer_on = false,
            _ => {}
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
        if inputs.is_empty() {
            self.sync_interaction();
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
        if needs_redraw {
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

        let cmd = menu::show(hwnd, screen, &self.state, &self.config, self.lang);
        let Some(cmd) = cmd else {
            return;
        };
        crate::log_info!("右键菜单：{}", menu::command_name(cmd));

        match cmd {
            MenuCommand::ToggleLyrics => {
                self.config.lyrics.enabled = !self.config.lyrics.enabled;
                let effects = self.commit_config();
                self.apply(effects);
            }
            MenuCommand::LyricOneLine => {
                self.config.lyrics.enabled = true;
                self.config.lyrics.mode = LyricModeSerde::OneLine;
                let effects = self.commit_config();
                self.apply(effects);
            }
            MenuCommand::LyricTwoLine => {
                self.config.lyrics.enabled = true;
                self.config.lyrics.mode = LyricModeSerde::TwoLine;
                let effects = self.commit_config();
                self.apply(effects);
            }
            MenuCommand::BgBlur => {
                self.config.appearance.background_mode = BackgroundMode::Blur;
                let effects = self.commit_config();
                self.apply(effects);
            }
            MenuCommand::BgTranslucent => {
                self.config.appearance.background_mode = BackgroundMode::Translucent;
                self.state.blur = None;
                let effects = self.commit_config();
                self.apply(effects);
            }
            MenuCommand::ToggleShuffle => {
                let next = !self.state.playing.shuffle;
                self.apply(vec![Effect::Media(MediaCommand::Shuffle(next))]);
            }
            MenuCommand::CycleRepeat => {
                let next = match self.state.playing.repeat {
                    RepeatMode::None => RepeatMode::List,
                    RepeatMode::List => RepeatMode::Track,
                    RepeatMode::Track => RepeatMode::None,
                };
                self.apply(vec![Effect::Media(MediaCommand::Repeat(next))]);
            }
            MenuCommand::OpenSettings => {
                crate::log_info!("打开设置页：M3/T13 落地，当前记录请求");
            }
            MenuCommand::ShowAbout => self.show_about(),
            MenuCommand::Quit => self.apply(vec![Effect::Quit]),
        }
    }

    /// 配置改动后：重排 + 重绘 + 落盘。返回要执行的 Effect。
    fn commit_config(&mut self) -> Vec<Effect> {
        self.relayout();
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
}
