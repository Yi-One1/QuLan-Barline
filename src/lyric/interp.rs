//! 逐字切分与时间插值（`ARCHITECTURE.md` §12.2）。
//!
//! 三件事：
//! 1. [`split_units`]：把一行文本切成「扫光单元」——CJK 逐字、拉丁/数字按词、
//!    标点与空格并入前一个单元；
//! 2. 在行时长内**按字符数加权**等分，得到每个单元的 `start_ms` / `end_ms`；
//! 3. [`fill_word_ends`]：增强 LRC 只有字级起始时间，用它把 `end_ms` 补齐。
//!
//! **为什么不用 `unicode-segmentation`**：本项目只需要「CJK 单字 / 拉丁词 / 其余」三级，
//! 手写 60 行、零依赖、行为可预测；`unicode-segmentation` 的中文按**词**切（需要词典），
//! 与 PRD「中文按字」的要求相反。

use crate::lyric::LyricWord;
use crate::media::Ms;

/// 字符分类。只用于切分，不参与渲染。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cls {
    /// 中日韩表意文字 / 假名 / 谚文：一字一单元
    Cjk,
    /// 拉丁字母与西文撇号：连续累积成一个词
    Latin,
    /// 数字：连续累积（`2024` 不该拆成 4 个单元）
    Digit,
    /// 空白：并入前一个单元（保持字间宽度感）
    Space,
    /// 标点：并入前一个单元（中文标点紧跟在前字后面）
    Punct,
}

/// 字符 → 分类。
fn classify(c: char) -> Cls {
    let u = c as u32;
    let is_cjk = matches!(u,
        0x3040..=0x30FF      // 平假名 / 片假名
        | 0x3400..=0x4DBF    // CJK 扩展 A
        | 0x4E00..=0x9FFF    // CJK 基本区
        | 0xF900..=0xFAFF    // CJK 兼容表意
        | 0xAC00..=0xD7AF    // 谚文音节
        | 0x20000..=0x2FA1F  // CJK 扩展 B+
    );
    if is_cjk {
        Cls::Cjk
    } else if c.is_ascii_alphabetic() || c == '\'' || c == '\u{2019}' {
        Cls::Latin
    } else if c.is_ascii_digit() {
        Cls::Digit
    } else if c.is_whitespace() {
        Cls::Space
    } else {
        Cls::Punct
    }
}

/// 把一行文本切成扫光单元。
///
/// 规则：
/// - CJK 每字一个单元；
/// - 拉丁字母 / 数字按「连续同类」聚成词（`hello world` → 2 个单元）；
/// - 标点并入**前一个**单元（`你好，世界` → `你` `好，` `世` `界`）；
/// - 空格并入前一个单元（行首空格丢弃）；
/// - 空文本 → 空 `Vec`（调用方据此判断"这一行没有可扫的内容"）。
pub fn split_units(text: &str, start_ms: Ms, end_ms: Ms) -> Vec<LyricWord> {
    let mut units: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_cls = Cls::Space;

    for c in text.chars() {
        let cls = classify(c);
        match cls {
            Cls::Cjk => {
                if !cur.is_empty() {
                    units.push(core::mem::take(&mut cur));
                }
                units.push(c.to_string());
                cur_cls = cls;
            }
            Cls::Punct => {
                if !cur.is_empty() {
                    units.push(core::mem::take(&mut cur));
                }
                match units.last_mut() {
                    Some(last) => last.push(c),
                    // 行首就是标点：自成一个单元，否则会被吞掉
                    None => units.push(c.to_string()),
                }
                cur_cls = cls;
            }
            Cls::Space => {
                if !cur.is_empty() {
                    units.push(core::mem::take(&mut cur));
                }
                if let Some(last) = units.last_mut() {
                    last.push(c);
                }
                cur_cls = cls;
            }
            Cls::Latin | Cls::Digit => {
                if cur_cls != cls && !cur.is_empty() {
                    units.push(core::mem::take(&mut cur));
                }
                cur.push(c);
                cur_cls = cls;
            }
        }
    }
    if !cur.is_empty() {
        units.push(cur);
    }

    weight_units(units, start_ms, end_ms)
}

/// 按「单元字符数」加权，把 `[start_ms, end_ms]` 切给每个单元。
///
/// 等分（每个单元一样长）在纯中文行上没问题，但 `hello 你好` 这种混排会让
/// 5 个字母的单词和 1 个汉字各占同样时间——扫光速度会突变。按字符数加权后，
/// 每毫秒推进的字符数恒定，观感才是"匀速扫过"。
///
/// 时间**严格单调不减**（`start_ms <= end_ms`，且后一个的 start == 前一个的 end），
/// 这是 PRD P0-09「无跳跃、无回退」的数学保证。
fn weight_units(units: Vec<String>, start_ms: Ms, end_ms: Ms) -> Vec<LyricWord> {
    if units.is_empty() {
        return Vec::new();
    }
    let total_chars: usize = units.iter().map(|u| u.chars().count().max(1)).sum();
    let span = (end_ms - start_ms).max(units.len() as Ms);
    let mut out = Vec::with_capacity(units.len());
    let mut acc = 0usize;
    let mut prev = start_ms;
    for (i, text) in units.iter().enumerate() {
        let chars = text.chars().count().max(1);
        acc += chars;
        let end = if i + 1 == units.len() {
            start_ms + span
        } else {
            start_ms + span * acc as Ms / total_chars as Ms
        };
        let end = end.max(prev);
        out.push(LyricWord {
            text: text.clone(),
            start_ms: prev,
            end_ms: end,
        });
        prev = end;
    }
    out
}

/// 增强 LRC：字级标签只给了起始时间，用它把 `end_ms` 补齐。
///
/// 最后一个单元补到整行结束（下一行开始），保证「最后一个字也会被扫完」。
pub fn fill_word_ends(words: &mut [LyricWord], end_ms: Ms) {
    if words.is_empty() {
        return;
    }
    for i in 0..words.len() {
        let next_start = words.get(i + 1).map(|w| w.start_ms).unwrap_or(end_ms);
        // 防御：某些歌词站导出的时间戳会乱序/重复，这里强制单调
        words[i].end_ms = next_start.max(words[i].start_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(w: &[LyricWord]) -> Vec<&str> {
        w.iter().map(|x| x.text.as_str()).collect()
    }

    #[test]
    fn chinese_splits_per_character() {
        let w = split_units("我的心", 0, 3000);
        assert_eq!(texts(&w), vec!["我", "的", "心"]);
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn latin_splits_per_word() {
        let w = split_units("hello world", 0, 2000);
        // 空格并入前一个词，所以是 2 个单元
        assert_eq!(texts(&w), vec!["hello ", "world"]);
    }

    #[test]
    fn punctuation_joins_previous_unit() {
        let w = split_units("你好，世界", 0, 4000);
        // 4 个字 → 4 个单元；逗号并入前一个单元（"好，"）
        assert_eq!(texts(&w), vec!["你", "好，", "世", "界"]);
        assert_eq!(w.len(), 4);
    }

    #[test]
    fn leading_punctuation_becomes_its_own_unit() {
        let w = split_units("，你好", 0, 3000);
        assert_eq!(texts(&w), vec!["，", "你", "好"]);
    }

    #[test]
    fn digits_stay_together() {
        let w = split_units("第2024年", 0, 5000);
        assert_eq!(texts(&w), vec!["第", "2024", "年"]);
    }

    #[test]
    fn mixed_cjk_and_latin_are_weighted_by_chars() {
        // "ab 你好" → ["ab ", "你", "好"]，总字符 3+1+1 = 5
        let w = split_units("ab 你好", 0, 5000);
        assert_eq!(texts(&w), vec!["ab ", "你", "好"]);
        // "ab " 占 3/5 = 3000ms，而不是等分的 1666ms
        assert_eq!(w[0].start_ms, 0);
        assert_eq!(w[0].end_ms, 3000);
        assert_eq!(w[2].end_ms, 5000);
    }

    #[test]
    fn word_times_are_monotonic() {
        let w = split_units("我们一起去看海吧 hello world 2024", 12_000, 20_000);
        assert!(!w.is_empty());
        let mut prev_end = 12_000;
        for x in &w {
            assert!(x.start_ms >= prev_end - 1, "start 回退：{:?}", x);
            assert!(x.end_ms >= x.start_ms, "end < start：{:?}", x);
            prev_end = x.end_ms;
        }
        assert_eq!(w.last().map(|x| x.end_ms), Some(20_000));
    }

    #[test]
    fn empty_text_has_no_units() {
        assert!(split_units("", 0, 1000).is_empty());
        assert!(split_units("   ", 0, 1000).is_empty());
    }

    #[test]
    fn zero_length_line_still_gets_one_ms() {
        let w = split_units("嗨", 5000, 5000);
        assert_eq!(w.len(), 1);
        assert!(w[0].end_ms > w[0].start_ms);
    }

    #[test]
    fn fill_word_ends_uses_next_start_then_line_end() {
        let mut w = vec![
            LyricWord {
                text: "你".into(),
                start_ms: 0,
                end_ms: 0,
            },
            LyricWord {
                text: "好".into(),
                start_ms: 500,
                end_ms: 0,
            },
        ];
        fill_word_ends(&mut w, 1200);
        assert_eq!(w[0].end_ms, 500);
        assert_eq!(w[1].end_ms, 1200);
    }

    #[test]
    fn fill_word_ends_handles_unsorted_input() {
        let mut w = vec![
            LyricWord {
                text: "a".into(),
                start_ms: 900,
                end_ms: 0,
            },
            LyricWord {
                text: "b".into(),
                start_ms: 100,
                end_ms: 0,
            },
        ];
        fill_word_ends(&mut w, 2000);
        // 第二个的 start 更早 → 第一个的 end 被钳到自己的 start
        assert_eq!(w[0].end_ms, 900);
        assert_eq!(w[1].end_ms, 2000);
    }
}
