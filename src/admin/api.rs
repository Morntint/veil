//! 运维管理 API 路由
//!
//! 挂载于配置指定的 admin 前缀下（默认 /_admin），提供配置查询、状态查询、
//! 路由列表、指标导出、手动重载等运维接口。所有接口跳过鉴权（由 auth
//! 中间件的 skip_paths 配置保证）。
//!
//! 路由清单：
//! - GET  /_admin/config   当前生效配置
//! - GET  /_admin/status   网关运行状态
//! - GET  /_admin/routes   路由列表摘要
//! - POST /_admin/reload   手动触发配置重载
//!
//! /metrics 端点单独挂载在根路径（便于 Prometheus 抓取）。

use axum::routing::{get, post};
use axum::Router;

use crate::admin::{controller, dashboard};
use crate::network::server::AppState;

/// 构建运维管理子路由
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/dashboard", get(dashboard::dashboard_handler))
        .route("/config", get(controller::get_config))
        .route("/status", get(controller::get_status))
        .route("/routes", get(controller::get_routes))
        .route("/reload", post(controller::reload_config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, SharedConfig};
    use arc_swap::ArcSwap;
    use std::sync::Arc;

    #[test]
    fn admin_router_builds_without_error() {
        let cfg: SharedConfig = Arc::new(ArcSwap::from_pointee(AppConfig::default()));
        let state = AppState::from_config(cfg);
        let _router: Router = admin_router().with_state(state);
    }
}
