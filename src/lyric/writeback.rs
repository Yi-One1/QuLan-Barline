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

/// Windows 文件名非法字符（与 `locate::normalize_name` **同一套**，见 `locate.rs`）。
///
/// 两处必须字面一致：查找侧把它们换成空格再做比对，写入侧若换成别的
/// （比如 `_`），写出来的名字就永远命中不了（v1.7.0 已踩过的坑）。
const ILLEGAL: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// 落盘目标：目录 + 文件名（不含扩展名的部分已带 `.lrc`，见 [`build_file_name`]）。
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
    /// **写出去也永远命中不了**（v1.8.0）：命名不变式不成立时宁可跳过，
    /// 也不写一个死文件。触发场景见 [`display_stem`] 的注释。
    Unsearchable,
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

/// 落盘文件名主干：`{歌名} - {歌手}`（保留原始大小写 / 全角），无歌手时退化为 `{歌名}`。
///
/// 三条铁律（任一条不满足就 Skip，**绝不写「永远命中不了的死文件」**）：
/// 1. 非法字符与控制字符 → **空格**（绝不是 `_`，见 [`ILLEGAL`]）；
/// 2. 保留设备名 / 主干 > 120 码元 → Skip（不截断：截断出来的名字与候选名对不上）；
/// 3. 归一化后必须落在 `locate::candidates(track)` 里（否则 [`Skip::Unsearchable`]）。
///
/// 为什么第 3 条要存在（它是本轮的「兜底一击」）：查找侧用
/// `normalize_name(file_stem)` 建索引（`locate.rs` 的 `index_dir`），所以
/// 「写出去能不能被搜到」等价于 `normalize_name(stem) ∈ candidates(track)`。
/// 已知的不成立场景是 `title`/`artist` 含**非空白控制字符**
/// （U+0001–U+0008 等）：`normalize_name` 不认识它们（原样保留），而
/// [`sanitize`] 必须把它们换成空格（Windows 文件名禁止 1–31），两边必然不等。
///
/// ## title-only 回退（v1.8.0 出包前追加裁决）
///
/// `artist` 归一化后为空（播放器把歌手报成 `/`、`／`、`?` 这类）会让主路径
/// `{title} - {artist}` 的不变式不成立。此时**回退到 `{歌名}` 主干**而不是整首丢掉：
/// 那是 v1.7.0 的既有行为，也正是 `candidates` 的第 3 项，命中链本来就是通的。
///
/// 回退的边界（三条，都不许放宽）：
/// - **只为 [`Skip::Unsearchable`] 触发**：`Reserved` / `TooLong` 说明 title 本身有问题
///   （`CON`、121 码元），换成 title-only 也救不了 → 不回退；
/// - **回退要再走一遍同一套校验**（[`check_stem`]，与主路径共用，不许复制）：
///   回退后仍不成立（如 title 含 `\u{1}` → `n("a b")` 不在候选里）→ 仍然 Skip；
/// - 无歌手时主路径本来就是 title-only，没有「回退」这回事。
pub fn display_stem(track: &Track) -> Result<String, Skip> {
    // 查找侧口径：先判「候选名是否为空」（title 归一化后为空 → 一个候选都没有）
    let cands = locate::candidates(track);
    if cands.is_empty() {
        return Err(Skip::NoName);
    }
    let title = sanitize(&track.title);
    if title.is_empty() {
        return Err(Skip::NoName);
    }
    let artist = sanitize(&track.artist);
    // 无歌手 → 只写歌名（不产出 " - 歌名"）
    if artist.is_empty() {
        check_stem(&title, &cands)?;
        return Ok(title);
    }

    let stem = format!("{title} - {artist}");
    match check_stem(&stem, &cands) {
        Ok(()) => Ok(stem),
        // 唯一允许回退的失败原因：名字本身合法，只是「写出去搜不到」
        Err(Skip::Unsearchable) => {
            check_stem(&title, &cands)?;
            Ok(title)
        }
        // Reserved / TooLong：title 自己有问题，回退成 title-only 也救不了
        Err(other) => Err(other),
    }
}

/// 主干校验：保留设备名 / 长度上限 / 命名不变式。
///
/// **主路径与 title-only 回退路径共用这一份**（`writeback.rs` 里只有一个地方定义
/// 「什么算合法主干」）。任何一条不过就给出对应的 [`Skip`]，由调用方决定是回退还是放弃。
fn check_stem(stem: &str, cands: &[String]) -> Result<(), Skip> {
    // 保留设备名：全大写比对（`con` / `Con.` / `CON` 都是保留名）
    let upper = stem.to_ascii_uppercase();
    if is_reserved_name(&upper) {
        return Err(Skip::Reserved);
    }
    if stem.encode_utf16().count() > MAX_STEM_UTF16 {
        return Err(Skip::TooLong);
    }
    // 硬不变式：写出去必须能被 locate::find 命中
    let norm = locate::normalize_name(stem);
    if !cands.iter().any(|c| c == &norm) {
        return Err(Skip::Unsearchable);
    }
    Ok(())
}

/// 清理文件名主干：只做两件必要的事，**大小写、全角字符、中文标点一律保留**。
///
/// 1. Windows 非法字符（[`ILLEGAL`]，与 `locate::normalize_name` 同一套）+ 控制字符
///    （U+0000–U+001F）→ **空格**；
/// 2. 空白折叠（连续空白 → 单个空格，覆盖 U+3000 / NBSP / `\t` / `\n`）+ 首尾 trim。
///
/// 不做全角折半角、不做小写化：这两件事由查找侧的 `normalize_name` 在比对时完成，
/// 所以「文件名保留原样」与「能被命中」并不冲突（全角斜杠 `／` 就是典型：
/// 文件名里保留它，`normalize_name` 在两侧都把它折成空格，比对仍然相等）。
///
/// **不做尾随点裁剪**（v1.7.0 出包前修正，v1.8.0 继续）：本模块拼出的是
/// `<stem>.lrc`，末尾是扩展名，而 Windows 只裁剪**整个文件名**末尾的点/空格，
/// 所以 `abc....lrc` 会原样保留，其 `file_stem`（`abc...`）恰好等于候选名 → 能命中。
/// 若在此裁掉尾随点，`file_stem` 会变成 `abc`，与候选名不等 → 写了永远搜不到
/// （更糟：这个死文件还会被「歌名恰好是 `abc`」的另一首歌命中，显示错歌词）。
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        // ① 非法字符 + 控制字符 → 空格
        let c = if ILLEGAL.contains(&c) || (c as u32) < 0x20 {
            ' '
        } else {
            c
        };
        // ② 空白折叠（首部空白天然被 `!out.is_empty()` 挡掉）
        if c.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// [`display_stem`] + `.lrc`，供 [`save`] 区分 `Skip`。
fn build_file_name(track: &Track) -> Result<String, Skip> {
    display_stem(track).map(|stem| format!("{stem}.lrc"))
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

    /// W1：落盘文件名主干 = `歌名 - 歌手`（保留大小写）。
    ///
    /// **v1.8.0 R4 改口径**：v1.7.0 断言的是 `candidates()[0]`（= `歌手 - 歌名`，已小写）；
    /// 本轮起改为 `歌名 - 歌手` 的**原始大小写**，且不变式相应地从「等于 `[0]`」
    /// 放宽为「落在 `candidates` 里」（见 W20 / W21）。
    #[test]
    fn w1_file_name_equals_first_candidate() {
        let t = track("晴天", "周杰伦");
        let second = locate::candidates(&t).get(1).cloned().expect("应有第二候选");
        assert_eq!(second, "晴天 - 周杰伦", "候选顺序本身不许动（查找侧红线）");
        assert_eq!(
            build_file_name(&t).as_deref(),
            Ok("晴天 - 周杰伦.lrc"),
            "v1.8.0 R4：文件名主干 = 歌名 - 歌手，不是 candidates()[0]"
        );
    }

    /// W2：非法字符替换成**空格**（不是 `_`）——否则写出来的名字永远命中不了。
    ///
    /// **v1.8.0 R4 改口径**：断言从「等于 `candidates()[0] + .lrc`」改为具体名字。
    #[test]
    fn w2_illegal_chars_become_spaces_not_underscores() {
        let t = track("A/B:C*D?E", "");
        let name = build_file_name(&t).expect("应能生成文件名");
        assert_eq!(name, "A B C D E.lrc");
        assert!(!name.contains('_'), "非法字符必须换成空格，不能是下划线");
        assert!(name.contains(' '));
        // 相对查找侧仍然命中：归一化后等于唯一的候选名
        assert_eq!(locate::candidates(&t), vec![String::from("a b c d e")]);
    }

    /// W3：Windows 保留设备名一律拒绝；`com0` / `lpt0` 不算保留名。
    #[test]
    fn w3_reserved_device_names_are_rejected() {
        assert_eq!(build_file_name(&track("CON", "")), Err(Skip::Reserved));
        assert_eq!(build_file_name(&track("nul", "")), Err(Skip::Reserved));
        assert_eq!(build_file_name(&track("com1", "")), Err(Skip::Reserved));
        assert_eq!(build_file_name(&track("lpt9", "")), Err(Skip::Reserved));
        assert_eq!(build_file_name(&track("aux", "")), Err(Skip::Reserved));
        assert_eq!(build_file_name(&track("prn", "")), Err(Skip::Reserved));
        // com0 / lpt0 不是保留名
        assert!(build_file_name(&track("com0", "")).is_ok());
        assert!(build_file_name(&track("lpt0", "")).is_ok());
    }

    /// W4：主干长度上限 120 码元（121 拒绝、120 通过）。
    #[test]
    fn w4_stem_length_limit_is_120() {
        let ok = "a".repeat(120);
        let bad = "a".repeat(121);
        let want = format!("{ok}.lrc");
        assert_eq!(build_file_name(&track(&ok, "")).as_deref(), Ok(want.as_str()));
        assert_eq!(build_file_name(&track(&bad, "")), Err(Skip::TooLong));
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
            let name = build_file_name(&t).expect("尾随点标题必须能生成文件名");
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
        let name = build_file_name(&t).expect("应有文件名");
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

    /// W13：`local_dirs` 为空 → 跳过，且**目标目录一个字节都不许动**。
    ///
    /// **v1.8.1（G6 / D-3）修掉一条恒真断言**：原写法是
    /// `assert!(!dir.join(SUB_DIR).exists())`，但 `save` 拿到的第二个参数是空切片，
    /// 它根本不可能知道 `dir` 存在 —— 那条断言改坏 `save` 也永远为真。
    /// 现在把 `dir` 预置一个**哨兵文件**，再断言「目录里仍然只有哨兵」：
    /// 只要 `save` 在空清单路径上碰了任何目录（哪怕将来改成「兜底用临时目录」），
    /// 条目数就会变 → 断言立刻红。
    #[test]
    fn w13_empty_local_dirs_writes_nothing_and_creates_no_dir() {
        let dir = tmp("w13");
        let sentinel = dir.join("sentinel.txt");
        std::fs::write(&sentinel, "KEEP").expect("预置哨兵文件");
        let out = save(&track("测试曲目ζ", "无名歌手ζ"), &[], &lyrics());
        assert!(
            matches!(out, Outcome::Skipped(Skip::NoDir)),
            "空清单必须按键 NoDir 跳过，实际 {:?}",
            describe(&out)
        );
        assert_eq!(
            std::fs::read_dir(&dir).expect("目录应可读").count(),
            1,
            "空清单下 save 不得创建任何东西（目录里应仍只有哨兵文件）"
        );
        assert!(!dir.join(SUB_DIR).exists(), "不该创建 Barline 子目录");
        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("哨兵文件必须还在"),
            "KEEP",
            "哨兵文件内容不许被改动"
        );
        clean(&dir);
    }

    /// W14：空歌词跳过，且**连 `Barline/` 目录都不建**。
    ///
    /// **v1.8.1（G6 / D-4）拆开一条弱断言**：原写法
    /// `match read_dir(&sub) { Ok(rd) => rd.count(), Err(_) => 0 }` 把「目录不存在」
    /// 与「目录存在但是空的」合并成同一个 0，于是「建了个空目录」这种回归抓不到
    /// —— 而 W13 的主张正是「这条路径连目录都不建」。现在两条分开断言。
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
        assert!(
            !sub.exists(),
            "空歌词不该建 Barline 目录：`save` 必须在解析目标目录之前就早退"
        );
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

    // ================================================ v1.8.0 / R4：display_stem（W16–W20）

    /// W16：A8.1 的四条命名断言（表驱动）。
    #[test]
    fn w16_display_stem_title_dash_artist_table() {
        for (title, artist, want) in [
            ("晴天", "周杰伦", "晴天 - 周杰伦"),
            ("Hello World", "Adele", "Hello World - Adele"),
            ("A/B:C*D", "X", "A B C D - X"),
            ("晴天", "", "晴天"),
        ] {
            let got = display_stem(&track(title, artist));
            assert_eq!(
                got.as_deref(),
                Ok(want),
                "title={title:?} artist={artist:?} 的主干不对：{got:?}"
            );
        }
    }

    /// W17：保留原始大小写与全角字符（只有查找侧归一化时才折平）。
    #[test]
    fn w17_display_stem_keeps_case_and_fullwidth() {
        assert_eq!(
            display_stem(&track("hello world", "adele")).as_deref(),
            Ok("hello world - adele"),
            "输入是小写就必须原样保留小写"
        );
        assert_eq!(
            display_stem(&track("Hello World", "Adele")).as_deref(),
            Ok("Hello World - Adele"),
            "大小写必须按上报原样保留"
        );
        assert_eq!(
            display_stem(&track("晴天（Live）", "周杰伦")).as_deref(),
            Ok("晴天（Live） - 周杰伦"),
            "全角括号必须保留"
        );
        // 全角折平只发生在查找侧 → 归一化后仍等于候选名
        let t = track("晴天（Live）", "周杰伦");
        let stem = display_stem(&t).expect("应能生成主干");
        assert_eq!(
            locate::normalize_name(&stem),
            locate::candidates(&t)[1],
            "文件名保留全角，但归一化口径必须与查找侧一致"
        );
    }

    /// W18：非法字符与控制字符 → **空格**（绝不是 `_`），连续空白折叠成一个。
    ///
    /// 控制字符那几条只能直接断言 `sanitize`：`normalize_name` 不认识控制字符，
    /// 这类标题的命名不变式本来就不成立 → `display_stem` 按设计返回
    /// `Err(Unsearchable)`（见 W22）。
    #[test]
    fn w18_display_stem_sanitizes_illegal_and_control() {
        assert_eq!(
            display_stem(&track("A/B:C*D?E\"F<G>H|I", "")).as_deref(),
            Ok("A B C D E F G H I"),
            "九个非法字符都要变成空格"
        );
        let name = build_file_name(&track("A/B:C*D?E\"F<G>H|I", "")).expect("应有文件名");
        assert!(!name.contains('_'), "必须是空格，不能是下划线");
        for (raw, want) in [
            ("a\u{1}b", "a b"),
            ("a\tb", "a b"),
            ("a\r\nb", "a b"),
            ("  a   b  ", "a b"),
            ("a\u{3000}b", "a b"),
        ] {
            assert_eq!(sanitize(raw), want, "raw={raw:?}");
        }
    }

    /// W19：保留设备名与长度上限（沿用 W3/W4 的语义）。
    #[test]
    fn w19_display_stem_reserved_and_length() {
        for name in ["CON", "nul", "com1", "lpt9"] {
            assert_eq!(
                display_stem(&track(name, "")),
                Err(Skip::Reserved),
                "{name} 是 Windows 保留设备名"
            );
        }
        for name in ["com0", "lpt0"] {
            assert!(
                display_stem(&track(name, "")).is_ok(),
                "{name} 不是保留名（Windows 只保留 1-9）"
            );
        }
        let ok = "a".repeat(120);
        let bad = "a".repeat(121);
        assert_eq!(display_stem(&track(&ok, "")).as_deref(), Ok(ok.as_str()));
        assert_eq!(display_stem(&track(&bad, "")), Err(Skip::TooLong));
    }

    /// W20：**命名不变式**——`normalize_name(stem)` 必须落在 `candidates(track)` 里，
    /// 且（除「歌名==歌手」那种被 `dedup` 折叠的情形）恰好等于 `candidates[1]`。
    ///
    /// 表里第 3 列是期望下标：`candidates` 用它做精确断言（比「只判 membership」更强）。
    #[test]
    fn w20_display_stem_invariant_membership() {
        // (title, artist, 期望在 candidates 里的下标)
        let table: [(&str, &str, usize); 12] = [
            ("晴天", "", 0),                  // 1 无歌手
            ("Hello World", "Adele", 1),      // 2 大小写
            ("晴天 ", "周杰伦", 1),           // 3 尾空格
            ("A/B:C*D", "X", 1),              // 4 非法字符
            ("A／B", "", 0),                  // 5 全角斜杠（归一化成 "a b"）
            ("第一行\n第二行", "X", 1),       // 6 换行
            ("CON", "X", 1),                  // 8 「CON - X」不是保留名
            ("晴天（Live）", "周杰伦", 1),    // 12 全角括号
            ("晴天", "   ", 0),               // 14 纯空白歌手
            ("晴天", "晴天", 0),              // 16 歌名==歌手 → dedup 折叠，只走 membership
            ("abc...", "", 0),                // 17 尾点不裁
            ("晴天 - ", "周杰伦", 1),         // 18 标题自带 " - "
        ];
        for (title, artist, want_idx) in table {
            let t = track(title, artist);
            let stem = display_stem(&t)
                .unwrap_or_else(|e| panic!("title={title:?} artist={artist:?} 应能命名，实际 {e:?}"));
            let norm = locate::normalize_name(&stem);
            let cands = locate::candidates(&t);
            assert!(
                cands.iter().any(|c| c == &norm),
                "title={title:?} artist={artist:?}：n(stem)={norm:?} 不在 {cands:?} 里",
            );
            assert_eq!(
                cands.get(want_idx),
                Some(&norm),
                "title={title:?} artist={artist:?}：n(stem)={norm:?} 应等于 candidates[{want_idx}]"
            );
        }
    }

    // ================================================ v1.8.0 / R4：闭环与边界（W21–W25）

    /// W21（A8.3 **闭环**）：落盘返回的路径 → `locate::find` 必须命中**同一路径**。
    ///
    /// 这比「文件名等于某个字符串」强：`find_in` 拿 `index_dir` 建的
    /// `normalize_name(file_stem) → path` 索引去查 `candidates`，所以「命中同一路径」
    /// 等价于「写名字时用的归一化口径与查找侧完全一致」。
    #[test]
    fn w21_saved_file_found_at_same_path() {
        let table = [
            ("Hello World Ωmega", "Adele Ωmega"), // 大小写
            ("A／B β", ""),                       // 全角斜杠
            ("晴天：第一 ξ", "周杰伦 ξ"),         // 全角冒号
            ("晴天\u{3000}夜 ω", ""),             // 全角空格
            ("尾点曲 ψ", ""),                     // 无歌手
            ("晴天（Live） χ", "周杰伦 χ"),       // 全角括号
            ("只有歌名 φ", ""),                   // 无歌手
            ("长名 τ", ""),                       // 长名（下面单独补 120 码元那例）
        ];
        for (i, (title, artist)) in table.iter().enumerate() {
            let dir = tmp(&format!("w21_{i}"));
            let dirs = dirs_of(&dir);
            let t = track(title, artist);
            let path = match save(&t, &dirs, &lyrics()) {
                Outcome::Written(p) => p,
                other => panic!("{title:?} 应写入成功，实际 {:?}", describe(&other)),
            };
            let hit = locate::find(&t, &dirs)
                .unwrap_or_else(|| panic!("{title:?} 落盘后必须能被命中：{}", path.display()));
            assert_eq!(hit.path, path, "{title:?} 命中的必须是刚落盘的那个文件");
            clean(&dir);
        }

        // 恰好 120 码元（主干上限）也要走通完整闭环
        let dir = tmp("w21_max");
        let dirs = dirs_of(&dir);
        let t = track(&"a".repeat(120), "");
        let path = match save(&t, &dirs, &lyrics()) {
            Outcome::Written(p) => p,
            other => panic!("120 码元应写入成功，实际 {:?}", describe(&other)),
        };
        let hit = locate::find(&t, &dirs).expect("120 码元落盘后必须能被命中");
        assert_eq!(hit.path, path);
        clean(&dir);
    }

    /// W22（**v1.8.0 出包前改口径**：title-only 回退）：`artist` 归一化后为空时，
    /// 主路径 `{歌名} - {歌手}` 的不变式不成立 → **回退到 `{歌名}`** 落一份，而不是整首丢掉。
    ///
    /// 断言是**闭环**的：`save` 返回的路径 → `locate::find` 必须命中同一路径。
    /// 这比「文件名等于 `晴天.lrc`」强：它同时证明「回退出来的名字能被查找侧认出来」。
    /// 回退的目标名就是 v1.7.0 的既有产物（= `candidates` 的第 3 项），命中链本来通的。
    #[test]
    fn w22_artist_only_punctuation_falls_back_to_title() {
        let dir = tmp("w22");
        let dirs = dirs_of(&dir);
        // 歌手字段只有一个全角斜杠：sanitize 保留它 → 主路径 "晴天 - ／" 归一化成 "晴天 -"，
        // 不在 candidates(["晴天"]) 里 → 触发回退
        let t = track("晴天", "／");
        assert_eq!(
            display_stem(&t).as_deref(),
            Ok("晴天"),
            "必须回退到 title-only 主干"
        );
        assert_eq!(build_file_name(&t).as_deref(), Ok("晴天.lrc"));

        let path = match save(&t, &dirs, &lyrics()) {
            Outcome::Written(p) => p,
            other => panic!("回退后应写入成功，实际 {:?}", describe(&other)),
        };
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("晴天.lrc"),
            "回退主干 + .lrc"
        );
        let hit = locate::find(&t, &dirs).expect("回退写出的文件必须能被 locate::find 命中");
        assert_eq!(hit.path, path, "命中的必须就是刚回退写出的那个文件");
        clean(&dir);
    }

    /// W26：**回退不是免检通道**——title 含非空白控制字符时，主路径与回退路径
    /// 都过不了命名不变式 → `Skip::Unsearchable`，且 `Barline/` 里 0 个文件。
    ///
    /// 为什么这条必须钉住：`normalize_name` 不认识 `\u{1}`（原样留在候选名里），
    /// 而 `sanitize` 必须把它换成空格（Windows 文件名禁止 1–31）→
    /// `n("a b")` 永远不在 `candidates(["a\u{1}b"])` 里。若有人为了让「尽量落一份」
    /// 而放宽校验，这里会写一个 `a b.lrc` 的死文件，下一轮播放就再也命中不了。
    #[test]
    fn w26_title_with_control_char_still_skips() {
        let t = track("a\u{1}b", "X");
        assert_eq!(
            display_stem(&t),
            Err(Skip::Unsearchable),
            "回退路径同样要过校验：n(\"a b\") 不在候选里，必须 Skip"
        );
        let dir = tmp("w26");
        let out = save(&t, &dirs_of(&dir), &lyrics());
        assert!(
            matches!(out, Outcome::Skipped(Skip::Unsearchable)),
            "实际 {:?}",
            describe(&out)
        );
        let files = match std::fs::read_dir(dir.join(SUB_DIR)) {
            Ok(rd) => rd.count(),
            Err(_) => 0,
        };
        assert_eq!(files, 0, "宁可跳过，也不写一个永远命中不了的死文件");
        assert!(
            !dir.join(SUB_DIR).exists(),
            "跳过时连目录都不许建（惰性建目录的唯一来源是 create_dir_all）"
        );
        clean(&dir);
    }

    /// W23：全角斜杠**保留在文件名里**，但归一化后仍能命中同一路径。
    ///
    /// 这就是「保留全角不影响命中」的机械证明（`normalize_name` 在两侧都把它折成空格）。
    #[test]
    fn w23_fullwidth_solidus_kept_but_still_hits() {
        let dir = tmp("w23");
        let dirs = dirs_of(&dir);
        let t = track("A／B", "");
        let path = match save(&t, &dirs, &lyrics()) {
            Outcome::Written(p) => p,
            other => panic!("应写入成功，实际 {:?}", describe(&other)),
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("文件名应为 UTF-8");
        assert!(name.contains('／'), "全角斜杠必须保留在文件名里，实际 {name}");
        assert_eq!(name, "A／B.lrc");
        let hit = locate::find(&t, &dirs).expect("保留全角也必须能被命中");
        assert_eq!(hit.path, path);
        clean(&dir);
    }

    /// W24：向后兼容——目录里只有旧式 `歌手 - 歌名.lrc` 时仍能命中（A8.4）。
    #[test]
    fn w24_old_style_name_still_hits() {
        let dir = tmp("w24");
        let sub = dir.join(SUB_DIR);
        std::fs::create_dir_all(&sub).expect("建子目录");
        let old = sub.join("周杰伦 - 晴天.lrc");
        std::fs::write(&old, "[00:01.00]旧式文件名").expect("预置旧式文件");
        let hit = locate::find(&track("晴天", "周杰伦"), &dirs_of(&dir))
            .expect("v1.7.0 写下的旧式文件名必须仍能命中（candidates[0] 口径未变）");
        assert_eq!(hit.path, old);
        clean(&dir);
    }

    /// W25：泛化保护——不变式不成立时**连 `Barline` 目录都不许建**。
    ///
    /// 判定顺序即证明：`resolve_target`（含 `build_file_name` → `Unsearchable`）
    /// 在 `create_dir_all` **之前**返回（见 [`save`] 的步骤 3/4），
    /// 所以「目录都不存在」= 「一个字节都没写」。
    /// 日志侧对应 `maybe_write_back` 的 `歌词不落盘：{s:?}` → 文本为 `歌词不落盘：Unsearchable`。
    #[test]
    fn w25_no_dead_file_when_invariant_breaks() {
        let dir = tmp("w25");
        let dirs = dirs_of(&dir);
        let out = save(&track("a\u{2}b", "Y"), &dirs, &lyrics());
        assert!(
            matches!(out, Outcome::Skipped(Skip::Unsearchable)),
            "实际 {:?}",
            describe(&out)
        );
        assert!(
            !dir.join(SUB_DIR).exists(),
            "跳过时连目录都不许建（惰性建目录的唯一来源是 create_dir_all）"
        );
        // 反向确认：同一目录下能写的歌照样能写（证明不是本次调用把目录锁住了）
        let ok = save(&track("正常曲目 ω", "正常歌手 ω"), &dirs, &lyrics());
        assert!(matches!(ok, Outcome::Written(_)), "实际 {:?}", describe(&ok));
        clean(&dir);
    }
}
