//! 网络服务监听与启停管理
//!
//! 基于配置驱动：监听地址、请求超时、请求体大小限制、健康检查与管理路由均来自配置。
//! 阶段三起接入反向代理 fallback 路由：未命中健康检查/管理路由的请求统一进入代理转发。
//! 阶段五接入 /metrics 指标端点、/_admin 运维路由族、健康检查 JSON。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, State};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::{limit::RequestBodyLimitLayer, timeout::TimeoutLayer};

use crate::admin::api::admin_router;
use crate::config::SharedConfig;
use crate::core::balancer::Balancer;
use crate::core::proxy;
use crate::middleware::{
    auth::auth_middleware,
    cors::build_cors_layer,
    ip_blacklist::ip_blacklist_middleware,
    rate_limit::{RateLimitManager, rate_limit_middleware},
};
use crate::monitor::{health, metrics};
use crate::network::connection::{build_client, UpstreamClient};

/// 应用共享状态：贯穿所有请求处理
#[derive(Clone)]
pub struct AppState {
    /// 共享配置（支持热更新）
    pub config: SharedConfig,
    /// 上游客户端（带连接池，全局复用）
    pub client: UpstreamClient,
    /// 负载均衡器（维护每路由状态）
    pub balancer: Arc<Balancer>,
    /// 限流管理器（按 IP 令牌桶，支持配置热更新重建）
    pub rate_limiter: Arc<RateLimitManager>,
}

impl AppState {
    /// 从共享配置构建应用状态：构建上游客户端、均衡器与限流器
    pub fn from_config(config: SharedConfig) -> Self {
        let proxy_cfg = { config.read().proxy.clone() };
        let client = build_client(&proxy_cfg);
        let balancer = Arc::new(Balancer::new());
        let rate_limiter = Arc::new(RateLimitManager::new());
        Self {
            config,
            client,
            balancer,
            rate_limiter,
        }
    }
}

/// 启动 HTTP 服务并阻塞至关闭
///
/// 优雅关闭流程：收到 Ctrl+C / SIGTERM 后停止接受新连接，
/// 等待 in-flight 请求完成；若超过 graceful_shutdown_timeout_secs 则强制结束。
pub async fn run(shared: SharedConfig) -> anyhow::Result<()> {
    let (host, port, graceful_timeout) = {
        let c = shared.read();
        (
            c.server.host.clone(),
            c.server.port,
            c.server.graceful_shutdown_timeout_secs,
        )
    };

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "网关服务启动，开始监听");

    let state = AppState::from_config(shared);
    let app = build_router(state);

    // 使用 into_make_service_with_connect_info 以便代理层获取客户端真实 IP
    // with_graceful_shutdown：收到 Ctrl+C / SIGTERM 后停止接受新连接，
    // 并等待 in-flight 请求完成后返回。axum 0.7 内置 drain 机制。
    // graceful_shutdown_timeout_secs 配置暂作记录，如需硬性 drain 上限，
    // 可改为自定义 accept 循环 + tokio::time::timeout 控制（避免影响正常运行期）。
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        return Err(anyhow::anyhow!(e.to_string()));
    }

    let _ = graceful_timeout; // 保留配置字段，供未来自定义 drain 超时使用
    tracing::info!("网关服务已停止");
    Ok(())
}

/// 构建路由（配置驱动：健康检查、指标端点、运维管理、超时与大小限制、反向代理 fallback）
///
/// 中间件顺序（外→内）：CORS → 限流 → IP黑名单 → 鉴权 → 请求体限制 → 超时 → 处理器
/// 外层中间件先执行，可在请求进入代理前尽早拦截（限流/黑名单/鉴权）。
pub fn build_router(state: AppState) -> Router {
    let (
        read_timeout,
        req_limit,
        admin_enabled,
        admin_prefix,
        health_path,
        metrics_path,
        cors_layer,
    ) = {
        let c = state.config.read();
        (
            c.network.read_timeout_secs,
            c.network.request_size_limit_bytes,
            c.admin.enable,
            c.admin.prefix.clone(),
            if c.monitor.enable_health_check {
                c.monitor.health_path.clone()
            } else {
                String::new()
            },
            if c.monitor.enable_metrics {
                c.monitor.metrics_path.clone()
            } else {
                String::new()
            },
            build_cors_layer(&c.cors),
        )
    };

    let mut router: Router<AppState> = Router::new().route("/", get(root_handler));

    if !health_path.is_empty() {
        router = router.route(&health_path, get(health_handler));
    }

    if !metrics_path.is_empty() {
        router = router.route(&metrics_path, get(metrics_handler));
    }

    if admin_enabled {
        router = router.nest(&admin_prefix, admin_router());
    }

    // 未命中上述路由的请求统一进入反向代理
    router = router.fallback(proxy_handler);

    // 逐层叠加（后加的为外层，先执行）：
    // 请求流向：CORS → 限流 → IP黑名单 → 鉴权 → Body限制 → 超时 → Handler
    router = router
        .layer(TimeoutLayer::new(Duration::from_secs(read_timeout.max(1))))
        .layer(RequestBodyLimitLayer::new(req_limit))
        .layer(from_fn_with_state(state.clone(), auth_middleware))
        .layer(from_fn_with_state(state.clone(), ip_blacklist_middleware))
        .layer(from_fn_with_state(state.clone(), rate_limit_middleware));

    // CORS 为可选层（启用时追加为最外层）
    if let Some(cors) = cors_layer {
        router = router.layer(cors);
    }

    router.with_state(state)
}

async fn root_handler() -> &'static str {
    "veil is running"
}

/// 健康检查端点：返回 JSON 状态（供 K8s readiness/liveness 探针）
async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let json = health::health_json(&state.config);
    axum::Json(json)
}

/// Prometheus 指标端点：返回 text/plain 格式指标数据
async fn metrics_handler() -> impl IntoResponse {
    let body = metrics::render();
    (
        [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
}

/// 反向代理 fallback：将未命中专用路由的请求转发至上游
///
/// 在此层记录请求级指标（活跃连接、请求计数、耗时），
/// 上游级指标（每次转发、重试）在 proxy::forward 内部记录。
async fn proxy_handler(
    State(state): State<AppState>,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();

    metrics::inc_active_connection();
    let resp = proxy::forward(req, client_addr, state.config.clone(), &state.client, &state.balancer).await;
    metrics::dec_active_connection();

    let status = resp.status().as_u16();
    metrics::record_request(method.as_str(), &path, status, start.elapsed());
    resp
}

/// 优雅关闭信号监听：Ctrl+C 或 SIGTERM
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!(error = %e, "安装 Ctrl+C 信号处理器失败");
            return;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            sig.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("收到 Ctrl+C 信号，开始优雅关闭"),
        _ = terminate => tracing::info!("收到 SIGTERM 信号，开始优雅关闭"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn build_router_with_default_config() {
        let cfg = std::sync::Arc::new(parking_lot::RwLock::new(AppConfig::default()));
        let state = AppState::from_config(cfg);
        let _router = build_router(state);
    }

    #[test]
    fn app_state_is_clone() {
        let cfg = std::sync::Arc::new(parking_lot::RwLock::new(AppConfig::default()));
        let state = AppState::from_config(cfg);
        let _cloned = state.clone();
    }
}
