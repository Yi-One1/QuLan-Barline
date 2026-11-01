//! LRC / 增强 LRC 解析（`ARCHITECTURE.md` §12.1）。
//!
//! **手写状态机，不用正则**：正则版本要 3 个 `Regex` 静态量（额外依赖 + 首次编译开销），
//! 而且 `[00:12.34][00:40.00]副歌` 这种"一行多标签"用正则表达起来反而绕。
//!
//! 容错原则：**任何一行解析失败都只跳过这一行**，绝不 panic、绝不中断整首。
//! 整首解析完 `lines.is_empty()` → 调用方视为「没有歌词」。

use crate::lyric::interp::{fill_word_ends, split_units};
use crate::lyric::{LyricKind, LyricLine, LyricWord, Lyrics};
use crate::media::Ms;

/// 元数据累加器。
#[derive(Debug, Default)]
struct Meta {
    title: String,
    artist: String,
    album: String,
    by: String,
    /// `[offset:±ms]`，正值表示歌词提前
    offset_ms: Ms,
    /// 出现过字级标签 `<mm:ss.xx>`
    has_word_tags: bool,
}

/// 解析一份 LRC 文本。
///
/// `kind_hint` 只在**没有任何字级标签**时用来兜底（某些增强 LRC 的字标签被
/// 导出工具吃掉了，调用方仍希望按 Word 处理）。正常情况下 `kind` 自动判定。
pub fn parse(src: &str, kind_hint: LyricKind) -> Lyrics {
    let mut meta = Meta::default();
    // (start, 纯文本, 字级单元)
    let mut raw: Vec<(Ms, String, Vec<LyricWord>)> = Vec::new();

    for line in src.lines() {
        // BOM 只可能出现在第一行；trim_end 去掉 \r（CRLF 文件）
        let line = line.trim_start_matches('\u{feff}').trim_end();
        if line.is_empty() {
            continue;
        }

        // ---- 1) 吃掉行首连续的 [..] 标签 ----
        let mut cursor = 0usize;
        let mut stamps: Vec<Ms> = Vec::new();
        loop {
            let rest = &line[cursor..];
            if !rest.starts_with('[') {
                break;
            }
            let Some(close) = rest.find(']') else {
                break;
            };
            let inner = &rest[1..close];
            if let Some(ms) = parse_time_tag(inner) {
                stamps.push(ms);
                cursor += close + 1;
            } else if let Some((key, value)) = parse_meta(inner) {
                apply_meta(&mut meta, key, value);
                cursor += close + 1;
            } else {
                // 未知标签：停止扫描（正文里可能出现 `[xx]` 这种装饰）
                break;
            }
        }

        let body = &line[cursor..];
        if stamps.is_empty() {
            // 纯元数据行 / 纯正文行（没有时间戳）——都无法定位，跳过
            continue;
        }

        // ---- 2) 解析正文里的字级标签 ----
        let (plain, words) = split_word_tags(body);
        if words.len() >= 2 {
            meta.has_word_tags = true;
        }

        // ---- 3) 多标签展开 ----
        for st in stamps {
            raw.push((st, plain.clone(), words.clone()));
        }
    }

    // ---- 4) 排序 + 去重 ----
    raw.sort_by_key(|r| r.0);
    raw.dedup_by_key(|r| r.0);

    // ---- 5) 算 end_ms 并生成扫光单元 ----
    let n = raw.len();
    let mut lines: Vec<LyricLine> = Vec::with_capacity(n);
    for i in 0..n {
        let (start, text, words) = &raw[i];
        let start = *start;
        let end = if i + 1 < n { raw[i + 1].0 } else { start + 5000 };
        let words = if words.len() >= 2 {
            let mut w = words.clone();
            fill_word_ends(&mut w, end);
            w
        } else {
            split_units(text, start, end)
        };
        lines.push(LyricLine {
            start_ms: start,
            end_ms: end,
            text: text.clone(),
            words,
        });
    }

    // ---- 6) 应用 offset（正值 = 歌词提前 → 时间戳变小）----
    let off = meta.offset_ms;
    if off != 0 {
        for l in &mut lines {
            l.start_ms -= off;
            l.end_ms -= off;
            for w in &mut l.words {
                w.start_ms -= off;
                w.end_ms -= off;
            }
        }
    }

    let kind = if meta.has_word_tags {
        LyricKind::Word
    } else if !lines.is_empty() {
        LyricKind::Line
    } else {
        kind_hint
    };

    Lyrics {
        lines,
        kind,
        offset_ms: off,
        title: meta.title,
        artist: meta.artist,
        album: meta.album,
        by: meta.by,
    }
}

/// 解析 `[..]` 里的时间标签。
///
/// 支持 `mm:ss` / `mm:ss.x` / `mm:ss.xx` / `mm:ss.xxx` / `mm:ss:xx`（容错）。
/// 返回 `None` 表示不是时间标签（可能是 `ti:` 之类的元数据，或正文）。
pub fn parse_time_tag(inner: &str) -> Option<Ms> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }

    // 按 ':' 切；`[mm:ss:xx]` 这种把最后一个 ':' 当小数点
    let parts: Vec<&str> = inner.split(':').collect();
    let (mm_s, ss_s, frac_s) = match parts.len() {
        2 => (parts[0], parts[1], ""),
        // [mm:ss:xx] 容错：把最后一个 ':' 当小数点
        3 if parts[2].len() <= 2 && parts[2].chars().all(|c| c.is_ascii_digit()) => {
            (parts[0], parts[1], parts[2])
        }
        _ => return None,
    };

    let mm: Ms = parse_digits(mm_s)?;
    // ss 可能带小数
    let (ss, frac) = if frac_s.is_empty() {
        match ss_s.split_once('.') {
            Some((s, f)) => (parse_digits(s)?, f),
            None => (parse_digits(ss_s)?, ""),
        }
    } else {
        (parse_digits(ss_s)?, frac_s)
    };

    if ss > 59 {
        return None;
    }
    if mm > 999 {
        return None;
    }
    // 小数位：1 位 → 百分秒，2 位 → 百分秒，3 位 → 毫秒
    let frac_ms: Ms = if frac.is_empty() {
        0
    } else {
        if !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let v = parse_digits(frac)?;
        match frac.len() {
            1 => v * 100,
            2 => v * 10,
            3 => v,
            // 超过 3 位：截断到毫秒
            _ => {
                let head: String = frac.chars().take(3).collect();
                parse_digits(&head)?
            }
        }
    };

    Some(mm * 60_000 + ss * 1000 + frac_ms)
}

/// 只接受纯数字（空串 → None）。手写而不是 `parse()`，避免 `+`/`-`/空白被接受。
fn parse_digits(s: &str) -> Option<Ms> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut v: Ms = 0;
    for c in s.chars() {
        v = v.checked_mul(10)?.checked_add((c as u8 - b'0') as Ms)?;
        if v > 10_000_000 {
            return None;
        }
    }
    Some(v)
}

/// 解析 `ti:xxx` 这类元数据键值。
fn parse_meta(inner: &str) -> Option<(&str, &str)> {
    let (k, v) = inner.split_once(':')?;
    let k = k.trim();
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((k, v))
}

/// 元数据落到累加器。未知键直接忽略（`[re:]` `[ve:]` 之类）。
fn apply_meta(meta: &mut Meta, key: &str, value: &str) {
    let v = value.trim();
    match key.to_ascii_lowercase().as_str() {
        "ti" => meta.title = v.to_string(),
        "ar" => meta.artist = v.to_string(),
        "al" => meta.album = v.to_string(),
        "by" => meta.by = v.to_string(),
        "offset" => {
            // 可能带 '+'，也可能是负数
            let cleaned: String = v.chars().filter(|c| *c != '+').collect();
            if let Ok(n) = cleaned.trim().parse::<Ms>() {
                meta.offset_ms = n;
            }
        }
        _ => {}
    }
}

/// 拆出正文里的 `<mm:ss.xx>字` 序列。
///
/// 返回 `(纯文本, 单元)`。没有字级标签时返回 `(body, [])`。
fn split_word_tags(body: &str) -> (String, Vec<LyricWord>) {
    let mut plain = String::with_capacity(body.len());
    let mut words: Vec<LyricWord> = Vec::new();
    let mut rest = body;
    let mut cur: Option<(Ms, String)> = None;

    while let Some(lt) = rest.find('<') {
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            break;
        };
        let inner = &after[..gt];
        let Some(ms) = parse_time_tag(inner) else {
            // `<` 不是字级标签：当普通字符处理，继续往后找
            let head = &rest[..lt + 1];
            plain.push_str(head);
            if let Some((_, text)) = cur.as_mut() {
                text.push_str(head);
            }
            rest = after;
            continue;
        };
        // 标签**之前**的文本归当前单元（否则第一个标签前的正文会丢）
        let head = &rest[..lt];
        if !head.is_empty() {
            plain.push_str(head);
            if let Some((_, text)) = cur.as_mut() {
                text.push_str(head);
            }
        }
        // 收束上一个单元
        if let Some((start, text)) = cur.take() {
            if !text.is_empty() {
                words.push(LyricWord {
                    text,
                    start_ms: start,
                    end_ms: 0,
                });
            }
        }
        cur = Some((ms, String::new()));
        rest = &after[gt + 1..];
    }
    // 剩余尾巴
    plain.push_str(rest);
    if let Some((_, text)) = cur.as_mut() {
        text.push_str(rest);
    }
    if let Some((start, text)) = cur.take() {
        if !text.is_empty() {
            words.push(LyricWord {
                text,
                start_ms: start,
                end_ms: 0,
            });
        }
    }
    (plain, words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_digit_fraction() {
        assert_eq!(parse_time_tag("00:12.34"), Some(12_340));
    }

    #[test]
    fn parses_three_digit_fraction() {
        assert_eq!(parse_time_tag("00:12.345"), Some(12_345));
    }

    #[test]
    fn parses_no_fraction() {
        assert_eq!(parse_time_tag("00:12"), Some(12_000));
    }

    #[test]
    fn parses_colon_fraction_fallback() {
        assert_eq!(parse_time_tag("01:02:34"), Some(62_340));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_time_tag("abc"), None);
        assert_eq!(parse_time_tag("99:99.99"), None);
        assert_eq!(parse_time_tag(""), None);
        assert_eq!(parse_time_tag("ti:标题"), None);
    }

    #[test]
    fn multiple_tags_expand_to_multiple_lines() {
        let l = parse("[00:10.00][00:40.00]副歌\n[00:20.00]主歌", LyricKind::Line);
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[0].start_ms, 10_000);
        assert_eq!(l.lines[0].text, "副歌");
        assert_eq!(l.lines[1].start_ms, 20_000);
        assert_eq!(l.lines[2].start_ms, 40_000);
        assert_eq!(l.lines[2].text, "副歌");
    }

    #[test]
    fn word_tags_produce_word_kind() {
        let l = parse("[00:10.00]<00:10.00>你<00:10.50>好", LyricKind::Line);
        assert_eq!(l.kind, LyricKind::Word);
        assert_eq!(l.lines.len(), 1);
        let line = &l.lines[0];
        assert_eq!(line.text, "你好");
        assert_eq!(line.words.len(), 2);
        assert_eq!(line.words[0].text, "你");
        assert_eq!(line.words[0].start_ms, 10_000);
        assert_eq!(line.words[0].end_ms, 10_500);
        assert_eq!(line.words[1].start_ms, 10_500);
    }

    #[test]
    fn negative_offset_shifts_lyrics_later() {
        let l = parse("[offset:-500]\n[00:10.00]词", LyricKind::Line);
        assert_eq!(l.offset_ms, -500);
        assert_eq!(l.lines[0].start_ms, 10_500);
    }

    #[test]
    fn positive_offset_shifts_lyrics_earlier() {
        let l = parse("[offset:300]\n[00:10.00]词", LyricKind::Line);
        assert_eq!(l.lines[0].start_ms, 9_700);
        assert_eq!(l.lines[0].words[0].start_ms, 9_700);
    }

    #[test]
    fn metadata_is_collected_and_not_treated_as_lyrics() {
        let l = parse(
            "[ti:标题]\n[ar:歌手]\n[al:专辑]\n[by:某人]\n[00:01.00]第一句",
            LyricKind::Line,
        );
        assert_eq!(l.title, "标题");
        assert_eq!(l.artist, "歌手");
        assert_eq!(l.album, "专辑");
        assert_eq!(l.by, "某人");
        assert_eq!(l.lines.len(), 1);
    }

    #[test]
    fn invalid_lines_are_skipped() {
        let l = parse("[abc]\n[99:99.99]坏\n\n[00:02.00]好", LyricKind::Line);
        assert_eq!(l.lines.len(), 1);
        assert_eq!(l.lines[0].text, "好");
    }

    #[test]
    fn empty_source_yields_no_lines() {
        let l = parse("", LyricKind::Line);
        assert!(l.lines.is_empty());
        let l = parse("[ti:只有元数据]", LyricKind::Line);
        assert!(l.lines.is_empty());
    }

    #[test]
    fn duplicate_timestamps_are_deduped() {
        let l = parse("[00:01.00]A\n[00:01.00]B", LyricKind::Line);
        assert_eq!(l.lines.len(), 1);
    }

    #[test]
    fn line_end_is_next_start_and_last_is_plus_5s() {
        let l = parse("[00:01.00]A\n[00:03.00]B", LyricKind::Line);
        assert_eq!(l.lines[0].end_ms, 3_000);
        assert_eq!(l.lines[1].end_ms, 8_000);
    }

    #[test]
    fn blank_lyric_line_is_preserved() {
        // 间奏：`[00:05.00]` 后面没有正文，保留成空行（PRD：用于间奏显示空白）
        let l = parse("[00:01.00]A\n[00:05.00]", LyricKind::Line);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[1].text, "");
        assert!(l.lines[1].words.is_empty());
    }

    #[test]
    fn plain_text_without_tags_is_ignored() {
        let l = parse("这是一行没有时间戳的正文", LyricKind::Line);
        assert!(l.lines.is_empty());
    }

    #[test]
    fn crlf_and_bom_are_handled() {
        let l = parse("\u{feff}[00:01.00]A\r\n[00:02.00]B\r\n", LyricKind::Line);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "A");
    }

    #[test]
    fn words_are_monotonic_after_parse() {
        let l = parse("[00:00.00]我们去看海\n[00:04.00]下一句", LyricKind::Line);
        let w = &l.lines[0].words;
        assert_eq!(w.len(), 5);
        let mut prev = l.lines[0].start_ms;
        for x in w {
            assert!(x.start_ms >= prev);
            assert!(x.end_ms >= x.start_ms);
            prev = x.end_ms;
        }
        assert_eq!(w.last().map(|x| x.end_ms), Some(4_000));
    }
}
