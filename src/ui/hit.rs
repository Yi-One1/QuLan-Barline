//! 命中测试（`ARCHITECTURE.md` §10.7 / T08 第 1 条）。
//!
//! **纯函数**：给定布局与客户区坐标，返回语义化的 [`HitTarget`]。
//! 不碰任何句柄，可以单测——这也是「拖动进度条不误触按钮」这条验收的
//! 第一道保障（第二道是 `SetCapture`，第三道是 Seeking 态屏蔽）。
//!
//! ## 命中顺序（重要）
//!
//! 1. **Seeking 态只返回 `Progress`**：拖动期间指针可能扫过按钮，
//!    此时任何按钮都必须是死区（PRD P0-10 的核心验收）。
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
}

impl HitTarget {
    /// 是否是一个「按钮」类目标（用于按下态渲染与点击分发）。
    pub fn is_button(self) -> bool {
        matches!(
            self,
            HitTarget::Prev | HitTarget::Toggle | HitTarget::Next
        )
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
        }
    }
}

/// 命中测试。坐标是**客户区物理像素**。
///
/// `seeking == true` 时只可能返回 `Progress` / `None`。
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
    use crate::render::layout::{compute, TaskbarEdge, WidgetForm};

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
}
