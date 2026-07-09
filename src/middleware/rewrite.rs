//! 请求路径改写中间件
//!
//! 基于正则表达式的路径改写，支持捕获组替换（$1 $2 ...）。
//! 改写仅作用于 path 部分，query string 原样保留。
//! 配置热更新：每次请求实时读取路由配置，立即生效。

use std::collections::HashMap;

use http::Uri;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use regex::Regex;

use crate::config::RewriteConfig;

/// 正则缓存：避免每次请求重新编译相同改写 pattern
static REGEX_CACHE: Lazy<Mutex<HashMap<String, Regex>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 从缓存获取或编译正则，编译失败返回 None
fn get_cached_regex(pattern: &str) -> Option<Regex> {
    let mut cache = REGEX_CACHE.lock();
    if let Some(re) = cache.get(pattern) {
        return Some(re.clone());
    }
    match Regex::new(pattern) {
        Ok(re) => {
            cache.insert(pattern.to_string(), re.clone());
            Some(re)
        }
        Err(_) => None,
    }
}

/// 应用路径改写规则，返回改写后的 URI
///
/// 若改写未启用或正则编译失败或改写后路径非法，返回原始 URI。
pub fn apply_rewrite(original: &Uri, rewrite: &RewriteConfig) -> Uri {
    if !rewrite.enable {
        return original.clone();
    }

    let re = match get_cached_regex(&rewrite.path_pattern) {
        Some(r) => r,
        None => {
            tracing::warn!(
                pattern = %rewrite.path_pattern,
                "改写正则编译失败，跳过改写"
            );
            return original.clone();
        }
    };

    let path = original.path();
    let query = original.query().map(|q| format!("?{q}")).unwrap_or_default();

    let new_path = re.replace(path, rewrite.path_replace.as_str()).to_string();

    // 路径未变化，直接返回原始 URI
    if new_path == path {
        return original.clone();
    }

    let target = format!("{new_path}{query}");
    match target.parse::<Uri>() {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(target = %target, error = %e, "改写后 URI 解析失败，使用原始 URI");
            original.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_rewrite_returns_original() {
        let original: Uri = "/api/v1/users".parse().unwrap();
        let rewrite = RewriteConfig::default();
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/v1/users");
    }

    #[test]
    fn rewrite_replaces_path() {
        let original: Uri = "/api/v1/users".parse().unwrap();
        let rewrite = RewriteConfig {
            enable: true,
            path_pattern: r"/api/v1".into(),
            path_replace: "/api/v2".into(),
        };
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/v2/users");
    }

    #[test]
    fn rewrite_preserves_query() {
        let original: Uri = "/api/v1/users?id=42&name=foo".parse().unwrap();
        let rewrite = RewriteConfig {
            enable: true,
            path_pattern: r"/api/v1".into(),
            path_replace: "/api/v2".into(),
        };
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/v2/users");
        assert_eq!(result.query(), Some("id=42&name=foo"));
    }

    #[test]
    fn rewrite_with_capture_groups() {
        let original: Uri = "/api/old/users/123".parse().unwrap();
        let rewrite = RewriteConfig {
            enable: true,
            path_pattern: r"/api/old/(.+)".into(),
            path_replace: "/api/new/$1".into(),
        };
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/new/users/123");
    }

    #[test]
    fn rewrite_invalid_regex_returns_original() {
        let original: Uri = "/api/users".parse().unwrap();
        let rewrite = RewriteConfig {
            enable: true,
            path_pattern: "[invalid".into(),
            path_replace: "/new".into(),
        };
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/users");
    }
}
