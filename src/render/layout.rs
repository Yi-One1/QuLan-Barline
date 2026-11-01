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
//! ## v1.1.1：单行大字歌词 + 歌手名右侧
//!
//! 小组件**高度恒等于任务栏条带厚度**（歌词开/关都不变）。信息区分两行：
//! **上行 = 歌名 + 歌手名（同排横向，歌手紧跟歌名右侧）**，
//! **下行 = 单行歌词**（只画当前行，逐字扫光，字号尽量大）。
//! 信息区宽 = max(歌名+歌手自然宽, 歌词最小宽 160px)，随开关动画插值；
//! v1.1 的「右侧 160px 歌词块 + 双行压缩」方案自 v1.1.1 起作废。
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

/// 小组件形态：决定**信息区最小宽度**（v1.1 起不再决定高度——高度恒为条带厚度；
/// v1.1.1 起行数固定单行，歌词区不再区分单/双行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidgetForm {
    /// 无歌词：信息区宽 = 歌名+歌手自然宽（不加长）
    #[default]
    Compact,
    /// 歌词开启：信息区宽至少 160 * scale，下行显示当前行歌词
    Lyric,
}

impl WidgetForm {
    /// 信息区最小宽度（逻辑像素，未乘 scale）。`Compact`（歌词关闭）为 0。
    pub fn lyric_w(self) -> f32 {
        match self {
            WidgetForm::Compact => 0.0,
            WidgetForm::Lyric => limits::LYRIC_W,
        }
    }

    /// 由「歌词是否启用」映射。
    pub fn from_lyric_enabled(enabled: bool) -> Self {
        if enabled {
            WidgetForm::Lyric
        } else {
            WidgetForm::Compact
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
    /// 主行区域（封面 + 文本 + 按钮）
    pub main_row: RectF,
    pub cover: RectF,
    /// 上行歌名（歌手名在其右侧，见 [`WidgetLayout::artist`]）
    pub title: RectF,
    /// 上行歌手名（紧跟歌名右侧；自然宽超过信息区一半时截断省略）
    pub artist: RectF,
    pub btn_prev: RectF,
    pub btn_toggle: RectF,
    pub btn_next: RectF,
    /// 进度条（悬停 4px，否则 2px；这里给的是悬停态完整矩形）
    pub progress: RectF,
    /// 下行单行歌词（`form == Compact` 或宽度动画尚未展开时为空矩形）
    pub lyric_cur: RectF,
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

    /// 内容自然宽度（不含宽度钳位，含歌词最小宽），用于日志与调试。
    pub fn fixed_width(&self) -> f32 {
        let s = self.effective_scale();
        limits::CONTENT_PAD * s * 2.0
            + limits::COVER_SIDE * s
            + limits::COVER_TEXT_GAP * s
            + limits::TEXT_BTN_GAP * s
            + (limits::BTN_HIT * 3.0 + limits::BTN_GAP * 2.0) * s
            + self.form.lyric_w() * s
    }
}

/// 计算布局（不带文本自然宽度，等价于「文本宽 0」→ 宽度取下限）。
///
/// 保留这个 4 参数签名是为了让既有单测与调用点零改动；
/// 正式路径用 [`compute_ex`]。
pub fn compute(edge: TaskbarEdge, form: WidgetForm, scale: f32, bar_thickness: i32) -> WidgetLayout {
    compute_ex(edge, form, scale, bar_thickness, 0.0, 0.0)
}

/// 计算布局（信息区最小宽取形态默认值：Compact→0 / Lyric→160 * scale）。
///
/// * `edge`        —— 任务栏方位
/// * `form`        —— 形态（决定信息区最小宽；高度恒为条带厚度）
/// * `scale`       —— DPI 缩放系数（96dpi → 1.0）
/// * `bar_thickness` —— 任务栏条带厚度（物理像素）
/// * `title_w`     —— 歌名自然宽度（物理像素，未截断）
/// * `artist_w`    —— 歌手自然宽度（物理像素，未截断）
pub fn compute_ex(
    edge: TaskbarEdge,
    form: WidgetForm,
    scale: f32,
    bar_thickness: i32,
    title_w: f32,
    artist_w: f32,
) -> WidgetLayout {
    compute_full(edge, form, scale, bar_thickness, title_w, artist_w, form.lyric_w())
}

/// 计算布局，并**显式指定信息区最小宽**（逻辑像素，未乘 scale）。
///
/// 存在的唯一理由是歌词开关的 200ms 宽度缓动：动画期间每帧的最小宽是
/// 插值出来的中间值，不等于任何 `WidgetForm` 的默认值。
/// `compute_ex` 就是「最小宽 = 形态默认值」的特例。
pub fn compute_full(
    edge: TaskbarEdge,
    form: WidgetForm,
    scale: f32,
    bar_thickness: i32,
    title_w: f32,
    artist_w: f32,
    lyric_w_logical: f32,
) -> WidgetLayout {
    let s = if scale > 0.0 { scale } else { 1.0 };

    let pad = limits::CONTENT_PAD * s;
    let cover_side = limits::COVER_SIDE * s;
    let gap_ct = limits::COVER_TEXT_GAP * s;
    let gap_tb = limits::TEXT_BTN_GAP * s;
    let btn = limits::BTN_HIT * s;
    let bgap = limits::BTN_GAP * s;
    let btn_group = btn * 3.0 + bgap * 2.0;
    let ta_gap = limits::TITLE_ARTIST_GAP * s;

    // 信息区最小宽（歌词开关动画的插值中间值），物理像素
    let lyric_w_px = (lyric_w_logical.max(0.0) * s).round();

    // 信息区宽 = max(歌名 + 间隔 + 歌手自然宽, 信息区最小宽)，钳到 340*s 上限。
    // v1.1.1：上行 = 歌名 + 歌手（同排），下行 = 单行歌词，两行共享同一信息区宽。
    let aw_px = artist_w.max(0.0) * s;
    let text_nat = title_w.max(0.0) * s + if aw_px > 0.5 { ta_gap + aw_px } else { 0.0 };
    let info_w = text_nat
        .max(lyric_w_px)
        .round()
        .clamp(0.0, (limits::WIDGET_MAX_W * s).round());

    // 总宽 = 固定件(边距+封面+间距+三键) + 信息区宽，整体下限 200*s
    let fixed = pad * 2.0 + cover_side + gap_ct + gap_tb + btn_group;
    let width = (((fixed + info_w) as i32).max((limits::WIDGET_MIN_W * s).round() as i32)).max(1);

    // v1.1：高度恒为任务栏条带厚度，主行与客户区完全重合。
    let main_h = bar_thickness.max(1);
    let height = main_h;

    let w = width as f32;
    let mh = main_h as f32;

    // 主行：整条任务栏高度，左右各留 pad
    let main_row = RectF::new(pad, 0.0, (w - pad * 2.0).max(1.0), mh);

    // 封面 32×32（×scale），在主行内垂直居中
    let cover = RectF::new(pad, ((mh - cover_side) * 0.5).max(0.0), cover_side, cover_side);

    // 按钮组：右对齐
    let btn_y = ((mh - btn) * 0.5).max(0.0);
    let btn_next = RectF::new(w - pad - btn, btn_y, btn, btn);
    let btn_toggle = RectF::new(btn_next.x - bgap - btn, btn_y, btn, btn);
    let btn_prev = RectF::new(btn_toggle.x - bgap - btn, btn_y, btn, btn);

    // 进度条：主行底部，留 2*s 的内边距
    let ph = limits::PROGRESS_H_HOVER * s;
    let progress = RectF::new(pad, (mh - ph - 2.0 * s).max(0.0), (w - pad * 2.0).max(1.0), ph);

    // ---- 两行结构（48px = 2 + 22 + 22 + 2 @100% DPI）----
    // 上行：歌名 + 歌手名（同排横向）；下行：单行歌词。
    // 行高随条带厚度自适应收缩（每行 ≤ 厚度一半），保证任何厚度下都不出条带。
    let row_h = (limits::LYRIC_LINE_H * s).min((mh * 0.5).max(1.0));
    let row1_y = ((mh - row_h * 2.0) * 0.5).max(0.0);
    let row2_y = row1_y + row_h;

    // 文本区横向范围：封面右侧 → 上一首按钮左侧
    let text_x = cover.right() + gap_ct;
    let text_right = (btn_prev.x - gap_tb).max(text_x + 1.0);
    let info_avail = (text_right - text_x).max(1.0);
    let info = info_w.min(info_avail).max(1.0);

    // 歌手名：按自然宽放置，钳到信息区一半（超宽省略，歌名永远占主导）
    let artist_w_px = if aw_px > 0.5 { aw_px.min(info * 0.5) } else { 0.0 };
    let title_w_px = (info - artist_w_px - if artist_w_px > 0.0 { ta_gap } else { 0.0 }).max(1.0);
    let title = RectF::new(text_x, row1_y, title_w_px, row_h);
    let artist = RectF::new(
        if artist_w_px > 0.0 {
            (text_x + title_w_px + ta_gap).min(text_right)
        } else {
            text_x
        },
        row1_y,
        artist_w_px,
        row_h,
    );

    // 下行单行歌词（v1.1.1：不再有下一行预览）
    let lyric_cur = if form == WidgetForm::Compact || lyric_w_px < 1.0 {
        RectF::EMPTY
    } else {
        RectF::new(text_x, row2_y, info, row_h)
    };

    // 命中区：按钮本身就是 28×28（≥ HIT_MIN），封面可能只有 32（也 ≥ 28），
    // 但为了 DPI 缩放下也不低于门槛，统一过一次 expanded_to。
    let hit_min = limits::HIT_MIN * s;
    let hit_cover = cover.expanded_to(hit_min);
    // 进度条命中区：整宽，从「按钮命中区下沿 + 1」到主行底部（避免抢按钮的点击）
    let main_bottom = mh;
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
    fn height_is_constant_across_all_forms() {
        // v1.1 裁决：高度恒定 = 任务栏条带厚度，歌词开/关不变
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for bar in [40i32, 48, 60] {
                let l = compute(TaskbarEdge::Bottom, form, 1.0, bar);
                assert_eq!(l.height, bar, "{form:?}@{bar}");
            }
        }
    }

    #[test]
    fn lyric_form_widens_info_area_to_min_160() {
        // 歌名+歌手自然宽 0（下限 200 生效）：关 → 200；开 → 固定件 162 + 信息区 160 = 322
        let off = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        let on = compute(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48);
        assert_eq!(off.width, 200);
        assert_eq!(on.width, 322);
        // 文本自然宽 200 > 最小宽 160：信息区 = 200 → 宽 = 162 + 200 = 362；
        // 文本自然宽顶到信息区上限 340：宽 = 162 + 340 = 502
        let mid = compute_ex(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 200.0, 0.0);
        assert_eq!(mid.width, 162 + 200);
        let wide = compute_ex(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 4000.0, 0.0);
        assert_eq!(wide.width, 162 + 340);
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

    /// 宽度自适应：极短文本取下限 200，信息区不超过 340（总宽 ≤ 162+340）。
    #[test]
    fn width_clamps_between_min_and_max() {
        let short = compute_ex(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0);
        assert_eq!(short.width, 200);
        let long = compute_ex(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 4000.0, 0.0);
        assert_eq!(long.width, 162 + 340);
        let mid = compute_ex(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 60.0, 0.0);
        assert!(mid.width > 200 && mid.width < 340, "mid.width = {}", mid.width);
    }

    /// T06 要求的纯函数单测：两种形态 × 两档 DPI 下，
    /// 视觉元素互不重叠、命中区 ≥ 28*scale。
    #[test]
    fn rects_do_not_overlap_and_hits_meet_minimum() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for (scale, bar) in [(1.0f32, 48i32), (1.25, 60)] {
                let l = compute_ex(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0);
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

                // v1.1.1：歌手名在歌名右侧（同排），不与按钮重叠
                assert!(l.artist.x >= l.title.right() - 0.01, "歌手必须在歌名右侧");
                assert!(l.artist.right() <= l.btn_prev.x + 0.01);

                // 文本不与进度条重叠（两行都在进度条上方）
                assert!(l.title.bottom() <= l.progress.y + 0.01);
                assert!(l.artist.bottom() <= l.progress.y + 0.01);

                // 进度条在主行内
                assert!(l.progress.bottom() <= l.main_row.bottom() + 0.01);

                // v1.1.1：歌词在歌名/歌手行**下方**，不出条带
                if form == WidgetForm::Lyric {
                    assert!(l.lyric_cur.w > 0.0, "歌词行必须有宽度");
                    assert!(
                        l.lyric_cur.y >= l.title.bottom() - 0.01,
                        "歌词行必须在歌名歌手行下方"
                    );
                    assert!(
                        l.lyric_cur.bottom() <= l.main_row.bottom() + 0.01,
                        "歌词行不得超出任务栏条带（bottom = {} > {}）",
                        l.lyric_cur.bottom(),
                        l.main_row.bottom()
                    );
                } else {
                    assert!(l.lyric_cur.is_empty());
                }

                // 命中区不出窗口
                assert!(l.hit_next.right() <= l.width as f32 + 0.01);
                assert!(l.hit_progress.bottom() <= l.main_row.bottom() + 0.01);
            }
        }
    }

    /// v1.1.1 两行结构：上行歌名+歌手（y=2..24），下行歌词（y=24..46），
    /// 高度恒为条带厚度，歌词行压在条带内。
    #[test]
    fn bottom_edge_keeps_main_row_on_the_taskbar_strip() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48);
        assert_eq!(l.height, 48);
        assert!(l.main_row.y.abs() < 0.001, "main_row.y = {}", l.main_row.y);
        assert!((l.main_row.bottom() - 48.0).abs() < 0.001);
        assert!(l.cover.y >= 0.0 && l.cover.bottom() <= 48.0, "封面必须落在任务栏条带内");
        assert!(l.btn_toggle.y >= 0.0 && l.btn_toggle.bottom() <= 48.0, "按钮必须落在任务栏条带内");
        assert!(l.progress.y >= 0.0 && l.progress.bottom() <= 48.0, "进度条必须落在任务栏条带内");
        // 上行（歌名 + 歌手）与下行（歌词）各 22px：y = 2 与 24
        assert!((l.title.y - 2.0).abs() < 0.001, "title.y = {}", l.title.y);
        assert!((l.artist.y - 2.0).abs() < 0.001, "artist.y = {}", l.artist.y);
        assert!((l.lyric_cur.y - 24.0).abs() < 0.001, "lyric_cur.y = {}", l.lyric_cur.y);
        assert!((l.lyric_cur.bottom() - 46.0).abs() < 0.001);
        assert!((l.lyric_cur.w - 160.0).abs() < 0.001, "lyric_cur.w = {}", l.lyric_cur.w);
    }

    /// 顶部任务栏：主行同样从 0 开始（歌词不向桌面方向扩展）。
    #[test]
    fn top_edge_keeps_main_row_on_the_taskbar_strip() {
        let l = compute(TaskbarEdge::Top, WidgetForm::Lyric, 1.0, 48);
        assert!(l.main_row.y.abs() < 0.001);
        assert!((l.main_row.bottom() - 48.0).abs() < 0.001);
        assert!((l.lyric_cur.y - 24.0).abs() < 0.001);
        assert!(l.lyric_cur.bottom() <= 48.0, "歌词行不得超出条带");
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
    fn lyric_enabled_maps_to_form() {
        assert_eq!(WidgetForm::from_lyric_enabled(false), WidgetForm::Compact);
        assert_eq!(WidgetForm::from_lyric_enabled(true), WidgetForm::Lyric);
    }

    /// 歌词开关的 200ms 宽度过渡：中间宽度下两行结构仍然互不越界，
    /// 且高度保持条带厚度不变（宽度动画取代旧的高度动画）。
    #[test]
    fn width_animation_intermediate_stays_inside_strip() {
        for w_logical in [0.0f32, 20.0, 80.0, 120.0, 160.0] {
            let l = compute_full(
                TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 60.0, 30.0, w_logical,
            );
            assert_eq!(l.height, 48, "宽度动画期间高度不得变化");
            assert!(l.artist.x >= l.title.right() - 0.01, "歌手不得压住歌名（w={w_logical}）");
            assert!(l.artist.right() <= l.btn_prev.x + 0.01, "歌手不得压住按钮（w={w_logical}）");
            assert!(l.lyric_cur.bottom() <= 48.0, "歌词行不得出条带（w={w_logical}）");
        }
        // 起点 / 终点：0 = 完全收合（空歌词行），160 = 信息区最小宽完全展开
        let closed = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 0.0, 0.0, 0.0);
        assert!(closed.lyric_cur.is_empty());
        let open = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 0.0, 0.0, 160.0);
        assert!((open.lyric_cur.w - 160.0).abs() < 0.001);
    }

    /// v1.1.1 布局：上行 = 歌名 + 歌手（歌名右侧），下行 = 单行歌词，无下一行。
    #[test]
    fn lyric_row_sits_below_title_and_artist_row() {
        let l = compute_ex(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 120.0, 60.0);
        // 歌手紧跟歌名右侧
        assert!(l.artist.x >= l.title.right(), "歌手必须在歌名右侧");
        assert!(l.artist.w > 0.0, "有歌手名时歌手区必须有宽度");
        // 歌词行在两行文本的下方
        assert!(l.lyric_cur.y >= l.title.bottom() - 0.01);
        assert!(l.lyric_cur.y >= l.artist.bottom() - 0.01);
        assert!(!l.lyric_cur.is_empty());
    }

    /// 歌名过长时歌手名仍可见（钳到信息区一半），且两者不重叠。
    #[test]
    fn long_title_keeps_artist_visible_on_same_row() {
        let l = compute_ex(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 4000.0, 80.0);
        assert!(l.artist.w > 0.0, "歌名再长歌手区也不能被挤没了");
        assert!(l.artist.w <= l.title.w + 0.01, "歌手区不得超过歌名区（歌名主导）");
        assert!(l.artist.x >= l.title.right() - 0.01);
        assert!(l.title.right() <= l.artist.x + 0.01);
    }

    /// 歌词关闭时歌词行完全收合。
    #[test]
    fn off_form_has_no_lyric_rect() {
        let l = compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48);
        assert!(l.lyric_cur.is_empty());
        assert_eq!(l.width, 200);
    }
}
