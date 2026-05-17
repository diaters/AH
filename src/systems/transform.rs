use bevy::prelude::*;

use crate::{
    app::{Clock, ExecutionResultReceiver, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutionResultMessage,
        AgentRequestKind, BrainDecisionError, CreateTaskMessage, EntryMetadata, EntryRole,
        FailureReason, RetryReadyMessage, ShortTermMemory, Signal, SignalPayload, Task, TaskStatus,
        TaskTerminatedMessage, UserInputMessage, UserOutputMessage, WaitingReason,
    },
    llm::parse_brain_decision,
};

pub(crate) fn signal_ingest_system(mut commands: Commands, signals: Query<(Entity, &Signal)>) {
    for (entity, signal) in &signals {
        match &signal.payload {
            SignalPayload::UserInput(content) => {
                commands.spawn(UserInputMessage {
                    content: content.clone(),
                });
            }
            SignalPayload::RetryWakeup(task_id) => {
                commands.spawn(RetryReadyMessage { task_id: *task_id });
            }
            SignalPayload::SystemWakeup => {}
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
        // 外部输入创建单轮任务（Ready 状态）
        commands.spawn(Task::from_user_input_ready(
            message.content.clone(),
            settings.0.max_retries,
        ));
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
            continue;
        }

        let result = &result_message.result;

        for (mut task, short_term) in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            match &result.result {
                Ok(content) => {
                    // 追加 Agent 响应到 ShortTermMemory
                    if let Some(mut stm) = short_term {
                        stm.add_entry(EntryRole::Assistant, content, EntryMetadata::default());
                    }

                    // 检查是否支持多轮对话
                    if task.multi_turn {
                        // 多轮对话：响应后进入 Waiting(User)
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                        task.input_summary = content.clone();
                        task.updated_at = clock.0;
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    } else {
                        // 单轮对话：标记完成
                        task.mark_done(content.clone(), clock.0);
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    }
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    task.schedule_retry(error, clock.0);
                }
                Err(error) => {
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
                task.mark_ready_for_retry(clock.0);
                break;
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn task_termination_system(mut commands: Commands, tasks: Query<&Task, Changed<Task>>) {
    for task in &tasks {
        if task.status.is_terminal() {
            commands.spawn(TaskTerminatedMessage { task_id: task.id });
        }
    }
}

fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}
