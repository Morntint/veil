//! Token 鉴权中间件
//!
//! 支持 Bearer Token / 自定义方案鉴权，从配置读取预期 Token 进行校验。
//! 可配置跳过路径（健康检查、运维接口等），配置热更新实时生效。

use axum::extract::State;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, Request};

use crate::monitor::metrics;
use crate::network::server::AppState;
use crate::utils::GatewayError;

/// Token 鉴权中间件：校验请求头中的 Token 是否匹配配置
pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let (enabled, token, header_name, scheme, skip_paths) = {
        let cfg = state.config.read();
        (
            cfg.auth.enable,
            cfg.auth.token.clone(),
            cfg.auth.header_name.clone(),
            cfg.auth.scheme.clone(),
            cfg.auth.skip_paths.clone(),
        )
    };

    if !enabled {
        return next.run(req).await;
    }

    // 跳过鉴权路径
    let path = req.uri().path();
    if skip_paths.iter().any(|p| path.starts_with(p.as_str())) {
        return next.run(req).await;
    }

    // 校验 Token
    let headers = req.headers();
    let expected = if scheme.is_empty() {
        token.clone()
    } else {
        format!("{scheme} {token}")
    };

    match headers.get(&header_name) {
        Some(val) => {
            let val_str = val.to_str().unwrap_or("");
            if val_str != expected {
                metrics::record_auth_failure("invalid_token");
                tracing::warn!(path = %path, "Token 鉴权失败");
                return GatewayError::Auth("Token 无效".into()).into_response();
            }
        }
        None => {
            metrics::record_auth_failure("missing_token");
            tracing::warn!(path = %path, header = %header_name, "缺少鉴权头");
            return GatewayError::Auth("缺少鉴权请求头".into()).into_response();
        }
    }

    next.run(req).await
}

/// 从请求头中提取 Token 值（去除方案前缀）
pub fn extract_token(headers: &HeaderMap, header_name: &str, scheme: &str) -> Option<String> {
    let val = headers.get(header_name)?.to_str().ok()?;
    if scheme.is_empty() {
        Some(val.to_string())
    } else {
        let prefix = format!("{scheme} ");
        val.strip_prefix(&prefix).map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    #[test]
    fn extract_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer abc123".parse().unwrap());
        let token = extract_token(&headers, "authorization", "Bearer").unwrap();
        assert_eq!(token, "abc123");
    }

    #[test]
    fn extract_token_without_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", "secret-key".parse().unwrap());
        let token = extract_token(&headers, "x-api-key", "").unwrap();
        assert_eq!(token, "secret-key");
    }

    #[test]
    fn extract_token_missing_header() {
        let headers = HeaderMap::new();
        assert!(extract_token(&headers, "authorization", "Bearer").is_none());
    }

    #[test]
    fn extract_token_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic abc123".parse().unwrap());
        assert!(extract_token(&headers, "authorization", "Bearer").is_none());
    }
}
