//! 最小 WinHTTP 客户端（`ARCHITECTURE.md` §12.4 / T11 第 2 条）。
//!
//! **为什么不用 reqwest / ureq**：它们各自会拖进 5–15 个 crate（TLS、异步运行时、
//! 压缩……），而本项目只需要「GET 一个 HTTPS JSON」，WinHTTP 是系统自带、零依赖、
//! 且和 Windows 的代理设置天然一致（企业环境里这点很重要）。
//!
//! 句柄用 [`Handle`] 做 RAII：`WinHttpCloseHandle` 在 `Drop` 里调用，
//! 任何 `?` 提前返回都不会泄漏（WinHTTP 句柄泄漏会一直占着 socket）。

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use crate::platform::wide::to_wide;

/// 一次 GET 的四段超时（毫秒）。
///
/// **必须分开设**：`WinHttpSetTimeouts` 的 resolve / connect / send / receive 是四个
/// 独立的预算，用一个值卡死会出现「域名解析慢一点就整条链路失败」。
///
/// 实测 lrclib.net 从本机访问：TCP 连上很快，但**响应体要 6.6 秒才回来**
/// （`GET /api/get` 实测 6.64s，`/api/search` 12.0s）。原来 receive 只给 5s，
/// 等于每次都自己掐断，表现成"网络失败"——这是本轮修的 P1 缺陷。
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub resolve: i32,
    pub connect: i32,
    pub send: i32,
    pub receive: i32,
}

/// `/api/get`（单曲精确查询）：实测 6.6s，给到 12s。
pub const TIMEOUT_GET: Timeouts = Timeouts {
    resolve: 5_000,
    connect: 5_000,
    send: 5_000,
    receive: 12_000,
};

/// `/api/search`（模糊搜索）：实测 12s，给到 15s。
pub const TIMEOUT_SEARCH: Timeouts = Timeouts {
    resolve: 5_000,
    connect: 5_000,
    send: 5_000,
    receive: 15_000,
};
/// 单次响应体上限：歌词 JSON 最大也就几十 KB，1 MB 是安全阀。
const MAX_BODY: usize = 1024 * 1024;
/// 一次请求的**总**预算（毫秒）。
///
/// `WinHttpSetTimeouts` 的 receive 是「单次接收操作」的超时，不是总时长：
/// 实测 lrclib 一个 404 也能拖 22.4s（数据一点点滴出来，每次都小于 12s）。
/// 所以这里再加一道总闸，超过就主动放弃——否则歌词线程会被一个请求卡住半分钟。
const TOTAL_BUDGET_MS: u128 = 25_000;

/// 网络错误分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// 连不上 / 超时 / 系统错误
    Transport(String),
    /// 响应体过大或 URL 非法
    BadRequest(String),
}

impl NetError {
    pub fn as_str(&self) -> &str {
        match self {
            NetError::Transport(s) | NetError::BadRequest(s) => s,
        }
    }
}

/// 一次 HTTP 响应。
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u32,
    pub body: String,
    /// 响应体字节数（诊断 / 日志用）
    pub bytes: usize,
}

/// WinHTTP 句柄的 RAII 包装。
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 句柄由 WinHttpOpen/Connect/OpenRequest 返回且由本结构体独占，
            // Drop 只执行一次；WinHttpCloseHandle 对合法句柄安全。
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// 发一个 GET 请求（用 `/api/get` 的超时）。
pub fn get(url: &str, user_agent: &str) -> Result<Response, NetError> {
    get_with(url, user_agent, TIMEOUT_GET)
}

/// 发一个 GET 请求。`url` 必须是 `https://host/path?query` 形式。
pub fn get_with(url: &str, user_agent: &str, t: Timeouts) -> Result<Response, NetError> {
    let (host, port, path, secure) = split_url(url)?;

    let agent = to_wide(user_agent);
    // SAFETY: 三个 PCWSTR 都由本地 Vec<u16> 持有且以 NUL 结尾（to_wide 保证）；
    // 返回的会话句柄立即被 Handle 接管。
    let session = Handle(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err(NetError::Transport("WinHttpOpen 失败".into()));
    }
    // SAFETY: session 是刚创建的有效会话句柄；四个超时值都是正的有限毫秒数。
    unsafe {
        let _ = WinHttpSetTimeouts(session.0, t.resolve, t.connect, t.send, t.receive);
    }

    let host_w = to_wide(&host);
    // SAFETY: host_w 以 NUL 结尾；session 有效。
    let connect = Handle(unsafe { WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), port, 0) });
    if connect.0.is_null() {
        return Err(NetError::Transport(format!("连接 {host} 失败")));
    }

    let path_w = to_wide(&path);
    let verb = to_wide("GET");
    let flags = if secure { WINHTTP_FLAG_SECURE } else { Default::default() };
    // SAFETY: 所有 PCWSTR 参数都指向本地 NUL 结尾缓冲；connect 有效。
    let request = Handle(unsafe {
        WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            core::ptr::null(),
            flags,
        )
    });
    if request.0.is_null() {
        return Err(NetError::Transport("WinHttpOpenRequest 失败".into()));
    }

    // SAFETY: request 有效；没有额外请求头（UA 已在 WinHttpOpen 里给），
    // lpoptional / dwoptionallength 传空 = 无请求体。
    unsafe {
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)
            .map_err(|e| NetError::Transport(format!("发送失败：{e}")))?;
        WinHttpReceiveResponse(request.0, core::ptr::null_mut())
            .map_err(|e| NetError::Transport(format!("接收响应失败：{e}")))?;
    }

    let status = query_status(request.0).unwrap_or(0);
    let body = read_body(request.0)?;
    let bytes = body.len();
    Ok(Response { status, body, bytes })
}

/// 读 HTTP 状态码。
fn query_status(request: *mut core::ffi::c_void) -> Option<u32> {
    let mut code: u32 = 0;
    let mut len: u32 = core::mem::size_of::<u32>() as u32;
    // SAFETY: buffer 指向一个 u32，len 是它的字节数；dwIndex 传空表示取第一个同名头。
    let ok = unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            core::ptr::null_mut(),
        )
    };
    ok.ok().map(|_| code)
}

/// 循环读响应体直到没有数据。
fn read_body(request: *mut core::ffi::c_void) -> Result<String, NetError> {
    let started = std::time::Instant::now();
    let mut out: Vec<u8> = Vec::with_capacity(4096);
    let mut buf = vec![0u8; 8192];
    loop {
        if started.elapsed().as_millis() > TOTAL_BUDGET_MS {
            return Err(NetError::Transport(format!(
                "总耗时超过 {}s 预算（已收 {} 字节）",
                TOTAL_BUDGET_MS / 1000,
                out.len()
            )));
        }
        let mut avail: u32 = 0;
        // SAFETY: avail 是本地变量；request 有效。
        unsafe {
            WinHttpQueryDataAvailable(request, &mut avail)
                .map_err(|e| NetError::Transport(format!("QueryDataAvailable 失败：{e}")))?;
        }
        if avail == 0 {
            break;
        }
        let want = (avail as usize).min(buf.len());
        let mut read: u32 = 0;
        // SAFETY: buf 有 want 字节可写；read 是本地变量。
        unsafe {
            WinHttpReadData(
                request,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                want as u32,
                &mut read,
            )
            .map_err(|e| NetError::Transport(format!("ReadData 失败：{e}")))?;
        }
        if read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
        if out.len() > MAX_BODY {
            return Err(NetError::BadRequest("响应体超过 1 MB".into()));
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// 拆 URL → (host, port, path+query, secure)。
///
/// 只支持 `http://` / `https://`，其余（file / ftp）直接拒绝——
/// 这是安全边界：URL 全部由我们自己构造，但仍显式校验，避免未来被误用。
fn split_url(url: &str) -> Result<(String, u16, String, bool), NetError> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(NetError::BadRequest(format!("不支持的协议：{url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(NetError::BadRequest(format!("URL 缺少主机名：{url}")));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h.to_string(), port),
            Err(_) => return Err(NetError::BadRequest(format!("端口非法：{url}"))),
        },
        None => (
            authority.to_string(),
            if secure { 443 } else { 80 },
        ),
    };
    if host.is_empty() {
        return Err(NetError::BadRequest(format!("URL 缺少主机名：{url}")));
    }
    Ok((host, port, path.to_string(), secure))
}

/// percent-encode（UTF-8 逐字节 `%XX`，空格用 `%20`）。
///
/// 保留 RFC 3986 的 unreserved 字符（字母 / 数字 / `-._~`），其余全部编码。
/// **必须自己实现**：`url::form_urlencoded` 会把空格编成 `+`，
/// 而 lrclib 的查询参数里 `+` 会被当成字面量加号（歌名里真有 `+`）。
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
            out.push(c as char);
        } else {
            out.push('%');
            out.push(hex(c >> 4));
            out.push(hex(c & 0x0F));
        }
    }
    out
}

fn hex(v: u8) -> char {
    match v {
        0..=9 => (b'0' + v) as char,
        _ => (b'A' + (v - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_keeps_unreserved() {
        assert_eq!(percent_encode("abcXYZ019-._~"), "abcXYZ019-._~");
    }

    #[test]
    fn percent_encode_handles_space_and_plus() {
        assert_eq!(percent_encode("a b"), "a%20b");
        // `+` 必须编码成 %2B，否则服务端会当成空格
        assert_eq!(percent_encode("a+b"), "a%2Bb");
    }

    #[test]
    fn percent_encode_handles_cjk_and_ampersand() {
        assert_eq!(percent_encode("晴天"), "%E6%99%B4%E5%A4%A9");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn split_url_parses_https_default_port() {
        let (h, p, path, secure) = split_url("https://lrclib.net/api/get?x=1").expect("解析失败");
        assert_eq!(h, "lrclib.net");
        assert_eq!(p, 443);
        assert_eq!(path, "/api/get?x=1");
        assert!(secure);
    }

    #[test]
    fn split_url_parses_explicit_port_and_http() {
        let (h, p, _, secure) = split_url("http://127.0.0.1:8765/x").expect("解析失败");
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, 8765);
        assert!(!secure);
    }

    #[test]
    fn split_url_rejects_other_schemes() {
        assert!(split_url("file:///c:/x").is_err());
        assert!(split_url("ftp://a/b").is_err());
        assert!(split_url("https://").is_err());
    }

    #[test]
    fn split_url_without_path_uses_root() {
        let (_, _, path, _) = split_url("https://example.com").expect("解析失败");
        assert_eq!(path, "/");
    }

    /// 起一个只回一次的本地 HTTP 服务器，验证 WinHTTP 全链路（含状态码与响应体）。
    fn serve_once(status: u16, body: &'static str) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定端口失败");
        let port = listener.local_addr().expect("取端口失败").port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            }
        });
        port
    }

    #[test]
    fn http_get_reads_status_and_body() {
        let port = serve_once(200, r#"{"syncedLyrics":"[00:01.00]hi"}"#);
        let r = get(&format!("http://127.0.0.1:{port}/api/get?x=1"), "Barline/test")
            .expect("请求失败");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("syncedLyrics"), "body = {}", r.body);
    }

    /// **需要联网**：验证真实 lrclib 的端到端耗时。
    ///
    /// 手动跑：`cargo test lrclib_live -- --ignored --nocapture`
    /// （默认 `#[ignore]`，不让 CI / 常规 `cargo test` 依赖外网）
    #[test]
    #[ignore = "需要联网：手动跑 cargo test lrclib_live -- --ignored --nocapture"]
    fn lrclib_live_get_returns_synced_lyrics() {
        let t0 = std::time::Instant::now();
        let r = get(
            "https://lrclib.net/api/get?artist_name=Coldplay&track_name=Yellow",
            "Barline/0.0.1 (https://github.com/Yi-One1/QuLan-Barline-Next)",
        )
        .expect("请求失败");
        eprintln!(
            "lrclib /api/get → HTTP {} / {} 字节 / {:.1}s",
            r.status,
            r.bytes,
            t0.elapsed().as_secs_f32()
        );
        assert_eq!(r.status, 200, "body = {}", &r.body[..r.body.len().min(200)]);
        assert!(r.body.contains("syncedLyrics"));
    }

    #[test]
    fn http_get_passes_through_404() {
        let port = serve_once(404, r#"{"code":404}"#);
        let r = get(&format!("http://127.0.0.1:{port}/api/get"), "Barline/test")
            .expect("请求失败");
        assert_eq!(r.status, 404);
    }
}
