//! IP 黑名单中间件
//!
//! 基于 axum from_fn 实现，从配置读取黑名单列表，拒绝黑名单 IP 的请求。
//! 配置热更新实时生效（每次请求实时读取 SharedConfig）。

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::Request;

use crate::monitor::metrics;
use crate::network::server::AppState;
use crate::utils::GatewayError;

/// IP 黑名单中间件：拒绝黑名单中的客户端 IP
pub async fn ip_blacklist_middleware(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let (enabled, blacklist) = {
        let cfg = state.config.read();
        (cfg.security.enable_ip_blacklist, cfg.security.ip_blacklist.clone())
    };

    if !enabled {
        return next.run(req).await;
    }

    let client_ip = addr.ip().to_string();
    if blacklist.iter().any(|b| {
        // 支持精确匹配和 CIDR 前缀匹配（简易：仅精确匹配 + 前缀通配）
        b == &client_ip || (b.ends_with('*') && client_ip.starts_with(&b[..b.len() - 1]))
    }) {
        metrics::record_auth_failure("ip_blacklisted");
        tracing::warn!(client_ip = %client_ip, "IP 黑名单拦截");
        return GatewayError::Auth(format!("IP 已被拉黑: {client_ip}")).into_response();
    }

    next.run(req).await
}
