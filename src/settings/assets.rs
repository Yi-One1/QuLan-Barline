//! 设置页的内嵌资源。
//!
//! `include_str!` 在**编译期**把 HTML 塞进 exe——便携版是单文件，
//! 任何"运行时读 assets 目录"的做法都会变成交付缺陷。

/// 整页 HTML（内联 CSS + JS，零外部依赖）。
pub const INDEX_HTML: &str = include_str!("../../assets/ui/index.html");

/// 取整页 HTML。
pub fn index_html() -> String {
    INDEX_HTML.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_is_embedded_and_has_charset() {
        assert!(INDEX_HTML.len() > 10_000, "HTML 太小，可能没内嵌进来");
        assert!(INDEX_HTML.contains("<meta charset=\"utf-8\">"), "缺 charset 会中文乱码");
        assert!(INDEX_HTML.contains("chrome.webview"), "没有 WebView2 消息桥");
        // 体积目标 60–80KB（PRD 要求纯手写、零框架）
        assert!(INDEX_HTML.len() < 200_000, "HTML 过大：{}", INDEX_HTML.len());
    }

    #[test]
    fn html_has_all_six_groups() {
        for key in [
            "group-appearance",
            "group-lyrics",
            "group-volume",
            "group-hotkeys",
            "group-general",
            "group-about",
        ] {
            assert!(INDEX_HTML.contains(key), "缺少分组 {key}");
        }
    }

    /// 契约回归（v1.1.22）：「外观-悬停背景」下拉必须「透明」在前、「半透明」在后，
    /// 与「常规背景」(`#bg-idle-mode`) 的次序保持一致。
    ///
    /// 取值走 `option.value`，所以次序纯属观感——但正因为没人会因为功能报错而发现它，
    /// 才需要这条测试钉住，防止以后被顺手改回去。
    #[test]
    fn hover_bg_select_lists_transparent_first() {
        let start = INDEX_HTML
            .find("id=\"bg-mode\"")
            .expect("找不到 #bg-mode（悬停背景）下拉");
        let block = &INDEX_HTML[start..];
        let end = block.find("</select>").expect("找不到 #bg-mode 的 </select>");
        let options = &block[..end];

        let transparent = options
            .find("value=\"transparent\"")
            .expect("悬停背景缺 value=\"transparent\" 选项");
        let translucent = options
            .find("value=\"translucent\"")
            .expect("悬停背景缺 value=\"translucent\" 选项");
        assert!(
            transparent < translucent,
            "悬停背景下拉顺序反了：应在「半透明」之前放「透明」"
        );

        // 常规背景本来就是这个次序，一起钉住，避免只改一边改出不一致
        let idle_start = INDEX_HTML
            .find("id=\"bg-idle-mode\"")
            .expect("找不到 #bg-idle-mode（常规背景）下拉");
        let idle_block = &INDEX_HTML[idle_start..];
        let idle_end = idle_block
            .find("</select>")
            .expect("找不到 #bg-idle-mode 的 </select>");
        let idle_options = &idle_block[..idle_end];
        let idle_transparent = idle_options
            .find("value=\"transparent\"")
            .expect("常规背景缺 value=\"transparent\" 选项");
        let idle_translucent = idle_options
            .find("value=\"translucent\"")
            .expect("常规背景缺 value=\"translucent\" 选项");
        assert!(
            idle_transparent < idle_translucent,
            "常规背景下拉顺序反了：应在「半透明」之前放「透明」"
        );
    }

    /// 契约回归（v1.1.22）：「开机自启」已改名为「开机自启动」，设置页不得有残留。
    ///
    /// 注意「开机自启动」本身包含「开机自启」子串，所以逐处命中后要求后一个字符必须是
    /// 「动」，否则就是漏改的老文案（含 `general.sub` 那句副标题）。
    #[test]
    fn autostart_label_uses_full_name() {
        const SHORT: &str = "开机自启";

        assert!(
            INDEX_HTML.contains("开机自启动</div>"),
            "general.autostart 的标签文案应为「开机自启动」"
        );

        let mut cursor = 0usize;
        while let Some(pos) = INDEX_HTML[cursor..].find(SHORT) {
            let at = cursor + pos;
            let next = INDEX_HTML[at + SHORT.len()..].chars().next();
            assert_eq!(
                next,
                Some('动'),
                "发现未改名的「开机自启」（字节位置 {at}），应为「开机自启动」"
            );
            // 跳过整个命中片段：中文 3 字节/字，+1 会切在字符中间，`find` 的切片会 panic
            cursor = at + SHORT.len();
        }
    }

    /// 契约回归（v1.1.22）：「显示歌词」的描述必须与真实渲染行为一致。
    ///
    /// 关闭后走 `WidgetForm::Compact`——窗口高度恒为任务栏条带厚度，只是不画歌词行，
    /// 歌名/歌手/播控照常。旧文案「关闭后小组件收合到任务栏高度」是错的，
    /// 这里同时断言错误文案已清除、正确文案 zh 的静态标签与字典两处都已更新。
    #[test]
    fn lyrics_enabled_desc_matches_render_behavior() {
        assert!(
            !INDEX_HTML.contains("收合到任务栏高度"),
            "「显示歌词」描述仍写着「收合到任务栏高度」，与 WidgetForm::Compact 行为不符"
        );
        assert!(
            !INDEX_HTML.contains("collapses the widget to the taskbar height"),
            "英文描述仍在说收合高度"
        );
        assert_eq!(
            INDEX_HTML.matches("不再显示歌词行").count(),
            2,
            "zh 的静态标签与 i18n 字典两处都要改成「不再显示歌词行」"
        );
    }
}
