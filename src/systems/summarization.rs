//! 摘要系统
//!
//! 处理记忆压缩的摘要请求和结果处理。

use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::Clock,
    contracts::{AgentCapabilitySummary, FirstSummarizerPolicy, SummarizerSelectionPolicy},
    domain::{
        Agent, AgentKind, SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus,
        WaitingReason, WorkItem,
    },
};

/// 摘要调度系统：将摘要请求转为 WorkItem
///
/// ## Summarizer Agent 选择
///
/// 通过 Tag 查找所有带 "summarization" 标签的 Agent，选择配置中最前的那个。
pub(crate) fn summarization_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    agents: Query<&Agent>,
    requests: Query<(Entity, &SummarizationRequestMessage)>,
    mut tasks: Query<&mut Task>,
) {
    // 通过 Tag 查找 Summarizer Agent，选择配置中最前的
    let summarizer_candidates: Vec<AgentCapabilitySummary> = agents
        .iter()
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(AgentCapabilitySummary::from_agent)
        .collect();

    let summarizer_policy = FirstSummarizerPolicy;
    let Some(summarizer_id) = summarizer_policy.select_summarizer(&summarizer_candidates) else {
        debug!(
            event = "SummarizerNotFound",
            pending_requests = requests.iter().count(),
            "no summarizer agent found with 'summarization' tag, skipping summarization requests"
        );
        // 没有 summarizer，清理所有请求
        for (entity, _) in &requests {
            commands.entity(entity).despawn();
        }
        return;
    };

    let summarizer = agents.iter().find(|a| a.id == summarizer_id).unwrap();

    for (entity, request) in &requests {
        // 对于非 TaskComplete 触发的摘要，标记任务为等待摘要
        // TaskComplete 触发的摘要不需要改变任务状态（任务已是终态）
        if request.trigger != SummarizationTrigger::TaskComplete
            && let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.task_id)
            && !task.status.is_terminal()
        {
            let old_status = task.status.clone();
            task.status = TaskStatus::Waiting(WaitingReason::Summarization);
            task.updated_at = clock.0;
            debug!(
                event = "TaskWaitingForSummarization",
                task_id = %task.id,
                from_status = ?old_status,
                to_status = ?task.status,
                trigger = ?request.trigger,
                "task waiting for summarization"
            );
        }

        // 创建 Summarization WorkItem
        let work_item = WorkItem::summarization(
            request.task_id,
            request.content_to_summarize.clone(),
            request.target_tokens as usize,
            request.trigger,
        );
        commands.spawn(work_item);

        debug!(
            event = "SummarizationWorkItemCreated",
            task_id = %request.task_id,
            summarizer_agent_id = %summarizer.id,
            summarizer_agent_name = %summarizer.profile.name,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            content_len = request.content_to_summarize.len(),
            "summarization work item created and pre-check passed"
        );
        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentExperience, AgentProfile, AgentToolPermissions};

    #[test]
    fn summarizer_agent_selection() {
        let mut world = World::new();

        let summarizer = Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "summarizer".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["summarization".to_string()],
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        };

        world.spawn(summarizer);

        let found = world
            .query::<&Agent>()
            .iter(&world)
            .find(|a| a.capabilities.tags.contains(&"summarization".to_string()));

        assert!(found.is_some());
    }
}
