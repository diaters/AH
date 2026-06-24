//! Task 24-25 companion 系统：Agent 生命周期观察 hook 派发。
//!
//! 包含两个 companion 系统：
//!
//! - `agent_started_hook_system`：基于 `Added<Agent>` 变更检测，在新 Agent entity
//!   首次出现时派发 `OnAgentStarted` hook。`Added<Agent>` 天然去重，每个 entity
//!   生命周期仅触发一次，无需额外标记组件。
//!
//! - `agent_stopped_hook_system`：基于 `AgentStoppingHookPending` 标记组件，
//!   在 Agent 即将被 despawn 前派发 `OnAgentStopped` hook，然后负责 despawn。
//!   `handle_termination` 不再直接 despawn，而是插入此标记，由本系统接管 despawn
//!   职责，确保 hook 在 entity 消失前完成派发。

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use tracing::debug;

use crate::domain::{Agent, AgentStoppingHookPending};
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

/// `on_agent_started` 观察 hook companion 系统。
///
/// 使用 `Added<Agent>` 变更检测，在新 Agent entity 首次出现时派发
/// `HookPoint::OnAgentStarted`。`Added<Agent>` 天然去重 —— 每个 entity
/// 生命周期仅触发一次，无需额外标记组件或去重逻辑。
///
/// 在 `HarnessSet::Maintenance` 集合中运行。无 `PluginRegistry` 时 noop 不 panic。
pub fn agent_started_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 采集所有新添加的 Agent entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(Entity, Agent)> = world
        .query_filtered::<(Entity, &Agent), Added<Agent>>()
        .iter(world)
        .map(|(e, a)| (e, a.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy::ecs::change_detection::Mut<PluginRegistry>| {
            for (_entity, agent) in targets {
                dispatch_agent_lifecycle_hook(
                    world,
                    &mut registry,
                    &agent,
                    HookPoint::OnAgentStarted,
                );
            }
        },
    );
}

/// `on_agent_stopped` 观察 hook companion 系统。
///
/// 查询所有带 `AgentStoppingHookPending` 标记的 Agent entity，逐个派发
/// `HookPoint::OnAgentStopped`，flush 累积的 `WorldCommand`，然后 despawn
/// 该 entity。本系统接管了 `handle_termination` 原有的 despawn 职责。
///
/// 在 `HarnessSet::Maintenance` 集合中运行，在 `agent_factory_system` 之后执行
/// （确保 `handle_termination` 已插入标记）。无 `PluginRegistry` 时 noop 不 panic。
pub fn agent_stopped_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 采集所有带标记的 Agent entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(Entity, Agent)> = world
        .query_filtered::<(Entity, &Agent), With<AgentStoppingHookPending>>()
        .iter(world)
        .map(|(e, a)| (e, a.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy::ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, agent) in targets {
                dispatch_agent_lifecycle_hook(
                    world,
                    &mut registry,
                    &agent,
                    HookPoint::OnAgentStopped,
                );

                // 派发完成后 despawn entity（接管 handle_termination 的 despawn 职责）。
                if let Ok(e) = world.get_entity_mut(entity) {
                    e.despawn();
                }
            }
        },
    );
}

/// 对单个 Agent 派发生命周期 hook 并 flush WorldCommand。
///
/// 每个插件获取独立的 `SharedHookOutcome` / `WorldWriter`，不复用跨请求状态。
fn dispatch_agent_lifecycle_hook(
    world: &mut World,
    registry: &mut PluginRegistry,
    agent: &Agent,
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
        ctx_builder: Box::new(|plugin: &LoadedPlugin, _| {
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
                    store: Arc::new(crate::domain::ExperienceStore::default()),
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
        event = "AgentLifecycleHookDispatched",
        agent_id = %agent.id,
        agent_name = %agent.profile.name,
        hook_point = ?point,
        "agent lifecycle hook dispatched"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions};

    /// 构造一个占位 Agent 用于测试派发路径。
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

    /// agent_started_hook_system：无 PluginRegistry 时 noop 不 panic。
    #[test]
    fn agent_started_noop_without_registry() {
        let mut world = World::new();
        let agent = make_agent();
        let entity = world.spawn(agent).id();

        // 推一帧使 Added<Agent> 过期（但本系统跳过，因为无 registry）。
        // 直接调用，Added<Agent> 在同帧仍有效。
        agent_started_hook_system(&mut world);

        // Agent entity 仍在（系统 noop，未操作 entity）。
        assert!(world.get_entity(entity).is_ok());
    }

    /// agent_started_hook_system：空 registry 时应正常派发。
    #[test]
    fn agent_started_empty_registry_dispatches() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let agent = make_agent();
        let entity = world.spawn(agent).id();

        // Added<Agent> 在同帧有效。
        agent_started_hook_system(&mut world);

        // Agent entity 仍在（on_agent_started 不 despawn）。
        assert!(world.get_entity(entity).is_ok());
    }

    /// agent_stopped_hook_system：无 PluginRegistry 时 noop 不 panic。
    #[test]
    fn agent_stopped_noop_without_registry() {
        let mut world = World::new();
        let agent = make_agent();
        let entity = world.spawn((agent, AgentStoppingHookPending)).id();

        agent_stopped_hook_system(&mut world);

        // 标记仍在（系统 noop），entity 仍在。
        assert!(
            world
                .query::<&AgentStoppingHookPending>()
                .get(&world, entity)
                .is_ok()
        );
        assert!(world.get_entity(entity).is_ok());
    }

    /// agent_stopped_hook_system：空 registry 时应正常派发并 despawn。
    #[test]
    fn agent_stopped_empty_registry_dispatches_and_despawns() {
        let mut world = World::new();
        world.insert_resource(PluginRegistry::default());
        let agent = make_agent();
        let entity = world.spawn((agent, AgentStoppingHookPending)).id();

        agent_stopped_hook_system(&mut world);

        // entity 应被 despawn。
        assert!(
            world.get_entity(entity).is_err(),
            "带 AgentStoppingHookPending 的 entity 应在 hook 派发后 despawn"
        );
    }
}
