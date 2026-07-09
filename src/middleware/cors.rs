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
///
/// 安全检查：allow_credentials=true 时拒绝 Any 源（tower-http 会 panic），
/// 此时必须显式指定 allowed_origins，否则返回 None 表示配置非法。
pub fn build_cors_layer(cfg: &CorsConfig) -> Option<CorsLayer> {
    if !cfg.enable {
        return None;
    }

    // allow_credentials=true 且未指定具体 origins → 拒绝构建（tower-http 会 panic）
    if cfg.allow_credentials && cfg.allowed_origins.is_empty() {
        tracing::error!(
            "CORS 配置非法：allow_credentials=true 时必须显式指定 allowed_origins，不能使用通配符 Any"
        );
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
            // 指定了 origins 但全部解析失败 → 回退到 Any（allow_credentials 已在上面拦截）
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
        let cfg = CorsConfig { enable: true, ..Default::default() };
        assert!(build_cors_layer(&cfg).is_some());
    }

    #[test]
    fn cors_with_specific_origins() {
        let cfg = CorsConfig {
            enable: true,
            allowed_origins: vec!["https://example.com".into(), "https://api.example.com".into()],
            ..Default::default()
        };
        assert!(build_cors_layer(&cfg).is_some());
    }

    #[test]
    fn cors_credentials_with_any_origin_returns_none() {
        // allow_credentials=true + 无 origins → 必须拒绝构建（tower-http 会 panic）
        let cfg = CorsConfig {
            enable: true,
            allow_credentials: true,
            allowed_origins: vec![],
            ..Default::default()
        };
        assert!(build_cors_layer(&cfg).is_none());
    }

    #[test]
    fn cors_credentials_with_specific_origins_ok() {
        let cfg = CorsConfig {
            enable: true,
            allow_credentials: true,
            allowed_origins: vec!["https://example.com".into()],
            ..Default::default()
        };
        assert!(build_cors_layer(&cfg).is_some());
    }
}
