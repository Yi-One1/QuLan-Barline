//! 极简 i18n。词条走编译期 `match`，无分配、无 HashMap、无文件。
//!
//! 设置页的文案不在这里——它在内嵌 HTML 里自带字典（§14.6），
//! 避免 200+ 词条跨语言重复维护。

use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// 界面语言。`System` 只存在于配置层（见 `config::LangSerde`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    /// 从配置枚举映射。`System` → 探测系统语言。
    pub fn from_serde(v: crate::config::LangSerde) -> Self {
        match v {
            crate::config::LangSerde::Zh => Lang::Zh,
            crate::config::LangSerde::En => Lang::En,
            crate::config::LangSerde::System => detect_system_lang(),
        }
    }
}

/// 探测系统 UI 语言：`PRIMARYLANGID(LANG_CHINESE) == 0x04` → 中文，其余 → 英文。
pub fn detect_system_lang() -> Lang {
    // SAFETY: GetUserDefaultUILanguage 无参数、无副作用，返回当前用户 UI 的 LANGID。
    let langid = unsafe { GetUserDefaultUILanguage() };
    let primary = langid & 0x03FF;
    if primary == 0x04 {
        Lang::Zh
    } else {
        Lang::En
    }
}

/// 词条查表。未知 key 返回 key 本身（便于发现漏配，不会 panic）。
pub fn t(lang: Lang, key: &'static str) -> &'static str {
    match (lang, key) {
        // ---------------- 通用 ----------------
        (Lang::Zh, "app.name") => "曲栏",
        (Lang::En, "app.name") => "Barline",
        (Lang::Zh, "app.tagline") => "曲栏 Barline",
        (Lang::En, "app.tagline") => "Barline",

        // ---------------- 播放态 ----------------
        (Lang::Zh, "state.no_session") => "无播放",
        (Lang::En, "state.no_session") => "No playback",
        (Lang::Zh, "state.playing") => "正在播放",
        (Lang::En, "state.playing") => "Now playing",
        (Lang::Zh, "state.paused") => "已暂停",
        (Lang::En, "state.paused") => "Paused",
        (Lang::Zh, "state.fallback") => "正在播放（兜底）",
        (Lang::En, "state.fallback") => "Playing (fallback)",
        (Lang::Zh, "state.unknown_track") => "未知曲目",
        (Lang::En, "state.unknown_track") => "Unknown track",
        (Lang::Zh, "state.unknown_artist") => "未知艺术家",
        (Lang::En, "state.unknown_artist") => "Unknown artist",

        // ---------------- 歌词 ----------------
        (Lang::Zh, "lyric.loading") => "♪ 歌词加载中…",
        (Lang::En, "lyric.loading") => "♪ Loading lyrics…",
        (Lang::Zh, "lyric.missing") => "♪ 暂无歌词",
        (Lang::En, "lyric.missing") => "♪ No lyrics",

        // ---------------- 托盘 / 菜单 ----------------
        (Lang::Zh, "tray.tip") => "曲栏 Barline",
        (Lang::Zh, "settings.webview_missing_title") => "需要 WebView2 运行时",
        (Lang::En, "settings.webview_missing_title") => "WebView2 Runtime required",
        (Lang::Zh, "settings.webview_missing_body") => "设置页需要 Microsoft Edge WebView2 运行时，小组件本身不受影响。\n\n是否现在打开下载页？",
        (Lang::En, "settings.webview_missing_body") => {
            "The settings page needs the Microsoft Edge WebView2 Runtime. The widget itself keeps working.\n\nOpen the download page now?"
        }
        (Lang::Zh, "tray.conflict_title") => "曲栏",
        (Lang::En, "tray.conflict_title") => "Barline",
        (Lang::Zh, "tray.conflict_text") => "部分快捷键被其他程序占用，可在设置中改绑",
        (Lang::En, "tray.conflict_text") => {
            "Some hotkeys are taken by other apps. Rebind them in Settings."
        }
        (Lang::En, "tray.tip") => "Barline",
        (Lang::Zh, "menu.settings") => "显示设置",
        (Lang::En, "menu.settings") => "Settings",
        (Lang::Zh, "menu.toggle_widget") => "显示/隐藏小组件",
        (Lang::En, "menu.toggle_widget") => "Show/Hide widget",
        (Lang::Zh, "menu.quit") => "退出",
        (Lang::En, "menu.quit") => "Quit",

        // ---------------- 右键菜单（M2/T08） ----------------
        (Lang::Zh, "menu.toggle_lyrics") => "显示歌词",
        (Lang::En, "menu.toggle_lyrics") => "Show lyrics",
        (Lang::Zh, "menu.lyric_one_line") => "单行歌词",
        (Lang::En, "menu.lyric_one_line") => "One line",
        (Lang::Zh, "menu.lyric_two_line") => "双行歌词",
        (Lang::En, "menu.lyric_two_line") => "Two lines",
        (Lang::Zh, "menu.bg_blur") => "背景：模糊",
        (Lang::En, "menu.bg_blur") => "Background: Blur",
        (Lang::Zh, "menu.bg_translucent") => "背景：半透明",
        (Lang::En, "menu.bg_translucent") => "Background: Translucent",
        (Lang::Zh, "menu.shuffle") => "随机播放",
        (Lang::En, "menu.shuffle") => "Shuffle",
        (Lang::Zh, "menu.repeat") => "循环播放",
        (Lang::En, "menu.repeat") => "Repeat",
        (Lang::Zh, "menu.about") => "关于",
        (Lang::En, "menu.about") => "About",
        (Lang::Zh, "menu.separator") => "-",
        (Lang::En, "menu.separator") => "-",

        // ---------------- 播控 / HUD（M2） ----------------
        (Lang::Zh, "hud.volume") => "音量",
        (Lang::En, "hud.volume") => "Volume",
        (Lang::Zh, "state.no_cover") => "暂无封面",
        (Lang::En, "state.no_cover") => "No cover",
        (Lang::Zh, "btn.prev") => "上一首",
        (Lang::En, "btn.prev") => "Previous",
        (Lang::Zh, "btn.next") => "下一首",
        (Lang::En, "btn.next") => "Next",
        (Lang::Zh, "btn.play") => "播放",
        (Lang::En, "btn.play") => "Play",
        (Lang::Zh, "btn.pause") => "暂停",
        (Lang::En, "btn.pause") => "Pause",

        (Lang::Zh, "about.title") => "关于 曲栏 Barline",
        (Lang::En, "about.title") => "About Barline",
        (Lang::Zh, "about.text") => {
            "曲栏 Barline v0.0.1\n\
             Windows 11 任务栏媒体播控小组件\n\n\
             单文件便携版 · 无安装 · 无管理员权限\n\
             歌词数据来源：lrclib.net"
        }
        (Lang::En, "about.text") => {
            "Barline v0.0.1\n\
             Media control widget for the Windows 11 taskbar\n\n\
             Portable single exe · no install · no admin rights\n\
             Lyrics provided by lrclib.net"
        }

        // ---------------- 兜底 ----------------
        (_, _) => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_translated() {
        assert_eq!(t(Lang::Zh, "menu.quit"), "退出");
        assert_eq!(t(Lang::En, "menu.quit"), "Quit");
    }

    #[test]
    fn unknown_key_falls_back_to_key() {
        assert_eq!(t(Lang::Zh, "no.such.key"), "no.such.key");
    }
}
