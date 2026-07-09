//! 限流中间件
//!
//! 基于 governor 实现的按客户端 IP 限流（GCRA/令牌桶算法）。
//! 配置热更新：当限流参数变化时，自动重建限流器（per-IP 状态会重置）。
//! 限流策略：每秒补充令牌数 = rate_limit_per_second，桶容量 = rate_limit_burst。
//!
//! GC：后台周期任务调用 `retain_recent` 清理已恢复满额的 per-IP 状态，防止
//! 高基 IP 场景（如大量短连客户端/伪造源 IP）导致内存无限增长。

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

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
use parking_lot::Mutex;

use crate::config::SecurityConfig;
use crate::monitor::metrics;
use crate::network::protocol::{parse_trusted_cidrs, resolve_real_client_ip};
use crate::network::server::AppState;
use crate::utils::GatewayError;

/// 按键（IP）限流的 governor 限流器类型
pub type KeyedRateLimiter =
    RateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>;

/// 限流器内部状态：limiter 与签名同属一把锁，避免 TOCTOU 导致重复重建
struct Inner {
    limiter: Arc<KeyedRateLimiter>,
    signature: String,
}

/// 限流管理器：包装限流器，支持配置热更新时重建
pub struct RateLimitManager {
    inner: Mutex<Inner>,
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
            inner: Mutex::new(Inner {
                limiter,
                signature: String::new(),
            }),
        }
    }

    /// 根据配置同步限流器：参数变化时重建（per-IP 状态重置）
    ///
    /// 单把锁保证检查与替换原子进行，消除并发下重复重建/per-IP 计数被清零两次的竞态。
    pub fn sync(&self, cfg: &SecurityConfig) -> Arc<KeyedRateLimiter> {
        let sig = format!(
            "{}|{}|{}",
            cfg.enable_rate_limit, cfg.rate_limit_per_second, cfg.rate_limit_burst
        );
        let mut inner = self.inner.lock();
        if inner.signature == sig {
            return inner.limiter.clone();
        }
        let per_sec = NonZeroU32::new(cfg.rate_limit_per_second.max(1)).unwrap();
        let burst = NonZeroU32::new(cfg.rate_limit_burst.max(1)).unwrap();
        let quota = Quota::per_second(per_sec).allow_burst(burst);
        let new_limiter = Arc::new(RateLimiter::keyed(quota));
        inner.limiter = new_limiter.clone();
        inner.signature = sig;
        new_limiter
    }

    /// GC：清理 per-IP 状态中已恢复满额（与初始状态不可区分）的条目
    ///
    /// governor 的 `retain_recent` 会移除理论到达时间已落在过去的 key，
    /// 即超过桶容量恢复时长的 IP。这在不影响有效限流的前提下回收内存。
    /// 返回清理前的 key 数量，便于运维观测。
    pub fn gc(&self) -> usize {
        let limiter = {
            let inner = self.inner.lock();
            inner.limiter.clone()
        };
        let before = limiter.len();
        limiter.retain_recent();
        limiter.shrink_to_fit();
        let after = limiter.len();
        if before > after {
            tracing::debug!(before, after, evicted = before - after, "限流器 GC 完成");
        }
        before
    }
}

/// 启动限流器 GC 后台任务：每隔 `interval` 周期清理一次 per-IP 状态
///
/// 应在服务启动时调用一次，任务随运行时生命周期存活。
pub fn spawn_gc_task(mgr: Arc<RateLimitManager>, interval: Duration) {
    if interval.is_zero() {
        tracing::warn!("rate_limit_gc_secs 为 0，跳过限流器 GC 任务");
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // 首次 tick 立即返回（跳过，避免启动时无谓扫描）
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let before = mgr.gc();
            if before > 0 {
                tracing::trace!(keys = before, "限流器周期 GC 执行");
            }
        }
    });
}

/// 限流中间件：按客户端 IP 进行令牌桶限流
///
/// 受信代理场景下（`security.trusted_proxy_cidrs` 非空且 peer 落在受信 CIDR 内），
/// 从 X-Forwarded-For 回溯真实客户端 IP 进行限流，避免 ALB/Ingress 单一 peer IP
/// 触发误限流。
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let security = {
        let cfg = state.config.load_full();
        cfg.security.clone()
    };

    if !security.enable_rate_limit {
        return next.run(req).await;
    }

    let limiter = state.rate_limiter.sync(&security);

    // 真实客户端 IP 解析：peer 不在受信 CIDR 内时直接使用 peer
    let peer_ip = addr.ip();
    let xff = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok());
    let trusted = parse_trusted_cidrs(&security.trusted_proxy_cidrs);
    let ip = resolve_real_client_ip(peer_ip, xff, &trusted);

    match limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => {
            metrics::record_rate_limit(&ip.to_string());
            tracing::debug!(client_ip = %ip, peer_ip = %peer_ip, "限流触发");
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

    #[test]
    fn gc_runs_without_panic_on_empty_limiter() {
        let mgr = RateLimitManager::new();
        let cfg = SecurityConfig::default();
        mgr.sync(&cfg);
        // 空限流器执行 GC 不应 panic，返回 0
        let before = mgr.gc();
        assert_eq!(before, 0);
    }

    #[test]
    fn gc_evicts_refreshed_state_after_window() {
        use std::time::Duration;
        let mgr = RateLimitManager::new();
        let cfg = SecurityConfig {
            enable_rate_limit: true,
            rate_limit_per_second: 1,
            rate_limit_burst: 1,
            ..Default::default()
        };
        let limiter = mgr.sync(&cfg);
        let ip: IpAddr = "203.0.113.10".parse().unwrap();
        // 触发一次限流，写入 per-IP 状态
        assert!(limiter.check_key(&ip).is_ok());
        assert_eq!(limiter.len(), 1);
        // 等待桶恢复（>2 秒保证 1/s 速率的桶已回满）
        std::thread::sleep(Duration::from_secs(3));
        let before = mgr.gc();
        assert_eq!(before, 1);
        // GC 后状态应被回收（retain_recent 移除已恢复满额的 key）
        assert_eq!(limiter.len(), 0);
    }
}
