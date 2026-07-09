//! IP 黑名单中间件
//!
//! 基于 axum from_fn 实现，从配置读取黑名单列表，拒绝黑名单 IP 的请求。
//! 配置热更新实时生效（每次请求实时读取 SharedConfig）。
//!
//! 真实客户端 IP 解析：当 `security.trusted_proxy_cidrs` 非空且 peer 落在受信 CIDR 内时，
//! 从 X-Forwarded-For 回溯真实客户端 IP，避免 ALB/Ingress 场景下被代理 IP 误判。

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::Request;

use crate::monitor::metrics;
use crate::network::protocol::{ip_matches_blacklist, parse_trusted_cidrs, resolve_real_client_ip};
use crate::network::server::AppState;
use crate::utils::GatewayError;

/// IP 黑名单中间件：拒绝黑名单中的客户端 IP
pub async fn ip_blacklist_middleware(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let (enabled, blacklist, trusted_cidrs_raw) = {
        let cfg = state.config.load_full();
        (
            cfg.security.enable_ip_blacklist,
            cfg.security.ip_blacklist.clone(),
            cfg.security.trusted_proxy_cidrs.clone(),
        )
    };

    if !enabled {
        return next.run(req).await;
    }

    // 受信代理场景下回溯真实客户端 IP，否则使用 TCP peer
    let peer_ip = addr.ip();
    let xff = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok());
    let trusted = parse_trusted_cidrs(&trusted_cidrs_raw);
    let client_ip = resolve_real_client_ip(peer_ip, xff, &trusted);

    if ip_matches_blacklist(client_ip, &blacklist) {
        metrics::record_auth_failure("ip_blacklisted");
        tracing::warn!(client_ip = %client_ip, peer_ip = %peer_ip, "IP 黑名单拦截");
        return GatewayError::Auth(format!("IP 已被拉黑: {client_ip}")).into_response();
    }

    next.run(req).await
}
