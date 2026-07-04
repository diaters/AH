//! Task 29-30: `on_long_term_memory_write` / `on_long_term_memory_evicted` 观察 hook companion 系统。
//!
//! - `on_ltm_write_hook_system`: 当长期记忆写入时触发。
//!   由 `init_agent_memory_system` 或运行时写入长期记忆的系统附带 `LtmWriteHookPending` 标记，
//!   本系统查询带标记的 Agent entity，派发 hook 后移除标记。
//!
//! - `on_ltm_evicted_hook_system`: 当长期记忆条目被驱逐时触发。
//!   由 `long_term_memory_decay_system` 在检测到驱逐后附带 `LtmEvictedHookPending` 标记，
//!   本系统查询带标记的 Agent entity，派发 hook 后移除标记。

use crate::prelude::*;
use tracing::debug;

use crate::domain::{Agent, LtmEvictedHookPending, LtmWriteHookPending};
use crate::user_plugins::dispatcher::{
    HookDispatchInput, HookOutcome, PluginContext, SharedHookOutcome, dispatch_hook,
    flush_world_commands,
};
use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::host_api::{
    approval::ApprovalContext, entity_query::WorldSnapshot, entity_write::WorldWriter,
    experience::ExperienceContext, message::MessageContext, plugin_resource::PluginRoots,
    skills_meta::SkillsSnapshot, temp_resource::TempResourceSlot,
};
use crate::user_plugins::registry::PluginRegistry;

/// `on_long_term_memory_write` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Maintenance` 集合中运行，在 `init_agent_memory_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn on_ltm_write_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 Agent entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(bevy_ecs::entity::Entity, Agent)> = world
        .query_filtered::<(bevy_ecs::entity::Entity, &Agent), With<LtmWriteHookPending>>()
        .iter(world)
        .map(|(e, a)| (e, a.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, _agent) in targets {
                dispatch_ltm_hook(world, &mut registry, HookPoint::OnLongTermMemoryWrite);

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<LtmWriteHookPending>();
                }
            }
        },
    );
}

/// `on_long_term_memory_evicted` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Maintenance` 集合中运行，在 `long_term_memory_decay_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn on_ltm_evicted_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 Agent entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(bevy_ecs::entity::Entity, Agent)> = world
        .query_filtered::<(bevy_ecs::entity::Entity, &Agent), With<LtmEvictedHookPending>>()
        .iter(world)
        .map(|(e, a)| (e, a.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, _agent) in targets {
                dispatch_ltm_hook(world, &mut registry, HookPoint::OnLongTermMemoryEvicted);

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<LtmEvictedHookPending>();
                }
            }
        },
    );
}

/// 派发 LTM 相关 hook 并 flush WorldCommand。
fn dispatch_ltm_hook(world: &mut World, registry: &mut PluginRegistry, point: HookPoint) {
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
        event = "LtmHookDispatched",
        hook_point = ?point,
        "LTM hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions};

    fn make_agent() -> Agent {
        Agent {
            id: uuid::Uuid::new_v4(),
            profile: AgentProfile {
                name: "test-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        }
    }

    #[test]
    fn on_ltm_write_noop_without_registry() {
        let mut world = World::new();
        let agent = make_agent();
        let entity = world.spawn((agent, LtmWriteHookPending)).id();

        on_ltm_write_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&LtmWriteHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_ltm_write_empty_registry_removes_marker() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let agent = make_agent();
        let entity = world.spawn((agent, LtmWriteHookPending)).id();

        on_ltm_write_hook_system(&mut world);

        // 标记应被移除。
        assert!(
            world
                .query::<&LtmWriteHookPending>()
                .get(&world, entity)
                .is_err(),
            "LtmWriteHookPending 应在 hook 派发后移除"
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_ltm_evicted_noop_without_registry() {
        let mut world = World::new();
        let agent = make_agent();
        let entity = world.spawn((agent, LtmEvictedHookPending)).id();

        on_ltm_evicted_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&LtmEvictedHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_ltm_evicted_empty_registry_removes_marker() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let agent = make_agent();
        let entity = world.spawn((agent, LtmEvictedHookPending)).id();

        on_ltm_evicted_hook_system(&mut world);

        // 标记应被移除。
        assert!(
            world
                .query::<&LtmEvictedHookPending>()
                .get(&world, entity)
                .is_err(),
            "LtmEvictedHookPending 应在 hook 派发后移除"
        );
        assert!(world.get_entity(entity).is_ok());
    }
}
