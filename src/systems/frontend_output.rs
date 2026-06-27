use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::FrontendRegistry,
    domain::{
        Agent, AgentStatusKind, EngineEvent, EventTarget, MessageRole, SystemOutputMessage, Task,
        TaskStatusKind, ToolConfirmationRequestMessage, UserOutputMessage,
    },
};

/// 将 ECS 状态变化转为 EngineEvent 推送给所有前端
#[allow(clippy::too_many_arguments)]
pub(crate) fn frontend_output_system(
    registry: Res<FrontendRegistry>,
    mut commands: Commands,
    outputs: Query<(Entity, &UserOutputMessage)>,
    system_outputs: Query<(Entity, &SystemOutputMessage)>,
    all_tasks: Query<&Task>,
    tasks: Query<&Task, Changed<Task>>,
    agents: Query<&Agent, Changed<Agent>>,
    confirmations: Query<
        (Entity, &ToolConfirmationRequestMessage),
        Added<ToolConfirmationRequestMessage>,
    >,
) {
    // 用户可见文本输出
    for (entity, output) in &outputs {
        debug!(
            event = "FrontendOutputText",
            task_id = %output.task_id,
            content_len = output.content.len(),
            "pushing text to frontends"
        );
        let target = all_tasks
            .iter()
            .find(|t| t.id == output.task_id)
            .map(|t| EventTarget::Directed(vec![t.origin_channel.clone()]))
            .unwrap_or(EventTarget::Broadcast);
        let event = EngineEvent::Text {
            target,
            role: MessageRole::Agent,
            content: output.content.clone(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
        commands.entity(entity).despawn();
    }

    // 系统通知输出（不进入 STM，路由到 task 的 origin_channel）
    for (entity, output) in &system_outputs {
        // 查找关联的 task 以获取 origin_channel
        let target = all_tasks
            .iter()
            .find(|t| t.id == output.task_id)
            .map(|t| EventTarget::Directed(vec![t.origin_channel.clone()]))
            .unwrap_or(EventTarget::Broadcast);

        debug!(
            event = "FrontendSystemOutput",
            task_id = %output.task_id,
            content_len = output.content.len(),
            target = ?target,
            "pushing system notification to frontends"
        );
        let event = EngineEvent::Text {
            target,
            role: MessageRole::System,
            content: output.content.clone(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
        commands.entity(entity).despawn();
    }

    // Task 状态变化
    for task in &tasks {
        let target = EventTarget::Directed(vec![task.origin_channel.clone()]);
        let status = task_status_to_kind(&task.status);
        let result = if task.status.is_terminal() {
            Some(task.result_summary.clone())
        } else {
            None
        };
        let event = EngineEvent::TaskStatusChanged {
            target,
            task_id: task.id,
            name: task.input_summary.clone(),
            status,
            result,
            parent_id: task.parent_task_id,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }

    // Agent 状态变化
    for agent in &agents {
        let event = EngineEvent::AgentStatusChanged {
            target: EventTarget::Broadcast,
            agent_id: agent.id,
            name: agent.profile.name.clone(),
            status: AgentStatusKind::Idle,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }

    // 审批请求
    for (entity, confirmation) in &confirmations {
        let options: Vec<crate::domain::ApprovalOption> = confirmation
            .options
            .iter()
            .map(|opt| crate::domain::ApprovalOption {
                id: opt.id.clone(),
                label: opt.label.clone(),
                description: if opt.id == "deny" {
                    "拒绝".to_string()
                } else {
                    match opt.mode {
                        crate::domain::GrantMode::Once => "仅本次允许".to_string(),
                        crate::domain::GrantMode::Permanent => "永久允许此工具".to_string(),
                    }
                },
            })
            .collect();

        let event = EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id: confirmation.request_id,
            agent_name: String::new(),
            tool_name: confirmation.tool_name.clone(),
            tool_input: confirmation.tool_input.clone(),
            options,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }

        // 审批请求已推送给前端，清理 entity
        commands.entity(entity).despawn();
    }
}

fn task_status_to_kind(status: &crate::domain::TaskStatus) -> TaskStatusKind {
    match status {
        crate::domain::TaskStatus::Pending => TaskStatusKind::Pending,
        crate::domain::TaskStatus::Ready => TaskStatusKind::Pending,
        crate::domain::TaskStatus::Running => TaskStatusKind::Running,
        crate::domain::TaskStatus::Waiting(_) => TaskStatusKind::Waiting,
        crate::domain::TaskStatus::Done => TaskStatusKind::Done,
        crate::domain::TaskStatus::Failed(_) => TaskStatusKind::Failed,
    }
}
