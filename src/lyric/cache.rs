//! 歌词缓存：内存 LRU + 磁盘正缓存 + 负缓存（`ARCHITECTURE.md` §12.5）。
//!
//! 三层设计的原因：
//! - **内存 LRU(8)**：连续切歌时避免重复解析；
//! - **磁盘正缓存(30 天)**：重启后不用再联网（PRD P0-08 的"2 秒内"在第二次播放时变成 0 秒）；
//! - **负缓存(24h)**：同一首查不到的歌不要每次播放都打一次网络（PRD §6.2）。
//!
//! 文件名用 `fnv1a64(track.key())` 的 16 位十六进制，**不用标题**——
//! 避免非法字符、路径长度、编码问题（§12.5 明确要求）。
//!
//! 原子写：先写 `.tmp` 再 `rename`（Windows 上 `std::fs::rename` 走
//! `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`，语义一致）。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::defaults::limits;
use crate::lyric::{LyricKind, LyricLine, LyricMissReason, LyricWord, Lyrics};
use crate::media::Ms;

/// 磁盘正缓存保留天数 → 秒。
const POS_TTL_SECS: i64 = limits::LYRIC_DISK_TTL_DAYS * 86_400;
/// 负缓存保留小时 → 秒。
const NEG_TTL_SECS: i64 = limits::LYRIC_MISS_TTL_HOURS * 3_600;

/// **负缓存 key 的版本前缀**（v1.3.0 / G6 问题 7）。
///
/// 为什么要它：v1.3.0 换了歌词查询策略（artist 为空不再走精确接口 + 模糊检索 +
/// 网易云兜底），**以前查不到的歌现在有可能查到**。但那些失败的曲目已经写进了
/// 24h 负缓存，升级后会被直接拦住，等于新功能完全白干。
///
/// 做法：负缓存的文件名改成 `fnv1a64("v2:" + key)`——旧文件名（`fnv1a64(key)`）
/// 从此不再命中，**旧 miss 自动作废**；旧文件留在目录里等 TTL 清理自然淘汰
/// （不主动删：删除是破坏性操作，且旧文件最多留 24h）。
///
/// **正缓存不受影响**（正缓存里的歌词永远是对的，没必要重下）。
///
/// 以后如果再换查询策略，把这里的版本号 +1 即可。
const MISS_KEY_VERSION: &str = "v2:";

/// 负缓存的**逻辑 key**（`版本前缀 + track.key()`）。
///
/// 单独暴露成函数是为了可测：任何人都能断言"新版 key 与旧的不同"，
/// 防止以后有人手滑把它改回不带前缀（那会让升级后仍然命中旧 miss）。
pub fn miss_cache_key(track_key: &str) -> String {
    let mut s = String::with_capacity(MISS_KEY_VERSION.len() + track_key.len());
    s.push_str(MISS_KEY_VERSION);
    s.push_str(track_key);
    s
}
/// 磁盘缓存总大小上限（5 MB）。
const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// **内存** LRU 的字节上限（512 KB）。
///
/// 为什么不是架构里的 1 MB：QA 实测 M2 常驻工作集已经 38.74 MB，
/// 离 PRD P0-14 的 40 MB 只剩 1.3 MB 余量；歌词缓存必须在**字节**上封顶，
/// 而不是"8 条"——8 首 300 行的增强 LRC 轻松超过 1 MB。
const MEM_BUDGET_BYTES: usize = 512 * 1024;

// ---------------------------------------------------------------- 磁盘格式

#[derive(Debug, Serialize, Deserialize)]
struct DiskWord {
    s: Ms,
    e: Ms,
    t: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskLine {
    s: Ms,
    e: Ms,
    #[serde(default)]
    t: String,
    #[serde(default)]
    w: Vec<DiskWord>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskLyrics {
    key: String,
    saved_at: i64,
    #[serde(default)]
    source: String,
    #[serde(default)]
    kind: u8,
    #[serde(default)]
    offset_ms: Ms,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    by: String,
    lines: Vec<DiskLine>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskMiss {
    key: String,
    at: i64,
    #[serde(default)]
    reason: String,
}

impl DiskLyrics {
    fn from_lyrics(key: &str, l: &Lyrics, source: &str, now: i64) -> Self {
        Self {
            key: key.to_string(),
            saved_at: now,
            source: source.to_string(),
            kind: match l.kind {
                LyricKind::None => 0,
                LyricKind::Line => 1,
                LyricKind::Word => 2,
            },
            offset_ms: l.offset_ms,
            title: l.title.clone(),
            artist: l.artist.clone(),
            album: l.album.clone(),
            by: l.by.clone(),
            lines: l
                .lines
                .iter()
                .map(|ln| DiskLine {
                    s: ln.start_ms,
                    e: ln.end_ms,
                    t: ln.text.clone(),
                    w: ln
                        .words
                        .iter()
                        .map(|w| DiskWord {
                            s: w.start_ms,
                            e: w.end_ms,
                            t: w.text.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn into_lyrics(self) -> Lyrics {
        Lyrics {
            lines: self
                .lines
                .into_iter()
                .map(|ln| LyricLine {
                    start_ms: ln.s,
                    end_ms: ln.e,
                    text: ln.t,
                    words: ln
                        .w
                        .into_iter()
                        .map(|w| LyricWord {
                            text: w.t,
                            start_ms: w.s,
                            end_ms: w.e,
                        })
                        .collect(),
                })
                .collect(),
            kind: match self.kind {
                2 => LyricKind::Word,
                1 => LyricKind::Line,
                _ => LyricKind::None,
            },
            offset_ms: self.offset_ms,
            title: self.title,
            artist: self.artist,
            album: self.album,
            by: self.by,
        }
    }
}

// ---------------------------------------------------------------- 缓存本体

/// 一条**缓存命中**（v1.8.0）：歌词本体 + 来源标识。
///
/// `source` 是写这条缓存时的 provider 名（`"local"` / `"lrclib"` / `"netease"` / `"test"`），
/// 也兼容 v1.7.0 之前没有该字段的老缓存文件（`#[serde(default)]` → 空串）。
/// 判定规矩只有一条：`source == "local"` 表示「歌词来自用户自己的 .lrc」，
/// 落盘必须跳过（`service::maybe_write_back` 的早退）；其余一律落盘。
pub struct Hit {
    pub lyrics: Arc<Lyrics>,
    pub source: String,
}

/// 内存条目：歌词本体 + 来源。
///
/// 为什么内存也要记 `source`：`service` 落盘前要判「来源是不是本地文件」
/// （A6.6：缓存来源为 local 的命中不得再写一份到 `Barline/`）。
/// 只把 source 留在磁盘 `DiskLyrics` 里是不够的——内存命中那条路径读不到磁盘格式。
struct Entry {
    lyrics: Arc<Lyrics>,
    source: String,
}

/// 三层缓存。
pub struct Cache {
    dir: PathBuf,
    /// 内存 LRU 的顺序表（头部最新）
    order: VecDeque<String>,
    map: HashMap<String, Entry>,
    /// 内存里这些歌词的大致字节数（用于按预算淘汰）
    mem_bytes: usize,
}

/// 歌词磁盘缓存的**唯一路径出处**（v1.8.0）。
///
/// 歌词线程建缓存（`service::run`）与设置页统计（`app::run_settings_action`）
/// 都从这里取，路径不可能漂移（PRD A4.3 的机械保证）。
pub fn dir() -> PathBuf {
    crate::platform::data_dir().join("lyrics-cache")
}

/// 磁盘占用统计结果（v1.8.0 / R2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// 缓存目录（与 [`dir`] 同源）
    pub dir: PathBuf,
    /// 顶层文件数（`<hex>.json` 与 `miss-<hex>.json` 都算，与 `sweep` 口径一致）
    pub files: u64,
    /// 顶层文件总字节
    pub bytes: u64,
    /// 是否因达到 `cap` 而早停（页面显示 `N+`）
    pub truncated: bool,
}

/// 统计当前缓存目录的占用（`limits::CACHE_STATS_MAX_ENTRIES` 封顶）。
///
/// 只读顶层、不递归、**不建目录**：目录不存在就是 `(0, 0, false)`（PRD A4.4）。
pub fn stats() -> Stats {
    let d = dir();
    let (files, bytes, truncated) = collect_stats(&d, limits::CACHE_STATS_MAX_ENTRIES);
    Stats {
        dir: d,
        files,
        bytes,
        truncated,
    }
}

/// [`stats`] 的可测内核：`cap` 可注入，单测不用真造 5007 个文件。
fn collect_stats(dir: &Path, cap: usize) -> (u64, u64, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0, false);
    };
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;
    let mut truncated = false;
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if files as usize >= cap {
            // 还有一个文件没数 → 已达上限，早停
            truncated = true;
            break;
        }
        files += 1;
        bytes += meta.len();
    }
    (files, bytes, truncated)
}

impl Cache {
    /// 建缓存并做一次启动清理（过期 / 超量）。
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let cache = Self {
            dir,
            order: VecDeque::new(),
            map: HashMap::new(),
            mem_bytes: 0,
        };
        cache.sweep();
        cache
    }

    /// 只查内存。
    pub fn get_mem(&mut self, key: &str) -> Option<Hit> {
        let hit = self.map.get(key).map(|e| Hit {
            lyrics: Arc::clone(&e.lyrics),
            source: e.source.clone(),
        });
        if hit.is_some() {
            self.touch(key);
        }
        hit
    }

    /// 查磁盘（命中会回填内存）。
    ///
    /// **回填必须把 `disk.source` 一起带进 `Entry`**（v1.8.0 / A6.6 的命门）：
    /// 漏了这一步，第一次（磁盘命中）落盘正常、第二次（内存命中）source 变空串
    /// → 被判「非本地」→ 又落盘一次。
    pub fn get_disk(&mut self, key: &str) -> Option<Hit> {
        let path = self.positive_path(key);
        let text = std::fs::read_to_string(&path).ok()?;
        let disk: DiskLyrics = serde_json::from_str(&text).ok()?;
        if disk.lines.is_empty() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let source = disk.source.clone();
        let lyrics = Arc::new(disk.into_lyrics());
        self.insert_mem(
            key,
            Entry {
                lyrics: Arc::clone(&lyrics),
                source: source.clone(),
            },
        );
        Some(Hit { lyrics, source })
    }

    /// 写入内存 + 磁盘。`source` = provider 名（`"local"` = 用户自己的 .lrc）。
    pub fn put(&mut self, key: &str, lyrics: &Lyrics, source: &str) {
        self.insert_mem(
            key,
            Entry {
                lyrics: Arc::new(lyrics.clone()),
                source: source.to_string(),
            },
        );
        let now = now_secs();
        let disk = DiskLyrics::from_lyrics(key, lyrics, source, now);
        let Ok(json) = serde_json::to_string(&disk) else {
            return;
        };
        write_atomic(&self.positive_path(key), json.as_bytes());
    }

    /// 负缓存命中则返回原因（24h 内不再请求）。
    pub fn miss_reason(&self, key: &str) -> Option<LyricMissReason> {
        let text = std::fs::read_to_string(self.miss_path(key)).ok()?;
        let miss: DiskMiss = serde_json::from_str(&text).ok()?;
        if now_secs() - miss.at > NEG_TTL_SECS {
            return None;
        }
        Some(match miss.reason.as_str() {
            "network" => LyricMissReason::NetworkError,
            "disabled" => LyricMissReason::OnlineDisabled,
            "meta" => LyricMissReason::InsufficientMetadata,
            _ => LyricMissReason::NotFound,
        })
    }

    /// 记一条负缓存。
    pub fn put_miss(&self, key: &str, reason: LyricMissReason) {
        let disk = DiskMiss {
            key: key.to_string(),
            at: now_secs(),
            reason: match reason {
                LyricMissReason::NetworkError => "network",
                LyricMissReason::OnlineDisabled => "disabled",
                LyricMissReason::InsufficientMetadata => "meta",
                LyricMissReason::NotFound => "not_found",
            }
            .to_string(),
        };
        if let Ok(json) = serde_json::to_string(&disk) {
            write_atomic(&self.miss_path(key), json.as_bytes());
        }
    }

    /// 磁盘清理：删过期文件；仍超 5 MB 时按 `saved_at` 从旧到新删。
    pub fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let now = now_secs();
        let mut files: Vec<(PathBuf, i64, u64)> = Vec::new();
        let mut total: u64 = 0;
        for e in entries.flatten() {
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let size = meta.len();
            let saved = read_saved_at(&path).unwrap_or(0);
            let ttl = if is_miss_file(&path) {
                NEG_TTL_SECS
            } else {
                POS_TTL_SECS
            };
            if now - saved > ttl {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            total += size;
            files.push((path, saved, size));
        }
        if total <= MAX_BYTES {
            return;
        }
        files.sort_by_key(|f| f.1);
        for (path, _, size) in files {
            if total <= MAX_BYTES {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }

    // ---- 内部 ----

    fn insert_mem(&mut self, key: &str, entry: Entry) {
        let bytes = approx_bytes(&entry.lyrics);
        if let Some(old) = self.map.insert(key.to_string(), entry) {
            self.mem_bytes = self.mem_bytes.saturating_sub(approx_bytes(&old.lyrics));
        }
        self.mem_bytes += bytes;
        self.touch(key);
        // 两条淘汰条件：条数上限（架构 §7.6）与字节预算（QA 要求 512 KB）
        while self.order.len() > limits::LYRIC_LRU_CAP || self.mem_bytes > MEM_BUDGET_BYTES {
            // 至少留一条，避免单条超预算时把刚放进去的也踢掉
            if self.order.len() <= 1 {
                break;
            }
            let Some(old) = self.order.pop_back() else {
                break;
            };
            if let Some(e) = self.map.remove(&old) {
                self.mem_bytes = self.mem_bytes.saturating_sub(approx_bytes(&e.lyrics));
            }
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_front(key.to_string());
    }

    fn positive_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.json", fnv1a64(key.as_bytes())))
    }

    /// 负缓存文件路径。
    ///
    /// v1.3.0：文件名哈希的是**带版本前缀的 key**（见 [`MISS_KEY_VERSION`]），
    /// 升级后旧 miss 文件自动不再命中。
    fn miss_path(&self, key: &str) -> PathBuf {
        let versioned = miss_cache_key(key);
        self.dir
            .join(format!("miss-{:016x}.json", fnv1a64(versioned.as_bytes())))
    }
}

/// 估算一份歌词在内存里占多少字节（字符串 + Vec 容量 + 固定开销）。
///
/// 只求量级正确：`String` 的 `len()` 是实际堆占用，`Vec` 按元素数 × 元素大小算，
/// 再加上每行 64 字节的结构体开销（`LyricLine` + 分配器碎片）。
fn approx_bytes(l: &Lyrics) -> usize {
    let mut n = core::mem::size_of::<Lyrics>();
    n += l.title.len() + l.artist.len() + l.album.len() + l.by.len();
    n += l.lines.len() * (core::mem::size_of::<LyricLine>() + 64);
    for line in &l.lines {
        n += line.text.len();
        n += line.words.len() * (core::mem::size_of::<LyricWord>() + 32);
        for w in &line.words {
            n += w.text.len();
        }
    }
    n
}

/// 原子写：`.tmp` → `rename`。
fn write_atomic(path: &Path, bytes: &[u8]) {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// 读文件里的 `saved_at`（解析失败按 0 = 立即过期处理）。
fn read_saved_at(path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    if is_miss_file(path) {
        serde_json::from_str::<DiskMiss>(&text).ok().map(|d| d.at)
    } else {
        serde_json::from_str::<DiskLyrics>(&text)
            .ok()
            .map(|d| d.saved_at)
    }
}

fn is_miss_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("miss-"))
        .unwrap_or(false)
}

/// 当前 Unix 秒。
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// FNV-1a 64 位。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lyrics {
        Lyrics {
            lines: vec![
                LyricLine {
                    start_ms: 1000,
                    end_ms: 2000,
                    text: "你好".into(),
                    words: vec![
                        LyricWord {
                            text: "你".into(),
                            start_ms: 1000,
                            end_ms: 1500,
                        },
                        LyricWord {
                            text: "好".into(),
                            start_ms: 1500,
                            end_ms: 2000,
                        },
                    ],
                },
                LyricLine {
                    start_ms: 2000,
                    end_ms: 7000,
                    text: "世界".into(),
                    words: Vec::new(),
                },
            ],
            kind: LyricKind::Word,
            offset_ms: -100,
            title: "t".into(),
            artist: "a".into(),
            album: "al".into(),
            by: "b".into(),
        }
    }

    fn tmp_cache(tag: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("barline_cache_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        Cache::new(dir)
    }

    #[test]
    fn fnv_hash_is_stable() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv1a64(b"a"), fnv1a64(b"b"));
    }

    #[test]
    fn roundtrip_through_disk_preserves_everything() {
        let mut c = tmp_cache("roundtrip");
        let l = sample();
        c.put("k1", &l, "test");
        // 换一个实例，只走磁盘
        let mut c2 = Cache::new(c.dir.clone());
        // v1.8.0：`get_disk` 返回 `Option<Hit>`（歌词 + 来源），测试只关心歌词本体
        let got = c2.get_disk("k1").expect("磁盘未命中").lyrics;
        assert_eq!(got.lines.len(), 2);
        assert_eq!(got.lines[0].words.len(), 2);
        assert_eq!(got.lines[0].words[1].text, "好");
        assert_eq!(got.kind, LyricKind::Word);
        assert_eq!(got.offset_ms, -100);
        assert_eq!(got.title, "t");
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    // ------------------------------------------------ v1.8.0：来源透传（C1–C3）

    /// C1：内存命中必须带出写入时的 `source`。
    #[test]
    fn get_mem_returns_source() {
        let mut c = tmp_cache("src_mem");
        c.put("k", &sample(), "lrclib");
        let hit = c.get_mem("k").expect("内存应命中");
        assert_eq!(hit.source, "lrclib", "内存条目必须记住来源");
        assert_eq!(hit.lyrics.lines.len(), 2);
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    /// C2：**A6.6 的命门**——磁盘命中带出来源，且回填内存时**不许丢** source。
    ///
    /// 漏掉回填这一步的症状：第一次播放（磁盘命中）落盘正常，第二次播放
    /// （内存命中）source 变空串 → 被判「非本地」→ 又落盘一次。
    #[test]
    fn get_disk_returns_source_and_backfill_keeps_it() {
        let mut c = tmp_cache("src_disk");
        c.put("k", &sample(), "lrclib");
        // 换实例 = 模拟重启（内存 LRU 清空，磁盘保留）
        let mut c2 = Cache::new(c.dir.clone());
        let disk_hit = c2.get_disk("k").expect("磁盘应命中");
        assert_eq!(disk_hit.source, "lrclib", "磁盘命中必须带出 source");
        // 紧接着走内存（get_disk 已回填）——source 必须还在
        let mem_hit = c2.get_mem("k").expect("回填后内存应命中");
        assert_eq!(
            mem_hit.source, "lrclib",
            "磁盘回填内存时必须把 source 一起带进 Entry，否则二次播放会重复落盘"
        );
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    /// C3：v1.7.0 之前的老缓存文件没有 `source` 字段 → 空串，不 panic。
    #[test]
    fn legacy_disk_file_without_source_is_empty() {
        let mut c = tmp_cache("src_legacy");
        let path = c.positive_path("legacy");
        let json = r#"{"key":"legacy","saved_at":4102444800,"lines":[{"s":0,"e":1000,"t":"老歌词"}]}"#;
        write_atomic(&path, json.as_bytes());
        let hit = c.get_disk("legacy").expect("老格式必须仍可读");
        assert_eq!(hit.source, "", "缺 source 字段按空串（= 未知来源 → 照常落盘）");
        assert_eq!(hit.lyrics.lines[0].text, "老歌词");
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    // ------------------------------------------------ v1.8.0：占用统计（C4–C5）

    /// C4：只数**顶层文件**（子目录不计、不递归）；目录不存在 → `(0,0,false)`。
    #[test]
    fn stats_counts_top_level_only() {
        let dir = std::env::temp_dir().join("barline_cache_stats_top");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        std::fs::write(dir.join("a.json"), b"12345").expect("写 a");
        std::fs::write(dir.join("b.json"), b"123").expect("写 b");
        std::fs::write(dir.join("c.json"), b"1").expect("写 c");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).expect("建子目录");
        std::fs::write(sub.join("d.json"), b"999999").expect("写 d");

        let (files, bytes, truncated) = collect_stats(&dir, 5000);
        assert_eq!(files, 3, "子目录里的文件不许计入");
        assert_eq!(bytes, 5 + 3 + 1, "字节只算顶层三个文件");
        assert!(!truncated);

        // 目录不存在：不报错、不建目录
        let missing = std::env::temp_dir().join("barline_cache_stats_missing_α");
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(collect_stats(&missing, 5000), (0, 0, false));
        assert!(!missing.exists(), "统计不许顺手把目录造出来");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C5：达到上限立即早停并把 `truncated` 置真（用小 cap 测，别真造 5007 个文件）。
    #[test]
    fn stats_truncates_at_cap() {
        let dir = std::env::temp_dir().join("barline_cache_stats_cap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        for i in 0..7 {
            std::fs::write(dir.join(format!("{i:016x}.json")), b"12").expect("写文件");
        }
        let (files, _, truncated) = collect_stats(&dir, 3);
        assert_eq!(files, 3, "达到 cap 必须停在 cap");
        assert!(truncated, "还有没数到的文件 → truncated 必须为 true");

        // 恰好等于 cap 且没有更多文件 → 不算被截断
        let (files, bytes, truncated) = collect_stats(&dir, 7);
        assert_eq!((files, bytes, truncated), (7, 14, false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stats()` 的路径必须与 [`dir`] 同源（PRD A4.3：路径与占用不可能漂移）。
    #[test]
    fn stats_dir_matches_cache_dir() {
        let st = stats();
        assert_eq!(st.dir, dir());
        assert!(
            st.dir.ends_with("lyrics-cache"),
            "缓存目录名固定为 lyrics-cache，实际 {}",
            st.dir.display()
        );
    }

    #[test]
    fn memory_lru_evicts_beyond_capacity() {
        let mut c = tmp_cache("lru");
        let l = sample();
        for i in 0..(limits::LYRIC_LRU_CAP + 3) {
            c.put(&format!("k{i}"), &l, "test");
        }
        assert!(c.map.len() <= limits::LYRIC_LRU_CAP);
        // 最早的不在了，最新的在
        assert!(c.get_mem("k0").is_none());
        assert!(c.get_mem(&format!("k{}", limits::LYRIC_LRU_CAP + 2)).is_some());
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    /// 内存缓存必须按**字节**封顶（QA 要求 512 KB），不能只数条数。
    #[test]
    fn memory_cache_respects_byte_budget() {
        let mut c = tmp_cache("bytes");
        // 造一份 ~260 KB 的歌词（1200 行 × 90 字节文本 + 每行结构体开销）
        let big = Lyrics {
            lines: (0..1200)
                .map(|i| LyricLine {
                    start_ms: i * 10,
                    end_ms: i * 10 + 10,
                    text: "一二三四五六七八九十一二三四五六七八九十一二三四五".into(),
                    words: Vec::new(),
                })
                .collect(),
            ..Default::default()
        };
        assert!(approx_bytes(&big) > 150 * 1024, "样本太小，测不出预算");
        for i in 0..6 {
            c.put(&format!("big{i}"), &big, "test");
        }
        assert!(
            c.mem_bytes <= MEM_BUDGET_BYTES,
            "内存歌词 {} 字节，超过 512 KB 预算",
            c.mem_bytes
        );
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    #[test]
    fn negative_cache_roundtrip() {
        let c = tmp_cache("miss");
        assert!(c.miss_reason("k2").is_none());
        c.put_miss("k2", LyricMissReason::NetworkError);
        assert_eq!(c.miss_reason("k2"), Some(LyricMissReason::NetworkError));
        c.put_miss("k3", LyricMissReason::NotFound);
        assert_eq!(c.miss_reason("k3"), Some(LyricMissReason::NotFound));
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    /// **v1.3.0 负缓存作废的回归**：逻辑 key 必须带版本前缀，且**落盘文件名**必须
    /// 与不带前缀时不同——否则升级后旧 miss 仍然命中，新查询策略等于白干。
    #[test]
    fn negative_cache_key_changed() {
        let key = "张杰|天下|240000";
        let versioned = miss_cache_key(key);
        assert_ne!(versioned, key, "负缓存 key 必须与旧版不同");
        assert!(versioned.starts_with(MISS_KEY_VERSION), "必须带版本前缀");
        assert!(versioned.ends_with(key), "前缀之外要保留原 key，便于日志对照");

        // 真正决定"会不会命中旧文件"的是哈希后的文件名
        let c = tmp_cache("miss_version");
        let new_path = c.miss_path(key);
        let old_name = format!("miss-{:016x}.json", fnv1a64(key.as_bytes()));
        assert_ne!(
            new_path.file_name().and_then(|n| n.to_str()),
            Some(old_name.as_str()),
            "新旧负缓存文件名相同 → 升级后仍会命中旧 miss"
        );
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    #[test]
    fn expired_negative_cache_is_ignored() {
        let c = tmp_cache("miss_expired");
        let path = c.miss_path("old");
        let old = DiskMiss {
            key: "old".into(),
            at: now_secs() - NEG_TTL_SECS - 10,
            reason: "not_found".into(),
        };
        write_atomic(&path, serde_json::to_string(&old).unwrap_or_default().as_bytes());
        assert!(c.miss_reason("old").is_none());
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    #[test]
    fn sweep_removes_expired_positive_cache() {
        let c = tmp_cache("sweep");
        let path = c.positive_path("expired");
        let mut disk = DiskLyrics::from_lyrics("expired", &sample(), "test", 0);
        disk.saved_at = now_secs() - POS_TTL_SECS - 10;
        write_atomic(&path, serde_json::to_string(&disk).unwrap_or_default().as_bytes());
        assert!(path.exists());
        c.sweep();
        assert!(!path.exists(), "过期文件应被删除");
        let _ = std::fs::remove_dir_all(&c.dir);
    }

    #[test]
    fn corrupt_disk_file_is_ignored_not_panicking() {
        let mut c = tmp_cache("corrupt");
        let path = c.positive_path("bad");
        write_atomic(&path, b"{ this is not json");
        assert!(c.get_disk("bad").is_none());
        let _ = std::fs::remove_dir_all(&c.dir);
    }
}
