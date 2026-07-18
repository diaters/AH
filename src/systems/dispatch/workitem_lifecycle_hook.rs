//! Task 21-23 companion 系统：WorkItem 生命周期观察 hook 派发。
//!
//! 检测带 `WorkItemLifecycleHookPending` 标记的 WorkItem entity，根据标记内
//! 的 `HookPoint` 派发对应 hook（`OnWorkItemStarted` / `OnWorkItemCompleted` /
//! `OnWorkItemFailed`），flush 累积的 `WorldCommand`，然后移除标记。
//!
//! 采用 companion-system + marker-component 模式，而非 `Changed<WorkItem>` 方案，
//! 原因：
//!
//! - WorkItem 有多个变异点（dispatch、llm_response、domain 方法），`Changed<>` 在
//!   entity 同帧 despawn 后无法被 companion 系统查询到；
//! - marker 组件在变异点插入、companion 系统消费后移除，时序与 despawn 解耦，
//!   即使 WorkItem 随后被 despawn 也不影响 hook 派发。

use std::sync::{Arc, Mutex};

use crate::prelude::*;
use crossbeam_channel::unbounded;
use tracing::debug;

use crate::app::Clock;
use crate::domain::{
    ExperienceStore, PendingExperienceHooks, ProfileGenerationContext, SkillUpdateContext, Task,
    TaskStatus, WaitingReason, WorkItem, WorkItemLifecycleHookPending, WorkItemType,
};
use crate::systems::experience::profile_generation::handle_profile_generation_failure;
use crate::user_plugins::dispatcher::{
    HookDispatchInput, HookOutcome, PluginContext, SharedHookOutcome, dispatch_hook,
    flush_world_commands,
};
use crate::user_plugins::hook_point::HookPoint;
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
use crate::user_plugins::registry::{LoadedPlugin, PluginRegistry};

/// WorkItem 生命周期观察 hook companion 系统。
///
/// 在 `HarnessSet::Dispatch` 集合中运行，在 `dispatch_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn workitem_lifecycle_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 WorkItem entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(Entity, WorkItem, HookPoint)> = world
        .query_filtered::<(Entity, &WorkItem, &WorkItemLifecycleHookPending), ()>()
        .iter(world)
        .map(|(e, wi, marker)| (e, wi.clone(), marker.0))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, work_item, hook_point) in targets {
                dispatch_workitem_lifecycle_hook(world, &mut registry, &work_item, hook_point);

                // 失败特化处理：按 Context Component 分流（设计文档 §2.7 决策 14、§3.7）。
                if hook_point == HookPoint::OnWorkItemFailed {
                    handle_workitem_failure_by_context(world, entity, &work_item);
                }

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<WorkItemLifecycleHookPending>();
                }
            }
        },
    );
}

/// 按 Context Component 分流 WorkItem 失败处理。
///
/// 在 `OnWorkItemFailed` hook 派发后调用，依据 WorkItem Entity 上附加的 Context
/// Component 选择特化路径：
///
/// - `SkillUpdateContext`：候选保持 `GovernanceResolved`（仅日志，不强制降级）。
/// - `ProfileGenerationContext`：调用 `handle_profile_generation_failure`
///   （孵化场景标记候选失败 + 通知用户；更新场景静默跳过）。
/// - 默认（Evaluation / Summarization / ExperienceCollection）：
///   - Evaluation: `Task Waiting(Evaluator)` → `Ready`
///   - Summarization: `Task Waiting(Summarization)` → `Waiting(User)`
///   - ExperienceCollection: 不回滚 Task
///
/// Task 状态恢复逻辑迁移自 `workitem_dispatch.rs`（task 5.1 将删除该文件）。
fn handle_workitem_failure_by_context(world: &mut World, entity: Entity, work_item: &WorkItem) {
    // 1. SkillUpdateContext：候选保持 GovernanceResolved（仅日志，不强制降级）
    if world.get::<SkillUpdateContext>(entity).is_some() {
        debug!(
            event = "SkillUpdateWorkItemFailedContext",
            work_item_id = %work_item.id,
            task_id = %work_item.task_id,
            "skill update work item failed, candidate remains GovernanceResolved"
        );
        return;
    }

    // 2. ProfileGenerationContext：调用 handle_profile_generation_failure
    if let Some(ctx) = world.get::<ProfileGenerationContext>(entity).cloned() {
        debug!(
            event = "ProfileGenerationWorkItemFailedContext",
            work_item_id = %work_item.id,
            task_id = %work_item.task_id,
            kind = ?ctx.kind,
            "profile generation work item failed, invoking failure handler"
        );

        world.resource_scope(|world, mut store: Mut<ExperienceStore>| {
            world.resource_scope(|world, mut pending_hooks: Mut<PendingExperienceHooks>| {
                let mut commands = world.commands();
                handle_profile_generation_failure(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    work_item.task_id,
                    ctx.kind.clone(),
                    "profile-designer Agent not found by dispatch_system",
                    Some(entity),
                );
            });
        });
        world.flush();
        return;
    }

    // 3. 默认：ExperienceCollection 不回滚 Task；Evaluation / Summarization 恢复 Task 状态
    if work_item.work_type == WorkItemType::ExperienceCollection {
        debug!(
            event = "ExperienceCollectionWorkItemFailedNoRollback",
            work_item_id = %work_item.id,
            task_id = %work_item.task_id,
            "experience collection work item failed, no task rollback"
        );
        return;
    }

    let clock = world.resource::<Clock>().0;
    let mut task_query = world.query::<&mut Task>();
    if let Some(mut task) = task_query
        .iter_mut(world)
        .find(|t| t.id == work_item.task_id)
    {
        match task.status {
            TaskStatus::Waiting(WaitingReason::Evaluator) => {
                let old_status = task.status.clone();
                task.status = TaskStatus::Ready;
                task.updated_at = clock;
                debug!(
                    event = "TaskStatusRestoredAfterWorkItemFailed",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    work_type = ?work_item.work_type,
                    "task restored to Ready after work item failed"
                );
            }
            TaskStatus::Waiting(WaitingReason::Summarization) => {
                let old_status = task.status.clone();
                task.status = TaskStatus::Waiting(WaitingReason::User);
                task.updated_at = clock;
                debug!(
                    event = "TaskStatusRestoredAfterWorkItemFailed",
                    task_id = %task.id,
                    from_status = ?old_status,
                    to_status = ?task.status,
                    work_type = ?work_item.work_type,
                    "task restored to Waiting(User) after work item failed"
                );
            }
            _ => {}
        }
    }
}

/// 对单个 WorkItem 派发生命周期 hook 并 flush WorldCommand。
///
/// 每个插件获取独立的 `SharedHookOutcome` / `WorldWriter`，不复用跨请求状态。
fn dispatch_workitem_lifecycle_hook(
    world: &mut World,
    registry: &mut PluginRegistry,
    work_item: &WorkItem,
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
        ctx_builder: Box::new(|plugin: &LoadedPlugin, world: &mut World| {
            let local_outcome: SharedHookOutcome = Arc::new(Mutex::new(HookOutcome::default()));
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
                    store: Arc::new(
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
        }),
    };

    let _ = dispatch_hook(input);
    flush_world_commands(world, &writer_rx);

    debug!(
        event = "WorkItemLifecycleHookDispatched",
        work_item_id = %work_item.id,
        task_id = %work_item.task_id,
        hook_point = ?point,
        "work item lifecycle hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        WorkItemInput, WorkItemOrigin, WorkItemStatus, WorkItemType, WorkItemWritebackTarget,
    };

    /// 构造一个占位 WorkItem 用于测试派发路径。
    fn make_work_item(status: WorkItemStatus) -> WorkItem {
        let mut wi = WorkItem::new(
            uuid::Uuid::nil(),
            WorkItemType::Evaluation,
            WorkItemInput::new("test".to_string()),
            WorkItemOrigin::Evaluation,
            WorkItemWritebackTarget::TaskResult,
        );
        wi.status = status;
        wi
    }

    /// 无 PluginRegistry 时 companion 系统应 noop 不 panic。
    #[test]
    fn noop_without_registry() {
        let mut world = World::new();
        let work_item = make_work_item(WorkItemStatus::Running);
        let entity = world
            .spawn((
                work_item,
                WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted),
            ))
            .id();

        workitem_lifecycle_hook_system(&mut world);

        // 标记仍在（companion 系统从未运行）。
        assert!(
            world
                .query::<&WorkItemLifecycleHookPending>()
                .get(&world, entity)
                .is_ok()
        );
    }

    /// 空插件 registry 时应正常派发并移除 OnWorkItemStarted 标记。
    #[test]
    fn empty_registry_removes_started_marker() {
        let mut world = World::new();
        let work_item = make_work_item(WorkItemStatus::Running);
        let entity = world
            .spawn((
                work_item,
                WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted),
            ))
            .id();
        world.insert_resource(PluginRegistry::default());

        workitem_lifecycle_hook_system(&mut world);

        assert!(
            world
                .query::<&WorkItemLifecycleHookPending>()
                .get(&world, entity)
                .is_err(),
            "应移除 WorkItemLifecycleHookPending 标记"
        );
    }

    /// 空插件 registry 时应正常派发并移除 OnWorkItemCompleted 标记。
    #[test]
    fn empty_registry_removes_completed_marker() {
        let mut world = World::new();
        let work_item = make_work_item(WorkItemStatus::Completed);
        let entity = world
            .spawn((
                work_item,
                WorkItemLifecycleHookPending(HookPoint::OnWorkItemCompleted),
            ))
            .id();
        world.insert_resource(PluginRegistry::default());

        workitem_lifecycle_hook_system(&mut world);

        assert!(
            world
                .query::<&WorkItemLifecycleHookPending>()
                .get(&world, entity)
                .is_err(),
            "应移除 WorkItemLifecycleHookPending 标记"
        );
    }

    /// 空插件 registry 时应正常派发并移除 OnWorkItemFailed 标记。
    #[test]
    fn empty_registry_removes_failed_marker() {
        let mut world = World::new();
        let work_item = make_work_item(WorkItemStatus::Failed);
        let entity = world
            .spawn((
                work_item,
                WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed),
            ))
            .id();
        world.insert_resource(PluginRegistry::default());
        // OnWorkItemFailed 分流到默认分支时需要 Clock 资源恢复 Task 状态。
        world.insert_resource(Clock::default());

        workitem_lifecycle_hook_system(&mut world);

        assert!(
            world
                .query::<&WorkItemLifecycleHookPending>()
                .get(&world, entity)
                .is_err(),
            "应移除 WorkItemLifecycleHookPending 标记"
        );
    }
}
