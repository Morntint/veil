//! Veil — 轻量化企业级 HTTP API 网关
//!
//! 采用分层架构 + 插件化设计，自底向上：底层基础层、网络传输层、
//! 网关核心层、插件中间件层、监控运维层、对外服务层。

pub mod admin;
pub mod config;
pub mod constant;
pub mod core;
pub mod middleware;
pub mod monitor;
pub mod network;
pub mod utils;
