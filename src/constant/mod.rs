//! 全局常量定义

/// 网关版本号（取自 Cargo.toml）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 默认服务监听地址
pub const DEFAULT_HOST: &str = "0.0.0.0";
/// 默认服务监听端口
pub const DEFAULT_PORT: u16 = 8080;

/// 默认优雅关闭超时（秒）
pub const DEFAULT_GRACEFUL_SHUTDOWN_SECS: u64 = 30;

/// 默认配置文件目录
pub const DEFAULT_CONFIG_DIR: &str = "config";
/// 默认运行环境
pub const DEFAULT_ENV: &str = "default";

/// 环境变量名集合
pub mod env_keys {
    /// 运行环境：default / dev / prod
    pub const ENV: &str = "VEIL_ENV";
    /// 配置文件目录
    pub const CONFIG_DIR: &str = "VEIL_CONFIG_DIR";
    /// 服务监听端口覆盖
    pub const SERVER_PORT: &str = "VEIL_SERVER_PORT";
    /// 服务监听地址覆盖
    pub const HOST: &str = "VEIL_HOST";
}
