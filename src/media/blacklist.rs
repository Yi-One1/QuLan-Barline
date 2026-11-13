//! 黑名单 + 身份索引：归一化、三级回退身份解析、命中判定、真实名缓存
//! （`docs/ARCH-G7-黑名单.md` §3.2 / §4.1，v1.9.0 / G7）。
//!
//! **硬约束（progress.md §2.2 第 11 条）**：本模块**只允许**做内存字符串运算。
//! 任何 `OpenProcess` / SMTC 属性读 / WASAPI 枚举都不许出现在这里——
//! `identity()` 与 `matches()` 跑在媒体主循环里，每轮都会调用。
//! 反查（WASAPI）在 `smtc::reverse_lookup_names`，只有枚举时才跑。
//!
//! **单一出处纪律（R6）**：UI 显示、配置写入、媒体线程过滤三处的身份解析
//! 只有本文件的 `identity()` 一个实现——「列表显示的值 = 判定用的值 =
//! 用户手输可命中的值」三者同源是本需求的正确性基石。
//!
//! 数值唯一定义处在 [`crate::config::defaults::limits`]，这里一律 `limits::` 引用。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::config::defaults::limits;
use crate::config::MediaConfig;
use crate::media::player_map;

// ---------------------------------------------------------------- 错误与身份

/// 归一化失败原因（决定给用户哪条 toast）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormError {
    /// 空串 / 全空白 / 剥完路径后文件名为空（如 `.exe`）
    Empty,
    /// 含 `* ? | < > "`（含全角 `＊？｜＜＞"`）等通配符或非法字符
    InvalidChar,
    /// 超过 `limits::BLACKLIST_MAX_LEN`
    TooLong,
}

/// 名字来源，决定 UI 是否要提示"这是猜的"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentitySource {
    /// ① 缓存（实测真实名；来自 `config.media.app_names` 的同源只读副本）
    Cache,
    /// ② player_map 静态表
    Table,
    /// ③ AUMID 末段兜底（可能是猜的）
    AumidSegment,
    /// ④ 无法解析（永不可命中）
    None,
}

/// 一次身份解析的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// 身份键（小写、含 `.exe`）；空串 = 完全无法解析 → 永不可命中
    pub key: String,
    /// 名字来源
    pub source: IdentitySource,
}

// ---------------------------------------------------------------- 归一化

/// 通配符 / 非法字符（半角 + 全角）。**引号不在此列**：首尾引号按"路径粘贴"剥掉
/// （ARCH §3.2 步骤 2 的 `"C:\x\a.exe"` 必须是合法输入），但剥完之后**中间**再出现
/// 引号仍算非法。
fn is_invalid_char(c: char) -> bool {
    matches!(
        c,
        '*' | '?' | '|' | '<' | '>' | '"' | '＊' | '？' | '｜' | '＜' | '＞' | '＂'
    )
}

/// 已以 `.exe` 结尾则原样，空串则原样（→ 空身份，永不命中），否则补 `.exe`。
fn with_exe(s: &str) -> String {
    if s.is_empty() || s.ends_with(".exe") {
        s.to_string()
    } else {
        format!("{s}.exe")
    }
}

/// 去 `.exe` 后缀的主干（比较用：`msedge` 与 `msedge.exe` 等价）。
/// `.exe` 本身 → 空串（永不命中）。
fn stem_of(s: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    match s.strip_suffix(".exe") {
        Some(st) => st.to_string(),
        None => s,
    }
}

/// 单条归一化（用户输入 / 缓存写入的**唯一**入口）。纯函数。
///
/// 顺序很重要：① trim/剥首尾引号 → ② 非法字符 → ③ 取文件名 → ④ 小写
/// → ⑤ 补 `.exe` → ⑥ 主干为空 → ⑦ 长度校验。
/// 先剥引号再判非法字符，是为了让 `"C:\x\a.exe"`（路径粘贴）合法、
/// 而 `a*b.exe` 得到"不支持通配符"而不是被当成合法文件名存进去。
pub fn normalize_entry(raw: &str) -> Result<String, NormError> {
    // ① trim + 剥首尾引号（`"` `'` 与反引号），再 trim
    let s = raw.trim().trim_matches(['"', '\'', '`']).trim();
    // ② 非法字符（通配符必须最先拒绝，即使藏在路径里）
    if s.chars().any(is_invalid_char) {
        return Err(NormError::InvalidChar);
    }
    // ③ 取最后一个 `\` 或 `/` 之后的部分（填完整路径只取文件名）
    let file = s.rsplit(['\\', '/']).next().unwrap_or("").trim();
    // ④ 小写
    let lower = file.to_ascii_lowercase();
    if lower.chars().any(|c| c.is_control()) {
        return Err(NormError::InvalidChar);
    }
    // ⑤ 无 `.exe` 后缀则补
    let with = with_exe(&lower);
    // ⑥ 主干为空（`.exe` / 空串 / 全空白）→ 拒绝
    if stem_of(&with).is_empty() {
        return Err(NormError::Empty);
    }
    // ⑦ 长度校验
    if with.chars().count() > limits::BLACKLIST_MAX_LEN {
        return Err(NormError::TooLong);
    }
    Ok(with)
}

/// 列表归一化（`Config::load` 的兜底：手改 json 也安全）。
/// 剔空、剔非法、剔超长、去重（归一化后相等即重复）、保序、截断到
/// `limits::BLACKLIST_MAX_ENTRIES`。**永不失败、永不 panic、永不弹窗**。
pub fn normalize_list(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in raw {
        if let Ok(k) = normalize_entry(s) {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out.truncate(limits::BLACKLIST_MAX_ENTRIES);
    out
}

/// 黑白名单**成对**归一化（v1.12.0）。
///
/// 两张表各自过 [`normalize_list`]，然后执行**互斥**：出现在黑名单里的键
/// 一律从白名单剔除（主干比较，`msedge` 与 `msedge.exe` 视为同一条）。
///
/// **为什么冲突时黑名单优先**：黑名单是"否决"，白名单是"允许"。否决权更高，
/// 用户拉黑某个应用后，不该因为有旧的同名白名单条目就又冒出来。正常写入路径
/// 根本不允许这种状态（`apply_blacklist` 直接拒绝），这里只兜手改 json 的情况。
///
/// 纯函数、幂等、永不失败——`Config::load` 的唯一调用点，写坏 json 也不炸。
pub fn normalize_lists(
    blacklist: &[String],
    whitelist: &[String],
) -> (Vec<String>, Vec<String>) {
    let bl = normalize_list(blacklist);
    let mut wl = normalize_list(whitelist);
    wl.retain(|w| !bl.iter().any(|b| matches_entry(b, w)));
    (bl, wl)
}

/// AUMID 归一化（**只给缓存的键用**，不参与用户输入）：/// `trim()` + 小写 + 剔控制字符 + 长度 ≤ `limits::BLACKLIST_MAX_LEN`；非法返回 `None`。
/// 不改结构（`!` `_` `.` 中文都保留），因为键只用于查表。
pub fn normalize_aumid(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let cleaned: String = s
        .to_ascii_lowercase()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if cleaned.is_empty() || cleaned.chars().count() > limits::BLACKLIST_MAX_LEN {
        return None;
    }
    Some(cleaned)
}

/// AUMID 末段：最后一个 `!` `|` `\` `/` 之后的部分。
/// 打包应用 `Publisher.App_hash!AppId` → `AppId`；win32 常无分隔符 → 原样。
fn aumid_last_segment(aumid: &str) -> &str {
    aumid.rsplit(['!', '|', '\\', '/']).next().unwrap_or("")
}

/// 缓存表归一化（**`Config::load` 必须调**，手改 json 兜底）：键走 `normalize_aumid`、
/// 值走 `normalize_entry`，任一非法即丢弃该条；超 `limits::APP_NAME_CACHE_MAX` 时
/// 逐个淘汰字典序最小键直到达标。**永不失败、永不 panic、永不弹窗。**
pub fn normalize_app_names(raw: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in raw {
        if let (Some(k), Ok(v)) = (normalize_aumid(k), normalize_entry(v)) {
            out.insert(k, v);
        }
    }
    while out.len() > limits::APP_NAME_CACHE_MAX {
        // 与 merge_learned 同一条淘汰规则：删字典序最小键（确定性 + 自愈）
        let smallest = match out.keys().next().cloned() {
            Some(k) => k,
            None => break,
        };
        out.remove(&smallest);
    }
    out
}

/// 把枚举学到的 `(aumid, process)` 合并进缓存（**同一个**归一化 + 容量 + 淘汰规则）。
/// 返回「真的有新增/变化」——调用方据此决定要不要落盘（`SaveConfig`）。
///
/// 写入缓存那一刻若发现静态表与实测不符，打一条 `WARN`
/// （"表该更新了"的唯一发现途径，成本为零：只在学新名字时打一次）。
pub fn merge_learned(
    cache: &mut BTreeMap<String, String>,
    learned: &[(String, String)],
) -> bool {
    let mut changed = false;
    for (aumid, process) in learned {
        let (Some(k), Ok(v)) = (normalize_aumid(aumid), normalize_entry(process)) else {
            continue; // 坏值丢弃，不影响其它条目
        };
        if cache.get(&k) == Some(&v) {
            continue; // 已知且一致 → 无变化
        }
        let table = player_map::resolve(&k).process_name;
        if !table.is_empty() && table != v {
            crate::log_warn!("静态表与实测不符：aumid={k} 表={table} 实测={v}（缓存以实测为准）");
        }
        if !cache.contains_key(&k) && cache.len() >= limits::APP_NAME_CACHE_MAX {
            // 满 64 → 删字典序最小键（确定性 + 自愈：被淘汰条目下次枚举重新学到）
            let smallest = match cache.keys().next().cloned() {
                Some(k) => k,
                None => break,
            };
            cache.remove(&smallest);
        }
        cache.insert(k, v);
        changed = true;
    }
    changed
}

// ---------------------------------------------------------------- 判定

/// **身份解析的唯一实现**（三级回退，ARCH-G7 §3.2）。纯内存，无任何跨进程调用。
///
/// 顺序即优先级：① 内存缓存命中 → ② 静态表命中 → ③ AUMID 末段兜底（可能是猜的）
/// → ④ 全空（永不可命中）。
pub fn identity(
    cache: &BTreeMap<String, String>,
    process_name: &str,
    aumid: &str,
) -> Identity {
    // ① 缓存：本次会话内实测的真实名，最高优先（与静态表冲突时以实测为准）
    if let Some(a) = normalize_aumid(aumid) {
        if let Some(name) = cache.get(&a) {
            return Identity { key: name.clone(), source: IdentitySource::Cache };
        }
    }
    // ② 静态表：player_map 的权威值
    let p = process_name.trim().to_ascii_lowercase();
    if !p.is_empty() {
        return Identity { key: with_exe(&p), source: IdentitySource::Table };
    }
    // ③ AUMID 末段：可能是猜的（`!` `|` `\` `/` 之后）
    let seg = with_exe(aumid_last_segment(aumid).trim().to_ascii_lowercase().as_str());
    if !seg.is_empty() {
        return Identity { key: seg, source: IdentitySource::AumidSegment };
    }
    // ④ 无法解析
    Identity { key: String::new(), source: IdentitySource::None }
}

/// 一条黑名单是否命中该身份键（比对"去 `.exe` 的主干"，所以 `msedge` 与
/// `msedge.exe` 等价）。**禁止子串匹配**——那等于隐式通配。
pub fn matches_entry(entry: &str, key: &str) -> bool {
    let e = stem_of(entry);
    let k = stem_of(key);
    !e.is_empty() && !k.is_empty() && e == k
}

// ---------------------------------------------------------------- 共享索引

/// UI 线程与媒体线程共享的索引：`blacklist` 与 `app_names` 都属于
/// `Config.media`（一起落盘），本结构体是它们在内存里的**同源只读副本**，
/// 供判定阶段零跨进程读取（判定只读，不回写）。
///
/// **单读者（媒体线程）+ 写极少（配置变更）**，`Mutex` 足够。
/// 实现铁律：先取快照（释放锁）→ 再算 → 绝不嵌套持锁。
pub struct MediaIndex {
    blacklist: Mutex<Vec<String>>,
    /// 白名单（v1.12.0）。**空 = 不启用**；非空时只有名单内的应用会被接管。
    /// 与 `blacklist` 互斥（写入路径拒绝冲突，`normalize_lists` 兜底）。
    whitelist: Mutex<Vec<String>>,
    /// AUMID（小写）→ 真实进程名。数据源是 `Config.media.app_names`（落盘），
    /// 这里只是副本；`BTreeMap` 与配置侧同类型（迭代顺序确定，测试/日志可复现）。
    app_names: Mutex<BTreeMap<String, String>>,
}

impl MediaIndex {
    /// 启动时从配置注入（App::bootstrap 唯一构造点）。
    ///
    /// 两张表都先过同一套归一化（与 `Config::load` 同源）：内存副本里**只允许**
    /// 存归一化后的值——「UI 显示 = 判定 = 用户手输」三者同源的另一半保证。
    pub fn new(cfg: &MediaConfig) -> Arc<Self> {
        let (bl, wl) = normalize_lists(&cfg.blacklist, &cfg.whitelist);
        Arc::new(Self {
            blacklist: Mutex::new(bl),
            whitelist: Mutex::new(wl),
            app_names: Mutex::new(normalize_app_names(&cfg.app_names)),
        })
    }

    /// **整组同步**（三张表一起替换），写路径只有 `Effect::SaveConfig` 一处。
    /// ⚠️ 调用前必须先把 `merge_learned` 的结果写进 `cfg.media.app_names`，
    /// 否则会用旧表把刚学到的名字盖掉（ARCH-G7 §5.6 顺序陷阱）。
    /// 输入表同样先归一化（幂等：正常调用方传来的已是归一化值，无额外成本）。
    pub fn set(&self, cfg: &MediaConfig) {
        let (bl, wl) = normalize_lists(&cfg.blacklist, &cfg.whitelist);
        *self.blacklist.lock().unwrap() = bl;
        *self.whitelist.lock().unwrap() = wl;
        *self.app_names.lock().unwrap() = normalize_app_names(&cfg.app_names);
    }

    /// 三级身份解析。**每轮 tick 都会调**。
    pub fn identity(&self, process_name: &str, aumid: &str) -> Identity {
        // 先取 app_names 快照并释放锁，再算（绝不嵌套持锁）
        let cache = self.app_names.lock().unwrap().clone();
        identity(&cache, process_name, aumid)
    }

    /// **命中黑名单**（UI 语义：这个应用被用户明确拉黑了）。
    ///
    /// ⚠️ 只回答"是否在黑名单里"，**不是**过滤判据——白名单非空时，名单外的
    /// 应用同样不该接管，但它并没有被拉黑。要判"要不要接管"请用 [`Self::is_filtered`]。
    /// 纯内存 + 早退。
    pub fn matches(&self, process_name: &str, aumid: &str) -> bool {
        let id = self.identity(process_name, aumid);
        if id.key.is_empty() {
            return false;
        }
        let bl = self.blacklist.lock().unwrap();
        bl.iter().any(|e| matches_entry(e, &id.key))
    }

    /// **过滤判据（唯一出处）**：这个应用现在该不该被小组件接管。
    ///
    /// `true` = 不接管（不渲染、不查歌词、不响应播控）。三条规则：
    /// 1. 命中黑名单 → 过滤（无论白名单是否启用）；
    /// 2. 白名单为空 → 不过滤（功能未启用，行为与 v1.11.0 完全一致）；
    /// 3. 白名单非空 → **只放行名单内的**；名单外一律过滤。
    ///
    /// 身份解析失败（`key` 为空）时：黑名单不可能命中（用户填不进空键），
    /// 但白名单非空意味着"只接管明确允许的"——无法证明它在名单内，按规则 3 过滤。
    /// 这是白名单语义的自然结果，日志侧可由调用方的 `hit_log_due` 观察。
    ///
    /// **媒体线程每轮 tick 都会调**：纯内存、无 IO、无 COM（本模块铁律）。
    pub fn is_filtered(&self, process_name: &str, aumid: &str) -> bool {
        let id = self.identity(process_name, aumid);
        if id.key.is_empty() {
            // 无法解析身份：黑名单永不可命中；白名单非空时按"名单外"处理
            return !self.whitelist.lock().unwrap().is_empty();
        }
        if self
            .blacklist
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches_entry(e, &id.key))
        {
            return true;
        }
        let wl = self.whitelist.lock().unwrap();
        if wl.is_empty() {
            return false;
        }
        !wl.iter().any(|e| matches_entry(e, &id.key))
    }

    /// 黑名单快照（设置页/托盘读）。
    pub fn blacklist_snapshot(&self) -> Vec<String> {
        self.blacklist.lock().unwrap().clone()
    }

    /// 黑名单条数。
    pub fn blacklist_len(&self) -> usize {
        self.blacklist.lock().unwrap().len()
    }

    /// 媒体线程在枚举阶段**立即**写入新学到的名字（内存副本先行，
    /// 配置落盘随后由 UI 线程做，见 ARCH-G7 §5.6 三段式）。
    pub fn learn(&self, learned: &[(String, String)]) {
        if learned.is_empty() {
            return;
        }
        let mut cache = self.app_names.lock().unwrap();
        merge_learned(&mut cache, learned);
    }

    /// 缓存里是否存在这个真实名（`KeyRecognition::Verified` 的判据之一）。
    /// 同时查静态表——判据：`index.knows_process(key)` **或**
    /// `player_map::TABLE` 任一条 `process_name` 的主干等于该键的主干。
    /// 纯内存，UI 线程可直接算；**不查运行时 AUMID**（那要跨进程）。
    pub fn knows_process(&self, key: &str) -> bool {
        let want = stem_of(key);
        if want.is_empty() {
            return false;
        }
        if self
            .app_names
            .lock()
            .unwrap()
            .values()
            .any(|v| stem_of(v) == want)
        {
            return true;
        }
        player_map::TABLE
            .iter()
            .any(|(_, _, info)| stem_of(info.process_name) == want)
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------- normalize_entry（PRD §P0-01）

    /// PRD §P0-01 表格 7 种输入的归一化结果。
    #[test]
    fn normalize_entry_prd_table() {
        assert_eq!(normalize_entry("CloudMusic").unwrap(), "cloudmusic.exe");
        assert_eq!(normalize_entry("  cloudmusic.EXE  ").unwrap(), "cloudmusic.exe");
        assert_eq!(normalize_entry("C:\\x\\a.exe").unwrap(), "a.exe");
        assert_eq!(normalize_entry("/usr/bin/a.exe").unwrap(), "a.exe");
        assert_eq!(normalize_entry("CloudMusic").unwrap(), "cloudmusic.exe", "无后缀补 .exe");
        assert_eq!(normalize_entry("\"C:\\x\\a.exe\"").unwrap(), "a.exe", "首尾引号剥离");
        assert_eq!(normalize_entry("msedge").unwrap(), "msedge.exe");
    }

    /// 通配符 / 非法字符最先判定（半角 + 全角），即使藏在路径里也拒绝。
    #[test]
    fn normalize_entry_rejects_wildcards_half_and_full_width() {
        for bad in ["a*b.exe", "a?b.exe", "a|b.exe", "a<b.exe", "a>b.exe", "a\"b.exe",
                    "ａ＊ｂ.exe", "a？b.exe", "a｜b.exe", "C:\\x\\a*b.exe"] {
            assert_eq!(normalize_entry(bad), Err(NormError::InvalidChar), "{bad}");
        }
    }

    /// 空串 / 全空白 / `.exe` / 只有引号 → `Empty`。
    #[test]
    fn normalize_entry_rejects_empty_stem() {
        assert_eq!(normalize_entry(""), Err(NormError::Empty));
        assert_eq!(normalize_entry("   "), Err(NormError::Empty));
        assert_eq!(normalize_entry(".exe"), Err(NormError::Empty));
        assert_eq!(normalize_entry("\"\""), Err(NormError::Empty));
        assert_eq!(normalize_entry("dir\\.exe"), Err(NormError::Empty));
    }

    /// 超长（> 260）→ `TooLong`。
    #[test]
    fn normalize_entry_rejects_too_long() {
        let ok_len = limits::BLACKLIST_MAX_LEN - 5; // ".exe" 占 4，留余量
        let ok = format!("{}.exe", "a".repeat(ok_len));
        assert!(normalize_entry(&ok).is_ok(), "恰好 ≤ 上限必须通过");
        let bad = format!("{}.exe", "a".repeat(limits::BLACKLIST_MAX_LEN));
        assert_eq!(normalize_entry(&bad), Err(NormError::TooLong));
    }

    // ---------------------------------------------- normalize_list

    /// PRD / ARCH T01 验收 b：脏列表 → 归一化 + 去重 + 保序。
    #[test]
    fn normalize_list_dirty_fixture() {
        let raw: Vec<String> = ["  CloudMusic  ", "", "CloudMusic.EXE", "a*b.exe"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(normalize_list(&raw), vec!["cloudmusic.exe".to_string()]);
    }

    /// 去重（大小写不敏感 → 归一化后相等）、保序、`BLACKLIST_MAX_ENTRIES` 截断。
    #[test]
    fn normalize_list_dedup_order_and_cap() {
        let raw: Vec<String> = ["b.exe", "B", "a.exe", "a"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            normalize_list(&raw),
            vec!["b.exe".to_string(), "a.exe".to_string()],
            "去重保序：后出现的重复项被丢弃"
        );
        let many: Vec<String> = (0..limits::BLACKLIST_MAX_ENTRIES + 10)
            .map(|i| format!("app{i}.exe"))
            .collect();
        let out = normalize_list(&many);
        assert_eq!(out.len(), limits::BLACKLIST_MAX_ENTRIES, "截断到 200");
        assert_eq!(out[0], "app0.exe", "保序：保留前 200 条");
    }

    // ---------------------------------------------- normalize_app_names

    /// 坏键 / 坏值丢弃：空键、值含通配符、控制字符键一律不保留。
    #[test]
    fn normalize_app_names_drops_bad_entries() {
        let mut raw = BTreeMap::new();
        raw.insert(String::new(), "x.exe".to_string()); // 空键
        raw.insert("a!b".to_string(), "a*b.exe".to_string()); // 坏值
        raw.insert("c!d".to_string(), "cloudmusic.exe".to_string()); // 合法
        let out = normalize_app_names(&raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out.get("c!d").map(String::as_str), Some("cloudmusic.exe"));
    }

    /// 65 条溢出 → 裁到 64，且淘汰的是**字典序最小**的键（确定性）。
    /// 65 条溢出 → 裁到 64；被裁掉的是**字典序最小**的键（确定性，与插入顺序无关）。
    #[test]
    fn normalize_app_names_caps_at_64_by_evicting_smallest() {
        let mut raw = BTreeMap::new();
        for i in 0..limits::APP_NAME_CACHE_MAX {
            raw.insert(format!("z{:02}!app", i), format!("app{i}.exe"));
        }
        raw.insert("a!overflow".to_string(), "new.exe".to_string()); // 字典序最小
        let out = normalize_app_names(&raw);
        assert_eq!(out.len(), limits::APP_NAME_CACHE_MAX);
        assert!(
            !out.contains_key("a!overflow"),
            "淘汰规则不看新旧，只看字典序：最小键 a!overflow 被裁"
        );
        assert!(out.contains_key("z00!app"), "字典序更大侧全部保留");
        assert!(out.contains_key(&format!("z{:02}!app", limits::APP_NAME_CACHE_MAX - 1)));
    }

    // ---------------------------------------------- 幂等（R12 红线）

    /// R12：归一化必须**幂等**——脏到极致的 fixture 跑两遍，结果相同，
    /// 否则每次启动都判定"有变化"并写盘。
    #[test]
    fn normalization_is_idempotent_r12() {
        let mut dirty_list: Vec<String> = [
            "  CloudMusic  ", "", "CloudMusic.EXE", "a*b.exe", "\"C:\\x\\Edge.exe\"",
            "MSEDGE", ".exe", "　ＡＢ　",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for i in 0..limits::BLACKLIST_MAX_ENTRIES + 5 {
            dirty_list.push(format!("pad{i}.exe"));
        }
        let once = normalize_list(&dirty_list);
        let twice = normalize_list(&once);
        assert_eq!(once, twice, "normalize_list 第二遍不许有变化");

        let mut dirty_map = BTreeMap::new();
        dirty_map.insert(String::new(), "x.exe".to_string());
        dirty_map.insert("A!B".to_string(), "CloudMusic".to_string());
        dirty_map.insert("c!d".to_string(), "a*b.exe".to_string());
        for i in 0..limits::APP_NAME_CACHE_MAX + 5 {
            dirty_map.insert(format!("k{:02}!app", i), format!("p{i}.exe"));
        }
        let once = normalize_app_names(&dirty_map);
        let twice = normalize_app_names(&once);
        assert_eq!(once, twice, "normalize_app_names 第二遍不许有变化");
    }

    // ---------------------------------------------- identity 四条路径

    /// c：四条路径全覆盖 + source 正确。
    #[test]
    fn identity_four_paths() {
        let mut cache = BTreeMap::new();
        cache.insert("publisher.app_hash!appid".to_string(), "real.exe".to_string());

        // ① 缓存
        let id = identity(&cache, "", "Publisher.App_hash!AppId");
        assert_eq!(id, Identity { key: "real.exe".into(), source: IdentitySource::Cache });

        // ② 静态表（process_name 非空、aumid 不在缓存）
        let id = identity(&cache, "CloudMusic.exe", "whatever");
        assert_eq!(id, Identity { key: "cloudmusic.exe".into(), source: IdentitySource::Table });

        // ③ AUMID 末段（两者都查不到）
        let id = identity(&cache, "", "SomeVendor.Player_abc!MainExe");
        assert_eq!(id.key, "mainexe.exe");
        assert_eq!(id.source, IdentitySource::AumidSegment);

        // ④ 全空
        let id = identity(&cache, "", "");
        assert_eq!(id, Identity { key: String::new(), source: IdentitySource::None });
        let id = identity(&cache, "   ", "!");
        assert_eq!(id.source, IdentitySource::None, "末段为空 → 全空");
    }

    /// d：缓存优先于表——同 AUMID 两处都有且不一致 → 返回 Cache 的值。
    #[test]
    fn identity_cache_beats_table() {
        let mut cache = BTreeMap::new();
        // player_map 表内条目：AUMID 含 "msedge" → 表给 msedge.exe；缓存给实测值
        cache.insert("msedge".to_string(), "msedge_real.exe".to_string());
        let id = identity(&cache, "msedge.exe", "MSEdge");
        assert_eq!(id.source, IdentitySource::Cache);
        assert_eq!(id.key, "msedge_real.exe", "冲突时以实测缓存为准");
    }

    /// g：`player_map::TABLE` 每条 `process_name` 都能被 identity() 的非 None
    /// 路径解析（表覆盖护栏；R6 的守门测试）。
    #[test]
    fn player_map_table_all_resolvable_via_table_tier() {
        let cache = BTreeMap::new();
        for (_, _, info) in player_map::TABLE {
            let id = identity(&cache, info.process_name, "");
            assert_eq!(
                id.source,
                IdentitySource::Table,
                "表内进程名 {} 必须走 Table 级",
                info.process_name
            );
            assert_eq!(id.key, info.process_name);
        }
    }

    // ---------------------------------------------- matches_entry

    /// f：主干比较护栏——`msedge` 与 `msedge.exe` 等价；子串不算命中。
    #[test]
    fn matches_entry_stem_comparison() {
        assert!(matches_entry("cloudmusic.exe", "cloudmusic"));
        assert!(matches_entry("cloudmusic", "cloudmusic.exe"));
        assert!(matches_entry("CloudMusic.EXE", "cloudmusic"));
        assert!(matches_entry("cloudmusic.exe", "cloudmusic.exe"));
        assert!(!matches_entry("cloud.exe", "cloudmusic.exe"), "主干必须全等");
        assert!(!matches_entry("cloud", "cloudmusic.exe"), "禁止子串匹配");
        assert!(!matches_entry("", "cloudmusic.exe"));
        assert!(!matches_entry(".exe", "cloudmusic.exe"));
    }

    // ---------------------------------------------- merge_learned

    /// e：新增 / 更新 / 坏值丢弃 / 返回"是否有变化"。
    #[test]
    fn merge_learned_adds_updates_and_drops_bad() {
        let mut cache = BTreeMap::new();
        // 新增
        assert!(merge_learned(
            &mut cache,
            &[("App.One!Main".to_string(), "one.exe".to_string())]
        ));
        assert_eq!(cache.get("app.one!main").map(String::as_str), Some("one.exe"));
        // 相同 → 无变化
        assert!(!merge_learned(
            &mut cache,
            &[("APP.ONE!MAIN".to_string(), "one.exe".to_string())]
        ));
        // 更新（同键不同值）
        assert!(merge_learned(
            &mut cache,
            &[("app.one!main".to_string(), "One.Real.exe".to_string())]
        ));
        assert_eq!(cache.get("app.one!main").map(String::as_str), Some("one.real.exe"));
        // 坏值丢弃（通配符）
        assert!(!merge_learned(
            &mut cache,
            &[("app.bad!x".to_string(), "a*b.exe".to_string())]
        ));
        assert!(!cache.contains_key("app.bad!x"));
    }

    /// e：满 64 时插入新键 → 淘汰字典序最小键。
    #[test]
    fn merge_learned_evicts_lexicographically_smallest_when_full() {
        let mut cache = BTreeMap::new();
        for i in 0..limits::APP_NAME_CACHE_MAX {
            cache.insert(format!("m{:02}!app", i), format!("p{i}.exe"));
        }
        assert!(merge_learned(
            &mut cache,
            &[("aa!new".to_string(), "new.exe".to_string())]
        ));
        assert_eq!(cache.len(), limits::APP_NAME_CACHE_MAX, "淘汰后仍是 64");
        assert!(cache.contains_key("aa!new"));
        assert!(!cache.contains_key("m00!app"), "字典序最小的 m00!app 被淘汰");
        assert!(
            cache.values().all(|v| v.ends_with(".exe")),
            "所有值都过了 normalize_entry"
        );
    }

    // ---------------------------------------------- MediaIndex

    /// index.matches 三级路径（缓存 / 表 / 末段各一条）。
    #[test]
    fn media_index_matches_three_tiers() {
        // ① 缓存级：aumid 在缓存 → real.exe → 命中
        let mut hit_cfg = MediaConfig::default();
        hit_cfg.app_names.insert("publisher.app!id".into(), "real.exe".into());
        hit_cfg.blacklist = vec!["real.exe".into()];
        let idx = MediaIndex::new(&hit_cfg);
        assert!(idx.matches("", "Publisher.App!ID"));
        assert!(!idx.matches("", "Other.App!ID"));

        // ② 表级：网易云在静态表（真实调用路径里 process_name 来自 player_map::resolve）
        let resolved = player_map::resolve("NeteaseCloudMusic!App");
        let mut cfg = MediaConfig::default();
        cfg.blacklist = vec!["cloudmusic".into()];
        let idx = MediaIndex::new(&cfg);
        assert!(
            idx.matches(&resolved.process_name.to_string(), "NeteaseCloudMusic!App"),
            "表级：player_map 给出 cloudmusic.exe → 主干命中"
        );
        // 表外应用：player_map 返回空 process_name → 只剩 AUMID 末段 "app"，
        // 与黑名单 "cloudmusic" 主干不等 → 不命中
        assert!(!idx.matches("", "UnknownPlayer!App"));

        // ③ 末段级：表外 win32，AUMID 就是 exe 名
        let mut cfg = MediaConfig::default();
        cfg.blacklist = vec!["foo.exe".into()];
        let idx = MediaIndex::new(&cfg);
        assert!(idx.matches("", "Foo.exe"));
        // 空 parts（无 aumid 无进程名）永不命中
        assert!(!idx.matches("", ""));
    }

    /// set 整组替换两张表；learn 后 identity 变 Cache（R6：反查写缓存 → source 变 Cache）。
    #[test]
    fn media_index_set_replaces_and_learn_promotes_to_cache() {
        let mut cfg = MediaConfig::default();
        cfg.blacklist = vec!["a.exe".into()];
        let idx = MediaIndex::new(&cfg);
        assert_eq!(idx.blacklist_len(), 1);

        // learn → identity 走缓存级
        idx.learn(&[("vendor.app!id".to_string(), "learned.exe".to_string())]);
        let id = idx.identity("", "Vendor.App!ID");
        assert_eq!(id, Identity { key: "learned.exe".into(), source: IdentitySource::Cache });

        // set 用不含新条目的旧表整表替换 → 学到的名字被盖掉（§5.6 顺序陷阱的负面教材）
        idx.set(&MediaConfig::default());
        assert_eq!(idx.blacklist_len(), 0);
        let id = idx.identity("", "Vendor.App!ID");
        assert_ne!(id.source, IdentitySource::Cache, "set 旧表会把刚学到的名字盖掉");
    }

    /// d2：落盘往返——`MediaConfig` serde roundtrip 后再建 `MediaIndex`，
    /// identity() 结果与 roundtrip 前一致（等价于"重启后仍命中"）。
    #[test]
    fn media_config_roundtrip_keeps_identity() {
        let mut cfg = MediaConfig::default();
        cfg.blacklist = vec!["learned.exe".into()];
        cfg.app_names.insert("Publisher.App_Hash!AppId".into(), "learned.exe".into());

        let json = serde_json::to_vec(&cfg).expect("序列化");
        let back: MediaConfig = serde_json::from_slice(&json).expect("反序列化");
        assert_eq!(back, cfg, "BTreeMap 保证 JSON 输出稳定、往返逐字节一致");

        let idx = MediaIndex::new(&back);
        let id = idx.identity("", "Publisher.App_hash!AppId");
        assert_eq!(id, Identity { key: "learned.exe".into(), source: IdentitySource::Cache });
        assert!(idx.matches("", "Publisher.App_hash!AppId"), "重启后仍命中");
    }

    // ---------------------------------------------- 白名单（v1.12.0）

    /// 成对归一化：脏值丢弃 + 冲突从白名单剔除（黑名单优先）+ 去重保序 + 幂等。
    #[test]
    fn normalize_lists_drops_conflicts_and_is_idempotent() {
        let bl: Vec<String> = ["  CloudMusic  ", "a*b.exe"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let wl: Vec<String> = ["CloudMusic.EXE", " Spotify ", "qqmusic.exe", "spotify"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (b, w) = normalize_lists(&bl, &wl);
        assert_eq!(b, vec!["cloudmusic.exe".to_string()], "坏值被丢弃");
        assert_eq!(
            w,
            vec!["spotify.exe".to_string(), "qqmusic.exe".to_string()],
            "已在黑名单的条目从白名单剔除；重复项只留第一次"
        );

        let (b2, w2) = normalize_lists(&b, &w);
        assert_eq!((b2, w2), (b, w), "归一化必须幂等（R12）");
    }

    /// **空白名单 = 功能不启用**：判定退化为「只看黑名单」，与 v1.11.0 完全一致。
    #[test]
    fn is_filtered_with_empty_whitelist_only_uses_blacklist() {
        let mut cfg = MediaConfig::default();
        cfg.blacklist = vec!["cloudmusic.exe".into()];
        let idx = MediaIndex::new(&cfg);
        assert!(idx.is_filtered("cloudmusic.exe", "NeteaseCloudMusic!App"));
        assert!(
            !idx.is_filtered("Spotify.exe", "Spotify.exe"),
            "白名单为空 → 名单外应用照常接管"
        );
        assert!(idx.matches("cloudmusic.exe", "NeteaseCloudMusic!App"), "matches 仍是纯黑名单语义");
        assert!(!idx.matches("Spotify.exe", "Spotify.exe"));
    }

    /// **非空白名单 = 只接管名单内**：名单外过滤、名单内放行、黑名单照样否决。
    #[test]
    fn is_filtered_with_whitelist_only_accepts_listed_apps() {
        let mut cfg = MediaConfig::default();
        cfg.whitelist = vec!["cloudmusic.exe".into(), "spotify".into()];
        let idx = MediaIndex::new(&cfg);
        assert!(
            !idx.is_filtered("cloudmusic.exe", "NeteaseCloudMusic!App"),
            "名单内 → 放行"
        );
        assert!(!idx.is_filtered("Spotify.exe", "Spotify.exe"), "主干等价 → 放行");
        assert!(idx.is_filtered("QQMusic.exe", "QQMusic.exe"), "名单外 → 过滤");

        // 黑白都非空：白名单外过滤、黑名单命中过滤、白名单内放行
        let mut cfg = MediaConfig::default();
        cfg.whitelist = vec!["cloudmusic.exe".into()];
        cfg.blacklist = vec!["spotify.exe".into()];
        let idx = MediaIndex::new(&cfg);
        assert!(idx.is_filtered("QQMusic.exe", "QQMusic.exe"), "两边都不在 → 过滤");
        assert!(idx.is_filtered("Spotify.exe", "Spotify.exe"), "黑名单命中 → 过滤");
        assert!(
            !idx.is_filtered("cloudmusic.exe", "NeteaseCloudMusic!App"),
            "白名单内 → 放行"
        );
    }

    /// 身份无法解析（无 AUMID、无进程名）：黑名单永不可能命中（用户填不进空键），
    /// 但**白名单非空时必须按「名单外」过滤**——否则解析不出的应用会绕过白名单。
    #[test]
    fn is_filtered_treats_unresolvable_identity_as_outside_when_whitelist_active() {
        let mut cfg = MediaConfig::default();
        cfg.whitelist = vec!["cloudmusic.exe".into()];
        let idx = MediaIndex::new(&cfg);
        assert!(idx.is_filtered("", ""), "白名单非空 + 身份不可解析 → 过滤");

        let idx2 = MediaIndex::new(&MediaConfig::default());
        assert!(!idx2.is_filtered("", ""), "白名单空 + 身份不可解析 → 放行");
    }

    /// `set` 同步白名单（配置变更后媒体线程立即看到）；冲突项被剔除后功能自动关闭。
    #[test]
    fn media_index_set_syncs_whitelist_and_drops_conflicts() {
        let idx = MediaIndex::new(&MediaConfig::default());
        assert!(!idx.is_filtered("QQMusic.exe", "QQMusic.exe"), "初始空白名单");

        let mut cfg = MediaConfig::default();
        cfg.whitelist = vec!["cloudmusic.exe".into()];
        idx.set(&cfg);
        assert!(idx.is_filtered("QQMusic.exe", "QQMusic.exe"), "set 后立即只接管名单内");
        assert!(!idx.is_filtered("cloudmusic.exe", "NeteaseCloudMusic!App"));

        // 手改 json 造成同一应用两边都有 → 白名单项被剔除（黑名单优先）
        let mut cfg = MediaConfig::default();
        cfg.blacklist = vec!["cloudmusic.exe".into()];
        cfg.whitelist = vec!["cloudmusic.exe".into()];
        idx.set(&cfg);
        assert!(
            !idx.is_filtered("QQMusic.exe", "QQMusic.exe"),
            "白名单被清空 → 功能关闭，只剩黑名单生效"
        );
        assert!(idx.is_filtered("cloudmusic.exe", "NeteaseCloudMusic!App"));
    }

    // ---------------------------------------------- knows_process

    /// `KeyRecognition` 判据：表内 → Verified；缓存值命中 → Verified；否则 Unverified。
    #[test]
    fn knows_process_uses_cache_and_table() {
        let idx = MediaIndex::new(&MediaConfig::default());
        assert!(idx.knows_process("CloudMusic"), "player_map 表内 → Verified");
        assert!(idx.knows_process("cloudmusic.exe"));
        assert!(!idx.knows_process("zzz-never-seen.exe"), "两处都查不到 → Unverified");
        assert!(!idx.knows_process(""), "空键恒 Unverified");

        let mut cfg = MediaConfig::default();
        cfg.app_names.insert("a!b".into(), "seen.exe".into());
        let idx = MediaIndex::new(&cfg);
        assert!(idx.knows_process("seen"), "缓存值命中 → Verified");
        assert!(idx.knows_process("seen.exe"));
        assert!(!idx.knows_process("other"));
    }

    // ---------------------------------------------- AUMID 末段 / with_exe

    /// 末段切分：`!` `|` `\` `/` 之后；无分隔符 → 原样。
    #[test]
    fn aumid_last_segment_splits_on_all_separators() {
        // 经 identity 间接验证（私有函数走公共出口）
        let cache = BTreeMap::new();
        let cases = [
            ("Publisher.App_hash!AppId", "appid.exe"),
            ("Foo|Bar", "bar.exe"),
            ("C:\\x\\a.exe", "a.exe"),
            ("a/b", "b.exe"),
            ("Plain.exe", "plain.exe"),
        ];
        for (aumid, want) in cases {
            let id = identity(&cache, "", aumid);
            assert_eq!(id.key, want, "aumid={aumid}");
            assert_eq!(id.source, IdentitySource::AumidSegment);
        }
    }
}
