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

/// 三层缓存。
pub struct Cache {
    dir: PathBuf,
    /// 内存 LRU 的顺序表（头部最新）
    order: VecDeque<String>,
    map: HashMap<String, Arc<Lyrics>>,
    /// 内存里这些歌词的大致字节数（用于按预算淘汰）
    mem_bytes: usize,
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
    pub fn get_mem(&mut self, key: &str) -> Option<Arc<Lyrics>> {
        let hit = self.map.get(key).cloned();
        if hit.is_some() {
            self.touch(key);
        }
        hit
    }

    /// 查磁盘（命中会回填内存）。
    pub fn get_disk(&mut self, key: &str) -> Option<Arc<Lyrics>> {
        let path = self.positive_path(key);
        let text = std::fs::read_to_string(&path).ok()?;
        let disk: DiskLyrics = serde_json::from_str(&text).ok()?;
        if disk.lines.is_empty() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let lyrics = Arc::new(disk.into_lyrics());
        self.insert_mem(key, lyrics.clone());
        Some(lyrics)
    }

    /// 写入内存 + 磁盘。`source` 只用于诊断。
    pub fn put(&mut self, key: &str, lyrics: &Lyrics, source: &str) {
        self.insert_mem(key, Arc::new(lyrics.clone()));
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

    fn insert_mem(&mut self, key: &str, lyrics: Arc<Lyrics>) {
        if let Some(old) = self.map.insert(key.to_string(), lyrics.clone()) {
            self.mem_bytes = self.mem_bytes.saturating_sub(approx_bytes(&old));
        }
        self.mem_bytes += approx_bytes(&lyrics);
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
            if let Some(l) = self.map.remove(&old) {
                self.mem_bytes = self.mem_bytes.saturating_sub(approx_bytes(&l));
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
        let got = c2.get_disk("k1").expect("磁盘未命中");
        assert_eq!(got.lines.len(), 2);
        assert_eq!(got.lines[0].words.len(), 2);
        assert_eq!(got.lines[0].words[1].text, "好");
        assert_eq!(got.kind, LyricKind::Word);
        assert_eq!(got.offset_ms, -100);
        assert_eq!(got.title, "t");
        let _ = std::fs::remove_dir_all(&c.dir);
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
