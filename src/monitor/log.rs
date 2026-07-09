//! 结构化日志配置：基于 tracing + tracing-subscriber
//!
//! 支持 env-filter 优先级覆盖、JSON / 文本两种输出格式。

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// 初始化全局日志订阅者
///
/// - `level`：默认日志级别（trace/debug/info/warn/error），可被 `RUST_LOG` 环境变量覆盖
/// - `format`：输出格式，`json` 为结构化 JSON，其余为可读文本
pub fn init(level: &str, format: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let registry = tracing_subscriber::registry().with(filter);

    let _ = match format {
        "json" => registry.with(fmt::layer().json()).try_init(),
        _ => registry.with(fmt::layer()).try_init(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_does_not_panic_on_repeated_calls() {
        init("info", "text");
        init("debug", "json");
    }
}
