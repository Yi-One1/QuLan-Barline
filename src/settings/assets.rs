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
}
