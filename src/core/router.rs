//! 路由规则定义、动态匹配
//!
//! 支持三种匹配类型（按 `match.type` 配置）：
//! - `exact`  ：路径完全相等
//! - `prefix` ：路径前缀匹配（最长前缀优先）
//! - `regex`  ：正则匹配（`regex` crate 语法）
//!
//! 匹配顺序：先按配置文件中路由声明顺序逐条尝试，命中即返回。
//! 为保证 `prefix` 语义更精确，建议在配置中将更长的前缀路由声明在前。
//!
//! 路由信息来源于 `SharedConfig`，每次请求实时读取，因此配置热更新后立即可见，
//! 无需重启或重建路由表。

use std::sync::Arc;

use crate::config::RouteConfig;

/// 在给定路由集合中匹配路径，返回命中的路由（包装为 Arc 以便在请求上下文中共享）
///
/// 匹配规则：按 `routes` 声明顺序逐条尝试，首条命中即返回。
/// 正则编译失败的路由会被跳过并记录警告（不阻塞其它路由匹配）。
pub fn match_route(routes: &[RouteConfig], path: &str) -> Option<Arc<RouteConfig>> {
    for route in routes {
        if matches_path(&route.r#match.match_type, &route.r#match.path, path) {
            return Some(Arc::new(route.clone()));
        }
    }
    None
}

/// 单条匹配规则判定
pub fn matches_path(match_type: &str, pattern: &str, path: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    match match_type {
        "exact" => path == pattern,
        "prefix" => path.starts_with(pattern),
        "regex" => match regex::Regex::new(pattern) {
            Ok(re) => re.is_match(path),
            Err(e) => {
                tracing::warn!(pattern = %pattern, error = %e, "路由正则编译失败，跳过该路由");
                false
            }
        },
        other => {
            tracing::warn!(match_type = %other, "未知匹配类型，跳过该路由");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteMatchConfig;

    fn make_route(name: &str, match_type: &str, path: &str) -> RouteConfig {
        RouteConfig {
            name: name.into(),
            r#match: RouteMatchConfig {
                match_type: match_type.into(),
                path: path.into(),
            },
            upstream: vec!["http://127.0.0.1:9001".into()],
            ..Default::default()
        }
    }

    #[test]
    fn exact_match_hits_only_exact_path() {
        let routes = vec![make_route("r1", "exact", "/api/users")];
        assert!(match_route(&routes, "/api/users").is_some());
        assert!(match_route(&routes, "/api/users/1").is_none());
    }

    #[test]
    fn prefix_match_hits_subpaths() {
        let routes = vec![make_route("r1", "prefix", "/api")];
        assert!(match_route(&routes, "/api").is_some());
        assert!(match_route(&routes, "/api/users").is_some());
        assert!(match_route(&routes, "/web").is_none());
    }

    #[test]
    fn regex_match_works() {
        let routes = vec![make_route("r1", "regex", r"^/api/v\d+/users$")];
        assert!(match_route(&routes, "/api/v1/users").is_some());
        assert!(match_route(&routes, "/api/v2/users").is_some());
        assert!(match_route(&routes, "/api/v1/users/1").is_none());
    }

    #[test]
    fn first_match_wins_in_declaration_order() {
        let routes = vec![
            make_route("broad", "prefix", "/api"),
            make_route("narrow", "exact", "/api/special"),
        ];
        let hit = match_route(&routes, "/api/special").unwrap();
        assert_eq!(hit.name, "broad");
    }

    #[test]
    fn invalid_regex_is_skipped() {
        let routes = vec![
            make_route("bad", "regex", r"[invalid"),
            make_route("good", "prefix", "/api"),
        ];
        let hit = match_route(&routes, "/api/x").unwrap();
        assert_eq!(hit.name, "good");
    }

    #[test]
    fn empty_pattern_never_matches() {
        let routes = vec![make_route("r1", "prefix", "")];
        assert!(match_route(&routes, "/anything").is_none());
    }
}
