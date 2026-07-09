//! 配置合法性校验
//!
//! 加载后预校验，非法配置直接报错，避免运行期异常。

use crate::config::{AppConfig, RouteConfig};
use crate::utils::{GatewayError, Result};

/// 校验整体配置
pub fn validate(cfg: &AppConfig) -> Result<()> {
    if cfg.server.port == 0 {
        return Err(GatewayError::Config("server.port 不能为 0".into()));
    }
    if cfg.server.host.is_empty() {
        return Err(GatewayError::Config("server.host 不能为空".into()));
    }
    if cfg.network.read_timeout_secs == 0
        || cfg.network.write_timeout_secs == 0
        || cfg.network.connect_timeout_secs == 0
    {
        return Err(GatewayError::Config(
            "network 超时时间必须大于 0".into(),
        ));
    }
    if cfg.network.request_size_limit_bytes == 0 {
        return Err(GatewayError::Config(
            "network.request_size_limit_bytes 必须大于 0".into(),
        ));
    }
    if cfg.proxy.timeout_secs == 0 || cfg.proxy.connect_timeout_secs == 0 {
        return Err(GatewayError::Config("proxy 超时时间必须大于 0".into()));
    }

    for route in &cfg.routes {
        validate_route(route)?;
    }

    // 鉴权启用时 token 不能为空
    if cfg.auth.enable && cfg.auth.token.is_empty() {
        return Err(GatewayError::Config(
            "auth.enable 启用时 auth.token 不能为空".into(),
        ));
    }

    if cfg.security.enable_rate_limit && cfg.security.rate_limit_per_second == 0 {
        return Err(GatewayError::Config(
            "security.rate_limit_per_second 启用限流时必须大于 0".into(),
        ));
    }

    Ok(())
}

fn validate_route(route: &RouteConfig) -> Result<()> {
    if route.name.is_empty() {
        return Err(GatewayError::Config("路由 name 不能为空".into()));
    }
    if route.r#match.path.is_empty() {
        return Err(GatewayError::Config(format!(
            "路由 {} 的 match.path 不能为空",
            route.name
        )));
    }
    if !matches!(
        route.r#match.match_type.as_str(),
        "exact" | "prefix" | "regex"
    ) {
        return Err(GatewayError::Config(format!(
            "路由 {} 的 match.type 不支持: {}（仅支持 exact/prefix/regex）",
            route.name, route.r#match.match_type
        )));
    }
    if route.upstream.is_empty() {
        return Err(GatewayError::Config(format!(
            "路由 {} 的 upstream 不能为空",
            route.name
        )));
    }
    for u in &route.upstream {
        if u.parse::<http::Uri>().is_err() {
            return Err(GatewayError::Config(format!(
                "路由 {} 的 upstream 地址非法: {}",
                route.name, u
            )));
        }
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            return Err(GatewayError::Config(format!(
                "路由 {} 的 upstream 必须以 http:// 或 https:// 开头: {}",
                route.name, u
            )));
        }
    }
    if !matches!(
        route.load_balance.as_str(),
        "round_robin" | "random" | "least_conn" | "weighted_round_robin"
    ) {
        return Err(GatewayError::Config(format!(
            "路由 {} 的 load_balance 不支持: {}（仅支持 round_robin/random/least_conn/weighted_round_robin）",
            route.name, route.load_balance
        )));
    }
    // 加权轮询：若提供权重，长度必须与 upstream 对齐，且每个权重 > 0
    if route.load_balance == "weighted_round_robin" && !route.upstream_weights.is_empty() {
        if route.upstream_weights.len() != route.upstream.len() {
            return Err(GatewayError::Config(format!(
                "路由 {} 的 upstream_weights 长度({})与 upstream({})不一致",
                route.name,
                route.upstream_weights.len(),
                route.upstream.len()
            )));
        }
        for (i, w) in route.upstream_weights.iter().enumerate() {
            if *w == 0 {
                return Err(GatewayError::Config(format!(
                    "路由 {} 的 upstream_weights[{}] 不能为 0",
                    route.name, i
                )));
            }
        }
    }
    // 改写规则：启用时正则必须可编译
    if route.rewrite.enable {
        if route.rewrite.path_pattern.is_empty() {
            return Err(GatewayError::Config(format!(
                "路由 {} 的 rewrite.path_pattern 启用改写时不能为空",
                route.name
            )));
        }
        if regex::Regex::new(&route.rewrite.path_pattern).is_err() {
            return Err(GatewayError::Config(format!(
                "路由 {} 的 rewrite.path_pattern 正则编译失败: {}",
                route.name, route.rewrite.path_pattern
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn validate_rejects_zero_port() {
        let mut cfg = AppConfig::default();
        cfg.server.port = 0;
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn validate_accepts_default() {
        let cfg = AppConfig::default();
        assert!(validate(&cfg).is_ok());
    }
}
