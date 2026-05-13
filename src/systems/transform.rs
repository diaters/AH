use bevy::prelude::*;

use crate::{
    app::{Clock, ExecutionResultReceiver, HarnessSettings},
    domain::{
        Agent, AgentExecutionResultMessage, FailureReason, RetryReadyMessage, Signal,
        SignalPayload, Task, TaskStatus, UserInputMessage, UserOutputMessage,
    },
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

/// 根据执行结果更新 Task，并在需要时生成输出消息或重试状态。
pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    mut agents: Query<&mut Agent>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    for (entity, result_message) in &results {
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
