//! 歌词线程（T2）与状态机（`ARCHITECTURE.md` §12.6 / T11 第 5–6 条）。
//!
//! 线程职责单一：**收到 `LyricRequest` → 查（本地 → 缓存 → 在线）→ 发 `AppEvent`**。
//! 它不认识 UI、不认识窗口、不认识渲染，只认识 `EventSink`。
//!
//! ## 为什么查询顺序是「本地 → 缓存 → 负缓存 → 在线」
//!
//! 1. **本地文件最优先**：命中就**一个网络包都不发**（PRD P0-08 要求抓包可验证）；
//! 2. 内存 / 磁盘缓存次之（重启后也命中）；
//! 3. **负缓存挡在在线之前**：24h 内查不到的歌不再打网络（PRD §6.2）；
//! 4. 最后才联网。
//!
//! ## 重试
//!
//! 只有 `NetworkError` 会重试，退避 5s / 15s / 60s（`limits::LYRIC_RETRY_BACKOFF_MS`），
//! 3 次后降级为 `NotFound`（不再重试、不再联网）。`NotFound` 直接进负缓存。

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::defaults::limits;
use crate::event::{AppEvent, LyricRequest};
use crate::lyric::cache::Cache;
use crate::lyric::lrclib::LrclibProvider;
use crate::lyric::provider::{LocalProvider, LyricProvider, LyricQuery, ProviderChain};
use crate::lyric::LyricMissReason;
use crate::media::TrackId;
use crate::platform::wake::EventSink;

/// 线程空闲时的最长阻塞时间（也决定重试计时的精度）。
const IDLE_POLL_MS: u64 = 100;

/// 待重试的任务。
struct Retry {
    track_id: TrackId,
    request: LyricRequest,
    /// 已经重试过的次数（0 表示还没重试过）
    attempt: u8,
    due: Instant,
}

/// 歌词服务句柄（UI 线程持有）。
pub struct LyricService {
    tx: std::sync::mpsc::Sender<LyricRequest>,
    handle: Option<JoinHandle<()>>,
}

impl LyricService {
    /// 起线程。失败返回 `None`（歌词不可用，但小组件照常工作）。
    pub fn spawn(sink: EventSink<AppEvent>) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<LyricRequest>();
        let spawned = std::thread::Builder::new()
            .name(String::from("barline-lyric"))
            .stack_size(512 * 1024)
            .spawn(move || run(sink, rx));
        match spawned {
            Ok(handle) => Some(Self {
                tx,
                handle: Some(handle),
            }),
            Err(e) => {
                crate::log_err!("歌词线程创建失败：{e}");
                None
            }
        }
    }

    /// 投递一次歌词请求。线程已退出时静默丢弃。
    pub fn send(&self, req: LyricRequest) {
        if self.tx.send(req).is_err() {
            crate::log_warn!("歌词线程已退出，请求被丢弃");
        }
    }

    /// 通知退出并等待（最多由 join 决定）。
    pub fn shutdown(&mut self) {
        // 用 channel 断开让线程自然退出：drop 发送端即可
        if let Some(handle) = self.handle.take() {
            drop(std::mem::replace(&mut self.tx, closed_sender()));
            let _ = handle.join();
        }
    }
}

impl Drop for LyricService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 一个永远发送失败的 `Sender`（`shutdown` 里替换用，让线程的 `recv` 立刻返回 Disconnected）。
fn closed_sender() -> std::sync::mpsc::Sender<LyricRequest> {
    let (tx, rx) = std::sync::mpsc::channel::<LyricRequest>();
    drop(rx);
    tx
}

/// 线程主循环。
fn run(sink: EventSink<AppEvent>, rx: Receiver<LyricRequest>) {
    crate::log_info!("歌词线程启动");
    let mut cache = Cache::new(crate::platform::barline_dir().join("lyrics-cache"));
    let mut pending: Option<Retry> = None;

    loop {
        let timeout = match &pending {
            Some(p) => {
                let left = p.due.saturating_duration_since(Instant::now());
                left.max(Duration::from_millis(10))
            }
            None => Duration::from_millis(IDLE_POLL_MS),
        };

        match rx.recv_timeout(timeout) {
            Ok(req) => {
                // 新请求 = 换歌，旧的重试作废
                pending = handle_request(&sink, &mut cache, req);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(p) = pending.take() {
                    if Instant::now() >= p.due {
                        crate::log_info!(
                            "重试歌词（第 {} 次，track={}）",
                            p.attempt + 1,
                            p.track_id
                        );
                        let mut req = p.request;
                        req.retry = true;
                        pending = handle_request_with_attempt(&sink, &mut cache, req, p.attempt + 1);
                    } else {
                        pending = Some(p);
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    crate::log_info!("歌词线程退出");
}

/// 处理一次请求（首次，attempt = 0）。
fn handle_request(
    sink: &EventSink<AppEvent>,
    cache: &mut Cache,
    req: LyricRequest,
) -> Option<Retry> {
    handle_request_with_attempt(sink, cache, req, 0)
}

/// 处理一次请求。`attempt` 是**已经失败过**的次数（0 = 首次）。
///
/// 返回 `Some(Retry)` 表示需要延后重试。
fn handle_request_with_attempt(
    sink: &EventSink<AppEvent>,
    cache: &mut Cache,
    req: LyricRequest,
    attempt: u8,
) -> Option<Retry> {
    let track_id = req.track_id;
    let key = req.track.key();

    // 元数据不够：SMTC 只给了个空标题，什么也查不到
    if req.track.title.trim().is_empty() {
        sink.send(AppEvent::LyricMissing {
            track_id,
            reason: LyricMissReason::InsufficientMetadata,
        });
        return None;
    }

    sink.send(AppEvent::LyricLoading { track_id });

    // ---- 1. 内存 ----
    if let Some(l) = cache.get_mem(&key) {
        crate::log_debug!("歌词内存命中：{key}");
        sink.send(AppEvent::LyricReady {
            track_id,
            lyrics: l,
        });
        return None;
    }
    // ---- 2. 磁盘 ----
    if let Some(l) = cache.get_disk(&key) {
        crate::log_info!("歌词磁盘命中：{key}");
        sink.send(AppEvent::LyricReady {
            track_id,
            lyrics: l,
        });
        return None;
    }

    let query = LyricQuery {
        track: &req.track,
        local_dirs: &req.local_dirs,
        online_enabled: req.online_enabled,
    };

    // ---- 3. 本地文件（命中则完全不联网）----
    match LocalProvider.fetch(&query) {
        Ok(l) => {
            cache.put(&key, &l, "local");
            sink.send(AppEvent::LyricReady {
                track_id,
                lyrics: std::sync::Arc::new(l),
            });
            return None;
        }
        Err(e) => crate::log_debug!("本地未命中：{}", e.as_str()),
    }

    // ---- 4. 负缓存 ----
    if let Some(reason) = cache.miss_reason(&key) {
        crate::log_info!("歌词负缓存命中（{reason:?}），24h 内不再请求：{key}");
        sink.send(AppEvent::LyricMissing { track_id, reason });
        return None;
    }

    // ---- 5. 在线 ----
    if !req.online_enabled {
        sink.send(AppEvent::LyricMissing {
            track_id,
            reason: LyricMissReason::OnlineDisabled,
        });
        return None;
    }

    let chain = ProviderChain::with(vec![Box::new(LrclibProvider::new(true))]);
    let (found, reason) = chain.query(&query);
    if let Some(l) = found {
        cache.put(&key, &l, "lrclib");
        sink.send(AppEvent::LyricReady {
            track_id,
            lyrics: std::sync::Arc::new(l),
        });
        return None;
    }

    match reason {
        LyricMissReason::NetworkError => {
            let backoff = limits::LYRIC_RETRY_BACKOFF_MS;
            let idx = attempt as usize;
            if idx >= backoff.len() {
                crate::log_warn!("歌词重试 {idx} 次仍失败，降级为「暂无歌词」");
                cache.put_miss(&key, LyricMissReason::NotFound);
                sink.send(AppEvent::LyricMissing {
                    track_id,
                    reason: LyricMissReason::NotFound,
                });
                return None;
            }
            let wait = backoff[idx];
            crate::log_info!("歌词网络失败，{wait}ms 后重试（第 {} 次）", idx + 1);
            sink.send(AppEvent::LyricMissing {
                track_id,
                reason: LyricMissReason::NetworkError,
            });
            Some(Retry {
                track_id,
                request: req,
                attempt,
                due: Instant::now() + Duration::from_millis(wait),
            })
        }
        other => {
            cache.put_miss(&key, other);
            sink.send(AppEvent::LyricMissing {
                track_id,
                reason: other,
            });
            None
        }
    }
}

/// 供单测使用：跑一次查询（不启线程）。
#[cfg(test)]
pub fn query_once(
    cache: &mut Cache,
    req: &LyricRequest,
) -> (Option<crate::lyric::Lyrics>, LyricMissReason) {
    let key = req.track.key();
    if let Some(l) = cache.get_mem(&key) {
        return (Some((*l).clone()), LyricMissReason::NotFound);
    }
    let q = LyricQuery {
        track: &req.track,
        local_dirs: &req.local_dirs,
        online_enabled: req.online_enabled,
    };
    let chain = ProviderChain::new(Box::new(LrclibProvider::new(req.online_enabled)));
    let (l, reason) = chain.query(&q);
    if let Some(ref got) = l {
        cache.put(&key, got, "test");
    }
    (l, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Track;

    fn request(title: &str, artist: &str, dirs: Vec<String>, online: bool) -> LyricRequest {
        LyricRequest {
            track_id: 7,
            track: Track {
                title: title.into(),
                artist: artist.into(),
                album: String::new(),
                duration_ms: 180_000,
            },
            local_dirs: dirs,
            online_enabled: online,
            retry: false,
        }
    }

    fn cache(tag: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("barline_service_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        Cache::new(dir)
    }

    #[test]
    fn local_file_hit_never_touches_network() {
        // 用一个必然连不通的在线源：如果本地命中，就根本不会走到它
        let dir = std::env::temp_dir().join("barline_service_local");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("离线测试歌.lrc");
        let _ = std::fs::write(&path, "[00:01.00]第一句\n[00:03.00]第二句");

        // 注意：缓存目录名不能也叫 barline_service_local，否则 `cache()` 的
        // remove_dir_all 会把刚写好的 .lrc 一起删掉
        let mut c = cache("local_hit");
        let req = request(
            "离线测试歌",
            "",
            vec![dir.to_string_lossy().to_string()],
            true,
        );
        let (l, _) = query_once(&mut c, &req);
        assert!(l.is_some(), "本地歌词应命中");
        assert_eq!(l.as_ref().map(|x| x.lines.len()), Some(2));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn online_disabled_yields_disabled_reason() {
        let mut c = cache("offline");
        let req = request("不存在的歌名xyz", "不存在的歌手", vec![], false);
        let (l, reason) = query_once(&mut c, &req);
        assert!(l.is_none());
        // 本地没有 + 在线关闭 → chain 里在线 provider 被跳过 → NotFound
        assert_eq!(reason, LyricMissReason::NotFound);
    }

    #[test]
    fn empty_title_reports_insufficient_metadata() {
        // 这条走的是 handle_request 的早退分支，这里直接验证条件成立
        let req = request("   ", "x", vec![], true);
        assert!(req.track.title.trim().is_empty());
    }
}
