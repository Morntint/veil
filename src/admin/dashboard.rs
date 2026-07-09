//! 可视化 Dashboard 处理器
//!
//! 内嵌 HTML/CSS/JS 单文件面板，零外部依赖，保持单二进制部署。
//! 通过 `include_str!` 在编译期嵌入 dashboard.html，运行时零 IO 开销。
//! 面板通过轮询 /metrics、/_admin/status、/_admin/routes 实时展示网关状态。

use axum::response::{Html, IntoResponse};

/// Dashboard HTML 内容（编译期嵌入）
const DASHBOARD_HTML: &str = include_str!("dashboard.html");

/// GET /_admin/dashboard — 返回可视化监控面板
pub async fn dashboard_handler() -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_html_not_empty() {
        assert!(!DASHBOARD_HTML.is_empty());
        assert!(DASHBOARD_HTML.contains("<html"));
        assert!(DASHBOARD_HTML.contains("Veil"));
    }
}
