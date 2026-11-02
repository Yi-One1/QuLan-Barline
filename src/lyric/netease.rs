//! 网易云音乐歌词源（v1.3.0 / G6 问题 7，lrclib 之后的兜底）。
//!
//! 两个接口（实测本机可用：普通 UA + `Referer: https://music.163.com/`，
//! **无需登录、无 Key**）：
//!
//! * 搜索：`GET /api/search/get?s=<关键词>&type=1&limit=5`
//!   → `result.songs[]`，每条含 `id` / `name` / `artists[].name` / `duration`(ms)
//! * 歌词：`GET /api/song/lyric?id=<id>&lv=1&kv=1`
//!   → `lrc.lyric`（带时间轴的标准 LRC，直接喂现有 parser）
//!
//! ## 风控与失败姿态
//!
//! 网易云没有 SLA，接口字段随时可能变、也可能临时要求验证。所以这里的策略是
//! **彻底静默**：非 200 / JSON 结构变化 / `lrc.lyric` 缺失 / 网络失败 →
//! 一律当作 `NotFound`（或 `Network`）往下一环走，只记 `log_debug`，
//! 不弹窗、不影响任何现有功能（一个兜底源绝不能把主流程搞坏）。
//!
//! ## 请求数上限
//!
//! 搜索关键词**逐条试、命中即停**，最多 [`query::MAX_SEARCH_KEYWORDS`] 条；
//! 拿到候选后只在本地择优，选中谁才发**一次**歌词请求。所以最坏开销是
//! 「3 次搜索 + 1 次歌词」，绝不会按候选数量放大。

use crate::lyric::net::{self, NetError};
use crate::lyric::parser;
use crate::lyric::provider::{LyricProvider, LyricQuery, ProviderError};
use crate::lyric::query::{self, Candidate, Want};
use crate::lyric::{LyricKind, Lyrics};

/// 搜索接口（`type=1` = 单曲）。
const BASE_SEARCH: &str = "https://music.163.com/api/search/get";
/// 歌词接口（`lv=1` = 逐行 LRC）。
const BASE_LYRIC: &str = "https://music.163.com/api/song/lyric";
/// 搜索返回条数（够本地择优即可，别贪多）。
const SEARCH_LIMIT: u32 = 5;
/// UA：普通浏览器串（默认 WinHTTP UA 会被网易云直接拒）。
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Barline/1.3.0";
/// 网易云接口对 `Referer` 有校验（不带会返回空 body / 风控页）。
pub const REFERER: &str = "https://music.163.com/";
/// 额外请求头。
const HEADERS: &[(&str, &str)] = &[("Referer", REFERER)];

/// 网易云歌词源（兜底）。
pub struct NeteaseProvider {
    enabled: bool,
}

impl NeteaseProvider {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// 搜索 + 择优 + 取歌词。命中返回 `Lyrics`。
    fn search(&self, track: &crate::media::Track) -> Result<Lyrics, ProviderError> {
        let want = Want::of(track);
        let keywords = query::search_keywords(&track.title, &track.artist);
        if keywords.is_empty() {
            return Err(ProviderError::NotFound);
        }
        let mut saw_network = false;
        let started = std::time::Instant::now();

        for kw in &keywords {
            // G1 提速：本源预算用尽就不再发新请求（网易云只是兜底源，不该吃满 8s）
            if started.elapsed().as_millis() >= net::LYRIC_PROVIDER_BUDGET_MS {
                crate::log_debug!(
                    "netease 本源预算 {}ms 用尽，放弃剩余关键词（已试到 {kw}）",
                    net::LYRIC_PROVIDER_BUDGET_MS
                );
                break;
            }
            let url = build_search_url(kw);
            crate::log_debug!("请求 netease(search)：{url}");
            let t0 = std::time::Instant::now();
            let resp = match net::get_with_headers(&url, USER_AGENT, HEADERS, net::TIMEOUT_LYRIC) {
                Ok(r) => r,
                Err(NetError::Transport(s)) => {
                    crate::log_debug!("netease 搜索网络失败：{s}");
                    saw_network = true;
                    continue;
                }
                Err(NetError::BadRequest(s)) => {
                    crate::log_debug!("netease 搜索请求非法：{s}");
                    continue;
                }
            };
            crate::log_debug!(
                "netease 搜索响应：HTTP {} / {} 字节 / {:.1}s（关键词={kw}）",
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
            let cands = parse_search(&resp.body);
            if cands.is_empty() {
                continue;
            }
            let Some((idx, score)) = query::pick_best(&want, &cands) else {
                crate::log_debug!("netease 搜索有 {} 条结果但无一达标（关键词={kw}）", cands.len());
                continue;
            };
            let hit = &cands[idx];
            crate::log_info!(
                "netease 搜索命中：id={} 「{} - {}」score={score}（关键词={kw}）",
                hit.id,
                hit.title,
                hit.artist
            );
            match self.fetch_lyric(hit) {
                Ok(l) => return Ok(l),
                Err(ProviderError::Network(e)) => {
                    saw_network = true;
                    crate::log_debug!("netease 取歌词网络失败：{e}");
                }
                Err(e) => crate::log_debug!("netease 取歌词未命中：{}", e.as_str()),
            }
        }

        if saw_network {
            Err(ProviderError::Network("netease 网络失败".into()))
        } else {
            Err(ProviderError::NotFound)
        }
    }

    /// 取一条候选的 LRC。
    fn fetch_lyric(&self, cand: &Candidate) -> Result<Lyrics, ProviderError> {
        let url = build_lyric_url(cand.id);
        crate::log_debug!("请求 netease(lyric)：{url}");
        let t0 = std::time::Instant::now();
        let resp = net::get_with_headers(&url, USER_AGENT, HEADERS, net::TIMEOUT_LYRIC).map_err(
            |e| match e {
                NetError::Transport(s) => ProviderError::Network(s),
                NetError::BadRequest(s) => ProviderError::Parse(s),
            },
        )?;
        crate::log_debug!(
            "netease 歌词响应：HTTP {} / {} 字节 / {:.1}s",
            resp.status,
            resp.bytes,
            t0.elapsed().as_secs_f32()
        );
        if resp.status != 200 {
            return Err(ProviderError::NotFound);
        }
        let Some(lrc) = parse_lyric(&resp.body) else {
            // 结构变化 / 没版权 / 纯音乐无词 → 都按"没有"处理
            return Err(ProviderError::NotFound);
        };
        let mut lyrics = parser::parse(&lrc, LyricKind::Line);
        if lyrics.lines.is_empty() {
            return Err(ProviderError::Parse("netease lrc 解析不出行".into()));
        }
        lyrics.title = cand.title.clone();
        lyrics.artist = cand.artist.clone();
        crate::log_info!("netease 歌词命中：{} 行", lyrics.lines.len());
        Ok(lyrics)
    }
}

impl LyricProvider for NeteaseProvider {
    fn name(&self) -> &'static str {
        "netease"
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn fetch(&self, q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError> {
        if !self.enabled {
            return Err(ProviderError::Disabled);
        }
        if q.track.title.trim().is_empty() {
            return Err(ProviderError::NotFound);
        }
        self.search(q.track)
    }
}

/// 搜索 URL。
pub fn build_search_url(keyword: &str) -> String {
    let mut url = String::with_capacity(160);
    url.push_str(BASE_SEARCH);
    url.push_str("?s=");
    url.push_str(&net::percent_encode(keyword.trim()));
    url.push_str("&type=1&limit=");
    url.push_str(&SEARCH_LIMIT.to_string());
    url
}

/// 歌词 URL。
pub fn build_lyric_url(id: i64) -> String {
    let mut url = String::with_capacity(96);
    url.push_str(BASE_LYRIC);
    url.push_str("?id=");
    url.push_str(&id.to_string());
    url.push_str("&lv=1&kv=1");
    url
}

/// 解析搜索响应体 → 候选列表。**任何结构异常都退化为"空列表"**，绝不 panic。
///
/// 真实形状（裁剪）：
/// ```json
/// {"result":{"songs":[{"id":186016,"name":"晴天",
///   "artists":[{"name":"周杰伦"}],"duration":269000}]},"code":200}
/// ```
pub fn parse_search(body: &str) -> Vec<Candidate> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(songs) = v
        .get("result")
        .and_then(|r| r.get("songs"))
        .and_then(|s| s.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(songs.len());
    for song in songs {
        let Some(id) = song.get("id").and_then(|x| x.as_i64()) else {
            continue;
        };
        let title = song
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if title.is_empty() {
            continue;
        }
        let artist = song
            .get("artists")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        let duration_ms = song
            .get("duration")
            .and_then(|x| x.as_i64())
            .unwrap_or(0);
        out.push(Candidate {
            id,
            title,
            artist,
            duration_ms,
        });
    }
    out
}

/// 解析歌词响应体 → LRC 文本。缺 `lrc.lyric` / 空文本 → `None`。
///
/// 真实形状（裁剪）：
/// ```json
/// {"lrc":{"version":1,"lyric":"[00:00.00] 词"},"code":200}
/// ```
pub fn parse_lyric(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = v.get("lrc")?.get("lyric")?.as_str()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 裁剪自真实响应的搜索夹具（只保留用到的字段）。
    const SEARCH_JSON: &str = r#"{
      "result": {"songCount": 2, "songs": [
        {"id": 186016, "name": "晴天", "duration": 269000,
         "artists": [{"id": 6452, "name": "周杰伦"}, {"id": 1, "name": "某人"}]},
        {"id": 186017, "name": "晴天 (Live)", "duration": 300200,
         "artists": [{"id": 6452, "name": "周杰伦"}]}
      ]},
      "code": 200
    }"#;

    /// 裁剪自真实响应的歌词夹具。
    const LYRIC_JSON: &str = r#"{
      "lrc": {"version": 1, "lyric": "[00:00.00] 晴天\n[00:12.34] 故事的小黄花"},
      "code": 200
    }"#;

    #[test]
    fn netease_search_json_parses() {
        let cands = parse_search(SEARCH_JSON);
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].id, 186016);
        assert_eq!(cands[0].title, "晴天");
        assert_eq!(cands[0].artist, "周杰伦", "多歌手取第一个");
        assert_eq!(cands[0].duration_ms, 269_000, "网易云的 duration 是毫秒");
        assert_eq!(cands[1].id, 186017);
    }

    #[test]
    fn netease_lyric_json_parses() {
        let lrc = parse_lyric(LYRIC_JSON).expect("应取到 lrc.lyric");
        assert!(lrc.starts_with("[00:00.00]"));
        let lyrics = parser::parse(&lrc, LyricKind::Line);
        assert_eq!(lyrics.lines.len(), 2, "喂现有 parser 应解出 2 行");
    }

    /// 结构不符一律**静默返回空/None**，绝不 panic（风控页 / 接口改版）。
    #[test]
    fn netease_tolerates_garbage() {
        assert!(parse_search("not json").is_empty());
        assert!(parse_search("{}").is_empty());
        assert!(parse_search(r#"{"result":{}}"#).is_empty());
        assert!(parse_search(r#"{"code":200}"#).is_empty());
        // 单条缺 id / 缺 name 跳过，其余照收
        let partial = r#"{"result":{"songs":[{"name":"x"},{"id":5,"name":"y"}]}}"#;
        let got = parse_search(partial);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, 5);

        // 缺 lrc 字段 → None（不是 panic）
        assert!(parse_lyric(r#"{"code":200}"#).is_none());
        assert!(parse_lyric(r#"{"lrc":{}}"#).is_none());
        assert!(parse_lyric(r#"{"lrc":{"lyric":""}}"#).is_none());
        assert!(parse_lyric(r#"{"lrc":{"lyric":"   "}}"#).is_none());
        assert!(parse_lyric("not json").is_none());
    }

    #[test]
    fn netease_urls_carry_required_params() {
        let s = build_search_url("晴天 周杰伦");
        assert!(s.starts_with("https://music.163.com/api/search/get?s="));
        assert!(s.contains("&type=1"), "必须限定单曲");
        assert!(s.contains("&limit="));
        assert!(!s.contains(' '), "关键词必须编码");

        let l = build_lyric_url(186016);
        assert!(l.contains("id=186016"));
        assert!(l.contains("lv=1"));
    }

    /// 兜底源必须受「在线歌词」开关控制。
    #[test]
    fn netease_respects_online_switch() {
        let off = NeteaseProvider::new(false);
        assert!(!off.is_enabled());
        let t = crate::media::Track {
            title: "晴天".into(),
            ..Default::default()
        };
        let q = LyricQuery {
            track: &t,
            local_dirs: &[],
            online_enabled: false,
        };
        assert!(matches!(off.fetch(&q), Err(ProviderError::Disabled)));
    }
}
