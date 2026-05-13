use bevy::prelude::*;

use crate::{
    app::Clock,
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentRequestKind, AgentStatus,
        Task, TaskStatus,
    },
};

/// 将 Ready 任务转换为 Agent 执行请求。
pub(crate) fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    mut agents: Query<&mut Agent>,
) {
    for mut task in &mut tasks {
        if task.status != TaskStatus::Ready {
            continue;
        }

        let Some(mut agent) = agents.iter_mut().find(|agent| agent.status == AgentStatus::Idle) else {
            continue;
        };

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: agent.id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: task.content.clone(),
        };

        agent.status = AgentStatus::Busy;
        task.mark_waiting_for_agent(agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}
