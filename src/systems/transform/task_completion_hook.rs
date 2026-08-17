//! Task 完成与失败 hook 派发 System
//!
//! 检测 Task 进入终态（`Done` 或 `Failed`）后派发对应的 `on_task_completed`
//! / `on_task_failed` hook。采用 `Changed<Task>` + 去重 `HashSet<Uuid>` 组合，
//! 不侵入 `mark_done` / `mark_failed` 的调用点。

use std::collections::HashSet;

use crate::prelude::*;
use crossbeam_channel::unbounded;
use tracing::debug;
use uuid::Uuid;

use crate::domain::HookPoint;
use crate::domain::{Task, TaskStatus};
use crate::user_plugins::dispatcher::{
    HookDispatchInput, HookOutcome, PluginContext, SharedHookOutcome, dispatch_hook,
    flush_world_commands,
};
use crate::user_plugins::host_api::{
    approval::ApprovalContext,
    entity_query::WorldSnapshot,
    entity_write::{WorldCommand, WorldWriter},
    experience::ExperienceContext,
    message::MessageContext,
    plugin_resource::PluginRoots,
    skills_meta::SkillsSnapshot,
    temp_resource::TempResourceSlot,
};
use crate::user_plugins::registry::PluginRegistry;

/// 已派发过终态 hook 的 task id 集合，避免同一 task 重复派发。
///
/// Task 一旦进入终态不会回退（`mark_done` / `mark_failed` 是单向的），
/// 但 `Changed<Task>` 可能在终态后仍有更新（例如 `updated_at` 字段刷新），
/// 故以集合去重。
#[derive(Resource, Default)]
pub struct TaskTerminalDispatched(pub HashSet<Uuid>);

/// 终态 hook 派发 companion System。
///
/// 与 `task_termination_system` 一样使用 `Query<..., Changed<Task>>` 检测
/// 新近进入终态的 Task，对每个尚未在 `TaskTerminalDispatched` 中的 task：
///
/// - `TaskStatus::Done` -> 派发 `HookPoint::OnTaskCompleted`
/// - `TaskStatus::Failed(_)` -> 派发 `HookPoint::OnTaskFailed`
///
/// 派发完成后将 task id 写入去重集合。`task_termination_system` 也用
/// `Changed<Task>`，但只读不写，故两者不会互相抑制变更检测。
pub fn task_completion_hook_system(world: &mut World) {
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集即将派发的 Task，避免在 dispatch_hook 期间借用 world。
    let targets: Vec<(Entity, Task)> = world
        .query_filtered::<(Entity, &Task), Changed<Task>>()
        .iter(world)
        .filter(|(_, t)| t.status.is_terminal())
        .map(|(e, t)| (e, t.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    // 取出去重集合，过滤掉已派发的；余下需要派发的个体。
    let mut dispatched = world
        .remove_resource::<TaskTerminalDispatched>()
        .unwrap_or_default();
    let pending: Vec<(Entity, Task)> = targets
        .into_iter()
        .filter(|(_, t)| !dispatched.0.contains(&t.id))
        .collect();

    if pending.is_empty() {
        world.insert_resource(dispatched);
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (_entity, task) in pending {
                let point = match &task.status {
                    TaskStatus::Done => HookPoint::OnTaskCompleted,
                    TaskStatus::Failed(_) => HookPoint::OnTaskFailed,
                    // 过滤器已限定为终态，理论不可达。
                    _ => continue,
                };
                dispatch_terminal_hook(world, &mut registry, &task, point);
                dispatched.0.insert(task.id);
            }
        },
    );

    world.insert_resource(dispatched);
}

/// 对单个 Task 派发终态 hook 并 flush WorldCommand。
fn dispatch_terminal_hook(
    world: &mut World,
    registry: &mut PluginRegistry,
    task: &Task,
    point: HookPoint,
) {
    let (writer_tx, writer_rx) = unbounded::<WorldCommand>();
    let (message_tx, _message_rx) = unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point,
        world,
        registry,
        writer_tx: writer_tx.clone(),
        ctx_builder: Box::new(
            |plugin: &crate::user_plugins::registry::LoadedPlugin, world: &mut World| {
                let local_outcome: SharedHookOutcome =
                    std::sync::Arc::new(std::sync::Mutex::new(HookOutcome::default()));
                PluginContext {
                    snapshot: snap.clone(),
                    writer: WorldWriter::new(writer_tx.clone()),
                    outcome: local_outcome,
                    plugin_roots: PluginRoots::single(plugin.root_dir.clone()),
                    approval: ApprovalContext {
                        current_request_id: None,
                        tx: writer_tx.clone(),
                    },
                    experience: ExperienceContext {
                        store: std::sync::Arc::new(
                            world
                                .get_resource::<crate::domain::ExperienceStore>()
                                .cloned()
                                .unwrap_or_default(),
                        ),
                        tx: writer_tx.clone(),
                    },
                    skills: SkillsSnapshot::empty(),
                    message: MessageContext {
                        plugin_id: plugin.manifest.id.clone(),
                        tx: message_tx.clone(),
                    },
                    temp_resource: TempResourceSlot::new(),
                }
            },
        ),
    };

    let _ = dispatch_hook(input);
    flush_world_commands(world, &writer_rx);

    debug!(
        event = "TerminalHookDispatched",
        task_id = %task.id,
        hook_point = ?point,
        "terminal-state hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, FailureReason, FrontendKind};

    /// 构造一个进度为 Done 的 Task entity 并 spawn 到 world。
    fn spawn_done_task(world: &mut World) -> Uuid {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let mut task = Task::from_user_input("done-task", 0, channel);
        task.id = Uuid::new_v4();
        task.mark_done("ok", chrono::Utc::now());
        world.spawn(task.clone());
        task.id
    }

    fn spawn_failed_task(world: &mut World) -> Uuid {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let mut task = Task::from_user_input("failed-task", 0, channel);
        task.id = Uuid::new_v4();
        // 手动改为 Failed（避免构造 ExecutionError）
        task.status = TaskStatus::Failed(FailureReason::AgentError);
        world.spawn(task.clone());
        task.id
    }

    /// 无 PluginRegistry 时 system 应 noop 不 panic。
    #[test]
    fn noop_without_registry() {
        let mut world = World::new();
        spawn_done_task(&mut world);
        // 不插入 PluginRegistry 也不插入 TaskTerminalDispatched
        task_completion_hook_system(&mut world);
        // 验证不会创建资源
        assert!(!world.contains_resource::<TaskTerminalDispatched>());
    }

    /// 空插件 registry 时也应正常派发并记录 task。
    #[test]
    fn empty_registry_records_dispatched_done() {
        let mut world = World::new();
        let task_id = spawn_done_task(&mut world);
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(TaskTerminalDispatched::default());

        task_completion_hook_system(&mut world);

        let dispatched = world.remove_resource::<TaskTerminalDispatched>().unwrap();
        assert!(dispatched.0.contains(&task_id));
    }

    #[test]
    fn empty_registry_records_dispatched_failed() {
        let mut world = World::new();
        let task_id = spawn_failed_task(&mut world);
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(TaskTerminalDispatched::default());

        task_completion_hook_system(&mut world);

        let dispatched = world.remove_resource::<TaskTerminalDispatched>().unwrap();
        assert!(dispatched.0.contains(&task_id));
    }

    /// 非终态 Task 不应被加入去重集合。
    #[test]
    fn non_terminal_task_not_recorded() {
        let mut world = World::new();
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("pending", 0, channel);
        let task_id = task.id;
        world.spawn(task);
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(TaskTerminalDispatched::default());

        task_completion_hook_system(&mut world);

        let dispatched = world.remove_resource::<TaskTerminalDispatched>().unwrap();
        assert!(!dispatched.0.contains(&task_id));
    }
}
