//! 运维管理控制器：配置查询、状态查询、配置重载
//!
//! 供 admin::api 模块挂载为路由处理器，所有运维接口共享 AppState。
//! 接口设计：只读接口用 GET，变更类接口用 POST，返回统一 JSON 结构。

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::config::SharedConfig;
use crate::monitor::{health, metrics};
use crate::network::server::AppState;

/// 返回当前生效配置（含版本号，可用于验证热更新）
pub async fn get_config(State(state): State<AppState>) -> Json<Value> {
    let json = {
        let cfg = state.config.read();
        serde_json::to_value(&*cfg)
    };
    Json(json.unwrap_or_else(|_| json!({"error": "配置序列化失败"})))
}

/// 返回网关运行状态（健康、版本、启动时长、配置版本、路由数量）
pub async fn get_status(State(state): State<AppState>) -> Json<Value> {
    Json(health::health_json(&state.config))
}

/// 返回路由列表摘要（名称、匹配规则、上游、负载均衡策略）
pub async fn get_routes(State(state): State<AppState>) -> Json<Value> {
    let routes = {
        let cfg = state.config.read();
        cfg.routes
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "match_type": r.r#match.match_type,
                    "path": r.r#match.path,
                    "upstream": r.upstream,
                    "load_balance": r.load_balance,
                    "retries": r.retries,
                    "timeout_secs": r.timeout_secs,
                    "rewrite_enabled": r.rewrite.enable,
                })
            })
            .collect::<Vec<_>>()
    };
    Json(json!({ "routes": routes }))
}

/// 返回 Prometheus 指标文本
pub async fn get_metrics() -> String {
    metrics::render()
}

/// 触发配置重载（从配置目录重新加载）
///
/// 注意：配置热更新由 watcher 自动监听，此接口提供手动触发能力。
/// 重载失败时返回错误信息，但不影响现有配置（兜底回滚）。
pub async fn reload_config(State(state): State<AppState>) -> Json<Value> {
    // watcher 已在后台自动重载，此处提供手动触发入口
    // 重新加载需知道 config_dir 和 env，暂从环境变量读取
    let config_dir = std::env::var(crate::constant::env_keys::CONFIG_DIR)
        .unwrap_or_else(|_| crate::constant::DEFAULT_CONFIG_DIR.to_string());
    let env = std::env::var(crate::constant::env_keys::ENV)
        .unwrap_or_else(|_| crate::constant::DEFAULT_ENV.to_string());

    match crate::config::loader::load(&config_dir, &env) {
        Ok(mut new_cfg) => {
            let new_version = {
                let mut guard = state.config.write();
                new_cfg.version = guard.version + 1;
                *guard = new_cfg;
                guard.version
            };
            tracing::info!(version = new_version, "手动触发配置重载成功");
            Json(json!({
                "success": true,
                "message": "配置重载成功",
                "config_version": new_version,
            }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "手动触发配置重载失败，保留旧配置");
            Json(json!({
                "success": false,
                "message": format!("配置重载失败: {e}"),
            }))
        }
    }
}

/// 重新加载配置的共享配置引用（供外部调用）
pub fn current_config(config: &SharedConfig) -> Value {
    let cfg = config.read();
    serde_json::to_value(&*cfg).unwrap_or_else(|_| json!({"error": "序列化失败"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use parking_lot::RwLock;
    use std::sync::Arc;

    #[test]
    fn current_config_serializes() {
        let cfg: SharedConfig = Arc::new(RwLock::new(AppConfig::default()));
        let json = current_config(&cfg);
        assert!(json.get("server").is_some());
    }
}
