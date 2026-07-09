//! 请求重试辅助函数
//!
//! 提供可重试错误/状态码判定，供代理层在转发失败时决定是否重试。
//!
//! 重试策略：
//! - 客户端错误（连接失败/超时/读取失败等）：所有方法均可重试
//!   （请求体已缓冲，重试安全；每次重试重新选择上游节点）
//! - 5xx 服务端错误：仅幂等方法（GET/HEAD/OPTIONS/PUT/DELETE）重试
//! - 4xx 客户端错误：不重试（客户端请求有问题，重试无意义）

use http::{Method, StatusCode};
use hyper_util::client::legacy::Error as ClientError;

/// 判断客户端错误是否可重试
///
/// 所有客户端级别错误均视为可重试：请求体已在内存中缓冲（Bytes 引用计数克隆），
/// 上游未返回有效响应时重试不会造成副作用。每次重试会重新选择上游节点，
/// 避免持续命中同一故障节点。
pub fn is_retryable_error(_err: &ClientError) -> bool {
    true
}

/// 判断响应状态码是否可重试（5xx 且方法幂等）
pub fn is_retryable_status(status: StatusCode, method: &Method) -> bool {
    if !status.is_server_error() {
        return false;
    }
    is_idempotent(method)
}

/// 判断 HTTP 方法是否幂等
pub fn is_idempotent(method: &Method) -> bool {
    matches!(
        method,
        &Method::GET | &Method::HEAD | &Method::OPTIONS | &Method::PUT | &Method::DELETE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent_methods() {
        assert!(is_idempotent(&Method::GET));
        assert!(is_idempotent(&Method::HEAD));
        assert!(is_idempotent(&Method::OPTIONS));
        assert!(is_idempotent(&Method::PUT));
        assert!(is_idempotent(&Method::DELETE));
        assert!(!is_idempotent(&Method::POST));
        assert!(!is_idempotent(&Method::PATCH));
    }

    #[test]
    fn retryable_status_only_5xx() {
        assert!(is_retryable_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            &Method::GET
        ));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY, &Method::GET));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND, &Method::GET));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST, &Method::GET));
    }

    #[test]
    fn retryable_status_requires_idempotent() {
        assert!(!is_retryable_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            &Method::POST
        ));
        assert!(is_retryable_status(
            StatusCode::SERVICE_UNAVAILABLE,
            &Method::DELETE
        ));
    }
}
