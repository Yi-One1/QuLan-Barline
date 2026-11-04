//! 在线歌词落盘（v1.7.0 / G5）。
//!
//! 铁律：**只新建文件，绝不覆盖/修改/删除任何既有文件**
//! （唯一例外：删除自己刚 `create_new` 成功且写失败的那一个）。
//!
//! ## 为什么不用 `.tmp` + rename
//!
//! `std::fs::rename` 在 Windows 上是
//! `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，会覆盖既有目标，与「绝不覆盖」直接冲突。
//! `create_new`（`CREATE_NEW`）是唯一能**原子**表达「不存在才创建」的原语：
//! 两次并发写同一个名字时，只有一次拿到句柄，另一次拿 `AlreadyExists`。
//!
//! ## 为什么文件名必须复用 `locate::candidates`
//!
//! 落盘的目的是「下次能命中本地文件」。`locate::normalize_name` 会把
//! `\ / : * ? " < > |` 替换成**空格**再做比对；这里若把非法字符换成 `_`，
//! 写出来的文件名与查找侧的候选名就永远对不上——文件写成功了却永远命中不了。
//! 所以文件名一律取候选名本身（已归一化：小写、全角折半角、非法字符→空格、
//! 空白折叠），只补一个 `.lrc` 扩展名。
//!
//! ## 只写行级 LRC
//!
//! 输出只有 `[ti:]` / `[ar:]` / `[al:]` 三行元数据 + 每行一个 `[mm:ss.cc]` 时间戳，
//! **不写逐字标签**（`<mm:ss.cc>`）。逐字标签是增强 LRC，写出去会把「行级」歌词
//! 伪装成「字级」，而卡拉OK 扫光是靠 `interp` 插值算的，不需要也不应该依赖它。
//! 同理不写 `[offset:]`：`start_ms` 已经扣过 offset，再写一遍会二次偏移。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::lyric::locate;
use crate::lyric::Lyrics;
use crate::media::Track;

/// 落盘子目录名（固定写在「第一个本地歌词目录」下面）。
const SUB_DIR: &str = "Barline";
/// 文件名主干长度上限（UTF-16 码元）。超了直接放弃——不是截断，
/// 截断出来的名字和查找侧的候选名对不上，写下去也是废文件。
const MAX_STEM_UTF16: usize = 120;
/// 完整路径长度上限（UTF-16 码元，`MAX_PATH` 去掉结尾 NUL）。
const MAX_PATH_UTF16: usize = 259;

/// 落盘目标：目录 + 文件名（不含扩展名的部分已带 `.lrc`，见 [`file_name_for`]）。
pub struct Target {
    /// 目标目录（`<第一个本地歌词目录>/Barline`）
    pub dir: PathBuf,
    /// 目标文件名（含 `.lrc`）
    pub file: String,
}

/// 不落盘的原因。**每一个变体都必须在 [`save`] 里有真实构造路径**
/// （`#![deny(warnings)]` 之下，`dbg!`/日志都会读它，不会有"从未构造"的变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// `local_dirs` 空 / 首项 trim 后空 / 非绝对路径 / 不是目录
    NoDir,
    /// `locate::candidates` 为空（标题空）
    NoName,
    /// Windows 保留设备名（CON / PRN / AUX / NUL / COM1-9 / LPT1-9）
    Reserved,
    /// 文件名主干 > 120 码元，或完整路径 > 259 码元
    TooLong,
    /// `lyrics.lines` 为空（没什么可写）
    Empty,
    /// `create_new` 返回 `AlreadyExists`：同名文件已存在，按需求跳过
    Exists,
}

/// 一次落盘尝试的结果。
pub enum Outcome {
    /// 写入成功，带上最终路径（日志用）
    Written(PathBuf),
    /// 按规则跳过（正常路径，不是错误）
    Skipped(Skip),
    /// 真的失败了（IO 错误），带上原因。失败**静默降级**：只记日志。
    Failed(String),
}

/// 解析落盘目录：`local_dirs` 的**第一项**下的 `Barline` 子目录。
///
/// 返回 `None` 的情形（一个目录都不许建）：
/// - `local_dirs` 空 / 首项 trim 后空；
/// - 首项不是绝对路径（相对路径的「当前目录」取决于工作目录，不可靠）；
/// - 首项不存在或不是目录（用户删了 / 写错了——不替他造一个）。
pub fn resolve_target_dir(local_dirs: &[String]) -> Option<PathBuf> {
    let first = local_dirs.first()?.trim();
    if first.is_empty() {
        return None;
    }
    let dir = Path::new(first);
    if !dir.is_absolute() || !dir.is_dir() {
        return None;
    }
    Some(dir.join(SUB_DIR))
}

/// 由曲目生成落盘文件名（`<候选名>.lrc`）。
///
/// 候选名取 [`locate::candidates`] 的第一项（`歌手 - 歌名`），保证与本地查找
/// **机械等价**：写下去的名字就是查找时第一个会去比对的名字。
pub fn file_name_for(track: &Track) -> Option<String> {
    build_file_name(track).ok()
}

/// [`file_name_for`] 的实现体：把「为什么不行」也带出来，供 [`save`] 区分 `Skip`。
fn build_file_name(track: &Track) -> Result<String, Skip> {
    let raw = locate::candidates(track).into_iter().next().ok_or(Skip::NoName)?;
    // 不做尾随点裁剪（v1.7.0 出包前修正）：本模块拼出的名字是 `<stem>.lrc`，**末尾是扩展名**，
    // 而 Windows 只裁剪**整个文件名**末尾的点/空格，所以 `abc....lrc` 会原样保留全部点，
    // 其 `file_stem` 恰好等于 `candidates()[0]`（`abc...`）→ 能命中。
    // 反过来，若在此裁掉尾随点，`file_stem` 会变成 `abc`，与候选名不等 → 写了永远搜不到
    // （更糟：这个死文件还会被「歌名恰好是 `abc`」的另一首歌命中，显示错歌词）。
    //
    // 空名安全：`candidates()` 对空标题直接返回空 vec，所以走到这里的 `raw` 必然非空。
    let stem = raw.as_str();
    // 保留设备名：去扩展名后**全大写**比对（`con` / `Con.` / `CON` 都是保留名）。
    let upper = stem.to_ascii_uppercase();
    if is_reserved_name(&upper) {
        return Err(Skip::Reserved);
    }
    if stem.encode_utf16().count() > MAX_STEM_UTF16 {
        return Err(Skip::TooLong);
    }
    Ok(format!("{stem}.lrc"))
}

/// Windows 保留设备名判定（`stem` 必须已是 ASCII 大写）。
fn is_reserved_name(upper: &str) -> bool {
    if matches!(upper, "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    // COM1-9 / LPT1-9：恰好 4 个字符，前 3 个是 COM/LPT，第 4 个是 1..=9。
    // `com0` / `lpt0` 不是保留名（Windows 只保留 1-9）。
    if upper.len() == 4 && (upper.starts_with("COM") || upper.starts_with("LPT")) {
        let last = upper.as_bytes()[3];
        if last.is_ascii_digit() && last != b'0' {
            return true;
        }
    }
    false
}

/// 渲染行级 LRC 文本。
///
/// **只读 `line.start_ms` 与 `line.text`，绝不读 `line.words`**：逐字标签不写。
/// UTF-8 无 BOM + CRLF（Windows 记事本友好，也是绝大多数 .lrc 的现状）。
pub fn render_lrc(track: &Track, lyrics: &Lyrics) -> String {
    let mut out = String::new();
    // 元数据：空值整行省略；值里的 `]` / 换行必须换成空格，否则会破坏标签结构
    // （`[ti:a]b]` 之后的内容会被解析器当成正文，直接把第一行歌词顶掉）。
    for (tag, val) in [
        ("ti", &track.title),
        ("ar", &track.artist),
        ("al", &track.album),
    ] {
        let v = val.trim();
        if v.is_empty() {
            continue;
        }
        let v = v.replace([']', '\r', '\n'], " ");
        out.push_str(&format!("[{tag}:{v}]\r\n"));
    }
    for line in &lyrics.lines {
        // offset 过大时 `start_ms` 可能为负：钳到 0，绝不让 `[-` 出现在输出里
        // （负数的 `[-01:23.45]` 在大多数播放器里是非法标签）。
        let ms = line.start_ms.max(0) as u64;
        let (mm, ss, cs) = (ms / 60_000, ms / 1000 % 60, ms % 1000 / 10);
        // 正文里的换行换成空格：一个换行会把一行拆成两行，行数与时间轴都对不上
        let text = line.text.replace(['\r', '\n'], " ");
        // 空行照样输出（`[00:05.00]` 后面什么都不跟）：间奏要留着，
        // 少一行会让 `line_at` 的时间轴与歌曲错位。
        out.push_str(&format!("[{mm:02}:{ss:02}.{cs:02}]{text}\r\n"));
    }
    out
}

/// 落盘。返回 [`Outcome`]：**这个函数永远不 panic、不返回 `Err`**，
/// 失败只是 `Failed(String)`，由调用方记日志后继续。
///
/// 执行顺序即断言点：
/// 1. 空歌词直接跳过（连目录都不解析）；
/// 2. 先渲染（纯内存，无副作用）；
/// 3. 解析目标（目录 / 文件名 / 路径长度）；
/// 4. 到这一步才 `create_dir_all` ——「`local_dirs` 为空不建目录」靠这个顺序保证；
/// 5. `create_new` 打开（原子「不存在才创建」）；
/// 6. 写入 + flush，失败则删掉自己刚建的那个文件。
pub fn save(track: &Track, local_dirs: &[String], lyrics: &Lyrics) -> Outcome {
    if lyrics.lines.is_empty() {
        return Outcome::Skipped(Skip::Empty);
    }
    let content = render_lrc(track, lyrics);

    let target = match resolve_target(track, local_dirs) {
        Ok(t) => t,
        Err(skip) => return Outcome::Skipped(skip),
    };

    let path = target.dir.join(&target.file);
    // 超长路径直接放弃（不截断）：Win32 的 260 限制在未开长路径支持时是硬墙，
    // 强行写只会得到 `os error 206`，不如自己先判掉。
    if path.to_string_lossy().encode_utf16().count() > MAX_PATH_UTF16 {
        return Outcome::Skipped(Skip::TooLong);
    }

    // 惰性建目录：只有真的要写文件时才建 `<第一个目录>/Barline`
    if let Err(e) = std::fs::create_dir_all(&target.dir) {
        return Outcome::Failed(format!("建目录失败 {}：{e}", target.dir.display()));
    }

    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => f,
        // 已存在 = 正常跳过（**绝不覆盖**用户手改过的 .lrc）
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Outcome::Skipped(Skip::Exists)
        }
        Err(e) => return Outcome::Failed(format!("创建失败 {}：{e}", path.display())),
    };

    if let Err(e) = file.write_all(content.as_bytes()).and_then(|()| file.flush()) {
        // 只删自己刚 `create_new` 成功的那一个：留着半截文件会污染下次查找
        // （查找只看文件名，不看内容长短 —— 一个空 .lrc 就等于"这首歌没歌词了"）。
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Outcome::Failed(format!("写入失败 {}：{e}", path.display()));
    }

    Outcome::Written(path)
}

/// 解析完整落盘目标（目录 + 文件名），并把「路径超长」也算进来。
fn resolve_target(track: &Track, local_dirs: &[String]) -> Result<Target, Skip> {
    let dir = resolve_target_dir(local_dirs).ok_or(Skip::NoDir)?;
    let file = build_file_name(track)?;
    Ok(Target { dir, file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyric::LyricLine;

    /// 测试用曲目：标题一律用不可能存在的名字（`测试曲目α` 之类），
    /// 避免误命中用户真实的 `%USERPROFILE%\Music`。
    fn track(title: &str, artist: &str) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_ms: 200_000,
        }
    }

    fn line(start_ms: i64, text: &str) -> LyricLine {
        LyricLine {
            start_ms,
            end_ms: start_ms + 1000,
            text: text.into(),
            words: Vec::new(),
        }
    }

    /// 两行 + 一个空行（间奏）的最小歌词。
    fn lyrics() -> Lyrics {
        Lyrics {
            lines: vec![line(0, "第一句"), line(5_000, "")],
            ..Default::default()
        }
    }

    /// 独占的临时目录（先清空再建，避免上一轮的残留影响断言）。
    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("barline_writeback_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn clean(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn dirs_of(dir: &Path) -> Vec<String> {
        vec![dir.to_string_lossy().to_string()]
    }

    // ------------------------------------------------ 文件名（W1–W5）

    /// W1：落盘文件名与本地查找的**首选候选名**机械等价。
    #[test]
    fn w1_file_name_equals_first_candidate() {
        let t = track("晴天", "周杰伦");
        let first = locate::candidates(&t).first().cloned().expect("应有候选");
        let want = format!("{first}.lrc");
        assert_eq!(file_name_for(&t).as_deref(), Some(want.as_str()));
    }

    /// W2：非法字符替换成**空格**（不是 `_`）——否则写出来的名字永远命中不了。
    #[test]
    fn w2_illegal_chars_become_spaces_not_underscores() {
        let t = track("A/B:C*D?E", "");
        let first = locate::candidates(&t).first().cloned().expect("应有候选");
        let name = file_name_for(&t).expect("应能生成文件名");
        assert_eq!(name, format!("{first}.lrc"));
        assert!(!name.contains('_'), "非法字符必须换成空格，不能是下划线");
        assert!(name.contains(' '));
    }

    /// W3：Windows 保留设备名一律拒绝；`com0` / `lpt0` 不算保留名。
    #[test]
    fn w3_reserved_device_names_are_rejected() {
        assert_eq!(file_name_for(&track("CON", "")), None);
        assert_eq!(file_name_for(&track("nul", "")), None);
        assert_eq!(file_name_for(&track("com1", "")), None);
        assert_eq!(file_name_for(&track("lpt9", "")), None);
        assert_eq!(file_name_for(&track("aux", "")), None);
        assert_eq!(file_name_for(&track("prn", "")), None);
        // com0 / lpt0 不是保留名
        assert!(file_name_for(&track("com0", "")).is_some());
        assert!(file_name_for(&track("lpt0", "")).is_some());
    }

    /// W4：主干长度上限 120 码元（121 拒绝、120 通过）。
    #[test]
    fn w4_stem_length_limit_is_120() {
        let ok = "a".repeat(120);
        let bad = "a".repeat(121);
        let want = format!("{ok}.lrc");
        assert_eq!(file_name_for(&track(&ok, "")).as_deref(), Some(want.as_str()));
        assert_eq!(file_name_for(&track(&bad, "")), None);
    }

    /// W5：尾随点标题必须**落盘后能被自己搜到**（v1.7.0 出包前修正：
    /// 原断言「尾随点被裁掉」是错的——裁剪会让 `file_stem` 与 `candidates()[0]` 不等，
    /// 导致写了永远命中不了）。
    #[test]
    fn w5_trailing_dot_titles_round_trip() {
        let dir = tmp("w5");
        let dirs = dirs_of(&dir);
        for title in ["abc...", "..."] {
            let t = track(title, "");
            let name = file_name_for(&t).expect("尾随点标题必须能生成文件名");
            assert_eq!(
                name,
                format!("{}.lrc", locate::candidates(&t)[0]),
                "文件名主干必须**逐字**等于 candidates()[0]，不得裁剪"
            );
            let out = save(&t, &dirs, &lyrics());
            assert!(
                matches!(out, Outcome::Written(_)),
                "{title} 应该落盘成功，实际 {:?}",
                describe(&out)
            );
            // 闭环：立刻用现有 locate::find 找回来，且路径一致
            let hit = locate::find(&t, &dirs).expect("刚落盘的文件必须能被 locate::find 命中");
            assert_eq!(
                hit.path.file_name().and_then(|s| s.to_str()),
                Some(name.as_str())
            );
        }
        clean(&dir);
    }

    // ------------------------------------------------ 目录解析（W6）

    /// W6：目录解析的四条拒绝路径 + 一条通过路径。
    #[test]
    fn w6_resolve_target_dir_rules() {
        assert_eq!(resolve_target_dir(&[]), None, "空清单不落盘");
        assert_eq!(resolve_target_dir(&[String::from("   ")]), None, "空白项不落盘");
        assert_eq!(
            resolve_target_dir(&[String::from("sub")]),
            None,
            "相对路径不可靠，拒绝"
        );
        let missing = std::env::temp_dir().join("barline_writeback_no_such_dir_α");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(
            resolve_target_dir(&[missing.to_string_lossy().to_string()]),
            None,
            "不存在的目录不许替他造"
        );

        let dir = tmp("w6");
        assert_eq!(resolve_target_dir(&dirs_of(&dir)), Some(dir.join("Barline")));
        clean(&dir);
    }

    // ------------------------------------------------ 渲染（W7–W10）

    /// W7：行级 LRC 的形状——元数据打头、CRLF、无 BOM、无 offset、无逐字标签、空行保留。
    #[test]
    fn w7_render_is_line_level_lrc() {
        let mut t = track("测试曲目α", "无名歌手α");
        t.album = String::from("专辑α");
        let mut l = lyrics();
        l.lines.push(LyricLine {
            start_ms: 8_000,
            end_ms: 9_000,
            text: String::from("第二句"),
            // 故意给上逐字时间戳：渲染结果里**不许**出现 `<`
            words: vec![crate::lyric::LyricWord {
                text: String::from("第二句"),
                start_ms: 8_000,
                end_ms: 9_000,
            }],
        });
        let out = render_lrc(&t, &l);
        assert!(out.starts_with("[ti:"), "元数据必须在最前面");
        assert!(out.contains("\r\n"), "必须用 CRLF");
        assert!(!out.contains('\u{feff}'), "UTF-8 无 BOM");
        assert!(!out.contains("[offset:"), "不写 offset（start_ms 已扣过）");
        assert!(!out.contains('<'), "不写逐字标签");
        assert!(out.contains("[00:00.00]第一句\r\n"));
        assert!(out.contains("[00:05.00]\r\n"), "空行必须保留");
    }

    /// W8：`words` 非空与为空时输出**逐字节相同**——机械证明不读 `words`。
    #[test]
    fn w8_words_do_not_affect_output() {
        let t = track("测试曲目β", "无名歌手β");
        let plain = Lyrics {
            lines: vec![line(0, "你好世界"), line(2_500, "第二行")],
            ..Default::default()
        };
        let mut worded = plain.clone();
        worded.lines[0].words = vec![
            crate::lyric::LyricWord {
                text: String::from("你好"),
                start_ms: 0,
                end_ms: 1_250,
            },
            crate::lyric::LyricWord {
                text: String::from("世界"),
                start_ms: 1_250,
                end_ms: 2_500,
            },
        ];
        worded.lines[1].words = vec![crate::lyric::LyricWord {
            text: String::from("第二行"),
            start_ms: 2_500,
            end_ms: 3_500,
        }];
        assert_eq!(
            render_lrc(&t, &plain).as_bytes(),
            render_lrc(&t, &worded).as_bytes()
        );
    }

    /// W9：元数据里的 `]` / 换行换成空格；空元数据整行省略。
    #[test]
    fn w9_metadata_is_sanitized_and_empties_omitted() {
        let mut t = track("标题]带换行\n第二行", "");
        t.album = String::new();
        let out = render_lrc(&t, &lyrics());
        assert!(
            out.starts_with("[ti:标题 带换行 第二行]\r\n"),
            "`]` 与换行必须换成空格，实际：{}",
            out.lines().next().unwrap_or("")
        );
        assert!(!out.contains("[ar:"), "空歌手整行省略");
        assert!(!out.contains("[al:"), "空专辑整行省略");
    }

    /// W10：`start_ms` 为负时钳 0，输出里绝不出现 `[-`。
    #[test]
    fn w10_negative_start_ms_is_clamped_to_zero() {
        let out = render_lrc(
            &track("测试曲目γ", "无名歌手γ"),
            &Lyrics {
                lines: vec![line(-4_500, "负偏移行")],
                ..Default::default()
            },
        );
        assert!(!out.contains("[-"), "负时间戳必须钳到 0");
        assert!(out.contains("[00:00.00]负偏移行\r\n"), "该行仍必须在");
    }

    // ------------------------------------------------ 写入语义（W11–W15）

    /// W11：首次写入成功；第二次按 `Exists` 跳过且**字节不变**。
    #[test]
    fn w11_second_save_is_skipped_and_bytes_unchanged() {
        let dir = tmp("w11");
        let dirs = dirs_of(&dir);
        let t = track("测试曲目δ", "无名歌手δ");
        let l = lyrics();

        let path = match save(&t, &dirs, &l) {
            Outcome::Written(p) => p,
            other => panic!("首次应写入成功，实际 {:?}", describe(&other)),
        };
        assert!(path.exists(), "写成功后文件必须存在");
        let before = std::fs::read(&path).expect("读回");

        let second = save(&t, &dirs, &l);
        assert!(
            matches!(second, Outcome::Skipped(Skip::Exists)),
            "第二次必须按 Exists 跳过，实际 {:?}",
            describe(&second)
        );
        let after = std::fs::read(&path).expect("读回");
        assert_eq!(before, after, "跳过时绝不能改动文件内容");
        clean(&dir);
    }

    /// W12：**绝不覆盖**——用户手写的同名文件原样保留。
    #[test]
    fn w12_existing_user_file_is_never_overwritten() {
        let dir = tmp("w12");
        let t = track("测试曲目ε", "无名歌手ε");
        let sub = dir.join(SUB_DIR);
        std::fs::create_dir_all(&sub).expect("建子目录");
        let name = file_name_for(&t).expect("应有文件名");
        let path = sub.join(&name);
        std::fs::write(&path, "USER").expect("预置用户文件");

        let out = save(&t, &dirs_of(&dir), &lyrics());
        assert!(
            matches!(out, Outcome::Skipped(Skip::Exists)),
            "已存在必须跳过，实际 {:?}",
            describe(&out)
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("读回"),
            "USER",
            "用户手改过的 .lrc 绝不能被覆盖"
        );
        clean(&dir);
    }

    /// W13：`local_dirs` 为空 → 跳过且**连目录都不建**。
    #[test]
    fn w13_empty_local_dirs_writes_nothing_and_creates_no_dir() {
        let dir = tmp("w13");
        let out = save(&track("测试曲目ζ", "无名歌手ζ"), &[], &lyrics());
        assert!(
            matches!(out, Outcome::Skipped(Skip::NoDir)),
            "空清单必须按键 NoDir 跳过，实际 {:?}",
            describe(&out)
        );
        assert!(!dir.join(SUB_DIR).exists(), "不该创建任何目录");
        clean(&dir);
    }

    /// W14：空歌词跳过，且不产生任何文件。
    #[test]
    fn w14_empty_lyrics_are_skipped() {
        let dir = tmp("w14");
        let out = save(
            &track("测试曲目η", "无名歌手η"),
            &dirs_of(&dir),
            &Lyrics::default(),
        );
        assert!(
            matches!(out, Outcome::Skipped(Skip::Empty)),
            "空歌词必须按键 Empty 跳过，实际 {:?}",
            describe(&out)
        );
        let sub = dir.join(SUB_DIR);
        let count = match std::fs::read_dir(&sub) {
            Ok(rd) => rd.count(),
            Err(_) => 0,
        };
        assert_eq!(count, 0, "不许产生任何文件");
        clean(&dir);
    }

    /// W15（P0 闭环）：落盘后**立刻**能被本地查找命中，且命中的就是刚写的那个文件。
    ///
    /// 这条是整个特性的核心断言：写文件名若与查找侧的归一化口径有一丝偏差，
    /// 这里就会失败——那意味着落盘白写（用户永远等不到这次缓存的价值）。
    #[test]
    fn w15_saved_file_is_immediately_locatable() {
        let dir = tmp("w15");
        let t = track("测试曲目θ", "无名歌手θ");
        let dirs = dirs_of(&dir);
        let path = match save(&t, &dirs, &lyrics()) {
            Outcome::Written(p) => p,
            other => panic!("应写入成功，实际 {:?}", describe(&other)),
        };
        let hit = locate::find(&t, &dirs)
            .unwrap_or_else(|| panic!("落盘后必须能被本地查找命中：{}", path.display()));
        assert_eq!(hit.path, path, "命中的必须是刚落盘的那个文件");
        clean(&dir);
    }

    /// `Outcome` 没有 `Debug`（对外不要求），测试里自定义一个描述用的。
    fn describe(o: &Outcome) -> String {
        match o {
            Outcome::Written(p) => format!("Written({})", p.display()),
            Outcome::Skipped(s) => format!("Skipped({s:?})"),
            Outcome::Failed(e) => format!("Failed({e})"),
        }
    }
}
