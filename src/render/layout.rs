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
//! ## v1.1.2：紧凑排列 + 歌词行上移
//!
//! 歌名/歌手**紧凑排列**（歌手左缘 = 歌名右缘 + 固定间隔，不再从右端锚定；
//! 只有紧凑总宽超出信息区上限才截断）。下行歌词整体上移 [`limits::LYRIC_LIFT`]。
//! 长歌词滚动（跑马灯）的时间状态机在 `painter` 侧，布局只提供视口矩形。
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
    /// 右上角关闭 × 的**字形**矩形（边长 [`limits::DISMISS_ICON`] * scale，居中于命中区）
    pub dismiss: RectF,
    /// 右上角关闭 × 的**命中**矩形（边长 [`limits::DISMISS_HIT`] * scale，贴右边缘、y 从 0）
    pub hit_dismiss: RectF,

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
    ///
    /// v1.5.2：左侧仍只有一份 `CONTENT_PAD`（右边距改由
    /// [`limits::DISMISS_RIGHT_MARGIN`] 承担）。
    /// v1.5.3：× 那一列 = 右边距 4 + 命中 16 + 净距 **2** = 22（v1.5.2 是 28），
    /// 故固定件从 182 收到 176（= v1.4.4 的 162 + 14）。
    /// v1.5.4：命中 16 → **14**（字形 8 → 6），这一列 = 4 + 14 + 2 = **20**，
    /// 固定件 176 → **174**（= v1.4.4 的 162 + 12）。
    /// 这里必须与 [`compute_full`] 的 `fixed` 严格同源，否则日志里的调试宽度会
    /// 与真实窗口对不上。
    pub fn fixed_width(&self) -> f32 {
        let s = self.effective_scale();
        limits::CONTENT_PAD * s
            + limits::COVER_SIDE * s
            + limits::COVER_TEXT_GAP * s
            + limits::TEXT_BTN_GAP * s
            + (limits::BTN_HIT * 3.0 + limits::BTN_GAP * 2.0) * s
            + (limits::DISMISS_RIGHT_MARGIN + limits::DISMISS_HIT + limits::DISMISS_GAP) * s
            + self.form.lyric_w() * s
    }

    /// 关闭 × 在**悬停 / 按下**时那层极淡圆角底的矩形（v1.5.1）。
    ///
    /// 为什么要有这个方法而不是在 `painter` 里直接拿 `hit_dismiss`：
    /// 「底色不许越界、不许压到 next」是一条**几何契约**，只有把它做成
    /// 布局侧的纯函数，`layout::tests` 才能在无渲染目标的情况下写死断言
    /// （`painter` 的绘制需要 D2D 上下文，单测跑不了）。
    ///
    /// 返回值语义上恒等于 `hit_dismiss`：底色就是那一格，**不**向外扩、
    /// **不**做 1px 内缩（内缩会让 6px 字形在 0x14 的淡底上看起来偏位）。
    pub fn dismiss_bg_rect(&self) -> RectF {
        self.hit_dismiss
    }
}

/// 计算布局，并**显式指定信息区最小宽**（逻辑像素，未乘 scale）。
///
/// 这是**唯一**的布局入口（v1.8.1 / G6 清理）：过去另有 `compute`（4 参数）与
/// `compute_ex`（6 参数）两个便利包装，但它们的调用点在生产代码里已经归零
/// （`app.rs` 直接调本函数），只剩单测在用——等于给测试开了一条与线上不同的
/// 入口。两个包装层删除后，单测显式传「文本宽 0/实测宽 + 形态默认信息区宽」，
/// 与线上走**同一条**路径。
///
/// `lyric_w_logical` 之所以要显式传：歌词开关有 200ms 宽度缓动，动画期间每帧的
/// 信息区最小宽是插值出来的中间值，不等于任何 `WidgetForm` 的默认值
/// （`WidgetForm::lyric_w()`）。
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

    // v1.1.2 紧凑排列：歌名自然宽 + 固定间隔 + 歌手自然宽，歌手**不从右侧锚定**。
    // 信息区宽 = max(紧凑总宽, 信息区最小宽)，钳到 340*s 上限；
    // 只有紧凑总宽超出上限（放不下）时才截断：歌手钳到一半、歌名占其余并省略。
    // v1.1.11（问题 10）：**ceil 而不是 round**——矩形宽必须 ≥ 实测自然宽，
    // 否则 prepare_text 的省略判定（measure ≤ max_w）会被 0.x px 的取整损失
    // 误判为「放不下」，把放得下的歌名截成「主…」。
    let title_nat = (title_w.max(0.0) * s).ceil();
    let artist_nat = (artist_w.max(0.0) * s).ceil();
    let text_nat = title_nat + if artist_nat > 0.5 { ta_gap + artist_nat } else { 0.0 };
    let info_w = text_nat
        .max(lyric_w_px)
        .round()
        .clamp(0.0, (limits::WIDGET_MAX_W * s).round());

    // 总宽 = 固定件(左边距+封面+间距+三键+关闭×列) + 信息区宽，整体下限 200*s
    //
    // v1.5.0（G3）：右上角常驻 × 独占「右边缘的固定列」——命中 24 + 净距 8 = 32*s，
    // 播放三键因此整体左移，窗口总宽相应 +32*s（信息区宽度不变，用户看到的是
    // 「三键左移、右侧多出一个 ×」而不是「文字被挤窄」）。
    // v1.5.1：命中区 24 → 16，这列缩到 16 + 8 = 24*s，窗口总宽增量随之变成 +24*s。
    // v1.5.2（用户反馈「右侧留空太多」）：右边距不再复用 `CONTENT_PAD`，
    // 改由 `DISMISS_RIGHT_MARGIN = 4` 承担 —— 4 + 字形居中余量 4 = 字形右缘
    // 距窗口右缘 8px，与左侧 `CONTENT_PAD` 完全对称。这列因此 = 4 + 16 + 8 = 28*s。
    // v1.5.3（用户反馈「× 左侧的按键和内容可以右移」）：`DISMISS_GAP` 8 → 2，
    // 这列收到 4 + 16 + 2 = **22*s**，固定件 182 → **176**（= v1.4.4 的 162 + 14）。
    // v1.5.4（用户反馈「× 大小改为 6px」）：`DISMISS_HIT` 16 → 14，这列再收到
    // 4 + 14 + 2 = **20*s**，固定件 176 → **174**（= v1.4.4 的 162 + 12）。
    // 净距 2 与右距 4 一个字没动 —— 用户要求的两条视觉量（图标↔× 12px、
    // 字形右缘↔窗口右缘 8px）都靠这两个不变值 + 新的居中余量 4 维持。
    //
    // 两种场景表现不同，这正是用户 v1.5.3 要的「右移」，v1.5.4 再右移 2px：
    //   * 窗口被 `WIDGET_MIN_W` 钳住时（无文本 / 极短文本）：窗口宽度不变，
    //     三键整体右移（v1.5.3 +6px，v1.5.4 再 +2px），吃掉原来信息区那块空白；
    //   * 正常有文本时：窗口再窄 2px，三键**绝对位置不动**，右边界左收 2px。
    let dismiss_col = (limits::DISMISS_RIGHT_MARGIN + limits::DISMISS_HIT + limits::DISMISS_GAP) * s;
    let fixed = pad + cover_side + gap_ct + gap_tb + btn_group + dismiss_col;
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

    // 按钮组：右对齐，但**右边缘让出关闭 × 的整列**（右边距 4 + 命中 14 + 净距 2 = 20）
    let btn_y = ((mh - btn) * 0.5).max(0.0);
    let btn_next = RectF::new(w - dismiss_col - btn, btn_y, btn, btn);
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

    // v1.1.2 紧凑排列：放得下时歌名/歌手各取自然宽，歌手紧跟歌名右侧（无锚定空隙）；
    // 只有放不下才截断——歌手钳到信息区一半（保证可见），歌名占其余并省略。
    let (title_w_px, artist_w_px) = if text_nat <= info {
        (title_nat.max(1.0), artist_nat)
    } else {
        let a = if artist_nat > 0.5 { artist_nat.min(info * 0.5) } else { 0.0 };
        let t = (info - a - if a > 0.5 { ta_gap } else { 0.0 }).max(1.0);
        (t, a)
    };
    let title = RectF::new(text_x, row1_y, title_w_px, row_h);
    let artist = RectF::new(
        if artist_w_px > 0.5 {
            (text_x + title_w_px + ta_gap).min(text_right)
        } else {
            text_x
        },
        row1_y,
        artist_w_px,
        row_h,
    );

    // 下行单行歌词（v1.1.1：不再有下一行预览）。
    // v1.1.2：整体上移 LYRIC_LIFT，视觉靠近歌名行但不贴死；矩形上移后底部
    // 仍留在条带内（bottom 只会更小）。
    let lyric_cur = if form == WidgetForm::Compact || lyric_w_px < 1.0 {
        RectF::EMPTY
    } else {
        let y = (row2_y - limits::LYRIC_LIFT * s).max(0.0);
        RectF::new(text_x, y, info, row_h)
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

    // ---- 右上角关闭 ×（v1.5.0 / G3；v1.5.1 缩到 16/8；v1.5.2 右距收到 4；
    //      v1.5.3 净距收到 2；v1.5.4 命中 14 / 字形 6）----
    // 命中区贴右上角：右缘 = 客户区右缘 − DISMISS_RIGHT_MARGIN，y 从 0 开始
    // （纵向占满 DISMISS_HIT*s）。右缘距窗口只有 4（而不是 CONTENT_PAD 的 8），
    // 加上字形在热区内的 4px 居中余量，字形右缘正好落在「窗口右缘 − 8」，
    // 与左侧 CONTENT_PAD 对称 —— 那是 v1.5.2 的验收本质，v1.5.4 一个字没动
    // （`(14 − 6) / 2` 仍然是 4）。
    // 纵向不居中是有意的——「右上角」是用户对位置的描述，贴角比居中更像常驻角标，
    // 而且条带高度（48@100%）远大于 14，居中/贴角的点击差异可以忽略。
    let dismiss_hit = limits::DISMISS_HIT * s;
    let hit_dismiss = RectF::new(
        w - limits::DISMISS_RIGHT_MARGIN * s - dismiss_hit,
        0.0,
        dismiss_hit,
        dismiss_hit,
    );
    // 字形在命中区内**居中**：6 字形 / 14 命中 → 每边 4px 余量（v1.5.4）。
    let icon = limits::DISMISS_ICON * s;
    let dismiss = RectF::new(
        hit_dismiss.x + (dismiss_hit - icon) * 0.5,
        hit_dismiss.y + (dismiss_hit - icon) * 0.5,
        icon,
        icon,
    );

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
        dismiss,
        hit_dismiss,
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
        let a = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        let b = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.25, 60, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert_eq!(a.width, 200);
        assert_eq!(b.width, 250);
    }

    #[test]
    fn compact_height_equals_bar_thickness() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert_eq!(l.height, 48);
    }

    #[test]
    fn height_is_constant_across_all_forms() {
        // v1.1 裁决：高度恒定 = 任务栏条带厚度，歌词开/关不变
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for bar in [40i32, 48, 60] {
                let l = compute_full(TaskbarEdge::Bottom, form, 1.0, bar, 0.0, 0.0, form.lyric_w());
                assert_eq!(l.height, bar, "{form:?}@{bar}");
            }
        }
    }

    #[test]
    fn lyric_form_widens_info_area_to_min_160() {
        // 歌名+歌手自然宽 0（下限 200 生效）：关 → 200；开 → 固定件 174 + 信息区 160 = 334
        // （固定件 174 = v1.4.4 的 162 + 12：v1.5.4 的 ×「右距 4 + 命中 14 + 净距 2」）
        let off = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        let on = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 0.0, 0.0, WidgetForm::Lyric.lyric_w());
        assert_eq!(off.width, 200);
        assert_eq!(on.width, 334);
        // 文本自然宽 200 > 最小宽 160：信息区 = 200 → 宽 = 174 + 200 = 374；
        // 文本自然宽顶到信息区上限 340：宽 = 174 + 340 = 514
        let mid = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 200.0, 0.0, WidgetForm::Lyric.lyric_w());
        assert_eq!(mid.width, 174 + 200);
        let wide = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 4000.0, 0.0, WidgetForm::Lyric.lyric_w());
        assert_eq!(wide.width, 174 + 340);
    }

    #[test]
    fn cover_inside_main_row_and_vertically_centered() {
        let l = compute_full(TaskbarEdge::Top, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert!(l.cover.x >= l.main_row.x);
        assert!(l.cover.right() <= l.main_row.right());
        assert!(l.cover.bottom() <= l.main_row.bottom());
        let (_, cy) = l.cover.center();
        assert!((cy - 24.0).abs() < 0.001);
    }

    #[test]
    fn title_starts_right_of_cover() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert!(l.title.x > l.cover.right());
        assert!(l.title.right() <= l.btn_prev.x + 0.001);
    }

    #[test]
    fn zero_scale_falls_back_to_one() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 0.0, 40, 0.0, 0.0, WidgetForm::Compact.lyric_w());
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

    /// 宽度自适应：极短文本取下限 200，信息区不超过 340（总宽 ≤ 174+340）。
    #[test]
    fn width_clamps_between_min_and_max() {
        let short = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert_eq!(short.width, 200);
        let long = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 4000.0, 0.0, WidgetForm::Compact.lyric_w());
        assert_eq!(long.width, 174 + 340);
        let mid = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 60.0, 0.0, WidgetForm::Compact.lyric_w());
        assert!(mid.width > 200 && mid.width < 340, "mid.width = {}", mid.width);
    }

    /// T06 要求的纯函数单测：两种形态 × 两档 DPI 下，
    /// 视觉元素互不重叠、命中区 ≥ 28*scale。
    #[test]
    fn rects_do_not_overlap_and_hits_meet_minimum() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for (scale, bar) in [(1.0f32, 48i32), (1.25, 60)] {
                let l = compute_full(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0, form.lyric_w());
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

                // v1.1.1：歌词在歌名/歌手行**下方**（v1.1.2 允许上移 LYRIC_LIFT 的重叠），不出条带
                if form == WidgetForm::Lyric {
                    assert!(l.lyric_cur.w > 0.0, "歌词行必须有宽度");
                    assert!(
                        l.lyric_cur.y
                            >= l.title.bottom() - limits::LYRIC_LIFT * scale - 0.01,
                        "歌词行上移不得超过 LYRIC_LIFT"
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

                // v1.5.0（G3）：× 命中区贴右上角（v1.5.2 起距右缘只有
                // DISMISS_RIGHT_MARGIN），且与 next 命中区留 DISMISS_GAP 净距
                // （v1.5.3：8 → 2，仍严格不相交）
                assert!(
                    (l.hit_dismiss.right()
                        - (l.width as f32 - limits::DISMISS_RIGHT_MARGIN * scale))
                        .abs()
                        < 0.01,
                    "{form:?}@{scale} hit_dismiss.right = {} 应距右缘 DISMISS_RIGHT_MARGIN",
                    l.hit_dismiss.right()
                );
                assert!(
                    (l.hit_dismiss.x - l.hit_next.right() - limits::DISMISS_GAP * scale).abs()
                        < 0.001,
                    "{form:?}@{scale} × 与 next 净距 = {} ≠ {}（误触防线被破）",
                    l.hit_dismiss.x - l.hit_next.right(),
                    limits::DISMISS_GAP * scale
                );

                // QA 补充（G1）：`hit_dismiss` 是**唯一**刻意小于 HIT_MIN 的命中区，
                // 所以它不在上面的 ≥HIT_MIN 循环里。这个例外必须有断言兜底，否则
                // 就是「被悄悄跳过」：
                //   - 命中区恰为 14*s（不是"差不多"）；
                //   - 低于 HIT_MIN 的理由写在 `limits::DISMISS_HIT` 的注释里
                //     （× 独占右上角、与 next 有 DISMISS_GAP 净距，误触靠"不相邻"而不是放大热区；
                //     放大反而更容易压到 next 并吃掉 +12*s 的宽度预算）。
                assert!(
                    (l.hit_dismiss.w - limits::DISMISS_HIT * scale).abs() < 0.001
                        && (l.hit_dismiss.h - limits::DISMISS_HIT * scale).abs() < 0.001,
                    "{form:?}@{scale} hit_dismiss = {}×{}，应恰为 DISMISS_HIT*s = {}（刻意低于 HIT_MIN {}）",
                    l.hit_dismiss.w,
                    l.hit_dismiss.h,
                    limits::DISMISS_HIT * scale,
                    limits::HIT_MIN * scale
                );
            }
        }
    }

    /// v1.1.1 两行结构：上行歌名+歌手（y=2..24），下行歌词（y=24..46），
    /// 高度恒为条带厚度，歌词行压在条带内。
    #[test]
    fn bottom_edge_keeps_main_row_on_the_taskbar_strip() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 0.0, 0.0, WidgetForm::Lyric.lyric_w());
        assert_eq!(l.height, 48);
        assert!(l.main_row.y.abs() < 0.001, "main_row.y = {}", l.main_row.y);
        assert!((l.main_row.bottom() - 48.0).abs() < 0.001);
        assert!(l.cover.y >= 0.0 && l.cover.bottom() <= 48.0, "封面必须落在任务栏条带内");
        assert!(l.btn_toggle.y >= 0.0 && l.btn_toggle.bottom() <= 48.0, "按钮必须落在任务栏条带内");
        assert!(l.progress.y >= 0.0 && l.progress.bottom() <= 48.0, "进度条必须落在任务栏条带内");
        // 上行（歌名 + 歌手）与下行（歌词）：y = 2 与 24−LYRIC_LIFT（v1.1.3 累计上移 3.5px）
        assert!((l.title.y - 2.0).abs() < 0.001, "title.y = {}", l.title.y);
        assert!((l.artist.y - 2.0).abs() < 0.001, "artist.y = {}", l.artist.y);
        assert!((l.lyric_cur.y - (24.0 - limits::LYRIC_LIFT)).abs() < 0.001, "lyric_cur.y = {}", l.lyric_cur.y);
        assert!((l.lyric_cur.bottom() - (46.0 - limits::LYRIC_LIFT)).abs() < 0.001);
        assert!((l.lyric_cur.w - 160.0).abs() < 0.001, "lyric_cur.w = {}", l.lyric_cur.w);
    }

    /// 顶部任务栏：主行同样从 0 开始（歌词不向桌面方向扩展）。
    #[test]
    fn top_edge_keeps_main_row_on_the_taskbar_strip() {
        let l = compute_full(TaskbarEdge::Top, WidgetForm::Lyric, 1.0, 48, 0.0, 0.0, WidgetForm::Lyric.lyric_w());
        assert!(l.main_row.y.abs() < 0.001);
        assert!((l.main_row.bottom() - 48.0).abs() < 0.001);
        assert!((l.lyric_cur.y - (24.0 - limits::LYRIC_LIFT)).abs() < 0.001);
        assert!(l.lyric_cur.bottom() <= 48.0, "歌词行不得超出条带");
    }

    /// v1.1.2 核心修复：短歌名 + 短歌手时**紧凑排列**——歌手左缘 = 歌名右缘 +
    /// 固定间隔，不再锚定到信息区右端（4145b89/v1.1.1 的大空隙根因）。
    #[test]
    fn compact_spacing_artist_follows_title() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 50.0, 40.0, WidgetForm::Lyric.lyric_w());
        let gap = limits::TITLE_ARTIST_GAP;
        assert!(
            (l.artist.x - (l.title.x + l.title.w + gap)).abs() < 0.01,
            "歌手左缘必须 = 歌名右缘 + {gap}px：artist.x={} title.right={} gap={gap}",
            l.artist.x,
            l.title.right()
        );
        // 各自自然宽，不被拉伸/锚定
        assert!((l.title.w - 50.0).abs() < 0.01, "title.w = {}", l.title.w);
        assert!((l.artist.w - 40.0).abs() < 0.01, "artist.w = {}", l.artist.w);
        // 信息区宽 = 紧凑总宽与最小宽取大者：50+6+40=96 < 160 → 160
        assert!((l.lyric_cur.w - 160.0).abs() < 0.001);
    }

    /// 紧凑总宽超过信息区上限 340 时才截断：歌手钳到一半，歌名占其余（歌手仍可见）。
    #[test]
    fn overflow_truncates_but_keeps_artist_visible() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 4000.0, 200.0, WidgetForm::Lyric.lyric_w());
        assert!(l.artist.w > 0.0 && l.artist.w <= 170.0 + 0.01, "artist.w = {}", l.artist.w);
        assert!(l.artist.x >= l.title.right() - 0.01);
        assert!(l.title.w < l.artist.w * 4.0, "歌名仍占主导");
        assert!(l.title.right() <= l.btn_prev.x + 0.01);
    }

    /// v1.1.11（问题 10）：带小数的自然宽必须 ceil 而不是 round——
    /// 矩形宽一旦小于实测自然宽，`prepare_text` 的省略判定会把
    /// 放得下的歌名误截成「主…」（歌名"主动"+歌手"加木"的实际案例）。
    #[test]
    fn fractional_natural_width_is_not_truncated() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 60.4, 40.3, WidgetForm::Lyric.lyric_w());
        assert!((l.title.w - 61.0).abs() < 0.001, "title.w = {}", l.title.w);
        assert!((l.artist.w - 41.0).abs() < 0.001, "artist.w = {}", l.artist.w);
        // 矩形宽 ≥ 实测自然宽 → 省略判定（measure ≤ max_w）必然通过
        assert!(l.title.w >= 60.4);
        assert!(l.artist.w >= 40.3);
    }

    /// 歌词行上移量（v1.1.2）：y = 行位 − LYRIC_LIFT，底部仍在条带内。
    #[test]
    fn lyric_row_is_lifted_toward_title_row() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 0.0, 0.0, WidgetForm::Lyric.lyric_w());
        assert!((l.lyric_cur.y - (24.0 - limits::LYRIC_LIFT)).abs() < 0.001);
        assert!(l.lyric_cur.bottom() <= 48.0);
        assert!(l.lyric_cur.y >= l.title.y + l.title.h - limits::LYRIC_LIFT - 0.01);
    }

    /// 紧凑形态（无歌词）两种方位下主行都从 0 开始，高度 = 任务栏厚度。
    #[test]
    fn compact_form_has_no_vertical_offset() {
        for edge in [TaskbarEdge::Bottom, TaskbarEdge::Top] {
            let l = compute_full(edge, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
            assert!((l.main_row.y).abs() < 0.001);
            assert_eq!(l.height, 48);
            assert!((l.hit_progress.bottom() - 48.0).abs() < 0.001);
        }
    }

    /// 进度条高度切换（PRD：静止 2px，悬停 4px）。
    #[test]
    fn progress_height_toggles() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
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
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 120.0, 60.0, WidgetForm::Lyric.lyric_w());
        // 歌手紧跟歌名右侧（v1.1.2 紧凑排列）
        assert!(l.artist.x >= l.title.right(), "歌手必须在歌名右侧");
        assert!(l.artist.w > 0.0, "有歌手名时歌手区必须有宽度");
        // 歌词行在两行文本的下方（允许 LYRIC_LIFT 上移）
        assert!(l.lyric_cur.y >= l.title.bottom() - limits::LYRIC_LIFT - 0.01);
        assert!(l.lyric_cur.y >= l.artist.bottom() - limits::LYRIC_LIFT - 0.01);
        assert!(!l.lyric_cur.is_empty());
    }

    /// 歌名过长时歌手名仍可见（钳到信息区一半），且两者不重叠。
    #[test]
    fn long_title_keeps_artist_visible_on_same_row() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Lyric, 1.0, 48, 4000.0, 80.0, WidgetForm::Lyric.lyric_w());
        assert!(l.artist.w > 0.0, "歌名再长歌手区也不能被挤没了");
        assert!(l.artist.w <= l.title.w + 0.01, "歌手区不得超过歌名区（歌名主导）");
        assert!(l.artist.x >= l.title.right() - 0.01);
        assert!(l.title.right() <= l.artist.x + 0.01);
    }

    /// 歌词关闭时歌词行完全收合。
    #[test]
    fn off_form_has_no_lyric_rect() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert!(l.lyric_cur.is_empty());
        assert_eq!(l.width, 200);
    }

    // ------------------------------------------------- v1.5.0（G3）右上角关闭 ×
    // v1.5.1：字形 16 → 8、命中 24 → 16、笔画 1.6 → 1.0（用户反馈「× 太大」）。
    // v1.5.2：右边距从 CONTENT_PAD(8) 收到 DISMISS_RIGHT_MARGIN(4)，让字形右缘
    // 距窗口右缘 = 8（= 左侧 CONTENT_PAD），宽度增量从 +24 变成 +20。
    // v1.5.3：净距 DISMISS_GAP 8 → 2（用户反馈「× 左侧的按键和内容可以右移」），
    // 宽度增量 +20 → +14；三键尺寸/间距一个字没动，整组右移。
    // v1.5.4：字形 8 → **6**、命中 16 → **14**、笔画 1.0 → **1.2**
    // （用户反馈「大小改为 6px」「不透明度与下一首一致」），宽度增量 +14 → **+12**；
    // 净距 2 / 右距 4 / 居中余量 4 全部不变，于是「图标↔× 12px」与
    // 「字形右缘↔窗口右缘 8px」两条验收量原样保留。
    // 下面所有断言仍**写死真值**，不放宽成区间。

    /// 宽度：× 的「右距 4 + 命中 14 + 净距 2」让窗口**必须**比 v1.4.4 宽 12*s。
    ///
    /// 这里把 v1.4.4 的绝对宽度（362 / 452）**写死**而不是从公式推——这样任何人
    /// 想「优化掉」那 12px（例如偷偷把 × 叠到 next 上、或把净距 2 也吃掉）都会
    /// 立刻被这条断言拦住。
    ///
    /// 容差 0.51 而不是 0：`compute_full` 最后一步是 `(fixed + info_w) as i32`
    /// **截断**，scale=1.25 时 fixed = 174*1.25 = 217.5、info_w = 250，
    /// 和 = 467.5 截断成 467，与公式 467 差 0.5 以内。这不是放宽，是把截断误差
    /// 算进来——精确值写在下面的字面量 `assert_eq!` 里。
    ///
    /// 改名史：v1.5.0 `..._adds_32_...` → v1.5.1 `..._adds_24_...`
    /// → v1.5.2 `..._adds_20_...` → v1.5.3 `..._adds_14_...` → v1.5.4 `..._adds_12_...`。
    #[test]
    fn dismiss_column_adds_12_logical_px_to_width() {
        for (scale, width_v144) in [(1.0f32, 362.0f32), (1.25, 452.0)] {
            let bar = (48.0 * scale) as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 200.0, 0.0, WidgetForm::Compact.lyric_w());
            let want = width_v144 + 12.0 * scale;
            assert!(
                (l.width as f32 - want).abs() < 0.51,
                "scale={scale}: width = {} 应为 v1.4.4 的 {width_v144} + 12*{scale} = {want}",
                l.width
            );
        }
    }

    /// × 的命中区：贴右上角（right = w − DISMISS_RIGHT_MARGIN、y = 0），边长 14*s。
    #[test]
    fn dismiss_hit_sits_in_top_right_corner() {
        for scale in [1.0f32, 1.25, 1.5] {
            let bar = (48.0 * scale) as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 0.0, 0.0, WidgetForm::Compact.lyric_w());
            let margin = limits::DISMISS_RIGHT_MARGIN * scale;
            let side = limits::DISMISS_HIT * scale;
            assert!((side - 14.0 * scale).abs() < 0.001, "DISMISS_HIT 应已缩到 14");
            assert!((margin - 4.0 * scale).abs() < 0.001, "DISMISS_RIGHT_MARGIN 应为 4");
            assert!((l.hit_dismiss.right() - (l.width as f32 - margin)).abs() < 0.001);
            assert!(l.hit_dismiss.y.abs() < 0.001, "× 必须从客户区顶部开始");
            assert!((l.hit_dismiss.w - side).abs() < 0.001, "hit.w = {}", l.hit_dismiss.w);
            assert!((l.hit_dismiss.h - side).abs() < 0.001, "hit.h = {}", l.hit_dismiss.h);
            assert!((l.hit_dismiss.x - (l.width as f32 - margin - side)).abs() < 0.001);
        }
    }

    /// 误触防线：× 命中区与「下一首」命中区**水平净距恰为 2*s**（绝不重叠）。
    ///
    /// v1.5.3：净距从 8 收到 2（用户授权「只要按键之间不冲突即可」）。
    /// v1.5.4：命中区 16 → 14，但净距**结构上恒等于 `DISMISS_GAP*s`**——
    /// `hit_dismiss.x = w − RM − HIT` 而 `hit_next.right = w − (RM + HIT + GAP)`，
    /// 两者相减只剩 GAP。所以缩字号只会让 × 的左缘与三键**一起**右移 2px，
    /// 缝本身不动（这正是用户「按键整体右移」的要求）。
    /// 这里用**等号**而不是 `≥`：布局里净距与按钮右缘是同一个减法，
    /// 任何「顺手多让一点 / 多占一点」的改动都必须先在这里红。
    #[test]
    fn dismiss_hit_keeps_2px_clear_of_next_hit() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let bar = (48.0 * scale) as i32;
                let l = compute_full(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0, form.lyric_w());
                let gap = l.hit_dismiss.x - l.hit_next.right();
                assert!(
                    (gap - limits::DISMISS_GAP * scale).abs() < 0.001,
                    "{form:?}@{scale}: × 与 next 净距 = {gap}，应恰为 DISMISS_GAP*{scale} = {}",
                    limits::DISMISS_GAP * scale
                );
                // 写死真值：2*s（v1.5.2 是 8*s）
                assert!(
                    (gap - 2.0 * scale).abs() < 0.001,
                    "{form:?}@{scale}: × 与 next 净距 = {gap}，应恰为 2*{scale}"
                );
                // 视觉上三键也整体右移（btn_next 右缘同样让出整列）
                assert!(l.btn_next.right() < l.hit_dismiss.x);
            }
        }
    }

    /// 字形在命中区内居中：每边余量 = (14 − 6) / 2 * s = 4*s（v1.5.4）。
    #[test]
    fn dismiss_glyph_is_centred_in_hit() {
        let s = 1.0f32;
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, s, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        let margin = (limits::DISMISS_HIT - limits::DISMISS_ICON) * 0.5 * s;
        assert!((margin - 4.0).abs() < 0.001, "每边余量应恰为 4*s");
        assert!((l.dismiss.x - (l.hit_dismiss.x + margin)).abs() < 0.001);
        assert!((l.dismiss.y - (l.hit_dismiss.y + margin)).abs() < 0.001);
        assert!((l.dismiss.w - limits::DISMISS_ICON * s).abs() < 0.001);
        assert!((l.dismiss.h - limits::DISMISS_ICON * s).abs() < 0.001);
        let (hx, hy) = l.hit_dismiss.center();
        let (cx, cy) = l.dismiss.center();
        assert!((hx - cx).abs() < 0.001 && (hy - cy).abs() < 0.001, "字形必须居中");
    }

    /// **v1.5.4**：字形 6*s、命中区 14*s，四周余量**写死 4*s**。
    ///
    /// 上面那条用 `DISMISS_HIT − DISMISS_ICON` 反推余量，两个常量一起错（例如
    /// 有人把字形改回 8、命中改回 16）它照样通过。这条把「6 和 14」与「4/4/4/4」
    /// 全写成字面量，缩字号这件事才算真被锁住。
    ///
    /// 改名史：v1.5.1 `..._8px_with_4px_margin_in_16px_hit`
    /// → v1.5.4 `..._6px_with_4px_margin_in_14px_hit`。
    #[test]
    fn dismiss_glyph_is_6px_with_4px_margin_in_14px_hit() {
        // 常量层：6 / 14，且 6 在 14 里每边留 4
        assert!((limits::DISMISS_ICON - 6.0).abs() < 0.001, "字形应缩到 6");
        assert!((limits::DISMISS_HIT - 14.0).abs() < 0.001, "命中区应缩到 14");
        assert!(
            (limits::DISMISS_LINE - 1.2).abs() < 0.001,
            "6px 字形配 1.2px 笔画（v1.5.4：加粗以对齐三键的墨色浓度），实为 {}",
            limits::DISMISS_LINE
        );

        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let bar = (48.0 * scale).round() as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 0.0, 0.0, WidgetForm::Compact.lyric_w());

            // 字形边长 = 6*s，命中区 = 14*s
            assert!(
                (l.dismiss.w - 6.0 * scale).abs() < 0.001
                    && (l.dismiss.h - 6.0 * scale).abs() < 0.001,
                "scale={scale}: 字形 = {}×{}，应为 6*{scale}",
                l.dismiss.w,
                l.dismiss.h
            );
            assert!(
                (l.hit_dismiss.w - 14.0 * scale).abs() < 0.001,
                "scale={scale}: 命中区 = {}，应为 14*{scale}",
                l.hit_dismiss.w
            );

            // 四周余量各 4*s：左 / 上 / 右 / 下
            let left = l.dismiss.x - l.hit_dismiss.x;
            let top = l.dismiss.y - l.hit_dismiss.y;
            let right = l.hit_dismiss.right() - l.dismiss.right();
            let bottom = l.hit_dismiss.bottom() - l.dismiss.bottom();
            for (name, m) in [("左", left), ("上", top), ("右", right), ("下", bottom)] {
                assert!(
                    (m - 4.0 * scale).abs() < 0.001,
                    "scale={scale}: 字形{name}侧余量 = {m}，应恰为 4*{scale}"
                );
            }
            // 字形整个落在命中区内（余量非负的另一半保证）
            assert!(left >= 0.0 && top >= 0.0 && right >= 0.0 && bottom >= 0.0);
        }
    }

    /// **v1.5.1 新增**：悬停/按下的极淡圆角底**不得越界**。
    ///
    /// 底色是「事后加的一层视觉」，最容易在调参时悄悄变大：一旦它越出
    /// `hit_dismiss`，用户就会看到亮块比可点区域大（或压到三键），而且
    /// 点不中的地方反而有反馈——比没有反馈更糟。
    #[test]
    fn dismiss_hover_background_stays_inside_hit_and_clear_of_next() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let bar = (48.0 * scale).round() as i32;
                let l = compute_full(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0, form.lyric_w());
                let bg = l.dismiss_bg_rect();
                let hit = l.hit_dismiss;
                let tag = format!("{form:?}@{scale}");

                // 1) 底矩形 ⊆ 命中区（四条边都不越界）
                assert!(
                    bg.x >= hit.x - 0.001
                        && bg.y >= hit.y - 0.001
                        && bg.right() <= hit.right() + 0.001
                        && bg.bottom() <= hit.bottom() + 0.001,
                    "{tag}: 底色 ({},{},{},{}) 越出命中区 ({},{},{},{})",
                    bg.x,
                    bg.y,
                    bg.w,
                    bg.h,
                    hit.x,
                    hit.y,
                    hit.w,
                    hit.h
                );

                // 2) 与「下一首」命中区**不相交**（水平方向就已分开）
                let overlap_w = bg.right().min(l.hit_next.right()) - bg.x.max(l.hit_next.x);
                let overlap_h = bg.bottom().min(l.hit_next.bottom()) - bg.y.max(l.hit_next.y);
                assert!(
                    overlap_w <= 0.0 || overlap_h <= 0.0,
                    "{tag}: 底色与 hit_next 重叠 {overlap_w}×{overlap_h}"
                );
                // 3) 净距仍是 DISMISS_GAP（底色不占用那 2*s，v1.5.3）
                assert!(
                    (bg.x - l.hit_next.right() - limits::DISMISS_GAP * scale).abs() < 0.001,
                    "{tag}: 底色左缘与 next 净距 = {}，应恰为 {}",
                    bg.x - l.hit_next.right(),
                    limits::DISMISS_GAP * scale
                );

                // 4) 圆角半径 4*s 不能超过底色短边的一半（否则 D2D 会画成胶囊/异常）
                let r = limits::DISMISS_BG_RADIUS * scale;
                assert!(
                    r <= bg.w.min(bg.h) * 0.5 + 0.001,
                    "{tag}: 圆角 = {r} 超过底色短边一半（{}）",
                    bg.w.min(bg.h) * 0.5
                );
            }
        }
    }

    /// 回归：加了 × 之后高度**仍恒等于条带厚度**（不变量不许破）。
    #[test]
    fn dismiss_does_not_change_height() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for bar in [40i32, 48, 60, 72] {
                let l = compute_full(TaskbarEdge::Bottom, form, 1.0, bar, 0.0, 0.0, form.lyric_w());
                assert_eq!(l.height, bar, "{form:?}@{bar}");
            }
        }
    }

    /// `fixed_width()` 必须与 `compute_full` 的固定件同源（+12*s）。
    #[test]
    fn fixed_width_matches_computed_width() {
        // 162（v1.4.4 固定件，含**左右两份** CONTENT_PAD 8+8）
        // − 8（右侧那份 pad 被 DISMISS_RIGHT_MARGIN 取代）
        // + 4（右距）+ 14（命中，v1.5.4）+ 2（净距，v1.5.3）= 174
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, 0.0);
        assert!((l.fixed_width() - 174.0).abs() < 0.001, "fixed_width = {}", l.fixed_width());
        // 换算出每一项，把「三个常量一起漂移」的等价改动也拦住
        assert!(
            (l.fixed_width() - (162.0 - 8.0 + limits::DISMISS_RIGHT_MARGIN + limits::DISMISS_HIT + limits::DISMISS_GAP))
                .abs()
                < 0.001,
            "固定件必须 = 162 − 右侧 pad 8 + 右距 {} + 命中 {} + 净距 {}",
            limits::DISMISS_RIGHT_MARGIN,
            limits::DISMISS_HIT,
            limits::DISMISS_GAP
        );
        // 字面量版本（不经过常量，锁死 174 这个数本身）
        assert!(
            (l.fixed_width() - (162.0 - 8.0 + 4.0 + 14.0 + 2.0)).abs() < 0.001,
            "固定件必须 = 154 + 4 + 14 + 2 = 174"
        );
    }

    // ================================================ QA（G3 独立验证）补充断言
    //
    // 下面几条是 QA 侧新增的「跨维度」断言，理由：既有用例里 × 的几何只在
    // `TaskbarEdge::Bottom` 上验证过，而左右任务栏走的是旋转渲染路径
    // （`TaskbarEdge::is_vertical`），布局仍按水平语义算——一旦有人给左右方位
    // 加特判，只有全矩阵断言能拦住。

    /// **B 组**：几何不变量跨「2 形态 × 4 缩放 × 4 方位」全矩阵。
    ///
    /// 五条断言对应最容易回归的五件事：
    /// 1. `height == bar_thickness`（不变量，× 不许改变高度）；
    /// 2. × 命中区贴右边缘（`right = width − DISMISS_RIGHT_MARGIN`，v1.5.2）、
    ///    `y = 0`、边长 `14*s`；
    /// 3. 与「下一首」命中区水平净距**恰为** `DISMISS_GAP*s`（v1.5.3 = 2*s，
    ///    仍严格不相交）；
    /// 4. 与「进度条」命中区**不重叠**——进度条命中区是**整宽**，最容易被忽略；
    /// 5. 完全落在窗口内（`x ≥ 0`、`right ≤ width`、`bottom ≤ height`）。
    #[test]
    fn dismiss_geometry_invariants_matrix() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                for edge in [
                    TaskbarEdge::Bottom,
                    TaskbarEdge::Top,
                    TaskbarEdge::Left,
                    TaskbarEdge::Right,
                ] {
                    let bar = (48.0 * scale).round() as i32;
                    let l = compute_full(edge, form, scale, bar, 120.0, 40.0, form.lyric_w());
                    let margin = limits::DISMISS_RIGHT_MARGIN * scale;
                    let side = limits::DISMISS_HIT * scale;
                    let tag = format!("{form:?}/{scale}/{edge:?}");

                    // 1) 高度不变量
                    assert_eq!(l.height, bar, "{tag}: 高度必须恒等于条带厚度");

                    // 2) 贴右上角 + 边长（v1.5.2：距右缘 DISMISS_RIGHT_MARGIN，不是 CONTENT_PAD）
                    assert!(
                        (l.hit_dismiss.right() - (l.width as f32 - margin)).abs() < 0.001,
                        "{tag}: hit_dismiss.right = {} 应距右缘 DISMISS_RIGHT_MARGIN 内侧（{}）",
                        l.hit_dismiss.right(),
                        l.width as f32 - margin
                    );
                    assert!(
                        l.hit_dismiss.y.abs() < 0.001,
                        "{tag}: hit_dismiss.y = {} 应从客户区顶部开始",
                        l.hit_dismiss.y
                    );
                    assert!(
                        (l.hit_dismiss.w - side).abs() < 0.001
                            && (l.hit_dismiss.h - side).abs() < 0.001,
                        "{tag}: hit_dismiss 边长 = {}×{}，应为 {side}",
                        l.hit_dismiss.w,
                        l.hit_dismiss.h
                    );

                    // 3) 与 next 的水平净距（误触防线；v1.5.3：恰为 2*s，不是 ≥）
                    let gap = l.hit_dismiss.x - l.hit_next.right();
                    assert!(
                        (gap - limits::DISMISS_GAP * scale).abs() < 0.001,
                        "{tag}: × 与 next 净距 = {gap}，应恰为 {}",
                        limits::DISMISS_GAP * scale
                    );

                    // 4) 与进度条命中区不重叠（进度条命中区横跨整宽）
                    let overlap_w = l.hit_dismiss.right().min(l.hit_progress.right())
                        - l.hit_dismiss.x.max(l.hit_progress.x);
                    let overlap_h = l.hit_dismiss.bottom().min(l.hit_progress.bottom())
                        - l.hit_dismiss.y.max(l.hit_progress.y);
                    assert!(
                        overlap_w <= 0.0 || overlap_h <= 0.0,
                        "{tag}: × 命中区与进度条命中区重叠 {overlap_w}×{overlap_h}\
                         （dismiss {}/{}，progress {}/{}）",
                        l.hit_dismiss.y,
                        l.hit_dismiss.bottom(),
                        l.hit_progress.y,
                        l.hit_progress.bottom()
                    );

                    // 5) 不出窗口
                    assert!(l.hit_dismiss.x >= 0.0, "{tag}: hit_dismiss.x = {} < 0", l.hit_dismiss.x);
                    assert!(
                        l.hit_dismiss.right() <= l.width as f32 + 0.001,
                        "{tag}: hit_dismiss.right = {} > width {}",
                        l.hit_dismiss.right(),
                        l.width
                    );
                    assert!(
                        l.hit_dismiss.bottom() <= l.height as f32 + 0.001,
                        "{tag}: hit_dismiss.bottom = {} > height {}",
                        l.hit_dismiss.bottom(),
                        l.height
                    );

                    // 字形 6*s 且居中于命中区（v1.5.4：8 → 6）
                    assert!(
                        (l.dismiss.w - limits::DISMISS_ICON * scale).abs() < 0.001,
                        "{tag}: 字形边长 = {}，应为 {}",
                        l.dismiss.w,
                        limits::DISMISS_ICON * scale
                    );
                    let (hx, hy) = l.hit_dismiss.center();
                    let (gx, gy) = l.dismiss.center();
                    assert!(
                        (hx - gx).abs() < 0.001 && (hy - gy).abs() < 0.001,
                        "{tag}: 字形中心 ({gx},{gy}) 必须与命中区中心 ({hx},{hy}) 重合"
                    );
                }
            }
        }
    }

    /// **B6 组**：最小窗宽场景——`compute_full(Compact, 1.0, 48, 0.0, 0.0, 0.0)`，无文本、宽度被
    /// `WIDGET_MIN_W = 200` 钳住（固定件 174 + 信息区 0 被吃掉 26px）。
    ///
    /// 这是 × 最可能出问题的角落：窗口只剩 200px，净距压到 2 之后**余量极小**。
    /// 任何一处微调（右边距/按钮宽/净距）都会先在这里破防，
    /// 所以必须把实际数值写进断言消息，失败时一眼能看出被谁挤掉了。
    ///
    /// v1.5.4 的真值（width = 200 固定）：
    /// - `hit_dismiss = (182, 0, 14, 14)`，`right = 196 = 200 − DISMISS_RIGHT_MARGIN 4`
    ///   —— **与 v1.5.3 完全相同**：× 的右缘一个字没动，只是左边薄了 2px
    /// - `btn_next.x = 152 = 200 − dismiss_col 20 − btn 28`（v1.5.3 是 150 → **右移 2px**）
    /// - `hit_next.right() = 180`（v1.5.3 是 178）
    /// - 净距 = 182 − 180 = **2**
    /// - 字形右缘 = 196 − 4（居中余量）= 192 → 距窗口右缘 **8** = 左侧 CONTENT_PAD（不变）
    /// - 字形左缘 = 182 + 4 = 186；「下一首」图标右缘 = 152 + 22 = 174 →
    ///   墨迹缝 = 186 − 174 = **12**（用户要求「间距 12px 不变」）
    ///
    /// 这一段就是用户这一轮要的「× 变小 → 左侧内容整体右移」：窗口被下限钳住时
    /// 宽度**不变**，三键再右移 2px（v1.5.3 已右移 6px，累计相对 v1.5.2 右移 8px）。
    #[test]
    fn dismiss_fits_in_minimum_width_window() {
        let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48, 0.0, 0.0, WidgetForm::Compact.lyric_w());
        assert_eq!(l.width, 200, "无文本时窗口宽度应被 WIDGET_MIN_W 钳住");
        assert!(
            (l.fixed_width() - 174.0).abs() < 0.001,
            "固定件 = {}，应为 174（v1.4.4 的 162 + 右距 4 + 14 命中 + 2 净距）",
            l.fixed_width()
        );

        // 实际数值：hit_dismiss = (182, 0, 14, 14)
        assert!(
            (l.hit_dismiss.x - 182.0).abs() < 0.001,
            "hit_dismiss.x = {}，应为 182（200 − 右距 4 − 14）",
            l.hit_dismiss.x
        );
        assert!(
            (l.hit_dismiss.right() - 196.0).abs() < 0.001,
            "hit_dismiss.right = {}，应为 196（200 − 右距 4）",
            l.hit_dismiss.right()
        );
        assert!(
            (l.hit_dismiss.w - 14.0).abs() < 0.001 && (l.hit_dismiss.h - 14.0).abs() < 0.001,
            "hit_dismiss = {}×{}，应为 14×14",
            l.hit_dismiss.w,
            l.hit_dismiss.h
        );
        assert!(l.hit_dismiss.x >= 0.0 && l.hit_dismiss.right() <= l.width as f32);

        // 三键的真实落点（写死）：next.x = 152、命中区右缘 = 180
        assert!(
            (l.btn_next.x - 152.0).abs() < 0.001,
            "btn_next.x = {}，应为 152（200 − dismiss_col 20 − btn 28）",
            l.btn_next.x
        );
        assert!(
            (l.hit_next.right() - 180.0).abs() < 0.001,
            "hit_next.right = {}，应为 180",
            l.hit_next.right()
        );

        // 净距恰好 2（不是 "≥2" 的宽松判定，锁死数值）
        let gap = l.hit_dismiss.x - l.hit_next.right();
        assert!(
            (gap - 2.0).abs() < 0.001,
            "最小窗宽下 × 与 next 净距 = {gap}，应恰为 DISMISS_GAP = 2（next.right = {}）",
            l.hit_next.right()
        );

        // 三键仍完整落在窗口内（没有因为让出 × 的整列而被挤出左边界）
        assert!(l.btn_next.right() < l.hit_dismiss.x);
        assert!(
            l.btn_prev.x >= 0.0,
            "最小窗宽下「上一首」被挤出左边界：x = {}",
            l.btn_prev.x
        );
        assert!(
            l.hit_prev.x >= 0.0,
            "最小窗宽下「上一首」命中区被挤出左边界：x = {}",
            l.hit_prev.x
        );

        // 整条等式写死：next.x = 200 − dismiss_col(4+14+2) − btn 28 = 152
        // （用三个字面量重算，避开"只改一处也能过"的隐患）
        assert!(
            (l.btn_next.x - (200.0 - (4.0 + 14.0 + 2.0) - 28.0)).abs() < 0.001,
            "btn_next.x = {}，应为 200 − 20 − 28 = 152",
            l.btn_next.x
        );

        // v1.5.4 的「右移 2px」实证：v1.5.3 的 next.x = 150，现在 152
        assert!(
            (l.btn_next.x - (150.0 + 2.0)).abs() < 0.001,
            "btn_next.x = {} 应为 v1.5.3 的 150 + 2",
            l.btn_next.x
        );
        // 三键是**整体右移**，不是被压缩：尺寸 28、间距 6 一个字没动
        assert!(
            (l.btn_prev.w - 28.0).abs() < 0.001 && (l.btn_next.w - 28.0).abs() < 0.001,
            "三键宽度必须仍是 28（右移而非压缩）"
        );
        assert!(
            (l.btn_toggle.x - (l.btn_prev.right() + 6.0)).abs() < 0.001
                && (l.btn_next.x - (l.btn_toggle.right() + 6.0)).abs() < 0.001,
            "BTN_GAP 必须仍是 6"
        );
    }

    /// **G1 组**：× 命中区 `14*s` **刻意低于** `HIT_MIN = 28*s` 这件事必须显式写死。
    ///
    /// 既有 `rects_do_not_overlap_and_hits_meet_minimum` 的 ≥HIT_MIN 循环里
    /// **没有** `hit_dismiss`（它是唯一不达标的命中区），如果不写这条断言，
    /// 这个例外就是「被悄悄跳过」而不是「被明确豁免」——后人把 × 加进循环、
    /// 或把 `DISMISS_HIT` 调大到 28，都会静默破坏净距与 +12px 宽度预算。
    #[test]
    fn dismiss_hit_is_deliberately_smaller_than_hit_min() {
        assert!(
            limits::DISMISS_HIT < limits::HIT_MIN,
            "DISMISS_HIT（{}）必须小于 HIT_MIN（{}）：调大到 28 会挤掉 2*s 净距",
            limits::DISMISS_HIT,
            limits::HIT_MIN
        );
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let bar = (48.0 * scale).round() as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 0.0, 0.0, WidgetForm::Compact.lyric_w());
            assert!(
                (l.hit_dismiss.w - limits::DISMISS_HIT * scale).abs() < 0.001,
                "scale={scale}: hit_dismiss.w = {}，应恰为 14*s = {}",
                l.hit_dismiss.w,
                limits::DISMISS_HIT * scale
            );
            assert!(
                (l.hit_dismiss.w - 14.0 * scale).abs() < 0.001,
                "scale={scale}: hit_dismiss.w = {}，应恰为写死的 14*s = {}",
                l.hit_dismiss.w,
                14.0 * scale
            );
            // 其余可点击命中区必须 ≥ HIT_MIN（防止有人"为了对称"把它们也缩小）
            for (name, r) in [
                ("hit_prev", l.hit_prev),
                ("hit_toggle", l.hit_toggle),
                ("hit_next", l.hit_next),
                ("hit_cover", l.hit_cover),
            ] {
                assert!(
                    r.w >= limits::HIT_MIN * scale - 0.01 && r.h >= limits::HIT_MIN * scale - 0.01,
                    "scale={scale}: {name} = {}×{} < {}",
                    r.w,
                    r.h,
                    limits::HIT_MIN * scale
                );
            }
        }
    }

    // ================================================ QA（v1.5.1 缩小复验）补充断言
    //
    // 上面既有用例的写法基本都是「常量推导常量」（`DISMISS_HIT * scale`
    // 对 `hit_dismiss.w`）。那类断言能防"实现偏离常量"，但**防不住
    // 常量成对漂移**：把字形改回 12、命中改回 20，余量仍然是 4，
    // 一大批断言会照旧通过、而用户验收要的 8px 已经没了。
    // 下面这组把 v1.5.1 的**字面量真值**单独锁死。

    /// **A 组**：× 的 8 个常量必须逐个等于**当前**需求表里的字面量。
    ///
    /// 需求表（v1.5.4，用户验收反馈「× 的不透明度和"下一首"一致、大小改为 6px、
    /// 图标与 × 间距 12px 不变」）：
    /// 字形 **6** / 命中 **14** / 净距 2（不变）/ 笔画 **1.2** /
    /// 字形不透明度 0xFF（不变，但与按钮同源写法）/ 悬停底 0x14 / 按下底 0x22 / 圆角 4。
    ///
    /// 这条是整组几何断言的「根」：先证明根没动，底下写死的 14/2/4 才有意义。
    ///
    /// 改名史：v1.5.1 `..._match_v151_spec`（净距 8）→ v1.5.3 `..._match_v153_spec`（净距 2）
    /// → v1.5.4 `..._match_v154_spec`（字形 6 / 命中 14 / 笔画 1.2）。
    #[test]
    fn dismiss_constants_match_v154_spec() {
        let close = |a: f32, b: f32| (a - b).abs() < 0.001;
        // 本轮两个改动的尺寸量：字形 8 → 6、命中 16 → 14
        assert!(close(limits::DISMISS_ICON, 6.0), "字形应为 6，实为 {}", limits::DISMISS_ICON);
        assert!(close(limits::DISMISS_HIT, 14.0), "命中区应为 14，实为 {}", limits::DISMISS_HIT);
        // 净距与右距本轮不动（12px 墨迹缝、8px 右对称都靠它们 + 居中余量 4 维持）
        assert!(
            close(limits::DISMISS_GAP, 2.0),
            "净距应为 2（v1.5.3 的成果，本轮不动），实为 {}",
            limits::DISMISS_GAP
        );
        // 净距必须**严格为正**：0 就是热区相贴，误触防线彻底消失
        assert!(
            limits::DISMISS_GAP > 0.0,
            "净距必须为正，实为 {}",
            limits::DISMISS_GAP
        );
        // 三键本轮一个都没动
        assert!(close(limits::BTN_HIT, 28.0), "BTN_HIT 应为 28，实为 {}", limits::BTN_HIT);
        assert!(close(limits::BTN_GAP, 6.0), "BTN_GAP 应为 6，实为 {}", limits::BTN_GAP);
        assert!(
            close(limits::DISMISS_RIGHT_MARGIN, 4.0),
            "右边距应为 4（v1.5.2 的成果，本轮不动），实为 {}",
            limits::DISMISS_RIGHT_MARGIN
        );
        // 笔画 1.0 → 1.2（本轮第三个改动量：让墨色浓度追上三键）
        assert!(close(limits::DISMISS_LINE, 1.2), "笔画应为 1.2，实为 {}", limits::DISMISS_LINE);
        assert_eq!(limits::DISMISS_ALPHA, 0xFF, "字形应为满不透明 0xFF（与三键 enabled 同值）");
        assert_eq!(limits::DISMISS_BG_HOVER, 0x14, "悬停底应为 0x14");
        assert_eq!(limits::DISMISS_BG_PRESSED, 0x22, "按下底应比悬停深一档 0x22");
        assert!(close(limits::DISMISS_BG_RADIUS, 4.0), "圆角应为 4");
        // 语义前提：底色必须比字形淡得多，否则 6px 叉会长在一块实心块里
        assert!(
            (limits::DISMISS_BG_HOVER as u32) * 4 < limits::DISMISS_ALPHA as u32,
            "悬停底 0x14 必须远淡于字形 0xFF"
        );
        assert!(
            limits::DISMISS_BG_HOVER < limits::DISMISS_BG_PRESSED,
            "按下态必须比悬停态深"
        );
    }

    /// **v1.5.4 新增**：× 的**墨色浓度**必须与三键可比 —— 这是用户
    /// 「× 的不透明度改为和按钮"下一首"一致」这句反馈的**可执行表达**。
    ///
    /// 实情：alpha 本来就一样（`draw_dismiss` 与 `draw_buttons` 的 enabled 都是
    /// `theme.text @ 0xFF`）。用户看到的「淡」来自**笔画细**：三键是 2.2px 实心条，
    /// × 是 1.0px 斜线，抗锯齿后每行覆盖率只有一半。
    ///
    /// 所以这条把「浓度」定义成 `笔画 / 字形` 的比值，并给出两条下限：
    /// 1. `DISMISS_LINE >= 1.1` —— 6px 字形下的**最小可辨认**线宽（再细，
    ///    抗锯齿把两行像素都摊成 50% 灰，看上去就是「比按钮淡」）；
    /// 2. `DISMISS_LINE / DISMISS_ICON >= 0.18` —— **相对浓度**下限，
    ///    防止有人为了「更细更秀气」把线改回 1.0（1.0/6 ≈ 0.167 会红）。
    ///
    /// 为什么不能只写死 `== 1.2`：那是 A 组的事（常量表锁死）。这条锁的是
    /// **意图**——将来若字形再变（例如 8），线宽也得跟着变，而不是一起漂移后
    /// 又变回「看起来发灰」。两条一起才既锁当下、又锁语义。
    #[test]
    fn dismiss_ink_density_matches_buttons() {
        assert!(
            limits::DISMISS_LINE >= 1.1,
            "笔画 = {}，6px 字形下必须 ≥ 1.1，否则抗锯齿后覆盖率只有一半、看上去比三键淡",
            limits::DISMISS_LINE
        );
        let density = limits::DISMISS_LINE / limits::DISMISS_ICON;
        assert!(
            density >= 0.18,
            "笔画/字形 = {density}（{} / {}），必须 ≥ 0.18：低于它「×」会重新发灰，\
             与三键的墨色浓度不再一致",
            limits::DISMISS_LINE,
            limits::DISMISS_ICON
        );
        // 与三键的横向对照：三键实心条 2.2px，× 的实际墨量 = 笔画 × √2（45° 斜线）
        // ≈ 1.70px/行。写进断言消息方便评审直接看比例。
        assert!(
            limits::DISMISS_LINE * std::f32::consts::SQRT_2 >= 1.5,
            "× 每行水平墨量 = {}×√2 = {}，必须 ≥ 1.5px（三键实心条是 2.2px）",
            limits::DISMISS_LINE,
            limits::DISMISS_LINE * std::f32::consts::SQRT_2
        );
    }

    /// **B 组（补）**：2 形态 × 4 缩放 × 4 方位下，命中 14*s / 字形 6*s /
    /// 净距**恰为** 2*s —— 全部用字面量，不用常量推导。
    ///
    /// 既有 `dismiss_geometry_invariants_matrix` 也已改成等号判净距，但边长仍取自
    /// 常量；这里把「字面量」这一层补上。净距用等号而不是 ≥：
    /// 布局里 net gap 与按钮右缘是同一个减法，任何"多让一点"的改动都该被发现。
    #[test]
    fn dismiss_literal_geometry_matrix() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                for edge in [
                    TaskbarEdge::Bottom,
                    TaskbarEdge::Top,
                    TaskbarEdge::Left,
                    TaskbarEdge::Right,
                ] {
                    let bar = (48.0 * scale).round() as i32;
                    let l = compute_full(edge, form, scale, bar, 120.0, 40.0, form.lyric_w());
                    let tag = format!("{form:?}/{scale}/{edge:?}");

                    // 命中区 14*s，字形 6*s（字面量）
                    assert!(
                        (l.hit_dismiss.w - 14.0 * scale).abs() < 0.001
                            && (l.hit_dismiss.h - 14.0 * scale).abs() < 0.001,
                        "{tag}: 命中区 = {}×{}，应恰为 14*{scale}",
                        l.hit_dismiss.w,
                        l.hit_dismiss.h
                    );
                    assert!(
                        (l.dismiss.w - 6.0 * scale).abs() < 0.001
                            && (l.dismiss.h - 6.0 * scale).abs() < 0.001,
                        "{tag}: 字形 = {}×{}，应恰为 6*{scale}",
                        l.dismiss.w,
                        l.dismiss.h
                    );

                    // 净距**恰为** 2*s（不是 ≥）
                    let gap = l.hit_dismiss.x - l.hit_next.right();
                    assert!(
                        (gap - 2.0 * scale).abs() < 0.001,
                        "{tag}: × 与 next 净距 = {gap}，应恰为 2*{scale}"
                    );

                    // 字形四边余量各 4*s（字面量：(14 − 6) / 2）
                    for (name, m) in [
                        ("左", l.dismiss.x - l.hit_dismiss.x),
                        ("上", l.dismiss.y - l.hit_dismiss.y),
                        ("右", l.hit_dismiss.right() - l.dismiss.right()),
                        ("下", l.hit_dismiss.bottom() - l.dismiss.bottom()),
                    ] {
                        assert!(
                            (m - 4.0 * scale).abs() < 0.001,
                            "{tag}: 字形{name}侧余量 = {m}，应恰为 4*{scale}"
                        );
                    }
                }
            }
        }
    }

    /// **B 组（补）**：字形右缘到「下一首」命中区右缘的实际距离 = 12*s。
    ///
    /// = 净距 2*s + 字形左余量 4*s + 字形宽 6*s 的组合结果。
    /// 单看三条断言各自都能被"两个量一起动"糊过去，合成一个数才是用户
    /// 真正看到的那段空白（v1.5.2 是 20*s、v1.5.3 是 14*s）。
    ///
    /// 改名史：v1.5.2 `..._is_twenty_px` → v1.5.3 `..._is_fourteen_px`
    /// → v1.5.4 `..._is_twelve_px`。
    #[test]
    fn dismiss_glyph_clearance_from_next_is_twelve_px() {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let bar = (48.0 * scale).round() as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 0.0, 0.0, WidgetForm::Compact.lyric_w());
            let clearance = l.dismiss.right() - l.hit_next.right();
            assert!(
                (clearance - 12.0 * scale).abs() < 0.001,
                "scale={scale}: 字形右缘 − next 命中区右缘 = {clearance}，应恰为 12*{scale}"
            );
        }
    }

    /// **v1.5.4 验收本质**：热区净距恰 `2*s`，且用户真正看到的那两条缝都恰 `12*s`。
    ///
    /// 用户这一轮的原话是「× 的不透明度和"下一首"一致，大小改为 6px，"下一首"的
    /// 图标和 × 间距 12px 不变，但是由于 × 变小了，左侧内容应该会整体右移」。
    /// 这句话有三个判据，缺一不可：
    ///
    /// 1. **「不冲突」** = 两个命中区仍严格不相交 → `hit_dismiss.x −
    ///    hit_next.right()` 恰为 `DISMISS_GAP*s`（本轮仍是 2*s）。
    /// 2. **「间距 12px 不变」** = 用户真正**看到**的缝，而不是热区之间的缝。
    ///    热区比图形大，所以视觉缝有两个口径，两个都写死：
    ///    * `dismiss.right() − hit_next.right() = 12*s`
    ///      = 净距 2 + 字形左余量 4 + **字形宽 6**
    ///      —— 「命中区边缘 → × 笔画右端」；
    ///    * `dismiss.x − 图标右缘 = 12*s`
    ///      = 图标在 28 热区内的右侧余量 6 + 净距 2 + 字形左余量 4
    ///      —— **这才是「下一首」箭头到 × 笔画之间那条肉眼可见的白缝**。
    /// 3. **「× 变小了」** = 字形恰 6*s（热区不能跟着缩到 12，否则口径 B 会变 10）。
    ///
    /// ## v1.5.4 的巧合（值得写下来，否则后人会以为是笔误）
    ///
    /// v1.5.3（字形 8 / 命中 16）时口径 A = 14、口径 B = 12，两者差 **2**。
    /// 本轮字形 8→6、命中 16→14 之后，A 与 B **都等于 12**：
    /// `A − B = DISMISS_ICON − (BTN_HIT − BTN_ICON)/2 = 6 − 6 = 0`。
    /// 也就是说「A == B」本身就是一条**强断言**（左边三项是 × 的尺寸，
    /// 右边两项是三键的尺寸，任何一边漂移都会破），所以对账式从
    /// v1.5.3 的「差值 = 2」改成「两者相等」。
    ///
    /// 只锁热区（判据 1）不够：把 `DISMISS_ICON` / `BTN_ICON` 一起缩小同样能让
    /// 净距保持 2，视觉缝却会变宽 —— 而用户验收的正是视觉缝。
    ///
    /// 4 档 scale × 2 形态全跑：任何一处漏乘 `s`，或者谁把净距改回 8、
    /// 把图标/字形尺寸挪一下，都会先在这里红。
    ///
    /// 改名史：v1.5.3 `dismiss_gap_is_two_and_glyph_gap_is_fourteen`
    /// → v1.5.4 `dismiss_gap_is_two_and_ink_gap_is_twelve`。
    #[test]
    fn dismiss_gap_is_two_and_ink_gap_is_twelve() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let bar = (48.0 * scale).round() as i32;
                let l = compute_full(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0, form.lyric_w());
                let tag = format!("{form:?}@{scale}");

                // ① 「不冲突」：热区净距恰 2*s（绝不重叠，也绝不多让）
                let hit_gap = l.hit_dismiss.x - l.hit_next.right();
                assert!(
                    (hit_gap - 2.0 * scale).abs() < 0.001,
                    "{tag}: 热区净距 = {hit_gap}，应恰为 2*{scale}"
                );
                assert!(
                    hit_gap > 0.0,
                    "{tag}: 热区净距 = {hit_gap}，必须严格为正（0 就是热区相贴）"
                );
                assert!(
                    (hit_gap - limits::DISMISS_GAP * scale).abs() < 0.001,
                    "{tag}: 热区净距 = {hit_gap}，常量 DISMISS_GAP 也必须仍是 {}",
                    limits::DISMISS_GAP
                );

                // ② 视觉缝口径 A：命中区右缘 → × 笔画右端，恰 12*s（= 2 + 4 + 6）
                let glyph_right_gap = l.dismiss.right() - l.hit_next.right();
                assert!(
                    (glyph_right_gap - 12.0 * scale).abs() < 0.001,
                    "{tag}: × 笔画右端 − next 命中区右缘 = {glyph_right_gap}，应恰为 12*{scale}\
                     （= 净距 2 + 左余量 4 + 字形 6；v1.5.3 是 14*{scale}）"
                );

                // ② 视觉缝口径 B：**「下一首」图标右缘 → × 笔画左缘**，恰 12*s
                //    （= 图标右侧余量 6 + 净距 2 + 字形左余量 4）
                //    图标与字形都是「在各自热区里居中」，所以这两个余量必须由
                //    BTN_HIT/BTN_ICON、DISMISS_HIT/DISMISS_ICON 直接推出来，
                //    这里按 painter 的真实画法（`rect.center()` 居中）算。
                let next_icon_right =
                    l.btn_next.center().0 + limits::BTN_ICON * 0.5 * scale;
                assert!(
                    (next_icon_right - (l.btn_next.x + (limits::BTN_HIT - limits::BTN_ICON)
                        * 0.5 * scale
                        + limits::BTN_ICON * scale))
                        .abs()
                        < 0.001,
                    "{tag}: 图标右缘推算自相矛盾"
                );
                let ink_gap = l.dismiss.x - next_icon_right;
                assert!(
                    (ink_gap - 12.0 * scale).abs() < 0.001,
                    "{tag}: 「下一首」图标右缘 → × 笔画左缘 = {ink_gap}，应恰为 12*{scale}\
                     （= 6+2+4）；这是用户要求「间距 12px 不变」的直接判据"
                );
                // 拆开写死三项，防止"两个量一起动"糊过去
                assert!(
                    ((limits::BTN_HIT - limits::BTN_ICON) * 0.5
                        + limits::DISMISS_GAP
                        + (limits::DISMISS_HIT - limits::DISMISS_ICON) * 0.5
                        - 12.0)
                        .abs()
                        < 0.001,
                    "{tag}: 6(图标右余量) + {}(净距) + {}(字形左余量) 必须 = 12",
                    limits::DISMISS_GAP,
                    (limits::DISMISS_HIT - limits::DISMISS_ICON) * 0.5
                );

                // 两个视觉口径的自洽：v1.5.4 起 A − B = DISMISS_ICON − (BTN_HIT−BTN_ICON)/2
                // = 6 − 6 = 0，所以两者的对账式是「相等」——这比 v1.5.3 的「差 2」更强。
                assert!(
                    ((glyph_right_gap - ink_gap)
                        - (limits::DISMISS_ICON * scale
                            - (limits::BTN_HIT - limits::BTN_ICON) * 0.5 * scale))
                        .abs()
                        < 0.001,
                    "{tag}: 12*s 与 12*s 两个口径对不上（差值 {}，应为 DISMISS_ICON − (BTN_HIT−BTN_ICON)/2）",
                    glyph_right_gap - ink_gap
                );
                assert!(
                    (glyph_right_gap - ink_gap).abs() < 0.001,
                    "{tag}: v1.5.4 两个口径必须**相等**（都 12*s），实测 A−B = {}",
                    glyph_right_gap - ink_gap
                );

                // ③ 三键与前序版本对齐：是三键右移，不是 × 右移；字形恰 6*s
                assert!(
                    (l.dismiss.w - limits::DISMISS_ICON * scale).abs() < 0.001
                        && (l.dismiss.w - 6.0 * scale).abs() < 0.001,
                    "{tag}: × 字形必须仍是 6*{scale}（v1.5.3 是 8*{scale}）"
                );
            }
        }
    }

    /// **B 组（补）**：窗口宽度增量在 4 档缩放下都**恰为 +12*s**。
    ///
    /// v1.4.4 的基线宽度 362/452/543/724 是字面量（= trunc(362*s) 的排版结果），
    /// 所以「把净距偷偷改回 8」（+18）或「把右边距改回 CONTENT_PAD 8」（+16）
    /// 都会同时打破 `want` 与实测两条。
    ///
    /// v1.5.4 实测真值（= trunc(fixed 174*s + info 200*s)）：
    /// 374 / 467 / 561 / 748。
    ///
    /// 公式那条用 ±0.51 而不是 `assert_eq!`：`compute_full` 最后一步是
    /// `(fixed + info_w) as i32` **截断**。精确真值由下面的 `assert_eq!` 字面量表锁死。
    ///
    /// 改名史：v1.5.2 `..._is_20px_at_all_scales` → v1.5.3 `..._is_14px_at_all_scales`
    /// → v1.5.4 `..._is_12px_at_all_scales`。
    #[test]
    fn dismiss_width_delta_is_12px_at_all_scales() {
        for (scale, width_v144, width_now) in [
            (1.0f32, 362.0f32, 374.0f32),
            (1.25, 452.0, 467.0),
            (1.5, 543.0, 561.0),
            (2.0, 724.0, 748.0),
        ] {
            let bar = (48.0 * scale).round() as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 200.0, 0.0, WidgetForm::Compact.lyric_w());
            let want = width_v144 + 12.0 * scale;
            assert!(
                (l.width as f32 - want).abs() < 0.51,
                "scale={scale}: width = {}，应为 v1.4.4 的 {width_v144} + 12*{scale} = {want}",
                l.width
            );
            // 字面量真值表（不依赖上面那条公式的推导顺序）
            assert_eq!(
                l.width as f32, width_now,
                "scale={scale}: width = {}，应为写死的 {width_now}",
                l.width
            );
            // fixed_width 同源：固定件 = 162*s + 12*s = 174*s
            assert!(
                (l.fixed_width() - (162.0 + 12.0) * scale).abs() < 0.001,
                "scale={scale}: fixed_width = {}，应为 174*{scale}",
                l.fixed_width()
            );
        }
    }

    /// **v1.5.2 验收本质**：字形右缘到窗口右缘的空白 == 左侧 `CONTENT_PAD`。
    ///
    /// 用户反馈的原话是「右侧空间留多了」——判据不是「右边距等于某个数」，
    /// 而是**左右对称**。所以这条断言用「左边距」（封面左缘 = `CONTENT_PAD*s`）
    /// 直接与「字形右缘到右边界」比较，并额外写死 8*s 的字面量：
    /// v1.5.1 这里是 12（右边距 8 + 字形居中余量 4），v1.5.2 收到 8。
    ///
    /// 4 档 scale × 2 形态全跑：任何一处漏乘 s、或者谁把右边距改回 `CONTENT_PAD`
    /// 而没同步收窄，都会先在这里红。
    ///
    /// v1.5.4（字形 8 → 6、命中 16 → 14）：居中余量仍是 `(14 − 6) / 2 = 4`，
    /// 所以「4 + 4 = 8 = CONTENT_PAD」这条对称**一个字没变** —— 下面的断言原样通过。
    #[test]
    fn dismiss_glyph_right_edge_is_symmetric_with_left_pad() {
        // 常量层：右边距必须是 4（4 + 字形居中余量 4 = 8 = CONTENT_PAD）
        assert!(
            (limits::DISMISS_RIGHT_MARGIN - 4.0).abs() < 0.001,
            "DISMISS_RIGHT_MARGIN 应为 4，实为 {}",
            limits::DISMISS_RIGHT_MARGIN
        );
        // 居中余量本身也写死：v1.5.4 起 (14 − 6) / 2 = 4
        assert!(
            ((limits::DISMISS_HIT - limits::DISMISS_ICON) * 0.5 - 4.0).abs() < 0.001,
            "字形居中余量 = ({} − {}) / 2，应为 4",
            limits::DISMISS_HIT,
            limits::DISMISS_ICON
        );
        assert!(
            (limits::DISMISS_RIGHT_MARGIN + (limits::DISMISS_HIT - limits::DISMISS_ICON) * 0.5
                - limits::CONTENT_PAD)
                .abs()
                < 0.001,
            "右边距 + 字形居中余量 必须恰等于 CONTENT_PAD"
        );

        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let bar = (48.0 * scale).round() as i32;
                let l = compute_full(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0, form.lyric_w());
                let tag = format!("{form:?}@{scale}");

                // 右侧：字形右缘 → 窗口右缘
                let glyph_right_gap = l.width as f32 - l.dismiss.right();
                // 左侧：封面左缘 = CONTENT_PAD*s（同一份 pad）
                let left_gap = l.cover.x;

                assert!(
                    (left_gap - limits::CONTENT_PAD * scale).abs() < 0.001,
                    "{tag}: 左边距 = {left_gap}，应恰为 CONTENT_PAD*{scale}"
                );
                assert!(
                    (glyph_right_gap - left_gap).abs() < 0.001,
                    "{tag}: 左右不对称 —— 字形右缘距窗口 {glyph_right_gap}，左边距 {left_gap}"
                );
                // 写死真值：两侧都恰为 8*s（v1.5.1 右侧是 12*s）
                assert!(
                    (glyph_right_gap - 8.0 * scale).abs() < 0.001,
                    "{tag}: 字形右缘距窗口 = {glyph_right_gap}，应恰为 8*{scale}"
                );
                assert!(
                    l.dismiss.right() <= l.width as f32,
                    "{tag}: 字形越出窗口右缘"
                );
            }
        }
    }

    /// **C 组（补）**：`dismiss_bg_rect()` **恒等于** `hit_dismiss`（等号锁定，不是包含关系）。
    ///
    /// 这条特意用**等号**写：当前实现是 `fn dismiss_bg_rect(&self) -> RectF { self.hit_dismiss }`，
    /// 即"底色就是那一格"。等号锁定有代价——G4 若要把底色内缩 1px（更好看的做法），
    /// 这条用例会先红，迫使改动者显式改断言并在评审里说明"包含关系取代等号"。
    /// 这正是我们要的：视觉层的自由度必须显式宣告，不能悄悄发生。
    ///
    /// 顺带在 2 形态 × 4 缩放 × 4 方位上验：
    /// 底色非空、不出窗口、与 `hit_next` 净距**恰为** `DISMISS_GAP*s`（v1.5.3 = 2*s）。
    #[test]
    fn dismiss_bg_rect_equals_hit_dismiss_matrix() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                for edge in [
                    TaskbarEdge::Bottom,
                    TaskbarEdge::Top,
                    TaskbarEdge::Left,
                    TaskbarEdge::Right,
                ] {
                    let bar = (48.0 * scale).round() as i32;
                    let l = compute_full(edge, form, scale, bar, 120.0, 40.0, form.lyric_w());
                    let tag = format!("{form:?}/{scale}/{edge:?}");
                    let bg = l.dismiss_bg_rect();
                    let hit = l.hit_dismiss;

                    // 1) 等号：四条边 + 宽高逐一相等（不是 ⊆）
                    assert!(
                        (bg.x - hit.x).abs() < 0.001
                            && (bg.y - hit.y).abs() < 0.001
                            && (bg.w - hit.w).abs() < 0.001
                            && (bg.h - hit.h).abs() < 0.001,
                        "{tag}: 底色 ({},{},{},{}) 必须恒等于命中区 ({},{},{},{})",
                        bg.x,
                        bg.y,
                        bg.w,
                        bg.h,
                        hit.x,
                        hit.y,
                        hit.w,
                        hit.h
                    );

                    // 2) 非空（painter 里 `is_empty()` 守卫的反面前提）
                    assert!(!bg.is_empty(), "{tag}: 底色不得为空矩形（否则悬停无反馈）");

                    // 3) 与 next 净距恰为 DISMISS_GAP*s（v1.5.3 = 2*s）+ 不出窗口
                    assert!(
                        (bg.x - l.hit_next.right() - limits::DISMISS_GAP * scale).abs() < 0.001,
                        "{tag}: 底色左缘与 next 净距 = {}，应恰为 {}",
                        bg.x - l.hit_next.right(),
                        limits::DISMISS_GAP * scale
                    );
                    assert!(
                        (bg.x - l.hit_next.right() - 2.0 * scale).abs() < 0.001,
                        "{tag}: 底色左缘与 next 净距 = {}，应恰为写死的 2*{scale}",
                        bg.x - l.hit_next.right()
                    );
                    assert!(
                        bg.right() <= l.width as f32 + 0.001 && bg.bottom() <= l.height as f32 + 0.001,
                        "{tag}: 底色 ({},{}) 越出窗口 {}×{}",
                        bg.right(),
                        bg.bottom(),
                        l.width,
                        l.height
                    );
                }
            }
        }
    }

    /// **C 组（补）**：圆角 4*s 在 4 档缩放下都**严格小于**底色短边的一半（7*s）。
    ///
    /// 风险点：D2D 对 `radius > 短边/2` 的圆角矩形会退化成胶囊/异常形状。
    /// v1.5.4 起底色短边是 14*s（v1.5.3 是 16*s），4:7 仍有一倍以上余量；
    /// 但若有人把 `DISMISS_BG_RADIUS` 调大（或把底色缩小），这里会先红。
    /// painter 侧的 `.min(短边*0.5)` 兜底见 `src/render/painter.rs`（代码走查，
    /// 有 D2D 上下文才能测、单测跑不到）。
    ///
    /// v1.5.4 顺带调整：v1.5.3 有一条「圆角 ≤ 短边 1/4」的观感断言，源码是
    /// `r <= short * 0.25`，16px 方块下 4 ≤ 4 刚好成立。底色缩到 14 之后
    /// 4 > 14×0.25 = 3.5，这条**不再是当前设计的真值**（圆角故意保持 4 不变，
    /// 因为它在 14px 方块上仍像「任务栏图标悬停块」，而缩到 3.5 会引入小数
    /// 像素圆角、反而不干净）。所以判据改成 `r ≤ short * 0.30`（4/14 ≈ 0.286），
    /// 语义仍是「不许把方块画成圆形」，只是把阈值对齐到 v1.5.4 的实际设计。
    #[test]
    fn dismiss_bg_radius_is_safely_under_half_short_side() {
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let bar = (48.0 * scale).round() as i32;
            let l = compute_full(TaskbarEdge::Bottom, WidgetForm::Compact, scale, bar, 0.0, 0.0, WidgetForm::Compact.lyric_w());
            let bg = l.dismiss_bg_rect();
            let short = bg.w.min(bg.h);
            let r = limits::DISMISS_BG_RADIUS * scale;
            assert!(
                (short - 14.0 * scale).abs() < 0.001,
                "scale={scale}: 底色短边 = {short}，应恰为 14*{scale}（v1.5.3 是 16*{scale}）"
            );
            assert!(
                (r - 4.0 * scale).abs() < 0.001,
                "scale={scale}: 圆角 = {r}，应恰为 4*{scale}"
            );
            assert!(
                r < short * 0.5 - 0.001,
                "scale={scale}: 圆角 {r} 必须严格小于短边一半（{}），否则画成胶囊",
                short * 0.5
            );
            // 观感：圆角不超过短边的 30%（4/14 ≈ 28.6%），超过就偏圆
            assert!(
                r <= short * 0.30 + 0.001,
                "scale={scale}: 圆角 {r} 超过短边 30%（{}），14px 方块会显圆",
                short * 0.30
            );
        }
    }
}
