//! schedule_task 与 scheduler 共享类型
//!
//! `SchedulerState` 统一持有静态路由（来自 `triggers.toml`）与动态定时任务定义
//! （由 `schedule_task` 工具创建，见 [`ScheduleDefinition`]）。`SchedulerStateWatcher`
//! 持有通往 timer scheduler task 的 `watch::Sender`，热加载与动态任务提交都通过
//! `update_scheduler_state` 同步发送。
//!
//! 动态定时任务只有 `SchedulerState.dynamic_tasks` 这一份账本：触发路由、工具快照与
//! 持久化都直接读取它。

use bevy_ecs::prelude::{Resource, World};
use chrono::{DateTime, Utc};
use cron::Schedule;
use tokio::sync::watch;
use uuid::Uuid;

use crate::domain::ScheduleDefinition;
use crate::triggers::config::{TimerConfig, WebhookConfig};

/// 持有通往 timer_scheduler 的 watch sender。
///
/// `default()` 为 `None`，由 `main.rs` 在启动时用 `Some(tx)` 覆盖。
#[derive(Resource, Default)]
pub struct SchedulerStateWatcher(pub Option<watch::Sender<SchedulerState>>);

/// 统一的调度器状态：静态路由 + 动态定时任务定义。
///
/// 字段私有，通过 `update_scheduler_state` 统一修改，避免遗漏 watch 通知。
#[derive(Resource, Clone, Default)]
pub struct SchedulerState {
    static_routes: Option<SchedulerRoutes>,
    dynamic_tasks: Vec<ScheduleDefinition>,
}

/// 静态路由配置（来自 `triggers.toml`）。
#[derive(Debug, Clone)]
pub struct SchedulerRoutes {
    pub timer: TimerConfig,
    pub webhook: WebhookConfig,
}

impl SchedulerState {
    pub fn static_routes(&self) -> Option<&SchedulerRoutes> {
        self.static_routes.as_ref()
    }

    /// 设置静态路由。`reload_triggers_system` 在原子提交阶段调用。
    pub fn set_static_routes(&mut self, routes: SchedulerRoutes) {
        self.static_routes = Some(routes);
    }

    pub fn dynamic_tasks(&self) -> &[ScheduleDefinition] {
        &self.dynamic_tasks
    }

    pub fn dynamic_tasks_mut(&mut self) -> &mut Vec<ScheduleDefinition> {
        &mut self.dynamic_tasks
    }

    /// 按 kind（`scheduled:<id>`）查找定义。
    pub fn find_by_kind(&self, kind: &str) -> Option<&ScheduleDefinition> {
        self.dynamic_tasks.iter().find(|def| def.kind() == kind)
    }

    /// 按 kind 查找定义的可变引用。
    pub fn find_by_kind_mut(&mut self, kind: &str) -> Option<&mut ScheduleDefinition> {
        self.dynamic_tasks.iter_mut().find(|def| def.kind() == kind)
    }
}

/// timer scheduler 内部统一调度的条目。
///
/// `Cron` 使用 `Box<Schedule>` 以避免 `cron::Schedule`（约 248 字节）撑大
/// 整个枚举（clippy::large_enum_variant），与 `ScheduleSpec` 保持一致。
#[derive(Debug, Clone)]
pub enum ScheduledItem {
    Cron {
        kind: String,
        schedule: Box<Schedule>,
    },
    Once {
        id: Uuid,
        kind: String,
        at: DateTime<Utc>,
    },
}

/// 统一修改入口（exclusive system 用）：先 remove 资源，闭包修改，watch send，再 insert。
///
/// 不变量：`SchedulerState` 一切写路径都经此入口或 [`update_scheduler_state_with_watcher`]，
/// 二者共享 [`apply_and_notify`] 实现，watch 只在修改完成后发一次。资源缺失用
/// `unwrap_or_default()` 兜底，watcher 缺失用 `get_resource` 避免 panic。
pub fn update_scheduler_state(world: &mut World, f: impl FnOnce(&mut SchedulerState)) {
    let mut state = world
        .remove_resource::<SchedulerState>()
        .unwrap_or_default();
    let watcher = world
        .get_resource::<SchedulerStateWatcher>()
        .and_then(|w| w.0.as_ref())
        .cloned();
    apply_and_notify(&mut state, watcher.as_ref(), f);
    world.insert_resource(state);
}

/// 统一修改入口（`ResMut` system 用）：直接借用 `SchedulerState` /
/// `SchedulerStateWatcher` 调用，不经过 `&mut World`。
///
/// 用于无法持有 `&mut World` 的 system（如 `trigger_task_routing_system` 持有
/// `ResMut` 借用），保证它也走 [`apply_and_notify`] 共享逻辑。
pub fn update_scheduler_state_with_watcher(
    state: &mut SchedulerState,
    watcher: &SchedulerStateWatcher,
    f: impl FnOnce(&mut SchedulerState),
) {
    apply_and_notify(state, watcher.0.as_ref(), f);
}

/// 共享内部逻辑：执行闭包修改账本，watch 在闭包返回后 send 一次。
fn apply_and_notify(
    state: &mut SchedulerState,
    watcher: Option<&watch::Sender<SchedulerState>>,
    f: impl FnOnce(&mut SchedulerState),
) {
    f(state);
    if let Some(tx) = watcher {
        let _ = tx.send(state.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ScheduleExpr;

    fn sample_definition(name: &str) -> ScheduleDefinition {
        ScheduleDefinition::new(
            Uuid::new_v4(),
            Some(name.to_string()),
            format!("content of {name}"),
            ScheduleExpr::parse("cron:0 8 * * *").unwrap(),
            None,
            Utc::now(),
        )
    }

    #[test]
    fn update_scheduler_state_mutates_state_and_notifies_watcher() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        let (tx, mut rx) = watch::channel(SchedulerState::default());
        world.insert_resource(SchedulerStateWatcher(Some(tx)));

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_definition("t1"));
        });

        assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
        assert!(rx.has_changed().unwrap(), "watcher must be notified");
        assert_eq!(rx.borrow_and_update().dynamic_tasks().len(), 1);
    }

    #[test]
    fn update_scheduler_state_does_not_panic_without_watcher() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_definition("t2"));
        });

        assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    }

    #[test]
    fn update_scheduler_state_does_not_panic_without_state() {
        let mut world = World::new();
        let (tx, _rx) = watch::channel(SchedulerState::default());
        world.insert_resource(SchedulerStateWatcher(Some(tx)));

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_definition("t3"));
        });

        assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    }

    #[test]
    fn update_scheduler_state_preserves_dynamic_tasks_across_calls() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_definition("a"));
        });
        // 第二次调用只设置 static_routes，dynamic_tasks 必须保留
        update_scheduler_state(&mut world, |state| {
            state.set_static_routes(SchedulerRoutes {
                timer: TimerConfig::default(),
                webhook: WebhookConfig::default(),
            });
        });

        let state = world.resource::<SchedulerState>();
        assert_eq!(state.dynamic_tasks().len(), 1);
        assert!(state.static_routes().is_some());
    }

    #[test]
    fn find_by_kind_locates_definition() {
        let mut state = SchedulerState::default();
        let def = sample_definition("forum");
        let kind = def.kind();
        state.dynamic_tasks_mut().push(def);

        assert_eq!(state.find_by_kind(&kind).unwrap().name, "forum");
        assert!(state.find_by_kind("scheduled:missing").is_none());
        state.find_by_kind_mut(&kind).unwrap().enabled = false;
        assert!(!state.dynamic_tasks()[0].enabled);
    }
}
