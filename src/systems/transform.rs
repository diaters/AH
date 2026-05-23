use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::{Clock, ExecutionResultReceiver, HarnessSettings, MemoryConfig},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutionResultMessage,
        AgentRequestKind, BrainDecisionError, CreateTaskMessage, EntryMetadata, EntryRole,
        FailureReason, FinishTaskMessage, RetryReadyMessage, ShortTermMemory, Signal,
        SignalPayload, SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus,
        TaskTerminatedMessage, UserInputMessage, UserOutputMessage, WaitingReason,
    },
    llm::parse_brain_decision,
};

pub(crate) fn signal_ingest_system(mut commands: Commands, signals: Query<(Entity, &Signal)>) {
    for (entity, signal) in &signals {
        match &signal.payload {
            SignalPayload::UserInput(content) => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "UserInput",
                    content = %content,
                    content_len = content.len(),
                    "signal converted to UserInputMessage"
                );
                commands.spawn(UserInputMessage {
                    content: content.clone(),
                });
            }
            SignalPayload::RetryWakeup(task_id) => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "RetryWakeup",
                    task_id = %task_id,
                    "signal converted to RetryReadyMessage"
                );
                commands.spawn(RetryReadyMessage { task_id: *task_id });
            }
            SignalPayload::SystemWakeup => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "SystemWakeup",
                    "system wakeup signal received"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    messages: Query<(Entity, &CreateTaskMessage)>,
) {
    for (entity, message) in &messages {
        // 创建多轮对话任务（Pending 状态）并附带 ShortTermMemory
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, &message.content, EntryMetadata::default());
        let stm_tokens = stm.estimated_tokens;

        let task = Task::from_user_input(message.content.clone(), settings.0.max_retries);
        debug!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            content_len = message.content.len(),
            multi_turn = task.multi_turn,
            max_retries = task.max_retries,
            stm_initial_entries = 1,
            stm_initial_tokens = stm_tokens,
            "new task spawned from user message"
        );

        commands.spawn((task, stm));
        commands.entity(entity).despawn();
    }
}

pub(crate) fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn(AgentExecutionResultMessage { result });
    }
}

pub(crate) fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::BrainDecision {
            continue;
        }

        let result = &result_message.result;

        let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.task_id) else {
            commands.entity(entity).despawn();
            continue;
        };

        match &result.result {
            Ok(content) => match parse_brain_decision(content) {
                Ok(decision) => {
                    let selected_agent = agents.iter().find(|agent| {
                        agent.profile.name == decision.selected_agent_name
                            && agent.kind == crate::domain::AgentKind::Persistent
                    });

                    let Some(selected_agent) = selected_agent else {
                        let fallback = agents.iter().find(|agent| {
                            !agent.capabilities.tags.contains(&"brain".to_string())
                                && agent.kind == crate::domain::AgentKind::Persistent
                        });

                        let Some(fallback) = fallback else {
                            task.last_error = Some(format!(
                                "brain selected agent '{}' but no agent available",
                                decision.selected_agent_name
                            ));
                            task.status = TaskStatus::Failed(FailureReason::AgentError);
                            task.updated_at = clock.0;
                            commands.entity(entity).despawn();
                            continue;
                        };

                        let request = AgentExecutionRequest {
                            task_id: task.id,
                            agent_id: fallback.id,
                            request_kind: AgentRequestKind::LlmCompletion,
                            prompt: decision.delegate_prompt,
                            system_prompt: None,
                        };

                        task.delegate = Some(fallback.id);
                        task.status = TaskStatus::Waiting(crate::domain::WaitingReason::Agent);
                        task.updated_at = clock.0;
                        commands.spawn(AgentExecutionRequestMessage { request });
                        commands.entity(entity).despawn();
                        continue;
                    };

                    let request = AgentExecutionRequest {
                        task_id: task.id,
                        agent_id: selected_agent.id,
                        request_kind: AgentRequestKind::LlmCompletion,
                        prompt: decision.delegate_prompt,
                        system_prompt: None,
                    };

                    task.delegate = Some(selected_agent.id);
                    task.status = TaskStatus::Waiting(crate::domain::WaitingReason::Agent);
                    task.updated_at = clock.0;
                    commands.spawn(AgentExecutionRequestMessage { request });
                }
                Err(BrainDecisionError::ParseFailed(msg)) => {
                    task.last_error = Some(format!("brain decision parse failed: {msg}"));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                Err(BrainDecisionError::EmptyResponse) => {
                    task.last_error = Some("brain returned empty response".to_string());
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                Err(BrainDecisionError::UnknownAgent(name)) => {
                    task.last_error = Some(format!("brain selected unknown agent: {name}"));
                    task.status = TaskStatus::Failed(FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
            },
            Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                task.schedule_retry(error, clock.0);
            }
            Err(error) => {
                task.mark_failed(error, clock.0);
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&mut ShortTermMemory>)>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            // 对于 Summarization 和 BrainDecision 结果，由其他系统处理
            continue;
        }

        let result = &result_message.result;

        for (mut task, short_term) in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            debug!(
                event = "LlmResponseReceived",
                task_id = %task.id,
                agent_id = %result.agent_id,
                request_kind = ?result.request_kind,
                success = result.result.is_ok(),
                response_len = result.result.as_ref().ok().map(|c| c.len()),
                response_content = ?result.result.as_ref().ok(),
                multi_turn = task.multi_turn,
                "llm response received"
            );

            match &result.result {
                Ok(content) => {
                    // 追加 Agent 响应到 ShortTermMemory
                    let stm_len = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens_before =
                        short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });

                    if let Some(mut stm) = short_term {
                        stm.add_entry(EntryRole::Assistant, content, EntryMetadata::default());
                    }
                    let stm_tokens_after =
                        stm_tokens_before + crate::domain::estimate_tokens(content);

                    // 检查是否支持多轮对话
                    if task.multi_turn {
                        let old_status = task.status.clone();
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                        task.input_summary = content.clone();
                        task.updated_at = clock.0;
                        debug!(
                            event = "TaskStatusTransition",
                            task_id = %task.id,
                            from_status = ?old_status,
                            to_status = ?task.status,
                            reason = "multi_turn_response",
                            response_len = content.len(),
                            response_content = %content,
                            stm_entries = stm_len + 1,
                            stm_tokens_before = stm_tokens_before,
                            stm_tokens_after = stm_tokens_after,
                            stm_recent = ?stm_recent,
                            "multi_turn: task now waiting for user"
                        );
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    } else {
                        debug!(
                            event = "TaskStatusTransition",
                            task_id = %task.id,
                            from_status = ?task.status,
                            to_status = ?TaskStatus::Done,
                            reason = "single_turn_complete",
                            response_len = content.len(),
                            response_content = %content,
                            "single_turn: marking task Done"
                        );
                        task.mark_done(content.clone(), clock.0);
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    }
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    let stm_entries = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });
                    debug!(
                        event = "TaskRetryScheduled",
                        task_id = %task.id,
                        retry_count = task.retry_count,
                        max_retries = task.max_retries,
                        error = %error.message(),
                        error_type = std::any::type_name_of_val(error),
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        stm_recent = ?stm_recent,
                        "scheduling retry for task"
                    );
                    task.schedule_retry(error, clock.0);
                }
                Err(error) => {
                    let stm_entries = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
                    let stm_tokens = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);
                    let stm_recent: Option<Vec<_>> = short_term.as_ref().map(|s| {
                        s.entries
                            .iter()
                            .rev()
                            .take(3)
                            .map(|e| (e.role, e.content.clone()))
                            .collect()
                    });
                    debug!(
                        event = "TaskFailed",
                        task_id = %task.id,
                        error = %error.message(),
                        error_type = std::any::type_name_of_val(error),
                        retry_count = task.retry_count,
                        max_retries = task.max_retries,
                        stm_entries = stm_entries,
                        stm_tokens = stm_tokens,
                        stm_recent = ?stm_recent,
                        "task failed with non-retryable error"
                    );
                    task.mark_failed(error, clock.0);
                    commands.spawn(UserOutputMessage {
                        content: format!(
                            "任务执行失败（{:?}）：{}",
                            task_status_failure_reason(&task).unwrap_or(FailureReason::Unknown),
                            error.message()
                        ),
                    });
                }
            }

            break;
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn retry_ready_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &RetryReadyMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, message) in &messages {
        for mut task in &mut tasks {
            if task.id == message.task_id {
                debug!(
                    event = "RetryReady",
                    task_id = %task.id,
                    retry_count = task.retry_count,
                    max_retries = task.max_retries,
                    last_error = ?task.last_error,
                    "marking task ready for retry"
                );
                task.mark_ready_for_retry(clock.0);
                break;
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn task_termination_system(
    mut commands: Commands,
    config: Res<MemoryConfig>,
    tasks: Query<(&Task, Option<&ShortTermMemory>), Changed<Task>>,
) {
    for (task, memory) in &tasks {
        if task.status.is_terminal() {
            debug!(
                event = "TaskTerminated",
                task_id = %task.id,
                task_status = ?task.status,
                task_content = %task.content,
                result_summary = %task.result_summary,
                has_stm = memory.is_some(),
                "task reached terminal state"
            );
            commands.spawn(TaskTerminatedMessage { task_id: task.id });

            // 任务完成时触发摘要
            if let Some(stm) = memory
                && !stm.entries.is_empty()
            {
                let content: String = stm
                    .entries
                    .iter()
                    .map(|e| format!("{:?}: {}", e.role, e.content))
                    .collect::<Vec<_>>()
                    .join("\n");

                debug!(
                    event = "SummarizationTriggered",
                    task_id = %task.id,
                    trigger = "TaskComplete",
                    stm_entries = stm.entries.len(),
                    stm_tokens = stm.estimated_tokens,
                    content_len = content.len(),
                    target_tokens = config.summary_target_tokens,
                    "triggering summarization on task completion"
                );
                commands.spawn(SummarizationRequestMessage {
                    task_id: task.id,
                    content_to_summarize: content,
                    target_tokens: config.summary_target_tokens,
                    trigger: SummarizationTrigger::TaskComplete,
                });
            }
        }
    }
}

fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}

/// 处理 /finish 命令，将任务标记为 Done。
pub(crate) fn finish_task_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &FinishTaskMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &messages {
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == msg.task_id) {
            debug!(
                event = "TaskFinished",
                task_id = %task.id,
                task_status = ?task.status,
                task_content = %task.content,
                "finishing task via /finish command"
            );
            task.mark_done("finished by user", clock.0);
        }
        commands.entity(entity).despawn();
    }
}
