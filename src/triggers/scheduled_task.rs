//! schedule_task 与 scheduler 共享类型
//!
//! `SchedulerState` 统一持有静态路由（来自 `triggers.toml`）与动态任务
//! （由 `schedule_task` 工具创建）。`SchedulerStateWatcher` 持有通往
//! timer scheduler task 的 `watch::Sender`，热加载与动态任务提交都通过
//! `update_scheduler_state` 同步发送。

use std::collections::HashMap;

use bevy_ecs::prelude::{Resource, World};
use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use tokio::sync::watch;
use uuid::Uuid;

use crate::domain::{ChannelId, TaskRoutingPolicy};
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

/// 统一修改入口（exclusive system 用）：先 remove 两个资源，闭包修改，watch send，
/// 再 insert 两个资源。
///
/// D10 不变量：`SchedulerState` 与 `ScheduledTaskRegistry` 一切写路径都经此入口或
/// [`update_scheduler_state_with_watcher`]，二者共享 [`apply_and_notify`] 实现，
/// watch 只在两个资源都改完后发一次。资源缺失用 `unwrap_or_default()` 兜底
/// （与既有行为一致），watcher 缺失用 `get_resource` 避免 panic。
pub fn update_scheduler_state(
    world: &mut World,
    f: impl FnOnce(&mut SchedulerState, &mut ScheduledTaskRegistry),
) {
    let mut state = world
        .remove_resource::<SchedulerState>()
        .unwrap_or_default();
    let mut registry = world
        .remove_resource::<ScheduledTaskRegistry>()
        .unwrap_or_default();
    let watcher = world
        .get_resource::<SchedulerStateWatcher>()
        .and_then(|w| w.0.as_ref())
        .cloned();
    apply_and_notify(&mut state, &mut registry, watcher.as_ref(), f);
    world.insert_resource(registry);
    world.insert_resource(state);
}

/// 统一修改入口（`ResMut` system 用）：直接借用 `SchedulerState` /
/// `ScheduledTaskRegistry` / `SchedulerStateWatcher` 调用，不经过 `&mut World`。
///
/// 用于无法持有 `&mut World` 的 system（如 `trigger_task_routing_system` 持有
/// `ResMut` 借用），保证它也走 [`apply_and_notify`] 共享逻辑，不变量 4 字面成立。
pub fn update_scheduler_state_with_watcher(
    state: &mut SchedulerState,
    registry: &mut ScheduledTaskRegistry,
    watcher: &SchedulerStateWatcher,
    f: impl FnOnce(&mut SchedulerState, &mut ScheduledTaskRegistry),
) {
    apply_and_notify(state, registry, watcher.0.as_ref(), f);
}

/// 共享内部逻辑：执行闭包修改两条账本，watch 在闭包返回后 send 一次。
///
/// 两个 `update_scheduler_state*` 公开入口都经此函数，确保「双账本单一修改入口 +
/// watch 单次广播」的契约有唯一实现点。
fn apply_and_notify(
    state: &mut SchedulerState,
    registry: &mut ScheduledTaskRegistry,
    watcher: Option<&watch::Sender<SchedulerState>>,
    f: impl FnOnce(&mut SchedulerState, &mut ScheduledTaskRegistry),
) {
    f(state, registry);
    if let Some(tx) = watcher {
        let _ = tx.send(state.clone());
    }
}

/// 计算 `ScheduleSpec` 的下一次触发时间（UTC）。
///
/// - `Once(at)` 直接返回 `Some(at)`
/// - `Cron(schedule)` 通过 `Local` 时区计算下一次触发，再转换为 UTC；
///   若 cron 无下一次触发（理论上不会发生，因为 cron 表达式永远匹配未来某个时刻），
///   则返回 `None`
///
/// 仅依赖 `ScheduleSpec`，故与类型同住；list 工具与 commit 系统共用。
pub(crate) fn compute_next_trigger(schedule: &ScheduleSpec) -> Option<DateTime<Utc>> {
    match schedule {
        ScheduleSpec::Once(at) => Some(*at),
        ScheduleSpec::Cron(schedule) => schedule
            .upcoming(Local)
            .next()
            .map(|t| t.with_timezone(&Utc)),
    }
}

/// schedule_task 工具创建的动态任务元信息。
///
/// `tasks` 按 `kind` 索引。一次性任务（`is_once == true`）触发后由调度器
/// 通过 `remove` 清理；cron 任务保留在 registry 中以便反复触发。
#[derive(Resource, Default, Debug, Clone)]
pub struct ScheduledTaskRegistry {
    tasks: HashMap<String, ScheduledTaskInfo>,
}

/// 单条动态任务的描述：内容、输出通道、是否一次性。
#[derive(Debug, Clone)]
pub struct ScheduledTaskInfo {
    pub content: String,
    pub output_channel: Option<ChannelId>,
    /// true 表示一次性任务，触发后需清理；false 表示 cron 任务，保留在 registry 中
    pub is_once: bool,
}

impl ScheduledTaskInfo {
    pub fn build_task_input(&self) -> String {
        self.content.clone()
    }

    pub fn build_routing_policy(&self) -> TaskRoutingPolicy {
        TaskRoutingPolicy::scheduled_task(self.output_channel.clone(), "scheduled task")
    }
}

impl ScheduledTaskRegistry {
    pub fn insert(&mut self, kind: impl Into<String>, info: ScheduledTaskInfo) {
        self.tasks.insert(kind.into(), info);
    }

    pub fn get(&self, kind: &str) -> Option<&ScheduledTaskInfo> {
        self.tasks.get(kind)
    }

    pub fn remove(&mut self, kind: &str) -> Option<ScheduledTaskInfo> {
        self.tasks.remove(kind)
    }

    /// 只读迭代器。dispatch 构快照用；写路径仍只走 `update_scheduler_state`。
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ScheduledTaskInfo)> {
        self.tasks.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use std::str::FromStr;

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

        update_scheduler_state(&mut world, |state, _registry| {
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

        update_scheduler_state(&mut world, |state, _registry| {
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

        update_scheduler_state(&mut world, |state, _registry| {
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

        update_scheduler_state(&mut world, |state, _registry| {
            state.dynamic_tasks_mut().push(sample_dynamic_task("a"));
        });
        // 第二次调用只设置 static_routes，dynamic_tasks 必须保留
        update_scheduler_state(&mut world, |state, _registry| {
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

    fn sample_channel() -> ChannelId {
        ChannelId {
            frontend: crate::domain::FrontendKind::Tui,
            user_id: "test-user".to_string(),
            thread_id: None,
        }
    }

    fn sample_info(content: &str, is_once: bool) -> ScheduledTaskInfo {
        ScheduledTaskInfo {
            content: content.to_string(),
            output_channel: Some(sample_channel()),
            is_once,
        }
    }

    #[test]
    fn registry_insert_get_remove_roundtrip() {
        let mut registry = ScheduledTaskRegistry::default();
        assert!(registry.get("missing").is_none());

        registry.insert("cron-task", sample_info("hello cron", false));
        let info = registry
            .get("cron-task")
            .expect("inserted task must be retrievable");
        assert_eq!(info.content, "hello cron");
        assert!(!info.is_once);
        assert!(info.output_channel.is_some());

        let removed = registry
            .remove("cron-task")
            .expect("remove must return stored info");
        assert_eq!(removed.content, "hello cron");
        assert!(registry.get("cron-task").is_none());
    }

    #[test]
    fn registry_insert_overwrites_existing_kind() {
        let mut registry = ScheduledTaskRegistry::default();
        registry.insert("kind", sample_info("first", false));
        registry.insert("kind", sample_info("second", true));
        let info = registry
            .get("kind")
            .expect("kind must exist after overwrite");
        assert_eq!(info.content, "second");
        assert!(info.is_once);
    }

    #[test]
    fn registry_remove_returns_none_for_missing_kind() {
        let mut registry = ScheduledTaskRegistry::default();
        assert!(registry.remove("absent").is_none());
    }

    #[test]
    fn build_task_input_returns_content_clone() {
        let info = sample_info("do something", true);
        let input = info.build_task_input();
        assert_eq!(input, "do something");
        // 修改返回值不应影响原 content
        let _ = input.clone();
        assert_eq!(info.content, "do something");
    }

    #[test]
    fn build_routing_policy_uses_scheduled_task_constructor() {
        let channel = sample_channel();
        let info = ScheduledTaskInfo {
            content: "x".to_string(),
            output_channel: Some(channel.clone()),
            is_once: false,
        };
        let policy = info.build_routing_policy();
        assert_eq!(policy.output_channel, Some(channel.clone()));
        assert_eq!(policy.approval_channel, Some(channel));
        assert_eq!(policy.approval_context.as_deref(), Some("scheduled task"));
    }

    #[test]
    fn build_routing_policy_supports_no_output_channel() {
        let info = ScheduledTaskInfo {
            content: "y".to_string(),
            output_channel: None,
            is_once: true,
        };
        let policy = info.build_routing_policy();
        assert!(policy.output_channel.is_none());
        assert!(policy.approval_channel.is_none());
    }

    #[test]
    fn registry_is_default_constructable_as_resource() {
        let registry = ScheduledTaskRegistry::default();
        assert!(registry.get("anything").is_none());
    }

    /// `compute_next_trigger` 对 `Once(at)` 直接返回 `Some(at)`。
    #[test]
    fn compute_next_trigger_for_once_returns_some_at() {
        let at = Utc::now() + chrono::Duration::days(7);
        let schedule = ScheduleSpec::Once(at);
        let next = compute_next_trigger(&schedule);
        assert_eq!(next, Some(at));
    }

    /// `compute_next_trigger` 对 `Cron(schedule)` 返回下一次本地时区触发时间（转 UTC）。
    /// 工作日 9:00 cron 至少存在一个未来触发点。
    #[test]
    fn compute_next_trigger_for_cron_returns_next_upcoming() {
        let cron_schedule = cron::Schedule::from_str("0 0 9 * * * *").unwrap();
        let schedule = ScheduleSpec::Cron(Box::new(cron_schedule));
        let next = compute_next_trigger(&schedule).expect("cron must have a next trigger");
        // 转回 Local 验证小时为 9
        let local_next = next.with_timezone(&Local);
        assert_eq!(local_next.hour(), 9, "next trigger should be at local 9:00");
    }
}
