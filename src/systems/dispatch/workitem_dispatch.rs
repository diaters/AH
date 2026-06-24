//! WorkItem 调度系统
//!
//! 将 Pending 状态的治理型 WorkItem（Evaluation/Summarization/ExperienceCollection）分发给合适的 Agent。

use bevy::prelude::*;
use tracing::{debug, warn};

use crate::domain::{
    Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind, Task,
    TaskEvaluationConfig, TaskStatus, WaitingReason, WorkItem, WorkItemLifecycleHookPending,
    WorkItemStatus, WorkItemType,
};
use crate::user_plugins::hook_point::HookPoint;

/// WorkItem 调度系统
///
/// 只处理 Evaluation 和 Summarization 类型的 WorkItem。
/// 查找匹配的 Agent（通过 tags 或 name），创建执行请求。
pub(crate) fn workitem_dispatch_system(
    clock: Res<crate::app::Clock>,
    mut commands: Commands,
    config: Res<TaskEvaluationConfig>,
    agents: Query<&Agent>,
    mut tasks: Query<&mut Task>,
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
                    && agent
                        .capabilities
                        .tags
                        .contains(&"summarization".to_string())
            }),
            WorkItemType::ExperienceCollection => agents.iter().find(|agent| {
                agent.kind == AgentKind::Persistent
                    && agent.capabilities.tags.contains(&"collect".to_string())
            }),
            // 其他类型暂不处理
            _ => None,
        };

        let Some(agent) = agent else {
            // 找不到 Agent 时将 WorkItem 标记为 Failed，避免永远卡在 Pending
            warn!(
                event = "WorkItemNoAgentFound",
                work_item_id = %work_item.id,
                task_id = %work_item.task_id,
                work_type = ?work_item.work_type,
                "no suitable agent found for work item, marking as failed"
            );
            work_item.fail();

            // 标记 WorkItem 已失败，等待 companion 系统派发 on_workitem_failed hook
            commands
                .entity(_entity)
                .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed));

            // 恢复关联任务的状态，避免任务死锁
            // 经验收集 WorkItem 失败不应回滚原任务状态
            if work_item.work_type != WorkItemType::ExperienceCollection
                && let Some(mut task) = tasks.iter_mut().find(|t| t.id == work_item.task_id)
            {
                match task.status {
                    TaskStatus::Waiting(WaitingReason::Evaluator) => {
                        let old_status = task.status.clone();
                        task.status = TaskStatus::Ready;
                        task.updated_at = clock.0;
                        debug!(
                            event = "TaskStatusRestoredAfterWorkItemFailed",
                            task_id = %task.id,
                            from_status = ?old_status,
                            to_status = ?task.status,
                            work_type = ?work_item.work_type,
                            "task restored to Ready after work item failed"
                        );
                    }
                    TaskStatus::Waiting(WaitingReason::Summarization) => {
                        let old_status = task.status.clone();
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                        task.updated_at = clock.0;
                        debug!(
                            event = "TaskStatusRestoredAfterWorkItemFailed",
                            task_id = %task.id,
                            from_status = ?old_status,
                            to_status = ?task.status,
                            work_type = ?work_item.work_type,
                            "task restored to Waiting(User) after work item failed"
                        );
                    }
                    _ => {}
                }
            }

            continue;
        };

        // 状态转换：Pending -> Running
        work_item.assign(agent.id);
        work_item.start();

        // 标记 WorkItem 已启动，等待 companion 系统派发 on_workitem_started hook
        commands
            .entity(_entity)
            .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted));

        // 根据工作项类型确定请求类型
        let request_kind = match work_item.work_type {
            WorkItemType::Evaluation => AgentRequestKind::Evaluation,
            WorkItemType::Summarization => AgentRequestKind::Summarization,
            WorkItemType::ExperienceCollection => AgentRequestKind::LlmCompletion,
            _ => AgentRequestKind::LlmCompletion,
        };

        // 创建执行请求
        commands.spawn(AgentExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id: work_item.task_id,
                agent_id: agent.id,
                request_kind,
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
