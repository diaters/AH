//! Task 32-34: `on_experience_candidate_submitted` / `on_experience_candidate_approved` /
//! `on_experience_candidate_rejected` 观察 hook companion 系统。
//!
//! 由于 `ExperienceCandidate` 存储在 `ExperienceStore` Resource 中而非 ECS Entity，
//! 使用 `PendingExperienceHooks` scratch resource 记录待派发事件。
//!
//! 写入系统将 `(HookPoint, candidate_id)` 推入队列，
//! 本系统逐条派发对应 hook 后清空队列。

use crate::prelude::*;
use tracing::debug;

#[cfg(test)]
use crate::domain::ExperienceStore;
use crate::domain::PendingExperienceHooks;
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

/// 经验候选相关 hook companion 系统。
///
/// 在 `HarnessSet::Execution` 集合中运行，在 `experience_approval_result_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（队列保留，待下帧 registry 存在时处理）。
pub fn on_experience_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集待派发事件，避免在派发 hook 期间借用 world。
    let events: Vec<(HookPoint, uuid::Uuid)> = {
        let mut pending = world.resource_mut::<PendingExperienceHooks>();
        std::mem::take(&mut pending.0)
    };

    if events.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (point, _candidate_id) in &events {
                dispatch_experience_hook(world, &mut registry, *point);
            }
        },
    );

    debug!(
        event = "ExperienceHooksDispatched",
        count = events.len(),
        "experience candidate hooks dispatched"
    );
}

/// 派发经验候选相关 hook 并 flush WorldCommand。
fn dispatch_experience_hook(world: &mut World, registry: &mut PluginRegistry, point: HookPoint) {
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
        event = "ExperienceHookDispatched",
        hook_point = ?point,
        "experience candidate hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::HookPoint;

    #[test]
    fn on_experience_hook_noop_without_registry() {
        let mut world = World::new();
        world.insert_resource(ExperienceStore::default());
        world.insert_resource(PendingExperienceHooks(vec![(
            HookPoint::OnExperienceCandidateSubmitted,
            uuid::Uuid::new_v4(),
        )]));

        on_experience_hook_system(&mut world);

        // 队列保留（系统 noop），待下帧处理。
        let pending = world.resource::<PendingExperienceHooks>();
        assert_eq!(pending.0.len(), 1);
    }

    #[test]
    fn on_experience_hook_empty_registry_drains_queue() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(ExperienceStore::default());
        world.insert_resource(PendingExperienceHooks(vec![
            (
                HookPoint::OnExperienceCandidateSubmitted,
                uuid::Uuid::new_v4(),
            ),
            (
                HookPoint::OnExperienceCandidateApproved,
                uuid::Uuid::new_v4(),
            ),
            (
                HookPoint::OnExperienceCandidateRejected,
                uuid::Uuid::new_v4(),
            ),
        ]));

        on_experience_hook_system(&mut world);

        // 队列应被清空。
        let pending = world.resource::<PendingExperienceHooks>();
        assert!(
            pending.0.is_empty(),
            "PendingExperienceHooks 队列应在 hook 派发后清空"
        );
    }

    #[test]
    fn on_experience_hook_empty_queue_is_noop() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        world.insert_resource(ExperienceStore::default());
        world.insert_resource(PendingExperienceHooks::default());

        on_experience_hook_system(&mut world);

        let pending = world.resource::<PendingExperienceHooks>();
        assert!(pending.0.is_empty());
    }
}
