//! Tool 执行协调器
//!
//! 处理 Tool 执行动作和消息生成。

use crate::prelude::*;
use serde::Serialize;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::app::{Clock, FrontendRegistry};
use crate::contracts::SessionBackend;
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, AgentId, AgentKind, AskUserPending,
    BatchTaskState, ChannelId, ChatRoundStartedMessage, ChatSession, DispatchHint, DispatchKind,
    DispatchStrategy, EngineEvent, EntryRole, EventTarget, ExperienceCandidate,
    ExperienceCandidatePayload, ExperienceCandidateSubmission, ExperienceKindHint, ExperienceStore,
    FrontendKind, MessageRole, NewlyCreatedTask, OutputContent, PendingDispatch,
    PendingExperienceHooks, ProfileGenerationContext, SessionSummary, ShellSessionResult,
    ShortTermMemory, SkillUpdateContext, SubTaskBatchCreatedMessage, SubTaskBatchState,
    SubTaskConfig, SubTaskDefinition, Task, TaskId, TaskStatus, ToolAction, ToolCallingState,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolReturnedHookPending,
    WaitingForTasksInfo, WaitingReason, WorkItem,
};
use crate::ecs::EntityIndex;
use crate::infrastructure::skills::{SkillLoader, apply_skill_operations};

/// 清除任务上正在等待的工具确认 ID。
pub fn clear_task_pending_confirmation_id(
    tasks: &mut Query<(Entity, &mut Task)>,
    index: &EntityIndex,
    task_id: TaskId,
) {
    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
    if let Some((_, mut task)) = index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok()) {
        task.pending_confirmation_id = None;
    }
}

/// 等待任务结果
#[derive(Debug, Clone, Serialize)]
pub struct TaskWaitResult {
    pub task_id: String,
    pub status: TaskStatus,
    pub result: Option<String>,
    pub error: Option<String>,
}

/// 为 create_tasks 生成子 Task 实体、SubTaskBatchState 和消息
#[allow(clippy::too_many_arguments)]
pub fn spawn_create_tasks_messages(
    commands: &mut Commands,
    index: &mut EntityIndex,
    request_entity: Entity,
    agent_id: AgentId,
    task_id: TaskId,
    request_kind: crate::domain::AgentRequestKind,
    definitions: Vec<SubTaskDefinition>,
    tool_call_id: Option<String>,
    parent_origin_channel: Option<ChannelId>,
) {
    let batch_id = Uuid::new_v4();
    let total_count = definitions.len();

    // 计算反向依赖：对每个任务，找出哪些任务依赖它
    let mut depended_by_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for def in &definitions {
        for dep in &def.depends_on {
            depended_by_map
                .entry(dep.clone())
                .or_default()
                .push(def.name.clone());
        }
    }

    let mut batch_tasks = std::collections::HashMap::new();

    for def in &definitions {
        let child_task_id = Uuid::new_v4();
        let child_task = Task {
            id: child_task_id,
            content: def.content.clone(),
            creator: agent_id,
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: def.name.clone(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: Some(task_id),
            batch_id: Some(batch_id),
            origin_channel: parent_origin_channel.clone(),
            routing_policy: parent_origin_channel.as_ref().map_or_else(
                || crate::domain::TaskRoutingPolicy::event(None, None),
                |ch| crate::domain::TaskRoutingPolicy::conversational(ch.clone()),
            ),
            last_evaluated_turn: None,
        };

        let depended_by = depended_by_map.get(&def.name).cloned().unwrap_or_default();

        let sub_task_config = SubTaskConfig {
            batch_id,
            child_agent_name: def.name.clone(),
            child_agent_model: def.model.clone(),
            allowed_tools: def.tools.clone(),
            parent_agent_id: agent_id,
            depends_on: def.depends_on.clone(),
            depended_by,
        };

        let child_entity = crate::ecs::spawn_task(
            commands,
            index,
            child_task,
            ShortTermMemory::default(),
            NewlyCreatedTask,
            PendingDispatch {
                kind: DispatchKind::Task,
                hint: DispatchHint {
                    strategy: DispatchStrategy::BrainLlm,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        );
        // 移除 spawn_task 附加的占位 PendingDispatch，由 subtask_dispatch_preparation_system
        // 在 DAG 依赖检查通过后重新附加（含 AgentSpawnSpec 和兄弟任务结果注入）。
        commands.entity(child_entity).remove::<PendingDispatch>();
        commands.entity(child_entity).insert(sub_task_config);

        batch_tasks.insert(
            def.name.clone(),
            crate::domain::BatchTaskStatus {
                task_id: child_task_id,
                state: BatchTaskState::Pending,
                result_summary: None,
            },
        );
    }

    debug!(
        event = "CreateTasksBatchCreated",
        %batch_id,
        parent_task_id = %task_id,
        parent_agent_id = %agent_id,
        task_count = total_count,
        task_names = ?definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ?tool_call_id,
        "sub-task batch created"
    );

    // 产出 SubTaskBatchState（附加到父 Task 实体以便后续查询）
    commands.spawn(SubTaskBatchState {
        batch_id,
        parent_tool_call_id: tool_call_id.clone().unwrap_or_default(),
        tasks: batch_tasks.clone(),
        completed_count: 0,
        total_count,
    });

    // 产出 SubTaskBatchCreatedMessage（触发父 Task 阻塞 + Brain 分发）
    commands.spawn(SubTaskBatchCreatedMessage {
        parent_task_id: task_id,
        batch_id,
        parent_tool_call_id: tool_call_id.clone().unwrap_or_default(),
        tasks: definitions,
    });

    // 产出 ToolExecutionResultMessage（让 tool calling loop 收到结果）
    // 构建包含 name 和 task_id 映射的任务列表
    let task_names: Vec<String> = batch_tasks.keys().cloned().collect();
    let tasks_with_ids: Vec<serde_json::Value> = batch_tasks
        .iter()
        .map(|(name, status)| {
            serde_json::json!({
                "name": name,
                "task_id": status.task_id.to_string()
            })
        })
        .collect();

    commands.spawn((
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id,
                request_kind,
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(format!(
                        "created {} sub-tasks (batch {}): {}",
                        total_count,
                        batch_id,
                        task_names.join(", ")
                    )),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
            },
            tool_name: "create_tasks".to_string(),
            tool_output: Ok(serde_json::json!({
                "status": "batch_created",
                "batch_id": batch_id.to_string(),
                "task_count": total_count,
                "tasks": tasks_with_ids,
            })),
            tool_call_id,
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
}

/// 生成等待任务的消息和状态
pub fn spawn_wait_for_tasks(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    agent_id: AgentId,
    tool_call_id: String,
    task_ids: Vec<TaskId>,
    timeout_secs: u64,
) {
    debug!(
        event = "WaitForTasksInitiated",
        task_ids = ?task_ids,
        timeout_secs = timeout_secs,
        "task entering wait state for child tasks"
    );

    // 在 Task Entity 上添加等待信息组件
    commands.entity(task_entity).insert(WaitingForTasksInfo {
        target_task_ids: task_ids,
        timeout_at: chrono::Utc::now() + chrono::Duration::seconds(timeout_secs as i64),
        tool_call_id,
        agent_id,
    });

    // 清理请求实体
    commands.entity(request_entity).despawn();
}

/// 验证目标任务是否为当前任务的子任务
pub fn validate_task_ownership(
    current_task_id: TaskId,
    target_task_ids: &[TaskId],
    tasks: &Query<(Entity, &mut Task)>,
) -> Result<(), ToolError> {
    let _current_task = tasks
        .iter()
        .find(|(_, t)| t.id == current_task_id)
        .map(|(_, t)| t)
        .ok_or_else(|| ToolError::NotFound(format!("current task {}", current_task_id)))?;

    for target_id in target_task_ids {
        let target = tasks
            .iter()
            .find(|(_, t)| t.id == *target_id)
            .map(|(_, t)| t)
            .ok_or_else(|| ToolError::NotFound(format!("task {}", target_id)))?;

        // 目标任务必须是当前任务的子任务（parent_task_id 匹配）
        if target.parent_task_id != Some(current_task_id) {
            return Err(ToolError::PermissionDenied(format!(
                "task {} is not a child of current task",
                target_id
            )));
        }
    }

    Ok(())
}

/// 收集目标任务的结果
pub fn collect_task_results(
    task_ids: &[TaskId],
    tasks: &Query<&Task>,
    index: &EntityIndex,
) -> Vec<TaskWaitResult> {
    task_ids
        .iter()
        .map(|id| {
            // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
            let task = index.get_task(id).and_then(|e| tasks.get(e).ok());
            TaskWaitResult {
                task_id: id.to_string(),
                status: task
                    .map(|t| t.status.clone())
                    .unwrap_or(TaskStatus::Pending),
                result: task.and_then(|t| {
                    if t.status == TaskStatus::Done {
                        Some(t.result_summary.clone())
                    } else {
                        None
                    }
                }),
                error: task.and_then(|t| {
                    if matches!(t.status, TaskStatus::Failed(_)) {
                        t.last_error.clone()
                    } else {
                        None
                    }
                }),
            }
        })
        .collect()
}

/// 生成等待结果消息
pub fn spawn_wait_result_message(
    commands: &mut Commands,
    task_id: TaskId,
    info: &WaitingForTasksInfo,
    results: Vec<TaskWaitResult>,
    timed_out: bool,
) {
    let output = serde_json::json!({
        "results": results,
        "timed_out": timed_out,
    });

    debug!(
        event = "WaitForTasksCompleted",
        task_id = %task_id,
        results_count = results.len(),
        timed_out = timed_out,
        "wait_tasks completed, resuming task"
    );

    // 生成工具执行结果消息
    commands.spawn((
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id,
                agent_id: info.agent_id,
                request_kind: crate::domain::AgentRequestKind::LlmCompletion,
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text("wait_tasks completed".to_string()),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
            },
            tool_name: "wait_tasks".to_string(),
            tool_output: Ok(output),
            tool_call_id: Some(info.tool_call_id.clone()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

/// 统一处理 Tool 执行动作
#[allow(clippy::too_many_arguments)]
pub fn handle_tool_action<B: SessionBackend>(
    commands: &mut Commands,
    index: &mut EntityIndex,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
    tasks: &mut Query<(Entity, &mut Task)>,
    agents: &Query<&mut Agent>,
    chat_sessions: &Query<&ChatSession>,
    short_term_memories: &mut Query<&mut ShortTermMemory>,
    backend: &B,
    experience_store: &mut ExperienceStore,
    pending_experience_hooks: &mut PendingExperienceHooks,
    parent_agent_id: Option<AgentId>,
    clock: &Clock,
    // 合并 ProfileGenerationContext 与 SkillUpdateContext 查询：
    // 两者都是与 WorkItem 同 entity 的 Component，任一 WorkItem entity 至多只有其中之一。
    // 通过 Option<&...> 在单 SystemParam 中表达"存在与否"，避免触发 Bevy 16 参数上限。
    context_queries: &Query<(
        Entity,
        Option<&ProfileGenerationContext>,
        Option<&SkillUpdateContext>,
        &WorkItem,
    )>,
    skill_loader: &SkillLoader,
    // ToolCallingState 查询：用于在 ProfileGeneration 收尾路径
    // （SubmitProfileUpdate / SkipProfileUpdate）despawn 关联 State，
    // 阻止 tool_calling_orchestrator_system 触发 follow-up LLM 请求。
    // 按 (task_id, work_item_id) 严格匹配，与 find_calling_state 语义一致。
    calling_states: &Query<(Entity, &ToolCallingState)>,
    // ask_user 需要把问题推送到 task 的 output_channel 对应前端。
    frontend_registry: &FrontendRegistry,
) {
    match action {
        Ok(ToolAction::Direct(_value)) => {
            // list_experience_candidates 已上桥到 async worker（kind==Async），
            // 不再走 sync 路径产生 `ToolAction::Direct`。保留 arm 防止未来误用——
            // 若 Sync 工具误返回 `Direct`，立即报错而非静默构造结果消息绕过
            // 异步桥的 hook / 通道单点落地。
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::InternalState(
                    "Direct action is retired (list_experience_candidates is async-only); \
                     BuiltinTool must not return Direct on sync path"
                        .to_string(),
                ),
            );
        }
        Ok(ToolAction::CreateBatch(definitions)) => {
            let parent_origin_channel = tasks
                .get(task_entity)
                .map(|(_, t)| t.origin_channel.clone())
                .unwrap_or_else(|_| {
                    warn!(
                        event = "ParentTaskNotFoundForSubTaskChannel",
                        task_entity = ?task_entity,
                        task_id = %request.request.task_id,
                        "parent task entity not found, falling back to Tui/default for sub-task origin_channel"
                    );
                    Some(ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                        thread_id: None,
                    })
                });
            spawn_create_tasks_messages(
                commands,
                index,
                request_entity,
                request.request.agent_id,
                request.request.task_id,
                request.request.request_kind.clone(),
                definitions,
                request.tool_call_id.clone(),
                parent_origin_channel,
            );
        }
        Ok(ToolAction::WaitForTasks {
            task_ids,
            timeout_secs,
        }) => {
            // 验证任务归属
            match validate_task_ownership(request.request.task_id, &task_ids, tasks) {
                Ok(()) => {
                    spawn_wait_for_tasks(
                        commands,
                        request_entity,
                        task_entity,
                        request.request.agent_id,
                        request.tool_call_id.clone().unwrap_or_default(),
                        task_ids,
                        timeout_secs,
                    );
                }
                Err(e) => {
                    spawn_tool_error(commands, request_entity, request, e);
                }
            }
        }
        Ok(ToolAction::ExecSession(_session_request)) => {
            // shell_exec 已上桥到 async worker（kind==Async），不再走 sync 路径
            // 产生 `ToolAction::ExecSession`。保留 arm 防止未来误用——若 Sync
            // 工具误返回 `ExecSession`，立即报错而非静默调阻塞 backend 拉长帧。
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::InternalState(
                    "ExecSession action is retired (shell_exec is async-only); \
                     BuiltinTool must not return ExecSession on sync path"
                        .to_string(),
                ),
            );
        }
        Ok(ToolAction::StartSession(session_request)) => {
            match backend.start_session(session_request) {
                Ok(handle) => {
                    let summary = SessionSummary::from_handle(&handle);
                    spawn_shell_result(
                        commands,
                        request_entity,
                        request,
                        "shell_start",
                        serde_json::json!(ShellSessionResult::from_summary(&summary)),
                    );
                }
                Err(error) => {
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::ExecutionFailed(error),
                    );
                }
            }
        }
        Ok(ToolAction::ReadSession(read_request)) => {
            if let Err(e) =
                backend.assert_task_owns_session(request.request.task_id, read_request.handle_id)
            {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::PermissionDenied(e),
                );
            } else {
                match backend.read_session(read_request) {
                    Ok(summary) => {
                        spawn_shell_result(
                            commands,
                            request_entity,
                            request,
                            "shell_read",
                            serde_json::json!(ShellSessionResult::from_summary(&summary)),
                        );
                    }
                    Err(error) => {
                        spawn_tool_error(
                            commands,
                            request_entity,
                            request,
                            ToolError::ExecutionFailed(error),
                        );
                    }
                }
            }
        }
        Ok(ToolAction::ListSessions) => match backend.list_task_sessions(request.request.task_id) {
            Ok(sessions) => {
                let payload = sessions
                    .iter()
                    .map(ShellSessionResult::from_summary)
                    .collect::<Vec<_>>();
                spawn_shell_result(
                    commands,
                    request_entity,
                    request,
                    "shell_list",
                    serde_json::json!(payload),
                );
            }
            Err(error) => {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::ExecutionFailed(error),
                );
            }
        },
        Ok(ToolAction::InputSession(input_request)) => {
            if let Err(e) =
                backend.assert_task_owns_session(request.request.task_id, input_request.handle_id)
            {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::PermissionDenied(e),
                );
            } else {
                match backend.input_session(input_request) {
                    Ok(handle) => {
                        spawn_shell_result(
                            commands,
                            request_entity,
                            request,
                            "shell_input",
                            serde_json::json!(ShellSessionResult::accepted_input(&handle)),
                        );
                    }
                    Err(error) => {
                        spawn_tool_error(
                            commands,
                            request_entity,
                            request,
                            ToolError::ExecutionFailed(error),
                        );
                    }
                }
            }
        }
        Ok(ToolAction::StopSession(handle_id)) => {
            if let Err(e) = backend.assert_task_owns_session(request.request.task_id, handle_id) {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::PermissionDenied(e),
                );
            } else {
                match backend.stop_session(handle_id) {
                    Ok(handle) => {
                        spawn_shell_result(
                            commands,
                            request_entity,
                            request,
                            "shell_stop",
                            serde_json::json!(ShellSessionResult::stopped(&handle)),
                        );
                    }
                    Err(error) => {
                        spawn_tool_error(
                            commands,
                            request_entity,
                            request,
                            ToolError::ExecutionFailed(error),
                        );
                    }
                }
            }
        }
        Ok(ToolAction::SubmitExperienceCandidate(submission)) => {
            let candidate = submission_to_candidate(
                &submission,
                request.request.agent_id,
                request.request.task_id,
            );

            // 判断当前任务是否有父任务：有则写入父层 inbox，无则作为顶层 root 候选。
            let parent_task_id = tasks
                .iter()
                .find(|(_, t)| t.id == request.request.task_id)
                .and_then(|(_, t)| t.parent_task_id);

            match parent_task_id {
                Some(pid) => {
                    let owner_agent_id = parent_agent_id.unwrap_or(request.request.agent_id);
                    experience_store.queue_for_parent(pid, owner_agent_id, candidate.clone());
                }
                None => {
                    experience_store.stage_root_candidate(candidate.clone());
                }
            }

            // 推入待派发队列，由 companion 系统触发 on_experience_candidate_submitted hook。
            pending_experience_hooks.0.push((
                crate::user_plugins::hook_point::HookPoint::OnExperienceCandidateSubmitted,
                candidate.candidate_id,
            ));

            spawn_experience_candidate_result(commands, request_entity, request, &candidate);
        }
        Ok(ToolAction::SendChannelMessage {
            channel,
            target,
            content,
            attachments,
        }) => {
            commands.spawn(crate::domain::PendingChannelSend {
                channel,
                recipient: target,
                content,
                attachments,
                tool_call_id: request.tool_call_id.clone(),
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_entity,
            });
        }
        Ok(ToolAction::StartChatRound {
            agent_name,
            agent_tags,
            message,
            context,
            handle,
        }) => {
            let parent_task_id = request.request.task_id;
            let parent_tool_call_id = request.tool_call_id.clone().unwrap_or_default();

            // 一次性从父任务 clone 出所需信息，避免 Query 借用冲突
            let (parent_origin_channel, _parent_delegate) = tasks
                .get(task_entity)
                .map(|(_, t)| (t.origin_channel.clone(), t.delegate))
                .unwrap_or_else(|_| {
                    warn!(
                        event = "ParentTaskNotFoundForChatChannel",
                        task_id = %parent_task_id,
                        "parent task entity not found, falling back to Tui/default for chat subtask origin_channel"
                    );
                    (
                        Some(ChannelId {
                            frontend: FrontendKind::Tui,
                            user_id: "default".to_string(),
                            thread_id: None,
                        }),
                        None,
                    )
                });

            let (child_task_id, batch_id) = if let Some(handle) = handle {
                // 继续已有对话：先只读收集信息，再单独修改
                let Some((child_entity, child_task)) = tasks
                    .iter()
                    .find(|(_, t)| t.id == handle)
                    .map(|(e, t)| (e, t.clone()))
                else {
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::NotFound(format!("chat handle {}", handle)),
                    );
                    return;
                };

                if child_task.parent_task_id != Some(parent_task_id) {
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::PermissionDenied(
                            "chat handle does not belong to current task".to_string(),
                        ),
                    );
                    return;
                }

                if !matches!(
                    child_task.status,
                    TaskStatus::Waiting(WaitingReason::ChatAgent)
                ) {
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::InvalidInput("chat handle is not in waiting state".to_string()),
                    );
                    return;
                }

                let new_batch_id = Uuid::new_v4();
                let child_task_id = child_task.id;

                // 追加本轮用户消息到子任务 STM
                if let Ok(mut stm) = short_term_memories.get_mut(child_entity) {
                    stm.add_entry(EntryRole::User, &message, Default::default());
                }

                // 更新 ChatSession（保留 child_agent_name）
                let child_agent_name = chat_sessions
                    .get(child_entity)
                    .map(|s| s.child_agent_name.clone())
                    .unwrap_or_default();
                commands.entity(child_entity).insert(ChatSession {
                    child_agent_name: child_agent_name.clone(),
                    parent_tool_call_id: parent_tool_call_id.clone(),
                    current_batch_id: new_batch_id,
                });

                // 附加 PendingDispatch，由 dispatch_system 处理 DirectDelegate 派发
                commands.entity(child_entity).insert(PendingDispatch {
                    kind: DispatchKind::Task,
                    hint: DispatchHint {
                        strategy: DispatchStrategy::DirectDelegate,
                        preferred_agent_name: Some(child_agent_name),
                        required_skill_id: None,
                        agent_spawn_spec: None,
                    },
                });

                // 唤醒子任务
                if let Ok((_, mut task)) = tasks.get_mut(child_entity) {
                    task.status = TaskStatus::Ready;
                    task.updated_at = clock.0;
                }

                (child_task_id, new_batch_id)
            } else {
                // 开始新对话
                let agent = {
                    let name = agent_name.as_deref();
                    let by_name = name.and_then(|n| {
                        agents
                            .iter()
                            .find(|a| a.kind == AgentKind::Persistent && a.profile.name == n)
                    });
                    if let Some(a) = by_name {
                        Some(a)
                    } else if !agent_tags.is_empty() {
                        agents.iter().find(|a| {
                            a.kind == AgentKind::Persistent
                                && agent_tags
                                    .iter()
                                    .all(|tag| a.capabilities.tags.contains(tag))
                        })
                    } else {
                        None
                    }
                };
                let Some(agent) = agent else {
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::NotFound("no matching persistent agent found".to_string()),
                    );
                    return;
                };

                let child_task_id = Uuid::new_v4();
                let batch_id = Uuid::new_v4();

                let mut initial_stm = ShortTermMemory::default();
                if let Some(ref ctx) = context {
                    initial_stm.add_entry(
                        EntryRole::User,
                        format!("[System context]\n{}\n\n{}", ctx, message),
                        Default::default(),
                    );
                } else {
                    initial_stm.add_entry(EntryRole::User, &message, Default::default());
                }

                let mut child_task = Task::from_user_input(
                    &message,
                    0,
                    parent_origin_channel.clone().unwrap_or_else(|| ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                        thread_id: None,
                    }),
                );
                child_task.id = child_task_id;
                child_task.parent_task_id = Some(parent_task_id);
                child_task.delegate = Some(agent.id);
                child_task.creator = request.request.agent_id;
                child_task.status = TaskStatus::Ready;
                child_task.multi_turn = true;

                commands.spawn((
                    child_task,
                    initial_stm,
                    ChatSession {
                        child_agent_name: agent.profile.name.clone(),
                        parent_tool_call_id: parent_tool_call_id.clone(),
                        current_batch_id: batch_id,
                    },
                    PendingDispatch {
                        kind: DispatchKind::Task,
                        hint: DispatchHint {
                            strategy: DispatchStrategy::DirectDelegate,
                            preferred_agent_name: Some(agent.profile.name.clone()),
                            required_skill_id: None,
                            agent_spawn_spec: None,
                        },
                    },
                ));

                (child_task_id, batch_id)
            };

            commands.spawn(ChatRoundStartedMessage {
                parent_task_id,
                child_task_id,
                batch_id,
                parent_tool_call_id,
            });

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SubmitProfileUpdate {
            name,
            tags,
            description,
        }) => {
            // 通过 Query 查找匹配 task_id 的 ProfileGenerationContext Component
            // （与 WorkItem 同 Entity）。LLM 成功调用工具，异常计数归 0。
            let mut resolved_kind = crate::domain::ProfileGenerationKind::Incubation;
            if let Some((wi_entity, Some(ctx), _, _wi)) = context_queries
                .iter()
                .find(|(_, prof, _, wi)| prof.is_some() && wi.task_id == request.request.task_id)
            {
                resolved_kind = ctx.kind.clone();
                // 重置 exception_count：clone + modify + insert（不可变 Query + Commands 写回）
                let mut new_ctx = ctx.clone();
                new_ctx.exception_count = 0;
                commands.entity(wi_entity).insert(new_ctx);
            }

            // spawn ProfileGenerationCompletedMessage 供 profile_generation_completion_system 消费
            commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                generated_profile: Some(crate::domain::GeneratedProfile {
                    name: name.clone(),
                    tags: tags.clone(),
                    description: description.clone(),
                }),
                kind: resolved_kind.clone(),
            });

            // 返回工具执行结果给 LLM
            let output = serde_json::json!({
                "status": "submitted",
                "name": name,
                "tags": tags,
                "description": description,
            });

            let execution_result = AgentExecutionResult {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_kind: request.request.request_kind.clone(),
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(format!("profile submitted: {}", name)),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
            };

            commands.spawn((
                ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: "submit_profile_update".to_string(),
                    tool_output: Ok(output),
                    tool_call_id: request.tool_call_id.clone(),
                    processed: false,
                    original_tool_output: None,
                },
                ToolReturnedHookPending,
            ));

            // ProfileGeneration 收尾：despawn 关联的 ToolCallingState，
            // 阻止 tool_calling_orchestrator_system 触发 follow-up LLM 请求。
            // profile 已提交进入审批，LLM 对话语义上结束，State 应随之消亡。
            // 按 (task_id, work_item_id) 严格匹配，与 find_calling_state 语义一致。
            for (cs_entity, cs) in calling_states.iter() {
                if cs.task_id == request.request.task_id
                    && cs.work_item_id == request.request.work_item_id
                {
                    commands.entity(cs_entity).despawn();
                    debug!(
                        event = "ToolCallingStateDespawned",
                        task_id = %request.request.task_id,
                        work_item_id = ?request.request.work_item_id,
                        reason = "profile_generation_submit_completed",
                        "despawned ToolCallingState to prevent follow-up LLM loop"
                    );
                }
            }

            debug!(
                event = "ProfileUpdateSubmitted",
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                kind = ?resolved_kind,
                "profile update submitted by LLM"
            );

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SkipProfileUpdate) => {
            // 通过 Query 查找匹配 task_id 的 ProfileGenerationContext Component
            // （与 WorkItem 同 Entity）。LLM 成功调用工具，异常计数归 0。
            let mut resolved_kind = crate::domain::ProfileGenerationKind::Update;
            if let Some((wi_entity, Some(ctx), _, _wi)) = context_queries
                .iter()
                .find(|(_, prof, _, wi)| prof.is_some() && wi.task_id == request.request.task_id)
            {
                resolved_kind = ctx.kind.clone();
                let mut new_ctx = ctx.clone();
                new_ctx.exception_count = 0;
                commands.entity(wi_entity).insert(new_ctx);
            }

            // spawn ProfileGenerationCompletedMessage（None 表示 skip）
            commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                generated_profile: None,
                kind: resolved_kind.clone(),
            });

            let output = serde_json::json!({"status": "skipped"});

            let execution_result = AgentExecutionResult {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_kind: request.request.request_kind.clone(),
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text("profile update skipped".to_string()),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
            };

            commands.spawn((
                ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: "skip_profile_update".to_string(),
                    tool_output: Ok(output),
                    tool_call_id: request.tool_call_id.clone(),
                    processed: false,
                    original_tool_output: None,
                },
                ToolReturnedHookPending,
            ));

            // ProfileGeneration 收尾（与 SubmitProfileUpdate 分支对称）：
            // despawn 关联的 ToolCallingState，阻止 follow-up LLM 请求。
            for (cs_entity, cs) in calling_states.iter() {
                if cs.task_id == request.request.task_id
                    && cs.work_item_id == request.request.work_item_id
                {
                    commands.entity(cs_entity).despawn();
                    debug!(
                        event = "ToolCallingStateDespawned",
                        task_id = %request.request.task_id,
                        work_item_id = ?request.request.work_item_id,
                        reason = "profile_generation_skip_completed",
                        "despawned ToolCallingState to prevent follow-up LLM loop"
                    );
                }
            }

            debug!(
                event = "ProfileUpdateSkipped",
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                kind = ?resolved_kind,
                "profile update skipped by LLM"
            );

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SubmitSkillUpdate {
            operations,
            rationale,
        }) => {
            // skill_id / base_version / new_version 由 orchestrator 从
            // SkillUpdateContext 服务端权威注入，避免 LLM 臆造 skill_id。
            //
            // work_item_id（Uuid）仍需要保留：用于 SkillUpdateCompletedMessage.work_item_id
            // 字段与日志关联。work_item_entity（Entity）则用于 O(1) 直接查询 context。
            let work_item_id = match request.request.work_item_id {
                Some(id) => id,
                None => {
                    warn!(
                        event = "SkillUpdateMissingWorkItemId",
                        task_id = %request.request.task_id,
                        agent_id = %request.request.agent_id,
                        "submit_skill_update missing work_item_id, rejecting"
                    );
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::InternalState(
                            "work_item_id missing for submit_skill_update".to_string(),
                        ),
                    );
                    return;
                }
            };

            // [重要-1] 修复：work_item_entity 为 None 是不可达路径。
            // 前置条件：llm_response.rs 中 work_item_entity 是基于同一 WorkItem query 设置的，
            // 反查 work_item_id 成功意味着 work_item_entity 也应为 Some。
            // 原实现为 warn + spawn 独立 entity，但独立 entity 没有 SkillUpdateContext，
            // 会被 completion_system 的 fallback 路径 despawn，形成"fallback 必然失败"链。
            // 改为 error! + 直接拒绝，与其他错误路径风格一致，避免伪精细控制面。
            let work_item_entity = match request.work_item_entity {
                Some(e) => e,
                None => {
                    error!(
                        event = "SkillUpdateMissingWorkItemEntity",
                        task_id = %request.request.task_id,
                        agent_id = %request.request.agent_id,
                        work_item_id = %work_item_id,
                        "work_item_entity is None despite context lookup succeeded; \
                         rejecting submit_skill_update"
                    );
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::InternalState(
                            "work_item_entity missing for submit_skill_update \
                             (framework state inconsistency)"
                                .to_string(),
                        ),
                    );
                    return;
                }
            };

            // [重要-2] 修复：用 work_item_entity 做 O(1) 直接查询，替代 O(n) Uuid 反查。
            // 三层错误分支覆盖所有失败情况，每条路径都 spawn_tool_error 直接拒绝。
            let Some((_, _, context_opt, _)) = context_queries.get(work_item_entity).ok() else {
                warn!(
                    event = "SkillUpdateWorkItemNotInContextQueries",
                    task_id = %request.request.task_id,
                    agent_id = %request.request.agent_id,
                    work_item_id = %work_item_id,
                    work_item_entity = ?work_item_entity,
                    "WorkItem entity not found in context_queries, rejecting submit_skill_update"
                );
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InternalState(format!(
                        "WorkItem entity {:?} not in context_queries for work_item_id={}",
                        work_item_entity, work_item_id
                    )),
                );
                return;
            };

            let Some(context) = context_opt else {
                warn!(
                    event = "SkillUpdateContextNotFound",
                    task_id = %request.request.task_id,
                    agent_id = %request.request.agent_id,
                    work_item_id = %work_item_id,
                    work_item_entity = ?work_item_entity,
                    "SkillUpdateContext not found on work_item_entity, rejecting submit_skill_update"
                );
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InternalState(format!(
                        "SkillUpdateContext not found on work_item_entity {:?} for work_item_id={}",
                        work_item_entity, work_item_id
                    )),
                );
                return;
            };

            let skill_id = context.skill_id.clone();
            let base_version = context.base_version;
            let new_version = base_version + 1;

            // dry-run：提前 apply 一次 operations 到当前 SKILL.md，
            // 检查 section 名是否存在 / frontmatter 字段是否在白名单。
            // Bug C 修复：之前 operations 错误要等到 completion_system 才发现，
            // 错误反馈异步且不可见；现在改为同步返回 ToolError，LLM 可立即修正。
            //
            // TOCTOU 说明：dry-run 通过后 completion_system 会重新读取 SKILL.md 并 apply。
            // 两次读取之间存在理论上的 time-of-check-to-time-of-use 窗口，本实现未加文件锁。
            // 当前 skill-update 串行执行（同一 task 同一时刻至多一个 SkillUpdate WorkItem），
            // 外部并发修改风险可接受。若未来允许并发 skill update，需要引入文件锁或
            // work item 级互斥。
            let skill_path = skill_loader.skill_md_path(&skill_id);
            let content = match std::fs::read_to_string(&skill_path) {
                Ok(c) => c,
                Err(e) => {
                    warn!(
                        event = "SkillMdReadFailed",
                        task_id = %request.request.task_id,
                        agent_id = %request.request.agent_id,
                        skill_id = %skill_id.as_string(),
                        skill_path = ?skill_path,
                        error = %e,
                        "failed to read SKILL.md for dry-run, rejecting submit_skill_update"
                    );
                    spawn_tool_error(
                        commands,
                        request_entity,
                        request,
                        ToolError::InternalState(format!(
                            "failed to read SKILL.md for dry-run: {}",
                            e
                        )),
                    );
                    return;
                }
            };

            if let Err(apply_err) = apply_skill_operations(&content, &operations) {
                warn!(
                    event = "SkillUpdateDryRunFailed",
                    task_id = %request.request.task_id,
                    agent_id = %request.request.agent_id,
                    skill_id = %skill_id.as_string(),
                    error = %apply_err,
                    "operations dry-run failed, rejecting submit_skill_update"
                );
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InvalidInput(format!(
                        "operations dry-run failed: {}. 注意：replace_section / replace_subsection 的 content 字段不得包含标题行本身（系统会自动保留原标题），只需提供标题下方的正文内容。若为 SectionNotFound，请确保 section 名与原 SKILL.md 中实际存在的标题完全一致。",
                        apply_err
                    )),
                );
                return;
            }

            // dry-run 通过，将 SkillUpdateCompletedMessage insert 到 WorkItem entity 上
            // （而非 spawn 独立 entity），让 skill_update_completion_system 通过同 entity 的
            // Component 查询直接拿到 SkillUpdateContext，避免用 work_item_id 反查。
            // work_item_entity 已在前面校验为 Some（None 路径已 early return）。
            let completed_message = crate::domain::SkillUpdateCompletedMessage {
                work_item_id,
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                skill_id: skill_id.clone(),
                base_version,
                new_version,
                operations: operations.clone(),
                rationale: rationale.clone(),
            };
            commands.entity(work_item_entity).insert(completed_message);

            // 返回工具执行结果给 LLM
            let output = serde_json::json!({
                "status": "submitted",
                "skill_id": skill_id.as_string(),
                "base_version": base_version,
                "new_version": new_version,
                "operations_count": operations.len(),
                "rationale": rationale,
            });

            let execution_result = AgentExecutionResult {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_kind: request.request.request_kind.clone(),
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(format!(
                        "skill update submitted: {} (v{} -> v{})",
                        skill_id.as_string(),
                        base_version,
                        new_version
                    )),
                    reasoning_content: None,
                }),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
            };

            commands.spawn((
                ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: "submit_skill_update".to_string(),
                    tool_output: Ok(output),
                    tool_call_id: request.tool_call_id.clone(),
                    processed: false,
                    original_tool_output: None,
                },
                ToolReturnedHookPending,
            ));

            debug!(
                event = "SkillUpdateSubmitted",
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                skill_id = %skill_id.as_string(),
                base_version,
                new_version,
                operations_count = operations.len(),
                "skill update submitted by LLM"
            );

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::AskUser { question }) => {
            let task_id = request.request.task_id;
            let agent_id = request.request.agent_id;
            // request.tool_call_id 是 Option<String>，AskUserPending.tool_call_id 是 String。
            // ask_user 由 LLM 发起，正常情况 tool_call_id 为 Some；None 时用空串兜底（不影响 LLM loop 续跑）。
            let tool_call_id = request.tool_call_id.clone().unwrap_or_default();

            // 1. 读取 task 的 output_channel
            let output_channel = tasks
                .get(task_entity)
                .map(|(_, t)| t.routing_policy.output_channel.clone())
                .ok()
                .flatten();

            // 2. 无 output_channel 时返回错误（避免 task 永远卡在 Waiting(AskUser)）
            let Some(channel) = output_channel else {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InvalidInput(
                        "ask_user requires task with output_channel".to_string(),
                    ),
                );
                return;
            };

            // 3. 通过 EngineEvent::Text 把问题推送到 output_channel
            let event = EngineEvent::Text {
                target: EventTarget::Directed(vec![channel]),
                role: MessageRole::Agent,
                content: question.clone(),
                task_id: Some(task_id),
            };
            for frontend in &frontend_registry.frontends {
                frontend.push_event(event.clone());
            }

            // 4. 在 task entity 上挂 AskUserPending（先 insert，再切 status，保证不变量）
            commands.entity(task_entity).insert(AskUserPending {
                tool_call_id,
                agent_id,
            });

            // 5. task.status = Waiting(AskUser)
            if let Ok((_, mut task)) = tasks.get_mut(task_entity) {
                task.status = TaskStatus::Waiting(WaitingReason::AskUser);
            }

            // 6. despawn ToolExecutionRequestMessage
            commands.entity(request_entity).despawn();
        }
        Err(e) => {
            spawn_tool_error(commands, request_entity, request, e);
        }
    }
}

/// 将 ExperienceCandidateSubmission 转换为 ExperienceCandidate。
///
/// 将工具层的提交数据转换为领域模型，载荷根据 kind 进行解析。
fn submission_to_candidate(
    submission: &ExperienceCandidateSubmission,
    agent_id: AgentId,
    task_id: TaskId,
) -> ExperienceCandidate {
    let payload = match &submission.kind {
        ExperienceKindHint::Knowledge => {
            let content = submission.content.clone().unwrap_or_default();
            ExperienceCandidatePayload::Knowledge { content }
        }
        ExperienceKindHint::Skill => {
            let name = submission.title.clone();
            let description = submission.skill_description.clone().unwrap_or_default();
            let instructions = submission.instructions.clone().unwrap_or_default();
            let file_refs = submission.file_refs.clone();
            ExperienceCandidatePayload::Skill {
                name,
                description,
                instructions,
                file_refs,
            }
        }
    };

    ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: submission.title.clone(),
        kind_hint: submission.kind.clone(),
        payload,
        dependency_refs: Vec::new(),
        status: crate::domain::ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        derived_from_candidate_ids: Vec::new(),
    }
}

/// 生成经验候选提交结果。
///
/// 返回成功确认，候选实际存入 ExperienceStore 由 experience_collection 系统处理。
fn spawn_experience_candidate_result(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    candidate: &ExperienceCandidate,
) {
    let output = serde_json::json!({
        "status": "submitted",
        "candidate_id": candidate.candidate_id.to_string(),
        "title": candidate.title,
        "kind_hint": format!("{:?}", candidate.kind_hint),
    });

    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text("experience candidate submitted".to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result: execution_result,
            tool_name: "submit_experience_candidate".to_string(),
            tool_output: Ok(output),
            tool_call_id: request.tool_call_id.clone(),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
}

/// 恢复 Task 状态（从 Waiting(ToolExecution) 恢复到 Ready 或保持 Waiting(ToolExecution)）
///
/// 仅处理 Waiting(ToolExecution) 状态。其他 Waiting 变体（AskUser、ChatAgent、
/// SubTaskBatch 等）由各自的发起系统主动挂起，不应被本函数覆盖。
pub fn restore_task_after_tool(
    tasks: &mut Query<(Entity, &mut Task)>,
    calling_states: &Query<(Entity, &ToolCallingState)>,
    index: &EntityIndex,
    task_id: TaskId,
) {
    // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
    if let Some((_, mut task)) = index.get_task(&task_id).and_then(|e| tasks.get_mut(e).ok()) {
        // 仅恢复 Waiting(ToolExecution)；其他 Waiting 变体由各自系统管理，不在此处覆盖
        if !matches!(
            task.status,
            TaskStatus::Waiting(WaitingReason::ToolExecution)
        ) {
            return;
        }
        let has_calling_state = calling_states.iter().any(|(_, cs)| cs.task_id == task.id);
        task.status = if has_calling_state {
            TaskStatus::Waiting(WaitingReason::ToolExecution)
        } else {
            TaskStatus::Ready
        };
    }
}

/// 生成 Tool 错误结果
pub fn spawn_tool_error(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    error: ToolError,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Err(crate::domain::ExecutionError::Unknown(error.to_string())),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result: execution_result,
            tool_name: request.tool_name.clone(),
            tool_output: Err(error),
            tool_call_id: request.tool_call_id.clone(),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
}

/// 生成 Shell 工具执行结果
pub fn spawn_shell_result(
    commands: &mut Commands,
    request_entity: Entity,
    request: &ToolExecutionRequestMessage,
    tool_name: &str,
    tool_output: serde_json::Value,
) {
    let execution_result = AgentExecutionResult {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        request_kind: request.request.request_kind.clone(),
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(format!("{} completed", tool_name)),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result: execution_result,
            tool_name: tool_name.to_string(),
            tool_output: Ok(tool_output),
            tool_call_id: request.tool_call_id.clone(),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));

    commands.entity(request_entity).despawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AgentRequestKind;

    /// 测试系统：从世界中的父 Task 读取 origin_channel，调用 spawn_create_tasks_messages。
    ///
    /// 通过系统而非直接调用 `world.commands()`，确保 `app.update()` 能正确刷新 Commands。
    fn spawn_subtasks_for_inheritance_test(mut commands: Commands, mut index: ResMut<EntityIndex>, tasks: Query<&Task>) {
        let parent_task = tasks
            .iter()
            .find(|t| t.content == "parent")
            .expect("parent task should exist");
        let parent_task_id = parent_task.id;
        let parent_origin_channel = parent_task.origin_channel.clone();

        // spawn_create_tasks_messages 会在结束时 despawn request_entity，
        // 因此需要一个真实存在的 entity。
        let request_entity = commands.spawn(()).id();

        spawn_create_tasks_messages(
            &mut commands,
            &mut index,
            request_entity,
            uuid::Uuid::nil(),
            parent_task_id,
            AgentRequestKind::LlmCompletion,
            vec![SubTaskDefinition {
                name: "child-agent".to_string(),
                content: "do something".to_string(),
                tools: vec![],
                depends_on: vec![],
                model: None,
            }],
            None,
            parent_origin_channel,
        );
    }

    /// 验证 `spawn_create_tasks_messages` 生成的子 Task 继承父 Task 的非默认 origin_channel。
    ///
    /// 回归保护：曾出现过子任务硬编码 `Tui/default` 的回归。所有现有集成测试的父任务均使用
    /// `default_channel()` (Tui/default)，因此继承与硬编码无法区分。本测试使用 Telegram 通道
    /// 作为父通道，确保子任务的 `origin_channel == telegram_channel`，而非 `Tui/default`。
    #[test]
    fn create_tasks_subtask_inherits_parent_origin_channel() {
        let mut app = App::new();
        app.init_resource::<EntityIndex>();

        let telegram_channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        };

        // 生成父 Task，使用非默认的 Telegram 通道
        let parent_task_id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        let parent_task = Task {
            id: parent_task_id,
            content: "parent".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(telegram_channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(
                telegram_channel.clone(),
            ),
            last_evaluated_turn: None,
        };
        app.world_mut()
            .spawn((parent_task, ShortTermMemory::default()));

        app.add_systems(Update, spawn_subtasks_for_inheritance_test);
        app.update();

        // 查询生成的子 Task（通过 parent_task_id 过滤）
        let child_tasks: Vec<_> = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .filter(|t| t.parent_task_id == Some(parent_task_id))
            .collect();

        assert_eq!(
            child_tasks.len(),
            1,
            "exactly one child task should be spawned"
        );
        assert_eq!(
            child_tasks[0].origin_channel,
            Some(telegram_channel),
            "subtask should inherit parent's Telegram channel, not Tui/default"
        );
        // 显式断言：不得回退到硬编码的 Tui/default
        assert_ne!(
            child_tasks[0].origin_channel,
            Some(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            }),
            "subtask channel must NOT be the hardcoded Tui/default"
        );
    }

    /// 验证 spawn_create_tasks_messages 将子任务登记进 EntityIndex。
    ///
    /// 回归保护：子任务曾因直接 commands.spawn 绕过中心封装，
    /// 导致 EntityIndex.tasks 中查无子任务，brain_decision_system 静默丢弃决策结果。
    #[test]
    fn create_tasks_subtask_registered_in_entity_index() {
        let mut app = App::new();
        app.init_resource::<EntityIndex>();

        let parent_task_id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        let parent_task = Task {
            id: parent_task_id,
            content: "parent".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            }),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            }),
            last_evaluated_turn: None,
        };
        app.world_mut().spawn((parent_task, ShortTermMemory::default()));

        app.add_systems(Update, spawn_subtasks_for_index_test);
        app.update();

        // 验证子任务在 EntityIndex 中：先收集子任务 ID，再查询索引（避免 &app / &mut app 借用冲突）
        let child_tasks: Vec<_> = app
            .world_mut()
            .query::<&Task>()
            .iter(app.world())
            .filter(|t| t.parent_task_id == Some(parent_task_id))
            .collect();

        assert_eq!(
            child_tasks.len(),
            1,
            "exactly one child task should be spawned"
        );

        let child_task_id = child_tasks[0].id;
        let index = app.world().resource::<EntityIndex>();
        assert!(
            index.get_task(&child_task_id).is_some(),
            "child task {} must be registered in EntityIndex.tasks",
            child_task_id
        );
    }

    /// 测试用系统：调用 spawn_create_tasks_messages 并传入 EntityIndex。
    fn spawn_subtasks_for_index_test(
        mut commands: Commands,
        mut index: ResMut<EntityIndex>,
        tasks: Query<&Task>,
    ) {
        let parent_task = tasks
            .iter()
            .find(|t| t.content == "parent")
            .expect("parent task should exist");
        let parent_task_id = parent_task.id;
        let parent_origin_channel = parent_task.origin_channel.clone();

        let request_entity = commands.spawn(()).id();

        spawn_create_tasks_messages(
            &mut commands,
            &mut index,
            request_entity,
            uuid::Uuid::nil(),
            parent_task_id,
            AgentRequestKind::LlmCompletion,
            vec![SubTaskDefinition {
                name: "child-agent".to_string(),
                content: "do something".to_string(),
                tools: vec![],
                depends_on: vec![],
                model: None,
            }],
            None,
            parent_origin_channel,
        );
    }

    // ============ AskUser arm 测试 ============
    //
    // 通过 test system wrapper 调用 `handle_tool_action`，验证：
    // 1. task 切到 Waiting(AskUser)
    // 2. AskUserPending 组件挂到 task entity（先 insert 再切 status 的不变量）
    // 3. EngineEvent::Text 推送到 frontend
    // 4. request_entity 被 despawn
    // 5. 无 output_channel 时返回错误（task 不切 Waiting）

    use crate::SharedKnowledgeBase;
    use crate::domain::{Frontend, UserAction};
    use std::sync::{Arc, Mutex};

    /// 捕获推送事件的 mock frontend
    struct MockFrontend {
        kind: FrontendKind,
        events: Arc<Mutex<Vec<EngineEvent>>>,
    }

    impl Frontend for MockFrontend {
        fn kind(&self) -> FrontendKind {
            self.kind.clone()
        }
        fn push_event(&self, event: EngineEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
    }

    /// 测试 system：从 world 中取第一个 ToolExecutionRequestMessage，
    /// 调用 handle_tool_action 处理 AskUser action。
    #[allow(clippy::too_many_arguments)]
    fn ask_user_test_system(
        mut commands: Commands,
        mut index: ResMut<EntityIndex>,
        mut tasks: Query<(Entity, &mut Task)>,
        agents: Query<&mut Agent>,
        chat_sessions: Query<&ChatSession>,
        mut short_term_memories: Query<&mut ShortTermMemory>,
        backend: Res<crate::systems::NativeProcessBackend>,
        mut experience_store: ResMut<ExperienceStore>,
        mut pending_experience_hooks: ResMut<PendingExperienceHooks>,
        clock: Res<Clock>,
        context_queries: Query<(
            Entity,
            Option<&ProfileGenerationContext>,
            Option<&SkillUpdateContext>,
            &WorkItem,
        )>,
        skill_loader: Res<SkillLoader>,
        calling_states: Query<(Entity, &ToolCallingState)>,
        frontend_registry: Res<FrontendRegistry>,
        requests: Query<(Entity, &ToolExecutionRequestMessage)>,
    ) {
        let Some((request_entity, request)) = requests.iter().next() else {
            return;
        };
        let task_entity = tasks
            .iter()
            .find(|(_, t)| t.id == request.request.task_id)
            .map(|(e, _)| e)
            .expect("task entity should exist for request");

        handle_tool_action(
            &mut commands,
            &mut index,
            request_entity,
            task_entity,
            request,
            Ok(ToolAction::AskUser {
                question: "what is your name?".to_string(),
            }),
            &mut tasks,
            &agents,
            &chat_sessions,
            &mut short_term_memories,
            &*backend,
            &mut experience_store,
            &mut pending_experience_hooks,
            None,
            &clock,
            &context_queries,
            &skill_loader,
            &calling_states,
            &frontend_registry,
        );
    }

    /// 构造一个带 output_channel 的 Task
    fn make_ask_user_task(channel: ChannelId) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: Uuid::new_v4(),
            content: "ask".to_string(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
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

    /// 构造一个 ToolExecutionRequestMessage，关联到指定 task
    fn make_ask_user_request(task_id: Uuid, agent_id: Uuid) -> ToolExecutionRequestMessage {
        ToolExecutionRequestMessage {
            request: crate::domain::AgentExecutionRequest {
                task_id,
                agent_id,
                request_kind: crate::domain::AgentRequestKind::ToolExecution {
                    tool_name: "ask_user".to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
                model_override: None,
            },
            tool_name: "ask_user".to_string(),
            tool_input: serde_json::json!({"question": "what is your name?"}),
            pending_confirmation_id: None,
            tool_call_id: Some("call-1".to_string()),
            pending_confirmation_options: None,
            work_item_entity: None,
            confirmed_once: false,
        }
    }

    /// 初始化测试所需资源（不含 frontend，由各测试单独注入）
    fn init_ask_user_world(world: &mut World) {
        world.init_resource::<SharedKnowledgeBase>();
        world.init_resource::<ExperienceStore>();
        world.init_resource::<PendingExperienceHooks>();
        world.init_resource::<EntityIndex>();
        world.insert_resource(crate::systems::NativeProcessBackend::default());
        world.insert_resource(Clock::default());
        world.insert_resource(SkillLoader::new(std::path::PathBuf::from(
            "/nonexistent_skills_root",
        )));
    }

    /// ask_user 成功路径：task 切到 Waiting(AskUser)，AskUserPending 挂载，
    /// 问题推送到 frontend，request 被 despawn
    #[test]
    fn ask_user_action_sets_task_to_waiting_ask_user() {
        let mut world = World::new();
        init_ask_user_world(&mut world);
        world.insert_resource(FrontendRegistry { frontends: vec![] });

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "test".to_string(),
            thread_id: None,
        };
        let task = make_ask_user_task(channel);
        let task_id = task.id;
        let task_entity = world.spawn(task).id();

        let agent_id = Uuid::new_v4();
        world.spawn(ask_user_test_agent(agent_id));

        let request = make_ask_user_request(task_id, agent_id);
        world.spawn(request);

        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);

        let task = world
            .query::<&Task>()
            .get(&world, task_entity)
            .expect("task should exist");
        assert_eq!(
            task.status,
            TaskStatus::Waiting(WaitingReason::AskUser),
            "task should be Waiting(AskUser)"
        );

        let pending = world
            .query::<&AskUserPending>()
            .get(&world, task_entity)
            .expect("AskUserPending should be inserted on task entity");
        assert_eq!(pending.tool_call_id, "call-1");
        assert_eq!(pending.agent_id, agent_id);

        let remaining_requests = world
            .query::<&ToolExecutionRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(remaining_requests, 0, "request should be despawned");
    }

    /// ask_user 成功路径：EngineEvent::Text 推送到 frontend，内容与 target 正确
    #[test]
    fn ask_user_action_pushes_text_event_to_output_channel() {
        let mut world = World::new();
        init_ask_user_world(&mut world);

        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "alice".to_string(),
            thread_id: None,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let mock = MockFrontend {
            kind: FrontendKind::Tui,
            events: events.clone(),
        };
        world.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(mock)],
        });

        let task = make_ask_user_task(channel.clone());
        let task_id = task.id;
        let task_entity = world.spawn(task).id();

        let agent_id = Uuid::new_v4();
        world.spawn(ask_user_test_agent(agent_id));

        world.spawn(make_ask_user_request(task_id, agent_id));

        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);

        let captured = events.lock().unwrap().clone();
        assert_eq!(captured.len(), 1, "exactly one event should be pushed");
        match &captured[0] {
            EngineEvent::Text {
                target,
                role,
                content,
                task_id: evt_task_id,
            } => {
                assert_eq!(*role, MessageRole::Agent, "event role should be Agent");
                assert_eq!(content, "what is your name?");
                assert_eq!(*evt_task_id, Some(task_id));
                match target {
                    EventTarget::Directed(channels) => {
                        assert_eq!(channels.len(), 1);
                        assert_eq!(channels[0], channel);
                    }
                    other => panic!("expected Directed target, got {other:?}"),
                }
            }
            other => panic!("expected EngineEvent::Text, got {other:?}"),
        }

        // 同时验证 task 状态也切了（不变量：先 insert 再切 status）
        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert_eq!(task.status, TaskStatus::Waiting(WaitingReason::AskUser));
    }

    /// ask_user 无 output_channel：返回错误，task 不切 Waiting，request 不 despawn
    #[test]
    fn ask_user_action_without_output_channel_returns_error() {
        let mut world = World::new();
        init_ask_user_world(&mut world);
        world.insert_resource(FrontendRegistry { frontends: vec![] });

        // 构造一个无 output_channel 的 task（用 scheduled_task 也不行，它可能带 channel）
        // 直接用 event policy（output_channel = None）
        let now = chrono::Utc::now();
        let task_id = Uuid::new_v4();
        let task = Task {
            id: task_id,
            content: "ask-no-channel".to_string(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: None,
            routing_policy: crate::domain::TaskRoutingPolicy::event(None, None),
            last_evaluated_turn: None,
        };
        let task_entity = world.spawn(task).id();

        let agent_id = Uuid::new_v4();
        world.spawn(ask_user_test_agent(agent_id));

        world.spawn(make_ask_user_request(task_id, agent_id));

        let mut schedule = Schedule::default();
        schedule.add_systems(ask_user_test_system);
        schedule.run(&mut world);

        // task 不应切到 Waiting(AskUser)
        let task = world.query::<&Task>().get(&world, task_entity).unwrap();
        assert_ne!(
            task.status,
            TaskStatus::Waiting(WaitingReason::AskUser),
            "task must NOT be Waiting(AskUser) when no output_channel"
        );
        assert_eq!(task.status, TaskStatus::Pending, "task should stay Pending");

        // AskUserPending 不应挂载
        let pending_exists = world
            .query::<&AskUserPending>()
            .get(&world, task_entity)
            .is_ok();
        assert!(
            !pending_exists,
            "AskUserPending must NOT be inserted when no output_channel"
        );

        // 应该生成错误结果消息（spawn_tool_error 会 spawn ToolExecutionResultMessage）
        let error_results = world
            .query::<&ToolExecutionResultMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            error_results, 1,
            "spawn_tool_error should produce one ToolExecutionResultMessage"
        );
    }

    /// 构造一个最小 Agent 用于 ask_user 测试
    fn ask_user_test_agent(agent_id: Uuid) -> Agent {
        Agent {
            id: agent_id,
            profile: crate::domain::AgentProfile {
                name: "ask-user-agent".to_string(),
                model: "test".to_string(),
            },
            capabilities: crate::domain::AgentCapabilities {
                tags: vec![],
                description: "test".to_string(),
            },
            kind: AgentKind::TaskScoped,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions {
                default_permission: crate::domain::ToolPermission::Allow,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
            system_prompt: None,
        }
    }
}
