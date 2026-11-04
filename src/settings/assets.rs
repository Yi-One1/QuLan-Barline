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

    /// v1.9.0（G7）：七个分组（原六组 + `group-blacklist`）都必须存在。
    /// 原名 `html_has_all_six_groups` 随 G7 分区并入扩成七组（断言只增不减）。
    #[test]
    fn html_has_all_seven_groups() {
        for key in [
            "group-appearance",
            "group-lyrics",
            "group-volume",
            "group-blacklist",
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

    /// 契约回归（v1.2.3 / G4）：快捷键体系三件套必须在页面里落地——
    /// ①「录制」已改名「更改」；②每行有 ↺ 单行恢复默认按钮；③前端确实接了
    /// `hotkeys.reset.<action>` 这条配置路径。
    ///
    /// 改名那条是硬要求：`录制` / `Record` 一个都不许再出现（含 i18n 字典）。
    #[test]
    fn hotkey_ui_uses_change_and_per_row_reset() {
        // ① 单行恢复默认：图标 + i18n 键 + 配置路径
        assert!(INDEX_HTML.contains("↺"), "缺少单行恢复默认的 ↺ 图标");
        assert!(
            INDEX_HTML.contains("hotkeys.reset_one"),
            "缺少 hotkeys.reset_one 词条/引用"
        );
        assert!(
            INDEX_HTML.contains("hotkeys.reset."),
            "前端没有拼出 hotkeys.reset.<action> 配置路径"
        );
        assert!(
            INDEX_HTML.contains("btn icon"),
            "↺ 按钮应使用 class=\"btn icon\"（不影响 .btn 本体）"
        );

        // ② 「录制」改名「更改」：中英都不许残留
        for gone in ["\"hotkeys.record\": \"录制\"", "\"hotkeys.record\": \"Record\"", "「录制」"] {
            assert!(
                !INDEX_HTML.contains(gone),
                "「录制」未改名干净，仍存在：{gone}"
            );
        }
        assert!(
            INDEX_HTML.contains("\"hotkeys.record\": \"更改\""),
            "中文按钮文案应为「更改」"
        );
        assert!(
            INDEX_HTML.contains("\"hotkeys.record\": \"Change\""),
            "英文按钮文案应为 Change"
        );
    }

    /// 契约回归（v1.2.2 / G3）：「界面缩放」整项已删除，页面里不许再出现任何痕迹。
    ///
    /// `ui_scale` 曾经是**死配置**（渲染层与布局层从未读取），删除后若有人
    /// "顺手加回来"，控件、i18n 词条、`bind` 调用会一起复活并再次误导用户。
    /// 这里逐个关键字扫一遍，中英标签都查。
    #[test]
    fn ui_scale_widget_is_gone() {
        for needle in [
            "id=\"ui-scale\"",
            "appearance.ui_scale",
            "界面缩放",
            "UI scale",
            "appearance.scale",
        ] {
            assert!(
                !INDEX_HTML.contains(needle),
                "设置页仍残留已删除的「界面缩放」痕迹：{needle}"
            );
        }
        // 反向确认没有把整块外观设置删坏：相邻的两个 row 必须还在
        for keep in ["id=\"bg-mode\"", "id=\"bg-idle-mode\"", "id=\"monitor\""] {
            assert!(INDEX_HTML.contains(keep), "误删了外观设置里的 {keep}");
        }
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

    /// 契约回归（v1.8.0 / R2）：歌词磁盘缓存卡片的三条纪律。
    ///
    /// ① **JS 不拼路径**：页面里不许出现 `"lyrics-cache"` 这个**完整字符串字面量**
    ///    （元素 id 是 `lyrics-cache-dir` 之类，语义是 DOM 锚点，不是路径片段）；
    /// ② **动态占用文本不许带 `data-i18n`**：否则 `render()` 会把它打回
    ///    `{files} 个文件 · {size}` 模板串；
    /// ③ **零轮询**：统计只在用户进入歌词分区时那一次，页面里除 toast 之外
    ///    不许有任何超时 / 间隔定时器。
    #[test]
    fn lyrics_cache_card_follows_r2_contract() {
        assert!(
            !INDEX_HTML.contains("\"lyrics-cache\""),
            "页面里出现了 \"lyrics-cache\" 字面量：路径必须由 Rust 下发，JS 不拼"
        );
        assert!(
            INDEX_HTML.contains("id=\"lyrics-cache-dir\""),
            "缺缓存路径锚点"
        );
        assert!(
            INDEX_HTML.contains("id=\"lyrics-cache-usage\""),
            "缺缓存占用锚点"
        );
        // ② 两个锚点都必须是「无 data-i18n」的纯 JS 填充元素
        for anchor in ["lyrics-cache-dir", "lyrics-cache-usage"] {
            let at = INDEX_HTML
                .find(&format!("id=\"{anchor}\""))
                .unwrap_or_else(|| panic!("找不到 #{anchor}"));
            // 往前取到同一个标签的开始，检查这段属性里没有 data-i18n
            let tag_start = INDEX_HTML[..at].rfind('<').unwrap_or(0);
            let tag = &INDEX_HTML[tag_start..at];
            assert!(
                !tag.contains("data-i18n"),
                "#{anchor} 的标签带了 data-i18n，动态文本会被 render() 打回模板串：{tag}"
            );
        }
        // ③ 统计相关代码零定时器（`toast` 的自动隐藏与本特性无关）
        assert_eq!(
            INDEX_HTML.matches("setInterval").count(),
            0,
            "页面里不许有 setInterval"
        );
        assert_eq!(
            INDEX_HTML.matches("setTimeout").count(),
            1,
            "只允许 toast 的那一处 setTimeout"
        );
        // 统计动作名必须与 Rust 侧一致（`run_settings_action` 的分支字面量）
        assert!(
            INDEX_HTML.contains("\"lyricsCacheStats\""),
            "缺少 lyricsCacheStats 动作"
        );
        assert!(
            INDEX_HTML.contains("\"openLyricsCacheDir\""),
            "缺少 openLyricsCacheDir 动作"
        );
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

    // ------------------------------------------------ v1.9.0（G7）黑名单分区

    /// G7 契约一：黑名单分区的 7 个关键 id 必须存在，且配置路径三条都在页面里。
    #[test]
    fn blacklist_section_ids_and_paths_exist() {
        for id in [
            "blacklist-card",
            "blacklist-title",
            "blacklist-items",
            "blacklist-input",
            "blacklist-candidates",
            "blacklist-add",
            "blacklist-clear",
            "detected-card",
            "detected-at",
            "detected-refresh",
            "detected-items",
        ] {
            assert!(INDEX_HTML.contains(&format!("id=\"{id}\"")), "缺少 #{id}");
        }
        for path in [
            "media.blacklist_add",
            "media.blacklist_remove",
            "media.blacklist_clear",
        ] {
            assert!(INDEX_HTML.contains(path), "页面没有接配置路径 {path}");
        }
        // 新 action 字面量必须进断言（先例 lyricsCacheStats / openLyricsCacheDir）
        assert!(INDEX_HTML.contains("\"blacklistEnumerate\""), "缺少 blacklistEnumerate 动作");
    }

    /// G7 契约二：动态文本不得挂 data-i18n（#blacklist-title 计数会被
    /// render() 的 [data-i18n] 循环打回模板串——先例 #lyrics-cache-usage）；
    /// `nav.blacklist` 与 `blacklist.detected.unverified` 中英词条都在；
    /// `[data-i18n-placeholder]` 循环必须补齐（否则 placeholder 永远中文）。
    #[test]
    fn blacklist_dynamic_title_has_no_data_i18n_and_keys_are_bilingual() {
        // ① #blacklist-title 不带 data-i18n
        let at = INDEX_HTML.find("id=\"blacklist-title\"").expect("找不到 #blacklist-title");
        let tag_start = INDEX_HTML[..at].rfind('<').unwrap_or(0);
        let tag = &INDEX_HTML[tag_start..at];
        assert!(
            !tag.contains("data-i18n"),
            "#blacklist-title 带了 data-i18n，动态计数会被 render() 打回模板串：{tag}"
        );
        // ② 关键词条中英都在（I18N.zh 与 I18N.en 各一份；needle = key + 文案前缀）
        for (key, zh, en) in [
            ("nav.blacklist", "黑名单", "Blacklist"),
            ("blacklist.detected.unverified", "进程名未能验证", "Name unverified"),
            ("blacklist.hint.match", "只按进程名匹配", "Matched by process name only"),
            ("blacklist.clear.confirm", "清空后所有应用", "Clearing the list"),
        ] {
            let needle_zh = format!("\"{key}\": \"{zh}");
            assert!(INDEX_HTML.contains(&needle_zh), "zh 缺词条：{key}");
            let needle_en = format!("\"{key}\": \"{en}");
            assert!(INDEX_HTML.contains(&needle_en), "en 缺词条：{key}");
        }
        // hint.match 的末句必须采用 PRD P0-06 版（指路「检测到的应用」）
        assert!(
            INDEX_HTML.contains("从「检测到的应用」里加入最稳"),
            "blacklist.hint.match 缺 PRD 版末句"
        );
        // ③ placeholder 属性与 JS 循环补齐
        assert!(
            INDEX_HTML.contains("data-i18n-placeholder"),
            "缺少 data-i18n-placeholder（placeholder 永远中文）"
        );
        assert!(
            INDEX_HTML.contains("i18nPlaceholder"),
            "render() 缺 [data-i18n-placeholder] 循环（dataset 名是 i18nPlaceholder）"
        );
    }

    /// G7 契约三：枚举**只允许两处触发**（activateNav 守卫 + 刷新按钮）——
    /// 零定时器（setTimeout 仍只许 toast 那一处）、零轮询；
    /// 清空确认必须走 window.confirm()（裁决 A5，不用原生弹窗）；
    /// P0-03：零新增 CSS（仍只有一个 <style> 块）。
    #[test]
    fn blacklist_enumerate_has_two_triggers_and_no_timers() {
        // ① 触发点恰好两处：守卫 + 刷新按钮
        let guard = INDEX_HTML.matches("requestDetect()").count();
        assert_eq!(
            guard, 2,
            "requestDetect 调用点必须恰好 2 处（activateNav 守卫 + detected-refresh）：{guard}"
        );
        assert!(
            INDEX_HTML.contains("!detectedLoaded) requestDetect()"),
            "首次进入分区必须有 detectedLoaded 守卫"
        );
        // ② 零定时器：setTimeout 仍只许 toast 那一处（既有断言的 G7 侧重申）
        assert_eq!(
            INDEX_HTML.matches("setTimeout").count(),
            1,
            "枚举置灰的解除靠 mediaApps 消息回推，不许加 setTimeout"
        );
        assert_eq!(INDEX_HTML.matches("setInterval").count(), 0);
        // ③ 清空确认走 window.confirm()
        assert!(
            INDEX_HTML.contains("confirm(t(\"blacklist.clear.confirm\"))"),
            "清空黑名单必须用 window.confirm()"
        );
        // ④ P0-03：不新增 CSS——全页仍只有 1 个 <style> 块
        assert_eq!(INDEX_HTML.matches("<style").count(), 1, "不许新增 <style> 规则");
    }
}
