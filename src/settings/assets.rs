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

    /// 取 `id="xxx"` 所在的**整个开始标签**（`<` 到 `>`），用于检查属性。
    ///
    /// 坑：只切到 `id=` 之前是常见写法，但 `data-i18n` 几乎总写在 id 后面——
    /// 那种写法无论怎么加属性都不会红，等于没有断言（v1.12.0 变异测试抓到）。
    fn tag_of(html: &str, anchor: &str) -> String {
        let at = html
            .find(&format!("id=\"{anchor}\""))
            .unwrap_or_else(|| panic!("找不到 #{anchor}"));
        let start = html[..at].rfind('<').unwrap_or(0);
        let rest = &html[start..];
        let end = rest.find('>').map(|e| e + 1).unwrap_or(rest.len());
        rest[..end].to_string()
    }

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
            let tag = tag_of(INDEX_HTML, anchor);
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

    /// **v1.10.0 / G8-③**：设置页「通用」页的双击开关。
    ///
    /// 四条一次锁死：
    /// ① 控件是**开关**（`class="switch"` + `type="checkbox"`），不是按钮——
    ///    用户裁决 R2 明确要求与「开机自启动 / 显示小组件」同款；
    /// ② 元素 id、配置路径、`render()` 回填三处**逐字符一致**（写错一处就是
    ///    "拨了不生效"或"重开设置又跳回去"，页面上看不出来）；
    /// ③ 中英词条都在（`data-i18n` 的两个键）；
    /// ④ 这一行**不许带任何定时器**（页面唯一的 `setTimeout` 是 toast）。
    #[test]
    fn double_click_switch_row_matches_existing_switches() {
        let at = INDEX_HTML
            .find("id=\"double-click-settings\"")
            .expect("设置页缺少 #double-click-settings");
        // 往上取这一行的起点、往下取到下一行（不依赖换行符，CRLF 也不受影响）
        let row_start = INDEX_HTML[..at].rfind("<div class=\"row\">").unwrap_or(0);
        let row_end = INDEX_HTML[at..]
            .find("<div class=\"row\">")
            .map(|o| at + o)
            .unwrap_or(INDEX_HTML.len());
        let row = &INDEX_HTML[row_start..row_end];

        // ① 与既有三个开关同款样式
        assert!(
            row.contains("class=\"switch\"") && row.contains("type=\"checkbox\""),
            "双击开关必须复用既有 switch 控件样式（用户裁决 R2）"
        );
        assert!(
            row.contains("class=\"desc\""),
            "开关必须带一行说明（现有三个开关都有）"
        );

        // ② 路径 / 回填 / 绑定三处一致
        for needle in [
            "general.double_click_open_settings",
            "c.general.double_click_open_settings",
        ] {
            assert!(INDEX_HTML.contains(needle), "页面缺少 {needle}");
        }
        assert!(
            INDEX_HTML.contains("bind(\"double-click-settings\", \"general.double_click_open_settings\", \"bool\")"),
            "开关必须用 bind(..., \"bool\") 绑定，否则发出去的是字符串"
        );

        // ③ 中英词条
        assert!(INDEX_HTML.contains("\"general.dblclick\""), "缺中文标签词条");
        assert!(
            INDEX_HTML.contains("\"general.dblclick.desc\""),
            "缺中文说明词条"
        );
        assert!(
            INDEX_HTML.contains("Double-click to open settings"),
            "缺英文标签词条"
        );

        // ④ 无定时器
        assert!(
            !row.contains("setTimeout") && !row.contains("setInterval"),
            "这一行不得引入定时器（页面唯一的 setTimeout 属于 toast）"
        );
    }

    /// G7 契约一：黑名单分区的 7 个关键 id 必须存在，且配置路径三条都在页面里。
    #[test]
    fn blacklist_section_ids_and_paths_exist() {
        // v1.13.0：两张名单卡已合并进切换控件（名单相关的 id 由
        // `filter_mode_switch_ids_paths_and_keys_exist` 负责），这里只剩
        // 枚举候选的 datalist 与「检测到的应用」那张卡。
        for id in [
            "blacklist-candidates",
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
        // ① #filter-title 不带 data-i18n（v1.13.0：它是两份名单共用的动态标题）
        let tag = tag_of(INDEX_HTML, "filter-title");
        assert!(
            !tag.contains("data-i18n"),
            "#filter-title 带了 data-i18n，动态计数会被 render() 打回模板串：{tag}"
        );
        // ② 关键词条中英都在（I18N.zh 与 I18N.en 各一份；needle = key + 文案前缀）
        for (key, zh, en) in [
            ("nav.blacklist", "媒体筛选", "Media filter"),
            ("blacklist.detected.unverified", "进程名未能验证", "Process name unverified"),
            ("filter.hint.match", "只按进程名匹配", "Matched by process name only"),
            ("filter.clear.confirm", "清空当前这份名单", "Clear this list"),
        ] {
            let needle_zh = format!("\"{key}\": \"{zh}");
            assert!(INDEX_HTML.contains(&needle_zh), "zh 缺词条：{key}");
            let needle_en = format!("\"{key}\": \"{en}");
            assert!(INDEX_HTML.contains(&needle_en), "en 缺词条：{key}");
        }
        // hint.match 的末句必须采用 PRD P0-06 版（指路「检测到的应用」）
        assert!(
            INDEX_HTML.contains("从「检测到的应用」里加入最稳"),
            "filter.hint.match 缺 PRD 版末句"
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

    /// **v1.13.0 切换控件契约**：锚点 / 模式取值 / 两份名单的路径 / 中英词条，
    /// 以及**「不同时出现」的机械保证**（旧的两张卡与两套控件必须已被删除）。
    #[test]
    fn filter_mode_switch_ids_paths_and_keys_exist() {
        for id in [
            "filter-mode-card",
            "filter-mode-blacklist",
            "filter-mode-whitelist",
            "filter-mode-desc",
            "filter-title",
            "filter-items",
            "filter-input",
            "filter-add",
            "filter-clear",
        ] {
            assert!(INDEX_HTML.contains(&format!("id=\"{id}\"")), "缺少 #{id}");
        }
        // 切换按钮必须真的发 media.mode（值要与 Rust `FilterMode` 的 snake_case 一致）
        for v in ["\"media.mode\", \"blacklist\"", "\"media.mode\", \"whitelist\""] {
            assert!(INDEX_HTML.contains(v), "切换按钮没有发 {v}");
        }
        for path in [
            "media.blacklist_add",
            "media.blacklist_remove",
            "media.blacklist_clear",
            "media.whitelist_add",
            "media.whitelist_remove",
            "media.whitelist_clear",
        ] {
            assert!(INDEX_HTML.contains(path), "页面没有接配置路径 {path}");
        }

        // **不同时出现**（v1.13.0 的核心）：两张名单卡与两套控件必须已被合并掉。
        // 只断言"新元素在"是不够的——旧卡留着也不会让上面任何一条变红，
        // 而用户会同时看到两份列表，正是这次要消灭的形态。
        for gone in [
            "id=\"whitelist-card\"",
            "id=\"blacklist-card\"",
            "id=\"whitelist-items\"",
            "id=\"blacklist-items\"",
            "id=\"whitelist-input\"",
            "id=\"blacklist-input\"",
            "id=\"whitelist-add\"",
            "id=\"blacklist-add\"",
            "id=\"whitelist-clear\"",
            "id=\"blacklist-clear\"",
            "function renderWhitelist",
            "function renderBlacklist",
        ] {
            assert!(
                !INDEX_HTML.contains(gone),
                "旧的两张卡 / 两套控件应已合并进切换控件，仍残留：{gone}"
            );
        }

        // 动态文本不许带 data-i18n（会被 render() 的 [data-i18n] 循环打回模板串）
        for id in ["filter-title", "filter-mode-desc"] {
            let tag = tag_of(INDEX_HTML, id);
            assert!(
                !tag.contains("data-i18n"),
                "#{id} 带了 data-i18n，动态文本会被 render() 打回模板串：{tag}"
            );
        }

        // 中英词条各一份
        for (key, zh, en) in [
            ("filter.sub", "一次只用一种筛选方式", "One filter at a time"),
            ("filter.mode", "筛选方式", "Filter mode"),
            ("filter.mode.blacklist", "黑名单", "Blacklist"),
            ("filter.mode.whitelist", "白名单", "Whitelist"),
            ("filter.mode.desc.blacklist", "黑名单里的应用不被接管", "Apps on the blacklist are ignored"),
            ("filter.mode.desc.whitelist", "只接管白名单里的应用", "Only apps on the whitelist are picked up"),
            ("filter.add", "添加", "Add"),
            ("filter.clear", "清空", "Clear"),
            ("filter.clear.confirm", "清空当前这份名单", "Clear this list"),
            ("filter.empty.blacklist", "黑名单为空", "The blacklist is empty"),
            ("filter.empty.whitelist", "白名单为空", "The whitelist is empty"),
            ("filter.detected.toWhitelist", "加入白名单", "Add to whitelist"),
            ("filter.detected.toBlacklist", "加入黑名单", "Add to blacklist"),
            ("filter.detected.inList", "已在名单内", "Already on the list"),
            ("filter.detected.already.tip", "已经在当前名单里了", "Already on the current list"),
        ] {
            assert!(INDEX_HTML.contains(&format!("\"{key}\": \"{zh}")), "zh 缺词条：{key}");
            assert!(INDEX_HTML.contains(&format!("\"{key}\": \"{en}")), "en 缺词条：{key}");
        }
    }

    /// **v1.13.0 接线锁**：检测行的按钮必须跟着**当前模式**走。
    ///
    /// 退回「两个按钮 / 只认黑名单」时，白名单模式下用户看到的按钮仍是
    /// 「加入黑名单」——点下去改的是此刻不生效的那张表，症状是「点了没反应」，
    /// 而所有 Rust 侧测试照样全绿（它们只测后端判定）。
    #[test]
    fn detected_rows_follow_the_current_filter_mode() {
        let start = INDEX_HTML
            .find("function renderDetected()")
            .expect("找不到 renderDetected");
        // 按**字符**截（不能按字节切：HTML 是中文，切点会落在多字节字符中间 panic）
        let body: String = INDEX_HTML[start..].chars().take(4000).collect();
        assert!(
            body.contains("const white = currentMode() === \"whitelist\""),
            "检测行必须读当前筛选方式"
        );
        // 动作路径按模式二选一（不是写死黑名单）
        assert!(
            body.contains("media.whitelist_add") && body.contains("media.blacklist_add"),
            "检测行的加入路径必须覆盖两张表"
        );
        // 只给一个按钮：两个按钮等于暗示另一份名单也在生效
        assert!(
            !body.contains("filter.detected.toBlacklist\")]"),
            "检测行不得同时渲染两个动作按钮"
        );
        // 已在名单内 → 置灰并给提示，不再提供"换侧"路径（两张表允许重叠）
        assert!(
            body.contains("filter.detected.inList"),
            "已在名单内的行必须标出来"
        );
    }

    /// **v1.13.0 接线锁**：列表渲染必须按模式取配置，且只有**一份**实现。
    ///
    /// 两份 render 函数并存是这次要消灭的重复；只留一份却不看 `currentMode()`
    /// 更糟——切模式后列表纹丝不动，而标题/高亮都变了。
    #[test]
    fn filter_list_renders_the_active_mode_only() {
        assert!(
            INDEX_HTML.contains("function currentMode()"),
            "缺少 currentMode()——模式必须从 media.mode 单点读取"
        );
        // **必须切片到 renderFilterList 函数体**：`renderDetected` 里也写着同一句，
        // 全文 contains 会被它满足——本轮变异测试实测过（只改列表取值仍然全绿）。
        let fstart = INDEX_HTML
            .find("function renderFilterList()")
            .expect("找不到 renderFilterList");
        let body: String = INDEX_HTML[fstart..].chars().take(2500).collect();
        assert!(
            body.contains("(white ? media.whitelist : media.blacklist)"),
            "列表必须按模式二选一取配置"
        );
        assert!(
            body.contains("function renderFilterList"),
            "切片起点必须真的落在函数上"
        );
        // 切换控件的高亮同样由模式推导
        assert!(
            INDEX_HTML.contains("bl.className = white ? \"btn\" : \"btn primary\""),
            "切换控件必须把当前项高亮（复用 .btn.primary，零新增 CSS 类）"
        );
        // 添加/清空按模式拼路径
        assert!(
            INDEX_HTML.contains(
                "currentMode() === \"whitelist\" ? \"media.whitelist_add\" : \"media.blacklist_add\""
            ),
            "添加按钮必须按当前模式拼路径"
        );
        assert!(
            INDEX_HTML.contains(
                "currentMode() === \"whitelist\" ? \"media.whitelist_clear\" : \"media.blacklist_clear\""
            ),
            "清空按钮必须按当前模式拼路径"
        );
    }

    /// 收集 `prefix` 之后、下一个 `"` 之前的值（只收标识符形态，跳过动态参数）。
    fn attr_values(html: &str, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find(prefix) {
            let after = &rest[i + prefix.len()..];
            let Some(end) = after.find('"') else { break };
            let v = &after[..end];
            let ok = !v.is_empty()
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if ok {
                out.push(v.to_string());
            }
            rest = &after[end..];
        }
        out
    }

    /// **v1.13.0**：JS 里每个 `getElementById("字面量")` 都必须在 HTML 里有对应 id。
    ///
    /// 这次改版把 `whitelist-input` / `blacklist-add` 等一批 id 改名成 `filter-*`，
    /// 漏改一处就是 `null.addEventListener` —— 整段脚本从那一行起全部失效，
    /// 而所有 Rust 测试照样全绿（它们只查"新 id 在不在"）。这条是补那个死角。
    #[test]
    fn every_get_element_by_id_literal_exists_in_dom() {
        let used = attr_values(INDEX_HTML, "getElementById(\"");
        assert!(!used.is_empty(), "没扫到任何 getElementById，前缀写错了？");
        let defined: Vec<String> = attr_values(INDEX_HTML, "id=\"");
        for id in &used {
            assert!(
                defined.iter().any(|d| d == id),
                "JS 引用了不存在的 id：#{id}（改名时漏改了 JS）"
            );
        }
    }

    /// **v1.13.0**：每个 `data-i18n` / `data-i18n-placeholder` 的键都必须在字典里，
    /// 且中文块与英文块各有一份。缺一侧的后果是界面直接显示裸键名。
    #[test]
    fn every_i18n_attribute_has_both_languages() {
        let mut keys = attr_values(INDEX_HTML, "data-i18n=\"");
        keys.extend(attr_values(INDEX_HTML, "data-i18n-placeholder=\""));
        assert!(!keys.is_empty(), "没扫到 data-i18n");
        for key in keys {
            let zh = format!("\"{key}\": ");
            let hits = INDEX_HTML.matches(&zh).count();
            assert!(hits >= 2, "词条 {key} 只有 {hits} 份，中英必须各一份");
        }
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
            INDEX_HTML.contains("confirm(t(\"filter.clear.confirm\"))"),
            "清空黑名单必须用 window.confirm()"
        );
        // ④ P0-03：不新增 CSS——全页仍只有 1 个 <style> 块
        assert_eq!(INDEX_HTML.matches("<style").count(), 1, "不许新增 <style> 规则");
    }
}
