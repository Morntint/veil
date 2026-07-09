//! 健康检查与服务探活
//!
//! 提供网关健康状态查询：服务运行状态、启动时长、配置版本号、路由数量。
//! 探活接口供 Kubernetes liveness/readiness 探针或负载均衡器调用。

use std::time::Instant;

use once_cell::sync::Lazy;
use serde_json::json;

use crate::config::SharedConfig;

/// 网关启动时间点（进程级，首次访问时记录）
static STARTED_AT: Lazy<Instant> = Lazy::new(Instant::now);

/// 健康状态码
pub const STATUS_OK: &str = "ok";
pub const STATUS_DEGRADED: &str = "degraded";

/// 构建健康检查响应（JSON）
///
/// 返回网关运行状态、版本、启动时长（秒）、配置版本、路由数量。
pub fn health_json(config: &SharedConfig) -> serde_json::Value {
    let (version, env, routes_count, cfg_version) = {
        let c = config.read();
        (
            crate::constant::VERSION,
            c.env.clone(),
            c.routes.len(),
            c.version,
        )
    };

    let uptime_secs = STARTED_AT.elapsed().as_secs();

    json!({
        "status": STATUS_OK,
        "version": version,
        "env": env,
        "uptime_secs": uptime_secs,
        "config_version": cfg_version,
        "routes_count": routes_count,
    })
}

/// 简单存活检查（仅返回状态字符串，供轻量探针使用）
pub fn liveness() -> &'static str {
    STATUS_OK
}

/// 获取网关启动时间（用于运维状态展示）
pub fn started_at() -> Instant {
    *STARTED_AT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use std::sync::Arc;
    use parking_lot::RwLock;

    #[test]
    fn health_json_contains_required_fields() {
        let cfg: SharedConfig = Arc::new(RwLock::new(AppConfig::default()));
        let json = health_json(&cfg);
        assert_eq!(json["status"], "ok");
        assert!(json["uptime_secs"].as_u64().is_some());
        assert!(json["config_version"].as_u64().is_some());
        assert!(json["routes_count"].as_u64().is_some());
    }

    #[test]
    fn liveness_returns_ok() {
        assert_eq!(liveness(), "ok");
    }
}
