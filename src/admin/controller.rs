//! 运维管理控制器：配置查询、状态查询、配置重载
//!
//! 供 admin::api 模块挂载为路由处理器，所有运维接口共享 AppState。
//! 接口设计：只读接口用 GET，变更类接口用 POST，返回统一 JSON 结构。

use axum::extract::State;
use axum::Json;
use once_cell::sync::Lazy;
use serde_json::{json, Value};

use crate::monitor::{health, metrics};
use crate::network::server::AppState;

/// 返回当前生效配置（含版本号，可用于验证热更新）
///
/// 安全：auth.token 在序列化后做掩码处理，避免通过管理接口泄露密钥。
pub async fn get_config(State(state): State<AppState>) -> Json<Value> {
    let json = {
        let cfg = state.config.load_full();
        serde_json::to_value(&*cfg)
    };
    let mut json = json.unwrap_or_else(|_| json!({"error": "配置序列化失败"}));
    // 掩码 auth.token：非空时仅保留首末各 1 字符，中间用 *** 代替
    if let Some(token) = json
        .pointer_mut("/auth/token")
        .and_then(|v| v.as_str())
    {
        let masked = if token.len() <= 2 {
            "***".to_string()
        } else {
            format!("{}***{}", &token[..1], &token[token.len() - 1..])
        };
        if let Some(obj) = json.pointer_mut("/auth") {
            if let Some(map) = obj.as_object_mut() {
                map.insert("token".into(), json!(masked));
            }
        }
    }
    Json(json)
}

/// 返回网关运行状态（健康、版本、启动时长、配置版本、路由数量）
pub async fn get_status(State(state): State<AppState>) -> Json<Value> {
    Json(health::health_json(&state.config))
}

/// 返回路由列表摘要（名称、匹配规则、上游、负载均衡策略）
pub async fn get_routes(State(state): State<AppState>) -> Json<Value> {
    let routes = {
        let cfg = state.config.load_full();
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
///
/// 速率限制：两次手动重载间至少间隔 5 秒，防止误用导致频繁抖动。
static LAST_RELOAD: Lazy<parking_lot::Mutex<Option<std::time::Instant>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
const RELOAD_MIN_INTERVAL_SECS: u64 = 5;

pub async fn reload_config(State(state): State<AppState>) -> Json<Value> {
    // 速率限制：防止频繁手动重载导致服务抖动
    {
        let mut last = LAST_RELOAD.lock();
        let now = std::time::Instant::now();
        if let Some(t) = *last {
            let elapsed = now.duration_since(t).as_secs();
            if elapsed < RELOAD_MIN_INTERVAL_SECS {
                return Json(json!({
                    "success": false,
                    "message": format!("手动重载过于频繁，请 {} 秒后重试", RELOAD_MIN_INTERVAL_SECS - elapsed),
                }));
            }
        }
        *last = Some(now);
    }

    // config_dir 与 env 来自 AppState（启动时注入），不再依赖环境变量
    match crate::config::loader::load(&state.config_dir, &state.env) {
        Ok(mut new_cfg) => {
            let old = state.config.load_full();
            new_cfg.version = old.version + 1;
            let new_version = new_cfg.version;
            state.config.store(std::sync::Arc::new(new_cfg));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, SharedConfig};
    use arc_swap::ArcSwap;
    use std::sync::Arc;

    #[tokio::test]
    async fn get_config_masks_token() {
        let mut cfg = AppConfig::default();
        cfg.auth.enable = true;
        cfg.auth.token = "super-secret-token-123456".into();
        let shared: SharedConfig = Arc::new(ArcSwap::from_pointee(cfg));
        let state = AppState::from_config(shared);
        let Json(val) = get_config(State(state)).await;
        let token = val.pointer("/auth/token").and_then(|v| v.as_str()).unwrap();
        assert!(token.contains("***"));
        assert!(!token.contains("secret"));
        assert!(!token.contains("123456"));
    }
}
