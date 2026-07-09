//! HTTP 协议解析与请求预处理工具
//!
//! 提供反向代理所需的逐跳头过滤、转发头构造等能力，供阶段三代理层使用。

use std::net::SocketAddr;

use http::header::{HeaderMap, HeaderName, HeaderValue};

/// RFC 7230 定义的逐跳头，反向代理时需剔除
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// 剔除逐跳头，并处理 Connection 头中列出的自定义逐跳头
pub fn strip_hop_by_hop_headers(headers: &mut HeaderMap) {
    // 收集 Connection 头中声明的额外逐跳头
    let mut to_remove: Vec<HeaderName> = Vec::new();

    if let Some(conn) = headers.get("connection").cloned() {
        if let Ok(s) = conn.to_str() {
            for name in s.split(',') {
                let name = name.trim();
                if !name.is_empty() {
                    if let Ok(hn) = HeaderName::from_bytes(name.as_bytes()) {
                        to_remove.push(hn);
                    }
                }
            }
        }
    }

    for h in HOP_BY_HOP_HEADERS {
        if let Ok(hn) = HeaderName::from_bytes(h.as_bytes()) {
            to_remove.push(hn);
        }
    }

    for hn in to_remove {
        headers.remove(&hn);
    }
}

/// 追加 X-Forwarded-For 头，记录客户端真实 IP 链路
pub fn append_x_forwarded_for(headers: &mut HeaderMap, client_ip: SocketAddr) {
    let ip = client_ip.ip().to_string();
    let new_value = match headers.get("x-forwarded-for").cloned() {
        Some(existing) => {
            let existing = existing.to_str().unwrap_or("").to_string();
            format!("{existing}, {ip}")
        }
        None => ip,
    };
    if let Ok(val) = HeaderValue::from_str(&new_value) {
        headers.insert("x-forwarded-for", val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_hop_by_hop_and_connection_listed() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive, X-Custom".parse().unwrap());
        headers.insert("keep-alive", "timeout=5".parse().unwrap());
        headers.insert("x-custom", "v".parse().unwrap());
        headers.insert("x-keep", "v".parse().unwrap());

        strip_hop_by_hop_headers(&mut headers);

        assert!(headers.get("connection").is_none());
        assert!(headers.get("keep-alive").is_none());
        assert!(headers.get("x-custom").is_none());
        assert!(headers.get("x-keep").is_some());
    }
}
