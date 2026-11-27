//! HTTP 小工具：URL 解码、读请求体、查询参数、JSON 与静态资源响应。

use crate::error::{Error, ErrorKind};
use serde_json::{Value, json};
use tiny_http::{Header, Response};

pub(super) fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| -> Option<u8> {
                    match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        b'A'..=b'F' => Some(c - b'A' + 10),
                        _ => None,
                    }
                };
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(a), Some(c)) => {
                        out.push(a * 16 + c);
                        i += 3;
                    }
                    _ => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 请求体上限。控制台最长的报文是轨迹 JSON，实测 2 MB 的请求体对应约
/// 40 万个点，远超任何真实用例；给 8 MB 是留足余量又不至于让一次误发
/// 就把进程内存打满。
pub(super) const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn read_json(request: &mut tiny_http::Request) -> Result<Value, Error> {
    // `read_to_string` 原来不带上限：客户端声明 `Content-Length: 1000000000`
    // 却只发一个字节，这行会一直读下去把线程钉住；真发满就直接吃光内存。
    // 用 `take` 卡住上限，超出的明确报错而不是硬读。
    use std::io::Read as _;
    let mut s = String::new();
    let mut r = std::io::Read::take(request.as_reader(), MAX_BODY_BYTES + 1);
    r.read_to_string(&mut s)?;
    if s.len() as u64 > MAX_BODY_BYTES {
        return Err(ErrorKind::client(format!(
            "请求体超过 {MAX_BODY_BYTES} 字节上限"
        )));
    }
    serde_json::from_str(&s).map_err(|e| ErrorKind::client(format!("请求体不是 JSON: {e}")))
}

pub(super) fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k == key {
            it.next().map(url_decode)
        } else {
            None
        }
    })
}

// Every caller passes a constant header name and a MIME type from the asset table.
#[expect(clippy::expect_used, reason = "arguments are compile-time constants")]
pub(super) fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("header 构造失败")
}

pub(super) fn respond_json(
    request: tiny_http::Request,
    status: u16,
    body: Value,
) -> Result<(), Error> {
    let s = serde_json::to_string(&body).unwrap_or_else(|_| "{}".into());
    let resp = Response::from_string(s)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json; charset=utf-8"));
    request.respond(resp).map_err(ErrorKind::Storage)
}

/// `application/x-www-form-urlencoded` 的最小百分号编码（只用于自检查询串）。
pub(super) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(super) fn host_short(host: &str) -> String {
    host.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(host)
        .to_string()
}

pub(super) fn json_ok(request: tiny_http::Request, body: Value) -> Result<(), Error> {
    respond_json(request, 200, json!({"ok": true, "data": body}))
}

pub(super) fn json_err(request: tiny_http::Request, msg: &str) -> Result<(), Error> {
    respond_json(request, 200, json!({"ok": false, "error": msg}))
}

/// 类型化错误 → HTTP 响应(#62):状态码按 `ErrorKind`,不再一律 400;
/// 响应体带 `kind` 字段与厂商业务码 `vendor_code`,脚本按结构分流。
pub(super) fn json_err_kind(
    request: tiny_http::Request,
    e: &crate::error::Error,
) -> Result<(), Error> {
    let mut body = serde_json::json!({
        "ok": false,
        "error": e.to_string(),
        "kind": e.kind_name(),
    });
    if let crate::error::ErrorKind::Protocol { code: Some(c), .. } = e {
        body["vendor_code"] = serde_json::json!(c);
    }
    respond_json(request, e.http_status(), body)
}

pub(super) fn json_err_code(
    request: tiny_http::Request,
    code: u16,
    msg: &str,
) -> Result<(), Error> {
    respond_json(request, code, json!({"ok": false, "error": msg}))
}

pub(super) fn respond_static(
    request: tiny_http::Request,
    body: &str,
    content_type: &str,
) -> Result<(), Error> {
    let resp = Response::from_string(body)
        .with_header(header("Content-Type", content_type))
        .with_header(header("Referrer-Policy", "no-referrer"))
        // 不设这个头，浏览器会一直用缓存的旧页面：改了按钮逻辑用户也看不到，
        // 点了还是旧行为（表现为「退出键没反应」）。
        .with_header(header(
            "Cache-Control",
            "no-store, no-cache, must-revalidate, max-age=0",
        ))
        .with_header(header("Pragma", "no-cache"))
        .with_header(header("Expires", "0"));
    request.respond(resp).map_err(ErrorKind::Storage)
}

#[cfg(test)]
mod tests {
    use super::url_decode;

    #[test]
    fn decodes_space_plus_and_percent() {
        assert_eq!(url_decode("xiaomi%2013"), "xiaomi 13");
        assert_eq!(url_decode("xiaomi+13"), "xiaomi 13");
        assert_eq!(url_decode("2211133C"), "2211133C");
        // 中文（UTF-8 多字节）
        assert_eq!(url_decode("%E5%B0%8F%E7%B1%B3"), "小米");
        // 坏的转义原样留着，不 panic
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%zz"), "%zz");
    }
}
