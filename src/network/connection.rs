//! 上游连接池与连接复用
//!
//! 基于 hyper-util + hyper-rustls 构建连接池客户端，同时支持 HTTP 与 HTTPS 上游。
//! 复用后端 TCP 连接，减少握手开销。供反向代理使用。
//! 请求级超时由代理层 `tokio::time::timeout` 统一管控。

use std::time::Duration;

use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioTimer};

use crate::config::ProxyConfig;
use crate::constant;

/// 上游客户端类型（支持 HTTP/HTTPS，请求体复用 axum 的 Body）
pub type UpstreamClient = Client<HttpsConnector<HttpConnector>, axum::body::Body>;

/// 根据代理配置构建带连接池的上游客户端
///
/// 使用 hyper-rustls 构建同时支持 http:// 和 https:// 上游的连接器，
/// 启用 HTTP/2 与连接池定时器（pool_timer），确保空闲连接按时回收。
pub fn build_client(proxy_cfg: &ProxyConfig) -> UpstreamClient {
    let mut http = HttpConnector::new();
    http.set_nodelay(true);
    http.set_keepalive(Some(Duration::from_secs(constant::UPSTREAM_KEEPALIVE_SECS)));
    http.set_connect_timeout(Some(Duration::from_secs(
        proxy_cfg.connect_timeout_secs,
    )));

    // 构建 HTTPS 连接器：同时支持 http 和 https，启用 HTTP/2，使用 webpki 内置 CA 根证书
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http2()
        .build();

    let mut builder = Client::builder(TokioExecutor::new());
    builder.pool_max_idle_per_host(proxy_cfg.max_idle_per_host);
    builder.pool_idle_timeout(Some(Duration::from_secs(
        constant::UPSTREAM_POOL_IDLE_TIMEOUT_SECS,
    )));
    // 启用连接池定时器，定期清理过期空闲连接
    builder.pool_timer(TokioTimer::new());

    builder.build(connector)
}
