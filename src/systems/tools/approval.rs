//! 审批 System
//!
//! 处理父 Agent 审批请求和结果。

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::{
    app::{Clock, FrontendRegistry, HarnessSettings},
    domain::{
        Agent, ApprovalDecision, ApprovalRequestMessage, ApprovalResolvedHookPending,
        ApprovalResultMessage, BuiltinToolExecutors, ChatSession, EngineEvent, EventTarget,
        ExecutionError, ExperienceStore, GrantMode, PendingExperienceHooks,
        ProfileGenerationContext, SharedKnowledgeBase, ShortTermMemory, SkillUpdateContext, Task,
        TaskStatus, ToolCallingState, ToolContext, ToolError, ToolExecutionRequestMessage,
        ToolExecutionResultMessage, ToolReturnedHookPending, WaitingReason, WorkItem,
    },
    ecs::EntityIndex,
    infrastructure::skills::SkillLoader,
    systems::NativeProcessBackend,
};

use super::orchestrator::{
    clear_task_pending_confirmation_id, handle_tool_action, restore_task_after_tool,
    spawn_tool_error,
};

/// 审批分发 System
///
/// 为需要父 Agent 决策的请求创建审批任务。
///
/// TODO: 当前为 MVP 硬编码自动通过，需替换为真实父 Agent LLM 审查：
///       1. 给父 Agent 创建审批用 LLM 调用，传入 tool 信息和上下文
///       2. 解析 LLM 返回的决策（Approved/Rejected + reasoning）
///       3. 支持 GrantMode::Permanent 将权限写入 Agent
pub fn approval_dispatch_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    tasks: Query<&Task>,
    approval_requests: Query<(Entity, &ApprovalRequestMessage)>,
) {
    for (entity, request) in &approval_requests {
        debug!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            source_task_id = %request.source_task_id,
            parent_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            tool_input = ?request.tool_input,
            "approval request received - auto-approving in MVP"
        );

        info!(
            event = "ApprovalRequestReceived",
            request_id = %request.request_id,
            tool_name = %request.tool_name,
            parent_agent_id = %request.parent_agent_id,
            child_agent_id = %request.child_agent_id,
            "审批请求：Agent [{}] 请求调用 {}，等待 Agent [{}] 审批",
            request.child_agent_id,
            request.tool_name,
            request.parent_agent_id,
        );

        // 记录原 Task 状态
        // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
        if let Some(task) = index
            .get_task(&request.source_task_id)
            .and_then(|e| tasks.get(e).ok())
            && task.status == TaskStatus::Waiting(WaitingReason::Approval)
        {
            debug!(
                event = "SourceTaskWaiting",
                task_id = %task.id,
                "source task is waiting for approval"
            );
        }

        // 生成自动批准结果
        commands.spawn((
            ApprovalResultMessage {
                request_id: request.request_id,
                source_task_id: request.source_task_id,
                approval_task_id: request.approval_task_id,
                decision: ApprovalDecision::Approved,
                reasoning: "MVP auto-approve: parent agent approval".to_string(),
                grant_mode: GrantMode::Once,
            },
            ApprovalResolvedHookPending,
        ));

        commands.entity(entity).despawn();
    }
}

/// 审批结果处理 System
///
/// 处理父 Agent 审批结果，更新权限，恢复任务
#[allow(clippy::too_many_arguments)]
pub fn approval_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<(Entity, &mut Task)>,
    executors: Res<BuiltinToolExecutors>,
    knowledge: Res<SharedKnowledgeBase>,
    mut experience_store: ResMut<ExperienceStore>,
    mut pending_experience_hooks: ResMut<PendingExperienceHooks>,
    mut short_term_memories: Query<&mut ShortTermMemory>,
    chat_sessions: Query<&ChatSession>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    calling_states: Query<(Entity, &ToolCallingState)>,
    // 合并 ProfileGenerationContext 与 SkillUpdateContext 查询为单个 SystemParam，
    // 规避 Bevy 单 system 16 参数上限；两者都是与 WorkItem 同 entity 的 Component，
    // 通过 Option<&...> 区分（任一 WorkItem entity 至多只有其中之一）。
    context_queries: Query<(
        Entity,
        Option<&ProfileGenerationContext>,
        Option<&SkillUpdateContext>,
        &WorkItem,
    )>,
    settings: Res<HarnessSettings>,
    backend: Res<NativeProcessBackend>,
    // 合并 index / clock / skill_loader / frontend_registry 为单 SystemParam，规避 Bevy 单 system 16 参数上限；
    // index 用于 O(1) UUID 解析；clock/skill_loader 转发给 handle_tool_action；
    // frontend_registry 用于在 Approved 路径推送 ToolCallStarted 事件。
    index_clock_loader_frontends: (
        Res<EntityIndex>,
        Res<Clock>,
        Res<SkillLoader>,
        Res<FrontendRegistry>,
    ),
) {
    let index = &index_clock_loader_frontends.0;
    let frontend_registry = &index_clock_loader_frontends.3;
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

        match result.decision {
            ApprovalDecision::Rejected => {
                debug!(
                    event = "ToolApprovalRejected",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    reasoning = %result.reasoning,
                    "tool execution rejected by parent agent"
                );

                let execution_result = crate::domain::AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(format!(
                        "parent agent rejected: {}",
                        result.reasoning
                    ))),
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
                        tool_output: Err(ToolError::PermissionDenied(format!(
                            "parent agent rejected: {}",
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
                    "tool execution approved by parent agent"
                );

                // Permanent 模式：更新 Agent 权限
                if result.grant_mode == GrantMode::Permanent
                    && let Some(mut agent) = index
                        .get_agent(&tool_request.request.agent_id)
                        .and_then(|e| agents.get_mut(e).ok())
                {
                    agent.grant_permission(tool_request.tool_name.clone());
                    debug!(
                        event = "AgentPermissionUpdated",
                        agent_id = %agent.id,
                        tool_name = %tool_request.tool_name,
                        "agent permission updated to Allow permanently"
                    );
                }

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
                    .and_then(|(_, t)| t.routing_policy.output_channel.clone())
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
                    knowledge: &knowledge,
                    experience_store: &experience_store,
                    default_wait_tasks_timeout_secs: settings.0.default_wait_tasks_timeout_secs,
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
                        &mut tasks,
                        &agents,
                        &chat_sessions,
                        &mut short_term_memories,
                        &*backend,
                        &mut experience_store,
                        &mut pending_experience_hooks,
                        None,
                        &index_clock_loader_frontends.1,
                        &context_queries,
                        &index_clock_loader_frontends.2,
                        &calling_states,
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
    use crate::app::{Clock, HarnessConfig, HarnessSettings};
    use crate::domain::{
        AgentExecutionRequest, AgentRequestKind, ApprovalDecision, ApprovalResultMessage,
        ChannelId, FrontendKind, GrantMode, Task, TaskStatus, ToolExecutionRequestMessage,
        WaitingReason,
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
        world.insert_resource(crate::ecs::EntityIndex::default());
        world.insert_resource(crate::infrastructure::skills::SkillLoader::new(
            std::path::PathBuf::from("/nonexistent_skills_root"),
        ));
        world.insert_resource(crate::app::FrontendRegistry { frontends: vec![] });
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
                model_override: None,
            },
            tool_name: "shell_exec".to_string(),
            tool_input: serde_json::json!({"cmd": "echo ok"}),
            pending_confirmation_id: Some(request_id),
            tool_call_id: None,
            pending_confirmation_options: None,
            work_item_entity: None,
            confirmed_once: false,
        }
    }

    #[test]
    fn approval_rejected_clears_task_pending_id() {
        let mut world = test_world();
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
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
            approval_task_id: Uuid::new_v4(),
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
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
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
            approval_task_id: Uuid::new_v4(),
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
