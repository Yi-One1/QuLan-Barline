//! 交互修饰态状态机（PRD §5.3 机器 C / T08）。
//!
//! ## 四态
//!
//! ```text
//! Idle ──进入──► Hover ──按进度条──► Seeking ──松开──► Hover ──离开──► Idle
//!                 │                                        ▲
//!                 └──滚轮──► VolumeHud ──1.2s 无操作───────┘
//! ```
//!
//! ## 三条误触保护（PRD §4.3「通用误触保护」）
//!
//! 1. **mousedown 与 mouseup 必须命中同一元素**才触发；
//! 2. **拖动进度条期间屏蔽所有按钮**（`hit_test` 在 seeking 态只返回 Progress，
//!    外加 `SetCapture` 让指针跑出窗口也不丢事件）；
//! 3. **双击不绑定动作**：单击动作延迟 250ms 才执行；这 250ms 内收到第二下
//!    就取消，于是双击 = 零动作。
//!
//! ## 淡入淡出
//!
//! `hover_alpha` 0→1 用 120ms、1→0 用 200ms（PRD P0-07）。
//! 由 16ms 定时器推进；到达目标值后 `animating()` 变 false，
//! `App` 据此停掉定时器——这是「空闲 CPU ≈ 0」的一部分。

use std::time::Instant;

use crate::app::HoverState;
use crate::config::defaults::limits;
use crate::event::WidgetInput;
use crate::render::layout::WidgetLayout;
use crate::ui::hit::{hit_test, seek_fraction, HitTarget};

/// 按下时的现场。
#[derive(Debug, Clone, Copy)]
struct DownState {
    target: HitTarget,
    x: f32,
    y: f32,
}

/// 已排队、等 250ms 判定的单击。
#[derive(Debug, Clone, Copy)]
struct PendingClick {
    target: HitTarget,
    at: Instant,
}

/// 交互状态机。
pub struct Interaction {
    pub hover: HoverState,
    /// 悬停背景不透明度 0.0..=1.0
    pub hover_alpha: f32,
    /// 当前按下的按钮（用于画按下态）
    pub pressed: Option<HitTarget>,
    down: Option<DownState>,
    pending: Option<PendingClick>,
    seeking: bool,
    /// 音量 HUD 剩余毫秒
    pub hud_left_ms: u32,
    /// `WH_MOUSE_LL` 钩子是否已挂
    pub wheel_hooked: bool,
    /// 是否需要 App 启动 / 停止 16ms 动画定时器
    animating: bool,
    /// 需要 App 启动 250ms 单击判定定时器
    click_timer_wanted: bool,
}

impl Default for Interaction {
    fn default() -> Self {
        Self::new()
    }
}

impl Interaction {
    pub fn new() -> Self {
        Self {
            hover: HoverState::Idle,
            hover_alpha: 0.0,
            pressed: None,
            down: None,
            pending: None,
            seeking: false,
            hud_left_ms: 0,
            wheel_hooked: false,
            animating: false,
            click_timer_wanted: false,
        }
    }

    /// 强制复位（小组件被隐藏 / 切歌 / 会话消失时调用，PRD §5.3 跨状态约束）。
    pub fn reset(&mut self) {
        self.hover = HoverState::Idle;
        self.hover_alpha = 0.0;
        self.pressed = None;
        self.down = None;
        self.pending = None;
        self.seeking = false;
        self.hud_left_ms = 0;
        self.animating = false;
        self.click_timer_wanted = false;
    }

    /// 是否还在动画中（App 据此决定 16ms 定时器的开停）。
    pub fn animating(&self) -> bool {
        self.animating
    }

    /// 是否需要启动 250ms 单击判定定时器（取走即清除）。
    pub fn take_click_timer_wanted(&mut self) -> bool {
        let v = self.click_timer_wanted;
        self.click_timer_wanted = false;
        v
    }

    /// 是否有待判定的单击。
    pub fn has_pending_click(&self) -> bool {
        self.pending.is_some()
    }

    /// 是否处于拖动进度条状态。
    pub fn is_seeking(&self) -> bool {
        self.seeking
    }

    // ------------------------------------------------------------ 鼠标进入 / 移动

    /// `WM_MOUSEMOVE`。
    pub fn on_move(&mut self, layout: &WidgetLayout, x: f32, y: f32) -> Vec<WidgetInput> {
        let mut out = Vec::new();

        if self.hover == HoverState::Idle {
            self.hover = HoverState::Hover;
            self.animating = true;
            out.push(WidgetInput::HoverEnter);
        }

        if self.seeking {
            let frac = seek_fraction(layout, x);
            out.push(WidgetInput::SeekPreview(frac));
        }

        let _ = y;
        out
    }

    /// `WM_MOUSELEAVE`。
    pub fn on_leave(&mut self) -> Vec<WidgetInput> {
        // 拖动中指针跑出窗口不结束拖动（有 SetCapture，事件仍归我们）
        if self.seeking {
            return Vec::new();
        }
        if self.hover == HoverState::Idle {
            return Vec::new();
        }
        self.hover = HoverState::Idle;
        self.hud_left_ms = 0;
        self.animating = true;
        vec![WidgetInput::HoverLeave]
    }

    // ------------------------------------------------------------ 按下 / 抬起

    /// `WM_LBUTTONDOWN`。返回是否需要 `SetCapture`。
    pub fn on_down(&mut self, layout: &WidgetLayout, x: f32, y: f32) -> (Vec<WidgetInput>, bool) {
        let target = hit_test(layout, x, y, self.seeking);

        // 拖动中：只更新预览，不产生任何按钮动作
        if self.seeking {
            return (vec![WidgetInput::SeekPreview(seek_fraction(layout, x))], true);
        }

        // 双击检测：250ms 内同一元素第二下 → 取消待判定单击，什么都不做
        if let Some(p) = self.pending {
            if p.target == target && p.at.elapsed().as_millis() as u64 <= limits::DOUBLE_CLICK_MS {
                self.pending = None;
                self.down = None;
                self.pressed = None;
                crate::log_debug!("双击 {} → 取消单击动作（PRD §4.3）", target.as_str());
                return (vec![], false);
            }
        }

        self.down = Some(DownState { target, x, y });

        if target == HitTarget::Progress {
            // 进度条：立刻进入 Seeking（单击也要能跳转），并捕获鼠标
            self.seeking = true;
            self.hover = HoverState::Seeking;
            self.pressed = None;
            let frac = seek_fraction(layout, x);
            return (vec![WidgetInput::SeekBegin, WidgetInput::SeekPreview(frac)], true);
        }

        self.pressed = if target.is_button() || target == HitTarget::Cover {
            Some(target)
        } else {
            None
        };
        (Vec::new(), target != HitTarget::None)
    }

    /// `WM_LBUTTONUP`。返回 `(inputs, need_release_capture)`。
    pub fn on_up(&mut self, layout: &WidgetLayout, x: f32, y: f32) -> (Vec<WidgetInput>, bool) {
        let Some(d) = self.down.take() else {
            self.pressed = None;
            return (vec![], false);
        };
        self.pressed = None;

        if self.seeking {
            self.seeking = false;
            self.hover = HoverState::Hover;
            let frac = seek_fraction(layout, x);
            return (vec![WidgetInput::SeekCommit(frac)], true);
        }

        let up_target = hit_test(layout, x, y, false);
        let moved = ((x - d.x).powi(2) + (y - d.y).powi(2)).sqrt();
        let drag_limit = limits::SEEK_DRAG_PX * layout.effective_scale();

        // ① mousedown / mouseup 必须在同一元素
        if up_target != d.target || moved > drag_limit {
            crate::log_debug!(
                "点击取消：down={} up={} moved={moved:.1}",
                d.target.as_str(),
                up_target.as_str()
            );
            return (vec![], true);
        }

        // ② 只有按钮与封面产生点击
        if !(d.target.is_button() || d.target == HitTarget::Cover) {
            return (vec![], true);
        }

        // ③ 排队等 250ms 判定（双击保护）
        self.pending = Some(PendingClick {
            target: d.target,
            at: Instant::now(),
        });
        self.click_timer_wanted = true;
        (vec![], true)
    }

    /// 250ms 单击判定到点：把待判定单击转成语义化输入。
    pub fn on_click_timer(&mut self) -> Vec<WidgetInput> {
        let Some(p) = self.pending.take() else {
            return Vec::new();
        };
        let input = match p.target {
            HitTarget::Prev => WidgetInput::ClickPrev,
            HitTarget::Toggle => WidgetInput::ClickPlayPause,
            HitTarget::Next => WidgetInput::ClickNext,
            HitTarget::Cover => WidgetInput::ClickCover,
            _ => return Vec::new(),
        };
        crate::log_debug!("单击确认：{}", p.target.as_str());
        vec![input]
    }

    /// `WM_CAPTURECHANGED`（用户 Alt+Tab 走了 / 被别的窗口抢走）。
    pub fn on_capture_lost(&mut self) -> Vec<WidgetInput> {
        self.pressed = None;
        self.down = None;
        if self.seeking {
            self.seeking = false;
            self.hover = HoverState::Hover;
            crate::log_debug!("捕获丢失，取消拖动");
            return vec![WidgetInput::SeekCancel];
        }
        Vec::new()
    }

    // ------------------------------------------------------------ 滚轮

    /// 滚轮 ±1 格。返回 `Wheel` 输入并重置 HUD 计时。
    pub fn on_wheel(&mut self, delta: i32, hud_ms: u32) -> Vec<WidgetInput> {
        // 拖动中吞掉滚轮（PRD §4.3：Seeking 态屏蔽滚轮）
        if self.seeking {
            return Vec::new();
        }
        self.hover = HoverState::VolumeHud;
        self.hud_left_ms = hud_ms.max(200);
        self.animating = true;
        vec![WidgetInput::Wheel { delta }]
    }

    // ------------------------------------------------------------ 定时器

    /// 16ms 动画定时器。返回 HUD 过期事件（如有）。
    pub fn on_anim_tick(&mut self, dt_ms: u32) -> Vec<WidgetInput> {
        let mut out = Vec::new();

        // 1. 悬停淡入淡出
        let target = if self.hover == HoverState::Idle {
            0.0
        } else {
            1.0
        };
        if (self.hover_alpha - target).abs() > 0.001 {
            let fade_ms = if target > self.hover_alpha {
                limits::HOVER_FADE_IN_MS
            } else {
                limits::HOVER_FADE_OUT_MS
            };
            let step = dt_ms as f32 / fade_ms as f32;
            if target > self.hover_alpha {
                self.hover_alpha = (self.hover_alpha + step).min(1.0);
            } else {
                self.hover_alpha = (self.hover_alpha - step).max(0.0);
            }
        } else {
            self.hover_alpha = target;
        }

        // 2. 音量 HUD 倒计时
        if self.hud_left_ms > 0 {
            self.hud_left_ms = self.hud_left_ms.saturating_sub(dt_ms);
            if self.hud_left_ms == 0 {
                // 指针还在里面就回 Hover，否则回 Idle（PRD §5.3）
                if self.hover == HoverState::VolumeHud {
                    self.hover = HoverState::Hover;
                }
                out.push(WidgetInput::VolumeHudExpired);
            }
        }

        self.animating = (self.hover_alpha - target).abs() > 0.001 || self.hud_left_ms > 0;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::layout::{compute, TaskbarEdge, WidgetForm};

    fn layout() -> WidgetLayout {
        compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48)
    }

    #[test]
    fn enter_and_leave_toggle_hover_state() {
        let l = layout();
        let mut i = Interaction::new();
        let out = i.on_move(&l, 10.0, 10.0);
        assert_eq!(out, vec![WidgetInput::HoverEnter]);
        assert_eq!(i.hover, HoverState::Hover);
        assert!(i.animating());

        let out = i.on_leave();
        assert_eq!(out, vec![WidgetInput::HoverLeave]);
        assert_eq!(i.hover, HoverState::Idle);
    }

    /// 指针在小组件内部移动不重复触发 HoverEnter（PRD §4.3）。
    #[test]
    fn internal_moves_do_not_retrigger_enter() {
        let l = layout();
        let mut i = Interaction::new();
        i.on_move(&l, 10.0, 10.0);
        for x in [20.0f32, 40.0, 80.0, 120.0] {
            assert!(i.on_move(&l, x, 10.0).is_empty(), "x={x} 重复触发了进入");
        }
    }

    /// 淡入 120ms / 淡出 200ms。
    #[test]
    fn fade_times_match_spec() {
        let l = layout();
        let mut i = Interaction::new();
        i.on_move(&l, 10.0, 10.0);

        // 120ms / 16ms ≈ 8 帧
        let mut frames = 0;
        while i.hover_alpha < 1.0 && frames < 100 {
            i.on_anim_tick(16);
            frames += 1;
        }
        assert!((6..=9).contains(&frames), "淡入用了 {frames} 帧");

        i.on_leave();
        let mut frames = 0;
        while i.hover_alpha > 0.0 && frames < 200 {
            i.on_anim_tick(16);
            frames += 1;
        }
        assert!((11..=14).contains(&frames), "淡出用了 {frames} 帧");
        assert!(!i.animating(), "淡出结束后必须停掉动画定时器");
    }

    /// 单击延迟 250ms 才产生动作；双击产生零动作。
    #[test]
    fn single_click_is_delayed_then_fires() {
        let l = layout();
        let mut i = Interaction::new();
        let (cx, cy) = l.btn_toggle.center();
        i.on_down(&l, cx, cy);
        i.on_up(&l, cx, cy);
        assert!(i.has_pending_click());
        assert!(i.take_click_timer_wanted());
        assert_eq!(i.on_click_timer(), vec![WidgetInput::ClickPlayPause]);
        assert!(!i.has_pending_click());
    }

    #[test]
    fn double_click_produces_no_action() {
        let l = layout();
        let mut i = Interaction::new();
        let (cx, cy) = l.btn_next.center();

        i.on_down(&l, cx, cy);
        i.on_up(&l, cx, cy);
        assert!(i.has_pending_click());

        // 第二下（250ms 内）
        let (inputs, _) = i.on_down(&l, cx, cy);
        assert!(inputs.is_empty());
        assert!(!i.has_pending_click(), "双击必须取消待判定单击");
        let (inputs, _) = i.on_up(&l, cx, cy);
        assert!(inputs.is_empty());
        assert!(i.on_click_timer().is_empty());
    }

    /// 在 A 按下、B 松开 → 不触发（PRD §4.3）。
    #[test]
    fn down_and_up_on_different_targets_does_nothing() {
        let l = layout();
        let mut i = Interaction::new();
        let (px, py) = l.btn_prev.center();
        let (nx, ny) = l.btn_next.center();
        i.on_down(&l, px, py);
        let (inputs, _) = i.on_up(&l, nx, ny);
        assert!(inputs.is_empty());
        assert!(!i.has_pending_click());
    }

    /// PRD P0-10 核心：拖进度条经过按钮区不产生任何按钮输入。
    #[test]
    fn dragging_progress_never_triggers_buttons() {
        let l = layout();
        let mut i = Interaction::new();
        let py = l.hit_progress.y + l.hit_progress.h * 0.5;

        let (inputs, capture) = i.on_down(&l, 40.0, py);
        assert!(capture);
        assert!(inputs.contains(&WidgetInput::SeekBegin));
        assert!(i.is_seeking());

        // 拖过三个按钮的位置
        for x in [l.btn_prev.x, l.btn_toggle.x, l.btn_next.x] {
            let out = i.on_move(&l, x + 5.0, py);
            for input in out {
                assert!(
                    matches!(input, WidgetInput::SeekPreview(_)),
                    "拖动期间产生了非 SeekPreview 输入：{input:?}"
                );
            }
        }

        let (inputs, _) = i.on_up(&l, l.btn_next.x + 5.0, py);
        assert_eq!(inputs.len(), 1);
        assert!(matches!(inputs[0], WidgetInput::SeekCommit(_)));
        assert!(!i.is_seeking());
    }

    #[test]
    fn capture_loss_cancels_seek() {
        let l = layout();
        let mut i = Interaction::new();
        let py = l.hit_progress.y + l.hit_progress.h * 0.5;
        i.on_down(&l, 40.0, py);
        let out = i.on_capture_lost();
        assert_eq!(out, vec![WidgetInput::SeekCancel]);
        assert!(!i.is_seeking());
    }

    #[test]
    fn wheel_enters_hud_and_expires() {
        let l = layout();
        let mut i = Interaction::new();
        i.on_move(&l, 10.0, 10.0);
        let out = i.on_wheel(120, 1200);
        assert_eq!(out, vec![WidgetInput::Wheel { delta: 120 }]);
        assert_eq!(i.hover, HoverState::VolumeHud);

        // 连续滚动重置计时
        i.on_anim_tick(600);
        i.on_wheel(120, 1200);
        assert_eq!(i.hud_left_ms, 1200);

        // 走完 1200ms
        let mut got = Vec::new();
        for _ in 0..80 {
            got.extend(i.on_anim_tick(16));
        }
        assert!(got.contains(&WidgetInput::VolumeHudExpired));
        assert_eq!(i.hover, HoverState::Hover);
    }

    #[test]
    fn wheel_is_ignored_while_seeking() {
        let l = layout();
        let mut i = Interaction::new();
        let py = l.hit_progress.y + l.hit_progress.h * 0.5;
        i.on_down(&l, 40.0, py);
        assert!(i.on_wheel(120, 1200).is_empty());
        assert_eq!(i.hover, HoverState::Seeking);
    }

    #[test]
    fn reset_clears_everything() {
        let l = layout();
        let mut i = Interaction::new();
        i.on_move(&l, 10.0, 10.0);
        i.on_wheel(120, 1200);
        i.reset();
        assert_eq!(i.hover, HoverState::Idle);
        assert_eq!(i.hover_alpha, 0.0);
        assert!(!i.is_seeking());
        assert!(!i.has_pending_click());
        assert!(!i.animating());
    }
}
