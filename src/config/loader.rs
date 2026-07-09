//! 配置加载、多环境叠加、环境变量合并
//!
//! 加载流程：读取 `default.toml` → 叠加 `{env}.toml` → 反序列化 → 环境变量覆盖 →
//! 预编译正则 → 校验。
//! 热更新复用 `load`，校验失败时调用方保留旧配置（兜底回滚）。

use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use toml::Value;

use crate::config::{validate, AppConfig, SharedConfig};
use crate::constant;
use crate::utils::{GatewayError, Result};

/// 加载并校验配置
///
/// - `config_dir`：配置文件目录（如 `config`）
/// - `env`：运行环境，叠加 `{env}.toml` 覆盖默认配置
pub fn load(config_dir: &str, env: &str) -> Result<AppConfig> {
    let base_path = Path::new(config_dir).join("default.toml");
    let mut merged = read_toml(&base_path)?;

    let env_path = Path::new(config_dir).join(format!("{env}.toml"));
    if env_path.exists() {
        let env_val = read_toml(&env_path)?;
        merged = merge_values(merged, env_val);
    }

    let mut cfg: AppConfig = deserialize(&merged)?;
    apply_env_overrides(&mut cfg);
    precompile_routes(&mut cfg);
    cfg.version = 1;
    validate::validate(&cfg)?;
    Ok(cfg)
}

/// 加载配置并封装为共享配置（供热更新与多模块读取）
pub fn load_shared(config_dir: &str, env: &str) -> Result<SharedConfig> {
    let cfg = load(config_dir, env)?;
    Ok(std::sync::Arc::new(ArcSwap::from_pointee(cfg)))
}

/// 预编译所有路由的正则表达式（路由匹配 + 路径改写），存入 #[serde(skip)] 字段
///
/// 编译失败的正则记录警告但不中断加载——validate 阶段会二次校验。
fn precompile_routes(cfg: &mut AppConfig) {
    for route in cfg.routes.iter_mut() {
        let route = Arc::make_mut(route);
        // 路由匹配正则
        if route.r#match.match_type == "regex" && !route.r#match.path.is_empty() {
            match regex::Regex::new(&route.r#match.path) {
                Ok(re) => route.r#match.compiled_regex = Some(re),
                Err(e) => {
                    tracing::warn!(
                        route = %route.name,
                        pattern = %route.r#match.path,
                        error = %e,
                        "路由正则预编译失败"
                    );
                }
            }
        }
        // 路径改写正则
        if route.rewrite.enable && !route.rewrite.path_pattern.is_empty() {
            match regex::Regex::new(&route.rewrite.path_pattern) {
                Ok(re) => route.rewrite.compiled_regex = Some(re),
                Err(e) => {
                    tracing::warn!(
                        route = %route.name,
                        pattern = %route.rewrite.path_pattern,
                        error = %e,
                        "改写正则预编译失败"
                    );
                }
            }
        }
    }

    // 构建路由匹配索引：将所有 regex 路由模式合并为 RegexSet
    cfg.route_index = crate::core::router::RouteIndex::build(&cfg.routes);
}

/// 读取并解析单个 TOML 文件为 `toml::Value`
fn read_toml(path: &Path) -> Result<Value> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        GatewayError::config_with_source(
            format!("读取配置文件 {} 失败", path.display()),
            e,
        )
    })?;
    toml::from_str(&content).map_err(|e| {
        GatewayError::config_with_source(
            format!("解析配置文件 {} 失败", path.display()),
            e,
        )
    })
}

/// 将合并后的 `toml::Value` 反序列化为 `AppConfig`
///
/// 采用“序列化再解析”方式，避免 `IntoDeserializer` 的版本差异，稳定可靠。
fn deserialize(value: &Value) -> Result<AppConfig> {
    let toml_str = toml::to_string(value)
        .map_err(|e| GatewayError::config_with_source("配置序列化失败", e))?;
    toml::from_str::<AppConfig>(&toml_str)
        .map_err(|e| GatewayError::config_with_source("配置反序列化失败", e))
}

/// 深度合并两个 TOML 值：`over` 覆盖 `base`，表类型递归合并，标量直接替换
fn merge_values(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Table(mut base_table), Value::Table(over_table)) => {
            for (k, v) in over_table {
                let merged = base_table
                    .remove(&k)
                    .map(|existing| merge_values(existing, v.clone()))
                    .unwrap_or(v);
                base_table.insert(k, merged);
            }
            Value::Table(base_table)
        }
        // 非 table 类型：覆盖值直接替换
        (_, over) => over,
    }
}

/// 应用环境变量覆盖（优先级最高）
///
/// 支持的变量：`VEIL_HOST`、`VEIL_SERVER_PORT`、`VEIL_LOG_LEVEL`、`VEIL_LOG_FORMAT`、`VEIL_ENV`
fn apply_env_overrides(cfg: &mut AppConfig) {
    if let Ok(v) = std::env::var(constant::env_keys::HOST) {
        if !v.is_empty() {
            cfg.server.host = v;
        }
    }
    if let Ok(v) = std::env::var(constant::env_keys::SERVER_PORT) {
        if let Ok(p) = v.parse::<u16>() {
            cfg.server.port = p;
        }
    }
    if let Ok(v) = std::env::var(constant::env_keys::LOG_LEVEL) {
        if !v.is_empty() {
            cfg.log.level = v;
        }
    }
    if let Ok(v) = std::env::var(constant::env_keys::LOG_FORMAT) {
        if !v.is_empty() {
            cfg.log.format = v;
        }
    }
    if let Ok(v) = std::env::var(constant::env_keys::ENV) {
        if !v.is_empty() {
            cfg.env = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overrides_scalar_and_keeps_base() {
        // base: { a = 1, b = { x = 1, y = 2 } }
        let base: Value = toml::from_str("a = 1\n[b]\nx = 1\ny = 2\n").unwrap();
        // over: { a = 9, b = { y = 20 } }
        let over: Value = toml::from_str("a = 9\n[b]\ny = 20\n").unwrap();
        let merged = merge_values(base, over);
        let table = merged.as_table().unwrap();
        assert_eq!(table["a"].as_integer(), Some(9));
        let b = table["b"].as_table().unwrap();
        assert_eq!(b["x"].as_integer(), Some(1)); // 保留 base
        assert_eq!(b["y"].as_integer(), Some(20)); // 被覆盖
    }
}
