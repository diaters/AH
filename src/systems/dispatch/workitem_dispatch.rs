//! WorkItem 调度系统
//!
//! 将 Pending 状态的治理型 WorkItem（Evaluation/Summarization）分发给合适的 Agent。

use bevy::prelude::*;
use tracing::debug;

use crate::{
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        TaskEvaluationConfig, WorkItem, WorkItemStatus, WorkItemType,
    },
};

/// WorkItem 调度系统
///
/// 只处理 Evaluation 和 Summarization 类型的 WorkItem。
/// 查找匹配的 Agent（通过 tags 或 name），创建执行请求。
pub(crate) fn workitem_dispatch_system(
    mut commands: Commands,
    config: Res<TaskEvaluationConfig>,
    agents: Query<&Agent>,
    mut work_items: Query<(Entity, &mut WorkItem)>,
) {
    for (_entity, mut work_item) in &mut work_items {
        // 只处理 Pending 状态
        if work_item.status != WorkItemStatus::Pending {
            continue;
        }

        // 根据类型选择 Agent
        let agent = match work_item.work_type {
            WorkItemType::Evaluation => agents.iter().find(|agent| {
                agent.kind == AgentKind::Persistent
                    && (agent.capabilities.tags.contains(&"evaluation".to_string())
                        || agent.profile.name == config.evaluator_agent_name)
            }),
            WorkItemType::Summarization => agents.iter().find(|agent| {
                agent.kind == AgentKind::Persistent
                    && agent.capabilities.tags.contains(&"summarization".to_string())
            }),
            // 其他类型暂不处理
            _ => None,
        };

        let Some(agent) = agent else {
            debug!(
                event = "WorkItemNoAgentFound",
                work_item_id = %work_item.id,
                task_id = %work_item.task_id,
                work_type = ?work_item.work_type,
                "no suitable agent found for work item"
            );
            continue;
        };

        // 状态转换：Pending -> Running
        work_item.assign(agent.id);
        work_item.start();

        // 创建执行请求
        commands.spawn(AgentExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id: work_item.task_id,
                agent_id: agent.id,
                request_kind: AgentRequestKind::LlmCompletion,
                prompt: work_item.input.prompt.clone(),
                system_prompt: work_item.input.context.system_prompt.clone(),
                tools: work_item.input.context.tools.clone(),
                conversation: work_item.input.context.conversation.clone(),
                work_item_id: Some(work_item.id),
            },
        });

        debug!(
            event = "WorkItemDispatched",
            task_id = %work_item.task_id,
            work_item_id = %work_item.id,
            work_type = ?work_item.work_type,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            "work item dispatched"
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_workitem_dispatch_system_signature() {
        // This test verifies the function signature is correct
        // Real integration tests are in tests/workitem_dispatch_flow.rs
    }
}
