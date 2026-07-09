//! 配置模块：结构体定义、加载、校验、热更新
//!
//! 设计要点：
//! - 多环境配置叠加：`default.toml` 为基础，`{env}.toml` 覆盖
//! - 环境变量优先级最高：`VEIL_*` 覆盖配置文件值
//! - 合法性校验：加载后预校验，非法配置直接报错
//! - 热更新兜底：热重载失败或校验不通过时保留旧配置，避免宕机
//! - 配置版本：每次成功加载自增，便于运维追踪

pub mod loader;
pub mod validate;
pub mod watcher;

use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::constant;

/// 共享配置类型：无锁原子替换，支持热更新
///
/// 读端 `load_full()` 返回 `Arc<AppConfig>`（零拷贝、无阻塞）；
/// 写端 `store(Arc::new(new_cfg))` 原子替换。
pub type SharedConfig = std::sync::Arc<ArcSwap<AppConfig>>;

/// 网关全局配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    /// 运行环境（default / dev / prod），仅记录，非配置文件字段
    #[serde(default = "default_env_value")]
    pub env: String,
    /// 配置版本号，每次成功加载自增（非配置文件字段）
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub routes: Vec<Arc<RouteConfig>>,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub cors: CorsConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub admin: AdminConfig,
    /// 路由匹配索引（预编译的 RegexSet，热更新时自动重建）
    #[serde(skip)]
    pub route_index: crate::core::router::RouteIndex,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            env: default_env_value(),
            version: 0,
            server: ServerConfig::default(),
            log: LogConfig::default(),
            network: NetworkConfig::default(),
            proxy: ProxyConfig::default(),
            routes: Vec::new(),
            security: SecurityConfig::default(),
            cors: CorsConfig::default(),
            auth: AuthConfig::default(),
            monitor: MonitorConfig::default(),
            admin: AdminConfig::default(),
            route_index: crate::core::router::RouteIndex::default(),
        }
    }
}

/// 服务监听配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// 工作线程数，0 表示按 CPU 核数自动设置
    #[serde(default)]
    pub workers: usize,
    #[serde(default = "default_graceful_shutdown")]
    pub graceful_shutdown_timeout_secs: u64,
    /// TLS 监听配置（可选，配置后启用 HTTPS）
    #[serde(default)]
    pub tls: TlsConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            workers: 0,
            graceful_shutdown_timeout_secs: default_graceful_shutdown(),
            tls: TlsConfig::default(),
        }
    }
}

/// TLS 配置（监听端）
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TlsConfig {
    /// 是否启用 TLS 监听
    #[serde(default)]
    pub enable: bool,
    /// 证书文件路径（PEM 格式）
    #[serde(default)]
    pub cert_path: String,
    /// 私钥文件路径（PEM 格式）
    #[serde(default)]
    pub key_path: String,
}

/// 日志配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
    #[serde(default = "default_log_output")]
    pub output: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
            output: default_log_output(),
        }
    }
}

/// 网络传输配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    #[serde(default = "default_read_timeout")]
    pub read_timeout_secs: u64,
    #[serde(default = "default_write_timeout")]
    pub write_timeout_secs: u64,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    #[serde(default = "default_request_size_limit")]
    pub request_size_limit_bytes: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            read_timeout_secs: default_read_timeout(),
            write_timeout_secs: default_write_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            max_connections: default_max_connections(),
            request_size_limit_bytes: default_request_size_limit(),
        }
    }
}

/// 反向代理上游连接配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyConfig {
    #[serde(default = "default_proxy_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_proxy_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_max_idle_per_host")]
    pub max_idle_per_host: usize,
    /// 是否信任入站 X-Forwarded-For 头
    ///
    /// 默认 false：丢弃客户端自发的 XFF 后再追加真实 IP，防伪造。
    /// 网关部署在受信代理（ALB/Ingress）后时可设为 true。
    #[serde(default)]
    pub trust_client_xff: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_proxy_timeout(),
            connect_timeout_secs: default_proxy_connect_timeout(),
            max_idle_per_host: default_max_idle_per_host(),
            trust_client_xff: false,
        }
    }
}

/// 路由匹配规则
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteMatchConfig {
    /// 匹配类型：exact / prefix / regex
    #[serde(rename = "type", default = "default_match_type")]
    pub match_type: String,
    #[serde(default)]
    pub path: String,
    /// 预编译的正则（match_type=regex 时由加载阶段编译，序列化时跳过）
    #[serde(skip)]
    pub compiled_regex: Option<regex::Regex>,
}

impl Default for RouteMatchConfig {
    fn default() -> Self {
        Self {
            match_type: default_match_type(),
            path: String::new(),
            compiled_regex: None,
        }
    }
}

/// 单条路由配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub r#match: RouteMatchConfig,
    /// 上游服务地址列表
    #[serde(default)]
    pub upstream: Vec<String>,
    /// 上游权重列表（可选）：与 upstream 一一对应，缺省或长度不符时按全 1 处理
    /// 仅在 load_balance = weighted_round_robin 时生效
    #[serde(default)]
    pub upstream_weights: Vec<u32>,
    /// 负载均衡策略：round_robin / random / least_conn / weighted_round_robin
    #[serde(default = "default_load_balance")]
    pub load_balance: String,
    /// 路由级代理超时秒数（0 表示使用全局 proxy.timeout_secs）
    #[serde(default)]
    pub timeout_secs: u64,
    /// 路由级重试次数（0 表示不重试）
    #[serde(default)]
    pub retries: u32,
    /// 路径改写规则（可选）
    #[serde(default)]
    pub rewrite: RewriteConfig,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            r#match: RouteMatchConfig::default(),
            upstream: Vec::new(),
            upstream_weights: Vec::new(),
            load_balance: default_load_balance(),
            timeout_secs: 0,
            retries: 0,
            rewrite: RewriteConfig::default(),
        }
    }
}

impl RouteConfig {
    /// 返回归一化后的权重列表：长度与 upstream 对齐，缺省/非法值视为 1
    pub fn normalized_weights(&self) -> Vec<u32> {
        if self.upstream_weights.len() == self.upstream.len() {
            self.upstream_weights
                .iter()
                .map(|w| if *w == 0 { 1 } else { *w })
                .collect()
        } else {
            vec![1u32; self.upstream.len()]
        }
    }
}

/// 安全配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    #[serde(default = "default_true")]
    pub enable_rate_limit: bool,
    #[serde(default = "default_rate_per_second")]
    pub rate_limit_per_second: u32,
    #[serde(default = "default_rate_burst")]
    pub rate_limit_burst: u32,
    /// 限流器 GC 间隔（秒）：定期清理过期的 per-IP 状态，防止内存耗尽
    #[serde(default = "default_rate_limit_gc_secs")]
    pub rate_limit_gc_secs: u64,
    #[serde(default)]
    pub enable_ip_blacklist: bool,
    #[serde(default)]
    pub ip_blacklist: Vec<String>,
    /// 受信代理 CIDR 列表（如 ALB/Ingress 内网网段）
    ///
    /// 非空时，限流与黑名单使用 X-Forwarded-For 中最后一个非受信 IP 作为真实客户端 IP。
    /// 为空时，直接使用 TCP peer IP。
    #[serde(default)]
    pub trusted_proxy_cidrs: Vec<String>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enable_rate_limit: default_true(),
            rate_limit_per_second: default_rate_per_second(),
            rate_limit_burst: default_rate_burst(),
            rate_limit_gc_secs: default_rate_limit_gc_secs(),
            enable_ip_blacklist: false,
            ip_blacklist: Vec::new(),
            trusted_proxy_cidrs: Vec::new(),
        }
    }
}

/// 监控可观测配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MonitorConfig {
    #[serde(default = "default_true")]
    pub enable_metrics: bool,
    #[serde(default = "default_metrics_path")]
    pub metrics_path: String,
    #[serde(default = "default_true")]
    pub enable_health_check: bool,
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            enable_metrics: default_true(),
            metrics_path: default_metrics_path(),
            enable_health_check: default_true(),
            health_path: default_health_path(),
        }
    }
}

/// 运维管理接口配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminConfig {
    #[serde(default = "default_true")]
    pub enable: bool,
    #[serde(default = "default_admin_prefix")]
    pub prefix: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enable: default_true(),
            prefix: default_admin_prefix(),
        }
    }
}

/// CORS 跨域配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CorsConfig {
    #[serde(default)]
    pub enable: bool,
    /// 允许的源列表，空表示允许任意（*）
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// 允许的方法列表，空表示默认全量
    #[serde(default)]
    pub allowed_methods: Vec<String>,
    /// 允许的请求头列表，空表示任意
    #[serde(default)]
    pub allowed_headers: Vec<String>,
    /// 是否允许携带凭证
    #[serde(default)]
    pub allow_credentials: bool,
    /// 预检缓存秒数
    #[serde(default = "default_cors_max_age")]
    pub max_age_secs: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            enable: false,
            allowed_origins: Vec::new(),
            allowed_methods: Vec::new(),
            allowed_headers: Vec::new(),
            allow_credentials: false,
            max_age_secs: default_cors_max_age(),
        }
    }
}

/// Token 鉴权配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub enable: bool,
    /// 预期 Token 值
    #[serde(default)]
    pub token: String,
    /// Token 所在请求头名（默认 authorization）
    #[serde(default = "default_auth_header")]
    pub header_name: String,
    /// 认证方案（Bearer / Basic / 自定义），空表示直接比对 header 原值
    #[serde(default = "default_auth_scheme")]
    pub scheme: String,
    /// 跳过鉴权的路径前缀列表（健康检查、运维接口等）
    #[serde(default)]
    pub skip_paths: Vec<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enable: false,
            token: String::new(),
            header_name: default_auth_header(),
            scheme: default_auth_scheme(),
            skip_paths: Vec::new(),
        }
    }
}

/// 路径改写配置（路由级）
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RewriteConfig {
    #[serde(default)]
    pub enable: bool,
    /// 正则匹配模式
    #[serde(default)]
    pub path_pattern: String,
    /// 替换字符串（支持 $1 $2 捕获组）
    #[serde(default)]
    pub path_replace: String,
    /// 是否替换所有匹配项（false 仅替换首个，true 替换全部）
    #[serde(default)]
    pub replace_all: bool,
    /// 预编译的改写正则（enable=true 时由加载阶段编译，序列化时跳过）
    #[serde(skip)]
    pub compiled_regex: Option<regex::Regex>,
}

// ---- 默认值函数 ----

fn default_env_value() -> String {
    "default".into()
}
fn default_host() -> String {
    constant::DEFAULT_HOST.into()
}
fn default_port() -> u16 {
    constant::DEFAULT_PORT
}
fn default_graceful_shutdown() -> u64 {
    constant::DEFAULT_GRACEFUL_SHUTDOWN_SECS
}
fn default_log_level() -> String {
    "info".into()
}
fn default_log_format() -> String {
    "text".into()
}
fn default_log_output() -> String {
    "console".into()
}
fn default_read_timeout() -> u64 {
    30
}
fn default_write_timeout() -> u64 {
    30
}
fn default_connect_timeout() -> u64 {
    10
}
fn default_max_connections() -> usize {
    10000
}
fn default_request_size_limit() -> usize {
    1_048_576
}
fn default_proxy_timeout() -> u64 {
    30
}
fn default_proxy_connect_timeout() -> u64 {
    10
}
fn default_max_idle_per_host() -> usize {
    50
}
fn default_match_type() -> String {
    "prefix".into()
}
fn default_load_balance() -> String {
    "round_robin".into()
}
fn default_true() -> bool {
    true
}
fn default_rate_per_second() -> u32 {
    1000
}
fn default_rate_burst() -> u32 {
    2000
}
fn default_rate_limit_gc_secs() -> u64 {
    300
}
fn default_metrics_path() -> String {
    "/metrics".into()
}
fn default_health_path() -> String {
    "/health".into()
}
fn default_admin_prefix() -> String {
    "/_admin".into()
}
fn default_cors_max_age() -> u64 {
    600
}
fn default_auth_header() -> String {
    "authorization".into()
}
fn default_auth_scheme() -> String {
    "Bearer".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.server.port, 8080);
        assert!(cfg.monitor.enable_metrics);
    }
}
