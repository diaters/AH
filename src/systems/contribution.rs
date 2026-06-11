use bevy::prelude::*;
use tracing::debug;

use crate::domain::{
    Agent, AgentKind, LongTermMemory, LongTermMemoryEntry, MemoryAbsorptionMessage,
    MemoryContributionRequestMessage, MemoryImportance, SharedKnowledgeBase, SharedKnowledgeEntry,
    Task, TaskSummary, TaskTerminatedMessage,
};
use crate::infrastructure::memory::LongTermMemoryService;

/// Agent 终止系统：检测任务型 Agent 销毁，生成贡献请求
pub(crate) fn agent_termination_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    agents: Query<(Entity, &Agent)>,
    tasks: Query<&Task>,
    long_memories: Query<&LongTermMemory>,
) {
    for (_entity, terminated_msg) in &terminated {
        // 查找绑定的任务型 Agent
        for (agent_entity, agent) in &agents {
            if agent.kind != AgentKind::TaskScoped {
                continue;
            }
            if agent.bound_task_id != Some(terminated_msg.task_id) {
                continue;
            }

            // 获取父 Agent ID
            let Some(parent_id) = agent.parent_id else {
                continue;
            };

            // 获取任务信息用于摘要
            let task = tasks.iter().find(|t| t.id == terminated_msg.task_id);
            let task_summary = task.map(|t| TaskSummary {
                task_id: t.id,
                goal: t.content.clone(),
                outcome: t.result_summary.clone(),
            });

            // 获取长期记忆
            let long_memory = long_memories.get(agent_entity).ok();

            debug!(
                event = "AgentTerminationDetected",
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                task_id = %terminated_msg.task_id,
                parent_id = %parent_id,
                has_ltm = long_memory.is_some(),
                ltm_entries = long_memory.map(|m| m.entries.len()).unwrap_or(0),
                "generating memory contribution request"
            );

            // 生成贡献请求
            commands.spawn(MemoryContributionRequestMessage {
                contributor_id: agent.id,
                contributor_name: agent.profile.name.clone(),
                parent_id,
                memories: long_memory.map(|m| m.entries.clone()).unwrap_or_default(),
                task_summary: task_summary.unwrap_or_else(|| TaskSummary {
                    task_id: terminated_msg.task_id,
                    goal: String::new(),
                    outcome: String::new(),
                }),
            });
        }
        // Note: TaskTerminatedMessage is despawned by agent_factory_system
        // This system must run BEFORE agent_factory_system
    }
}

/// 记忆贡献处理系统：执行 LLM 评估并吸收记忆
pub(crate) fn memory_contribution_system(
    mut commands: Commands,
    mut knowledge: ResMut<SharedKnowledgeBase>,
    requests: Query<(Entity, &MemoryContributionRequestMessage)>,
) {
    for (entity, request) in &requests {
        let parent_id = request.parent_id;
        let (accepted, candidates) = extract_memory_writebacks(
            &request.contributor_name,
            &request.task_summary,
            &request.memories,
        );

        debug!(
            event = "MemoryContributionProcessing",
            contributor_id = %request.contributor_id,
            contributor_name = %request.contributor_name,
            parent_id = %parent_id,
            memories_count = request.memories.len(),
            accepted_count = accepted.len(),
            candidate_count = candidates.len(),
            memories = ?request.memories.iter().map(|m| &m.content).collect::<Vec<_>>(),
            task_summary = ?request.task_summary,
            "processing memory contribution request"
        );

        commands.spawn(MemoryAbsorptionMessage {
            parent_id,
            absorbed: accepted,
        });

        knowledge.entries.extend(candidates);
        commands.entity(entity).despawn();
    }
}

/// 根据子 Agent 贡献提炼长期记忆写回结果。
pub(crate) fn extract_memory_writebacks(
    contributor_name: &str,
    task_summary: &TaskSummary,
    memories: &[LongTermMemoryEntry],
) -> (Vec<LongTermMemoryEntry>, Vec<SharedKnowledgeEntry>) {
    let mut accepted = Vec::new();
    let mut candidates = Vec::new();

    for memory in memories {
        if memory.content.trim().is_empty() || memory.decay_score <= 0.2 {
            continue;
        }
        if memory.content.to_lowercase().contains("temporary") {
            continue;
        }

        let mut accepted_entry = memory.clone();
        accepted_entry.source = format!("task:{}:{}", task_summary.task_id, contributor_name);
        accepted.push(accepted_entry.clone());

        if accepted_entry.importance >= MemoryImportance::High && accepted_entry.confidence >= 0.9 {
            candidates.push(SharedKnowledgeEntry::candidate(
                accepted_entry.content.clone(),
                accepted_entry.kind,
            ));
        }
    }

    (accepted, candidates)
}

/// 记忆吸收系统：将评估后的记忆写入父 Agent，并立即落盘
pub(crate) fn memory_absorption_system(
    mut commands: Commands,
    absorptions: Query<(Entity, &MemoryAbsorptionMessage)>,
    agents: Query<(Entity, &Agent)>,
    mut long_memories: Query<&mut LongTermMemory>,
    mut service: ResMut<LongTermMemoryService>,
) {
    for (entity, absorption) in &absorptions {
        // 查找父 Agent
        let parent = agents.iter().find(|(_, a)| a.id == absorption.parent_id);

        if let Some((parent_entity, parent)) = parent {
            // 找到父 Agent 的长期记忆并吸收
            if let Ok(mut memory) = long_memories.get_mut(parent_entity) {
                let before_count = memory.entries.len();
                memory.absorb(absorption.absorbed.clone());

                // 立即落盘
                if let Err(e) = service.flush(&memory) {
                    debug!(
                        event = "LongTermMemoryPersistFailed",
                        parent_agent_id = %absorption.parent_id,
                        parent_agent_name = %parent.profile.name,
                        error = %e,
                        "failed to persist absorbed memories"
                    );
                }

                debug!(
                    event = "MemoryAbsorbed",
                    parent_agent_id = %absorption.parent_id,
                    parent_agent_name = %parent.profile.name,
                    absorbed_count = absorption.absorbed.len(),
                    ltm_entries_before = before_count,
                    ltm_entries_after = memory.entries.len(),
                    "absorbed memories into parent agent"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LongTermMemoryEntry, LongTermMemoryKind};

    #[test]
    fn memory_contribution_skips_low_value_entries_and_creates_candidates() {
        let summary = TaskSummary {
            task_id: uuid::Uuid::nil(),
            goal: "stabilize shell behavior".to_string(),
            outcome: "done".to_string(),
        };

        let entries = vec![
            LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "shell stop uses timeout"),
            LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "temporary debugging note"),
        ];

        let (accepted, candidates) = extract_memory_writebacks("worker", &summary, &entries);

        assert_eq!(accepted.len(), 1);
        assert!(accepted[0].content.contains("shell stop"));
        assert!(candidates.is_empty());
    }
}
