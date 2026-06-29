//! Tool 执行协调器
//!
//! 处理 Tool 执行动作和消息生成。

use bevy::prelude::*;
use serde::Serialize;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::contracts::SessionBackend;
use crate::domain::{
    AgentExecutionOutput, AgentExecutionResult, AgentId, BatchTaskState, ChannelId,
    ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateSubmission,
    ExperienceKindHint, ExperienceStore, FrontendKind, OutputContent, PendingExperienceHooks,
    SessionSummary, ShellExecResult, ShellSessionResult, ShortTermMemory,
    SubTaskBatchCreatedMessage, SubTaskBatchState, SubTaskConfig, SubTaskDefinition, Task, TaskId,
    TaskStatus, ToolAction, ToolCallingState, ToolError, ToolExecutionRequestMessage,
    ToolExecutionResultMessage, ToolReturnedHookPending, WaitingForTasksInfo, WaitingReason,
};

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
    parent_origin_channel: ChannelId,
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
    backend: &B,
    experience_store: &mut ExperienceStore,
    pending_experience_hooks: &mut PendingExperienceHooks,
    parent_agent_id: Option<AgentId>,
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
                    ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                        thread_id: None,
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AgentRequestKind;

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
            origin_channel: telegram_channel.clone(),
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
            child_tasks[0].origin_channel, telegram_channel,
            "subtask should inherit parent's Telegram channel, not Tui/default"
        );
        // 显式断言：不得回退到硬编码的 Tui/default
        assert_ne!(
            child_tasks[0].origin_channel,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
            "subtask channel must NOT be the hardcoded Tui/default"
        );
    }
}
