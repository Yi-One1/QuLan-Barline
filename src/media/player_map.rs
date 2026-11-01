//! AUMID / 进程名 → 播放器显示名映射表（`ARCHITECTURE.md` §14.5 / R3 拍板）。
//!
//! 双路匹配：**AUMID 优先，进程名兜底**。两条路都是「小写子串包含」，
//! 因为不同播放器上报的 AUMID 形态差异很大：
//! - 打包应用是 `Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic`
//! - 传统 win32 常常就是 `CloudMusic.exe`
//! - 浏览器是 `Chrome` / `MSEdge` 这种裸名
//!
//! M1 只需要「显示名」；v1.1.16 起不再有网页回退（回退字段已删除）。

/// 一条播放器映射。
#[derive(Debug, Clone, Copy)]
pub struct PlayerInfo {
    /// 显示名（中文优先，与系统语言无关——这是播放器品牌名）
    pub display_name: &'static str,
    /// 进程名（小写，含 `.exe`）；未知时为空串
    pub process_name: &'static str,
}

/// 未知播放器。
pub const UNKNOWN: PlayerInfo = PlayerInfo {
    display_name: "未知播放器",
    process_name: "",
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
        },
    ),
    (
        "qqmusic",
        "qqmusic.exe",
        PlayerInfo {
            display_name: "QQ 音乐",
            process_name: "qqmusic.exe",
        },
    ),
    (
        "tencent.qqmusic",
        "qqmusic.exe",
        PlayerInfo {
            display_name: "QQ 音乐",
            process_name: "qqmusic.exe",
        },
    ),
    (
        "spotify",
        "spotify.exe",
        PlayerInfo {
            display_name: "Spotify",
            process_name: "spotify.exe",
        },
    ),
    (
        // v1.1.16（问题 6/7）：汽水音乐的真实进程名是 SodaMusic.exe（实测
        // D:\APP\汽水音乐\Soda Music\3.1.2\SodaMusic.exe，多进程），SMTC AUMID
        // 也不含 "qishui"——旧键匹配不到，导致「未知播放器」与音量/激活全部失联。
        "sodamusic",
        "sodamusic.exe",
        PlayerInfo {
            display_name: "汽水音乐",
            process_name: "sodamusic.exe",
        },
    ),
    (
        "qishui",
        "sodamusic.exe",
        PlayerInfo {
            display_name: "汽水音乐",
            process_name: "sodamusic.exe",
        },
    ),
    (
        "douyinmusic",
        "sodamusic.exe",
        PlayerInfo {
            display_name: "汽水音乐",
            process_name: "sodamusic.exe",
        },
    ),
    (
        "kugou",
        "kugou.exe",
        PlayerInfo {
            display_name: "酷狗音乐",
            process_name: "kugou.exe",
        },
    ),
    (
        "kuwo",
        "kwmusic.exe",
        PlayerInfo {
            display_name: "酷我音乐",
            process_name: "kwmusic.exe",
        },
    ),
    (
        "kuwomusic",
        "kuwomusic.exe",
        PlayerInfo {
            display_name: "酷我音乐",
            process_name: "kuwomusic.exe",
        },
    ),
    (
        "foobar2000",
        "foobar2000.exe",
        PlayerInfo {
            display_name: "Foobar2000",
            process_name: "foobar2000.exe",
        },
    ),
    (
        "potplayer",
        "potplayermini64.exe",
        PlayerInfo {
            display_name: "PotPlayer",
            process_name: "potplayermini64.exe",
        },
    ),
    (
        "microsoft.zunemusic",
        "music.ui.exe",
        PlayerInfo {
            display_name: "媒体播放器",
            process_name: "music.ui.exe",
        },
    ),
    (
        "microsoft.zunevideo",
        "video.ui.exe",
        PlayerInfo {
            display_name: "电影和电视",
            process_name: "video.ui.exe",
        },
    ),
    (
        // Edge 的 SMTC AUMID 常常就是裸的 "MSEdge"，既不含 ".exe" 也不含 "microsoftedge"
        "msedge",
        "msedge.exe",
        PlayerInfo {
            display_name: "Edge",
            process_name: "msedge.exe",
        },
    ),
    (
        "microsoftedge",
        "msedge.exe",
        PlayerInfo {
            display_name: "Edge",
            process_name: "msedge.exe",
        },
    ),
    (
        "chrome",
        "chrome.exe",
        PlayerInfo {
            display_name: "Chrome",
            process_name: "chrome.exe",
        },
    ),
    (
        "firefox",
        "firefox.exe",
        PlayerInfo {
            display_name: "Firefox",
            process_name: "firefox.exe",
        },
    ),
    (
        "brave",
        "brave.exe",
        PlayerInfo {
            display_name: "Brave",
            process_name: "brave.exe",
        },
    ),
    (
        "vlc",
        "vlc.exe",
        PlayerInfo {
            display_name: "VLC",
            process_name: "vlc.exe",
        },
    ),
    (
        "aimp",
        "aimp.exe",
        PlayerInfo {
            display_name: "AIMP",
            process_name: "aimp.exe",
        },
    ),
    (
        "groove",
        "groove.exe",
        PlayerInfo {
            display_name: "Groove 音乐",
            process_name: "groove.exe",
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
