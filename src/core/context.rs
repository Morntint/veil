//! 请求上下文封装：全链路信息透传
//!
//! 在一次网关转发中，请求上下文贯穿：路由匹配 → 负载均衡 → 反向代理 → 响应回写，
//! 承载请求ID、客户端IP、命中路由、选中上游、起始时间等关键信息，供日志、
//! 监控、链路追踪统一引用，避免在各层之间重复传递散落参数。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use http::Method;

use crate::config::RouteConfig;

/// 请求上下文：一次客户端请求的全链路状态
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// 请求唯一ID（用于日志关联与链路追踪）
    pub request_id: String,
    /// 客户端原始地址
    pub client_addr: SocketAddr,
    /// HTTP 方法
    pub method: Method,
    /// 原始请求路径（含 query）
    pub path: String,
    /// 命中的路由（None 表示未匹配到任何路由）
    pub route: Option<Arc<RouteConfig>>,
    /// 选中的上游地址（负载均衡后确定）
    pub upstream: Option<String>,
    /// 请求起始时间（用于耗时统计）
    pub started_at: Instant,
}

impl RequestContext {
    /// 创建一个新的请求上下文，自动生成 request_id 并记录起始时间
    pub fn new(client_addr: SocketAddr, method: Method, path: String) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            client_addr,
            method,
            path,
            route: None,
            upstream: None,
            started_at: Instant::now(),
        }
    }

    /// 绑定命中的路由
    pub fn with_route(mut self, route: Arc<RouteConfig>) -> Self {
        self.route = Some(route);
        self
    }

    /// 绑定选中的上游地址
    pub fn with_upstream(mut self, upstream: String) -> Self {
        self.upstream = Some(upstream);
        self
    }

    /// 已经过去的时长（毫秒）
    pub fn elapsed_millis(&self) -> u128 {
        self.started_at.elapsed().as_millis()
    }

    /// 命中路由名称（未命中返回 "none"）
    pub fn route_name(&self) -> &str {
        self.route
            .as_ref()
            .map(|r| r.name.as_str())
            .unwrap_or("none")
    }

    /// 选中上游（未确定返回 "none"）
    pub fn upstream_str(&self) -> &str {
        self.upstream.as_deref().unwrap_or("none")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_tracks_route_and_upstream() {
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let mut ctx = RequestContext::new(addr, Method::GET, "/api/users".into());
        assert!(ctx.route.is_none());
        assert!(ctx.upstream.is_none());

        let route = Arc::new(RouteConfig::default());
        ctx = ctx.with_route(route).with_upstream("http://127.0.0.1:9001".into());
        assert_eq!(ctx.route_name(), "");
        assert_eq!(ctx.upstream_str(), "http://127.0.0.1:9001");
        assert!(ctx.elapsed_millis() < 1000);
        assert!(!ctx.request_id.is_empty());
    }

    #[test]
    fn context_handles_ipv6_addr() {
        let addr: SocketAddr = "[::1]:5678".parse().unwrap();
        let ctx = RequestContext::new(addr, Method::POST, "/v1/data".into());
        assert_eq!(ctx.client_addr.ip().to_string(), "::1");
    }
}
