//! 命中测试（`ARCHITECTURE.md` §10.7 / T08 第 1 条）。
//!
//! **纯函数**：给定布局与客户区坐标，返回语义化的 [`HitTarget`]。
//! 不碰任何句柄，可以单测——这也是「拖动进度条不误触按钮」这条验收的
//! 第一道保障（第二道是 `SetCapture`，第三道是 Seeking 态屏蔽）。
//!
//! ## 命中顺序（重要）
//!
//! 0. **关闭 × 最先**（v1.5.0 / G3）：它独占右上角，与播放三键的命中区之间有
//!    ≥ 8*s 的水平净距，正常情况下不可能与按钮抢点击；放在最前面是为了让
//!    「× 永远可点」成为结构性保证，而不是依赖布局微调后的巧合。
//! 1. **Seeking 态只返回 `Progress`**：拖动期间指针可能扫过按钮，
//!    此时任何按钮（含 ×）都必须是死区（PRD P0-10 的核心验收）。
//! 2. 平时按钮优先于进度条：进度条命中区是「整宽 + 底部 7~9px」，
//!    与按钮命中区在极端小的任务栏厚度下可能相接，按钮优先可以避免
//!    「想点下一首结果跳了进度」。

use crate::render::layout::WidgetLayout;

/// 命中目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// 未命中任何可交互元素（空白区）
    None,
    Cover,
    Prev,
    Toggle,
    Next,
    Progress,
    /// 右上角关闭 ×（v1.5.0 / G3）：点击 = 隐藏到托盘（仅本次运行）
    Dismiss,
}

impl HitTarget {
    /// 是否是一个「按钮」类目标（用于按下态渲染与点击分发）。
    ///
    /// **语义固定为「媒体播控按钮」**（上一首 / 播放暂停 / 下一首）：很多地方
    /// （按钮图标的启用/禁用配色、按下态绘制、`on_up` 的派发白名单）都靠它。
    /// 关闭 × **不属于**这一类，需要「可按下」的判定请用 [`Self::is_pressable`]。
    pub fn is_button(self) -> bool {
        matches!(
            self,
            HitTarget::Prev | HitTarget::Toggle | HitTarget::Next
        )
    }

    /// 是否「可按下」（有按下反馈、mousedown 时进 `pressed`）。
    ///
    /// = 媒体播控按钮 ∪ 关闭 ×。封面另有独立语义（不算按下态），由调用方单判。
    pub fn is_pressable(self) -> bool {
        self.is_button() || matches!(self, HitTarget::Dismiss)
    }

    /// 日志友好名。
    pub fn as_str(self) -> &'static str {
        match self {
            HitTarget::None => "None",
            HitTarget::Cover => "Cover",
            HitTarget::Prev => "Prev",
            HitTarget::Toggle => "Toggle",
            HitTarget::Next => "Next",
            HitTarget::Progress => "Progress",
            HitTarget::Dismiss => "Dismiss",
        }
    }
}

/// 命中测试。坐标是**客户区物理像素**。
///
/// `seeking == true` 时只可能返回 `Progress` / `None`（拖进度条期间 × 与按钮全部失活）。
pub fn hit_test(layout: &WidgetLayout, x: f32, y: f32, seeking: bool) -> HitTarget {
    if seeking {
        // 拖动期间把命中区纵向放宽到整个主行，避免指针轻微上移就"脱手"
        if x >= 0.0
            && x < layout.width as f32
            && y >= layout.hit_progress.y - layout.hit_progress.h
            && y < layout.main_row.bottom()
        {
            return HitTarget::Progress;
        }
        return HitTarget::None;
    }

    // 0) 关闭 ×（v1.5.0）：先判——它独占右上角，与按钮不重叠（净距 ≥ 8*s），
    //    但放在最前面可以保证将来任何布局微调都不会让按钮抢走它的点击。
    if layout.hit_dismiss.contains(x, y) || layout.dismiss.contains(x, y) {
        return HitTarget::Dismiss;
    }

    // 1) 按钮（视觉区 / 命中区都接受）
    if layout.hit_prev.contains(x, y) || layout.btn_prev.contains(x, y) {
        return HitTarget::Prev;
    }
    if layout.hit_toggle.contains(x, y) || layout.btn_toggle.contains(x, y) {
        return HitTarget::Toggle;
    }
    if layout.hit_next.contains(x, y) || layout.btn_next.contains(x, y) {
        return HitTarget::Next;
    }

    // 2) 封面
    if layout.hit_cover.contains(x, y) || layout.cover.contains(x, y) {
        return HitTarget::Cover;
    }

    // 3) 进度条（整宽、底部）
    if layout.hit_progress.contains(x, y) {
        return HitTarget::Progress;
    }

    HitTarget::None
}

/// 进度条命中区内的归一化位置（0.0..=1.0）。
///
/// 用整宽而不是 `progress` 的宽度：用户从最左端拖到最右端应该覆盖 0%..100%。
pub fn seek_fraction(layout: &WidgetLayout, x: f32) -> f32 {
    let w = layout.hit_progress.w.max(1.0);
    ((x - layout.hit_progress.x) / w).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::layout::{compute, compute_ex, TaskbarEdge, WidgetForm};

    fn layout() -> WidgetLayout {
        compute(TaskbarEdge::Bottom, WidgetForm::Compact, 1.0, 48)
    }

    #[test]
    fn buttons_hit_their_centres() {
        let l = layout();
        for (rect, expect) in [
            (l.btn_prev, HitTarget::Prev),
            (l.btn_toggle, HitTarget::Toggle),
            (l.btn_next, HitTarget::Next),
            (l.cover, HitTarget::Cover),
        ] {
            let (cx, cy) = rect.center();
            assert_eq!(hit_test(&l, cx, cy, false), expect);
        }
    }

    #[test]
    fn progress_hit_at_bottom_strip() {
        let l = layout();
        let y = l.hit_progress.y + l.hit_progress.h * 0.5;
        // 中间位置（不在按钮上）
        assert_eq!(hit_test(&l, l.width as f32 * 0.5, y, false), HitTarget::Progress);
    }

    #[test]
    fn blank_area_returns_none() {
        let l = layout();
        // 主行中部、按钮左侧的空白（文本区之上但不在任何控件上）
        assert_eq!(hit_test(&l, 2.0, 2.0, false), HitTarget::None);
    }

    /// PRD P0-10 的核心断言：Seeking 态下扫过按钮区不产生按钮命中。
    #[test]
    fn seeking_masks_every_button() {
        let l = layout();
        for rect in [l.btn_prev, l.btn_toggle, l.btn_next] {
            let (cx, cy) = rect.center();
            assert_eq!(
                hit_test(&l, cx, cy, true),
                HitTarget::None,
                "拖动期间按钮必须失活"
            );
        }
        // 但进度条区域仍然可拖
        let y = l.hit_progress.y + l.hit_progress.h * 0.5;
        assert_eq!(
            hit_test(&l, l.width as f32 * 0.5, y, true),
            HitTarget::Progress
        );
    }

    #[test]
    fn seek_fraction_spans_zero_to_one() {
        let l = layout();
        assert!((seek_fraction(&l, 0.0) - 0.0).abs() < 1e-6);
        assert!((seek_fraction(&l, l.width as f32) - 1.0).abs() < 1e-6);
        let mid = seek_fraction(&l, l.width as f32 * 0.5);
        assert!((mid - 0.5).abs() < 0.02, "mid = {mid}");
        // 越界钳位
        assert_eq!(seek_fraction(&l, -50.0), 0.0);
        assert_eq!(seek_fraction(&l, 9999.0), 1.0);
    }

    #[test]
    fn target_labels_are_stable() {
        assert_eq!(HitTarget::Prev.as_str(), "Prev");
        assert!(HitTarget::Toggle.is_button());
        assert!(!HitTarget::Progress.is_button());
    }

    // ------------------------------------------------- v1.5.0（G3）右上角关闭 ×

    /// × 命中区中心 → `Dismiss`。
    #[test]
    fn dismiss_hit_centre_returns_dismiss() {
        let l = layout();
        let (cx, cy) = l.hit_dismiss.center();
        assert_eq!(hit_test(&l, cx, cy, false), HitTarget::Dismiss);
        // 字形中心同样是 Dismiss
        let (gx, gy) = l.dismiss.center();
        assert_eq!(hit_test(&l, gx, gy, false), HitTarget::Dismiss);
    }

    /// 拖动进度条期间 × 必须失活（拖动中是死区，绝不收起小组件）。
    #[test]
    fn dismiss_is_dead_while_seeking() {
        let l = layout();
        let (cx, cy) = l.hit_dismiss.center();
        assert_eq!(
            hit_test(&l, cx, cy, true),
            HitTarget::None,
            "拖动期间 × 必须失活"
        );
    }

    /// × 的语义：字符串稳定、可按下、但**不是**媒体播控按钮。
    #[test]
    fn dismiss_is_pressable_but_not_a_media_button() {
        assert_eq!(HitTarget::Dismiss.as_str(), "Dismiss");
        assert!(HitTarget::Dismiss.is_pressable());
        assert!(!HitTarget::Dismiss.is_button(), "× 不属于媒体播控按钮");
        // 既有语义不被 is_pressable 影响
        for t in [HitTarget::Prev, HitTarget::Toggle, HitTarget::Next] {
            assert!(t.is_button() && t.is_pressable());
        }
        assert!(!HitTarget::Progress.is_pressable());
        assert!(!HitTarget::Cover.is_pressable());
        assert!(!HitTarget::None.is_pressable());
    }

    // ================================================ QA（G3 独立验证）补充断言

    /// **C3 组**：× 命中区的边界语义。
    ///
    /// `RectF::contains` 是**左闭右开**（`px >= x && px < right`），所以：
    /// 四角 / 四边**向内 0.5px** 必须仍是 `Dismiss`；向外 0.5px 必须**不是**
    /// `Dismiss`（也不许回落到 next/progress 而变成别的动作）。
    /// 取 0.5px 而不是整 1px：任何"边界差一像素"的取整错误都会在这里暴露。
    #[test]
    fn dismiss_hit_boundaries_are_exact() {
        let l = layout();
        let r = l.hit_dismiss;
        let eps = 0.5;
        let (cx, cy) = r.center();

        // 四角 + 四边中点，各向内缩 0.5px
        for (x, y) in [
            (r.x + eps, r.y + eps),
            (r.right() - eps, r.y + eps),
            (r.x + eps, r.bottom() - eps),
            (r.right() - eps, r.bottom() - eps),
            (cx, r.y + eps),
            (cx, r.bottom() - eps),
            (r.x + eps, cy),
            (r.right() - eps, cy),
        ] {
            assert_eq!(
                hit_test(&l, x, y, false),
                HitTarget::Dismiss,
                "({x},{y}) 在 × 命中区内 0.5px，必须返回 Dismiss"
            );
        }

        // 四条边向外 0.5px
        for (x, y) in [
            (r.x - eps, cy),
            (r.right() + eps, cy),
            (cx, r.y - eps),
            (cx, r.bottom() + eps),
        ] {
            assert_ne!(
                hit_test(&l, x, y, false),
                HitTarget::Dismiss,
                "({x},{y}) 在 × 命中区外 0.5px，不得返回 Dismiss"
            );
        }
    }

    /// **C4 组**：× 是「最先判」的，但不得抢走播放三键的点击。
    ///
    /// 既有 `buttons_hit_their_centres` 只在 100%/Bottom 上验证过；这里补全
    /// 2 形态 × 4 缩放，确认× 命中区左移三键之后没有覆盖到任何一个按钮中心。
    #[test]
    fn dismiss_does_not_steal_the_three_buttons() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                let bar = (48.0 * scale).round() as i32;
                let l = compute_ex(TaskbarEdge::Bottom, form, scale, bar, 120.0, 40.0);
                for (rect, expect, name) in [
                    (l.btn_prev, HitTarget::Prev, "prev"),
                    (l.btn_toggle, HitTarget::Toggle, "toggle"),
                    (l.btn_next, HitTarget::Next, "next"),
                ] {
                    let (bx, by) = rect.center();
                    assert_eq!(
                        hit_test(&l, bx, by, false),
                        expect,
                        "{form:?}@{scale}: 按钮 {name} 中心被 × 抢走"
                    );
                }
            }
        }
    }

    /// **C 组补充**：8px 净距必须是真的**死区**，而不只是"矩形不相交"。
    ///
    /// 紧贴「下一首」命中区右缘 1px 处，既不能是 `Next`（会误播下一首），
    /// 也不能是 `Dismiss`（会误收起小组件）。
    #[test]
    fn gap_between_next_and_dismiss_is_a_dead_zone() {
        let l = layout();
        let x = l.hit_next.right() + 1.0;
        let y = l.btn_next.center().1;
        assert!(x < l.hit_dismiss.x, "测试前提：该点必须落在净距里");
        assert_eq!(
            hit_test(&l, x, y, false),
            HitTarget::None,
            "净距 ({x},{y}) 必须是死区，不得命中 Next 或 Dismiss"
        );
    }

    // ================================================ QA（v1.5.1 缩小复验）补充断言

    /// **D 组（补）**：命中区**真的缩小了** —— v1.5.0 的 24×24 里、v1.5.1 的
    /// 16×16 外的那些点，现在必须是死区。
    ///
    /// 为什么需要从 `hit_test` 侧再验一次：布局层断言只证明"矩形是 16"，
    /// 而 `hit_test` 里除了 `hit_dismiss` 还 OR 了 `dismiss`（字形），
    /// 且外层还可能是 `expanded_to` 之类被后人加上。这条用例从**用户实际
    /// 点击的入口**确认热区缩了，且缩掉的区域不会回落到 Next（误触更糟）。
    #[test]
    fn dismiss_shrunk_area_is_dead_zone_after_v151() {
        let l = layout();
        // 前提：v1.5.0 的 24 热区是 (168, 0, 24, 24)，v1.5.1 是 (176, 0, 16, 16)
        assert!((l.hit_dismiss.x - 176.0).abs() < 0.001, "v1.5.1 热区 x 应为 176");
        assert!((l.hit_dismiss.w - 16.0).abs() < 0.001, "v1.5.1 热区边长应为 16");

        for (x, y) in [
            (170.0, 12.0), // 旧热区内、新热区左侧
            (184.0, 20.0), // 旧热区内、新热区下方
            (175.5, 8.0),  // 新热区左边界外 0.5px
        ] {
            assert_ne!(
                hit_test(&l, x, y, false),
                HitTarget::Dismiss,
                "({x},{y}) 落在 v1.5.0 旧热区但已出 v1.5.1 新热区，不得命中 Dismiss"
            );
            assert_ne!(
                hit_test(&l, x, y, false),
                HitTarget::Next,
                "({x},{y}) 是缩小腾出来的区域，绝不能回落成 Next（误触更糟）"
            );
        }

        // 热区中心仍然必须是 Dismiss（16px 热区缩小后中心点必须好点）
        let (cx, cy) = l.hit_dismiss.center();
        assert_eq!(hit_test(&l, cx, cy, false), HitTarget::Dismiss);
        assert_eq!(hit_test(&l, 184.0, 8.0, false), HitTarget::Dismiss, "中心右 8px 仍在内");
    }

    /// **D 组（补）**：2 形态 × 4 缩放 × 4 方位 —— 热区中心恒为 `Dismiss`，
    /// 且 `seeking` 期间恒为 `None`。
    ///
    /// 既有的 `dismiss_hit_centre_returns_dismiss` / `dismiss_is_dead_while_seeking`
    /// 只在 100% / Bottom 上跑。缩放后热区中心是 `16*s/2 = 8*s`，若有人把
    /// 竖向偏移写成固定 8px（而不是随 s），1.25+ 上中心点就会落到热区外。
    #[test]
    fn dismiss_centre_and_dead_state_matrix() {
        for form in [WidgetForm::Compact, WidgetForm::Lyric] {
            for scale in [1.0f32, 1.25, 1.5, 2.0] {
                for edge in [
                    TaskbarEdge::Bottom,
                    TaskbarEdge::Top,
                    TaskbarEdge::Left,
                    TaskbarEdge::Right,
                ] {
                    let bar = (48.0 * scale).round() as i32;
                    let l = compute_ex(edge, form, scale, bar, 120.0, 40.0);
                    let (cx, cy) = l.hit_dismiss.center();
                    assert_eq!(
                        hit_test(&l, cx, cy, false),
                        HitTarget::Dismiss,
                        "{form:?}@{scale}/{edge:?}: 热区中心 ({cx},{cy}) 必须命中 Dismiss"
                    );
                    assert_eq!(
                        hit_test(&l, cx, cy, true),
                        HitTarget::None,
                        "{form:?}@{scale}/{edge:?}: 拖动期间 × 必须失活"
                    );
                }
            }
        }
    }
}
