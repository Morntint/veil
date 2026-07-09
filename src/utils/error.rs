//! 统一错误类型与 Result 别名
//!
//! 提供网关全局错误分类，便于日志记录、告警与 HTTP 响应映射。
//!
//! 设计原则：
//! - `Config` / `Proxy` 携带结构化 `source`（原始错误链），而非 format! 拼接字符串，
//!   使错误链可通过 `Error::source()` 程序化遍历，便于日志聚合与告警分类。
//! - 简单变体（Auth / RateLimit 等）保持 String，上下文清晰无需额外结构。
//! - Io / Hyper / Http 使用 `#[from]` transparent，保留完整原始错误。

use std::error::Error as StdError;
use thiserror::Error;

/// 网关统一错误类型
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("配置错误: {message}")]
    Config {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("网络错误: {0}")]
    Network(String),

    #[error("路由未匹配: {0}")]
    Route(String),

    #[error("代理转发错误: {message}")]
    Proxy {
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("鉴权失败: {0}")]
    Auth(String),

    #[error("限流触发: {0}")]
    RateLimit(String),

    #[error("请求超时: {0}")]
    Timeout(String),

    #[error("请求体过大: {0}")]
    PayloadTooLarge(String),

    #[error("参数校验失败: {0}")]
    Validation(String),

    #[error("内部错误: {0}")]
    Internal(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Hyper(#[from] hyper::Error),

    #[error(transparent)]
    Http(#[from] http::Error),
}

/// 网关统一 Result
pub type Result<T> = std::result::Result<T, GatewayError>;

impl GatewayError {
    /// 构造配置错误（无原始错误源）
    pub fn config(message: impl Into<String>) -> Self {
        GatewayError::Config {
            message: message.into(),
            source: None,
        }
    }

    /// 构造配置错误（携带原始错误源，保留错误链）
    pub fn config_with_source(
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        GatewayError::Config {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// 构造代理错误（无原始错误源）
    pub fn proxy(message: impl Into<String>) -> Self {
        GatewayError::Proxy {
            message: message.into(),
            source: None,
        }
    }

    /// 构造代理错误（携带原始错误源，保留错误链）
    pub fn proxy_with_source(
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        GatewayError::Proxy {
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// 快速构造内部错误
    pub fn internal(msg: impl Into<String>) -> Self {
        GatewayError::Internal(msg.into())
    }
}

/// 将错误映射为 HTTP 响应，供 axum 中间件/处理器直接返回
impl axum::response::IntoResponse for GatewayError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            GatewayError::Auth(_) => http::StatusCode::UNAUTHORIZED,
            GatewayError::RateLimit(_) => http::StatusCode::TOO_MANY_REQUESTS,
            GatewayError::PayloadTooLarge(_) => http::StatusCode::PAYLOAD_TOO_LARGE,
            GatewayError::Timeout(_) => http::StatusCode::GATEWAY_TIMEOUT,
            GatewayError::Route(_) => http::StatusCode::NOT_FOUND,
            GatewayError::Validation(_) => http::StatusCode::BAD_REQUEST,
            GatewayError::Proxy { .. } => http::StatusCode::BAD_GATEWAY,
            GatewayError::Config { .. } => http::StatusCode::INTERNAL_SERVER_ERROR,
            GatewayError::Network(_) => http::StatusCode::BAD_GATEWAY,
            _ => http::StatusCode::INTERNAL_SERVER_ERROR,
        };

        // 构造错误消息：base + source 链，使 HTTP 响应包含完整错误信息
        let base = self.to_string();
        let error_msg = match self.source() {
            Some(s) => format!("{base}: {s}"),
            None => base,
        };

        tracing::warn!(error = %error_msg, "网关返回错误响应");
        let body = serde_json::json!({ "error": error_msg, "code": status.as_u16() });
        (status, axum::Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_error_preserves_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = GatewayError::config_with_source("读取配置失败", io_err);
        assert!(err.source().is_some());
        assert!(err.to_string().contains("读取配置失败"));
        // source 链可程序化遍历
        assert!(err.source().unwrap().to_string().contains("file not found"));
    }

    #[test]
    fn config_error_without_source_has_none() {
        let err = GatewayError::config("server.port 不能为 0");
        assert!(err.source().is_none());
        assert!(err.to_string().contains("server.port 不能为 0"));
    }

    #[test]
    fn proxy_error_preserves_source_chain() {
        let http_err: http::Error = "bad uri".parse::<http::Uri>().unwrap_err().into();
        let err = GatewayError::proxy_with_source("URI 构造失败", http_err);
        assert!(err.source().is_some());
        assert!(err.to_string().contains("URI 构造失败"));
    }

    #[test]
    fn simple_variants_keep_string_payload() {
        let err = GatewayError::Auth("Token 无效".into());
        assert!(err.to_string().contains("Token 无效"));
        assert!(err.source().is_none());
    }
}
