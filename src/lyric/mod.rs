//! 歌词模型（`ARCHITECTURE.md` §7.2）。
//!
//! **M1 只落数据契约，不落引擎**：解析器 / 插值 / 网络 / 缓存分别是 T10–T12（M3）的内容。
//! 这里提前把类型定义好，是因为 `AppState`（§7.7）必须持有 `LyricView`，
//! 而 `AppEvent`（§8.1）必须能携带 `Lyrics` / `LyricMissReason`。

use std::sync::Arc;

use crate::media::{Ms, TrackId};

/// 歌词类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LyricKind {
    /// 无歌词（占位）
    #[default]
    None,
    /// 行级时间戳，逐字靠插值
    Line,
    /// 增强 LRC，带字级时间戳，真逐字
    Word,
}

/// 一个扫光单元。
#[derive(Debug, Clone, Default)]
pub struct LyricWord {
    /// 该字 / 词的文本（已切分，不含分隔符；分隔符并入前一个 unit）
    pub text: String,
    pub start_ms: Ms,
    pub end_ms: Ms,
}

/// 一行歌词。
#[derive(Debug, Clone, Default)]
pub struct LyricLine {
    pub start_ms: Ms,
    /// 下一行的 start_ms；最后一行 = start_ms + 5000
    pub end_ms: Ms,
    /// 整行纯文本（用于无扫光时的静态显示与测量）
    pub text: String,
    /// 切分后的单元序列。Word 类型来自 LRC 字标签；Line 类型由 interp 生成。
    pub words: Vec<LyricWord>,
}

/// 一整首歌词。
#[derive(Debug, Clone, Default)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    pub kind: LyricKind,
    /// 来自 `[offset:]` 标签，单位毫秒（正值表示歌词提前）
    pub offset_ms: Ms,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub by: String,
}

impl Lyrics {
    /// 二分查找当前行下标。`t` 已含 offset。
    /// 返回 `None` 表示还没到第一行。
    pub fn line_at(&self, t: Ms) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        let idx = self.lines.partition_point(|l| l.start_ms <= t);
        if idx == 0 {
            None
        } else {
            Some(idx - 1)
        }
    }

    /// 当前行的扫光进度 `0.0..=1.0`，以及当前字下标。
    pub fn word_progress(&self, line_idx: usize, t: Ms) -> (f32, usize) {
        let Some(line) = self.lines.get(line_idx) else {
            return (0.0, 0);
        };
        if line.words.is_empty() {
            let span = (line.end_ms - line.start_ms).max(1) as f32;
            let p = ((t - line.start_ms) as f32 / span).clamp(0.0, 1.0);
            return (p, 0);
        }
        let wi = line.words.partition_point(|w| w.start_ms <= t);
        let cur = wi.saturating_sub(1);
        let w = &line.words[cur];
        let span = (w.end_ms - w.start_ms).max(1) as f32;
        let frac = ((t - w.start_ms) as f32 / span).clamp(0.0, 1.0);
        let total = line.words.len() as f32;
        ((cur as f32 + frac) / total, cur)
    }
}

/// 歌词加载状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LyricStatus {
    #[default]
    Idle,
    Loading,
    Ready,
    NotFound,
    NetworkError,
}

/// 歌词「没有」的原因。会一路传到 UI 决定占位文案与是否重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricMissReason {
    /// 本地与在线都没有
    NotFound,
    /// 网络失败（会重试）
    NetworkError,
    /// 用户关闭了在线歌词且无本地文件
    OnlineDisabled,
    /// 播放器未上报足够元数据（无标题 / 无时长）
    InsufficientMetadata,
}

/// UI 线程持有的歌词状态。高度由 mode 决定，与 status 无关（PRD P0-16 无抖动）。
#[derive(Debug, Clone)]
pub struct LyricView {
    pub status: LyricStatus,
    pub track_id: TrackId,
    pub lyrics: Option<Arc<Lyrics>>,
    pub line_idx: Option<usize>,
    /// 已尝试重试次数（0..=3）
    pub retry: u8,
}

impl Default for LyricView {
    fn default() -> Self {
        Self {
            status: LyricStatus::Idle,
            track_id: 0,
            lyrics: None,
            line_idx: None,
            retry: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: Ms, end: Ms, text: &str) -> LyricLine {
        LyricLine {
            start_ms: start,
            end_ms: end,
            text: String::from(text),
            words: Vec::new(),
        }
    }

    #[test]
    fn line_at_is_binary_search() {
        let l = Lyrics {
            lines: vec![line(0, 1000, "a"), line(1000, 2000, "b"), line(2000, 3000, "c")],
            ..Default::default()
        };
        assert_eq!(l.line_at(-1), None);
        assert_eq!(l.line_at(0), Some(0));
        assert_eq!(l.line_at(999), Some(0));
        assert_eq!(l.line_at(1000), Some(1));
        assert_eq!(l.line_at(99_999), Some(2));
    }

    #[test]
    fn empty_lyrics_have_no_line() {
        let l = Lyrics::default();
        assert_eq!(l.line_at(0), None);
        assert_eq!(l.word_progress(0, 0), (0.0, 0));
    }

    #[test]
    fn word_progress_without_words_uses_line_span() {
        let l = Lyrics {
            lines: vec![line(1000, 2000, "hello")],
            ..Default::default()
        };
        let (p, idx) = l.word_progress(0, 1500);
        assert!((p - 0.5).abs() < 1e-6);
        assert_eq!(idx, 0);
    }
}
