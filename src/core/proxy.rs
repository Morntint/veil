//! 反向代理、请求转发核心逻辑
//!
//! 流程：路由匹配 → 负载均衡选上游 → 路径改写 → 构造目标 URI →
//! 处理逐跳头/XFF → 超时管控转发 → 失败重试 → 回写响应。
//!
//! 请求上下文贯穿全程，用于日志关联。路由信息实时读取自 SharedConfig，
//! 配置热更新后立即对新请求生效。上游连接复用由 UpstreamClient 连接池管理。
//!
//! 重试机制：请求体在转发前缓冲至内存（Bytes 引用计数克隆），
//! 失败时按 route.retries 次数重试，每次重新选择上游节点。
//! 连接错误/超时对所有方法重试，5xx 仅对幂等方法重试。

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http::{HeaderMap, Request, Uri};
use http_body_util::BodyExt;
use tracing::Instrument;

use crate::config::SharedConfig;
use crate::core::{balancer::Balancer, context::RequestContext, router};
use crate::middleware::{retry, rewrite};
use crate::monitor::metrics;
use crate::network::connection::UpstreamClient;
use crate::network::protocol;

/// 响应扩展：携带命中路由名称，供外层 metrics 记录低基数标签
///
/// 避免 Prometheus 标签使用原始 path（用户 ID 等高基字段会导致序列爆炸），
/// 改用 route.name 作为标签维度。
#[derive(Clone, Debug)]
pub struct RouteLabel(pub String);

use crate::utils::{GatewayError, Result};

/// 反向代理转发入口
///
/// 接收原始请求，匹配路由并转发至上游，返回上游响应。
/// 支持路径改写和失败重试。任一环节失败映射为对应的 HTTP 错误响应。
pub async fn forward(
    req: Request<Body>,
    client_addr: SocketAddr,
    config: SharedConfig,
    client: &UpstreamClient,
    balancer: &Balancer,
) -> Response {
    let method = req.method().clone();
    let original_uri = req.uri().clone();
    let path_only = original_uri.path().to_string();

    let mut ctx = RequestContext::new(client_addr, method.clone(), path_only.clone());

    // 1. 路由匹配（读取 SharedConfig，实时反映热更新）
    let route = {
        let cfg = config.load_full();
        router::match_route(&cfg.routes, &path_only, &cfg.route_index)
    };
    let route = match route {
        Some(r) => r,
        None => {
            tracing::info!(
                request_id = %ctx.request_id,
                method = %method,
                path = %path_only,
                "未匹配到路由"
            );
            return GatewayError::Route(format!("未匹配路由: {path_only}")).into_response();
        }
    };
    ctx = ctx.with_route(route.clone());
    let route_name = ctx.route_name().to_string();

    // 2. 读取全局配置（代理超时、请求体限制、XFF 信任）
    let (body_limit, proxy_timeout_secs, trust_client_xff) = {
        let cfg = config.load_full();
        (
            cfg.network.request_size_limit_bytes,
            cfg.proxy.timeout_secs,
            cfg.proxy.trust_client_xff,
        )
    };

    // 3. 解析超时与重试次数：路由级优先，否则用全局配置
    let timeout_secs = if route.timeout_secs > 0 {
        route.timeout_secs
    } else {
        proxy_timeout_secs
    };
    let max_attempts = route.retries.saturating_add(1).max(1);

    let original_headers = req.headers().clone();

    // 4. 请求体策略：需重试时缓冲到内存（Bytes 零拷贝克隆），否则直接流式转发
    let mut resp = if max_attempts > 1 {
        // --- 缓冲模式：支持重试 ---
        let body_bytes = match to_bytes(req.into_body(), body_limit).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(request_id = %ctx.request_id, error = %e, "读取请求体失败");
                return GatewayError::PayloadTooLarge(format!("读取请求体失败: {e}")).into_response();
            }
        };
        forward_with_retry(
            ctx,
            &method,
            &original_uri,
            &original_headers,
            &body_bytes,
            &route,
            max_attempts,
            timeout_secs,
            trust_client_xff,
            client_addr,
            client,
            balancer,
        )
        .await
    } else {
        // --- 流式模式：无重试，请求体直接透传（支持 SSE/大文件上传） ---
        forward_streaming(
            ctx,
            &method,
            &original_uri,
            &original_headers,
            req.into_body(),
            &route,
            timeout_secs,
            trust_client_xff,
            client_addr,
            client,
            balancer,
        )
        .await
    };

    // 注入路由名称到响应扩展，供外层 metrics 用低基数标签记录
    resp.extensions_mut().insert(RouteLabel(route_name));
    resp
}

/// 缓冲模式转发：请求体已在内存中，支持多次重试
#[allow(clippy::too_many_arguments)]
async fn forward_with_retry(
    mut ctx: RequestContext,
    method: &http::Method,
    original_uri: &Uri,
    original_headers: &HeaderMap,
    body_bytes: &Bytes,
    route: &std::sync::Arc<crate::config::RouteConfig>,
    max_attempts: u32,
    timeout_secs: u64,
    trust_client_xff: bool,
    client_addr: SocketAddr,
    client: &UpstreamClient,
    balancer: &Balancer,
) -> Response {
    let mut last_response: Option<Response> = None;
    for attempt in 0..max_attempts {
        let attempt_label = attempt + 1;
        let (upstream_url, _guard) = match balancer.select(route) {
            Some(x) => x,
            None => {
                tracing::warn!(
                    request_id = %ctx.request_id,
                    attempt = attempt_label,
                    route = %route.name,
                    "无可用上游"
                );
                last_response =
                    Some(GatewayError::proxy(format!("路由 {} 无可用上游", route.name)).into_response());
                continue;
            }
        };
        if attempt == 0 {
            ctx = ctx.with_upstream(upstream_url.clone());
        }

        let effective_uri = rewrite::apply_rewrite(original_uri, &route.rewrite);
        let target_uri = match build_target_uri(&upstream_url, &effective_uri) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(
                    request_id = %ctx.request_id,
                    attempt = attempt_label,
                    error = %e,
                    "目标 URI 构造失败"
                );
                last_response =
                    Some(GatewayError::proxy_with_source("目标 URI 构造失败", e).into_response());
                continue;
            }
        };

        let mut headers = original_headers.clone();
        protocol::strip_hop_by_hop_headers(&mut headers);
        protocol::append_x_forwarded_for(&mut headers, client_addr, trust_client_xff);
        set_host_header(&mut headers, &upstream_url);

        let proxy_req = match Request::builder()
            .method(method.clone())
            .uri(target_uri)
            .body(Body::from(body_bytes.clone()))
        {
            Ok(mut r) => {
                *r.headers_mut() = headers;
                r
            }
            Err(e) => {
                tracing::error!(
                    request_id = %ctx.request_id,
                    attempt = attempt_label,
                    error = %e,
                    "构造代理请求失败"
                );
                last_response =
                    Some(GatewayError::proxy_with_source("构造代理请求失败", e).into_response());
                continue;
            }
        };

        let span = tracing::info_span!(
            "proxy_forward",
            request_id = %ctx.request_id,
            route = %ctx.route_name(),
            upstream = %upstream_url,
            method = %method,
            attempt = attempt_label,
        );
        let attempt_start = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs.max(1)),
            client.request(proxy_req),
        )
        .instrument(span)
        .await;

        match result {
            Ok(Ok(upstream_resp)) => {
                let status = upstream_resp.status();
                metrics::record_upstream_request(&upstream_url, status.as_u16(), attempt_start.elapsed());
                if retry::is_retryable_status(status, method) && attempt + 1 < max_attempts {
                    let _ = upstream_resp.into_body().collect().await;
                    metrics::record_upstream_retry(&route.name);
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        attempt = attempt_label,
                        status = status.as_u16(),
                        elapsed_ms = ctx.elapsed_millis(),
                        "上游返回可重试状态码，准备重试"
                    );
                    last_response = Some(
                        GatewayError::proxy(format!("上游返回 {status}")).into_response(),
                    );
                    continue;
                }
                tracing::info!(
                    request_id = %ctx.request_id,
                    status = status.as_u16(),
                    elapsed_ms = ctx.elapsed_millis(),
                    attempts = attempt_label,
                    "转发完成"
                );
                return convert_response(upstream_resp);
            }
            Ok(Err(e)) => {
                metrics::record_upstream_request(&upstream_url, 502, attempt_start.elapsed());
                if retry::is_retryable_error(&e, method) && attempt + 1 < max_attempts {
                    metrics::record_upstream_retry(&route.name);
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        attempt = attempt_label,
                        error = %e,
                        elapsed_ms = ctx.elapsed_millis(),
                        "转发失败，准备重试"
                    );
                    last_response =
                        Some(GatewayError::proxy_with_source("上游请求失败", e).into_response());
                    continue;
                }
                tracing::warn!(
                    request_id = %ctx.request_id,
                    error = %e,
                    elapsed_ms = ctx.elapsed_millis(),
                    "上游请求失败"
                );
                return GatewayError::proxy_with_source("上游请求失败", e).into_response();
            }
            Err(_) => {
                metrics::record_upstream_request(&upstream_url, 504, attempt_start.elapsed());
                if attempt + 1 < max_attempts {
                    metrics::record_upstream_retry(&route.name);
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        attempt = attempt_label,
                        timeout_secs,
                        elapsed_ms = ctx.elapsed_millis(),
                        "转发超时，准备重试"
                    );
                    last_response = Some(
                        GatewayError::Timeout(format!("上游请求超时({timeout_secs}s)")).into_response(),
                    );
                    continue;
                }
                tracing::warn!(
                    request_id = %ctx.request_id,
                    timeout_secs,
                    elapsed_ms = ctx.elapsed_millis(),
                    "上游请求超时"
                );
                return GatewayError::Timeout(format!("上游请求超时({timeout_secs}s)")).into_response();
            }
        }
    }

    tracing::warn!(
        request_id = %ctx.request_id,
        attempts = max_attempts,
        "重试耗尽"
    );
    last_response.unwrap_or_else(|| GatewayError::proxy("重试耗尽").into_response())
}

/// 流式模式转发：请求体直接透传，不支持重试（适用于 SSE/大文件/无重试路由）
#[allow(clippy::too_many_arguments)]
async fn forward_streaming(
    mut ctx: RequestContext,
    method: &http::Method,
    original_uri: &Uri,
    original_headers: &HeaderMap,
    body: axum::body::Body,
    route: &std::sync::Arc<crate::config::RouteConfig>,
    timeout_secs: u64,
    trust_client_xff: bool,
    client_addr: SocketAddr,
    client: &UpstreamClient,
    balancer: &Balancer,
) -> Response {
    let (upstream_url, _guard) = match balancer.select(route) {
        Some(x) => x,
        None => {
            tracing::warn!(
                request_id = %ctx.request_id,
                route = %route.name,
                "无可用上游"
            );
            return GatewayError::proxy(format!("路由 {} 无可用上游", route.name)).into_response();
        }
    };
    ctx = ctx.with_upstream(upstream_url.clone());

    let effective_uri = rewrite::apply_rewrite(original_uri, &route.rewrite);
    let target_uri = match build_target_uri(&upstream_url, &effective_uri) {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(
                request_id = %ctx.request_id,
                error = %e,
                "目标 URI 构造失败"
            );
            return GatewayError::proxy_with_source("目标 URI 构造失败", e).into_response();
        }
    };

    let mut headers = original_headers.clone();
    protocol::strip_hop_by_hop_headers(&mut headers);
    protocol::append_x_forwarded_for(&mut headers, client_addr, trust_client_xff);
    set_host_header(&mut headers, &upstream_url);

    let proxy_req = match Request::builder()
        .method(method.clone())
        .uri(target_uri)
        .body(body)
    {
        Ok(mut r) => {
            *r.headers_mut() = headers;
            r
        }
        Err(e) => {
            tracing::error!(
                request_id = %ctx.request_id,
                error = %e,
                "构造代理请求失败"
            );
            return GatewayError::proxy_with_source("构造代理请求失败", e).into_response();
        }
    };

    let span = tracing::info_span!(
        "proxy_forward",
        request_id = %ctx.request_id,
        route = %ctx.route_name(),
        upstream = %upstream_url,
        method = %method,
        attempt = 1u32,
    );
    let attempt_start = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs.max(1)),
        client.request(proxy_req),
    )
    .instrument(span)
    .await;

    match result {
        Ok(Ok(upstream_resp)) => {
            let status = upstream_resp.status();
            metrics::record_upstream_request(&upstream_url, status.as_u16(), attempt_start.elapsed());
            tracing::info!(
                request_id = %ctx.request_id,
                status = status.as_u16(),
                elapsed_ms = ctx.elapsed_millis(),
                "转发完成"
            );
            convert_response(upstream_resp)
        }
        Ok(Err(e)) => {
            metrics::record_upstream_request(&upstream_url, 502, attempt_start.elapsed());
            tracing::warn!(
                request_id = %ctx.request_id,
                error = %e,
                elapsed_ms = ctx.elapsed_millis(),
                "上游请求失败"
            );
            GatewayError::proxy_with_source("上游请求失败", e).into_response()
        }
        Err(_) => {
            metrics::record_upstream_request(&upstream_url, 504, attempt_start.elapsed());
            tracing::warn!(
                request_id = %ctx.request_id,
                timeout_secs,
                elapsed_ms = ctx.elapsed_millis(),
                "上游请求超时"
            );
            GatewayError::Timeout(format!("上游请求超时({timeout_secs}s)")).into_response()
        }
    }
}

/// 将上游响应转为 axum 响应（body 透传）
///
/// 使用泛型参数避免显式命名 hyper-util 的 Incoming body 类型。
fn convert_response<B>(resp: http::Response<B>) -> Response
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (parts, body) = resp.into_parts();
    let mut response = Response::new(Body::new(body));
    *response.status_mut() = parts.status;
    *response.headers_mut() = parts.headers;
    *response.version_mut() = parts.version;
    response
}

/// 构造目标 URI：保留上游 scheme://authority，路径与 query 取自原始请求
///
/// 使用 `Uri::builder()` 分字段构造，避免字符串拼接导致的编码问题：
/// - scheme / authority 取自上游地址
/// - path_and_query 取自原始请求（含改写后的路径）
///
/// 各字段独立解析校验，非法字段直接返回错误而非产生畸形 URI。
fn build_target_uri(upstream: &str, original: &Uri) -> Result<Uri> {
    let base: Uri = upstream.parse().map_err(|e: http::uri::InvalidUri| {
        GatewayError::proxy_with_source("上游地址解析失败", e)
    })?;
    let scheme = base
        .scheme()
        .cloned()
        .unwrap_or(http::uri::Scheme::HTTP);
    let authority = base.authority().ok_or_else(|| {
        GatewayError::proxy(format!("上游地址缺少 authority: {upstream}"))
    })?;

    // path_and_query 必须以 '/' 开头，否则构造的 URI 非法；
    // 原始请求缺失时回退到根路径 "/"
    let pq_str = original
        .path_and_query()
        .map(|pq| pq.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("/");
    let pq: String = if pq_str.starts_with('/') {
        pq_str.to_string()
    } else {
        // 路径不以 / 开头时补全，避免构造出相对引用
        format!("/{pq_str}")
    };

    http::uri::Builder::new()
        .scheme(scheme)
        .authority(authority.clone())
        .path_and_query(&pq[..])
        .build()
        .map_err(|e| GatewayError::proxy_with_source("目标 URI 构造失败", e))
}

/// 设置 Host 头为上游地址的 authority
fn set_host_header(headers: &mut HeaderMap, upstream: &str) {
    if let Ok(uri) = upstream.parse::<Uri>() {
        if let Some(auth) = uri.authority() {
            if let Ok(val) = http::HeaderValue::from_str(auth.as_str()) {
                headers.insert(http::header::HOST, val);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_target_uri_combines_base_and_path() {
        let original: Uri = "/api/users?id=1".parse().unwrap();
        let target = build_target_uri("http://127.0.0.1:9001", &original).unwrap();
        assert_eq!(target.to_string(), "http://127.0.0.1:9001/api/users?id=1");
    }

    #[test]
    fn build_target_uri_preserves_query() {
        let original: Uri = "/search?q=hello%20world&lang=en".parse().unwrap();
        let target = build_target_uri("https://api.example.com", &original).unwrap();
        assert_eq!(
            target.to_string(),
            "https://api.example.com/search?q=hello%20world&lang=en"
        );
    }

    #[test]
    fn build_target_uri_defaults_to_root() {
        let original: Uri = "/".parse().unwrap();
        let target = build_target_uri("http://127.0.0.1:9001", &original).unwrap();
        assert_eq!(target.to_string(), "http://127.0.0.1:9001/");
    }

    #[test]
    fn build_target_uri_rejects_missing_authority() {
        // http crate 的 URI 解析非常宽松：纯路径字符串会被解析为相对引用。
        // 这里用一个绝对不可能有 authority 的输入测试。
        let original: Uri = "/path".parse().unwrap();
        let result = build_target_uri("http://bad host", &original);
        assert!(result.is_err());
    }

    #[test]
    fn set_host_updates_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, "original.com".parse().unwrap());
        set_host_header(&mut headers, "http://127.0.0.1:9001");
        assert_eq!(headers.get(http::header::HOST).unwrap(), "127.0.0.1:9001");
    }

    #[test]
    fn build_target_uri_preserves_rewritten_path() {
        // 验证改写后的 URI（含新路径）能正确拼接上游
        let rewritten: Uri = "/api/v2/users?id=1".parse().unwrap();
        let target = build_target_uri("http://127.0.0.1:9001", &rewritten).unwrap();
        assert_eq!(target.to_string(), "http://127.0.0.1:9001/api/v2/users?id=1");
    }
}
