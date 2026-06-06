//! 摘要系统
//!
//! 处理记忆压缩的摘要请求和结果处理。

use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::{Clock, MemoryConfig},
    contracts::{AgentCapabilitySummary, FirstSummarizerPolicy, SummarizerSelectionPolicy},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        ShortTermMemory, SummarizationRequestMessage, SummarizationResultMessage,
        SummarizationTrigger, SystemOutputMessage, Task, TaskStatus, WaitingReason,
    },
    llm::{summarization_system_prompt, summarization_user_prompt},
};

/// 摘要调度系统：将摘要请求转为 AgentExecutionRequest
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

        // 构建 AgentExecutionRequest
        let execution_request = AgentExecutionRequest {
            task_id: request.task_id,
            agent_id: summarizer.id,
            request_kind: AgentRequestKind::Summarization,
            prompt: summarization_user_prompt(&request.content_to_summarize, request.target_tokens),
            system_prompt: Some(summarization_system_prompt()),
            tools: vec![],
            conversation: None,
        };

        commands.spawn(AgentExecutionRequestMessage {
            request: execution_request,
        });
        debug!(
            event = "SummarizationDispatched",
            task_id = %request.task_id,
            summarizer_agent_id = %summarizer.id,
            summarizer_agent_name = %summarizer.profile.name,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            content_len = request.content_to_summarize.len(),
            "dispatched summarization request"
        );
        commands.entity(entity).despawn();
    }
}

/// 摘要结果处理系统：更新 ShortTermMemory 并恢复任务状态
pub(crate) fn summarization_result_system(
    clock: Res<Clock>,
    config: Res<MemoryConfig>,
    mut commands: Commands,
    results: Query<(Entity, &SummarizationResultMessage)>,
    mut memories: Query<&mut ShortTermMemory>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, result) in &results {
        let task_id = result.task_id;

        match &result.summary {
            Ok(summary) => {
                // 更新摘要前缀
                if let Some(mut memory) = memories.iter_mut().next() {
                    memory.summary_prefix = Some(summary.clone());

                    // 移除已压缩的 entries（保留最近 N 轮）
                    let preserve_count = (config.preserve_recent_turns * 2) as usize;
                    let removed = if memory.entries.len() > preserve_count {
                        let removed = memory.entries.len() - preserve_count;
                        memory.entries.drain(0..removed);
                        removed
                    } else {
                        0
                    };

                    // 重新计算 token
                    memory.recalculate_tokens();

                    debug!(
                        event = "SummarizationCompleted",
                        task_id = %task_id,
                        summary_len = summary.len(),
                        summary = %summary,
                        removed_entries = removed,
                        remaining_entries = memory.entries.len(),
                        new_tokens = memory.estimated_tokens,
                        "summarization completed"
                    );
                }

                // 发送系统通知（不进入 STM）
                commands.spawn(SystemOutputMessage {
                    task_id,
                    content: format!("📝 摘要完成\n\n{}", summary),
                });

                // 恢复任务状态：从 Waiting(Summarization) 恢复为 Waiting(User)
                // 这适用于 UserCommand 和 TokenThreshold 触发的摘要
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == task_id)
                    && matches!(
                        task.status,
                        TaskStatus::Waiting(WaitingReason::Summarization)
                    )
                {
                    let old_status = task.status.clone();
                    task.status = TaskStatus::Waiting(WaitingReason::User);
                    task.updated_at = clock.0;
                    debug!(
                        event = "TaskStatusRestoredAfterSummarization",
                        task_id = %task.id,
                        from_status = ?old_status,
                        to_status = ?task.status,
                        "task restored to waiting for user after summarization"
                    );
                }
            }
            Err(error) => {
                debug!(
                    event = "SummarizationFailed",
                    task_id = %task_id,
                    error = %error.message(),
                    error_type = std::any::type_name_of_val(error),
                    "summarization failed"
                );

                // 发送系统通知（不进入 STM）
                commands.spawn(SystemOutputMessage {
                    task_id,
                    content: format!("⚠️ 摘要失败：{}", error.message()),
                });

                // 即使摘要失败，也恢复任务状态，避免任务卡住
                if let Some(mut task) = tasks.iter_mut().find(|t| t.id == task_id)
                    && matches!(
                        task.status,
                        TaskStatus::Waiting(WaitingReason::Summarization)
                    )
                {
                    let old_status = task.status.clone();
                    task.status = TaskStatus::Waiting(WaitingReason::User);
                    task.updated_at = clock.0;
                    debug!(
                        event = "TaskStatusRestoredAfterSummarizationFailed",
                        task_id = %task.id,
                        from_status = ?old_status,
                        to_status = ?task.status,
                        "task restored to waiting for user after summarization failed"
                    );
                }
            }
        }
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
