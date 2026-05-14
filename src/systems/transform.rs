use bevy::prelude::*;

use crate::{
    app::{Clock, ExecutionResultReceiver, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutionResultMessage,
        AgentRequestKind, AgentStatus, BrainDecisionError, FailureReason, RetryReadyMessage,
        Signal, SignalPayload, Task, TaskStatus, UserInputMessage, UserOutputMessage,
    },
    llm::parse_brain_decision,
};

/// 将轻量 Signal 转换为后续可消费的 Message。
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

/// 将用户输入消息沉淀为可持续演化的 Task。
pub(crate) fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    messages: Query<(Entity, &UserInputMessage)>,
) {
    for (entity, message) in &messages {
        commands.spawn(Task::from_user_input(
            message.content.clone(),
            settings.0.max_retries,
        ));
        commands.entity(entity).despawn();
    }
}

/// 将异步执行结果回注为 ECS 内的一次性 Message。
pub(crate) fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn(AgentExecutionResultMessage { result });
    }
}

/// 消费 Brain 决策的执行结果，解析结构化决策，产出具体 Agent 的执行请求。
pub(crate) fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    mut agents: Query<&mut Agent>,
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

        // 恢复 Brain Agent 状态
        for mut agent in &mut agents {
            if agent.id == result.agent_id {
                agent.status = AgentStatus::Idle;
                break;
            }
        }

        // 查找对应的 Task
        let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.task_id) else {
            commands.entity(entity).despawn();
            continue;
        };

        match &result.result {
            Ok(content) => {
                match parse_brain_decision(content) {
                    Ok(decision) => {
                        // 查找 Brain 选定的 Agent
                        let selected_agent = agents.iter_mut().find(|agent| {
                            agent.profile.name == decision.selected_agent_name
                                && agent.status == AgentStatus::Idle
                        });

                        let Some(mut selected_agent) = selected_agent else {
                            // 选定的 Agent 不存在或不可用，回退到默认 Agent
                            let fallback = agents.iter_mut().find(|agent| {
                                agent.profile.name != brain_config.agent_name
                                    && agent.status == AgentStatus::Idle
                            });

                            let Some(mut fallback) = fallback else {
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

                            fallback.status = AgentStatus::Busy;
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

                        selected_agent.status = AgentStatus::Busy;
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
                }
            }
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

/// 根据执行结果更新 Task，并在需要时生成输出消息或重试状态。
pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    mut agents: Query<&mut Agent>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    for (entity, result_message) in &results {
        // 只处理 LlmCompletion 类型的结果
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            continue;
        }

        let result = &result_message.result;

        for mut agent in &mut agents {
            if agent.id == result.agent_id {
                agent.status = crate::domain::AgentStatus::Idle;
                break;
            }
        }

        for mut task in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            match &result.result {
                Ok(content) => {
                    task.mark_done(content.clone(), clock.0);
                    commands.spawn(UserOutputMessage {
                        content: content.clone(),
                    });
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

/// 消费重试准备消息并把任务重新置回 Ready。
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

/// 从任务状态中提取失败原因，便于统一输出。
fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}
