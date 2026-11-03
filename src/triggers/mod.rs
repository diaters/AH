//! Webhook 与定时信号注入模块
//!
//! 提供：
//! - `triggers.toml` webhook 配置加载与校验
//! - HTTP webhook server（axum）
//! - 动态定时任务（`schedule_task`）的 timer scheduler 与 `SchedulerState`
//! - webhook 路由热加载（`/reload-triggers`）

pub mod config;
pub mod scheduled_task;
pub mod timer_scheduler;
pub mod webhook_server;

pub use crate::domain::ScheduleSpec;
pub use config::{
    TriggerConfig, WebhookConfig, WebhookRouteConfig, build_registry_from_config,
    load_triggers_config, validate_templates,
};
pub use scheduled_task::{
    ScheduledItem, SchedulerState, SchedulerStateWatcher, update_scheduler_state,
    update_scheduler_state_with_watcher,
};
pub use timer_scheduler::run_timer_scheduler;
pub use webhook_server::run_webhook_server;

use bevy_ecs::prelude::World;
use tracing::{error, info, warn};

use crate::domain::TriggersConfigPath;

/// `/reload-triggers` 系统。
///
/// 原子提交：解析 → 模板校验 → registry 构建 → 一次性替换 `SignalTriggerRegistry`。
/// 任一步骤失败则保留旧值，记日志，不更新任何资源。
///
/// 只刷新 webhook 路由；webhook server 的监听地址与 token 在启动时确定，
/// 动态定时任务（`SchedulerState`）不受影响。
pub fn reload_triggers_system(world: &mut World) {
    let path = world
        .get_resource::<TriggersConfigPath>()
        .and_then(|s| s.0.as_ref())
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

    // 步骤 5: 原子提交
    let webhook_count = new_config.webhook.routes.len();
    world.insert_resource(new_registry);

    info!(
        event = "TriggersReloaded",
        webhook_count, "triggers reloaded successfully"
    );
}
