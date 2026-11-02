//! lrclib.net provider（`ARCHITECTURE.md` §12.4）。
//!
//! 两个接口（v1.3.0 起都用）：
//!
//! * 精确：`GET /api/get?track_name=..&artist_name=..&album_name=..&duration=..`
//! * 模糊：`GET /api/search?q=<标题 + 主歌手>`
//!
//! 响应（200，`/api/get`）：
//! ```json
//! { "id": 1, "trackName": "...", "artistName": "...",
//!   "syncedLyrics": "[00:12.34]词", "plainLyrics": "词\n词" }
//! ```
//! `/api/search` 返回**同样结构的数组**，所以一次搜索就能拿到元数据 + 歌词，
//! 本地择优（[`crate::lyric::query::pick_best`]）后直接用选中那条的 `syncedLyrics`。
//!
//! **只认 `syncedLyrics`**：`plainLyrics` 没有时间轴，无法扫光/滚动，
//! 按 §12.4 的约定视为「没有歌词」（而不是"有歌词但不滚动"——那会让用户以为功能坏了）。
//!
//! ## v1.3.0 修的一个实证 400（G6 问题 7）
//!
//! 用户日志：Edge 放 B 站视频时 SMTC 的 `artist` 是空串，拼出
//! `…&artist_name=&duration=357`，lrclib 直接 **HTTP 400**。
//! 现在 [`build_url`] **不拼空的 `artist_name`**，并且 [`LrclibProvider::fetch`]
//! 在 artist 为空时**根本不走精确接口**，直接进模糊检索（用标题单独查）。
//!
//! lrclib **要求可识别的 User-Agent**（默认的 WinHTTP UA 会被 403），
//! 所以这里显式带上项目地址。

use crate::lyric::net::{self, NetError};
use crate::lyric::parser;
use crate::lyric::provider::{LyricProvider, LyricQuery, ProviderError};
use crate::lyric::query::{self, Candidate, Want};
use crate::lyric::{LyricKind, Lyrics};

/// lrclib 的精确查询接口。
const BASE_GET: &str = "https://lrclib.net/api/get";
/// lrclib 的模糊搜索接口（v1.3.0）。
const BASE_SEARCH: &str = "https://lrclib.net/api/search";
/// 项目 UA（lrclib 要求能识别调用方）。
pub const USER_AGENT: &str = "Barline/0.0.1 (https://github.com/Yi-One1/QuLan-Barline-Next)";
/// 模糊搜索里**最多**取前几条候选做本地择优（G1 提速）。
///
/// 为什么砍到 2：`/api/search` 实测 12s 才回，一次搜索就能烧掉整个 8s 预算。
/// 而 lrclib 的返回顺序本身就带相关性（同名的录音室版通常在最前），
/// 第 3 条之后的候选只是为了冷门歌，代价是让所有歌都变慢——不值得。
/// 择优阈值（`MIN_PICK_SCORE`）与「时长差 >10s 淘汰」都还在，砍候选**不会**放宽质量门槛。
const MAX_SEARCH_CANDIDATES: usize = 2;

/// lrclib.net 歌词源。
pub struct LrclibProvider {
    enabled: bool,
}

impl LrclibProvider {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl LyricProvider for LrclibProvider {
    fn name(&self) -> &'static str {
        "lrclib"
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn fetch(&self, q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError> {
        if !self.enabled {
            return Err(ProviderError::Disabled);
        }
        let t = q.track;
        if t.title.trim().is_empty() {
            return Err(ProviderError::NotFound);
        }
        let artist = query::primary_artist(&t.artist);

        // ---- 1. 精确查询：**artist 为空时直接跳过**（空参数会让 lrclib 返回 400）----
        if !artist.is_empty() {
            let url = build_url(t);
            crate::log_debug!("请求 lrclib(get)：{url}");
            match self.fetch_once(&url) {
                Ok(l) => return Ok(l),
                // G1 提速：**不再「网络失败立刻重试一次」**。
                // 那个重试会把最坏耗时直接翻倍（12s → 24s），而 service 侧还有
                // 一道 8s 硬截止与最多 1 次整链重试；单源自己再重试等于叠三层退避。
                // 失败就交给模糊检索（它对「精确接口挂掉」这件事本来就不敏感）。
                Err(e) => crate::log_debug!("lrclib 精确未命中：{}", e.as_str()),
            }
        } else {
            crate::log_debug!("SMTC 未提供歌手，跳过 lrclib 精确查询（空 artist_name 会 400）");
        }

        // ---- 2. 模糊检索 + 本地择优 ----
        self.search(t)
    }
}

impl LrclibProvider {
    /// 发一次请求并计时（日志里带 HTTP 状态码、字节数、耗时，便于验收）。
    fn fetch_once(&self, url: &str) -> Result<Lyrics, ProviderError> {
        let t0 = std::time::Instant::now();
        let resp = net::get_with_headers(url, USER_AGENT, &[], net::TIMEOUT_LYRIC).map_err(|e| {
            match e {
                NetError::Transport(s) => ProviderError::Network(s),
                NetError::BadRequest(s) => ProviderError::Parse(s),
            }
        })?;
        crate::log_debug!(
            "lrclib 响应：HTTP {} / {} 字节 / {:.1}s",
            resp.status,
            resp.bytes,
            t0.elapsed().as_secs_f32()
        );
        match resp.status {
            200 => parse_body(&resp.body),
            404 => Err(ProviderError::NotFound),
            // 5xx 与其它 4xx 一律按网络失败处理（可能是限流 / 临时故障）
            s => Err(ProviderError::Network(format!("HTTP {s}"))),
        }
    }

    /// 模糊检索：逐个关键词试（最多 [`query::MAX_SEARCH_KEYWORDS`] 条），命中即停。
    ///
    /// G1 提速：关键词循环前先看**本源还剩多少预算**（[`net::LYRIC_PROVIDER_BUDGET_MS`]）——
    /// 前一条请求慢了就少试一条关键词，绝不让一个源把 8s 全局截止全吃掉。
    fn search(&self, track: &crate::media::Track) -> Result<Lyrics, ProviderError> {
        let want = Want::of(track);
        let keywords = query::search_keywords(&track.title, &track.artist);
        if keywords.is_empty() {
            return Err(ProviderError::NotFound);
        }
        let mut saw_network = false;
        let started = std::time::Instant::now();
        for kw in &keywords {
            if started.elapsed().as_millis() >= net::LYRIC_PROVIDER_BUDGET_MS {
                crate::log_debug!(
                    "lrclib 本源预算 {}ms 用尽，放弃剩余关键词（已试到 {kw}）",
                    net::LYRIC_PROVIDER_BUDGET_MS
                );
                break;
            }
            let url = build_search_url(kw);
            crate::log_debug!("请求 lrclib(search)：{url}");
            let t0 = std::time::Instant::now();
            let resp = match net::get_with_headers(&url, USER_AGENT, &[], net::TIMEOUT_LYRIC) {
                Ok(r) => r,
                Err(NetError::Transport(s)) => {
                    crate::log_debug!("lrclib 搜索网络失败：{s}");
                    saw_network = true;
                    continue;
                }
                Err(NetError::BadRequest(s)) => {
                    crate::log_debug!("lrclib 搜索请求非法：{s}");
                    continue;
                }
            };
            crate::log_debug!(
                "lrclib 搜索响应：HTTP {} / {} 字节 / {:.1}s（关键词={kw}）",
                resp.status,
                resp.bytes,
                t0.elapsed().as_secs_f32()
            );
            if resp.status != 200 {
                if resp.status >= 500 {
                    saw_network = true;
                }
                continue;
            }
            let hits = parse_search_body(&resp.body);
            if hits.is_empty() {
                continue;
            }
            // 只在**前 2 条**候选里择优（见 MAX_SEARCH_CANDIDATES 的说明）
            let head = &hits[..hits.len().min(MAX_SEARCH_CANDIDATES)];
            let cands: Vec<Candidate> = head.iter().map(|h| h.cand.clone()).collect();
            let Some((idx, score)) = query::pick_best(&want, &cands) else {
                crate::log_debug!(
                    "lrclib 搜索有 {} 条结果（取前 {} 条）无一达标（关键词={kw}）",
                    hits.len(),
                    cands.len()
                );
                continue;
            };
            let hit = &head[idx];
            crate::log_info!(
                "lrclib 搜索命中：id={} 「{} - {}」score={score}（关键词={kw}）",
                hit.cand.id,
                hit.cand.title,
                hit.cand.artist
            );
            if hit.synced.trim().is_empty() {
                // 选中那条只有 plainLyrics → 换下一个关键词再试
                crate::log_debug!("lrclib 选中的结果没有时间轴，继续下一个关键词");
                continue;
            }
            return lyrics_from_lrc(hit.synced.as_str(), &hit.cand);
        }
        if saw_network {
            Err(ProviderError::Network("lrclib 搜索网络失败".into()))
        } else {
            Err(ProviderError::NotFound)
        }
    }
}

/// 构造**精确查询** URL。`duration` 只在播放器上报了时长时才带。
///
/// `artist` 清洗后为空时**不拼 `artist_name=`**——空值会让 lrclib 返回 HTTP 400
/// （v1.3.0 修的实证 bug）。
pub fn build_url(track: &crate::media::Track) -> String {
    let mut url = String::with_capacity(160);
    url.push_str(BASE_GET);
    url.push_str("?track_name=");
    url.push_str(&net::percent_encode(track.title.trim()));
    let artist = query::primary_artist(&track.artist);
    if !artist.is_empty() {
        url.push_str("&artist_name=");
        url.push_str(&net::percent_encode(&artist));
    }
    if !track.album.trim().is_empty() {
        url.push_str("&album_name=");
        url.push_str(&net::percent_encode(track.album.trim()));
    }
    if track.duration_ms > 0 {
        url.push_str("&duration=");
        url.push_str(&(track.duration_ms / 1000).to_string());
    }
    url
}

/// 构造**模糊搜索** URL（v1.3.0）。
pub fn build_search_url(query_text: &str) -> String {
    let mut url = String::with_capacity(120);
    url.push_str(BASE_SEARCH);
    url.push_str("?q=");
    url.push_str(&net::percent_encode(query_text.trim()));
    url
}

/// 搜索响应里的一条（元数据 + 可能带歌词）。
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub cand: Candidate,
    pub synced: String,
}

/// 解析 `/api/search` 的响应体（数组）。结构不符的条目**跳过**，不整体失败。
pub fn parse_search_body(body: &str) -> Vec<SearchHit> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(id) = item.get("id").and_then(|x| x.as_i64()) else {
            continue;
        };
        let title = item
            .get("trackName")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if title.is_empty() {
            continue;
        }
        let artist = item
            .get("artistName")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        // lrclib 的 duration 是**秒**
        let duration_ms = item
            .get("duration")
            .and_then(|x| x.as_f64())
            .map(|s| (s * 1000.0) as i64)
            .unwrap_or(0);
        let synced = item
            .get("syncedLyrics")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        out.push(SearchHit {
            cand: Candidate {
                id,
                title,
                artist,
                duration_ms,
            },
            synced,
        });
    }
    out
}

/// `syncedLyrics` 文本 → `Lyrics`。
fn lyrics_from_lrc(synced: &str, cand: &Candidate) -> Result<Lyrics, ProviderError> {
    let mut lyrics = parser::parse(synced, LyricKind::Line);
    if lyrics.lines.is_empty() {
        return Err(ProviderError::Parse("syncedLyrics 解析不出行".into()));
    }
    lyrics.title = cand.title.clone();
    lyrics.artist = cand.artist.clone();
    Ok(lyrics)
}

/// 解析响应体。
fn parse_body(body: &str) -> Result<Lyrics, ProviderError> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| ProviderError::Parse(format!("JSON 解析失败：{e}")))?;
    let synced = v
        .get("syncedLyrics")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    if synced.trim().is_empty() {
        // 只有 plainLyrics → 无时间轴 → 按"没有歌词"处理
        return Err(ProviderError::NotFound);
    }
    let cand = Candidate {
        id: v.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
        title: v
            .get("trackName")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        artist: v
            .get("artistName")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        duration_ms: 0,
    };
    let lyrics = lyrics_from_lrc(synced, &cand)?;
    crate::log_info!(
        "lrclib 精确命中：{} 行（kind={:?}）",
        lyrics.lines.len(),
        lyrics.kind
    );
    Ok(lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Track;

    fn track(title: &str, artist: &str) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_ms: 213_000,
        }
    }

    #[test]
    fn url_contains_encoded_params_and_duration_seconds() {
        let u = build_url(&track("晴天", "周杰伦"));
        assert!(u.starts_with("https://lrclib.net/api/get?"));
        assert!(u.contains("track_name=%E6%99%B4%E5%A4%A9"));
        assert!(u.contains("artist_name=%E5%91%A8%E6%9D%B0%E4%BC%A6"));
        assert!(u.contains("duration=213"));
        // 没有专辑就不带 album_name
        assert!(!u.contains("album_name"));
    }

    #[test]
    fn url_omits_duration_when_unknown() {
        let mut t = track("A", "B");
        t.duration_ms = 0;
        assert!(!build_url(&t).contains("duration"));
    }

    #[test]
    fn url_encodes_plus_and_ampersand() {
        let u = build_url(&track("A+B & C", "D"));
        assert!(u.contains("A%2BB%20%26%20C"));
    }

    #[test]
    fn parses_synced_lyrics() {
        let body = r#"{"id":1,"syncedLyrics":"[00:01.00]第一句\n[00:03.00]第二句","plainLyrics":"x"}"#;
        let l = parse_body(body).expect("应命中");
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.lines[0].text, "第一句");
    }

    #[test]
    fn plain_only_is_not_found() {
        let body = r#"{"id":1,"syncedLyrics":null,"plainLyrics":"没有时间轴"}"#;
        assert!(matches!(parse_body(body), Err(ProviderError::NotFound)));
    }

    #[test]
    fn empty_synced_is_not_found() {
        let body = r#"{"syncedLyrics":"   "}"#;
        assert!(matches!(parse_body(body), Err(ProviderError::NotFound)));
    }

    #[test]
    fn broken_json_is_parse_error() {
        assert!(matches!(
            parse_body("not json"),
            Err(ProviderError::Parse(_))
        ));
    }

    #[test]
    fn unsynced_garbage_is_parse_error() {
        let body = r#"{"syncedLyrics":"这不是歌词"}"#;
        assert!(matches!(
            parse_body(body),
            Err(ProviderError::Parse(_))
        ));
    }

    /// **v1.3.0 修 HTTP 400 的回归**：artist 为空时不拼 `artist_name=`。
    #[test]
    fn url_omits_empty_artist() {
        let u = build_url(&track("粉碎", ""));
        assert!(!u.contains("artist_name"), "空 artist 会让 lrclib 返回 400：{u}");
        assert!(u.contains("track_name="));
        assert!(u.contains("duration=213"));

        // 只有空白也算空
        let u2 = build_url(&track("粉碎", "   "));
        assert!(!u2.contains("artist_name"));

        // 多歌手优先用主歌手
        let u3 = build_url(&track("因为爱情", "陈奕迅/王菲"));
        assert!(u3.contains("artist_name=%E9%99%88%E5%A5%95%E8%BF%85"), "应只带主歌手：{u3}");
        assert!(!u3.contains("%E7%8E%8B%E8%8F%B2"), "副歌手不该进精确查询");
    }

    #[test]
    fn search_url_is_encoded() {
        let u = build_search_url("晴天 周杰伦");
        assert!(u.starts_with("https://lrclib.net/api/search?q="));
        assert!(u.contains("%20"), "空格必须编码成 %20（不是 +）：{u}");
    }

    /// 用裁剪过的真实响应形状做夹具：数组元素带 `trackName/artistName/duration(秒)`。
    #[test]
    fn search_body_parses_and_converts_seconds() {
        let body = r#"[
          {"id": 11, "trackName": "晴天", "artistName": "周杰伦", "duration": 269.0,
           "syncedLyrics": "[00:01.00]第一句", "plainLyrics": "第一句"},
          {"id": 12, "trackName": "晴天 (Live)", "artistName": "周杰伦", "duration": 300.2,
           "syncedLyrics": null}
        ]"#;
        let hits = parse_search_body(body);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].cand.id, 11);
        assert_eq!(hits[0].cand.title, "晴天");
        assert_eq!(hits[0].cand.duration_ms, 269_000, "lrclib 的 duration 是秒，要换算");
        assert!(hits[0].synced.starts_with("[00:01"));
        assert!(hits[1].synced.is_empty(), "null 的 syncedLyrics 当空串");
    }

    #[test]
    fn search_body_tolerates_garbage() {
        assert!(parse_search_body("not json").is_empty());
        assert!(parse_search_body("{}").is_empty(), "对象不是数组");
        // 缺 id 的条目跳过，其余照收
        let body = r#"[{"trackName":"x"},{"id":3,"trackName":"y"}]"#;
        let hits = parse_search_body(body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].cand.id, 3);
    }
}
