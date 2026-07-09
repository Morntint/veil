//! 路径改写中间件
//!
//! 基于正则的路径改写，支持捕获组替换与全量/首条替换。
//! 正则在配置加载阶段预编译（存入 RewriteConfig.compiled_regex），请求路径零编译开销。

use http::Uri;

use crate::config::RewriteConfig;

/// 应用路径改写
///
/// 根据改写配置的正则模式匹配原始路径，替换为目标模式。
/// 未启用改写或正则未编译时返回原始 URI 克隆。
/// 支持捕获组（$1, $2, ...）和全量替换（replace_all = true）。
pub fn apply_rewrite(original: &Uri, rewrite: &RewriteConfig) -> Uri {
    if !rewrite.enable {
        return original.clone();
    }

    let re = match &rewrite.compiled_regex {
        Some(r) => r,
        None => {
            tracing::warn!(
                pattern = %rewrite.path_pattern,
                "改写正则未预编译，跳过改写"
            );
            return original.clone();
        }
    };

    let path = original.path();
    let query = original.query().map(|q| format!("?{q}")).unwrap_or_default();

    let new_path = if rewrite.replace_all {
        re.replace_all(path, rewrite.path_replace.as_str()).to_string()
    } else {
        re.replace(path, rewrite.path_replace.as_str()).to_string()
    };

    let new_uri = format!("{new_path}{query}");
    new_uri.parse().unwrap_or_else(|_| original.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rewrite(pattern: &str, replace: &str) -> RewriteConfig {
        let mut rw = RewriteConfig {
            enable: true,
            path_pattern: pattern.into(),
            path_replace: replace.into(),
            ..Default::default()
        };
        rw.compiled_regex = regex::Regex::new(pattern).ok();
        rw
    }

    #[test]
    fn rewrite_replaces_path() {
        let original: Uri = "/api/v1/users".parse().unwrap();
        let rewrite = make_rewrite(r"/api/v1", "/api/v2");
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/v2/users");
    }

    #[test]
    fn rewrite_preserves_query() {
        let original: Uri = "/api/v1/users?id=42&name=foo".parse().unwrap();
        let rewrite = make_rewrite(r"/api/v1", "/api/v2");
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/v2/users");
        assert_eq!(result.query(), Some("id=42&name=foo"));
    }

    #[test]
    fn rewrite_with_capture_groups() {
        let original: Uri = "/api/old/users/123".parse().unwrap();
        let rewrite = make_rewrite(r"/api/old/(.+)", "/api/new/$1");
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
            compiled_regex: None, // 预编译失败
            ..Default::default()
        };
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/users");
    }

    #[test]
    fn rewrite_replace_all() {
        let original: Uri = "/a/b/a/c/a".parse().unwrap();
        let mut rewrite = make_rewrite(r"/a", "/x");
        rewrite.replace_all = true;
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/x/b/x/c/x");
    }

    #[test]
    fn rewrite_replace_first_only() {
        let original: Uri = "/a/b/a/c/a".parse().unwrap();
        let rewrite = make_rewrite(r"/a", "/x");
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/x/b/a/c/a");
    }

    #[test]
    fn rewrite_disabled_returns_original() {
        let original: Uri = "/api/users".parse().unwrap();
        let rewrite = RewriteConfig {
            enable: false,
            ..Default::default()
        };
        let result = apply_rewrite(&original, &rewrite);
        assert_eq!(result.path(), "/api/users");
    }
}
