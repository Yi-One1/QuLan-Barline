//! 布局（`ARCHITECTURE.md` §7.3 / PRD §4.1 尺寸规范）。
//!
//! **纯函数**：输入（边缘 / 形态 / 缩放 / 任务栏厚度 / 文本自然宽度），输出
//! `WidgetLayout`，不碰任何 Win32 句柄，因此可以单测。
//!
//! ## 坐标系
//!
//! 所有坐标都是**窗口客户区物理像素**。因为进程是 PER_MONITOR_AWARE_V2，
//! 1 逻辑像素 = 1 物理像素，所以「逻辑像素 × scale」直接就是客户区坐标。
//!
//! 当任务栏在左右两侧时，逻辑坐标仍是「水平布局」，由渲染层施加旋转变换
//! （§10.4，M4/T18 实现）；本模块只负责水平语义的长边/短边。
//!
//! ## 省略号缓存（M1 报告 §5.9）
//!
//! `title_text` / `artist_text` 是**已经截断好**的字符串，由
//! [`crate::render::Renderer::prepare_text`] 在「文本或宽度变化」时填一次。
//! `text_key` 是这次填充的指纹；painter 只读字符串，不建 `IDWriteTextLayout`。

use crate::config::defaults::limits;

/// 浮点矩形（窗口客户区坐标）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// 空矩形（宽高为 0）。
    pub const EMPTY: RectF = RectF::new(0.0, 0.0, 0.0, 0.0);

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    pub fn inset(&self, d: f32) -> Self {
        Self::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// 以自身中心为基准，向外扩展到至少 `side × side`。
    pub fn expanded_to(&self, side: f32) -> Self {
        let w = self.w.max(side);
        let h = self.h.max(side);
        let (cx, cy) = self.center();
        Self::new(cx - w * 0.5, cy - h * 0.5, w, h)
    }
}

/// 任务栏贴在屏幕的哪条边。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskbarEdge {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

impl TaskbarEdge {
    /// 日志友好名。
    pub fn as_str(self) -> &'static str {
        match self {
            TaskbarEdge::Bottom => "Bottom",
            TaskbarEdge::Top => "Top",
            TaskbarEdge::Left => "Left",
            TaskbarEdge::Right => "Right",
        }
    }

    /// 左右方位时内容需要旋转 90°。
    pub fn is_vertical(self) -> bool {
        matches!(self, TaskbarEdge::Left | TaskbarEdge::Right)
    }

    /// 从 `SHAppBarMessage(ABM_GETTASKBARPOS)` 的 `uEdge` 映射。
    /// `ABE_LEFT=0 / ABE_TOP=1 / ABE_RIGHT=2 / ABE_BOTTOM=3`（注意不是 `ABS_*`）。
    pub fn from_appbar_edge(u_edge: u32) -> Self {
        match u_edge {
            0 => TaskbarEdge::Left,
            1 => TaskbarEdge::Top,
            2 => TaskbarEdge::Right,
            _ => TaskbarEdge::Bottom,
        }
    }
}

/// 歌词显示模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LyricMode {
    Off,
    #[default]
    TwoLine,
    OneLine,
}

impl LyricMode {
    /// 由配置映射（配置层有自己的 serde 枚举，避免 render 类型泄漏到配置）。
    pub fn from_config(enabled: bool, one_line: bool) -> Self {
        if !enabled {
            LyricMode::Off
        } else if one_line {
            LyricMode::OneLine
        } else {
            LyricMode::TwoLine
        }
    }
}

/// 小组件形态：决定总高度（PRD §4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidgetForm {
    /// 无歌词：高度 = 任务栏高度
    Compact,
    /// 单行歌词：+26 * scale
    OneLine,
    /// 双行歌词：+46 * scale
    #[default]
    TwoLine,
}

impl WidgetForm {
    /// 歌词区额外高度（逻辑像素，未乘 scale）。
    pub fn lyric_extra_h(self) -> f32 {
        match self {
            WidgetForm::Compact => 0.0,
            WidgetForm::OneLine => limits::LYRIC_ONE_LINE_H,
            WidgetForm::TwoLine => limits::LYRIC_TWO_LINE_H,
        }
    }

    /// 由歌词模式映射。
    pub fn from_lyric_mode(mode: LyricMode) -> Self {
        match mode {
            LyricMode::Off => WidgetForm::Compact,
            LyricMode::OneLine => WidgetForm::OneLine,
            LyricMode::TwoLine => WidgetForm::TwoLine,
        }
    }
}

/// 布局结果。所有坐标都是**窗口客户区坐标**（物理像素，已乘 scale）。
#[derive(Debug, Clone, Default)]
pub struct WidgetLayout {
    pub form: WidgetForm,
    pub edge: TaskbarEdge,
    pub scale: f32,
    /// 逻辑宽高（已含 scale）
    pub width: i32,
    pub height: i32,
    /// 主行区域（封面 + 标题 + 按钮）
    pub main_row: RectF,
    pub cover: RectF,
    pub title: RectF,
    pub artist: RectF,
    pub btn_prev: RectF,
    pub btn_toggle: RectF,
    pub btn_next: RectF,
    /// 进度条（悬停 4px，否则 2px；这里给的是悬停态完整矩形）
    pub progress: RectF,
    /// 歌词当前行 / 下一行（form == Compact 时二者为空矩形）
    pub lyric_cur: RectF,
    pub lyric_next: RectF,
    /// 命中区（比视觉区大，保证 ≥ 28×28 * scale）
    pub hit_prev: RectF,
    pub hit_toggle: RectF,
    pub hit_next: RectF,
    pub hit_progress: RectF,
    pub hit_cover: RectF,

    // ---- 文本缓存（M1 报告 §5.9：截断结果缓存进布局，避免每帧建 TextLayout）----
    /// 已截断的歌名（含省略号）
    pub title_text: String,
    /// 已截断的歌手名
    pub artist_text: String,
    /// 文本缓存指纹。0 表示「还没算过」。
    pub text_key: u64,
}

impl WidgetLayout {
    /// 进度条在当前交互态下的实际高度（静止 2px / 悬停 4px）。
    pub fn progress_height(&self, hovering: bool) -> f32 {
        let s = self.effective_scale();
        if hovering {
            limits::PROGRESS_H_HOVER * s
        } else {
            limits::PROGRESS_H_IDLE * s
        }
    }

    /// 进度条在给定高度下的矩形（底部对齐，居中对齐到 `progress` 的中线）。
    pub fn progress_rect(&self, h: f32) -> RectF {
        let bottom = self.progress.bottom();
        RectF::new(self.progress.x, bottom - h, self.progress.w, h)
    }

    pub fn effective_scale(&self) -> f32 {
        if self.scale > 0.0 {
            self.scale
        } else {
            1.0
        }
    }

    /// 内容自然宽度（不含宽度钳位），用于日志与调试。
    pub fn fixed_width(&self) -> f32 {
        let s = self.effective_scale();
        limits::CONTENT_PAD * s * 2.0
            + limits::COVER_SIDE * s
            + limits::COVER_TEXT_GAP * s
            + limits::TEXT_BTN_GAP * s
            + (limits::BTN_HIT * 3.0 + limits::BTN_GAP * 2.0) * s
    }
}

/// 计算布局（不带文本自然宽度，等价于「文本宽 0」→ 宽度取下限）。
///
/// 保留这个 4 参数签名是为了让 M1 的单测与调用点零改动；
/// 正式路径用 [`compute_ex`]。
pub fn compute(edge: TaskbarEdge, form: WidgetForm, scale: f32, bar_thickness: i32) -> WidgetLayout {
    compute_ex(edge, form, scale, bar_thickness, 0.0)
}

/// 计算布局。
///
/// * `edge`          —— 任务栏方位
/// * `form`          —— 形态（决定高度）
/// * `scale`         —— DPI 缩放系数（96dpi → 1.0）
/// * `bar_thickness` —— 任务栏条带厚度（物理像素）
/// * `text_w`        —— 歌名 / 歌手中较宽者的自然宽度（物理像素，未截断）
///
/// **纵向排布与任务栏方位绑定**：主行（封面 / 文本 / 按钮 / 进度条）始终与任务栏
/// 条带重合——底部任务栏时贴客户区底边，顶部任务栏时贴顶边；歌词区排在主行
/// 朝屏幕内侧的一边。
pub fn compute_ex(
    edge: TaskbarEdge,
    form: WidgetForm,
    scale: f32,
    bar_thickness: i32,
    text_w: f32,
) -> WidgetLayout {
    let s = if scale > 0.0 { scale } else { 1.0 };

    let pad = limits::CONTENT_PAD * s;
    let cover_side = limits::COVER_SIDE * s;
    let gap_ct = limits::COVER_TEXT_GAP * s;
    let gap_tb = limits::TEXT_BTN_GAP * s;
    let btn = limits::BTN_HIT * s;
    let bgap = limits::BTN_GAP * s;
    let btn_group = btn * 3.0 + bgap * 2.0;

    // 宽度 = clamp(自然宽度, 200*s, 340*s)
    let fixed = pad * 2.0 + cover_side + gap_ct + gap_tb + btn_group;
    let want = (fixed + text_w.max(0.0)).round();
    let width = want
        .max((limits::WIDGET_MIN_W * s).round())
        .min((limits::WIDGET_MAX_W * s).round()) as i32;

    let main_h = bar_thickness.max(1);
    let lyric_extra = (form.lyric_extra_h() * s).round() as i32;
    let height = main_h + lyric_extra;

    let w = width as f32;
    let mh = main_h as f32;

    // ---- 纵向排布：主行必须与任务栏条带重合 ----
    //
    // 底部任务栏时窗口向上扩展（见 `taskbar::geometry::place_widget`），
    // 客户区底边才对齐任务栏底边，所以**主行要贴客户区底边**，
    // 多出来的歌词区落在上方桌面上；顶部任务栏时正好相反。
    // 若这里固定 `main_top = 0`，开启歌词后封面 / 歌名 / 按钮会整体浮到
    // 任务栏上方的桌面上，而歌词反倒压在任务栏条带里（视觉上完全错位）。
    let main_top = if edge == TaskbarEdge::Bottom {
        lyric_extra as f32
    } else {
        0.0
    };
    // 歌词区起点：底部任务栏在上方，顶部任务栏在下方。
    let lyric_top = if edge == TaskbarEdge::Bottom {
        0.0
    } else {
        mh
    };

    // 主行：整条任务栏高度，左右各留 pad
    let main_row = RectF::new(pad, main_top, (w - pad * 2.0).max(1.0), mh);

    // 封面 32×32（×scale），在主行内垂直居中
    let cover = RectF::new(
        pad,
        main_top + ((mh - cover_side) * 0.5).max(0.0),
        cover_side,
        cover_side,
    );

    // 按钮组：右对齐
    let btn_y = main_top + ((mh - btn) * 0.5).max(0.0);
    let btn_next = RectF::new(w - pad - btn, btn_y, btn, btn);
    let btn_toggle = RectF::new(btn_next.x - bgap - btn, btn_y, btn, btn);
    let btn_prev = RectF::new(btn_toggle.x - bgap - btn, btn_y, btn, btn);

    // 进度条：主行底部，留 2*s 的内边距
    let ph = limits::PROGRESS_H_HOVER * s;
    let progress = RectF::new(
        pad,
        main_top + (mh - ph - 2.0 * s).max(0.0),
        (w - pad * 2.0).max(1.0),
        ph,
    );

    // 文本块：封面右侧 → 按钮组左侧；垂直范围到进度条上沿
    let text_x = cover.right() + gap_ct;
    let text_right = (btn_prev.x - gap_tb).max(text_x + 1.0);
    let text_w_avail = (text_right - text_x).max(1.0);
    let text_h = (progress.y - main_top).max(1.0);
    let cy = main_top + text_h * 0.5;
    let title_h = (limits::TITLE_PX * 1.24 * s).max(1.0);
    let artist_h = (limits::ARTIST_PX * 1.24 * s).max(1.0);
    let title = RectF::new(text_x, cy - title_h, text_w_avail, title_h);
    let artist = RectF::new(text_x, cy, text_w_avail, artist_h);

    // 歌词区
    let (lyric_cur, lyric_next) = match form {
        WidgetForm::Compact => (RectF::EMPTY, RectF::EMPTY),
        WidgetForm::OneLine => (
            RectF::new(pad, lyric_top, (w - pad * 2.0).max(1.0), 26.0 * s),
            RectF::EMPTY,
        ),
        WidgetForm::TwoLine => (
            RectF::new(
                pad,
                lyric_top + 2.0 * s,
                (w - pad * 2.0).max(1.0),
                24.0 * s,
            ),
            RectF::new(
                pad,
                lyric_top + 26.0 * s,
                (w - pad * 2.0).max(1.0),
                20.0 * s,
            ),
        ),
    };

    // 命中区：按钮本身就是 28×28（≥ HIT_MIN），封面可能只有 32（也 ≥ 28），
    // 但为了 DPI 缩放下也不低于门槛，统一过一次 expanded_to。
    let hit_min = limits::HIT_MIN * s;
    let hit_cover = cover.expanded_to(hit_min);
    // 进度条命中区：整宽，从「按钮命中区下沿 + 1」到主行底部（避免抢按钮的点击）
    let main_bottom = main_top + mh;
    let prog_hit_top = (btn_y + btn + 1.0)
        .max(progress.y - 1.0)
        .min((main_bottom - 2.0).max(0.0));
    let hit_progress = RectF::new(0.0, prog_hit_top, w, (main_bottom - prog_hit_top).max(2.0));

    WidgetLayout {
        form,
        edge,
        scale: s,
        width,
        height,
        main_row,
        cover,
        title,
        artist,
        btn_prev,
        btn_toggle,
        btn_next,
        progress,
        lyric_cur,
        lyric_next,
        hit_prev: btn_prev.expanded_to(hit_min),
        hit_toggle: btn_toggle.expanded_to(hit_min),
        hit_next: btn_next.expanded_to(hit_min),
        hit_progress,
        hit_cover,
        title_text: String::new(),
        artist_text: String::new(),
        text_key: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_scales_with_dpi() {
        let a = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        let b = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.25, 60);
        assert_eq!(a.width, 200);
        assert_eq!(b.width, 250);
    }

    #[test]
    fn compact_height_equals_bar_thickness() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        assert_eq!(l.height, 48);
    }

    #[test]
    fn lyric_forms_add_height() {
        let compact = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        let one = compute(TaskbarEdge::Bottom, WidgetForm::OneLine, 1.0, 48);
        let two = compute(TaskbarEdge::Bottom, WidgetForm::TwoLine, 1.0, 48);
        assert_eq!(one.height - compact.height, 26);
        assert_eq!(two.height - compact.height, 46);
    }

    #[test]
    fn cover_inside_main_row_and_vertically_centered() {
        let l = compute(TaskbarEdge::Top, WidgetForm::Compact, 1.0, 48);
        assert!(l.cover.x >= l.main_row.x);
        assert!(l.cover.right() <= l.main_row.right());
        assert!(l.cover.bottom() <= l.main_row.bottom());
        let (_, cy) = l.cover.center();
        assert!((cy - 24.0).abs() < 0.001);
    }

    #[test]
    fn title_starts_right_of_cover() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        assert!(l.title.x > l.cover.right());
        assert!(l.title.right() <= l.btn_prev.x + 0.001);
    }

    #[test]
    fn zero_scale_falls_back_to_one() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 0.0, 40);
        assert_eq!(l.scale, 1.0);
        assert_eq!(l.width, 200);
    }

    #[test]
    fn appbar_edge_mapping() {
        assert_eq!(TaskbarEdge::from_appbar_edge(0), TaskbarEdge::Left);
        assert_eq!(TaskbarEdge::from_appbar_edge(1), TaskbarEdge::Top);
        assert_eq!(TaskbarEdge::from_appbar_edge(2), TaskbarEdge::Right);
        assert_eq!(TaskbarEdge::from_appbar_edge(3), TaskbarEdge::Bottom);
        assert_eq!(TaskbarEdge::from_appbar_edge(99), TaskbarEdge::Bottom);
    }

    /// 宽度自适应：极短歌名取下限 200，长歌名不超过 340。
    #[test]
    fn width_clamps_between_min_and_max() {
        let short = compute_ex(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0);
        assert_eq!(short.width, 200);
        let long = compute_ex(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 4000.0);
        assert_eq!(long.width, 340);
        let mid = compute_ex(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 60.0);
        assert!(mid.width > 200 && mid.width < 340, "mid.width = {}", mid.width);
    }

    /// T06 要求的纯函数单测：三种形态 × 两档 DPI 下，
    /// 视觉元素互不重叠、命中区 ≥ 28*scale。
    #[test]
    fn rects_do_not_overlap_and_hits_meet_minimum() {
        for form in [WidgetForm::Compact, WidgetForm::OneLine, WidgetForm::TwoLine] {
            for (scale, bar) in [(1.0f32, 48i32), (1.25, 60)] {
                let l = compute_ex(TaskbarEdge::Bottom, form, scale, bar, 120.0);
                let hit_min = limits::HIT_MIN * scale;

                // 命中区尺寸
                for (name, r) in [
                    ("hit_prev", l.hit_prev),
                    ("hit_toggle", l.hit_toggle),
                    ("hit_next", l.hit_next),
                    ("hit_cover", l.hit_cover),
                ] {
                    assert!(
                        r.w >= hit_min - 0.01 && r.h >= hit_min - 0.01,
                        "{form:?}@{scale} {name} = {}×{} < {hit_min}",
                        r.w,
                        r.h
                    );
                }
                assert!(l.hit_progress.h >= 2.0);

                // 三个按钮互不重叠
                assert!(l.btn_prev.right() <= l.btn_toggle.x + 0.01);
                assert!(l.btn_toggle.right() <= l.btn_next.x + 0.01);

                // 封面不与文本、按钮重叠
                assert!(l.cover.right() <= l.title.x + 0.01);
                assert!(l.title.right() <= l.btn_prev.x + 0.01);

                // 文本不与进度条重叠
                assert!(l.artist.bottom() <= l.progress.y + 0.01);

                // 进度条在主行内
                assert!(l.progress.bottom() <= l.main_row.bottom() + 0.01);

                // 歌词区在主行之外，且方向随任务栏方位变化
                if form != WidgetForm::Compact {
                    if l.edge == TaskbarEdge::Bottom {
                        // 底部任务栏：歌词区在上，主行贴客户区底边
                        assert!(l.lyric_cur.y >= -0.01);
                        assert!(l.lyric_cur.bottom() <= l.main_row.y + 0.01);
                    } else {
                        assert!(l.lyric_cur.y >= l.main_row.bottom() - 0.01);
                        assert!(l.lyric_cur.bottom() <= l.height as f32 + 0.01);
                    }
                } else {
                    assert!(l.lyric_cur.is_empty());
                }

                // 命中区不出窗口
                assert!(l.hit_next.right() <= l.width as f32 + 0.01);
                assert!(l.hit_progress.bottom() <= l.main_row.bottom() + 0.01);
            }
        }
    }

    /// 底部任务栏：主行贴客户区底边（与任务栏条带重合），歌词区在上方。
    #[test]
    fn bottom_edge_keeps_main_row_on_the_taskbar_strip() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::TwoLine, 1.0, 48);
        assert_eq!(l.height, 94);
        assert!((l.main_row.y - 46.0).abs() < 0.001, "main_row.y = {}", l.main_row.y);
        assert!((l.main_row.bottom() - 94.0).abs() < 0.001);
        assert!(l.cover.y >= 46.0, "封面必须落在任务栏条带内");
        assert!(l.btn_toggle.y >= 46.0, "按钮必须落在任务栏条带内");
        assert!(l.progress.y >= 46.0, "进度条必须落在任务栏条带内");
        assert!((l.lyric_cur.y - 2.0).abs() < 0.001);
        assert!((l.lyric_next.y - 26.0).abs() < 0.001);
    }

    /// 顶部任务栏：主行贴客户区顶边，歌词区在下方。
    #[test]
    fn top_edge_keeps_main_row_on_the_taskbar_strip() {
        let l = compute(TaskbarEdge::Top, WidgetForm::TwoLine, 1.0, 48);
        assert!((l.main_row.y).abs() < 0.001);
        assert!((l.main_row.bottom() - 48.0).abs() < 0.001);
        assert!((l.lyric_cur.y - 50.0).abs() < 0.001);
    }

    /// 紧凑形态（无歌词）两种方位下主行都从 0 开始，高度 = 任务栏厚度。
    #[test]
    fn compact_form_has_no_vertical_offset() {
        for edge in [TaskbarEdge::Bottom, TaskbarEdge::Top] {
            let l = compute(edge, WidgetForm::Compact, 1.0, 48);
            assert!((l.main_row.y).abs() < 0.001);
            assert_eq!(l.height, 48);
            assert!((l.hit_progress.bottom() - 48.0).abs() < 0.001);
        }
    }

    /// 进度条高度切换（PRD：静止 2px，悬停 4px）。
    #[test]
    fn progress_height_toggles() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        assert_eq!(l.progress_height(false), 2.0);
        assert_eq!(l.progress_height(true), 4.0);
        let idle = l.progress_rect(l.progress_height(false));
        let hover = l.progress_rect(l.progress_height(true));
        // 底部对齐
        assert!((idle.bottom() - hover.bottom()).abs() < 0.01);
        assert!(hover.y < idle.y);
    }

    #[test]
    fn lyric_mode_maps_to_form() {
        assert_eq!(WidgetForm::from_lyric_mode(LyricMode::Off), WidgetForm::Compact);
        assert_eq!(WidgetForm::from_lyric_mode(LyricMode::OneLine), WidgetForm::OneLine);
        assert_eq!(WidgetForm::from_lyric_mode(LyricMode::TwoLine), WidgetForm::TwoLine);
        assert_eq!(LyricMode::from_config(false, false), LyricMode::Off);
        assert_eq!(LyricMode::from_config(true, true), LyricMode::OneLine);
        assert_eq!(LyricMode::from_config(true, false), LyricMode::TwoLine);
    }
}
