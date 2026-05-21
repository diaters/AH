use bevy::prelude::*;
use tracing::info;

use crate::{
    app::{Clock, MemoryConfig},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        ShortTermMemory, SummarizationRequestMessage, SummarizationResultMessage, Task, TaskStatus,
        WaitingReason,
    },
    llm::{summarization_system_prompt, summarization_user_prompt},
};

/// 摘要调度系统：将摘要请求转为 AgentExecutionRequest
pub(crate) fn summarization_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    agents: Query<&Agent>,
    requests: Query<(Entity, &SummarizationRequestMessage)>,
    mut tasks: Query<&mut Task>,
) {
    // 查找 summarizer Agent
    let summarizer = agents.iter().find(|a| {
        a.kind == AgentKind::Persistent
            && a.capabilities.tags.contains(&"summarization".to_string())
    });

    let Some(summarizer) = summarizer else {
        info!("no summarizer agent found, skipping summarization requests");
        // 没有 summarizer，清理所有请求
        for (entity, _) in &requests {
            commands.entity(entity).despawn();
        }
        return;
    };

    for (entity, request) in &requests {
        // 标记任务为等待摘要
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.task_id) {
            task.status = TaskStatus::Waiting(WaitingReason::Summarization);
            task.updated_at = clock.0;
            info!(task_id = %request.task_id, "task waiting for summarization");
        }

        // 构建 AgentExecutionRequest
        let execution_request = AgentExecutionRequest {
            task_id: request.task_id,
            agent_id: summarizer.id,
            request_kind: AgentRequestKind::Summarization,
            prompt: summarization_user_prompt(&request.content_to_summarize, request.target_tokens),
            system_prompt: Some(summarization_system_prompt()),
        };

        commands.spawn(AgentExecutionRequestMessage {
            request: execution_request,
        });
        info!(
            task_id = %request.task_id,
            trigger = ?request.trigger,
            target_tokens = request.target_tokens,
            "dispatched summarization request"
        );
        commands.entity(entity).despawn();
    }
}

/// 摘要结果处理系统：更新 ShortTermMemory
pub(crate) fn summarization_result_system(
    clock: Res<Clock>,
    config: Res<MemoryConfig>,
    mut commands: Commands,
    results: Query<(Entity, &SummarizationResultMessage)>,
    mut tasks_with_memory: Query<(&mut Task, &mut ShortTermMemory)>,
    mut tasks_without_memory: Query<&mut Task, Without<ShortTermMemory>>,
) {
    for (entity, result) in &results {
        let task_id = result.task_id;

        match &result.summary {
            Ok(summary) => {
                // 查找与任务关联的记忆并更新（Task 和 ShortTermMemory 在同一实体上）
                let mut found = false;
                for (mut task, mut memory) in &mut tasks_with_memory {
                    if task.id == task_id {
                        // 更新摘要前缀
                        memory.summary_prefix = Some(summary.clone());

                        // 移除已压缩的 entries（保留最近 N 轮）
                        let preserve_count = (config.preserve_recent_turns * 2) as usize;
                        if memory.entries.len() > preserve_count {
                            let removed = memory.entries.len() - preserve_count;
                            memory.entries.drain(0..removed);
                            info!(task_id = %task_id, removed_count = removed, "removed compressed entries");
                        }

                        // 重新计算 token
                        memory.recalculate_tokens();

                        // 恢复任务状态
                        task.status = TaskStatus::Ready;
                        task.updated_at = clock.0;

                        found = true;
                        break;
                    }
                }

                if !found {
                    // 任务没有 ShortTermMemory，只恢复状态
                    if let Some(mut task) = tasks_without_memory.iter_mut().find(|t| t.id == task_id) {
                        task.status = TaskStatus::Ready;
                        task.updated_at = clock.0;
                    }
                }

                info!(
                    task_id = %task_id,
                    summary_len = summary.len(),
                    "summarization completed"
                );
            }
            Err(error) => {
                // 摘要失败，记录错误但恢复任务状态
                if let Some(mut task) = tasks_with_memory.iter_mut().map(|(t, _)| t).find(|t| t.id == task_id) {
                    task.status = TaskStatus::Ready;
                    task.updated_at = clock.0;
                    task.last_error = Some(format!("summarization failed: {}", error.message()));
                } else if let Some(mut task) = tasks_without_memory.iter_mut().find(|t| t.id == task_id) {
                    task.status = TaskStatus::Ready;
                    task.updated_at = clock.0;
                    task.last_error = Some(format!("summarization failed: {}", error.message()));
                }
                info!(task_id = %task_id, error = ?error, "summarization failed");
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
