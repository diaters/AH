//! Task 19 companion 系统：`on_tool_called` 前置 hook 派发。
//!
//! 在 `tool_dispatch_system` 之前运行。遍历带 `ToolCalledHookPending` 标记的
//! `ToolExecutionRequestMessage` entity，逐个对 `WorldSnapshot` 快照派发
//! `HookPoint::OnToolCalled`，flush 累积的 `WorldCommand`，然后：
//!
//! - 若插件调用 `tool_deny` 导致 `outcome.deny_reason.is_some()`：记 `tracing::warn!`
//!   审计，内联 `spawn_tool_error` 的等价逻辑，直接通过 `world.spawn` 产出一个
//!   `ToolExecutionResultMessage`（`ToolError::PermissionDenied("denied by plugin: ...")`），
//!   随后销毁原请求 entity。该请求不会流转到 `tool_dispatch_system`。
//! - 否则仅移除 `ToolCalledHookPending` 标记，请求继续在 `tool_dispatch_system`
//!   中按正常权限路径处理。
//!
//! 采用 companion-system 模式（而非内联 `dispatch_hook` 到 `tool_dispatch_system`）
//! 是因为 `tool_dispatch_system` 的 `Query<(Entity, &mut ToolExecutionRequestMessage)>`
//! 签名与 `dispatch_hook` 要求的 `&mut World` 互斥，且其 `match permission` 分支有
//! 直接执行 / 确认 / 拒绝 / 审批四条路径，内联后每条都需重复调用。

use std::sync::{Arc, Mutex};

use crate::prelude::*;
use crossbeam_channel::unbounded;
use tracing::{debug, warn};

use crate::domain::{
    AgentExecutionResult, ExecutionError, ToolCalledHookPending, ToolError,
    ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolReturnedHookPending,
};
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

/// `on_tool_called` 前置 hook companion 系统。
///
/// 在 `HarnessSet::Dispatch` 集合中运行，并通过 `.before(tool_dispatch_system)`
/// 保证先于分发处理。无 `PluginRegistry` 时 noop 不 panic。
pub fn on_tool_called_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记请求的 entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(Entity, ToolExecutionRequestMessage)> = world
        .query_filtered::<(Entity, &ToolExecutionRequestMessage), With<ToolCalledHookPending>>()
        .iter(world)
        .map(|(e, r)| (e, r.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, request) in targets {
                let outcome = dispatch_on_tool_called(world, &mut registry, &request);

                if let Some(reason) = outcome.deny_reason {
                    // 审计：插件以 `tool_deny` 拒绝了工具调用。
                    // 注：v1 host API 不在 HookOutcome 中追踪是哪个插件调用了
                    // `tool_deny`，因此本层审计不写 `plugin_id`（按计划要求），
                    // per-plugin attribution 推迟到后续 host API 升级。
                    warn!(
                        event = "PluginToolDeniedByHook",
                        tool_call_id = %request.tool_call_id.as_deref().unwrap_or(""),
                        reason = %reason,
                        tool_name = %request.tool_name,
                        "tool call denied by on_tool_called hook plugin"
                    );

                    // 内联 `spawn_tool_error` 逻辑（该 helper 签名要求 `Commands`，
                    // 此处使用 `&mut World`）。生成错误结果消息并销毁原请求。
                    let execution_result = AgentExecutionResult {
                        task_id: request.request.task_id,
                        agent_id: request.request.agent_id,
                        request_kind: request.request.request_kind.clone(),
                        result: Err(ExecutionError::Unknown(
                            ToolError::PermissionDenied(format!("denied by plugin: {}", reason))
                                .to_string(),
                        )),
                        prompt: String::new(),
                        system_prompt: None,
                        tools: vec![],
                        reasoning_content: None,
                        work_item_id: None,
                    };

                    world.spawn((
                        ToolExecutionResultMessage {
                            result: execution_result,
                            tool_name: request.tool_name.clone(),
                            tool_output: Err(ToolError::PermissionDenied(format!(
                                "denied by plugin: {}",
                                reason
                            ))),
                            tool_call_id: request.tool_call_id.clone(),
                            processed: false,
                            original_tool_output: None,
                        },
                        ToolReturnedHookPending,
                    ));

                    // 销毁原请求 entity，避免流转到 tool_dispatch_system 再次产出
                    // 错误结果或触发确认逻辑。
                    if let Ok(e) = world.get_entity_mut(entity) {
                        e.despawn();
                    }
                } else {
                    // 未拒绝：仅移除标记，请求继续在 tool_dispatch_system 中处理。
                    if let Ok(mut e) = world.get_entity_mut(entity) {
                        e.remove::<ToolCalledHookPending>();
                    }
                }
            }
        },
    );
}

/// 对单个 `ToolExecutionRequestMessage` 派发 `on_tool_called` hook 并 flush WorldCommand。
///
/// 每个插件获取独立的 `SharedHookOutcome` / `WorldWriter`，不复用跨请求状态。
fn dispatch_on_tool_called(
    world: &mut World,
    registry: &mut PluginRegistry,
    request: &ToolExecutionRequestMessage,
) -> HookOutcome {
    let (writer_tx, writer_rx) = unbounded::<WorldCommand>();
    let (message_tx, _message_rx) = unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point: HookPoint::OnToolCalled,
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

    let outcome = dispatch_hook(input);

    // flush 累积的 WorldCommand（host API 通过 writer 攒出的 create_task 等）。
    flush_world_commands(world, &writer_rx);

    debug!(
        event = "OnToolCalledHookDispatched",
        tool_name = %request.tool_name,
        tool_call_id = %request.tool_call_id.as_deref().unwrap_or(""),
        denied = outcome.deny_reason.is_some(),
        "on_tool_called hook dispatched for tool call"
    );

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentExecutionRequest, AgentRequestKind, ChannelId, FrontendKind, Task};

    /// 构造一个占位 `ToolExecutionRequestMessage` 用于测试派发路径。
    fn make_request(task_id: crate::domain::TaskId) -> ToolExecutionRequestMessage {
        ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id: uuid::Uuid::nil(),
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: "knowledge_search".to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
            },
            tool_name: "knowledge_search".to_string(),
            tool_input: serde_json::json!({}),
            pending_confirmation_id: None,
            tool_call_id: None,
            pending_confirmation_options: None,
        }
    }

    /// 无 PluginRegistry 时 companion 系统应 noop 不 panic。
    #[test]
    fn noop_without_registry() {
        let mut world = World::new();
        let task_id = {
            let channel = ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            };
            let task = Task::from_user_input("test", 0, channel);
            let id = task.id;
            world.spawn(task);
            id
        };
        let request = make_request(task_id);
        let entity = world.spawn((request, ToolCalledHookPending)).id();

        on_tool_called_hook_system(&mut world);

        // 标记仍在（companion 系统从未运行），请求 entity 仍在。
        assert!(
            world
                .query::<&ToolCalledHookPending>()
                .get(&world, entity)
                .is_ok()
        );
    }

    /// 空插件 registry 时应正常派发并移除标记。
    #[test]
    fn empty_registry_removes_marker() {
        let mut world = World::new();
        let task_id = {
            let channel = ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            };
            let task = Task::from_user_input("test", 0, channel);
            let id = task.id;
            world.spawn(task);
            id
        };
        let request = make_request(task_id);
        let entity = world.spawn((request, ToolCalledHookPending)).id();
        world.insert_resource(PluginRegistry::default());

        on_tool_called_hook_system(&mut world);

        // 应移除标记，请求 entity 仍在（因为未拒绝）。
        assert!(
            world
                .query::<&ToolCalledHookPending>()
                .get(&world, entity)
                .is_err(),
            "未拒绝时应移除 ToolCalledHookPending 标记"
        );
        assert!(world.get_entity(entity).is_ok());
    }
}
