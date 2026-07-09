//! 跨域处理中间件
//!
//! 基于 tower-http 的 CorsLayer 实现，从配置构造跨域策略。
//! 配置热更新时，CORS 策略在下次 build_router 调用时重建（当前实现为启动时固定）。

use std::time::Duration;

use http::header::{HeaderName, HeaderValue};
use http::Method;
use tower_http::cors::{Any, CorsLayer};

use crate::config::CorsConfig;

/// 从配置构造 CORS 层，未启用时返回 None
pub fn build_cors_layer(cfg: &CorsConfig) -> Option<CorsLayer> {
    if !cfg.enable {
        return None;
    }

    let mut layer = CorsLayer::new();

    // 允许的源
    layer = if cfg.allowed_origins.is_empty() {
        layer.allow_origin(Any)
    } else {
        let origins: Vec<HeaderValue> = cfg
            .allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        if origins.is_empty() {
            layer.allow_origin(Any)
        } else {
            layer.allow_origin(origins)
        }
    };

    // 允许的方法
    layer = if cfg.allowed_methods.is_empty() {
        layer.allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::OPTIONS,
            Method::HEAD,
        ])
    } else {
        let methods: Vec<Method> = cfg
            .allowed_methods
            .iter()
            .filter_map(|m| m.parse().ok())
            .collect();
        layer.allow_methods(methods)
    };

    // 允许的请求头
    layer = if cfg.allowed_headers.is_empty() {
        layer.allow_headers(Any)
    } else {
        let headers: Vec<HeaderName> = cfg
            .allowed_headers
            .iter()
            .filter_map(|h| h.parse().ok())
            .collect();
        layer.allow_headers(headers)
    };

    // 暴露的响应头（默认不额外暴露）
    layer = layer.expose_headers(Any);

    // 凭证
    if cfg.allow_credentials {
        layer = layer.allow_credentials(true);
    }

    // 预检缓存
    layer = layer.max_age(Duration::from_secs(cfg.max_age_secs));

    Some(layer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_cors_returns_none() {
        let cfg = CorsConfig::default();
        assert!(build_cors_layer(&cfg).is_none());
    }

    #[test]
    fn enabled_cors_returns_layer() {
        let mut cfg = CorsConfig::default();
        cfg.enable = true;
        assert!(build_cors_layer(&cfg).is_some());
    }

    #[test]
    fn cors_with_specific_origins() {
        let mut cfg = CorsConfig::default();
        cfg.enable = true;
        cfg.allowed_origins = vec!["https://example.com".into(), "https://api.example.com".into()];
        assert!(build_cors_layer(&cfg).is_some());
    }
}
