//! 在线歌词查询的**纯函数**部分（v1.3.0 / G6 问题 7）：标题清洗、候选生成、
//! 搜索结果择优。
//!
//! 为什么单独成模块：这一层**不碰网络、不碰文件**，可以完整单测；而
//! lrclib 与网易云两个 provider 都要用同一套规则——
//! 「怎么把 SMTC 给的脏标题洗成能查的关键词」和「怎么从一堆同名结果里挑出对的那首」
//! 各写一份必然漂移。
//!
//! ## 背景：SMTC 给的元数据有多脏
//!
//! Edge 放 B 站视频时 `artist` 是**空串**（lrclib 精确接口拿到空 `artist_name=`
//! 直接 400）；标题里常混着 `(Live)`、`（伴奏）`、`[Remastered 2011]`、
//! `- feat. XXX` 这类版本标记；多歌手用 `/`、`&`、`、` 连在一起。
//! 所以查之前必须先洗：**宁可查宽，别查窄**。
//!
//! ## 三件事
//!
//! 1. [`title_variants`]：一个原始标题 → 1..=4 个查询候选（顺序稳定）。
//! 2. [`primary_artist`]：多歌手串 → 第一个主歌手（空串仍返回空串）。
//! 3. [`pick_best`]：一堆候选 → 挑一条（标题 + 歌手 + **时长**打分），
//!    分数不够就返回 `None`（宁可无歌词，也不拿一首同名不同歌的歌词）。

use crate::lyric::locate::normalize_name;

/// 版本标记 / 噪声关键词（出现在括注或 `- xxx` 尾巴里就整段删）。
///
/// 全部小写比较（比较前会 `to_lowercase`）。
pub const NOISE_TAGS: &[&str] = &[
    "live",
    "remaster",
    "remastered",
    "acoustic",
    "instrumental",
    "伴奏",
    "纯音乐",
    "现场",
    "翻唱",
    "cover",
    "demo",
    "radio edit",
    "remix",
    "feat.",
    "ft.",
    "mv",
    "official",
    "高清",
    "完整版",
];

/// 候选上限（查询数不能放大）。
pub const MAX_TITLE_VARIANTS: usize = 4;
/// 搜索关键词上限（每个关键词 = 一次网络请求）。
pub const MAX_SEARCH_KEYWORDS: usize = 3;
/// 择优阈值：低于它就当没命中。
pub const MIN_PICK_SCORE: i32 = 60;
/// 时长差满分线（毫秒）：±2s 内认为就是同一首。
pub const DURATION_EXACT_MS: i64 = 2_000;
/// 时长差半分线。
pub const DURATION_OK_MS: i64 = 5_000;
/// 时长差淘汰线：超过它就是另一首歌（串歌比没歌词更糟）。
pub const DURATION_MAX_MS: i64 = 10_000;

/// 全角 → 半角（只处理高频标点与全角 ASCII），并把各种括号统一成 `(` / `[`。
///
/// 注意 `（`(U+FF08) / `）`(U+FF09) / `【`(U+3010) 等的归属：
/// `（` 落在 `FF01..=FF5E` 里，由通用折叠直接变成 `(`；
/// `【】〔〕〖〗「」『』` 不在该区间，需要显式映射。
fn fold_width(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = match c {
            '\u{3000}' => ' ',
            // 全角 ASCII（！ 到 ～）→ 半角（含 `（）`、`／`、全角数字字母）
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            // 中文特有括号统一成 `[` / `]`
            '【' | '〔' | '〖' | '「' | '『' => '[',
            '】' | '〕' | '〗' | '」' | '』' => ']',
            _ => c,
        };
        out.push(c);
    }
    out
}

/// 去掉所有成对括注（`(...)` / `[...]`）。
///
/// **不认识的括注也删**——括注里绝大多数是版本 / 合作信息，留着会让查询过窄。
/// 括号不配对时（脏数据）从开括号起到结尾整段丢弃。
fn strip_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth = depth.saturating_sub(1);
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// 判断一段尾巴是不是"版本 / 噪声"（命中任一关键词即算）。
fn is_noise_tail(tail: &str) -> bool {
    let t = fold_width(tail).trim().to_lowercase();
    if t.is_empty() {
        return false;
    }
    NOISE_TAGS.iter().any(|tag| t.contains(tag))
}

/// 去掉尾部噪声：反复砍掉 `- xxx` 与 `feat. xxx` / `ft. xxx` 尾巴。
fn strip_tail_noise(s: &str) -> String {
    let mut cur = s.trim().to_string();
    loop {
        let before = cur.clone();
        // `A - xxx`：只有当 xxx 是噪声时才砍
        if let Some(pos) = cur.rfind(" - ") {
            let (head, tail) = cur.split_at(pos);
            if is_noise_tail(tail.trim_start_matches(" - ")) {
                cur = head.trim_end().to_string();
            }
        }
        // `A feat. xxx` / `A ft. xxx` / `A with xxx`（无短横线的写法）
        for sep in [" feat. ", " ft. ", " Feat. ", " Ft. ", " feat ", " ft "] {
            if let Some(pos) = cur.rfind(sep) {
                let tail = &cur[pos + sep.len()..];
                if !tail.trim().is_empty() {
                    cur = cur[..pos].trim_end().to_string();
                    break;
                }
            }
        }
        if cur == before {
            return cur;
        }
    }
}

/// 标题清洗：全角归一 → 去括注 → 去尾部噪声 → 折叠空白。
pub fn clean_title(raw: &str) -> String {
    let s = fold_width(raw);
    let s = strip_brackets(&s);
    let s = strip_tail_noise(&s);
    collapse_spaces(&s)
}

/// 折叠多余空白并去首尾。
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim_end().to_string()
}

/// 一个原始标题 → **1..=4 个查询候选**（顺序稳定、已去重）。
///
/// 顺序：
/// 1. 清洗后的标题（主候选，最可能命中）；
/// 2. 原始标题（清洗可能是误删，留一次原样重试）；
/// 3. `A - B` 里短横线左侧（"晴天 - 周杰伦" 这类把歌手写进标题的情况）；
/// 4. 清洗后去掉末尾单字噪声后的结果（兜底）。
///
/// 空串不会进候选；结果最多 [`MAX_TITLE_VARIANTS`] 条。
pub fn title_variants(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(MAX_TITLE_VARIANTS);
    let mut push = |s: &str| {
        let s = collapse_spaces(s);
        if s.is_empty() || out.len() >= MAX_TITLE_VARIANTS {
            return;
        }
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };

    let cleaned = clean_title(raw);
    push(&cleaned);
    push(raw.trim());

    // `A - B`：短横线左侧
    let folded = fold_width(raw);
    if let Some(pos) = folded.find(" - ") {
        push(&strip_brackets(&folded[..pos]));
    }
    // 兜底：清洗结果再去掉尾部一个噪声词（如 "... live" 没有括注也没短横线）
    if let Some(pos) = cleaned.rfind(' ') {
        let (head, tail) = cleaned.split_at(pos);
        if is_noise_tail(tail) {
            push(head);
        }
    }
    out
}

/// 多歌手串 → **第一个主歌手**；空串仍返回空串。
///
/// 分隔符：`/`、`&`、`、`、`,`、`;`、`+`，以及 `feat.` / `ft.` / `with`。
/// SMTC 里 `周杰伦/费玉清`、`A & B`、`A feat. B` 都很常见，查的时候只用主歌手
/// 命中率高得多（副歌手在 lrclib / 网易云里经常不参与 artist 字段）。
pub fn primary_artist(raw: &str) -> String {
    let s = fold_width(raw);
    let mut cut = s.len();
    // 单字符分隔符
    for sep in ['/', '&', '、', ',', ';', '+', '|'] {
        if let Some(pos) = s.find(sep) {
            cut = cut.min(pos);
        }
    }
    // 词分隔符（大小写不敏感，逐个找）
    let lower = s.to_lowercase();
    for sep in [" feat.", " feat ", " ft.", " ft ", " with "] {
        if let Some(pos) = lower.find(sep) {
            cut = cut.min(pos);
        }
    }
    collapse_spaces(&s[..cut])
}

/// 生成搜索关键词（**最多 [`MAX_SEARCH_KEYWORDS`] 条**，顺序稳定、已去重）。
///
/// 规则：先用「清洗标题 + 主歌手」，再退到「纯标题」；歌手为空时只出纯标题。
/// 逐条试、命中即停，所以顺序就是优先级。
pub fn search_keywords(raw_title: &str, raw_artist: &str) -> Vec<String> {
    let artist = primary_artist(raw_artist);
    let titles = title_variants(raw_title);
    let mut out: Vec<String> = Vec::with_capacity(MAX_SEARCH_KEYWORDS);

    if !artist.is_empty() {
        for t in titles.iter().take(2) {
            push_keyword(&mut out, &format!("{t} {artist}"));
        }
    }
    for t in titles.iter().take(2) {
        push_keyword(&mut out, t);
    }
    out
}

fn push_keyword(out: &mut Vec<String>, s: &str) {
    let s = collapse_spaces(s);
    if s.is_empty() || out.len() >= MAX_SEARCH_KEYWORDS {
        return;
    }
    if !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
        out.push(s);
    }
}

// ---------------------------------------------------------------- 择优

/// 一条候选搜索结果（**provider 无关**：lrclib 与网易云都归一成它）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// provider 侧的 id（网易云取歌词要用；lrclib 只用于日志）
    pub id: i64,
    pub title: String,
    pub artist: String,
    /// 时长（毫秒）；`0` = 未知
    pub duration_ms: i64,
}

/// 我们想找的那首歌。
#[derive(Debug, Clone, Copy)]
pub struct Want<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub duration_ms: i64,
}

impl<'a> Want<'a> {
    /// 由一条曲目构造。
    pub fn of(track: &'a crate::media::Track) -> Self {
        Self {
            title: &track.title,
            artist: &track.artist,
            duration_ms: track.duration_ms,
        }
    }
}

/// 给一条候选打分。`None` = **直接淘汰**（时长差太大，或标题完全不沾边）。
///
/// 权重：标题相等 60 / 包含 40；歌手相等 25 / 包含 12；时长 ≤2s 30 / ≤5s 15 / 其余 5。
/// 满分 115。时长差 > [`DURATION_MAX_MS`] 一律淘汰——串歌比没歌词更糟。
pub fn score_candidate(want: &Want<'_>, cand: &Candidate) -> Option<i32> {
    // ---- 时长 ----
    let mut score = 0i32;
    if want.duration_ms > 0 && cand.duration_ms > 0 {
        let diff = (want.duration_ms - cand.duration_ms).abs();
        if diff > DURATION_MAX_MS {
            return None;
        }
        score += if diff <= DURATION_EXACT_MS {
            30
        } else if diff <= DURATION_OK_MS {
            15
        } else {
            5
        };
    }

    // ---- 标题 ----
    let want_t = normalize_name(&clean_title(want.title));
    let cand_t = normalize_name(&clean_title(&cand.title));
    if want_t.is_empty() || cand_t.is_empty() {
        return None;
    }
    if want_t == cand_t {
        score += 60;
    } else if cand_t.contains(&want_t) || want_t.contains(&cand_t) {
        score += 40;
    } else {
        // 标题完全不沾边：光靠歌手和时长不足以认人
        return None;
    }

    // ---- 歌手 ----
    let want_a = normalize_name(&primary_artist(want.artist));
    let cand_a = normalize_name(&primary_artist(&cand.artist));
    if !want_a.is_empty() && !cand_a.is_empty() {
        if want_a == cand_a {
            score += 25;
        } else if cand_a.contains(&want_a) || want_a.contains(&cand_a) {
            score += 12;
        }
    }

    Some(score)
}

/// 从候选里挑最好的那条：返回 `(下标, 分数)`；最高分低于
/// [`MIN_PICK_SCORE`] 时返回 `None`。
///
/// 同分取**靠前**的那条（provider 的返回顺序本身就带相关性）。
pub fn pick_best(want: &Want<'_>, cands: &[Candidate]) -> Option<(usize, i32)> {
    let mut best: Option<(usize, i32)> = None;
    for (i, c) in cands.iter().enumerate() {
        let Some(s) = score_candidate(want, c) else {
            continue;
        };
        if s < MIN_PICK_SCORE {
            continue;
        }
        match best {
            Some((_, bs)) if bs >= s => {}
            _ => best = Some((i, s)),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------ 标题清洗

    /// 用户点名的 ≥6 组：版本标记必须整段消失。
    #[test]
    fn title_cleaning_strips_version_tags() {
        assert_eq!(clean_title("晴天 (Live)"), "晴天");
        assert_eq!(clean_title("Shape of You [Remastered 2015]"), "Shape of You");
        assert_eq!(clean_title("某歌（伴奏）"), "某歌");
        assert_eq!(clean_title("A - feat. B"), "A");
        assert_eq!(clean_title("夜曲 - Live"), "夜曲");
        assert_eq!(clean_title("稻香（现场版）"), "稻香");
        assert_eq!(clean_title("【纯音乐】River Flows"), "River Flows");
        assert_eq!(clean_title("Hello (Acoustic) [Remaster]"), "Hello");
        // 非噪声的 `- xxx` 不砍（"晴天 - 周杰伦" 里右边是歌手，砍了反而查不到）
        assert_eq!(clean_title("晴天 - 周杰伦"), "晴天 - 周杰伦");
        // 空串 / 只有括注
        assert_eq!(clean_title(""), "");
        assert_eq!(clean_title("(Live)"), "");
    }

    #[test]
    fn title_variants_are_bounded_and_stable() {
        let v = title_variants("晴天 (Live)");
        assert_eq!(v.first().map(String::as_str), Some("晴天"));
        assert!(v.len() <= MAX_TITLE_VARIANTS);
        // 原始串作为第二候选保留
        assert_eq!(v.get(1).map(String::as_str), Some("晴天 (Live)"));
        // 幂等：同一输入两次调用结果一致
        assert_eq!(v, title_variants("晴天 (Live)"));
        // 空标题不产生候选
        assert!(title_variants("   ").is_empty());
    }

    // ------------------------------------------------ 歌手拆分

    /// 用户点名的 4 组 + 那个 HTTP 400 的根因（空 artist 不许拼参数）。
    #[test]
    fn artist_splitting_takes_primary() {
        assert_eq!(primary_artist("周杰伦/费玉清"), "周杰伦");
        assert_eq!(primary_artist("A & B"), "A");
        assert_eq!(primary_artist("A feat. B"), "A");
        assert_eq!(primary_artist("A ft. B"), "A");
        assert_eq!(primary_artist("A、B、C"), "A");
        assert_eq!(primary_artist(""), "");
        assert_eq!(primary_artist("   "), "");
        // 单个歌手原样保留（含空格）
        assert_eq!(primary_artist("Taylor Swift"), "Taylor Swift");
    }

    /// **HTTP 400 的根因**：artist 为空时不许拼 `artist_name=` 参数——
    /// 也不许把空歌手塞进搜索关键词。
    #[test]
    fn empty_artist_never_produces_artist_param() {
        let u = crate::lyric::lrclib::build_url(&crate::media::Track {
            title: "粉碎".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: 357_000,
        });
        assert!(!u.contains("artist_name"), "空 artist 不许拼 artist_name=（lrclib 会 400）");
        assert!(u.contains("track_name="), "标题参数仍要在");
        assert!(u.contains("duration=357"));

        // 搜索关键词里也不能出现"标题 + 空格"这种尾巴
        let kws = search_keywords("粉碎", "");
        assert_eq!(kws, vec!["粉碎".to_string()], "无歌手时只出纯标题");
        assert!(kws.iter().all(|k| !k.ends_with(' ')));
    }

    #[test]
    fn search_keywords_are_bounded_and_ordered() {
        let kws = search_keywords("晴天 (Live)", "周杰伦/费玉清");
        assert!(kws.len() <= MAX_SEARCH_KEYWORDS);
        assert_eq!(kws[0], "晴天 周杰伦", "第一条必须是「清洗标题 + 主歌手」");
        assert!(kws.contains(&"晴天".to_string()), "必须留下纯标题兜底");
        assert!(kws.iter().all(|k| !k.contains("费玉清")), "副歌手不参与查询");
    }

    // ------------------------------------------------ 择优

    fn cand(id: i64, title: &str, artist: &str, dur_ms: i64) -> Candidate {
        Candidate {
            id,
            title: title.into(),
            artist: artist.into(),
            duration_ms: dur_ms,
        }
    }

    /// 用户点名的核心测试：同名但时长差 30s / 标题差一词 / 完全匹配 → 选完全匹配；
    /// 全部时长差 > 10s → `None`（宁可无歌词也不串歌）。
    #[test]
    fn pick_best_prefers_duration_and_title() {
        let want = Want {
            title: "晴天",
            artist: "周杰伦",
            duration_ms: 269_000,
        };
        let cands = vec![
            cand(1, "晴天", "周杰伦", 299_000),        // 同名同歌手，但差 30s → 淘汰
            cand(2, "晴天 现场版", "周杰伦", 269_500), // 标题多一词 → 略低分
            cand(3, "晴天", "周杰伦", 270_000),        // 完全匹配
        ];
        let (idx, score) = pick_best(&want, &cands).expect("必须选中一条");
        assert_eq!(cands[idx].id, 3, "应选完全匹配那条");
        assert!(score >= 100, "完全匹配应拿到高分，实际 {score}");

        // 全部时长差 > 10s → 一条都不选
        let all_far = vec![
            cand(1, "晴天", "周杰伦", 300_000),
            cand(2, "晴天", "周杰伦", 240_000),
        ];
        assert!(
            pick_best(&want, &all_far).is_none(),
            "时长差 >10s 必须全部淘汰"
        );

        // 标题完全不沾边 → 淘汰（哪怕歌手对、时长对）
        let wrong_title = vec![cand(9, "七里香", "周杰伦", 269_000)];
        assert!(pick_best(&want, &wrong_title).is_none());
    }

    #[test]
    fn pick_best_handles_missing_duration_and_artist() {
        // 时长未知（0）不参与打分，不淘汰
        let want = Want {
            title: "晴天",
            artist: "周杰伦",
            duration_ms: 269_000,
        };
        let (idx, _) = pick_best(&want, &[cand(1, "晴天", "", 0)]).expect("缺时长/歌手也应能选");
        assert_eq!(idx, 0);

        // 我们自己的歌手为空时，不因为歌手缺失而扣到底
        let want2 = Want {
            title: "粉碎",
            artist: "",
            duration_ms: 357_000,
        };
        let (idx, _) = pick_best(&want2, &[cand(7, "粉碎", "", 357_000)]).expect("应命中");
        assert_eq!(idx, 0);
    }
}
