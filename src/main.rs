//! 程序入口：启动流程、配置加载、热更新、信号监听、优雅启停

use veil::{config, constant, monitor, network};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_dir = std::env::var(constant::env_keys::CONFIG_DIR)
        .unwrap_or_else(|_| constant::DEFAULT_CONFIG_DIR.to_string());
    let env = std::env::var(constant::env_keys::ENV)
        .unwrap_or_else(|_| constant::DEFAULT_ENV.to_string());

    // 加载并校验配置（此时日志尚未初始化，错误由 anyhow 直接返回）
    let shared = config::loader::load_shared(&config_dir, &env)?;

    {
        let cfg = shared.read();
        monitor::log::init(&cfg.log.level, &cfg.log.format);
        tracing::info!(
            version = constant::VERSION,
            env = %cfg.env,
            port = cfg.server.port,
            routes = cfg.routes.len(),
            "Veil 网关启动中"
        );
    }

    // 强制初始化启动时间戳，确保健康检查的 uptime 从程序启动时刻计算
    let _ = monitor::health::started_at();

    // 启动配置热更新监听（watcher 需保持存活）
    let _watcher = config::watcher::spawn(shared.clone(), config_dir, env)?;

    network::server::run(shared).await
}
