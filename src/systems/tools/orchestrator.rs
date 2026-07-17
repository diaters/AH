//! Tool 执行协调器
//!
//! 处理 Tool 执行动作和消息生成。

use crate::prelude::*;
use serde::Serialize;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::app::Clock;
use crate::contracts::SessionBackend;
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, AgentId, AgentKind, BatchTaskState,
    ChannelId, ChatRoundStartedMessage, ChatSession, EntryRole, ExperienceCandidate,
    ExperienceCandidatePayload, ExperienceCandidateSubmission, ExperienceKindHint, ExperienceStore,
    FrontendKind, OutputContent, PendingExperienceHooks, SessionSummary, ShellExecResult,
    ShellSessionResult, ShortTermMemory, SubTaskBatchCreatedMessage, SubTaskBatchState,
    SubTaskConfig, SubTaskDefinition, Task, TaskId, TaskStatus, ToolAction, ToolCallingState,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolReturnedHookPending,
    WaitingForTasksInfo, WaitingReason,
};
use crate::triggers::{
    DynamicScheduledTask, ScheduleSpec, ScheduleTaskCommitPending, ScheduleTaskRequestMessage,
    ScheduledTaskInfo, ScheduledTaskRegistry, update_scheduler_state,
};
use chrono::{DateTime, Local, Utc};

/// 清除任务上正在等待的工具确认 ID。
pub fn clear_task_pending_confirmation_id(tasks: &mut Query<(Entity, &mut Task)>, task_id: TaskId) {
    if let Some((_, mut task)) = tasks.iter_mut().find(|(_, t)| t.id == task_id) {
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

        commands.spawn((child_task, sub_task_config, ShortTermMemory::default()));

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
pub fn collect_task_results(task_ids: &[TaskId], tasks: &Query<&Task>) -> Vec<TaskWaitResult> {
    task_ids
        .iter()
        .map(|id| {
            let task = tasks.iter().find(|t| t.id == *id);
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
) {
    match action {
        Ok(ToolAction::Direct(value)) => {
            let execution_result = AgentExecutionResult {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_kind: request.request.request_kind.clone(),
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text("tool executed".to_string()),
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
                    tool_name: request.tool_name.clone(),
                    tool_output: Ok(value),
                    tool_call_id: request.tool_call_id.clone(),
                    processed: false,
                    original_tool_output: None,
                },
                ToolReturnedHookPending,
            ));

            commands.entity(request_entity).despawn();
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
        Ok(ToolAction::ExecSession(session_request)) => {
            match backend.exec_blocking(session_request) {
                Ok(handle) => {
                    spawn_shell_result(
                        commands,
                        request_entity,
                        request,
                        "shell_exec",
                        serde_json::json!(ShellExecResult::from_handle(&handle)),
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
                    child_agent_name,
                    parent_tool_call_id: parent_tool_call_id.clone(),
                    current_batch_id: new_batch_id,
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
        Ok(ToolAction::ScheduleTask {
            id,
            kind,
            content,
            schedule,
            output_channel,
        }) => {
            // 计算 next_trigger 用于工具返回（一次性任务返回其触发时间，
            // cron 任务返回下一次本地时区触发时间，无法计算时为 null）
            let next_trigger = compute_next_trigger(&schedule);

            // 投递 ScheduleTaskRequestMessage + ScheduleTaskCommitPending 标记，
            // 由 schedule_task_commit_system 提交到 SchedulerState 与 ScheduledTaskRegistry。
            commands.spawn((
                ScheduleTaskRequestMessage {
                    id,
                    kind: kind.clone(),
                    content,
                    schedule,
                    output_channel,
                },
                ScheduleTaskCommitPending,
            ));

            let output = serde_json::json!({
                "status": "scheduled",
                "schedule_id": id.to_string(),
                "kind": kind,
                "next_trigger": next_trigger.map(|t| t.to_rfc3339()),
            });

            let execution_result = AgentExecutionResult {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                request_kind: request.request.request_kind.clone(),
                result: Ok(AgentExecutionOutput {
                    content: OutputContent::Text(format!("task scheduled: {}", id)),
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
                    tool_name: "schedule_task".to_string(),
                    tool_output: Ok(output),
                    tool_call_id: request.tool_call_id.clone(),
                    processed: false,
                    original_tool_output: None,
                },
                ToolReturnedHookPending,
            ));

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SubmitProfileUpdate {
            name,
            tags,
            description,
        }) => {
            // 从 ExperienceStore 读取 kind 并重置 exception_count（LLM 成功调用工具，异常计数归 0）
            let kind = if let Some(ctx) = experience_store
                .profile_generation_context
                .get_mut(&request.request.task_id)
            {
                ctx.exception_count = 0;
                ctx.kind.clone()
            } else {
                crate::domain::ProfileGenerationKind::Incubation
            };

            // spawn ProfileGenerationCompletedMessage 供 profile_generation_completion_system 消费
            commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                generated_profile: Some(crate::domain::GeneratedProfile {
                    name: name.clone(),
                    tags: tags.clone(),
                    description: description.clone(),
                }),
                kind: kind.clone(),
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

            debug!(
                event = "ProfileUpdateSubmitted",
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                kind = ?kind,
                "profile update submitted by LLM"
            );

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SkipProfileUpdate) => {
            // 从 ExperienceStore 读取 kind 并重置 exception_count（LLM 成功调用工具，异常计数归 0）
            let kind = if let Some(ctx) = experience_store
                .profile_generation_context
                .get_mut(&request.request.task_id)
            {
                ctx.exception_count = 0;
                ctx.kind.clone()
            } else {
                crate::domain::ProfileGenerationKind::Update
            };

            // spawn ProfileGenerationCompletedMessage（None 表示 skip）
            commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                generated_profile: None,
                kind: kind.clone(),
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

            debug!(
                event = "ProfileUpdateSkipped",
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                kind = ?kind,
                "profile update skipped by LLM"
            );

            commands.entity(request_entity).despawn();
        }
        Ok(ToolAction::SubmitSkillUpdate { .. }) => {
            // TODO(skill-update): 在 skill_update_completion_system 实现后，
            // 此处应 spawn SkillUpdateCompletedMessage 并返回成功结果给 LLM。
            // 当前 skill_update_completion_system 尚未实现，先返回执行错误。
            warn!(
                event = "SkillUpdateOrchestratorNotImplemented",
                task_id = %request.request.task_id,
                agent_id = %request.request.agent_id,
                "submit_skill_update orchestrator handling not yet implemented"
            );
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::ExecutionFailed(
                    "skill_update_completion_system not yet implemented".to_string(),
                ),
            );
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

/// 恢复 Task 状态（从 Waiting 恢复到 Ready 或 Waiting(ToolExecution)）
pub fn restore_task_after_tool(
    tasks: &mut Query<(Entity, &mut Task)>,
    calling_states: &Query<&ToolCallingState>,
    task_id: TaskId,
) {
    if let Some((_, mut task)) = tasks.iter_mut().find(|(_, t)| t.id == task_id) {
        if !matches!(task.status, TaskStatus::Waiting(_)) {
            return;
        }
        let has_calling_state = calling_states.iter().any(|cs| cs.task_id == task.id);
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

/// 计算 `ScheduleSpec` 的下一次触发时间（UTC）。
///
/// - `Once(at)` 直接返回 `Some(at)`
/// - `Cron(schedule)` 通过 `Local` 时区计算下一次触发，再转换为 UTC；
///   若 cron 无下一次触发（理论上不会发生，因为 cron 表达式永远匹配未来某个时刻），
///   则返回 `None`
fn compute_next_trigger(schedule: &ScheduleSpec) -> Option<DateTime<Utc>> {
    match schedule {
        ScheduleSpec::Once(at) => Some(*at),
        ScheduleSpec::Cron(schedule) => schedule
            .upcoming(Local)
            .next()
            .map(|t| t.with_timezone(&Utc)),
    }
}

/// 提交 `ScheduleTaskRequestMessage` 到 `SchedulerState` 与 `ScheduledTaskRegistry`。
///
/// 独占系统：使用 `&mut World` 直接调用 `update_scheduler_state`，确保
/// `SchedulerState` 与 `SchedulerStateWatcher` 原子同步。
///
/// 处理步骤（每条待提交消息）：
/// 1. 通过 `update_scheduler_state` 向 `SchedulerState.dynamic_tasks` 追加
///    `DynamicScheduledTask`，同时通过 watch 通道通知 timer scheduler。
/// 2. 向 `ScheduledTaskRegistry` 插入 `ScheduledTaskInfo`，`is_once` 由
///    `matches!(msg.schedule, ScheduleSpec::Once(_))` 推导。
/// 3. despawn 消息实体。
///
/// 资源缺失时不会 panic：
/// - `SchedulerStateWatcher` 由 `update_scheduler_state` 内部用 `get_resource` 处理
/// - `ScheduledTaskRegistry` 用 `get_resource_mut` 防御性查询，缺失时跳过插入
pub fn schedule_task_commit_system(world: &mut World) {
    // 先收集所有待提交请求，避免在持有 world 不可变借用时调用 update_scheduler_state
    let mut to_commit: Vec<(Entity, ScheduleTaskRequestMessage)> = Vec::new();
    {
        let mut query = world.query_filtered::<
            (Entity, &ScheduleTaskRequestMessage),
            With<ScheduleTaskCommitPending>,
        >();
        for (entity, msg) in query.iter(world) {
            to_commit.push((entity, msg.clone()));
        }
    }

    for (entity, msg) in to_commit {
        let is_once = matches!(msg.schedule, ScheduleSpec::Once(_));

        // 1. 提交到 SchedulerState.dynamic_tasks（并通知 timer scheduler）
        update_scheduler_state(world, |state| {
            state.dynamic_tasks_mut().push(DynamicScheduledTask {
                id: msg.id,
                kind: msg.kind.clone(),
                schedule: msg.schedule.clone(),
                created_at: Utc::now(),
            });
        });

        // 2. 提交到 ScheduledTaskRegistry（防御性查询，缺失时跳过）
        if let Some(mut registry) = world.get_resource_mut::<ScheduledTaskRegistry>() {
            registry.insert(
                msg.kind.clone(),
                ScheduledTaskInfo {
                    content: msg.content.clone(),
                    output_channel: msg.output_channel.clone(),
                    is_once,
                },
            );
        }

        // 3. despawn 消息实体
        world.entity_mut(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AgentRequestKind;
    use crate::triggers::SchedulerState;
    use chrono::Timelike;
    use std::str::FromStr;

    /// 测试系统：从世界中的父 Task 读取 origin_channel，调用 spawn_create_tasks_messages。
    ///
    /// 通过系统而非直接调用 `world.commands()`，确保 `app.update()` 能正确刷新 Commands。
    fn spawn_subtasks_for_inheritance_test(mut commands: Commands, tasks: Query<&Task>) {
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

    /// 构造一次性 `ScheduleTaskRequestMessage` + `ScheduleTaskCommitPending` entity。
    fn spawn_once_request(world: &mut World, kind: &str) -> Entity {
        world
            .spawn((
                ScheduleTaskRequestMessage {
                    id: Uuid::new_v4(),
                    kind: kind.to_string(),
                    content: format!("content for {}", kind),
                    schedule: ScheduleSpec::Once(Utc::now() + chrono::Duration::days(1)),
                    output_channel: Some(ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "tester".to_string(),
                        thread_id: None,
                    }),
                },
                ScheduleTaskCommitPending,
            ))
            .id()
    }

    /// 构造 cron `ScheduleTaskRequestMessage` + `ScheduleTaskCommitPending` entity。
    fn spawn_cron_request(world: &mut World, kind: &str) -> Entity {
        let schedule = cron::Schedule::from_str("0 0 9 * * * *").unwrap();
        world
            .spawn((
                ScheduleTaskRequestMessage {
                    id: Uuid::new_v4(),
                    kind: kind.to_string(),
                    content: format!("cron content for {}", kind),
                    schedule: ScheduleSpec::Cron(Box::new(schedule)),
                    output_channel: None,
                },
                ScheduleTaskCommitPending,
            ))
            .id()
    }

    /// 注入调度相关的 Resource：`SchedulerState`、`SchedulerStateWatcher`、`ScheduledTaskRegistry`。
    fn insert_scheduler_resources(world: &mut World) {
        world.insert_resource(SchedulerState::default());
        world.insert_resource(ScheduledTaskRegistry::default());
    }

    /// `schedule_task_commit_system` 处理 Once 任务后：
    /// - `SchedulerState.dynamic_tasks` 新增一条
    /// - `ScheduledTaskRegistry` 含对应 `kind` 的 `ScheduledTaskInfo`，`is_once = true`
    /// - 消息实体被 despawn
    #[test]
    fn schedule_task_commit_system_commits_once_task() {
        let mut world = World::new();
        insert_scheduler_resources(&mut world);
        let entity = spawn_once_request(&mut world, "scheduled:once-1");

        schedule_task_commit_system(&mut world);

        let state = world.resource::<SchedulerState>();
        assert_eq!(state.dynamic_tasks().len(), 1);
        assert_eq!(state.dynamic_tasks()[0].kind, "scheduled:once-1");
        assert!(
            matches!(state.dynamic_tasks()[0].schedule, ScheduleSpec::Once(_)),
            "dynamic task schedule should preserve Once variant"
        );

        let registry = world.resource::<ScheduledTaskRegistry>();
        let info = registry
            .get("scheduled:once-1")
            .expect("Once task must be inserted into registry");
        assert_eq!(info.content, "content for scheduled:once-1");
        assert_eq!(info.output_channel.as_ref().unwrap().user_id, "tester");
        assert!(info.is_once, "is_once must be true for Once schedule");

        assert!(
            world.get_entity(entity).is_err(),
            "message entity must be despawn after commit"
        );
    }

    /// `schedule_task_commit_system` 处理 Cron 任务后：
    /// - `SchedulerState.dynamic_tasks` 新增一条
    /// - `ScheduledTaskRegistry` 含对应 `kind` 的 `ScheduledTaskInfo`，`is_once = false`
    #[test]
    fn schedule_task_commit_system_commits_cron_task() {
        let mut world = World::new();
        insert_scheduler_resources(&mut world);
        let entity = spawn_cron_request(&mut world, "scheduled:cron-1");

        schedule_task_commit_system(&mut world);

        let state = world.resource::<SchedulerState>();
        assert_eq!(state.dynamic_tasks().len(), 1);
        assert_eq!(state.dynamic_tasks()[0].kind, "scheduled:cron-1");
        assert!(
            matches!(state.dynamic_tasks()[0].schedule, ScheduleSpec::Cron(_)),
            "dynamic task schedule should preserve Cron variant"
        );

        let registry = world.resource::<ScheduledTaskRegistry>();
        let info = registry
            .get("scheduled:cron-1")
            .expect("Cron task must be inserted into registry");
        assert_eq!(info.content, "cron content for scheduled:cron-1");
        assert!(!info.is_once, "is_once must be false for Cron schedule");
        assert!(
            info.output_channel.is_none(),
            "output_channel should be None when message carried None"
        );

        assert!(
            world.get_entity(entity).is_err(),
            "message entity must be despawn after commit"
        );
    }

    /// `schedule_task_commit_system` 通过 `update_scheduler_state` 通知
    /// `SchedulerStateWatcher`，使 timer scheduler 能感知新动态任务。
    #[test]
    fn schedule_task_commit_system_notifies_watcher() {
        use tokio::sync::watch;

        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        world.insert_resource(ScheduledTaskRegistry::default());
        let (tx, mut rx) = watch::channel(SchedulerState::default());
        world.insert_resource(crate::triggers::SchedulerStateWatcher(Some(tx)));

        let _entity = spawn_once_request(&mut world, "scheduled:watch-1");

        schedule_task_commit_system(&mut world);

        assert!(
            rx.has_changed().unwrap(),
            "watcher must be notified after commit"
        );
        let state = rx.borrow_and_update();
        assert_eq!(state.dynamic_tasks().len(), 1);
        assert_eq!(state.dynamic_tasks()[0].kind, "scheduled:watch-1");
    }

    /// `schedule_task_commit_system` 在 `SchedulerStateWatcher` 缺失时不应 panic：
    /// `update_scheduler_state` 内部用 `get_resource` 处理 watcher。
    #[test]
    fn schedule_task_commit_system_does_not_panic_without_watcher() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        world.insert_resource(ScheduledTaskRegistry::default());
        // 故意不插入 SchedulerStateWatcher

        let _entity = spawn_once_request(&mut world, "scheduled:no-watcher");

        schedule_task_commit_system(&mut world);

        let state = world.resource::<SchedulerState>();
        assert_eq!(state.dynamic_tasks().len(), 1);
        assert!(
            world
                .resource::<ScheduledTaskRegistry>()
                .get("scheduled:no-watcher")
                .is_some(),
            "registry should still be updated without watcher"
        );
    }

    /// `schedule_task_commit_system` 在 `ScheduledTaskRegistry` 缺失时不应 panic，
    /// 但 `SchedulerState.dynamic_tasks` 仍应更新（保证 timer scheduler 一致）。
    #[test]
    fn schedule_task_commit_system_does_not_panic_without_registry() {
        let mut world = World::new();
        world.insert_resource(SchedulerState::default());
        // 故意不插入 ScheduledTaskRegistry

        let _entity = spawn_once_request(&mut world, "scheduled:no-registry");

        schedule_task_commit_system(&mut world);

        let state = world.resource::<SchedulerState>();
        assert_eq!(
            state.dynamic_tasks().len(),
            1,
            "SchedulerState should be updated even when registry is missing"
        );
    }

    /// `schedule_task_commit_system` 一次运行处理多条待提交消息，
    /// 顺序追加到 `SchedulerState.dynamic_tasks`，registry 含所有 kind。
    #[test]
    fn schedule_task_commit_system_handles_multiple_messages() {
        let mut world = World::new();
        insert_scheduler_resources(&mut world);
        let _e1 = spawn_once_request(&mut world, "scheduled:multi-1");
        let _e2 = spawn_cron_request(&mut world, "scheduled:multi-2");
        let _e3 = spawn_once_request(&mut world, "scheduled:multi-3");

        schedule_task_commit_system(&mut world);

        let state = world.resource::<SchedulerState>();
        assert_eq!(state.dynamic_tasks().len(), 3);
        assert_eq!(state.dynamic_tasks()[0].kind, "scheduled:multi-1");
        assert_eq!(state.dynamic_tasks()[1].kind, "scheduled:multi-2");
        assert_eq!(state.dynamic_tasks()[2].kind, "scheduled:multi-3");

        let registry = world.resource::<ScheduledTaskRegistry>();
        assert!(registry.get("scheduled:multi-1").is_some());
        assert!(registry.get("scheduled:multi-2").is_some());
        assert!(registry.get("scheduled:multi-3").is_some());
    }

    /// `schedule_task_commit_system` 不处理无 `ScheduleTaskCommitPending` 标记的
    /// `ScheduleTaskRequestMessage`，避免误提交。
    #[test]
    fn schedule_task_commit_system_ignores_messages_without_pending_marker() {
        let mut world = World::new();
        insert_scheduler_resources(&mut world);

        // spawn 一条不带 ScheduleTaskCommitPending 标记的 message
        let _untouched = world
            .spawn(ScheduleTaskRequestMessage {
                id: Uuid::new_v4(),
                kind: "scheduled:untouched".to_string(),
                content: "should not be committed".to_string(),
                schedule: ScheduleSpec::Once(Utc::now() + chrono::Duration::days(1)),
                output_channel: None,
            })
            .id();

        schedule_task_commit_system(&mut world);

        let state = world.resource::<SchedulerState>();
        assert_eq!(
            state.dynamic_tasks().len(),
            0,
            "non-pending message must not be committed"
        );
        let registry = world.resource::<ScheduledTaskRegistry>();
        assert!(
            registry.get("scheduled:untouched").is_none(),
            "non-pending message must not be inserted into registry"
        );
    }

    /// `compute_next_trigger` 对 `Once(at)` 直接返回 `Some(at)`。
    #[test]
    fn compute_next_trigger_for_once_returns_some_at() {
        let at = Utc::now() + chrono::Duration::days(7);
        let schedule = ScheduleSpec::Once(at);
        let next = compute_next_trigger(&schedule);
        assert_eq!(next, Some(at));
    }

    /// `compute_next_trigger` 对 `Cron(schedule)` 返回下一次本地时区触发时间（转 UTC）。
    /// 工作日 9:00 cron 至少存在一个未来触发点。
    #[test]
    fn compute_next_trigger_for_cron_returns_next_upcoming() {
        let cron_schedule = cron::Schedule::from_str("0 0 9 * * * *").unwrap();
        let schedule = ScheduleSpec::Cron(Box::new(cron_schedule));
        let next = compute_next_trigger(&schedule).expect("cron must have a next trigger");
        // 转回 Local 验证小时为 9
        let local_next = next.with_timezone(&Local);
        assert_eq!(local_next.hour(), 9, "next trigger should be at local 9:00");
    }
}
