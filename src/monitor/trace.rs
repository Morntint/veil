//! 链路追踪辅助模块
//!
//! 基于 tracing crate 实现轻量化全链路追踪：请求级 span 贯穿路由匹配 →
//! 负载均衡 → 反向代理 → 响应回写，配合 request_id 实现日志关联与全流程溯源。
//!
//! 设计理念：遵循方案"轻量化企业级"定位，不引入 opentelemetry 重型依赖，
//! 复用已有的 tracing + tracing-subscriber 基础设施。tracing 的 span 树
//! 本身即为分布式追踪的数据模型，可通过 tracing-subscriber 的 JSON 输出
//! 对接 Jaeger/Tempo 等后端。如需完整 W3C TraceContext 传播与 OTLP 导出，
//! 可在此模块扩展 opentelemetry-otlp，无需改动业务代码。
//!
//! 核心能力：
//! - 请求级 span（含 request_id、method、path、client_ip）
//! - 代理转发 span（含 route、upstream、attempt、status、elapsed_ms）
//! - 与日志共享同一 subscriber，日志自动携带当前 span 上下文

use std::net::SocketAddr;

use http::Method;

/// 创建请求级追踪 span，贯穿整个请求生命周期
///
/// 在请求入口创建，所有子 span（代理转发、中间件处理）自动成为其子节点，
/// 形成完整调用树。request_id 用于跨日志/指标关联。
#[macro_export]
macro_rules! request_span {
    ($request_id:expr, $method:expr, $path:expr, $client_ip:expr $(,)?) => {
        tracing::info_span!(
            "request",
            request_id = %$request_id,
            method = %$method,
            path = %$path,
            client_ip = %$client_ip,
        )
    };
}

/// 创建代理转发 span，记录单次上游转发 attempts
///
/// 由 proxy::forward 在每次重试时创建，父 span 为请求级 span。
#[macro_export]
macro_rules! proxy_span {
    ($request_id:expr, $route:expr, $upstream:expr, $method:expr, $attempt:expr $(,)?) => {
        tracing::info_span!(
            "proxy_forward",
            request_id = %$request_id,
            route = %$route,
            upstream = %$upstream,
            method = %$method,
            attempt = $attempt,
        )
    };
}

/// 记录请求开始（用于追踪入口埋点）
pub fn log_request_start(request_id: &str, method: &Method, path: &str, client_ip: SocketAddr) {
    tracing::info!(
        request_id = %request_id,
        method = %method,
        path = %path,
        client_ip = %client_ip,
        "请求开始处理"
    );
}

/// 记录请求结束（用于追踪出口埋点）
pub fn log_request_end(
    request_id: &str,
    status: u16,
    elapsed_ms: u128,
    route: &str,
    upstream: &str,
) {
    tracing::info!(
        request_id = %request_id,
        status = status,
        elapsed_ms = elapsed_ms,
        route = %route,
        upstream = %upstream,
        "请求处理完成"
    );
}

/// 从请求头中提取或生成 request_id
///
/// 优先使用上游传入的 X-Request-Id，便于跨服务链路关联；
/// 无则生成 UUID v4。
pub fn resolve_request_id(headers: &http::HeaderMap) -> String {
    if let Some(val) = headers.get("x-request-id") {
        if let Ok(s) = val.to_str() {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    #[test]
    fn resolve_id_generates_uuid_when_missing() {
        let headers = HeaderMap::new();
        let id = resolve_request_id(&headers);
        assert!(!id.is_empty());
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn resolve_id_uses_header_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "trace-abc-123".parse().unwrap());
        let id = resolve_request_id(&headers);
        assert_eq!(id, "trace-abc-123");
    }

    #[test]
    fn resolve_id_ignores_empty_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "".parse().unwrap());
        let id = resolve_request_id(&headers);
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }
}
