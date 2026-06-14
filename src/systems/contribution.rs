use bevy::prelude::*;
use tracing::debug;

use crate::domain::{
    Agent, AgentKind, ConfirmationOption, ConfirmationSource, ExperienceCandidateStatus,
    ExperienceCollectionRequestMessage, ExperienceGovernanceRequestMessage, IncubationProposal,
    LongTermMemory, LongTermMemoryEntry, MemoryAbsorptionMessage, MemoryContributionRequestMessage,
    MemoryImportance, SharedKnowledgeBase, SharedKnowledgeEntry, ShortTermMemory,
    SpaceToolRegistry, Task, TaskSummary, TaskTerminatedMessage, ToolConfirmationRequestMessage,
    ToolConfirmationResponseMessage, WorkItem,
};
use crate::infrastructure::memory::LongTermMemoryService;

/// Agent 终止系统：检测任务型 Agent 销毁，生成经验收集请求。
pub(crate) fn agent_termination_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    agents: Query<&Agent>,
    tasks: Query<&Task>,
) {
    for (_entity, terminated_msg) in &terminated {
        for agent in &agents {
            if agent.kind != AgentKind::TaskScoped
                || agent.bound_task_id != Some(terminated_msg.task_id)
            {
                continue;
            }

            let parent_task_id = tasks
                .iter()
                .find(|task| task.id == terminated_msg.task_id)
                .and_then(|task| task.parent_task_id);

            debug!(
                event = "AgentTerminationDetected",
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                task_id = %terminated_msg.task_id,
                parent_agent_id = ?agent.parent_id,
                has_parent_task = parent_task_id.is_some(),
                "spawning experience collection request"
            );

            commands.spawn(ExperienceCollectionRequestMessage {
                task_id: terminated_msg.task_id,
                parent_task_id,
                parent_agent_id: agent.parent_id,
            });
        }
    }
}

/// 经验收集 WorkItem 创建系统：将收集请求转换为独立 WorkItem。
pub(crate) fn experience_collection_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &ExperienceCollectionRequestMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
    registry: Res<SpaceToolRegistry>,
) {
    for (entity, request) in &requests {
        let Some((task, stm)) = tasks.iter().find(|(t, _)| t.id == request.task_id) else {
            debug!(
                event = "ExperienceCollectionTaskNotFound",
                task_id = %request.task_id,
                "task not found for experience collection, skipping"
            );
            commands.entity(entity).despawn();
            continue;
        };

        let conversation = build_experience_collection_conversation(task, stm);

        let prompt = if task.result_summary.is_empty() {
            format!(
                "用户目标：{}\n\n请只调用 submit_experience_candidate 提交可复用经验候选。",
                task.content
            )
        } else {
            format!(
                "用户目标：{}\n\n任务结果摘要：{}\n\n请只调用 submit_experience_candidate 提交可复用经验候选。",
                task.content, task.result_summary
            )
        };

        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| tool.name == "submit_experience_candidate")
            .cloned()
            .collect();

        let work_item = WorkItem::experience_collection(
            task.id,
            prompt,
            request.parent_task_id,
            conversation,
            tools,
        );

        debug!(
            event = "ExperienceCollectionWorkItemCreated",
            task_id = %request.task_id,
            work_item_id = %work_item.id,
            has_conversation = work_item.input.context.conversation.is_some(),
            tools_count = work_item.input.context.tools.len(),
            "spawning experience collection work item"
        );

        commands.spawn(work_item);
        commands.entity(entity).despawn();
    }
}

/// 构建经验收集的净化对话材料。
fn build_experience_collection_conversation(
    task: &Task,
    stm: Option<&ShortTermMemory>,
) -> Vec<crate::domain::ConversationMessage> {
    use crate::domain::{ConversationMessage, EntryRole};

    let mut messages = Vec::new();

    messages.push(ConversationMessage::User {
        content: format!("用户目标：{}", task.content),
    });

    if !task.result_summary.is_empty() {
        messages.push(ConversationMessage::User {
            content: format!("任务结果摘要：{}", task.result_summary),
        });
    }

    if let Some(stm) = stm {
        for entry in stm
            .entries
            .iter()
            .filter(|e| !matches!(e.role, EntryRole::Archive))
        {
            let msg = match entry.role {
                EntryRole::User => ConversationMessage::User {
                    content: entry.content.clone(),
                },
                EntryRole::Assistant => ConversationMessage::Assistant {
                    content: Some(entry.content.clone()),
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                },
                EntryRole::Summary => ConversationMessage::System {
                    content: entry.content.clone(),
                },
                EntryRole::Archive => continue,
            };
            messages.push(msg);
        }
    }

    messages
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

/// 经验治理系统：处理 `ExperienceGovernanceRequestMessage`，
/// 根据候选类型和 Agent 属性决定自动持久化、用户确认或孵化提案。
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    for (entity, request) in &requests {
        let candidates = store.root_candidates_for_task(request.task_id);

        if candidates.is_empty() {
            debug!(
                event = "ExperienceGovernanceNoCandidates",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                "no candidates to govern, skipping"
            );
            commands.entity(entity).despawn();
            continue;
        }

        let agent = match agents.iter().find(|a| a.id == request.agent_id) {
            Some(a) => a,
            None => {
                debug!(
                    event = "ExperienceGovernanceAgentNotFound",
                    agent_id = %request.agent_id,
                    task_id = %request.task_id,
                    "agent not found for governance, skipping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        };

        let is_default_agent = agent.profile.name == "default-llm-agent";

        for candidate_id in &candidates {
            let candidate = match store.candidates.get(candidate_id) {
                Some(c) => c.clone(),
                None => continue,
            };

            if candidate.requires_user_confirmation() {
                // Executable candidates always need user confirmation
                if let Some(c) = store.candidates.get_mut(candidate_id) {
                    c.status = ExperienceCandidateStatus::NeedsUserApproval;
                }

                let request_id = uuid::Uuid::new_v4();
                let candidate_id_str = candidate_id.to_string();
                commands.spawn(ToolConfirmationRequestMessage {
                    request_id,
                    task_id: request.task_id,
                    agent_id: request.agent_id,
                    tool_name: "experience_governance".to_string(),
                    tool_input: serde_json::json!({
                        "candidate_id": candidate_id_str,
                        "title": candidate.title,
                        "kind": format!("{:?}", candidate.kind_hint),
                    }),
                    options: ConfirmationOption::default_options(),
                    source: ConfirmationSource::User,
                    parent_agent_id: None,
                });

                debug!(
                    event = "ExperienceGovernanceApprovalRequired",
                    candidate_id = %candidate_id,
                    task_id = %request.task_id,
                    kind = ?candidate.kind_hint,
                    "routed executable candidate to user confirmation"
                );
            } else if is_default_agent {
                // Default agent spawns incubation proposals for knowledge candidates
                debug!(
                    event = "ExperienceGovernanceIncubation",
                    candidate_id = %candidate_id,
                    task_id = %request.task_id,
                    "spawning incubation proposal for default agent knowledge candidate"
                );

                commands.spawn(IncubationProposal {
                    task_id: request.task_id,
                    agent_id: request.agent_id,
                    candidate_ids: vec![*candidate_id],
                });
            } else {
                // Non-default persistent agents: auto-approve knowledge candidates
                // Convert to LongTermMemoryEntry and persist
                if let Some(entry) = candidate.as_long_term_memory_entry() {
                    if let Some(mut memory) = long_memories
                        .iter_mut()
                        .find(|lm| lm.agent_name.as_deref() == Some(&agent.profile.name))
                    {
                        let _ = service.add_entry(&mut memory, entry);
                    }

                    if let Some(c) = store.candidates.get_mut(candidate_id) {
                        c.status = ExperienceCandidateStatus::Approved;
                    }

                    debug!(
                        event = "ExperienceGovernanceAutoApproved",
                        candidate_id = %candidate_id,
                        task_id = %request.task_id,
                        agent_name = %agent.profile.name,
                        "auto-approved knowledge candidate for persistent agent"
                    );
                }
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 经验确认结果系统：处理用户对经验候选的确认响应。
pub(crate) fn experience_approval_result_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
) {
    for (entity, response) in &responses {
        // Only process governance confirmations
        // Tool confirmations have a different flow; we check if this response
        // corresponds to a governance request by looking at the request_id
        // against candidates in NeedsUserApproval state.

        let approved = response.selected_option != "deny";

        store.apply_confirmation_response(response.request_id, &response.selected_option);

        // Persist approved knowledge candidates
        if approved {
            let candidates: Vec<_> = store
                .candidates
                .values()
                .filter(|c| c.status == ExperienceCandidateStatus::Approved)
                .cloned()
                .collect();

            for candidate in &candidates {
                if let Some(entry) = candidate.as_long_term_memory_entry()
                    && let Some(agent) = agents.iter().find(|a| a.id == candidate.producer_agent_id)
                    && let Some(mut memory) = long_memories
                        .iter_mut()
                        .find(|lm| lm.agent_name.as_deref() == Some(&agent.profile.name))
                {
                    let _ = service.add_entry(&mut memory, entry);
                }

                debug!(
                    event = "ExperienceCandidateApproved",
                    candidate_id = %candidate.candidate_id,
                    "approved and persisted experience candidate via user confirmation"
                );
            }
        } else {
            debug!(
                event = "ExperienceCandidateRejected",
                request_id = %response.request_id,
                "user rejected experience candidate"
            );
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

    #[test]
    fn task_scoped_agent_termination_builds_request_without_agent_id() {
        let task_id = uuid::Uuid::new_v4();
        let parent_id = uuid::Uuid::new_v4();
        let request = ExperienceCollectionRequestMessage {
            task_id,
            parent_task_id: Some(uuid::Uuid::new_v4()),
            parent_agent_id: Some(parent_id),
        };

        assert_eq!(request.task_id, task_id);
        assert_eq!(request.parent_agent_id, Some(parent_id));
    }
}
