//! `LyricProvider` 抽象与有序链（`ARCHITECTURE.md` §12.4，风险 R1 拍板）。
//!
//! 为什么要抽象：在线源只有 lrclib 一个（风险 R1），但**本地文件**也是一路 provider；
//! 未来若加备用源，只需往链里插一个 `Box<dyn LyricProvider>`，`service` 一行不用改。

use crate::lyric::locate;
use crate::lyric::parser;
use crate::lyric::LyricKind;
use crate::lyric::LyricMissReason;
use crate::lyric::Lyrics;
use crate::media::Track;

/// 一次查询的输入。
pub struct LyricQuery<'a> {
    pub track: &'a Track,
    /// 用户在设置里添加的歌词目录
    pub local_dirs: &'a [String],
    /// 是否允许联网（关闭时链里的在线 provider 直接跳过）
    pub online_enabled: bool,
}

/// provider 的错误分类。**决定重试策略**，不要合并。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 明确没有（404 / 本地没找到）→ 负缓存 24h，不重试
    NotFound,
    /// 网络层失败（超时 / 连不上 / 5xx）→ 指数退避重试 3 次
    Network(String),
    /// 拿到了但解析不出歌词 → 视为 NotFound
    Parse(String),
    /// 用户关闭了该源
    Disabled,
}

impl ProviderError {
    /// 日志友好名。
    pub fn as_str(&self) -> &str {
        match self {
            ProviderError::NotFound => "NotFound",
            ProviderError::Network(s) => s,
            ProviderError::Parse(s) => s,
            ProviderError::Disabled => "Disabled",
        }
    }
}

/// 一个歌词来源。
pub trait LyricProvider: Send + Sync {
    /// provider 名字，用于日志与缓存分区。
    fn name(&self) -> &'static str;
    /// 是否可用（在线源检查"用户已关闭在线歌词"）。
    fn is_enabled(&self) -> bool;
    /// 同步查询。实现内部自己做超时（WinHTTP 5s）。
    fn fetch(&self, q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError>;
}

// ---------------------------------------------------------------- 本地文件

/// 本地 `.lrc` 文件 provider（链首，**命中就不联网**）。
pub struct LocalProvider;

impl LyricProvider for LocalProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn fetch(&self, q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError> {
        let hit = locate::find(q.track, q.local_dirs).ok_or(ProviderError::NotFound)?;
        let text = locate::read_text(&hit.path)
            .ok_or_else(|| ProviderError::Parse(format!("读取失败：{}", hit.path.display())))?;
        let lyrics = parser::parse(&text, LyricKind::Line);
        if lyrics.lines.is_empty() {
            return Err(ProviderError::Parse(format!(
                "{} 解析不出歌词行",
                hit.path.display()
            )));
        }
        crate::log_info!(
            "本地歌词命中：{}（{} 行，kind={:?}）",
            hit.source,
            lyrics.lines.len(),
            lyrics.kind
        );
        Ok(lyrics)
    }
}

// ---------------------------------------------------------------- 有序链

/// 有序 provider 链。任一成功即返回。
pub struct ProviderChain {
    providers: Vec<Box<dyn LyricProvider>>,
}

impl ProviderChain {
    /// 按「本地 → 在线」组链。`online` 由配置决定。
    pub fn new(online: Box<dyn LyricProvider>) -> Self {
        Self {
            providers: vec![Box::new(LocalProvider), online],
        }
    }

    /// 用任意 provider 列表组链（`service` 需要把"在线"单独成链，
    /// 好在本地与在线之间插入负缓存判断）。
    pub fn with(providers: Vec<Box<dyn LyricProvider>>) -> Self {
        Self { providers }
    }

    /// 依次查询。返回 `(Some(歌词), _)` 或 `(None, 失败原因)`。
    ///
    /// 语义细节：
    /// - `NotFound` / `Parse` **不短路**，继续问下一个源（本地没有就该去网上找）；
    /// - `Network` **也不短路**，但会记下来——链走完还没有结果时，
    ///   优先报 `NetworkError`（可重试）而不是 `NotFound`（不可重试）。
    pub fn query(&self, q: &LyricQuery<'_>) -> (Option<Lyrics>, LyricMissReason) {
        let mut saw_network = false;
        for p in &self.providers {
            if !p.is_enabled() {
                continue;
            }
            match p.fetch(q) {
                Ok(l) if !l.lines.is_empty() => return (Some(l), LyricMissReason::NotFound),
                Ok(_) => continue,
                Err(ProviderError::NotFound) => continue,
                Err(ProviderError::Parse(e)) => {
                    crate::log_debug!("provider {} 解析失败：{e}", p.name());
                    continue;
                }
                Err(ProviderError::Network(e)) => {
                    crate::log_warn!("provider {} 网络失败：{e}", p.name());
                    saw_network = true;
                    continue;
                }
                Err(ProviderError::Disabled) => continue,
            }
        }
        if saw_network {
            (None, LyricMissReason::NetworkError)
        } else {
            (None, LyricMissReason::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        name: &'static str,
        enabled: bool,
        result: Result<Vec<&'static str>, ProviderError>,
    }

    impl LyricProvider for Stub {
        fn name(&self) -> &'static str {
            self.name
        }
        fn is_enabled(&self) -> bool {
            self.enabled
        }
        fn fetch(&self, _q: &LyricQuery<'_>) -> Result<Lyrics, ProviderError> {
            match &self.result {
                Ok(lines) => Ok(Lyrics {
                    lines: lines
                        .iter()
                        .map(|t| crate::lyric::LyricLine {
                            start_ms: 0,
                            end_ms: 1000,
                            text: (*t).to_string(),
                            words: Vec::new(),
                        })
                        .collect(),
                    ..Default::default()
                }),
                Err(e) => Err(e.clone()),
            }
        }
    }

    fn q(track: &Track) -> LyricQuery<'_> {
        LyricQuery {
            track,
            local_dirs: &[],
            online_enabled: true,
        }
    }

    fn chain(list: Vec<Stub>) -> ProviderChain {
        ProviderChain {
            providers: list
                .into_iter()
                .map(|s| Box::new(s) as Box<dyn LyricProvider>)
                .collect(),
        }
    }

    #[test]
    fn first_success_wins() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Ok(vec!["A"]),
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert_eq!(l.and_then(|x| x.lines.first().map(|q| q.text.clone())), Some("A".to_string()));
    }

    #[test]
    fn not_found_falls_through_to_next_provider() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Err(ProviderError::NotFound),
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert!(l.is_some());
    }

    #[test]
    fn disabled_provider_is_skipped() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: false,
                result: Ok(vec!["A"]),
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert_eq!(l.and_then(|x| x.lines.first().map(|q| q.text.clone())), Some("B".to_string()));
    }

    #[test]
    fn network_error_is_reported_over_not_found() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Err(ProviderError::Network("timeout".into())),
            },
            Stub {
                name: "b",
                enabled: true,
                result: Err(ProviderError::NotFound),
            },
        ]);
        let (l, reason) = c.query(&q(&t));
        assert!(l.is_none());
        assert_eq!(reason, LyricMissReason::NetworkError);
    }

    #[test]
    fn all_not_found_reports_not_found() {
        let t = Track::default();
        let c = chain(vec![Stub {
            name: "a",
            enabled: true,
            result: Err(ProviderError::NotFound),
        }]);
        let (l, reason) = c.query(&q(&t));
        assert!(l.is_none());
        assert_eq!(reason, LyricMissReason::NotFound);
    }

    #[test]
    fn empty_lyrics_is_treated_as_miss() {
        let t = Track::default();
        let c = chain(vec![
            Stub {
                name: "a",
                enabled: true,
                result: Ok(vec![]),
            },
            Stub {
                name: "b",
                enabled: true,
                result: Ok(vec!["B"]),
            },
        ]);
        let (l, _) = c.query(&q(&t));
        assert_eq!(l.and_then(|x| x.lines.first().map(|q| q.text.clone())), Some("B".to_string()));
    }
}
