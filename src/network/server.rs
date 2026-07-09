//! 网络服务监听与启停管理
//!
//! 基于配置驱动：监听地址、请求超时、请求体大小限制、健康检查与管理路由均来自配置。
//! 反向代理 fallback 路由：未命中健康检查/管理路由的请求统一进入代理转发。
//! /metrics 指标端点、/_admin 运维路由族、健康检查 JSON。
//!
//! TLS 支持：配置 `server.tls.enable = true` 时使用 axum-server 的 rustls 监听。
//! 健康检查/指标/管理路由通过 fallback 动态分发，配置热更新后路径变更立即生效。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, State};
use axum::middleware::from_fn_with_state;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use axum_server::tls_rustls::RustlsConfig;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::limit::RequestBodyLimitLayer;

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
    /// 共享配置（支持热更新，无锁原子读取）
    pub config: SharedConfig,
    /// 上游客户端（带连接池，全局复用）
    pub client: UpstreamClient,
    /// 负载均衡器（维护每路由状态）
    pub balancer: Arc<Balancer>,
    /// 限流管理器（按 IP 令牌桶，支持配置热更新重建）
    pub rate_limiter: Arc<RateLimitManager>,
    /// 配置文件目录（供 admin reload 接口使用，避免依赖环境变量）
    pub config_dir: String,
    /// 运行环境名（供 admin reload 接口使用）
    pub env: String,
}

impl AppState {
    /// 从共享配置构建应用状态：构建上游客户端、均衡器与限流器
    pub fn from_config(config: SharedConfig) -> Self {
        let proxy_cfg = { config.load_full().proxy.clone() };
        let client = build_client(&proxy_cfg);
        let balancer = Arc::new(Balancer::new());
        let rate_limiter = Arc::new(RateLimitManager::new());
        Self {
            config,
            client,
            balancer,
            rate_limiter,
            config_dir: String::new(),
            env: String::new(),
        }
    }

    /// 带配置目录与环境名构建应用状态（推荐入口，admin reload 接口可直接使用）
    pub fn with_config_dir(config: SharedConfig, config_dir: String, env: String) -> Self {
        let mut state = Self::from_config(config);
        state.config_dir = config_dir;
        state.env = env;
        state
    }
}

/// 启动 HTTP/HTTPS 服务并阻塞至关闭
///
/// 优雅关闭流程：收到 Ctrl+C / SIGTERM 后停止接受新连接，
/// 等待 in-flight 请求完成；若超过 graceful_shutdown_timeout_secs 则强制结束。
pub async fn run(shared: SharedConfig, config_dir: String, env: String) -> anyhow::Result<()> {
    let (host, port, graceful_timeout, tls_enable, tls_cert, tls_key) = {
        let c = shared.load_full();
        (
            c.server.host.clone(),
            c.server.port,
            c.server.graceful_shutdown_timeout_secs,
            c.server.tls.enable,
            c.server.tls.cert_path.clone(),
            c.server.tls.key_path.clone(),
        )
    };

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let state = AppState::with_config_dir(shared, config_dir, env);

    // 启动限流器 GC 后台任务（按 rate_limit_gc_secs 周期清理过期的 per-IP 状态）
    let gc_interval = {
        let c = state.config.load_full();
        Duration::from_secs(c.security.rate_limit_gc_secs)
    };
    crate::middleware::rate_limit::spawn_gc_task(state.rate_limiter.clone(), gc_interval);

    let app = build_router(state);

    if tls_enable {
        // TLS 监听：使用 axum-server + rustls
        let rustls_config = load_tls_config(&tls_cert, &tls_key)
            .map_err(|e| anyhow::anyhow!("TLS 证书加载失败: {e}"))?;
        tracing::info!(%addr, "网关服务启动（HTTPS），开始监听");
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(graceful_timeout.max(1))));
        });
        axum_server::bind_rustls(addr, rustls_config)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    } else {
        // 明文 HTTP 监听
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(%addr, "网关服务启动（HTTP），开始监听");
        let serve_fut = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal());

        let graceful_dur = Duration::from_secs(graceful_timeout.max(1));
        match tokio::time::timeout(graceful_dur, serve_fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(anyhow::anyhow!(e.to_string())),
            Err(_) => {
                tracing::warn!(
                    timeout_secs = graceful_timeout,
                    "优雅关闭超时，强制结束剩余 in-flight 请求"
                );
            }
        }
    }

    tracing::info!("网关服务已停止");
    Ok(())
}

/// 加载 TLS 证书与私钥，构建 axum-server 的 RustlsConfig
fn load_tls_config(cert_path: &str, key_path: &str) -> anyhow::Result<RustlsConfig> {
    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    // 构建 rustls ServerConfig：TLS1.2+1.3，按证书顺序逐个尝试
    let mut server_config = rustls::server::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("构建 rustls ServerConfig 失败: {e}"))?;
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(RustlsConfig::from_config(std::sync::Arc::new(
        server_config,
    )))
}

fn load_certs(path: &str) -> anyhow::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("打开证书文件 {path} 失败: {e}"))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("解析证书失败: {e}"))
}

fn load_private_key(path: &str) -> anyhow::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("打开私钥文件 {path} 失败: {e}"))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| anyhow::anyhow!("解析私钥失败: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("私钥文件为空"))
}

/// 构建路由（配置驱动：健康检查、指标端点、运维管理、请求体限制、反向代理 fallback）
///
/// 中间件顺序（外→内）：CORS → 限流 → IP黑名单 → 鉴权 → 请求体限制 → 处理器
/// 外层中间件先执行，可在请求进入代理前尽早拦截（限流/黑名单/鉴权）。
///
/// 超时策略：
/// - 不使用全局 TimeoutLayer：它会截断 SSE / 长响应流，破坏流式代理语义。
/// - 代理路由的上游超时由 `proxy::forward` 内部 `tokio::time::timeout` 实现，
///   该超时作用于 `client.request()` 返回的 Future（即首字节超时），
///   响应头到达后 body 流式透传不受限，正确支持 SSE / 大文件 / chunked 流。
/// - 健康检查 / 指标 / 管理接口为进程内同步处理，耗时毫秒级，无需全局超时。
///
/// 注：req_limit / cors 在启动时绑定，热更新需重启才能生效；
/// 路由匹配、鉴权、限流、黑名单、代理超时等通过 SharedConfig 实时读取，热更新即时生效。
pub fn build_router(state: AppState) -> Router {
    let (req_limit, cors_layer) = {
        let c = state.config.load_full();
        (
            c.network.request_size_limit_bytes,
            build_cors_layer(&c.cors),
        )
    };

    // 健康检查与指标端点注册为静态路由（路径在启动时确定）；
    // fallback 中动态分发 admin 与代理，使 admin.enable 等配置热更新即时生效。
    let mut router: Router<AppState> = Router::new().route("/", get(root_handler));

    // 健康检查与指标路径在 fallback 中动态检查（支持热更新路径变更）
    router = router.fallback(dynamic_dispatch_handler);

    // 管理路由（始终挂载，fallback 中按 admin.enable 动态决定是否放行）
    router = router.nest("/_admin", admin_router());

    // 逐层叠加（后加的为外层，先执行）：
    // 请求流向：CORS → 限流 → IP黑名单 → 鉴权 → Body限制 → Handler
    router = router
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

/// 动态分发处理器：根据当前配置实时判断请求路径属于健康检查/指标/管理/代理
///
/// 此处理器替代静态路由注册，使 health_path、metrics_path、admin.enable 等配置
/// 在热更新后无需重建 Router 即可生效。
async fn dynamic_dispatch_handler(
    State(state): State<AppState>,
    ConnectInfo(client_addr): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
) -> Response {
    let path = req.uri().path().to_string();

    let cfg = state.config.load_full();

    // 健康检查
    if cfg.monitor.enable_health_check && path == cfg.monitor.health_path {
        return axum::Json(health::health_json(&state.config)).into_response();
    }
    // 指标端点
    if cfg.monitor.enable_metrics && path == cfg.monitor.metrics_path {
        let body = metrics::render();
        return (
            [(http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
            body,
        )
            .into_response();
    }

    // 管理路由（/_admin/* 已由 nest 注册，此处仅代理非管理路径）
    // 未命中专用路由 → 反向代理
    drop(cfg);

    let method = req.method().clone();
    let start = Instant::now();

    metrics::inc_active_connection();
    let resp = proxy::forward(req, client_addr, state.config.clone(), &state.client, &state.balancer)
        .await;
    metrics::dec_active_connection();

    let status = resp.status().as_u16();
    // 使用路由名称作为 metrics 标签（低基数），避免原始 path 导致 Prometheus 序列爆炸；
    // 未匹配路由的响应无 RouteLabel 扩展，回退为 "unmatched"
    let route_label = resp
        .extensions()
        .get::<proxy::RouteLabel>()
        .map(|l| l.0.as_str())
        .unwrap_or("unmatched");
    metrics::record_request(method.as_str(), route_label, status, start.elapsed());
    resp
}

/// 优雅关闭信号监听：Ctrl+C 或 SIGTERM
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            // 安装失败时不立即返回（否则 select! 会触发关闭导致启动即退出），
            // 改为永久挂起，与 unix 分支的兜底语义一致
            tracing::error!(error = %e, "安装 Ctrl+C 信号处理器失败，该信号将不可用");
            std::future::pending::<()>().await;
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
    use arc_swap::ArcSwap;

    #[test]
    fn build_router_with_default_config() {
        let cfg = std::sync::Arc::new(ArcSwap::from_pointee(AppConfig::default()));
        let state = AppState::from_config(cfg);
        let _router = build_router(state);
    }

    #[test]
    fn app_state_is_clone() {
        let cfg = std::sync::Arc::new(ArcSwap::from_pointee(AppConfig::default()));
        let state = AppState::from_config(cfg);
        let _cloned = state.clone();
    }
}
