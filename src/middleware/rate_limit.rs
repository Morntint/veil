//! 限流中间件
//!
//! 基于 governor 实现的按客户端 IP 限流（GCRA/令牌桶算法）。
//! 配置热更新：当限流参数变化时，自动重建限流器（per-IP 状态会重置）。
//! 限流策略：每秒补充令牌数 = rate_limit_per_second，桶容量 = rate_limit_burst。

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::{
    clock::DefaultClock,
    state::keyed::DefaultKeyedStateStore,
    Quota, RateLimiter,
};
use http::Request;
use parking_lot::RwLock;

use crate::config::SecurityConfig;
use crate::monitor::metrics;
use crate::network::server::AppState;
use crate::utils::GatewayError;

/// 按键（IP）限流的 governor 限流器类型
pub type KeyedRateLimiter =
    RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>;

/// 限流管理器：包装限流器，支持配置热更新时重建
pub struct RateLimitManager {
    inner: RwLock<Arc<KeyedRateLimiter>>,
    signature: RwLock<String>,
}

impl Default for RateLimitManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitManager {
    pub fn new() -> Self {
        // 默认占位：每秒 1 请求（启用后立即被实际配置覆盖）
        let quota = Quota::per_second(NonZeroU32::new(1).unwrap());
        let limiter = Arc::new(RateLimiter::keyed(quota));
        Self {
            inner: RwLock::new(limiter),
            signature: RwLock::new(String::new()),
        }
    }

    /// 根据配置同步限流器：参数变化时重建（per-IP 状态重置）
    pub fn sync(&self, cfg: &SecurityConfig) -> Arc<KeyedRateLimiter> {
        let sig = format!(
            "{}|{}|{}",
            cfg.enable_rate_limit, cfg.rate_limit_per_second, cfg.rate_limit_burst
        );
        if *self.signature.read() == sig {
            return self.inner.read().clone();
        }
        let per_sec = NonZeroU32::new(cfg.rate_limit_per_second.max(1)).unwrap();
        let burst = NonZeroU32::new(cfg.rate_limit_burst.max(1)).unwrap();
        let quota = Quota::per_second(per_sec).allow_burst(burst);
        let new_limiter = Arc::new(RateLimiter::keyed(quota));
        *self.inner.write() = new_limiter.clone();
        *self.signature.write() = sig;
        new_limiter
    }
}

/// 限流中间件：按客户端 IP 进行令牌桶限流
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let security = {
        let cfg = state.config.read();
        cfg.security.clone()
    };

    if !security.enable_rate_limit {
        return next.run(req).await;
    }

    let limiter = state.rate_limiter.sync(&security);
    let ip = addr.ip();

    match limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => {
            metrics::record_rate_limit(&ip.to_string());
            tracing::warn!(client_ip = %ip, "限流触发");
            GatewayError::RateLimit(format!("请求过于频繁，客户端 IP: {ip}")).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_sync_rebuilds_on_config_change() {
        let mgr = RateLimitManager::new();
        let cfg1 = SecurityConfig {
            enable_rate_limit: true,
            rate_limit_per_second: 100,
            rate_limit_burst: 200,
            ..Default::default()
        };
        let l1 = mgr.sync(&cfg1);
        // 相同配置，返回同一实例
        let l2 = mgr.sync(&cfg1);
        assert!(Arc::ptr_eq(&l1, &l2));
        // 配置变化，重建
        let cfg2 = SecurityConfig {
            enable_rate_limit: true,
            rate_limit_per_second: 200,
            rate_limit_burst: 400,
            ..Default::default()
        };
        let l3 = mgr.sync(&cfg2);
        assert!(!Arc::ptr_eq(&l1, &l3));
    }

    #[test]
    fn manager_default_does_not_panic() {
        let mgr = RateLimitManager::new();
        let cfg = SecurityConfig::default();
        let _limiter = mgr.sync(&cfg);
    }
}
