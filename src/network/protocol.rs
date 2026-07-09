//! HTTP 协议解析与请求预处理工具
//!
//! 提供反向代理所需的逐跳头过滤、转发头构造等能力，供阶段三代理层使用。

use std::net::{IpAddr, SocketAddr};

use http::header::{HeaderMap, HeaderName, HeaderValue};
use ipnet::IpNet;

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
///
/// `trust_client_xff` 为 false（默认）时，先丢弃入站 XFF 再追加真实客户端 IP，
/// 防止客户端伪造 XFF 欺骗上游。设为 true（部署在受信代理后）时保留并追加。
pub fn append_x_forwarded_for(headers: &mut HeaderMap, client_ip: SocketAddr, trust_client_xff: bool) {
    let ip = client_ip.ip().to_string();
    let new_value = if trust_client_xff {
        match headers.get("x-forwarded-for").cloned() {
            Some(existing) => {
                let existing = existing.to_str().unwrap_or("").to_string();
                format!("{existing}, {ip}")
            }
            None => ip,
        }
    } else {
        // 不信任入站 XFF：直接覆盖为真实客户端 IP
        ip
    };
    if let Ok(val) = HeaderValue::from_str(&new_value) {
        headers.insert("x-forwarded-for", val);
    }
}

/// 基于受信代理 CIDR 列表解析真实客户端 IP
///
/// 部署在 ALB / Ingress 等受信代理后时，TCP peer 总是代理而非真实客户端。
/// 此时通过 X-Forwarded-For 回溯可拿到真实 IP；但需校验 peer 是否在受信 CIDR 内，
/// 否则任意客户端均可伪造 XFF 绕过限流或黑名单。
///
/// 算法：
/// - `trusted_cidrs` 为空 → 直接返回 peer_ip（不信任 XFF，避免伪造）
/// - peer_ip 不在 trusted_cidrs 内 → 直接返回 peer_ip
/// - peer_ip 在 trusted_cidrs 内 → 从 XFF 右侧向左扫描，遇到第一个不在 trusted_cidrs
///   内的 IP 即视为真实客户端；若全部为受信代理则返回最左侧 IP（多层代理场景）；
///   若 XFF 缺失或解析失败则回退为 peer_ip
pub fn resolve_real_client_ip(
    peer_ip: IpAddr,
    xff_header: Option<&str>,
    trusted_cidrs: &[IpNet],
) -> IpAddr {
    if trusted_cidrs.is_empty() || !trusted_cidrs.iter().any(|c| c.contains(&peer_ip)) {
        return peer_ip;
    }
    let xff = match xff_header {
        Some(v) => v.trim(),
        None => return peer_ip,
    };
    if xff.is_empty() {
        return peer_ip;
    }
    let ips: Vec<IpAddr> = xff
        .split(',')
        .map(|s| s.trim().parse::<IpAddr>())
        .filter_map(Result::ok)
        .collect();
    if ips.is_empty() {
        return peer_ip;
    }
    // 从右往左找第一个非受信 IP（最接近客户端的那一跳）
    for ip in ips.iter().rev() {
        if !trusted_cidrs.iter().any(|c| c.contains(ip)) {
            return *ip;
        }
    }
    // 全部 IP 均为受信代理（罕见，多层代理同网段）→ 返回最左侧
    ips[0]
}

/// 判断 IP 是否匹配黑名单条目
///
/// 每个条目支持以下三种形式（按优先级尝试）：
/// 1. CIDR 表示法（如 `10.0.0.0/8`、`fe80::/10`）
/// 2. 单 IP 精确匹配（如 `192.168.1.5`）
/// 3. 旧式通配前缀（如 `192.168.*`）—— 仅向后兼容，新配置建议使用 CIDR
pub fn ip_matches_blacklist(ip: IpAddr, patterns: &[String]) -> bool {
    for p in patterns {
        if let Ok(net) = p.parse::<IpNet>() {
            if net.contains(&ip) {
                return true;
            }
            continue;
        }
        if let Ok(p_ip) = p.parse::<IpAddr>() {
            if p_ip == ip {
                return true;
            }
            continue;
        }
        if p.ends_with('*') && ip.to_string().starts_with(&p[..p.len() - 1]) {
            return true;
        }
    }
    false
}

/// 解析受信代理 CIDR 列表（容错：跳过非法条目）
pub fn parse_trusted_cidrs(raw: &[String]) -> Vec<IpNet> {
    raw.iter()
        .filter_map(|s| s.trim().parse::<IpNet>().ok())
        .collect()
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

    fn cidrs(items: &[&str]) -> Vec<IpNet> {
        items.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn resolve_ip_returns_peer_when_no_trusted_cidrs() {
        let peer: IpAddr = "203.0.113.10".parse().unwrap();
        let xff = "1.2.3.4, 5.6.7.8";
        assert_eq!(
            resolve_real_client_ip(peer, Some(xff), &[]),
            peer,
            "无受信 CIDR 时即便带 XFF 也应回退到 peer"
        );
    }

    #[test]
    fn resolve_ip_returns_peer_when_peer_not_trusted() {
        let peer: IpAddr = "203.0.113.10".parse().unwrap();
        let trusted = cidrs(&["10.0.0.0/8"]);
        let xff = "1.2.3.4";
        assert_eq!(
            resolve_real_client_ip(peer, Some(xff), &trusted),
            peer,
            "peer 不在受信 CIDR 内 → 不信任 XFF，返回 peer"
        );
    }

    #[test]
    fn resolve_ip_walks_xff_from_right_when_peer_trusted() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = cidrs(&["10.0.0.0/8"]);
        // XFF: client → proxy1(10.0.0.2) → proxy2(10.0.0.3) → veil
        // 从右往左第一个非受信 = 203.0.113.99
        let xff = "203.0.113.99, 10.0.0.2, 10.0.0.3";
        assert_eq!(
            resolve_real_client_ip(peer, Some(xff), &trusted),
            "203.0.113.99".parse::<IpAddr>().unwrap(),
        );
    }

    #[test]
    fn resolve_ip_returns_leftmost_when_all_trusted() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = cidrs(&["10.0.0.0/8"]);
        let xff = "10.0.0.5, 10.0.0.6";
        assert_eq!(
            resolve_real_client_ip(peer, Some(xff), &trusted),
            "10.0.0.5".parse::<IpAddr>().unwrap(),
        );
    }

    #[test]
    fn resolve_ip_handles_garbage_xff() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = cidrs(&["10.0.0.0/8"]);
        assert_eq!(
            resolve_real_client_ip(peer, Some("not-an-ip"), &trusted),
            peer,
        );
        assert_eq!(resolve_real_client_ip(peer, None, &trusted), peer);
        assert_eq!(
            resolve_real_client_ip(peer, Some("   "), &trusted),
            peer
        );
    }

    #[test]
    fn blacklist_matches_cidr_single_ip_and_wildcard() {
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(ip_matches_blacklist(ip, &["192.168.0.0/16".into()]));
        assert!(ip_matches_blacklist(ip, &["192.168.1.5".into()]));
        assert!(ip_matches_blacklist(ip, &["192.168.*".into()]));
        assert!(!ip_matches_blacklist(ip, &["10.0.0.0/8".into()]));
        assert!(!ip_matches_blacklist(ip, &["192.168.1.4".into()]));
    }

    #[test]
    fn parse_trusted_cidrs_skips_invalid() {
        let raw = vec!["10.0.0.0/8".into(), "garbage".into(), "::1/128".into()];
        let out = parse_trusted_cidrs(&raw);
        assert_eq!(out.len(), 2);
    }
}
