//! AUMID / 进程名 → 播放器显示名映射表（`ARCHITECTURE.md` §14.5 / R3 拍板）。
//!
//! 双路匹配：**AUMID 优先，进程名兜底**。两条路都是「小写子串包含」，
//! 因为不同播放器上报的 AUMID 形态差异很大：
//! - 打包应用是 `Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic`
//! - 传统 win32 常常就是 `CloudMusic.exe`
//! - 浏览器是 `Chrome` / `MSEdge` 这种裸名
//!
//! M1 只需要「显示名」，`fallback_url` 供 T09（M2）点击封面时兜底跳转。

/// 一条播放器映射。
#[derive(Debug, Clone, Copy)]
pub struct PlayerInfo {
    /// 显示名（中文优先，与系统语言无关——这是播放器品牌名）
    pub display_name: &'static str,
    /// 进程名（小写，含 `.exe`）；未知时为空串
    pub process_name: &'static str,
    /// 无法激活窗口时的回退网址；空串表示没有
    pub fallback_url: &'static str,
}

/// 未知播放器。
pub const UNKNOWN: PlayerInfo = PlayerInfo {
    display_name: "未知播放器",
    process_name: "",
    fallback_url: "",
};

/// 静态映射表（20 条，覆盖国内主流 + 国际主流 + 浏览器）。
///
/// 字段顺序：`(aumid 关键字, 进程名关键字, PlayerInfo)`
pub const TABLE: &[(&str, &str, PlayerInfo)] = &[
    (
        "cloudmusic",
        "cloudmusic.exe",
        PlayerInfo {
            display_name: "网易云音乐",
            process_name: "cloudmusic.exe",
            fallback_url: "https://music.163.com",
        },
    ),
    (
        "qqmusic",
        "qqmusic.exe",
        PlayerInfo {
            display_name: "QQ 音乐",
            process_name: "qqmusic.exe",
            fallback_url: "https://y.qq.com",
        },
    ),
    (
        "tencent.qqmusic",
        "qqmusic.exe",
        PlayerInfo {
            display_name: "QQ 音乐",
            process_name: "qqmusic.exe",
            fallback_url: "https://y.qq.com",
        },
    ),
    (
        "spotify",
        "spotify.exe",
        PlayerInfo {
            display_name: "Spotify",
            process_name: "spotify.exe",
            fallback_url: "https://open.spotify.com",
        },
    ),
    (
        "qishui",
        "qishui.exe",
        PlayerInfo {
            display_name: "汽水音乐",
            process_name: "qishui.exe",
            fallback_url: "https://www.douyin.com/music",
        },
    ),
    (
        "douyinmusic",
        "douyinmusic.exe",
        PlayerInfo {
            display_name: "汽水音乐",
            process_name: "douyinmusic.exe",
            fallback_url: "https://www.douyin.com/music",
        },
    ),
    (
        "kugou",
        "kugou.exe",
        PlayerInfo {
            display_name: "酷狗音乐",
            process_name: "kugou.exe",
            fallback_url: "https://www.kugou.com",
        },
    ),
    (
        "kuwo",
        "kwmusic.exe",
        PlayerInfo {
            display_name: "酷我音乐",
            process_name: "kwmusic.exe",
            fallback_url: "https://www.kuwo.cn",
        },
    ),
    (
        "kuwomusic",
        "kuwomusic.exe",
        PlayerInfo {
            display_name: "酷我音乐",
            process_name: "kuwomusic.exe",
            fallback_url: "https://www.kuwo.cn",
        },
    ),
    (
        "foobar2000",
        "foobar2000.exe",
        PlayerInfo {
            display_name: "Foobar2000",
            process_name: "foobar2000.exe",
            fallback_url: "https://www.foobar2000.org",
        },
    ),
    (
        "potplayer",
        "potplayermini64.exe",
        PlayerInfo {
            display_name: "PotPlayer",
            process_name: "potplayermini64.exe",
            fallback_url: "https://potplayer.daum.net",
        },
    ),
    (
        "microsoft.zunemusic",
        "music.ui.exe",
        PlayerInfo {
            display_name: "媒体播放器",
            process_name: "music.ui.exe",
            fallback_url: "https://www.microsoft.com/store/productId/9WZDNCRFJ3PT",
        },
    ),
    (
        "microsoft.zunevideo",
        "video.ui.exe",
        PlayerInfo {
            display_name: "电影和电视",
            process_name: "video.ui.exe",
            fallback_url: "https://www.microsoft.com/store/productId/9WZDNCRFJ3P2",
        },
    ),
    (
        // Edge 的 SMTC AUMID 常常就是裸的 "MSEdge"，既不含 ".exe" 也不含 "microsoftedge"
        "msedge",
        "msedge.exe",
        PlayerInfo {
            display_name: "Edge",
            process_name: "msedge.exe",
            fallback_url: "https://www.bing.com",
        },
    ),
    (
        "microsoftedge",
        "msedge.exe",
        PlayerInfo {
            display_name: "Edge",
            process_name: "msedge.exe",
            fallback_url: "https://www.bing.com",
        },
    ),
    (
        "chrome",
        "chrome.exe",
        PlayerInfo {
            display_name: "Chrome",
            process_name: "chrome.exe",
            fallback_url: "https://www.google.com",
        },
    ),
    (
        "firefox",
        "firefox.exe",
        PlayerInfo {
            display_name: "Firefox",
            process_name: "firefox.exe",
            fallback_url: "https://www.mozilla.org",
        },
    ),
    (
        "brave",
        "brave.exe",
        PlayerInfo {
            display_name: "Brave",
            process_name: "brave.exe",
            fallback_url: "https://brave.com",
        },
    ),
    (
        "vlc",
        "vlc.exe",
        PlayerInfo {
            display_name: "VLC",
            process_name: "vlc.exe",
            fallback_url: "https://www.videolan.org",
        },
    ),
    (
        "aimp",
        "aimp.exe",
        PlayerInfo {
            display_name: "AIMP",
            process_name: "aimp.exe",
            fallback_url: "https://www.aimp.ru",
        },
    ),
    (
        "groove",
        "groove.exe",
        PlayerInfo {
            display_name: "Groove 音乐",
            process_name: "groove.exe",
            fallback_url: "https://www.microsoft.com/store/productId/9WZDNCRFJ3PT",
        },
    ),
];

/// 解析 AUMID（可能为空串）。永不返回 `None`——未命中时返回 [`UNKNOWN`]。
pub fn resolve(aumid: &str) -> PlayerInfo {
    let lower = aumid.to_ascii_lowercase();
    if lower.is_empty() {
        return UNKNOWN;
    }
    for (aumid_key, process_key, info) in TABLE {
        if lower.contains(aumid_key) {
            return *info;
        }
        if !process_key.is_empty() && lower.contains(process_key) {
            return *info;
        }
    }
    UNKNOWN
}

/// 用进程名兜底匹配（WASAPI 通道只有进程名，没有 AUMID）。
pub fn resolve_by_process(process_name: &str) -> PlayerInfo {
    let lower = process_name.to_ascii_lowercase();
    if lower.is_empty() {
        return UNKNOWN;
    }
    for (_aumid_key, process_key, info) in TABLE {
        if !process_key.is_empty() && lower.contains(process_key) {
            return *info;
        }
    }
    UNKNOWN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_at_least_twenty_entries() {
        assert!(TABLE.len() >= 20, "映射表条目数 = {}", TABLE.len());
    }

    #[test]
    fn resolves_cloudmusic_aumid() {
        assert_eq!(resolve("CloudMusic.exe").display_name, "网易云音乐");
        assert_eq!(
            resolve("cloudmusic.exe").process_name,
            "cloudmusic.exe"
        );
    }

    #[test]
    fn resolves_packaged_app_aumid() {
        let p = resolve("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic");
        assert_eq!(p.display_name, "媒体播放器");
    }

    #[test]
    fn resolves_edge_and_chrome() {
        assert_eq!(resolve("MSEdge").display_name, "Edge");
        assert_eq!(resolve("Chrome").display_name, "Chrome");
    }

    #[test]
    fn unknown_falls_back_without_panic() {
        assert_eq!(resolve("SomeUnknownPlayer.exe").display_name, UNKNOWN.display_name);
        assert_eq!(resolve("").display_name, UNKNOWN.display_name);
        assert_eq!(resolve_by_process("").display_name, UNKNOWN.display_name);
    }

    #[test]
    fn process_name_path_matches() {
        assert_eq!(resolve_by_process("Spotify.exe").display_name, "Spotify");
    }
}
