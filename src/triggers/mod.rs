//! Webhook 与 Timer 信号注入模块
//!
//! 提供：
//! - `triggers.toml` 配置加载与校验
//! - HTTP webhook server（axum）
//! - cron timer scheduler
//! - 路由热加载（`/reload-triggers`）
//! - 统一的 `SchedulerState`（静态路由 + 动态任务）

pub mod config;
pub mod prompt_template;
pub mod scheduled_task;
pub mod timer_scheduler;
pub mod webhook_server;

pub use config::{
    TimerConfig, TimerRouteConfig, TriggerConfig, WebhookConfig, WebhookRouteConfig,
    build_registry_from_config, build_schedules, load_triggers_config, validate_templates,
};
pub use scheduled_task::{
    DynamicScheduledTask, ScheduleSpec, ScheduledItem, SchedulerRoutes, SchedulerState,
    SchedulerStateWatcher, update_scheduler_state,
};
pub use timer_scheduler::run_timer_scheduler;
pub use webhook_server::run_webhook_server;

use bevy_ecs::prelude::World;
use tracing::{error, info, warn};

use crate::app::HarnessSettings;

/// `/reload-triggers` 系统。
///
/// 遵循原子提交约束（spec L87-104）：解析 → 模板校验 → registry 构建 → cron 校验 → 一次性提交。
/// 任一步骤失败则保留旧值，记日志，不更新任何资源。
///
/// 通过 `update_scheduler_state` 提交静态路由，保留 `SchedulerState.dynamic_tasks` 不变。
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

    update_scheduler_state(world, |state| {
        state.set_static_routes(SchedulerRoutes {
            timer: new_config.timer.clone(),
            webhook: new_config.webhook.clone(),
        });
        // dynamic_tasks 保持不变
    });

    world.insert_resource(new_registry);

    info!(
        event = "TriggersReloaded",
        webhook_count, timer_count, "triggers reloaded successfully"
    );
}
