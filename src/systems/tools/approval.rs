//! 审批 System
//!
//! 处理父 Agent 审批请求和结果。

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::{
    contracts::{Clock, FrontendRegistry},
    domain::{
        Agent, ApprovalDecision, ApprovalRequestMessage, ApprovalResultMessage,
        ApprovalReviewInput, BuiltinToolExecutors, ChatSession, DispatchHint, DispatchKind,
        EngineEvent, EventTarget, ExecutionError, PendingDispatch, ShortTermMemory, SubTaskConfig,
        SupervisorApprovalOrigin, Task, TaskId, TaskStatus, ToolActionKind, ToolCallingState,
        ToolContext, ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage,
        ToolReturnedHookPending, WaitingReason, WorkItem, WorkItemType, build_approval_prompt,
        truncate_chars,
    },
    ecs::EntityIndex,
    systems::HarnessSettings,
    systems::NativeProcessBackend,
};

use super::orchestrator::{
    ToolResources, ToolWorldQueries, clear_task_pending_confirmation_id, handle_tool_action,
    restore_task_after_tool, spawn_tool_error,
};

/// 审批分发 System
///
/// 批次子任务的确认请求交给监督者（watchdog Agent）审批：按子任务目标、主 Agent 在
/// `create_tasks` 声明的授权范围与最近对话，组装一次无工具的判断请求。
/// 回复由 `watchdog_response_system` 落地为批准 / 拒绝 / 升级给用户确认。
pub fn approval_dispatch_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    tasks: Query<(&Task, Option<&SubTaskConfig>)>,
    calling: Query<&ToolCallingState>,
    approval_requests: Query<(Entity, &ApprovalRequestMessage)>,
) {
    for (entity, request) in &approval_requests {
        commands.entity(entity).despawn();
        let source = index
            .get_task(&request.source_task_id)
            .and_then(|e| tasks.get(e).ok());
        let Some((task, config)) = source else {
            warn!(
                event = "ApprovalSourceTaskMissing",
                request_id = %request.request_id,
                source_task_id = %request.source_task_id,
                "approval source task not found; request dropped"
            );
            continue;
        };
        let parent_goal = task
            .parent_task_id
            .and_then(|id| index.get_task(&id))
            .and_then(|e| tasks.get(e).ok())
            .map(|(parent, _)| truncate_chars(&parent.content, 400))
            .unwrap_or_default();
        let subject_label = if task.input_summary.is_empty() {
            format!("子任务 {}", task.id)
        } else {
            format!("子任务「{}」", task.input_summary)
        };
        let recent: Vec<String> = calling
            .iter()
            .find(|s| s.task_id == task.id && s.work_item_id.is_none())
            .map(|state| {
                let lines: Vec<String> = state
                    .conversation
                    .iter()
                    .filter_map(crate::systems::watchdog::render_message)
                    .collect();
                let skip = lines.len().saturating_sub(6);
                lines[skip..].to_vec()
            })
            .unwrap_or_default();
        let declared: Vec<String> = config.map(|c| c.allowed_tools.clone()).unwrap_or_default();
        let tool_input = truncate_chars(&request.tool_input.to_string(), 1500);
        let subtask_goal = truncate_chars(&task.content, 600);
        let prompt = build_approval_prompt(&ApprovalReviewInput {
            subject_label: &subject_label,
            parent_goal: &parent_goal,
            subtask_goal: &subtask_goal,
            declared_tools: &declared,
            tool_name: &request.tool_name,
            tool_input: &tool_input,
            recent: &recent,
        });

        info!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            source_task_id = %request.source_task_id,
            supervisor_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            "审批请求：{} 请求调用 {}，交监督者审批",
            subject_label,
            request.tool_name,
        );

        commands.spawn((
            WorkItem::watchdog_review(TaskId::new(), prompt),
            SupervisorApprovalOrigin {
                request_id: request.request_id,
                source_task_id: request.source_task_id,
                approval_task_id: request.approval_task_id,
                owner_task_id: task.parent_task_id,
                tool_name: request.tool_name.clone(),
                subject_label,
            },
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::WatchdogReview),
                hint: DispatchHint::default(),
            },
        ));
    }
}

/// 审批结果处理 System
///
/// 处理父 Agent 审批结果，更新权限，恢复任务
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn approval_result_system(
    mut commands: Commands,
    agents: Query<&mut Agent>,
    mut tasks: Query<(Entity, &mut Task)>,
    executors: Res<BuiltinToolExecutors>,
    mut short_term_memories: Query<&mut ShortTermMemory>,
    chat_sessions: Query<&ChatSession>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
    // 合并 index / clock / frontend_registry 为单 SystemParam，规避 Bevy 单 system 16 参数上限；
    // index 用于 O(1) UUID 解析；clock 转发给 handle_tool_action；
    // frontend_registry 用于在 Approved 路径推送 ToolCallStarted 事件。
    mut index_clock_frontends: (ResMut<EntityIndex>, Res<Clock>, Res<FrontendRegistry>),
) {
    let index = &mut index_clock_frontends.0;
    let frontend_registry = &index_clock_frontends.2;
    for (entity, result) in &approval_results {
        // 查找对应的 Tool 执行请求
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(result.request_id))
        else {
            debug!(
                event = "ApprovalResultNoMatch",
                request_id = %result.request_id,
                "no matching tool request found, may have been processed"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 审批已落地：任务从 Waiting(Approval) 回到 Waiting(ToolExecution)，
        // 使工具结果（真实或拒绝）能被工具循环收集并触发 follow-up。
        if let Some((_, mut task)) = index
            .get_task(&tool_request.request.task_id)
            .and_then(|e| tasks.get_mut(e).ok())
            && task.status == TaskStatus::Waiting(WaitingReason::Approval)
        {
            task.mark_waiting(WaitingReason::ToolExecution, index_clock_frontends.1.0);
        }

        match result.decision {
            ApprovalDecision::Rejected => {
                debug!(
                    event = "ToolApprovalRejected",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    reasoning = %result.reasoning,
                    "tool execution rejected by supervisor"
                );

                let execution_result = crate::domain::AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(format!(
                        "监督者拒绝：{}",
                        result.reasoning
                    ))),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                    conversation: None,
                    turn_generation: None,
                };

                commands.spawn((
                    ToolExecutionResultMessage {
                        result: execution_result,
                        tool_name: tool_request.tool_name.clone(),
                        tool_output: Err(ToolError::PermissionDenied(format!(
                            "监督者拒绝：{}",
                            result.reasoning
                        ))),
                        tool_call_id: tool_request.tool_call_id.clone(),
                        processed: false,
                        original_tool_output: None,
                    },
                    ToolReturnedHookPending,
                ));

                restore_task_after_tool(&mut tasks, &calling_states, index, result.source_task_id);
                clear_task_pending_confirmation_id(&mut tasks, index, tool_request.request.task_id);
                commands.entity(request_entity).despawn();
            }
            ApprovalDecision::Approved => {
                debug!(
                    event = "ToolApprovalGranted",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    grant_mode = ?result.grant_mode,
                    "tool execution approved by supervisor"
                );

                // 监督审批不写入任何持续授权：运行期授权只来自用户在会话中的选择
                // （见 RuntimeGrants），Permanent 在此按仅本次处理。
                info!(
                    event = "ApprovalResolved",
                    request_id = %result.request_id,
                    tool_name = %tool_request.tool_name,
                    decision = ?result.decision,
                    grant_mode = ?result.grant_mode,
                    "审批结果：{} => {:?}，授权模式 {:?}",
                    tool_request.tool_name,
                    result.decision,
                    result.grant_mode,
                );

                // 执行 Tool
                let Some(executor) = executors.get(&tool_request.tool_name) else {
                    warn!(
                        event = "ToolExecutorNotFound",
                        tool_name = %tool_request.tool_name,
                        "no executor registered for tool after approval"
                    );
                    spawn_tool_error(
                        &mut commands,
                        request_entity,
                        tool_request,
                        ToolError::NotFound(format!("executor for {}", tool_request.tool_name)),
                    );
                    clear_task_pending_confirmation_id(
                        &mut tasks,
                        index,
                        tool_request.request.task_id,
                    );
                    restore_task_after_tool(
                        &mut tasks,
                        &calling_states,
                        index,
                        result.source_task_id,
                    );
                    commands.entity(entity).despawn();
                    continue;
                };

                // Async 工具：清除 pending_confirmation_id 并标记本次已获批，交给
                // async_tool_dispatch_system 下一帧认领（execute() 对 async 工具不可用）。
                if executor.kind() == ToolActionKind::Async {
                    let mut updated_request = tool_request.clone();
                    updated_request.pending_confirmation_id = None;
                    updated_request.confirmed_once = true;
                    commands.entity(request_entity).insert(updated_request);
                    clear_task_pending_confirmation_id(
                        &mut tasks,
                        index,
                        tool_request.request.task_id,
                    );
                    commands.entity(entity).despawn();
                    continue;
                }

                // 推送 ToolCallStarted 事件到所有前端（仅当 task 有 output_channel 时；
                // 无 output_channel 时不推送，避免向无关 IM 通道广播）
                let tool_input_summary = crate::domain::summarize_tool_input(
                    &tool_request.tool_name,
                    &tool_request.tool_input,
                );
                let agent_name = index
                    .get_agent(&tool_request.request.agent_id)
                    .and_then(|e| agents.get(e).ok())
                    .map(|a| a.profile.name.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(target) = index
                    .get_task(&tool_request.request.task_id)
                    .and_then(|e| tasks.get(e).ok())
                    .and_then(|(_, t)| t.routing_policy.output_channel().cloned())
                    .map(|channel| EventTarget::Directed(vec![channel]))
                {
                    let event = EngineEvent::ToolCallStarted {
                        target,
                        task_id: tool_request.request.task_id,
                        agent_name,
                        tool_name: tool_request.tool_name.clone(),
                        tool_input_summary,
                    };
                    for frontend in &frontend_registry.frontends {
                        frontend.push_event(event.clone());
                    }
                } else {
                    debug!(
                        event = "ToolCallStartedDroppedNoChannel",
                        task_id = %tool_request.request.task_id,
                        tool_name = %tool_request.tool_name,
                        "dropping ToolCallStarted because task has no output channel"
                    );
                }

                let ctx = ToolContext {
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
                    tool_background_after_secs: settings.0.tool_background_after_secs,
                    shell_default_tail_lines: settings.0.shell_default_tail_lines,
                    shell_max_tail_lines: settings.0.shell_max_tail_lines,
                    shell_default_exec_timeout_secs: settings.0.shell_default_exec_timeout_secs,
                    shell_default_stop_timeout_secs: settings.0.shell_default_stop_timeout_secs,
                    tool_inflight_timeout_secs: settings.0.tool_inflight_timeout_secs,
                    current_task_id: tool_request.request.task_id,
                    current_agent_id: tool_request.request.agent_id,
                    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                    current_origin_channel: index
                        .get_task(&tool_request.request.task_id)
                        .and_then(|e| tasks.get(e).ok())
                        .map(|(_, t)| t.origin_channel.clone())
                        .unwrap_or(None),
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);

                // Find the task entity
                // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                if let Some((task_entity, _)) = index
                    .get_task(&tool_request.request.task_id)
                    .and_then(|e| tasks.get_mut(e).ok())
                {
                    handle_tool_action(
                        &mut commands,
                        request_entity,
                        task_entity,
                        tool_request,
                        action,
                        &mut ToolWorldQueries {
                            tasks: &mut tasks,
                            agents: &agents,
                            chat_sessions: &chat_sessions,
                            short_term_memories: &mut short_term_memories,
                        },
                        &mut ToolResources {
                            backend: &*backend,
                            frontend_registry,
                            clock: &index_clock_frontends.1,
                        },
                        &mut *index,
                    );
                }

                clear_task_pending_confirmation_id(&mut tasks, index, tool_request.request.task_id);
                restore_task_after_tool(&mut tasks, &calling_states, index, result.source_task_id);
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Clock;
    use crate::domain::{
        AgentExecutionRequest, AgentRequestKind, ApprovalDecision, ApprovalResultMessage,
        ChannelId, FrontendKind, GrantMode, Task, TaskStatus, ToolExecutionRequestMessage,
        WaitingReason,
    };
    use crate::systems::NativeProcessBackend;
    use crate::systems::{HarnessConfig, HarnessSettings};
    use bevy_ecs::system::RunSystemOnce;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_world() -> World {
        let mut world = World::new();
        world.insert_resource(HarnessSettings(HarnessConfig::default()));
        world.insert_resource(Clock(Utc::now()));
        world.insert_resource(NativeProcessBackend::default());
        world.insert_resource(crate::domain::BuiltinToolExecutors::default());
        world.insert_resource(crate::ecs::EntityIndex::default());
        world.insert_resource(crate::contracts::FrontendRegistry { frontends: vec![] });
        world
    }

    fn dummy_task(task_id: crate::domain::TaskId) -> Task {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        Task {
            id: task_id,
            content: "test".to_string(),
            creator: crate::domain::AgentId::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::Approval),
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
            origin_channel: Some(channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        }
    }

    fn dummy_request(
        task_id: crate::domain::TaskId,
        agent_id: crate::domain::AgentId,
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
                model_override: None,
                turn_generation: None,
                vision: false,
                trusted_images: Vec::new(),
            },
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"cmd": "echo ok"}),
            pending_confirmation_id: Some(request_id),
            tool_call_id: None,
            pending_confirmation_options: None,
            confirmed_once: false,
        }
    }

    #[test]
    fn approval_rejected_clears_task_pending_id() {
        let mut world = test_world();
        let task_id = crate::domain::TaskId::new();
        let agent_id = crate::domain::AgentId::new();
        let request_id = Uuid::new_v4();

        let mut task = dummy_task(task_id);
        task.pending_confirmation_id = Some(request_id);
        let task_entity = world.spawn(task).id();
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ApprovalResultMessage {
            request_id,
            source_task_id: task_id,
            approval_task_id: crate::domain::TaskId::new(),
            decision: ApprovalDecision::Rejected,
            reasoning: "no".to_string(),
            grant_mode: GrantMode::Once,
        });

        world.run_system_once(approval_result_system).unwrap();

        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert!(
            task.pending_confirmation_id.is_none(),
            "pending_confirmation_id should be cleared after parent agent rejection"
        );
    }

    #[test]
    fn approval_approved_clears_task_pending_id() {
        let mut world = test_world();
        let task_id = crate::domain::TaskId::new();
        let agent_id = crate::domain::AgentId::new();
        let request_id = Uuid::new_v4();

        let mut task = dummy_task(task_id);
        task.pending_confirmation_id = Some(request_id);
        let task_entity = world.spawn(task).id();
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ApprovalResultMessage {
            request_id,
            source_task_id: task_id,
            approval_task_id: crate::domain::TaskId::new(),
            decision: ApprovalDecision::Approved,
            reasoning: "yes".to_string(),
            grant_mode: GrantMode::Once,
        });

        // No executor registered, so the system emits an error result and
        // restores the task. The pending id must still be cleared.
        world.run_system_once(approval_result_system).unwrap();

        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert!(
            task.pending_confirmation_id.is_none(),
            "pending_confirmation_id should be cleared after parent agent approval"
        );
    }
}
