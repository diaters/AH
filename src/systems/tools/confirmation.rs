//! 用户确认 System
//!
//! 处理用户对 Tool 执行的确认请求和响应。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::{Clock, FrontendRegistry, HarnessSettings},
    domain::{
        Agent, BuiltinToolExecutors, ChatSession, ConfirmationOption, EngineEvent, EventTarget,
        ExecutionError, ExperienceStore, GrantMode, PendingExperienceHooks, PermissionAction,
        PermissionAuditContext, PermissionSource, ProfileGenerationContext, SharedKnowledgeBase,
        ShortTermMemory, SkillUpdateContext, Task, TaskStatus, ToolActionKind, ToolCallingState,
        ToolConfirmationResponseMessage, ToolContext, ToolError, ToolExecutionRequestMessage,
        ToolExecutionResultMessage, ToolPermission, ToolReturnedHookPending, WaitingReason,
        WorkItem,
    },
    ecs::EntityIndex,
    infrastructure::skills::SkillLoader,
    systems::NativeProcessBackend,
};

use super::dispatch::emit_permission_audit;

use super::orchestrator::{
    clear_task_pending_confirmation_id, handle_tool_action, restore_task_after_tool,
    spawn_tool_error,
};

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
    // frontend_registry 用于在用户确认路径推送 ToolCallStarted 事件。
    mut index_clock_loader_frontends: (
        ResMut<EntityIndex>,
        Res<Clock>,
        Res<SkillLoader>,
        Res<FrontendRegistry>,
    ),
) {
    let frontend_registry = &index_clock_loader_frontends.3;
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

        // experience_governance / profile_generation 特判：
        // 销毁执行占位实体，不执行工具，不销毁响应。
        // 这两种工具的审批由 experience_approval_result_system 统一处理
        // （通过 store.bind_approval_request 绑定 candidate_id）。
        if tool_request.tool_name == "experience_governance"
            || tool_request.tool_name == "profile_generation"
        {
            debug!(
                event = "ConfirmationHandledByDedicatedSystem",
                request_id = %response.request_id,
                tool_name = %tool_request.tool_name,
                "{} confirmation handled by dedicated system",
                tool_request.tool_name,
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

                restore_task_after_tool(
                    &mut tasks,
                    &calling_states,
                    &index_clock_loader_frontends.0,
                    tool_request.request.task_id,
                );
                clear_task_pending_confirmation_id(
                    &mut tasks,
                    &index_clock_loader_frontends.0,
                    tool_request.request.task_id,
                );
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

                // 权限审计：Permanent grant（用户确认路径写入永久权限）。
                // 仅在 Permanent 模式下发出；Once 模式未改 overrides，不发 Grant 审计。
                if option.mode == GrantMode::Permanent {
                    let agent_name = index_clock_loader_frontends
                        .0
                        .get_agent(&tool_request.request.agent_id)
                        .and_then(|e| agents.get(e).ok())
                        .map(|a| a.profile.name.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    let output_channel = index_clock_loader_frontends
                        .0
                        .get_task(&tool_request.request.task_id)
                        .and_then(|e| tasks.get(e).ok())
                        .and_then(|(_, t)| t.routing_policy.output_channel.clone());
                    emit_permission_audit(
                        frontend_registry,
                        output_channel.as_ref(),
                        tool_request.request.agent_id,
                        &agent_name,
                        &tool_request.tool_name,
                        PermissionAction::Grant,
                        PermissionSource::AgentOverride,
                        PermissionAuditContext::UserConfirmation,
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
                    clear_task_pending_confirmation_id(
                        &mut tasks,
                        &index_clock_loader_frontends.0,
                        tool_request.request.task_id,
                    );
                    restore_task_after_tool(
                        &mut tasks,
                        &calling_states,
                        &index_clock_loader_frontends.0,
                        tool_request.request.task_id,
                    );
                    commands.entity(entity).despawn();
                    continue;
                };

                // Async 工具：清除 pending_confirmation_id 后交给 async_tool_dispatch_system
                // 下一帧认领（execute() 对 async 工具返回 InternalState 错误，不能走 sync 路径）。
                // 任务保持 Waiting(ToolExecution)——async dispatch 会原地改造请求实体并 spawn
                // worker，worker 完成后 ingest 落地结果并 restore 任务。
                //
                // **状态恢复**：`tool_dispatch_system` 在 `ToolRequiresUserConfirmation` 时
                // 把 task.status 设为 `Waiting(User)`（dispatch.rs:317）。Async 工具确认后
                // 必须在此处恢复为 `Waiting(ToolExecution)`——否则下一帧 Transform 集中的
                // `tool_calling_turn_reset_system` 会看到 `Waiting(User) &&
                // pending_confirmation_id.is_none()`，错误 despawn `ToolCallingState`，
                // 导致 worker 完成后 LLM 调用循环无法续跑（竞态 bug，日志已证实）。
                //
                // **allow_once 路径**：设置 `confirmed_once = true` 让 async_tool_dispatch_system
                // 跳过权限检查直接认领——否则 Confirm 权限的 Async 工具会陷入
                // 「确认 → 清除 pending_id → sync 路径再派发审批」的循环。
                // `allow_always` 路径已通过 `overrides.insert(Allow)` 更新永久权限，
                // async_tool_dispatch_system 会直接认领，无需 `confirmed_once`。
                if executor.kind() == ToolActionKind::Async {
                    debug!(
                        event = "ToolConfirmationApprovedAsync",
                        tool_name = %tool_request.tool_name,
                        task_id = %tool_request.request.task_id,
                        mode = ?option.mode,
                        "async tool confirmed; clearing pending_confirmation_id for async dispatch"
                    );
                    let mut updated_request = tool_request.clone();
                    updated_request.pending_confirmation_id = None;
                    if option.mode == crate::domain::GrantMode::Once {
                        updated_request.confirmed_once = true;
                    }
                    commands.entity(request_entity).insert(updated_request);
                    clear_task_pending_confirmation_id(
                        &mut tasks,
                        &index_clock_loader_frontends.0,
                        tool_request.request.task_id,
                    );
                    // 恢复 task.status 为 Waiting(ToolExecution)，语义正确 + 防 reset 竞态
                    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                    if let Some((_, mut task)) = index_clock_loader_frontends
                        .0
                        .get_task(&tool_request.request.task_id)
                        .and_then(|e| tasks.get_mut(e).ok())
                        && task.status == TaskStatus::Waiting(WaitingReason::User)
                    {
                        task.status = TaskStatus::Waiting(WaitingReason::ToolExecution);
                    }
                    commands.entity(entity).despawn();
                    continue;
                }

                // 推送 ToolCallStarted 事件到所有前端（仅当 task 有 output_channel 时；
                // 无 output_channel 时不推送，避免向无关 IM 通道广播）。
                // **仅覆盖 sync 工具**：async 工具的推送由 `async_tool_dispatch_system`
                // 在认领请求时统一处理，避免在此处推送后下一帧重复推送。
                let tool_input_summary = crate::domain::summarize_tool_input(
                    &tool_request.tool_name,
                    &tool_request.tool_input,
                );
                let agent_name = index_clock_loader_frontends
                    .0
                    .get_agent(&tool_request.request.agent_id)
                    .and_then(|e| agents.get(e).ok())
                    .map(|a| a.profile.name.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(target) = index_clock_loader_frontends
                    .0
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
                    current_origin_channel: index_clock_loader_frontends
                        .0
                        .get_task(&tool_request.request.task_id)
                        .and_then(|e| tasks.get(e).ok())
                        .map(|(_, t)| t.origin_channel.clone())
                        .unwrap_or(None),
                    current_skill_dir: None,
                };
                let action = executor.execute(&tool_request.tool_input, &ctx);

                // 预缓存 task_entity，避免 &mut index 与 &index 借用冲突
                let task_entity_opt = index_clock_loader_frontends
                    .0
                    .get_task(&tool_request.request.task_id)
                    .and_then(|e| tasks.get_mut(e).ok())
                    .map(|(e, _)| e);
                if let Some(task_entity) = task_entity_opt {
                    handle_tool_action(
                        &mut commands,
                        &mut index_clock_loader_frontends.0,
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
                        frontend_registry,
                    );
                }

                clear_task_pending_confirmation_id(
                    &mut tasks,
                    &index_clock_loader_frontends.0,
                    tool_request.request.task_id,
                );
                restore_task_after_tool(
                    &mut tasks,
                    &calling_states,
                    &index_clock_loader_frontends.0,
                    tool_request.request.task_id,
                );
            }
            None => {
                warn!(
                    event = "ToolConfirmationUnknownOption",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    pending_sibling_count = pending_sibling_count,
                    "unknown option selected"
                );

                // 将未知选项视为拒绝，避免任务卡住
                let execution_result = crate::domain::AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(
                        "unknown confirmation option".to_string(),
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
                        tool_output: Err(ToolError::PermissionDenied(
                            "unknown confirmation option".to_string(),
                        )),
                        tool_call_id: tool_request.tool_call_id.clone(),
                        processed: false,
                        original_tool_output: None,
                    },
                    ToolReturnedHookPending,
                ));

                clear_task_pending_confirmation_id(
                    &mut tasks,
                    &index_clock_loader_frontends.0,
                    tool_request.request.task_id,
                );
                restore_task_after_tool(
                    &mut tasks,
                    &calling_states,
                    &index_clock_loader_frontends.0,
                    tool_request.request.task_id,
                );
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
            pending_confirmation_options: Some(ConfirmationOption::default_options()),
            work_item_entity: None,
            confirmed_once: false,
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
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "deny".to_string(),
            feedback: None,
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
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "allow_once".to_string(),
            feedback: None,
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

    /// 用于 async 路径测试的 dummy executor：`kind() == Async`，`execute` 返回
    /// `InternalState` 防御错误（与已上桥工具模式一致），`run_async` 返回固定值。
    struct AsyncDummyTool;

    impl crate::domain::BuiltinTool for AsyncDummyTool {
        fn name(&self) -> &str {
            "shell_exec"
        }
        fn kind(&self) -> crate::domain::ToolActionKind {
            crate::domain::ToolActionKind::Async
        }
        fn execute(
            &self,
            _input: &serde_json::Value,
            _ctx: &crate::domain::ToolContext,
        ) -> Result<crate::domain::ToolAction, crate::domain::ToolError> {
            Err(crate::domain::ToolError::InternalState(
                "async-only tool".to_string(),
            ))
        }
    }

    /// 复现并守护 async 工具确认后 task.status 恢复为 `Waiting(ToolExecution)` 的修复。
    ///
    /// `tool_dispatch_system` 在 `ToolRequiresUserConfirmation` 时把 task.status 设为
    /// `Waiting(User)`（dispatch.rs:317）。Async 工具确认后必须在 confirmation 路径
    /// 恢复为 `Waiting(ToolExecution)`——否则下一帧 `tool_calling_turn_reset_system`
    /// 会看到 `Waiting(User) && pending_confirmation_id.is_none()`，错误 despawn
    /// `ToolCallingState`，导致 worker 完成后 LLM 调用循环无法续跑（竞态 bug）。
    #[test]
    fn async_confirmation_restores_task_to_waiting_tool_execution() {
        let mut world = test_world();
        let task_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();

        // 注册 async dummy executor
        world
            .resource_mut::<crate::domain::BuiltinToolExecutors>()
            .register(Box::new(AsyncDummyTool));

        // task 模拟 dispatch.rs:317 后的状态：Waiting(User) + pending_confirmation_id = Some
        let mut task = dummy_task(task_id);
        task.status = TaskStatus::Waiting(WaitingReason::User);
        task.pending_confirmation_id = Some(request_id);
        let task_entity = world.spawn(task).id();
        world
            .resource_mut::<crate::ecs::EntityIndex>()
            .tasks
            .insert(task_id, task_entity);

        world.spawn(dummy_request(task_id, agent_id, request_id));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "allow_always".to_string(),
            feedback: None,
        });

        world
            .run_system_once(tool_confirmation_result_system)
            .unwrap();

        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Waiting(WaitingReason::ToolExecution),
            "async tool confirmation must restore task to Waiting(ToolExecution) \
             to prevent tool_calling_turn_reset_system from despawning ToolCallingState"
        );
        assert!(
            task.pending_confirmation_id.is_none(),
            "pending_confirmation_id should be cleared after async approval"
        );
    }
}
