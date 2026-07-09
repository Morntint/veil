//! 请求路径改写中间件
//!
//! 基于正则表达式的路径改写，支持捕获组替换（$1 $2 ...）。
//! 改写仅作用于 path 部分，query string 原样保留。
//! 配置热更新：每次请求实时读取路由配置，立即生效。

use http::Uri;

use crate::config::RewriteConfig;

/// 应用路径改写规则，返回改写后的 URI
///
/// 若改写未启用或正则编译失败或改写后路径非法，返回原始 URI。
pub fn apply_rewrite(original: &Uri, rewrite: &RewriteConfig) -> Uri {
    if !rewrite.enable {
        return original.clone();
    }

    let re = match regex::Regex::new(&rewrite.path_pattern) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                pattern = %rewrite.path_pattern,
                error = %e,
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
