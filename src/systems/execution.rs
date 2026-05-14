use std::sync::Arc;

use bevy::prelude::*;

use crate::{
    app::{AsyncRuntime, Clock, ExecutionResultSender, ExecutorHandle},
    domain::{AgentExecutionRequestMessage, AgentExecutionResult, AgentRequestKind, Task},
};

/// 消费执行请求并把任务提交给异步运行时。
pub(crate) fn agent_execution_system(
    clock: Res<Clock>,
    runtime: Res<AsyncRuntime>,
    executor: Res<ExecutorHandle>,
    result_sender: Res<ExecutionResultSender>,
    mut commands: Commands,
    requests: Query<(Entity, &AgentExecutionRequestMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, message) in &requests {
        let request = message.request.clone();
        let executor = Arc::clone(&executor.0);
        let sender = result_sender.0.clone();

        for mut task in &mut tasks {
            if task.id == request.task_id {
                if request.request_kind != AgentRequestKind::BrainDecision {
                    task.mark_running(clock.0);
                }
                break;
            }
        }

        runtime.0.spawn(async move {
            let result = executor.execute(request.clone()).await;
            let _ = sender.send(AgentExecutionResult {
                task_id: request.task_id,
                agent_id: request.agent_id,
                request_kind: request.request_kind,
                result,
            });
        });

        commands.entity(entity).despawn();
    }
}
