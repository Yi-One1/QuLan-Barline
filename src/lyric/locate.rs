//! 本地歌词查找（`ARCHITECTURE.md` §12.4 的「假设 A1」）。
//!
//! **SMTC 不暴露音频文件路径**，所以没法做「与音频同目录同名」。
//! 替代方案：在一组歌词目录里按文件名匹配曲目。
//!
//! 搜索顺序（第一个命中即返回）：
//! 1. `config.lyrics.local_dirs`（用户自己加的目录）
//! 2. `%LOCALAPPDATA%\Barline\lyrics`（应用自带的歌词目录）
//! 3. `%USERPROFILE%\Music`（用户音乐库）
//!
//! 文件名候选：`歌手 - 歌名.lrc` / `歌名 - 歌手.lrc` / `歌名.lrc`。
//! 匹配**大小写不敏感**，并对全角符号与 `\/:*?"<>|` 做归一化。
//!
//! 遍历上限（`TASKS.md` T10 第 3 条）：深度 ≤ 4 层、总条目 ≤ 5000，
//! 避免用户把整个 D 盘加进来时卡住歌词线程。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::media::Track;

/// 递归深度上限。
const MAX_DEPTH: usize = 4;
/// 单次查找扫描的文件条目上限。
const MAX_ENTRIES: usize = 5000;
/// 歌词文件扩展名（小写）。
const EXT: &str = "lrc";

/// 命中的本地歌词。
#[derive(Debug, Clone)]
pub struct LocalHit {
    pub path: PathBuf,
    /// 日志用：`local:Music/xxx.lrc`
    pub source: String,
}

/// 归一化文件名：小写 + 去掉首尾空白 + 全角转半角 + 非法字符替换成空格。
///
/// 不做 Unicode 正规化（那需要额外依赖）；中文全角字符的 `NFKC` 折叠只覆盖
/// 标点与数字这些**在歌名里高频**的情况。
pub fn normalize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.trim().chars() {
        let c = match c {
            // 全角 ASCII（！ 到 ～）→ 半角
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        };
        // Windows 文件名非法字符 + 常见分隔符，统一成空格
        let c = if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            ' '
        } else {
            c
        };
        if c.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            for lower in c.to_lowercase() {
                out.push(lower);
            }
            last_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// 由曲目生成文件名候选（已归一化，**不含扩展名**）。
///
/// 顺序即优先级：`歌手 - 歌名` 最具体，`歌名` 最宽松。
pub fn candidates(track: &Track) -> Vec<String> {
    let title = normalize_name(&track.title);
    let artist = normalize_name(&track.artist);
    let mut out: Vec<String> = Vec::with_capacity(3);
    if title.is_empty() {
        return out;
    }
    if !artist.is_empty() {
        out.push(format!("{artist} - {title}"));
        out.push(format!("{title} - {artist}"));
    }
    out.push(title);
    out.dedup();
    out
}

/// 目录清单：用户目录 → 应用目录 → 音乐库。
pub fn search_dirs(user_dirs: &[String]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::with_capacity(user_dirs.len() + 2);
    for d in user_dirs {
        let p = PathBuf::from(d);
        if p.is_dir() {
            out.push(p);
        }
    }
    let app = crate::platform::barline_dir().join("lyrics");
    if app.is_dir() {
        out.push(app);
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let music = PathBuf::from(home).join("Music");
        if music.is_dir() {
            out.push(music);
        }
    }
    out
}

/// 在目录里查曲目对应的 `.lrc`。命中返回第一个。
pub fn find(track: &Track, user_dirs: &[String]) -> Option<LocalHit> {
    let cands = candidates(track);
    if cands.is_empty() {
        return None;
    }
    for dir in search_dirs(user_dirs) {
        if let Some(hit) = find_in(&dir, &cands) {
            return Some(hit);
        }
    }
    None
}

/// 在单个目录（递归）里查候选名。
fn find_in(root: &Path, cands: &[String]) -> Option<LocalHit> {
    let index = index_dir(root);
    for c in cands {
        if let Some(path) = index.get(c.as_str()) {
            return Some(LocalHit {
                source: format!("local:{}", path.display()),
                path: path.clone(),
            });
        }
    }
    None
}

/// 建「归一化文件名（不含扩展名） → 路径」索引。
///
/// 后写入的覆盖先写入的：同名文件时**浅层目录优先**（递归顺序保证父目录先入表）。
fn index_dir(root: &Path) -> HashMap<String, PathBuf> {
    let mut map: HashMap<String, PathBuf> = HashMap::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut budget = MAX_ENTRIES;

    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if budget == 0 {
                return map;
            }
            budget -= 1;
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push((path, depth + 1));
                continue;
            }
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case(EXT) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let key = normalize_name(stem);
            if key.is_empty() {
                continue;
            }
            // 深度更浅的优先：只在键不存在时插入
            map.entry(key).or_insert(path);
        }
    }
    map
}

/// 读一个歌词文件的文本（自动处理 UTF-8 BOM / UTF-16 BOM / GBK 兜底）。
///
/// 国内歌词站大量是 **GBK**，纯 `read_to_string` 会直接失败。
/// 这里不做完整编码探测（那要 +1 个依赖）：UTF-8 失败时按 GBK 近似处理
/// ——用 `String::from_utf8_lossy` 兜底会出乱码，所以改为「按字节丢弃非法序列」，
/// 保证至少能显示可读部分而不是整首空白。
pub fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    // UTF-16 LE BOM
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    // UTF-16 BE BOM
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&u16s));
    }
    match String::from_utf8(bytes) {
        Ok(s) => Some(s),
        // 非 UTF-8：按 GBK 解码。GBK 与 UTF-8 的字节范围有交集，
        // 这里只求"能读"，不做严格校验。
        Err(e) => {
            let bytes = e.into_bytes();
            Some(decode_gbk(&bytes))
        }
    }
}

/// 极简 GBK 解码：走 `MultiByteToWideChar(936)`。
fn decode_gbk(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};

    if bytes.is_empty() {
        return String::new();
    }
    // SAFETY: 只读 bytes 的 len 个字节；先问长度再分配，两步之间没有 TOCTOU 风险
    // （输入是我们自己持有的不可变切片）。CP_ACP 在本机是 936（简体中文）。
    let need = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
    if need <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut buf = vec![0u16; need as usize];
    // SAFETY: buf 长度为 need，正好是上一步返回的宽字符数。
    let written = unsafe {
        MultiByteToWideChar(
            CP_ACP,
            MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
            bytes,
            Some(&mut buf),
        )
    };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&buf[..written as usize])
    .trim_start_matches('\u{feff}')
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_ms: 200_000,
        }
    }

    #[test]
    fn candidates_are_ordered_most_specific_first() {
        let c = candidates(&track("晴天", "周杰伦"));
        assert_eq!(c, vec!["周杰伦 - 晴天", "晴天 - 周杰伦", "晴天"]);
    }

    #[test]
    fn candidates_without_artist_only_title() {
        let c = candidates(&track("晴天", ""));
        assert_eq!(c, vec!["晴天"]);
    }

    #[test]
    fn candidates_for_empty_title_are_empty() {
        assert!(candidates(&track("   ", "周杰伦")).is_empty());
    }

    #[test]
    fn normalize_is_case_insensitive_and_strips_illegal_chars() {
        assert_eq!(normalize_name("Hello World"), "hello world");
        assert_eq!(normalize_name("A/B:C*D?E"), "a b c d e");
        assert_eq!(normalize_name("  多  空格  "), "多 空格");
        assert_eq!(normalize_name(""), "");
    }

    #[test]
    fn normalize_folds_fullwidth_ascii() {
        // 全角数字与字母
        assert_eq!(normalize_name("ＡＢＣ１２３"), "abc123");
        // 全角空格
        assert_eq!(normalize_name("a\u{3000}b"), "a b");
    }

    #[test]
    fn find_hits_title_and_artist_title_in_temp_dir() {
        let dir = std::env::temp_dir().join("barline_locate_test");
        let _ = std::fs::create_dir_all(&dir);
        let a = dir.join("周杰伦 - 晴天.lrc");
        let b = dir.join("简单爱.lrc");
        let _ = std::fs::write(&a, "[00:01.00]x");
        let _ = std::fs::write(&b, "[00:01.00]y");

        let dirs = vec![dir.to_string_lossy().to_string()];
        let hit = find(&track("晴天", "周杰伦"), &dirs);
        assert!(hit.is_some(), "artist - title 未命中");
        assert!(hit.map(|h| h.path.ends_with("周杰伦 - 晴天.lrc")).unwrap_or(false));

        let hit = find(&track("简单爱", ""), &dirs);
        assert!(hit.is_some(), "title 未命中");

        let miss = find(&track("不存在的歌", "无名"), &dirs);
        assert!(miss.is_none());

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn find_is_case_insensitive_on_disk() {
        let dir = std::env::temp_dir().join("barline_locate_case");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("Hello World.lrc");
        let _ = std::fs::write(&p, "[00:01.00]x");
        let dirs = vec![dir.to_string_lossy().to_string()];
        assert!(find(&track("hello world", ""), &dirs).is_some());
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn read_text_handles_utf8_and_utf16_bom() {
        let dir = std::env::temp_dir().join("barline_read_test");
        let _ = std::fs::create_dir_all(&dir);

        let utf8 = dir.join("u8.lrc");
        let _ = std::fs::write(&utf8, "[00:01.00]你好");
        assert_eq!(read_text(&utf8).as_deref(), Some("[00:01.00]你好"));

        let utf16 = dir.join("u16.lrc");
        let mut bytes = vec![0xFF, 0xFE];
        for u in "[00:01.00]你好".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let _ = std::fs::write(&utf16, &bytes);
        assert_eq!(read_text(&utf16).as_deref(), Some("[00:01.00]你好"));

        let _ = std::fs::remove_file(&utf8);
        let _ = std::fs::remove_file(&utf16);
        let _ = std::fs::remove_dir(&dir);
    }
}
