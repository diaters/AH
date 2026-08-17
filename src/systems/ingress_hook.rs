//! Task 27: `on_message_received` 观察 hook companion 系统。
//!
//! 当外部输入（用户文本、Tool 确认响应）到达时触发。
//! `input_ingress_system` 在 spawn `Signal::user_input` 或 `ToolConfirmationResponseMessage` 时
//! 附带 `MessageReceivedHookPending` 标记，本系统查询带标记的 entity，派发 hook 后移除标记。

use crate::prelude::*;
use tracing::debug;

use crate::domain::MessageReceivedHookPending;
use crate::user_plugins::dispatcher::{
    HookDispatchInput, HookOutcome, PluginContext, SharedHookOutcome, dispatch_hook,
    flush_world_commands,
};
use crate::domain::HookPoint;
use crate::user_plugins::host_api::{
    approval::ApprovalContext, entity_query::WorldSnapshot, entity_write::WorldWriter,
    experience::ExperienceContext, message::MessageContext, plugin_resource::PluginRoots,
    skills_meta::SkillsSnapshot, temp_resource::TempResourceSlot,
};
use crate::user_plugins::registry::PluginRegistry;

/// `on_message_received` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Ingress` 集合中运行，在 `input_ingress_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn on_message_received_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 采集所有带标记的 entity，避免在派发 hook 期间借用 world。
    let targets: Vec<bevy_ecs::entity::Entity> = world
        .query_filtered::<bevy_ecs::entity::Entity, With<MessageReceivedHookPending>>()
        .iter(world)
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for entity in targets {
                dispatch_message_received_hook(world, &mut registry);

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<MessageReceivedHookPending>();
                }
            }
        },
    );
}

/// 派发 `on_message_received` hook 并 flush WorldCommand。
fn dispatch_message_received_hook(world: &mut World, registry: &mut PluginRegistry) {
    let (writer_tx, writer_rx) =
        crossbeam_channel::unbounded::<crate::user_plugins::host_api::entity_write::WorldCommand>();
    let (message_tx, _message_rx) = crossbeam_channel::unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point: HookPoint::OnMessageReceived,
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
        event = "MessageReceivedHookDispatched",
        hook_point = ?HookPoint::OnMessageReceived,
        "on_message_received hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Signal;

    #[test]
    fn on_message_received_noop_without_registry() {
        let mut world = World::new();
        let entity = world
            .spawn((Signal::user_input("hello"), MessageReceivedHookPending))
            .id();

        on_message_received_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&MessageReceivedHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_message_received_empty_registry_removes_marker() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let entity = world
            .spawn((Signal::user_input("hello"), MessageReceivedHookPending))
            .id();

        on_message_received_hook_system(&mut world);

        // 标记应被移除。
        assert!(
            world
                .query::<&MessageReceivedHookPending>()
                .get(&world, entity)
                .is_err(),
            "MessageReceivedHookPending 应在 hook 派发后移除"
        );
        // entity 仍存在（观察 hook 不 despawn）。
        assert!(world.get_entity(entity).is_ok());
    }
}
