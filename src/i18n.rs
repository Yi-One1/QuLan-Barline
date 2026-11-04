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
///
/// v1.8.1（G6）：删掉 12 条**零调用点**的词条（`app.name` / `app.tagline` /
/// `btn.play` / `btn.pause` / `btn.prev` / `btn.next` / `menu.separator` /
/// `state.no_session` / `state.playing` / `state.paused` / `state.fallback` /
/// `state.no_cover`）。它们只有 zh/en 两条定义、没有任何 `t()` 调用点；
/// 实际活着的只有 `state.unknown_track` / `state.unknown_artist`。
pub fn t(lang: Lang, key: &'static str) -> &'static str {
    match (lang, key) {
        // ---------------- 通用 ----------------
        // ---------------- 播放态 ----------------
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
        // v1.9.0（G7）：托盘「拉黑/恢复当前应用」两态 + 未知应用名
        (Lang::Zh, "menu.blacklist_current") => "拉黑当前应用",
        (Lang::En, "menu.blacklist_current") => "Ignore this app",
        (Lang::Zh, "menu.unblacklist_current") => "恢复当前应用",
        (Lang::En, "menu.unblacklist_current") => "Stop ignoring this app",
        (Lang::Zh, "menu.unknown_app") => "未知应用",
        (Lang::En, "menu.unknown_app") => "Unknown app",
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
        // v1.1.22：托盘菜单与小组件右键菜单共用这一条，中文去掉「显示」二字
        (Lang::Zh, "menu.settings") => "设置",
        (Lang::En, "menu.settings") => "Settings",
        (Lang::Zh, "menu.toggle_widget") => "显示/隐藏小组件",
        (Lang::En, "menu.toggle_widget") => "Show/Hide widget",
        (Lang::Zh, "menu.quit") => "退出",
        (Lang::En, "menu.quit") => "Quit",

        // ---------------- 右键菜单（M2/T08） ----------------
        (Lang::Zh, "menu.toggle_lyrics") => "显示歌词",
        (Lang::En, "menu.toggle_lyrics") => "Show lyrics",
        // v1.2.1（G2 问题 6）：背景从 2 项扩成 4 项——常规背景 / 悬停背景各有
        // 透明 / 半透明两档，两组各自独立勾选。术语与设置页两个下拉保持一致
        //（`idle` = 常规背景，`hover` = 悬停背景）。
        (Lang::Zh, "menu.bg.idle.transparent") => "常规背景：透明",
        (Lang::En, "menu.bg.idle.transparent") => "Idle background: Transparent",
        (Lang::Zh, "menu.bg.idle.translucent") => "常规背景：半透明",
        (Lang::En, "menu.bg.idle.translucent") => "Idle background: Translucent",
        (Lang::Zh, "menu.bg.hover.transparent") => "悬停背景：透明",
        (Lang::En, "menu.bg.hover.transparent") => "Hover background: Transparent",
        (Lang::Zh, "menu.bg.hover.translucent") => "悬停背景：半透明",
        (Lang::En, "menu.bg.hover.translucent") => "Hover background: Translucent",
        // v1.1.16：menu.shuffle / menu.repeat 已随菜单项删除（播放器普遍拒绝
        // SMTC 远程循环/随机，功能形同虚设）
        (Lang::Zh, "menu.about") => "关于",
        (Lang::En, "menu.about") => "About",

        // ---------------- 播控 / HUD（M2） ----------------
        (Lang::Zh, "hud.volume") => "音量",
        (Lang::En, "hud.volume") => "Volume",

        (Lang::Zh, "about.title") => "关于 曲栏 Barline",
        (Lang::En, "about.title") => "About Barline",
        // 版本号必须从 `Cargo.toml` 编译期取（v1.1.22 修：原来写死 v0.0.1，
        // 与设置页用 `env!("CARGO_PKG_VERSION")` 显示的版本对不上）。
        // `concat!` 产出 `&'static str`，与本函数返回类型一致；不要改成运行时 `format!`。
        (Lang::Zh, "about.text") => concat!(
            "曲栏 Barline v",
            env!("CARGO_PKG_VERSION"),
            "\nWindows 11 任务栏媒体播控小组件\n\n",
            "单文件便携版 · 无安装 · 无管理员权限\n",
            "歌词数据来源：lrclib.net、网易云音乐"
        ),
        (Lang::En, "about.text") => concat!(
            "Barline v",
            env!("CARGO_PKG_VERSION"),
            "\nMedia control widget for the Windows 11 taskbar\n\n",
            "Portable single exe · no install · no admin rights\n",
            "Lyrics provided by lrclib.net and NetEase Cloud Music"
        ),

        // ---------------- 黑名单 toast（v1.9.0 / G7，ARCH-G7 §8.2）----------------
        // 设置页的静态标签在 index.html 自带字典里（blacklist.*）；这里只管
        // Rust 发的 toast 与托盘菜单文案。`blacklist.hint.match` 的末句口径
        // 采用 PRD P0-06 版（「从「检测到的应用」里加入最稳」）。
        (Lang::Zh, "toast.blacklist.added") => "已加入黑名单",
        (Lang::En, "toast.blacklist.added") => "Added to the blacklist",
        (Lang::Zh, "toast.blacklist.dup") => "已在黑名单中",
        (Lang::En, "toast.blacklist.dup") => "Already in the blacklist",
        (Lang::Zh, "toast.blacklist.empty") => "请输入进程名",
        (Lang::En, "toast.blacklist.empty") => "Enter a process name",
        (Lang::Zh, "toast.blacklist.invalid") => "进程名不支持通配符或路径之外的特殊字符",
        (Lang::En, "toast.blacklist.invalid") => "Process name cannot contain wildcards or special characters",
        (Lang::Zh, "toast.blacklist.too_long") => "进程名过长（最多 260 字符）",
        (Lang::En, "toast.blacklist.too_long") => "Process name too long (max 260)",
        (Lang::Zh, "toast.blacklist.full") => "黑名单已满（上限 200 条）",
        (Lang::En, "toast.blacklist.full") => "The blacklist is full (max 200)",
        (Lang::Zh, "toast.blacklist.removed") => "已从黑名单移除",
        (Lang::En, "toast.blacklist.removed") => "Removed from the blacklist",
        (Lang::Zh, "toast.blacklist.cleared") => "黑名单已清空",
        (Lang::En, "toast.blacklist.cleared") => "Blacklist cleared",
        (Lang::Zh, "toast.blacklist.unverified") => "已加入。这个进程名还没在这台机器上出现过；如果之后没生效，请到「检测到的应用」里点「加入」。",
        (Lang::En, "toast.blacklist.unverified") => "Added. This process name hasn't been seen on this machine yet — if it doesn't take effect, use Add in \"Detected apps\" instead.",
        (Lang::Zh, "toast.blacklist.enum_failed") => "未检测到正在播放的应用",
        (Lang::En, "toast.blacklist.enum_failed") => "No playing apps detected",

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

    /// 契约回归（v1.9.0 / G7）：黑名单全部 13 个词条（托盘 3 + toast 10）必须
    /// 中英双语都存在且非空。`t()` 未知 key 原样返回（`i18n.rs` 兜底）——
    /// **没有这条测试就发现不了漏翻**，界面上会直接显示裸键名。
    #[test]
    fn blacklist_keys_are_bilingual_and_non_empty() {
        for key in [
            "menu.blacklist_current",
            "menu.unblacklist_current",
            "menu.unknown_app",
            "toast.blacklist.added",
            "toast.blacklist.dup",
            "toast.blacklist.empty",
            "toast.blacklist.invalid",
            "toast.blacklist.too_long",
            "toast.blacklist.full",
            "toast.blacklist.removed",
            "toast.blacklist.cleared",
            "toast.blacklist.unverified",
            "toast.blacklist.enum_failed",
        ] {
            for lang in [Lang::Zh, Lang::En] {
                assert_ne!(t(lang, key), key, "{lang:?} 缺词条 {key}");
                assert!(!t(lang, key).is_empty(), "{lang:?} 词条为空：{key}");
            }
        }
        // 旧命名空间不得残留（黑名单 toast 一律走 i18n.rs，不在 app.rs 写中英 match）
        assert_eq!(t(Lang::Zh, "blacklist.invalid"), "blacklist.invalid");
    }

    /// 契约回归（v1.1.22）：托盘菜单与右键菜单共用的「设置」项，中文只留两字。
    /// 改回「显示设置」会让两个菜单一起变形，必须被这条测试拦住。
    #[test]
    fn menu_settings_label_is_short() {
        assert_eq!(t(Lang::Zh, "menu.settings"), "设置");
        assert_eq!(t(Lang::En, "menu.settings"), "Settings");
    }

    /// 契约回归（v1.2.1）：4 个背景菜单项的词条必须存在、非空，且中文术语要与
    /// 设置页一致（「常规背景」/「悬停背景」）——防止以后又被改回「背景：透明」
    /// 这种分不清是哪一组的写法。
    #[test]
    fn menu_background_labels_are_split_by_slot() {
        for (key, zh_head, en_head) in [
            ("menu.bg.idle.transparent", "常规背景", "Idle background"),
            ("menu.bg.idle.translucent", "常规背景", "Idle background"),
            ("menu.bg.hover.transparent", "悬停背景", "Hover background"),
            ("menu.bg.hover.translucent", "悬停背景", "Hover background"),
        ] {
            let zh = t(Lang::Zh, key);
            let en = t(Lang::En, key);
            assert_ne!(zh, key, "中文词条缺失：{key}");
            assert!(!zh.is_empty(), "中文词条为空：{key}");
            assert!(zh.starts_with(zh_head), "{key} 的中文应以「{zh_head}」开头，实际「{zh}」");
            assert!(en.starts_with(en_head), "{key} 的英文应以「{en_head}」开头，实际「{en}」");
        }
        // 旧的合并写法必须已删除（否则菜单会出现两套并存的说法）
        assert_eq!(t(Lang::Zh, "menu.bg_translucent"), "menu.bg_translucent");
        assert_eq!(t(Lang::Zh, "menu.bg_transparent"), "menu.bg_transparent");
    }

    /// 契约回归（v1.1.22）：「关于」对话框的版本号必须来自 `Cargo.toml`。
    ///
    /// `show_about()` 直接把 `about.text` 塞进 `MessageBoxW`，而设置页用的是
    /// `env!("CARGO_PKG_VERSION")`——词条里一旦写死版本号，两处就会打架
    /// （历史上就写死过 `v0.0.1`）。这条测试同时钉住「用的是编译期版本」和
    /// 「没有残留写死的字面量」两个方向。
    #[test]
    fn about_text_uses_compile_time_version() {
        const VERSION: &str = env!("CARGO_PKG_VERSION");
        let zh = t(Lang::Zh, "about.text");
        let en = t(Lang::En, "about.text");

        assert!(
            zh.contains(VERSION),
            "中文关于文案没带编译期版本号 {VERSION}：{zh}"
        );
        assert!(
            en.contains(VERSION),
            "英文关于文案没带编译期版本号 {VERSION}：{en}"
        );
        assert!(!zh.contains("v0.0.1"), "中文关于文案仍写死旧版本号：{zh}");
        assert!(!en.contains("v0.0.1"), "英文关于文案仍写死旧版本号：{en}");
        // 顺手钉住前缀，避免以后有人把版本号挪到句尾导致「曲栏 Barline」后面空一截
        assert!(
            zh.starts_with(&format!("曲栏 Barline v{VERSION}")),
            "中文关于文案首行应为「曲栏 Barline v{VERSION}」：{zh}"
        );
        assert!(
            en.starts_with(&format!("Barline v{VERSION}")),
            "英文关于文案首行应为「Barline v{VERSION}」：{en}"
        );
    }

    /// 契约回归（v1.1.22 / **v1.3.0 网易云兜底接入前铺路**）：歌词来源文案必须同时
    /// 提到 lrclib.net 与网易云，否则接入网易云兜底时还得再回来改一次文案。
    ///
    /// （原文写的是「为 G6 铺路」——那指的是 v1.3.0 的那轮 G6，与 v1.8.1 的
    /// 「G6 清理专项」是两回事，容易误读，故改写字面口径。）
    #[test]
    fn about_text_credits_both_lyric_sources() {
        let zh = t(Lang::Zh, "about.text");
        let en = t(Lang::En, "about.text");
        assert!(
            zh.contains("lrclib.net") && zh.contains("网易云音乐"),
            "中文歌词来源应同时写 lrclib.net 与网易云音乐：{zh}"
        );
        assert!(
            en.contains("lrclib.net") && en.contains("NetEase Cloud Music"),
            "英文歌词来源应同时写 lrclib.net 与 NetEase Cloud Music：{en}"
        );
    }
}
