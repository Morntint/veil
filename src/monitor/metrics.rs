//! Prometheus 指标采集
//!
//! 覆盖网关核心监控维度：QPS、请求成功率、响应耗时分位数、活跃连接数、
//! 限流触发次数、错误码分布、上游转发统计。
//!
//! 设计：每个指标用 once_cell::Lazy 声明为全局静态实例，初始化时注册到全局 Registry。
//! 采集方式：在代理转发、限流、鉴权等关键路径调用 record_* 辅助函数。
//! 暴露方式：/metrics 端点返回 Prometheus 文本格式。

use once_cell::sync::Lazy;
use prometheus::{
    CounterVec, Gauge, HistogramOpts, HistogramVec, Opts, Registry, TextEncoder,
};
use std::time::Duration;

/// 全局 Prometheus 注册表（所有指标实例初始化时注册到此）
pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

// ---- 全局指标实例 ----
// 每个指标在首次访问时创建并注册到 REGISTRY，后续访问直接复用。

/// HTTP 请求总数（按方法、状态码、路由分维度）
static REQUESTS_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    let m = CounterVec::new(
        Opts::new("gateway_http_requests_total", "网关接收的 HTTP 请求总数"),
        &["method", "status", "route"],
    )
    .expect("构造 REQUESTS_TOTAL 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 REQUESTS_TOTAL 失败");
    m
});

/// HTTP 请求处理耗时直方图（秒）
static REQUEST_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let m = HistogramVec::new(
        HistogramOpts::new(
            "gateway_http_request_duration_seconds",
            "HTTP 请求处理耗时（秒）",
        )
        .buckets(vec![
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ]),
        &["method", "route"],
    )
    .expect("构造 REQUEST_DURATION 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 REQUEST_DURATION 失败");
    m
});

/// 活跃连接数（当前正在处理的请求数）
static ACTIVE_CONNECTIONS: Lazy<Gauge> = Lazy::new(|| {
    let m = Gauge::new("gateway_active_connections", "当前活跃连接数（正在处理的请求数）")
        .expect("构造 ACTIVE_CONNECTIONS 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 ACTIVE_CONNECTIONS 失败");
    m
});

/// 限流触发总次数
static RATE_LIMIT_TOTAL: Lazy<CounterVec> = Lazy::new(|| {
    let m = CounterVec::new(
        Opts::new("gateway_rate_limit_total", "限流触发总次数"),
        &["ip"],
    )
    .expect("构造 RATE_LIMIT_TOTAL 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 RATE_LIMIT_TOTAL 失败");
    m
});

/// 鉴权失败总次数
static AUTH_FAILURES: Lazy<CounterVec> = Lazy::new(|| {
    let m = CounterVec::new(
        Opts::new("gateway_auth_failures_total", "鉴权失败总次数"),
        &["reason"],
    )
    .expect("构造 AUTH_FAILURES 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 AUTH_FAILURES 失败");
    m
});

/// 上游转发请求总数（按上游地址、状态码分维度）
static UPSTREAM_REQUESTS: Lazy<CounterVec> = Lazy::new(|| {
    let m = CounterVec::new(
        Opts::new("gateway_upstream_requests_total", "上游转发请求总数"),
        &["upstream", "status"],
    )
    .expect("构造 UPSTREAM_REQUESTS 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 UPSTREAM_REQUESTS 失败");
    m
});

/// 上游转发耗时直方图（秒）
static UPSTREAM_DURATION: Lazy<HistogramVec> = Lazy::new(|| {
    let m = HistogramVec::new(
        HistogramOpts::new(
            "gateway_upstream_request_duration_seconds",
            "上游转发耗时（秒）",
        )
        .buckets(vec![
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ]),
        &["upstream"],
    )
    .expect("构造 UPSTREAM_DURATION 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 UPSTREAM_DURATION 失败");
    m
});

/// 上游重试总次数
static UPSTREAM_RETRIES: Lazy<CounterVec> = Lazy::new(|| {
    let m = CounterVec::new(
        Opts::new("gateway_upstream_retries_total", "上游重试总次数"),
        &["route"],
    )
    .expect("构造 UPSTREAM_RETRIES 失败");
    REGISTRY
        .register(Box::new(m.clone()))
        .expect("注册 UPSTREAM_RETRIES 失败");
    m
});

// ---- 指标采集辅助函数 ----
// 调用这些函数会触发对应 Lazy 静态的初始化（仅首次），后续调用零开销。

/// 记录一次 HTTP 请求（计数 + 耗时）
pub fn record_request(method: &str, route: &str, status: u16, duration: Duration) {
    REQUESTS_TOTAL
        .with_label_values(&[method, &status.to_string(), route])
        .inc();
    REQUEST_DURATION
        .with_label_values(&[method, route])
        .observe(duration.as_secs_f64());
}

/// 递增活跃连接计数
pub fn inc_active_connection() {
    ACTIVE_CONNECTIONS.inc();
}

/// 递减活跃连接计数
pub fn dec_active_connection() {
    ACTIVE_CONNECTIONS.dec();
}

/// 记录一次限流触发
pub fn record_rate_limit(ip: &str) {
    RATE_LIMIT_TOTAL.with_label_values(&[ip]).inc();
}

/// 记录一次鉴权失败
pub fn record_auth_failure(reason: &str) {
    AUTH_FAILURES.with_label_values(&[reason]).inc();
}

/// 记录一次上游转发（计数 + 耗时）
pub fn record_upstream_request(upstream: &str, status: u16, duration: Duration) {
    UPSTREAM_REQUESTS
        .with_label_values(&[upstream, &status.to_string()])
        .inc();
    UPSTREAM_DURATION
        .with_label_values(&[upstream])
        .observe(duration.as_secs_f64());
}

/// 记录一次上游重试
pub fn record_upstream_retry(route: &str) {
    UPSTREAM_RETRIES.with_label_values(&[route]).inc();
}

/// 渲染所有指标为 Prometheus 文本格式
pub fn render() -> String {
    // 触发所有指标初始化，确保 gather 返回完整数据
    Lazy::force(&REQUESTS_TOTAL);
    Lazy::force(&REQUEST_DURATION);
    Lazy::force(&ACTIVE_CONNECTIONS);
    Lazy::force(&RATE_LIMIT_TOTAL);
    Lazy::force(&AUTH_FAILURES);
    Lazy::force(&UPSTREAM_REQUESTS);
    Lazy::force(&UPSTREAM_DURATION);
    Lazy::force(&UPSTREAM_RETRIES);

    let mut buffer = String::new();
    let encoder = TextEncoder::new();
    let metrics = REGISTRY.gather();
    encoder
        .encode_utf8(&metrics, &mut buffer)
        .expect("编码 Prometheus 指标失败");
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_request_increments_counter() {
        record_request("GET", "test_route", 200, Duration::from_millis(15));
        record_request("POST", "/api", 201, Duration::from_millis(50));
        record_request("GET", "/api", 500, Duration::from_millis(1500));
    }

    #[test]
    fn active_connection_gauge_works() {
        inc_active_connection();
        inc_active_connection();
        dec_active_connection();
    }

    #[test]
    fn record_rate_limit_and_auth() {
        record_rate_limit("127.0.0.1");
        record_auth_failure("missing_token");
        record_auth_failure("invalid_token");
    }

    #[test]
    fn record_upstream_metrics() {
        record_upstream_request("http://127.0.0.1:9001", 200, Duration::from_millis(30));
        record_upstream_request("http://127.0.0.1:9001", 503, Duration::from_millis(2000));
        record_upstream_retry("example-service");
    }

    #[test]
    fn render_outputs_metrics_text() {
        record_request("GET", "render_test", 200, Duration::from_millis(10));
        let output = render();
        assert!(
            output.contains("gateway_http_requests_total"),
            "输出应包含指标名，实际: {output}"
        );
        assert!(output.contains("gateway_active_connections"));
        assert!(output.contains("gateway_upstream_requests_total"));
    }
}
