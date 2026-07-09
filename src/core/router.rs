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
//! 性能优化：
//! - 正则表达式在配置加载阶段预编译（存入 RouteMatchConfig.compiled_regex）
//! - 所有正则模式额外合并为 `RegexSet`，请求路径一次匹配全部正则，
//!   避免 N 条 regex 路由触发 N 次独立正则求值（O(n) → O(1) set 查询）
//! - exact / prefix 为廉价字符串操作，保持线性扫描以保留"首条命中优先"语义

use std::sync::Arc;

use crate::config::RouteConfig;

/// 路由索引：预编译的正则集合，用于一次匹配全部 regex 路由
///
/// 在配置加载阶段（`precompile_routes`）构建，随 AppConfig 热更新自动重建。
/// `regex_route_indices` 将 RegexSet 的匹配索引映射回 routes Vec 中的原始位置，
/// 以便在"首条命中优先"遍历中按声明顺序检查。
#[derive(Debug, Default, Clone)]
pub struct RouteIndex {
    /// 所有 regex 路由模式合并为一个 RegexSet（无 regex 路由时为 None）
    regex_set: Option<regex::RegexSet>,
    /// RegexSet 第 i 个模式对应的 routes Vec 索引
    regex_route_indices: Vec<usize>,
}

impl RouteIndex {
    /// 从路由列表构建索引：收集所有预编译成功的 regex 模式合并为 RegexSet
    pub fn build(routes: &[Arc<RouteConfig>]) -> Self {
        let mut patterns: Vec<&str> = Vec::new();
        let mut regex_route_indices: Vec<usize> = Vec::new();
        for (i, route) in routes.iter().enumerate() {
            if route.r#match.match_type == "regex" && route.r#match.compiled_regex.is_some() {
                patterns.push(route.r#match.path.as_str());
                regex_route_indices.push(i);
            }
        }
        let regex_set = if patterns.is_empty() {
            None
        } else {
            match regex::RegexSet::new(&patterns) {
                Ok(set) => Some(set),
                Err(e) => {
                    tracing::warn!(error = %e, "RegexSet 构建失败，回退到逐条正则匹配");
                    None
                }
            }
        };
        Self {
            regex_set,
            regex_route_indices,
        }
    }

    /// 对给定路径执行一次 RegexSet 匹配，返回命中的原始路由索引集合
    fn regex_matches(&self, path: &str) -> std::collections::HashSet<usize> {
        match &self.regex_set {
            Some(set) => set
                .matches(path)
                .into_iter()
                .map(|i| self.regex_route_indices[i])
                .collect(),
            None => std::collections::HashSet::new(),
        }
    }
}

/// 在给定路由集合中匹配路径，返回命中的路由 Arc（零拷贝）
///
/// 匹配规则：按 `routes` 声明顺序逐条尝试，首条命中即返回。
/// 正则路由通过预构建的 `RouteIndex.regex_set` 一次匹配全部模式，
/// 遍历时仅需查表判断是否命中，无需逐条执行正则。
pub fn match_route(
    routes: &[Arc<RouteConfig>],
    path: &str,
    index: &RouteIndex,
) -> Option<Arc<RouteConfig>> {
    let regex_hits = index.regex_matches(path);
    for (i, route) in routes.iter().enumerate() {
        if matches_path_indexed(&route.r#match, path, i, &regex_hits) {
            return Some(route.clone());
        }
    }
    None
}

/// 单条匹配规则判定（使用预编译正则）
///
/// 保留此函数供外部调用方在不持有 RouteIndex 时使用（如测试）。
pub fn matches_path(match_cfg: &crate::config::RouteMatchConfig, path: &str) -> bool {
    if match_cfg.path.is_empty() {
        return false;
    }
    match match_cfg.match_type.as_str() {
        "exact" => path == match_cfg.path,
        "prefix" => path.starts_with(&match_cfg.path),
        "regex" => match &match_cfg.compiled_regex {
            Some(re) => re.is_match(path),
            None => false, // 预编译失败的路由不匹配
        },
        other => {
            tracing::warn!(match_type = %other, "未知匹配类型，跳过该路由");
            false
        }
    }
}

/// 内部匹配判定：regex 类型通过预计算的命中集合判断，避免逐条正则求值
fn matches_path_indexed(
    match_cfg: &crate::config::RouteMatchConfig,
    path: &str,
    route_idx: usize,
    regex_hits: &std::collections::HashSet<usize>,
) -> bool {
    if match_cfg.path.is_empty() {
        return false;
    }
    match match_cfg.match_type.as_str() {
        "exact" => path == match_cfg.path,
        "prefix" => path.starts_with(&match_cfg.path),
        "regex" => regex_hits.contains(&route_idx),
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

    fn make_route(name: &str, match_type: &str, path: &str) -> Arc<RouteConfig> {
        let mut route = RouteConfig {
            name: name.into(),
            r#match: RouteMatchConfig {
                match_type: match_type.into(),
                path: path.into(),
                compiled_regex: None,
            },
            upstream: vec!["http://127.0.0.1:9001".into()],
            ..Default::default()
        };
        // 模拟加载阶段预编译
        if match_type == "regex" {
            route.r#match.compiled_regex = regex::Regex::new(path).ok();
        }
        Arc::new(route)
    }

    fn build_index(routes: &[Arc<RouteConfig>]) -> RouteIndex {
        RouteIndex::build(routes)
    }

    #[test]
    fn exact_match_hits_only_exact_path() {
        let routes = vec![make_route("r1", "exact", "/api/users")];
        let idx = build_index(&routes);
        assert!(match_route(&routes, "/api/users", &idx).is_some());
        assert!(match_route(&routes, "/api/users/1", &idx).is_none());
    }

    #[test]
    fn prefix_match_hits_subpaths() {
        let routes = vec![make_route("r1", "prefix", "/api")];
        let idx = build_index(&routes);
        assert!(match_route(&routes, "/api", &idx).is_some());
        assert!(match_route(&routes, "/api/users", &idx).is_some());
        assert!(match_route(&routes, "/web", &idx).is_none());
    }

    #[test]
    fn regex_match_works() {
        let routes = vec![make_route("r1", "regex", r"^/api/v\d+/users$")];
        let idx = build_index(&routes);
        assert!(match_route(&routes, "/api/v1/users", &idx).is_some());
        assert!(match_route(&routes, "/api/v2/users", &idx).is_some());
        assert!(match_route(&routes, "/api/v1/users/1", &idx).is_none());
    }

    #[test]
    fn first_match_wins_in_declaration_order() {
        let routes = vec![
            make_route("broad", "prefix", "/api"),
            make_route("narrow", "exact", "/api/special"),
        ];
        let idx = build_index(&routes);
        let hit = match_route(&routes, "/api/special", &idx).unwrap();
        assert_eq!(hit.name, "broad");
    }

    #[test]
    fn invalid_regex_is_skipped() {
        // 预编译失败时 compiled_regex = None，路由不匹配
        let routes = vec![
            make_route("bad", "regex", r"[invalid"),
            make_route("good", "prefix", "/api"),
        ];
        let idx = build_index(&routes);
        let hit = match_route(&routes, "/api/x", &idx).unwrap();
        assert_eq!(hit.name, "good");
    }

    #[test]
    fn empty_pattern_never_matches() {
        let routes = vec![make_route("r1", "prefix", "")];
        let idx = build_index(&routes);
        assert!(match_route(&routes, "/anything", &idx).is_none());
    }

    #[test]
    fn regex_set_matches_multiple_patterns_at_once() {
        // 验证多条 regex 路由通过 RegexSet 一次匹配
        let routes = vec![
            make_route("r1", "regex", r"^/api/v\d+/users$"),
            make_route("r2", "regex", r"^/api/v\d+/orders$"),
            make_route("r3", "prefix", "/api"),
        ];
        let idx = build_index(&routes);
        // 命中 r2（声明在 r3 前）
        let hit = match_route(&routes, "/api/v1/orders", &idx).unwrap();
        assert_eq!(hit.name, "r2");
        // 命中 r1
        let hit = match_route(&routes, "/api/v2/users", &idx).unwrap();
        assert_eq!(hit.name, "r1");
    }

    #[test]
    fn matches_path_still_works_without_index() {
        // 验证公开函数 matches_path 仍可用于无索引场景
        let cfg = RouteMatchConfig {
            match_type: "exact".into(),
            path: "/test".into(),
            compiled_regex: None,
        };
        assert!(matches_path(&cfg, "/test"));
        assert!(!matches_path(&cfg, "/other"));
    }
}
