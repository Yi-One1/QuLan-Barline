//! 歌词引擎（`ARCHITECTURE.md` §7.2 数据契约 + §12 实现）。
//!
//! 分层：
//! ```text
//! parser.rs    文本 → Lyrics（LRC / 增强 LRC 状态机）
//! interp.rs    一行文本 → 扫光单元（CJK 逐字 / 拉丁按词）+ 时间插值
//! locate.rs    本地 .lrc 查找（文件名匹配）
//! query.rs     在线查询的纯函数：标题清洗 / 候选 / 择优（v1.3.0）
//! provider.rs  LyricProvider trait + 有序链（本地 → 在线）
//! lrclib.rs    lrclib.net 实现（精确 + 模糊检索）
//! netease.rs   网易云音乐实现（兜底，v1.3.0）
//! net.rs       WinHTTP GET（零依赖）
//! cache.rs     内存 LRU + 磁盘正缓存 + 负缓存
//! service.rs   T2 线程：请求 → 查询 → AppEvent
//! ```
//!
//! 本文件只放**数据模型**（`Lyrics` / `LyricLine` / `LyricWord` / `LyricView`，逐字段
//! 照抄 §7.2）与**绘制态推导**（`LyricView::draw`，纯函数，无句柄）。

pub mod cache;
pub mod interp;
pub mod locate;
pub mod lrclib;
pub mod net;
pub mod netease;
pub mod parser;
pub mod provider;
pub mod query;
pub mod service;

use std::sync::Arc;

use crate::i18n::{self, Lang};
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
    /// 二分查找当前行下标。`t` 是播放位置（时间戳里已经扣过 offset，这里直接比）。
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

/// 扫光边界在一行内的位置（**UTF-16 码元**，连续值）。
///
/// 为什么是 UTF-16 而不是 char 下标：DirectWrite 的
/// `IDWriteTextLayout::HitTestTextPosition` 收的就是 UTF-16 位置，
/// 直接用可以省掉一次 char → u16 的映射表。
///
/// 返回值 `0.0..=行长度`：整数部分是"第几个码元之前"，小数部分是"当前字扫了多少"。
pub fn sweep_offset(line: &LyricLine, t: Ms) -> f32 {
    if line.words.is_empty() {
        // 没有切分单元（空行 / 间奏）：整行按时间线性扫
        let span = (line.end_ms - line.start_ms).max(1) as f32;
        let p = ((t - line.start_ms) as f32 / span).clamp(0.0, 1.0);
        return p * line.text.encode_utf16().count() as f32;
    }
    let idx = line
        .words
        .partition_point(|w| w.start_ms <= t)
        .saturating_sub(1);
    let mut before = 0usize;
    for w in &line.words[..idx] {
        before += w.text.encode_utf16().count();
    }
    let w = &line.words[idx];
    let span = (w.end_ms - w.start_ms).max(1) as f32;
    let local = ((t - w.start_ms) as f32 / span).clamp(0.0, 1.0);
    let wlen = w.text.encode_utf16().count() as f32;
    before as f32 + local * wlen
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

/// 一帧要画的歌词内容。**纯数据**，由 [`LyricView::draw`] 推导。
///
/// v1.1.1 起行数固定单行，不再携带下一行文本。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LyricDraw<'a> {
    /// 当前行文本（占位态时是占位文案）
    pub cur: &'a str,
    /// 扫光边界（当前行内的 UTF-16 位置，连续值）
    pub sweep: f32,
    /// 当前行下标（占位 / 首行之前为 `None`）
    pub line: Option<usize>,
    /// `true` 表示 `cur` 是占位文案：不做扫光、用低对比色
    pub placeholder: bool,
    /// 当前行时间窗口（v1.1.3）：跑马灯按窗口分配滚动量。
    /// 占位 / 首行之前为 `(0, 0)`——窗口为 0 时跑马灯自然不激活。
    pub line_start_ms: Ms,
    pub line_end_ms: Ms,
    /// 当前行**演唱**结束时刻（v1.1.4：最后一个字的 end_ms；无字级时间戳
    /// 则退化为 `line_end_ms`）。跑马灯比它提前完成滚动，留出读尾时间。
    pub line_sing_end_ms: Ms,
}

impl<'a> LyricDraw<'a> {
    fn placeholder_of(text: &'a str) -> Self {
        Self {
            cur: text,
            sweep: 0.0,
            line: None,
            placeholder: true,
            line_start_ms: 0,
            line_end_ms: 0,
            line_sing_end_ms: 0,
        }
    }

    /// 是否什么都不用画（歌词关闭且无占位）。
    pub fn is_empty(&self) -> bool {
        self.cur.is_empty()
    }
}

impl LyricView {
    /// 由当前状态 + 播放位置推出「这一帧画什么」。
    ///
    /// **尺寸与它无关**：四种降级态都返回同样结构的数据，布局只看
    /// 「歌词是否启用」（PRD P0-16 无抖动的实现方式）。
    pub fn draw(&self, t: Ms, lang: Lang) -> LyricDraw<'_> {
        match (self.status, self.lyrics.as_deref()) {
            (LyricStatus::Ready, Some(l)) if !l.lines.is_empty() => match l.line_at(t) {
                Some(i) => {
                    let line = &l.lines[i];
                    // 演唱结束 = 最后一个字的 end_ms（可能早于行窗口结束，
                    // 中间的差值就是行间间歇——留给读尾）；异常时间戳钳到行尾。
                    let sing_end = line
                        .words
                        .last()
                        .map(|w| w.end_ms.min(line.end_ms))
                        .unwrap_or(line.end_ms)
                        .max(line.start_ms);
                    LyricDraw {
                        cur: line.text.as_str(),
                        sweep: sweep_offset(line, t),
                        line: Some(i),
                        placeholder: false,
                        line_start_ms: line.start_ms,
                        line_end_ms: line.end_ms,
                        line_sing_end_ms: sing_end,
                    }
                }
                // 还没唱到第一行：留白（v1.1.1 起没有下一行预告）
                None => LyricDraw {
                    cur: "",
                    sweep: 0.0,
                    line: None,
                    placeholder: false,
                    line_start_ms: 0,
                    line_end_ms: 0,
                    line_sing_end_ms: 0,
                },
            },
            (LyricStatus::Loading, _) => LyricDraw::placeholder_of(i18n::t(lang, "lyric.loading")),
            (LyricStatus::NotFound, _) | (LyricStatus::NetworkError, _) => {
                LyricDraw::placeholder_of(i18n::t(lang, "lyric.missing"))
            }
            _ => LyricDraw::placeholder_of(""),
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

    fn word(text: &str, s: Ms, e: Ms) -> LyricWord {
        LyricWord {
            text: text.into(),
            start_ms: s,
            end_ms: e,
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

    #[test]
    fn sweep_offset_is_monotonic_and_bounded() {
        let l = Lyrics {
            lines: vec![LyricLine {
                start_ms: 0,
                end_ms: 3000,
                text: "你好世界".into(),
                words: vec![
                    word("你", 0, 750),
                    word("好", 750, 1500),
                    word("世", 1500, 2250),
                    word("界", 2250, 3000),
                ],
            }],
            ..Default::default()
        };
        let line = &l.lines[0];
        let mut prev = 0.0f32;
        for t in [-500, 0, 400, 750, 1000, 1500, 2999, 5000] {
            let s = sweep_offset(line, t);
            assert!(s >= prev - 1e-6, "扫光回退了：t={t} s={s} prev={prev}");
            assert!((0.0..=4.0).contains(&s), "越界：t={t} s={s}");
            prev = s;
        }
        // 整行唱完 = 4 个码元
        assert!((sweep_offset(line, 3000) - 4.0).abs() < 1e-6);
    }

    #[test]
    fn sweep_offset_handles_latin_words_by_utf16_len() {
        let l = Lyrics {
            lines: vec![LyricLine {
                start_ms: 0,
                end_ms: 2000,
                text: "hello world".into(),
                words: vec![word("hello ", 0, 1000), word("world", 1000, 2000)],
            }],
            ..Default::default()
        };
        let line = &l.lines[0];
        // 第一个词 6 个码元（含空格）
        assert!((sweep_offset(line, 1000) - 6.0).abs() < 1e-6);
        assert!((sweep_offset(line, 1500) - 8.5).abs() < 1e-6);
        assert!((sweep_offset(line, 2000) - 11.0).abs() < 1e-6);
    }

    #[test]
    fn draw_returns_placeholders_for_each_status() {
        let mut v = LyricView::default();
        assert!(v.draw(0, Lang::Zh).is_empty());

        v.status = LyricStatus::Loading;
        let d = v.draw(0, Lang::Zh);
        assert!(d.placeholder);
        assert!(d.cur.contains("加载"));

        v.status = LyricStatus::NotFound;
        let d = v.draw(0, Lang::Zh);
        assert!(d.placeholder);
        assert!(d.cur.contains("暂无"));

        v.status = LyricStatus::NetworkError;
        let d = v.draw(0, Lang::En);
        assert!(d.placeholder);
        assert!(d.cur.contains("No lyrics"));
    }

    #[test]
    fn draw_picks_current_line_only() {
        let l = Arc::new(Lyrics {
            lines: vec![line(0, 1000, "第一"), line(1000, 2000, "第二"), line(2000, 3000, "第三")],
            ..Default::default()
        });
        let v = LyricView {
            status: LyricStatus::Ready,
            lyrics: Some(l),
            ..Default::default()
        };
        let d = v.draw(1500, Lang::Zh);
        assert_eq!(d.cur, "第二");
        assert!(!d.placeholder);
    }

    /// v1.1.1：还没唱到第一行时整块留白（不再预告下一行）。
    #[test]
    fn draw_before_first_line_is_blank() {
        let l = Arc::new(Lyrics {
            lines: vec![line(5000, 6000, "第一")],
            ..Default::default()
        });
        let v = LyricView {
            status: LyricStatus::Ready,
            lyrics: Some(l),
            ..Default::default()
        };
        let d = v.draw(0, Lang::Zh);
        assert_eq!(d.cur, "");
        assert!(d.is_empty());
    }

    /// v1.1.4：演唱结束 = 最后一个字的 end_ms（无字级时间戳退化为行尾）。
    #[test]
    fn draw_reports_sing_end() {
        let with_words = Arc::new(Lyrics {
            lines: vec![LyricLine {
                start_ms: 0,
                end_ms: 5000,
                text: "你好".into(),
                words: vec![word("你", 0, 1000), word("好", 1000, 1800)],
            }],
            ..Default::default()
        });
        let v = LyricView {
            status: LyricStatus::Ready,
            lyrics: Some(with_words),
            ..Default::default()
        };
        let d = v.draw(500, Lang::Zh);
        assert_eq!(d.line_sing_end_ms, 1800, "唱完 = 最后一个字的 end_ms");

        let without_words = Arc::new(Lyrics {
            lines: vec![line(1000, 4000, "无字级时间戳")],
            ..Default::default()
        });
        let v = LyricView {
            status: LyricStatus::Ready,
            lyrics: Some(without_words),
            ..Default::default()
        };
        let d = v.draw(2000, Lang::Zh);
        assert_eq!(d.line_sing_end_ms, 4000, "无词退化为行尾");

        // 异常时间戳（词尾晚于行尾）钳到行尾
        let bad = Arc::new(Lyrics {
            lines: vec![LyricLine {
                start_ms: 0,
                end_ms: 3000,
                text: "坏".into(),
                words: vec![word("坏", 0, 9000)],
            }],
            ..Default::default()
        });
        let v = LyricView {
            status: LyricStatus::Ready,
            lyrics: Some(bad),
            ..Default::default()
        };
        let d = v.draw(100, Lang::Zh);
        assert_eq!(d.line_sing_end_ms, 3000);
    }

    #[test]
    fn draw_after_last_line_keeps_last_line() {
        let l = Arc::new(Lyrics {
            lines: vec![line(0, 1000, "只有一句")],
            ..Default::default()
        });
        let v = LyricView {
            status: LyricStatus::Ready,
            lyrics: Some(l),
            ..Default::default()
        };
        let d = v.draw(99_999, Lang::Zh);
        assert_eq!(d.cur, "只有一句");
    }
}
