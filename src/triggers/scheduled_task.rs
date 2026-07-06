//! schedule_task 与 scheduler 共享类型
//!
//! `SchedulerState` 统一持有静态路由（来自 `triggers.toml`）与动态任务
//! （由 `schedule_task` 工具创建）。`SchedulerStateWatcher` 持有通往
//! timer scheduler task 的 `watch::Sender`，热加载与动态任务提交都通过
//! `update_scheduler_state` 同步发送。

use bevy_ecs::prelude::{Resource, World};
use chrono::{DateTime, Utc};
use cron::Schedule;
use tokio::sync::watch;
use uuid::Uuid;

use crate::triggers::config::{TimerConfig, WebhookConfig};

/// 持有通往 timer_scheduler 的 watch sender。
///
/// `default()` 为 `None`，由 `main.rs` 在启动时用 `Some(tx)` 覆盖。
#[derive(Resource, Default)]
pub struct SchedulerStateWatcher(pub Option<watch::Sender<SchedulerState>>);

/// 统一的调度器状态：静态路由 + 动态任务。
///
/// 字段私有，通过 `update_scheduler_state` 统一修改，避免遗漏 watch 通知。
#[derive(Resource, Clone, Default)]
pub struct SchedulerState {
    static_routes: Option<SchedulerRoutes>,
    dynamic_tasks: Vec<DynamicScheduledTask>,
}

/// 静态路由配置（来自 `triggers.toml`）。
#[derive(Debug, Clone)]
pub struct SchedulerRoutes {
    pub timer: TimerConfig,
    pub webhook: WebhookConfig,
}

/// 由 `schedule_task` 工具创建的动态任务条目。
#[derive(Debug, Clone)]
pub struct DynamicScheduledTask {
    pub id: Uuid,
    pub kind: String,
    pub schedule: ScheduleSpec,
    pub created_at: DateTime<Utc>,
}

/// 动态任务调度规格：一次性或 cron 周期。
///
/// `Cron` 使用 `Box<Schedule>` 以避免 `cron::Schedule`（约 248 字节）撑大
/// 整个枚举（clippy::large_enum_variant）。
#[derive(Debug, Clone)]
pub enum ScheduleSpec {
    Once(DateTime<Utc>),
    Cron(Box<Schedule>),
}

impl SchedulerState {
    pub fn static_routes(&self) -> Option<&SchedulerRoutes> {
        self.static_routes.as_ref()
    }

    /// 设置静态路由。`reload_triggers_system` 在原子提交阶段调用。
    pub fn set_static_routes(&mut self, routes: SchedulerRoutes) {
        self.static_routes = Some(routes);
    }

    pub fn dynamic_tasks(&self) -> &[DynamicScheduledTask] {
        &self.dynamic_tasks
    }

    pub fn dynamic_tasks_mut(&mut self) -> &mut Vec<DynamicScheduledTask> {
        &mut self.dynamic_tasks
    }
}

/// 统一修改入口：先 remove_resource，修改，watch send，再 insert_resource。
///
/// 使用 `world.get_resource::<SchedulerStateWatcher>()` 而非 `world.resource()`
/// 以避免 watcher 缺失时 panic。
pub fn update_scheduler_state(world: &mut World, f: impl FnOnce(&mut SchedulerState)) {
    let mut state = world
        .remove_resource::<SchedulerState>()
        .unwrap_or_default();
    f(&mut state);
    if let Some(watcher) = world
        .get_resource::<SchedulerStateWatcher>()
        .and_then(|w| w.0.as_ref())
    {
        let _ = watcher.send(state.clone());
    }
    world.insert_resource(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_dynamic_task(kind: &str) -> DynamicScheduledTask {
        DynamicScheduledTask {
            id: Uuid::new_v4(),
            kind: kind.to_string(),
            schedule: ScheduleSpec::Once(Utc::now() + chrono::Duration::minutes(5)),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn update_scheduler_state_mutates_state_and_notifies_watcher() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        let (tx, mut rx) = watch::channel(SchedulerState::default());
        world.insert_resource(SchedulerStateWatcher(Some(tx)));

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_dynamic_task("t1"));
        });

        assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
        assert!(rx.has_changed().unwrap(), "watcher must be notified");
        assert_eq!(rx.borrow_and_update().dynamic_tasks().len(), 1);
    }

    #[test]
    fn update_scheduler_state_does_not_panic_without_watcher() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        // 故意不插入 SchedulerStateWatcher

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_dynamic_task("t2"));
        });

        assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    }

    #[test]
    fn update_scheduler_state_does_not_panic_without_state() {
        let mut world = World::new();
        let (tx, _rx) = watch::channel(SchedulerState::default());
        world.insert_resource(SchedulerStateWatcher(Some(tx)));
        // 故意不插入 SchedulerState，应使用 default

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_dynamic_task("t3"));
        });

        assert_eq!(world.resource::<SchedulerState>().dynamic_tasks().len(), 1);
    }

    #[test]
    fn update_scheduler_state_preserves_dynamic_tasks_across_calls() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        let (tx, _rx) = watch::channel(SchedulerState::default());
        world.insert_resource(SchedulerStateWatcher(Some(tx)));

        update_scheduler_state(&mut world, |state| {
            state.dynamic_tasks_mut().push(sample_dynamic_task("a"));
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
        assert_eq!(state.dynamic_tasks()[0].kind, "a");
        assert!(state.static_routes().is_some());
    }
}
