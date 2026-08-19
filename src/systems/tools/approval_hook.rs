//! Task 35-36: `on_approval_requested` / `on_approval_resolved` 观察 hook companion 系统。
//!
//! - `on_approval_requested_hook_system`: 当工具审批请求创建时触发。
//!   由 `tool_dispatch_system` 在 spawn `ApprovalRequestMessage` 时附带
//!   `ApprovalRequestedHookPending` 标记，本系统查询带标记的 entity，派发 hook 后移除标记。
//!
//! - `on_approval_resolved_hook_system`: 当工具审批结果产生时触发。
//!   由 `approval_dispatch_system` 在 spawn `ApprovalResultMessage` 时附带
//!   `ApprovalResolvedHookPending` 标记，本系统查询带标记的 entity，派发 hook 后移除标记。

use crate::prelude::*;
use tracing::debug;

use crate::domain::HookPoint;
use crate::domain::{
    ApprovalRequestMessage, ApprovalRequestedHookPending, ApprovalResolvedHookPending,
    ApprovalResultMessage,
};
use crate::user_plugins::dispatcher::{
    HookDispatchInput, HookOutcome, PluginContext, SharedHookOutcome, dispatch_hook,
    flush_world_commands,
};
use crate::user_plugins::host_api::{
    approval::ApprovalContext, entity_query::WorldSnapshot, entity_write::WorldWriter,
    experience::ExperienceContext, message::MessageContext, plugin_resource::PluginRoots,
    skills_meta::SkillsSnapshot, temp_resource::TempResourceSlot, tool_control::ToolCallContext,
};
use crate::user_plugins::registry::PluginRegistry;

/// `on_approval_requested` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Dispatch` 集合中运行，在 `approval_dispatch_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn on_approval_requested_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(bevy_ecs::entity::Entity, ApprovalRequestMessage)> = world
        .query_filtered::<(bevy_ecs::entity::Entity, &ApprovalRequestMessage), With<ApprovalRequestedHookPending>>()
        .iter(world)
        .map(|(e, msg)| (e, msg.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, msg) in targets {
                dispatch_approval_hook(
                    world,
                    &mut registry,
                    HookPoint::OnApprovalRequested,
                    Some(msg.request_id),
                );

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<ApprovalRequestedHookPending>();
                }
            }
        },
    );
}

/// `on_approval_resolved` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Transform` 集合中运行，在 `approval_result_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn on_approval_resolved_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(bevy_ecs::entity::Entity, ApprovalResultMessage)> = world
        .query_filtered::<(bevy_ecs::entity::Entity, &ApprovalResultMessage), With<ApprovalResolvedHookPending>>()
        .iter(world)
        .map(|(e, msg)| (e, msg.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, msg) in targets {
                dispatch_approval_hook(
                    world,
                    &mut registry,
                    HookPoint::OnApprovalResolved,
                    Some(msg.request_id),
                );

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<ApprovalResolvedHookPending>();
                }
            }
        },
    );
}

/// 派发审批相关 hook 并 flush WorldCommand。
fn dispatch_approval_hook(
    world: &mut World,
    registry: &mut PluginRegistry,
    point: HookPoint,
    current_request_id: Option<uuid::Uuid>,
) {
    let (writer_tx, writer_rx) =
        crossbeam_channel::unbounded::<crate::user_plugins::host_api::entity_write::WorldCommand>();
    let (message_tx, _message_rx) = crossbeam_channel::unbounded();
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
                    approval: ApprovalContext { current_request_id },
                    experience: ExperienceContext {
                        store: std::sync::Arc::new(
                            world
                                .get_resource::<crate::domain::ExperienceStore>()
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    },
                    skills: SkillsSnapshot::empty(),
                    message: MessageContext {
                        plugin_id: plugin.manifest.id.clone(),
                        tx: message_tx.clone(),
                    },
                    temp_resource: TempResourceSlot::new(),
                    tool: ToolCallContext::default(),
                }
            },
        ),
    };

    let _ = dispatch_hook(input);
    flush_world_commands(world, &writer_rx);

    debug!(
        event = "ApprovalHookDispatched",
        hook_point = ?point,
        "approval hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ApprovalDecision, GrantMode};

    fn make_approval_request() -> ApprovalRequestMessage {
        ApprovalRequestMessage {
            request_id: uuid::Uuid::new_v4(),
            source_task_id: crate::domain::TaskId::nil(),
            approval_task_id: crate::domain::TaskId::new(),
            parent_agent_id: crate::domain::AgentId::nil(),
            child_agent_id: crate::domain::AgentId::nil(),
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"command": "ls"}),
            context: String::new(),
        }
    }

    fn make_approval_result() -> ApprovalResultMessage {
        ApprovalResultMessage {
            request_id: uuid::Uuid::new_v4(),
            source_task_id: crate::domain::TaskId::nil(),
            approval_task_id: crate::domain::TaskId::new(),
            decision: ApprovalDecision::Approved,
            reasoning: "test".to_string(),
            grant_mode: GrantMode::Once,
        }
    }

    #[test]
    fn on_approval_requested_noop_without_registry() {
        let mut world = World::new();
        let entity = world
            .spawn((make_approval_request(), ApprovalRequestedHookPending))
            .id();

        on_approval_requested_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&ApprovalRequestedHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_approval_requested_empty_registry_removes_marker() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let entity = world
            .spawn((make_approval_request(), ApprovalRequestedHookPending))
            .id();

        on_approval_requested_hook_system(&mut world);

        // 标记应被移除。
        assert!(
            world
                .query::<&ApprovalRequestedHookPending>()
                .get(&world, entity)
                .is_err(),
            "ApprovalRequestedHookPending 应在 hook 派发后移除"
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_approval_resolved_noop_without_registry() {
        let mut world = World::new();
        let entity = world
            .spawn((make_approval_result(), ApprovalResolvedHookPending))
            .id();

        on_approval_resolved_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&ApprovalResolvedHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_approval_resolved_empty_registry_removes_marker() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let entity = world
            .spawn((make_approval_result(), ApprovalResolvedHookPending))
            .id();

        on_approval_resolved_hook_system(&mut world);

        // 标记应被移除。
        assert!(
            world
                .query::<&ApprovalResolvedHookPending>()
                .get(&world, entity)
                .is_err(),
            "ApprovalResolvedHookPending 应在 hook 派发后移除"
        );
        assert!(world.get_entity(entity).is_ok());
    }
}
