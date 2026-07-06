//! Webhook 与 Timer 信号注入模块
//!
//! 提供：
//! - `triggers.toml` 配置加载与校验
//! - HTTP webhook server（axum）
//! - cron timer scheduler
//! - 路由热加载（`/reload-triggers`）

pub mod config;
pub mod prompt_template;
pub mod timer_scheduler;
pub mod webhook_server;

pub use config::{
    TimerConfig, TimerRouteConfig, TriggerConfig, WebhookConfig, WebhookRouteConfig,
    build_registry_from_config, build_schedules, load_triggers_config, validate_templates,
};
pub use timer_scheduler::run_timer_scheduler;
pub use webhook_server::run_webhook_server;

use bevy_ecs::prelude::{Resource, World};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::app::HarnessSettings;

/// 持有 timer_scheduler 的 watch sender。
///
/// `default()` 为 `None`，由 `main.rs` 在启用 triggers 时用 `Some(tx)` 覆盖。
#[derive(Resource, Default)]
pub struct TriggerConfigWatcher(pub Option<watch::Sender<TriggerConfig>>);

/// 持有当前 TriggerConfig 副本，供 `reload_triggers_system` 与诊断使用。
#[derive(Resource, Clone, Default)]
pub struct TriggerConfigState(pub Option<TriggerConfig>);

/// `/reload-triggers` 系统。
///
/// 遵循原子提交约束（spec L87-104）：解析 → 模板校验 → registry 构建 → cron 校验 → 一次性提交。
/// 任一步骤失败则保留旧值，记日志，不更新任何资源。
pub fn reload_triggers_system(world: &mut World) {
    let path = world
        .get_resource::<HarnessSettings>()
        .and_then(|s| s.0.triggers_config_path.as_ref())
        .map(std::path::PathBuf::from);

    let Some(path) = path else {
        warn!(
            event = "TriggersReloadFailed",
            reason = "triggers_config_path not set",
            "skip reload: no triggers_config_path configured"
        );
        return;
    };

    // 步骤 1-2: 读取并解析 TOML
    let new_config = match load_triggers_config(&path) {
        Ok(c) => c,
        Err(e) => {
            error!(
                event = "TriggersReloadFailed",
                error = %e,
                path = %path.display(),
                "failed to load triggers config, keeping old config"
            );
            return;
        }
    };

    // 步骤 3: 模板预校验
    if let Err(e) = validate_templates(&new_config) {
        error!(
            event = "TriggersReloadFailed",
            error = %e,
            path = %path.display(),
            "template validation failed, keeping old config"
        );
        return;
    }

    // 步骤 4: 构建 registry
    let new_registry = match build_registry_from_config(&new_config) {
        Ok(r) => r,
        Err(e) => {
            error!(
                event = "TriggersReloadFailed",
                error = %e,
                path = %path.display(),
                "registry build failed, keeping old config"
            );
            return;
        }
    };

    // 步骤 5: 预校验 cron 表达式
    if let Err(e) = build_schedules(&new_config.timer) {
        error!(
            event = "TriggersReloadFailed",
            error = %e,
            path = %path.display(),
            "cron validation failed, keeping old config"
        );
        return;
    }

    // 步骤 6: 原子提交
    let webhook_count = new_config.webhook.routes.len();
    let timer_count = new_config.timer.routes.len();
    world.insert_resource(new_registry);
    world.resource_mut::<TriggerConfigState>().0 = Some(new_config.clone());
    match world.resource_mut::<TriggerConfigWatcher>().0.as_ref() {
        Some(tx) => {
            if let Err(e) = tx.send(new_config) {
                warn!(
                    event = "TriggerConfigWatcherSendFailed",
                    error = %e,
                    "timer_scheduler receiver dropped, but registry and state updated"
                );
            }
        }
        None => {
            warn!(
                event = "TriggerConfigWatcherMissing",
                "timer_scheduler not running, only registry and state updated"
            );
        }
    }

    info!(
        event = "TriggersReloaded",
        webhook_count, timer_count, "triggers reloaded successfully"
    );
}
