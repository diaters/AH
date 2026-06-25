//! Task 31: `on_shared_knowledge_write` 观察 hook companion 系统。
//!
//! 由于 `SharedKnowledgeBase` 是 Resource 而非 Entity，无法附带 Component 标记，
//! 因此使用 `PendingKnowledgeWriteHooks` scratch resource 作为写入事件队列。
//!
//! 写入系统（如 `command_parse_system`）将条目推入队列，
//! 本系统逐条派发 `on_shared_knowledge_write` hook 后清空队列。

use bevy::prelude::*;
use tracing::debug;

use crate::domain::{PendingKnowledgeWriteHooks, SharedKnowledgeEntry};
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

/// `on_shared_knowledge_write` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Transform` 集合中运行，在 `command_parse_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（队列保留，待下帧 registry 存在时处理）。
pub fn on_shared_knowledge_write_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集待派发条目，避免在派发 hook 期间借用 world。
    let entries: Vec<SharedKnowledgeEntry> = {
        let mut pending = world.resource_mut::<PendingKnowledgeWriteHooks>();
        std::mem::take(&mut pending.0)
    };

    if entries.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy::ecs::change_detection::Mut<PluginRegistry>| {
            for _entry in &entries {
                dispatch_shared_knowledge_write_hook(world, &mut registry);
            }
        },
    );

    debug!(
        event = "SharedKnowledgeWriteHooksDispatched",
        count = entries.len(),
        "on_shared_knowledge_write hooks dispatched"
    );
}

/// 派发 `on_shared_knowledge_write` hook 并 flush WorldCommand。
fn dispatch_shared_knowledge_write_hook(world: &mut World, registry: &mut PluginRegistry) {
    let (writer_tx, writer_rx) =
        crossbeam_channel::unbounded::<crate::user_plugins::host_api::entity_write::WorldCommand>();
    let (message_tx, _message_rx) = crossbeam_channel::unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point: HookPoint::OnSharedKnowledgeWrite,
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
        event = "SharedKnowledgeWriteHookDispatched",
        hook_point = ?HookPoint::OnSharedKnowledgeWrite,
        "on_shared_knowledge_write hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SharedKnowledgeEntry;

    #[test]
    fn on_shared_knowledge_write_noop_without_registry() {
        let mut world = World::new();
        world.insert_resource(PendingKnowledgeWriteHooks(vec![
            SharedKnowledgeEntry::approved_from_user_input("test"),
        ]));

        on_shared_knowledge_write_hook_system(&mut world);

        // 队列保留（系统 noop），待下帧处理。
        let pending = world.resource::<PendingKnowledgeWriteHooks>();
        assert_eq!(pending.0.len(), 1);
    }

    #[test]
    fn on_shared_knowledge_write_empty_registry_drains_queue() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(PendingKnowledgeWriteHooks(vec![
            SharedKnowledgeEntry::approved_from_user_input("test entry 1"),
            SharedKnowledgeEntry::approved_from_user_input("test entry 2"),
        ]));

        on_shared_knowledge_write_hook_system(&mut world);

        // 队列应被清空。
        let pending = world.resource::<PendingKnowledgeWriteHooks>();
        assert!(
            pending.0.is_empty(),
            "PendingKnowledgeWriteHooks 队列应在 hook 派发后清空"
        );
    }

    #[test]
    fn on_shared_knowledge_write_empty_queue_is_noop() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(PendingKnowledgeWriteHooks::default());

        on_shared_knowledge_write_hook_system(&mut world);

        let pending = world.resource::<PendingKnowledgeWriteHooks>();
        assert!(pending.0.is_empty());
    }
}
