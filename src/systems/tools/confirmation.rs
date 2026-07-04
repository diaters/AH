//! 用户确认 System
//!
//! 处理用户对 Tool 执行的确认请求和响应。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, BuiltinToolExecutors, ChatSession, ConfirmationOption, ExecutionError,
        ExperienceStore, GrantMode, PendingExperienceHooks, SharedKnowledgeBase, ShortTermMemory,
        Task, ToolCallingState, ToolConfirmationRequestMessage, ToolConfirmationResponseMessage,
        ToolContext, ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage,
        ToolPermission, ToolReturnedHookPending,
    },
    systems::NativeProcessBackend,
};

use super::orchestrator::{
    clear_task_pending_confirmation_id, handle_tool_action, restore_task_after_tool,
    spawn_tool_error,
};

/// Tool 确认请求输出 System
///
/// 将确认请求通过 frontend_output_system 推送给前端
pub fn tool_confirmation_request_system(
    _agents: Query<&Agent>,
    _requests: Query<(Entity, &ToolConfirmationRequestMessage)>,
) {
    // frontend_output_system 负责监听 Added<ToolConfirmationRequestMessage> 并推送给前端，
    // 此 system 保留为占位，后续可在此添加额外逻辑（如日志增强）
}

/// Tool 确认响应处理 System
///
/// 处理用户的确认响应
#[allow(clippy::too_many_arguments)]
pub fn tool_confirmation_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<(Entity, &mut Task)>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SharedKnowledgeBase>,
    mut experience_store: ResMut<ExperienceStore>,
    mut pending_experience_hooks: ResMut<PendingExperienceHooks>,
    mut short_term_memories: Query<&mut ShortTermMemory>,
    chat_sessions: Query<&ChatSession>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
    calling_states: Query<&ToolCallingState>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
    clock: Res<Clock>,
) {
    for (entity, response) in &responses {
        // 查找对应的 Tool 执行请求（通过 pending_confirmation_id 关联）
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(response.request_id))
        else {
            // 经验治理与孵化审批不属于 ToolExecutionRequestMessage，留给专用 system 处理。
            // 检查是否有对应的经验候选审批绑定，有则跳过（不报 NoMatch）。
            let is_experience_approval = experience_store
                .candidate_id_for_request(response.request_id)
                .is_some();
            if !is_experience_approval {
                warn!(
                    event = "ToolConfirmationNoMatch",
                    request_id = %response.request_id,
                    "no matching tool request found"
                );
            }
            commands.entity(entity).despawn();
            continue;
        };

        // experience_governance 特判：销毁执行占位实体，不执行工具，不销毁响应
        if tool_request.tool_name == "experience_governance" {
            debug!(
                event = "ExperienceGovernanceConfirmationSkipped",
                request_id = %response.request_id,
                "experience_governance confirmation handled by dedicated system"
            );
            commands.entity(request_entity).despawn();
            // 不 despawn response entity，留给 experience_approval_result_system
            continue;
        }

        // 统计仍排队的 sibling 请求数（不含当前这条）
        let pending_sibling_count = tool_requests
            .iter()
            .filter(|(e, r)| {
                *e != request_entity && r.request.task_id == tool_request.request.task_id
            })
            .count();

        // 从 ToolExecutionRequestMessage 保存的选项中查找
        let options = tool_request
            .pending_confirmation_options
            .clone()
            .unwrap_or_else(ConfirmationOption::default_options);
        let selected_option = options
            .iter()
            .find(|opt| opt.id == response.selected_option);

        match selected_option {
            Some(option) if option.is_deny() => {
                // 用户拒绝
                warn!(
                    event = "ToolConfirmationDenied",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    pending_sibling_count = pending_sibling_count,
                    "tool execution denied by user"
                );

                // 生成错误结果
                let execution_result = crate::domain::AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(
                        "user denied tool execution".to_string(),
                    )),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                };

                commands.spawn((
                    ToolExecutionResultMessage {
                        result: execution_result,
                        tool_name: tool_request.tool_name.clone(),
                        tool_output: Err(ToolError::PermissionDenied("user denied".to_string())),
                        tool_call_id: tool_request.tool_call_id.clone(),
                        processed: false,
                        original_tool_output: None,
                    },
                    ToolReturnedHookPending,
                ));

                restore_task_after_tool(&mut tasks, &calling_states, tool_request.request.task_id);
                clear_task_pending_confirmation_id(&mut tasks, tool_request.request.task_id);
                commands.entity(request_entity).despawn();
            }
            Some(option) => {
                // 用户确认
                debug!(
                    event = "ToolConfirmationApproved",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    mode = ?option.mode,
                    pending_sibling_count = pending_sibling_count,
                    "tool execution confirmed by user"
                );

                // Permanent 模式：更新 Agent 权限
                if option.mode == GrantMode::Permanent
                    && let Some(mut agent) = agents
                        .iter_mut()
                        .find(|a| a.id == tool_request.request.agent_id)
                {
                    agent
                        .tool_permissions
                        .overrides
                        .insert(tool_request.tool_name.clone(), ToolPermission::Allow);
                    debug!(
                        event = "AgentPermissionUpdated",
                        agent_id = %agent.id,
                        tool_name = %tool_request.tool_name,
                        new_permission = ?ToolPermission::Allow,
                        "agent permission updated to Allow permanently"
                    );
                }

                // 执行 Tool
                let Some(executor) = executors.get(&tool_request.tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_request.tool_name,
                        "no executor registered for tool after confirmation"
                    );
                    spawn_tool_error(
                        &mut commands,
                        request_entity,
                        tool_request,
                        ToolError::NotFound(format!("executor for {}", tool_request.tool_name)),
                    );
                    clear_task_pending_confirmation_id(&mut tasks, tool_request.request.task_id);
                    restore_task_after_tool(
                        &mut tasks,
                        &calling_states,
                        tool_request.request.task_id,
                    );
                    commands.entity(entity).despawn();
                    continue;
                };

                let ctx = ToolContext {
                    knowledge: &knowledge,
                    experience_store: &experience_store,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                    shell_default_tail_lines: settings.0.shell_default_tail_lines,
                    shell_max_tail_lines: settings.0.shell_max_tail_lines,
                    shell_default_exec_timeout_secs: settings.0.shell_default_exec_timeout_secs,
                    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
                    current_task_id: tool_request.request.task_id,
                    current_agent_id: tool_request.request.agent_id,
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);

                // Find the task entity
                if let Some((task_entity, _)) = tasks
                    .iter_mut()
                    .find(|(_, t)| t.id == tool_request.request.task_id)
                {
                    handle_tool_action(
                        &mut commands,
                        request_entity,
                        task_entity,
                        tool_request,
                        action,
                        &mut tasks,
                        &agents,
                        &chat_sessions,
                        &mut short_term_memories,
                        &*backend,
                        &mut experience_store,
                        &mut pending_experience_hooks,
                        None,
                        &clock,
                    );
                }

                clear_task_pending_confirmation_id(&mut tasks, tool_request.request.task_id);
                restore_task_after_tool(&mut tasks, &calling_states, tool_request.request.task_id);
            }
            None => {
                warn!(
                    event = "ToolConfirmationUnknownOption",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    "unknown option selected"
                );
                clear_task_pending_confirmation_id(&mut tasks, tool_request.request.task_id);
                // 清理残留的请求 entity，避免永久泄漏
                commands.entity(request_entity).despawn();
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Clock, HarnessConfig, HarnessSettings};
    use crate::domain::{
        AgentExecutionRequest, AgentRequestKind, ChannelId, ConfirmationOption, FrontendKind, Task,
        TaskStatus, ToolConfirmationResponseMessage, ToolExecutionRequestMessage, WaitingReason,
    };
    use crate::systems::NativeProcessBackend;
    use bevy_ecs::system::RunSystemOnce;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_world() -> World {
        let mut world = World::new();
        world.insert_resource(HarnessSettings(HarnessConfig::default()));
        world.insert_resource(Clock(Utc::now()));
        world.insert_resource(NativeProcessBackend::default());
        world.insert_resource(crate::domain::BuiltinToolExecutors::default());
        world.insert_resource(crate::domain::SharedKnowledgeBase::default());
        world.insert_resource(crate::domain::ExperienceStore::default());
        world.insert_resource(crate::domain::PendingExperienceHooks::default());
        world
    }

    fn dummy_task(task_id: Uuid) -> Task {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        Task {
            id: task_id,
            content: "test".to_string(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: channel,
            last_evaluated_turn: None,
        }
    }

    fn dummy_request(
        task_id: Uuid,
        agent_id: Uuid,
        request_id: Uuid,
    ) -> ToolExecutionRequestMessage {
        ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id,
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: "shell_exec".to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
            },
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"cmd": "echo ok"}),
            pending_confirmation_id: Some(request_id),
            tool_call_id: None,
            pending_confirmation_options: Some(ConfirmationOption::default_options()),
        }
    }

    #[test]
    fn confirmation_denied_clears_task_pending_id() {
        let mut world = test_world();
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();

        let mut task = dummy_task(task_id);
        task.pending_confirmation_id = Some(request_id);
        let task_entity = world.spawn(task).id();

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "deny".to_string(),
        });

        world
            .run_system_once(tool_confirmation_result_system)
            .unwrap();

        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert!(
            task.pending_confirmation_id.is_none(),
            "pending_confirmation_id should be cleared after denial"
        );
    }

    #[test]
    fn confirmation_approved_clears_task_pending_id() {
        let mut world = test_world();
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();

        let mut task = dummy_task(task_id);
        task.pending_confirmation_id = Some(request_id);
        let task_entity = world.spawn(task).id();

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "allow_once".to_string(),
        });

        // No executor registered, so the system will emit an error result and
        // restore the task. The pending id must still be cleared.
        world
            .run_system_once(tool_confirmation_result_system)
            .unwrap();

        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert!(
            task.pending_confirmation_id.is_none(),
            "pending_confirmation_id should be cleared after approval"
        );
    }
}
