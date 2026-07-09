//! 上游连接池与连接复用
//!
//! 基于 hyper-util 的连接池客户端，复用后端 TCP 连接，减少握手开销。
//! 供阶段三反向代理使用。请求级超时由代理层 `tokio::time::timeout` 统一管控。

use std::time::Duration;

use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;

use crate::config::ProxyConfig;

/// 上游客户端类型（请求体复用 axum 的 Body，支持零拷贝转发）
pub type UpstreamClient = Client<HttpConnector, axum::body::Body>;

/// 根据代理配置构建带连接池的上游客户端
pub fn build_client(proxy_cfg: &ProxyConfig) -> UpstreamClient {
    let mut connector = HttpConnector::new();
    connector.set_nodelay(true);
    connector.set_keepalive(Some(Duration::from_secs(60)));
    connector.set_connect_timeout(Some(Duration::from_secs(
        proxy_cfg.connect_timeout_secs,
    )));

    let mut builder = Client::builder(TokioExecutor::new());
    builder.pool_max_idle_per_host(proxy_cfg.max_idle_per_host);
    builder.pool_idle_timeout(Some(Duration::from_secs(90)));

    builder.build(connector)
}
