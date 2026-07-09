//! 统一错误类型与 Result 别名
//!
//! 提供网关全局错误分类，便于日志记录、告警与 HTTP 响应映射。

use thiserror::Error;

/// 网关统一错误类型
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("网络错误: {0}")]
    Network(String),

    #[error("路由未匹配: {0}")]
    Route(String),

    #[error("代理转发错误: {0}")]
    Proxy(String),

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
            GatewayError::Proxy(_) => http::StatusCode::BAD_GATEWAY,
            GatewayError::Network(_) => http::StatusCode::BAD_GATEWAY,
            _ => http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        tracing::warn!(error = %self, "网关返回错误响应");
        let body = serde_json::json!({ "error": self.to_string(), "code": status.as_u16() });
        (status, axum::Json(body)).into_response()
    }
}

impl GatewayError {
    /// 快速构造内部错误
    pub fn internal(msg: impl Into<String>) -> Self {
        GatewayError::Internal(msg.into())
    }
}
