//! lrclib.net provider（`ARCHITECTURE.md` §12.4）。
//!
//! `GET https://lrclib.net/api/get?track_name=..&artist_name=..&album_name=..&duration=..`
//!
//! 响应（200）：
//! ```json
//! { "id": 1, "trackName": "...", "artistName": "...",
//!   "syncedLyrics": "[00:12.34]词", "plainLyrics": "词\n词" }
//! ```
//! **只认 `syncedLyrics`**：`plainLyrics` 没有时间轴，无法扫光/滚动，
//! 按 §12.4 的约定视为「没有歌词」（而不是"有歌词但不滚动"——那会让用户以为功能坏了）。
//!
//! lrclib **要求可识别的 User-Agent**（默认的 WinHTTP UA 会被 403），
//! 所以这里显式带上项目地址。

use crate::lyric::net::{self, NetError};
use crate::lyric::parser;
use crate::lyric::provider::{LyricProvider, LyricQuery, ProviderError};
use crate::lyric::{LyricKind, Lyrics};

/// lrclib 的接口根。
const BASE: &str = "https://lrclib.net/api/get";
/// 项目 UA（lrclib 要求能识别调用方）。
pub const USER_AGENT: &str = "Barline/0.0.1 (https://github.com/Yi-One1/QuLan-Barline-Next)";

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

        let url = build_url(t);
        crate::log_info!("请求 lrclib：{url}");
        match self.fetch_once(&url) {
            // 超时 / 5xx：**立刻**重试一次（总预算 12s × 2 = 24s ≤ 25s）。
            // 为什么不用 service 的退避（5s/15s/60s）：那个是给"整首查不到"用的，
            // 而这里只是"这一次慢了"，等 5s 再试等于白白多等 5s。
            Err(ProviderError::Network(e)) => {
                crate::log_warn!("lrclib 首次失败（{e}），立刻重试一次");
                self.fetch_once(&url)
            }
            other => other,
        }
    }
}

impl LrclibProvider {
    /// 发一次请求并计时（日志里带 HTTP 状态码、字节数、耗时，便于验收）。
    fn fetch_once(&self, url: &str) -> Result<Lyrics, ProviderError> {
        let t0 = std::time::Instant::now();
        let resp = net::get(url, USER_AGENT).map_err(|e| match e {
            NetError::Transport(s) => ProviderError::Network(s),
            NetError::BadRequest(s) => ProviderError::Parse(s),
        })?;
        crate::log_info!(
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
}

/// 构造请求 URL。`duration` 只在播放器上报了时长时才带。
pub fn build_url(track: &crate::media::Track) -> String {
    let mut url = String::with_capacity(160);
    url.push_str(BASE);
    url.push_str("?track_name=");
    url.push_str(&net::percent_encode(track.title.trim()));
    url.push_str("&artist_name=");
    url.push_str(&net::percent_encode(track.artist.trim()));
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
    let lyrics = parser::parse(synced, LyricKind::Line);
    if lyrics.lines.is_empty() {
        return Err(ProviderError::Parse("syncedLyrics 解析不出行".into()));
    }
    crate::log_info!(
        "lrclib 命中：{} 行（kind={:?}）",
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
}
