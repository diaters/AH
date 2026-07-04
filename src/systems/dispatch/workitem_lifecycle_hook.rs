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

use crate::domain::{WorkItem, WorkItemLifecycleHookPending};
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
/// 在 `HarnessSet::Dispatch` 集合中运行，在 `workitem_dispatch_system` 之后执行。
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

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<WorkItemLifecycleHookPending>();
                }
            }
        },
    );
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
    use crate::contracts::TagSet;
    use crate::domain::{
        WorkItemInput, WorkItemOrigin, WorkItemStatus, WorkItemType, WorkItemWritebackTarget,
    };

    /// 构造一个占位 WorkItem 用于测试派发路径。
    fn make_work_item(status: WorkItemStatus) -> WorkItem {
        let mut wi = WorkItem::new(
            uuid::Uuid::nil(),
            WorkItemType::Evaluation,
            WorkItemInput::new("test".to_string()),
            TagSet::empty(),
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
