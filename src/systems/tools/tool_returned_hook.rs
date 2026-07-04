//! Task 20 companion 系统：`on_tool_returned` 观察 hook 派发。
//!
//! 在 `tool_result_system` 之前运行。遍历带 `ToolReturnedHookPending` 标记的
//! `ToolExecutionResultMessage` entity，逐个对 `WorldSnapshot` 快照派发
//! `HookPoint::OnToolReturned`，flush 累积的 `WorldCommand`，然后：
//!
//! - 若插件调用 `tool_set_result` 导致 `outcome.replaced_result.is_some()`：将
//!   `tool_output` 替换为插件提供的 JSON 值（`Ok(replaced)`），原始输出保留在
//!   `original_tool_output` 审计字段中，供 `tool_result_system` 日志记录。
//! - 若插件调用 `tool_deny`（后 hook 上无语义）：仅记 `tracing::warn!` 审计，
//!   不阻止结果流转。
//! - 无论是否修改，始终移除 `ToolReturnedHookPending` 标记。
//!
//! 采用 companion-system 模式（而非内联 `dispatch_hook` 到 `tool_result_system`）
//! 是因为 `tool_result_system` 的 `Query<(Entity, &mut ToolExecutionResultMessage)>`
//! 签名与 `dispatch_hook` 要求的 `&mut World` 互斥。

use std::sync::{Arc, Mutex};

use crate::prelude::*;
use crossbeam_channel::unbounded;
use tracing::{debug, warn};

use crate::domain::{ToolExecutionResultMessage, ToolReturnedHookPending};
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

/// `on_tool_returned` 观察 hook companion 系统。
///
/// 在 `HarnessSet::Transform` 集合中运行，并通过 `.before(tool_result_system)`
/// 保证先于结果处理。无 `PluginRegistry` 时 noop 不 panic。
pub fn on_tool_returned_hook_system(world: &mut World) {
    // 若没有 plugin registry，说明插件层未启用，直接跳过。
    if !world.contains_resource::<PluginRegistry>() {
        return;
    }

    // 先采集所有带标记结果消息的 entity 及 clone，避免在派发 hook 期间借用 world。
    let targets: Vec<(Entity, ToolExecutionResultMessage)> = world
        .query_filtered::<(Entity, &ToolExecutionResultMessage), With<ToolReturnedHookPending>>()
        .iter(world)
        .map(|(e, r)| (e, r.clone()))
        .collect();

    if targets.is_empty() {
        return;
    }

    world.resource_scope(
        |world: &mut World, mut registry: bevy_ecs::change_detection::Mut<PluginRegistry>| {
            for (entity, result) in targets {
                let outcome = dispatch_on_tool_returned(world, &mut registry, &result);

                // 后 hook 中 deny 无语义，仅记审计警告。
                if outcome.deny_reason.is_some() {
                    warn!(
                        event = "PluginToolDenyOnPostHook",
                        tool_name = %result.tool_name,
                        tool_call_id = %result.tool_call_id.as_deref().unwrap_or(""),
                        "deny called on on_tool_returned post-hook; ignored"
                    );
                }

                // 若插件调用 tool_set_result 替换了输出。
                if let Some(replaced) = outcome.replaced_result
                    && let Ok(mut e) = world.get_entity_mut(entity)
                    && let Some(mut msg) = e.get_mut::<ToolExecutionResultMessage>()
                {
                    // 保留原始 tool_output 到审计字段（仅当结果为 Ok 时有意义）。
                    if let Ok(original) = &msg.tool_output {
                        msg.original_tool_output = Some(original.clone());
                    }
                    // 替换 tool_output 为插件提供的值。
                    msg.tool_output = Ok(replaced);

                    // 审计：插件以 tool_set_result 替换了工具结果。
                    warn!(
                        event = "PluginToolResultSetByHook",
                        tool_call_id = %result.tool_call_id.as_deref().unwrap_or(""),
                        tool_name = %result.tool_name,
                        "tool result replaced by on_tool_returned hook plugin"
                    );
                }

                // 始终移除标记，结果继续在 tool_result_system 中处理。
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    e.remove::<ToolReturnedHookPending>();
                }
            }
        },
    );
}

/// 对单个 `ToolExecutionResultMessage` 派发 `on_tool_returned` hook 并 flush WorldCommand。
///
/// 每个插件获取独立的 `SharedHookOutcome` / `WorldWriter`，不复用跨请求状态。
fn dispatch_on_tool_returned(
    world: &mut World,
    registry: &mut PluginRegistry,
    result: &ToolExecutionResultMessage,
) -> HookOutcome {
    let (writer_tx, writer_rx) = unbounded::<WorldCommand>();
    let (message_tx, _message_rx) = unbounded();
    let snap = WorldSnapshot::from_world(world);

    let input = HookDispatchInput {
        point: HookPoint::OnToolReturned,
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
        event = "OnToolReturnedHookDispatched",
        tool_name = %result.tool_name,
        tool_call_id = %result.tool_call_id.as_deref().unwrap_or(""),
        replaced = outcome.replaced_result.is_some(),
        "on_tool_returned hook dispatched for tool result"
    );

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentExecutionResult, AgentRequestKind, ChannelId, FrontendKind, Task};

    /// 构造一个占位 `ToolExecutionResultMessage` 用于测试派发路径。
    fn make_result(task_id: crate::domain::TaskId) -> ToolExecutionResultMessage {
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id: uuid::Uuid::nil(),
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: "knowledge_search".to_string(),
                },
                result: Ok(crate::domain::AgentExecutionOutput {
                    content: crate::domain::OutputContent::Text("result".to_string()),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
            },
            tool_name: "knowledge_search".to_string(),
            tool_output: Ok(serde_json::json!({"count": 1})),
            tool_call_id: Some("call-1".to_string()),
            processed: false,
            original_tool_output: None,
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
        let result = make_result(task_id);
        let entity = world.spawn((result, ToolReturnedHookPending)).id();

        on_tool_returned_hook_system(&mut world);

        // 标记仍在（companion 系统从未运行），结果 entity 仍在。
        assert!(
            world
                .query::<&ToolReturnedHookPending>()
                .get(&world, entity)
                .is_ok()
        );
    }

    /// 空插件 registry 时应正常派发并移除标记，不修改 tool_output。
    #[test]
    fn empty_registry_removes_marker_without_modification() {
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
        let result = make_result(task_id);
        let entity = world.spawn((result, ToolReturnedHookPending)).id();
        world.insert_resource(PluginRegistry::default());

        on_tool_returned_hook_system(&mut world);

        // 应移除标记。
        assert!(
            world
                .query::<&ToolReturnedHookPending>()
                .get(&world, entity)
                .is_err(),
            "应移除 ToolReturnedHookPending 标记"
        );

        // tool_output 不应被修改。
        let msg = world.get::<ToolExecutionResultMessage>(entity).unwrap();
        assert_eq!(msg.tool_output, Ok(serde_json::json!({"count": 1})));
        assert!(msg.original_tool_output.is_none());
    }
}
