//! 字符串 ↔ 宽字符（UTF-16）工具。
//!
//! Win32 的 `*W` 系列 API 全部吃 `PCWSTR`（NUL 结尾 UTF-16）。Rust 侧统一用
//! `Vec<u16>` 中转，生命周期由调用方持有，避免悬垂指针。

/// `&str` → NUL 结尾的 UTF-16 序列。
///
/// 返回值**包含**结尾的 `0`，可直接喂给 `PCWSTR::from_raw(v.as_ptr())`。
pub fn to_wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// NUL 结尾的 UTF-16 序列 → `String`（读到第一个 `0` 为止）。
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `&str` → 不含 NUL 的 UTF-16 切片，供 `ID2D1RenderTarget::DrawText` 等
/// 「显式长度」API 使用。
pub fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ascii() {
        let v = to_wide("Barline");
        assert_eq!(v.last(), Some(&0));
        assert_eq!(from_wide(&v), "Barline");
    }

    #[test]
    fn roundtrip_chinese() {
        let s = "曲栏 Barline — 任务栏小组件";
        assert_eq!(from_wide(&to_wide(s)), s);
    }

    #[test]
    fn roundtrip_emoji() {
        // 代理对：1 个 char = 2 个 u16
        let s = "🎵🎶";
        let v = to_wide(s);
        assert_eq!(v.len(), 4 + 1);
        assert_eq!(from_wide(&v), s);
    }

    #[test]
    fn roundtrip_empty() {
        let v = to_wide("");
        assert_eq!(v, vec![0u16]);
        assert_eq!(from_wide(&v), "");
    }

    #[test]
    fn from_wide_stops_at_nul() {
        let v = to_wide("abc");
        let mut padded = v.clone();
        padded.extend_from_slice(&[0x4E2D, 0x6587, 0]); // 中文，在 NUL 之后
        assert_eq!(from_wide(&padded), "abc");
    }
}
