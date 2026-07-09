//! 插件中间件模块（企业级能力）
//!
//! 洋葱模型中间件机制，支持按需启用、顺序配置、动态开关。
//! 阶段四实现：CORS、鉴权、限流、IP黑白名单、重试、改写。

pub mod auth;
pub mod cors;
pub mod ip_blacklist;
pub mod rate_limit;
pub mod retry;
pub mod rewrite;
