//! Task 28: `on_llm_response` 观察 hook companion 系统。
//!
//! 当 LLM 执行结果被接收时触发。
//! `ingest_execution_results_system` 在 spawn `AgentExecutionResultMessage` 时附带
//! `LlmResponseHookPending` 标记，本系统查询带标记的 entity，派发 hook 后移除标记。

use bevy::prelude::*;
use tracing::debug;

use crate::domain::ExperienceStore;
use crate::domain::{AgentExecutionResultMessage, LlmResponseHookPending};
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

/// `on_llm_response` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Transform` 集合中运行，在 `ingest_execution_results_system` 之后执行。
/// 无 `PluginRegistry` 时 noop 不 panic（标记保留，待下帧 registry 存在时清理，
/// 或随 entity despawn 自然消失）。
pub fn on_llm_response_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记的 entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(bevy::ecs::entity::Entity, AgentExecutionResultMessage)> = world
        .query_filtered::<(bevy::ecs::entity::Entity, &AgentExecutionResultMessage), With<LlmResponseHookPending>>()
        .iter(world)
        .map(|(e, msg)| (e, msg.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy::ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, _msg) in targets {
                dispatch_llm_response_hook(world, &mut registry);

                // 移除标记。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<LlmResponseHookPending>();
                }
            }
        },
    );
}

/// 派发 `on_llm_response` hook 并 flush WorldCommand。
fn dispatch_llm_response_hook(world: &mut World, registry: &mut PluginRegistry) {
    let (writer_tx, writer_rx) =
        crossbeam_channel::unbounded::<crate::user_plugins::host_api::entity_write::WorldCommand>();
    let (message_tx, _message_rx) = crossbeam_channel::unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point: HookPoint::OnLlmResponse,
        world,
        registry,
        writer_tx: writer_tx.clone(),
        ctx_builder: Box::new(|plugin: &crate::user_plugins::registry::LoadedPlugin, _| {
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
                    store: std::sync::Arc::new(ExperienceStore::default()),
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
        event = "LlmResponseHookDispatched",
        hook_point = ?HookPoint::OnLlmResponse,
        "on_llm_response hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentExecutionOutput, AgentExecutionResult, OutputContent};

    fn make_test_result() -> AgentExecutionResult {
        AgentExecutionResult {
            task_id: uuid::Uuid::nil(),
            agent_id: uuid::Uuid::nil(),
            request_kind: crate::domain::AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text("test".to_string()),
                reasoning_content: None,
            }),
            prompt: "test".to_string(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: None,
        }
    }

    #[test]
    fn on_llm_response_noop_without_registry() {
        let mut world = World::new();
        let msg = AgentExecutionResultMessage {
            result: make_test_result(),
        };
        let entity = world.spawn((msg, LlmResponseHookPending)).id();

        on_llm_response_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&LlmResponseHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    #[test]
    fn on_llm_response_empty_registry_removes_marker() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let msg = AgentExecutionResultMessage {
            result: make_test_result(),
        };
        let entity = world.spawn((msg, LlmResponseHookPending)).id();

        on_llm_response_hook_system(&mut world);

        // 标记应被移除。
        assert!(
            world
                .query::<&LlmResponseHookPending>()
                .get(&world, entity)
                .is_err(),
            "LlmResponseHookPending 应在 hook 派发后移除"
        );
        // entity 仍存在（观察 hook 不 despawn）。
        assert!(world.get_entity(entity).is_ok());
    }
}
