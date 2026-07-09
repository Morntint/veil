//! 配置文件热更新监听
//!
//! 基于 notify 监听配置目录变更，去抖后重新加载并校验。
//! 加载或校验失败时保留旧配置（兜底回滚），并记录变更日志。
//! 配置版本号自增，便于运维追踪热更新次数。

use std::path::Path;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing;

use crate::config::{loader, SharedConfig};
use crate::constant;
use crate::utils::{GatewayError, Result};

/// 启动配置热更新监听
///
/// 返回的 `RecommendedWatcher` 需由调用方持有以保持监听存活。
pub fn spawn(shared: SharedConfig, config_dir: String, env: String) -> Result<RecommendedWatcher> {
    let (tx, mut rx) = mpsc::channel::<()>(64);

    // 只关心 default.toml 与 {env}.toml，避免编辑器临时文件(.swp/.tmp 等)触发无谓重载
    let watch_targets: Vec<String> = vec!["default.toml".into(), format!("{env}.toml")];

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    // 按文件名过滤，仅目标配置文件变更才通知
                    let hit = event.paths.iter().any(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| watch_targets.iter().any(|t| t == n))
                            .unwrap_or(false)
                    });
                    if hit {
                        // try_send 不阻塞回调线程；channel 满时丢弃（去抖循环会处理）
                        let _ = tx.try_send(());
                    }
                }
            }
        },
        notify::Config::default(),
    )
    .map_err(|e| GatewayError::config_with_source("创建配置监听器失败", e))?;

    let watch_path = Path::new(&config_dir).to_path_buf();
    watcher
        .watch(&watch_path, RecursiveMode::NonRecursive)
        .map_err(|e| GatewayError::config_with_source(
            format!("监听配置目录 {} 失败", watch_path.display()),
            e,
        ))?;

    tracing::info!(dir = %config_dir, env = %env, "配置热更新监听已启动");

    tokio::spawn(async move {
        loop {
            // 等待首个变更事件
            if rx.recv().await.is_none() {
                break;
            }
            // 去抖：等待静默期并排空后续事件
            tokio::time::sleep(Duration::from_millis(constant::CONFIG_WATCH_DEBOUNCE_MS)).await;
            while rx.try_recv().is_ok() {}

            tracing::info!("检测到配置文件变更，开始热重载");
            match loader::load(&config_dir, &env) {
                Ok(mut new_cfg) => {
                    let old = shared.load_full();
                    new_cfg.version = old.version + 1;
                    tracing::info!(
                        version = new_cfg.version,
                        routes = new_cfg.routes.len(),
                        "配置已热更新"
                    );
                    shared.store(std::sync::Arc::new(new_cfg));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "配置热重载失败，保留旧配置（兜底回滚）");
                }
            }
        }
    });

    Ok(watcher)
}
