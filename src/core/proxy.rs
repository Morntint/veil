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
use http::{HeaderMap, Method, Request, Uri};
use tracing::Instrument;

use crate::config::SharedConfig;
use crate::core::{balancer::Balancer, context::RequestContext, router};
use crate::middleware::{retry, rewrite};
use crate::monitor::metrics;
use crate::network::connection::UpstreamClient;
use crate::network::protocol;
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
    let path = original_uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| original_uri.path().to_string());
    let path_only = original_uri.path().to_string();

    let mut ctx = RequestContext::new(client_addr, method.clone(), path.clone());

    // 1. 路由匹配（读取 SharedConfig，实时反映热更新）
    let route = {
        let cfg = config.read();
        router::match_route(&cfg.routes, &path_only)
    };
    let route = match route {
        Some(r) => r,
        None => {
            tracing::info!(
                request_id = %ctx.request_id,
                method = %method,
                path = %path,
                "未匹配到路由"
            );
            return GatewayError::Route(format!("未匹配路由: {path}")).into_response();
        }
    };
    ctx = ctx.with_route(route.clone());

    // 2. 缓冲请求体（为重试做准备，Bytes 克隆为零拷贝引用计数）
    let (body_limit, proxy_timeout_secs) = {
        let cfg = config.read();
        (cfg.network.request_size_limit_bytes, cfg.proxy.timeout_secs)
    };
    let original_headers = req.headers().clone();
    let body_bytes = match to_bytes(req.into_body(), body_limit).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(request_id = %ctx.request_id, error = %e, "读取请求体失败");
            return GatewayError::PayloadTooLarge(format!("读取请求体失败: {e}")).into_response();
        }
    };

    // 3. 解析超时与重试次数：路由级优先，否则用全局配置
    let timeout_secs = if route.timeout_secs > 0 {
        route.timeout_secs
    } else {
        proxy_timeout_secs
    };
    let max_attempts = route.retries.saturating_add(1).max(1);

    // 4. 重试循环：每次重新选择上游，避免持续命中同一故障节点
    let mut last_response: Option<Response> = None;
    for attempt in 0..max_attempts {
        let attempt_label = attempt + 1;
        let (upstream_url, _guard) = match balancer.select(&route) {
            Some(x) => x,
            None => {
                tracing::warn!(
                    request_id = %ctx.request_id,
                    attempt = attempt_label,
                    route = %route.name,
                    "无可用上游"
                );
                last_response =
                    Some(GatewayError::Proxy(format!("路由 {} 无可用上游", route.name)).into_response());
                continue;
            }
        };
        if attempt == 0 {
            ctx = ctx.with_upstream(upstream_url.clone());
        }

        // 5. 路径改写（路由级，未启用则原样返回）
        let effective_uri = rewrite::apply_rewrite(&original_uri, &route.rewrite);

        // 6. 构造目标 URI（上游 authority + 改写后 path_and_query）
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
                    Some(GatewayError::Proxy(format!("目标 URI 构造失败: {e}")).into_response());
                continue;
            }
        };

        // 7. 构造代理请求：克隆头 → 处理逐跳头/XFF/Host → 组装
        let mut headers = original_headers.clone();
        protocol::strip_hop_by_hop_headers(&mut headers);
        protocol::append_x_forwarded_for(&mut headers, client_addr);
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
                    Some(GatewayError::Proxy(format!("构造代理请求失败: {e}")).into_response());
                continue;
            }
        };

        // 8. 超时管控转发（带 tracing span 关联全链路）
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
                // 记录上游转发指标
                metrics::record_upstream_request(
                    &upstream_url,
                    status.as_u16(),
                    attempt_start.elapsed(),
                );
                // 5xx + 幂等方法 + 仍有重试次数 → 重试
                if retry::is_retryable_status(status, &method) && attempt + 1 < max_attempts {
                    metrics::record_upstream_retry(&route.name);
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        attempt = attempt_label,
                        status = status.as_u16(),
                        elapsed_ms = ctx.elapsed_millis(),
                        "上游返回可重试状态码，准备重试"
                    );
                    last_response = Some(convert_response(upstream_resp));
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
                // 连接错误记录上游指标（状态码 502 表示网关错误）
                metrics::record_upstream_request(
                    &upstream_url,
                    502,
                    attempt_start.elapsed(),
                );
                // 连接错误/超时 + 仍有重试次数 → 重试
                if retry::is_retryable_error(&e) && attempt + 1 < max_attempts {
                    metrics::record_upstream_retry(&route.name);
                    tracing::warn!(
                        request_id = %ctx.request_id,
                        attempt = attempt_label,
                        error = %e,
                        elapsed_ms = ctx.elapsed_millis(),
                        "转发失败，准备重试"
                    );
                    last_response =
                        Some(GatewayError::Proxy(format!("上游请求失败: {e}")).into_response());
                    continue;
                }
                tracing::warn!(
                    request_id = %ctx.request_id,
                    error = %e,
                    elapsed_ms = ctx.elapsed_millis(),
                    "上游请求失败"
                );
                return GatewayError::Proxy(format!("上游请求失败: {e}")).into_response();
            }
            Err(_) => {
                // 超时记录上游指标（状态码 504 表示网关超时）
                metrics::record_upstream_request(
                    &upstream_url,
                    504,
                    attempt_start.elapsed(),
                );
                // 超时 + 仍有重试次数 → 重试
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

    // 重试耗尽，返回最后一次错误响应
    tracing::warn!(
        request_id = %ctx.request_id,
        attempts = max_attempts,
        "重试耗尽"
    );
    last_response.unwrap_or_else(|| GatewayError::Proxy("重试耗尽".into()).into_response())
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
fn build_target_uri(upstream: &str, original: &Uri) -> Result<Uri> {
    let base: Uri = upstream.parse().map_err(|e: http::uri::InvalidUri| {
        GatewayError::Proxy(format!("上游地址解析失败: {e}"))
    })?;
    let scheme = base.scheme_str().unwrap_or("http");
    let authority = base
        .authority()
        .ok_or_else(|| GatewayError::Proxy(format!("上游地址缺少 authority: {upstream}")))?;
    let path_and_query = original
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let target = format!("{scheme}://{authority}{path_and_query}");
    target
        .parse()
        .map_err(|e: http::uri::InvalidUri| GatewayError::Proxy(format!("目标 URI 解析失败: {e}")))
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

// 避免未使用导入警告（Method 在泛型约束中间接使用）
#[allow(dead_code)]
fn _ensure_method_in_scope(_m: Method) {}

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
