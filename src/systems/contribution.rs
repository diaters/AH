use bevy::prelude::*;
use tracing::{debug, warn};

use crate::domain::{
    Agent, AgentKind, ConfirmationOption, ConfirmationSource, ExperienceCandidateStatus,
    ExperienceCollectionCompletedMessage, ExperienceCollectionRequestMessage,
    ExperienceGovernanceRequestMessage, ExperienceKindHint, IncubationProposal,
    IncubationProposalStatus, LongTermMemory, LongTermMemoryEntry, MemoryAbsorptionMessage,
    MemoryContributionRequestMessage, MemoryImportance, SharedKnowledgeBase, SharedKnowledgeEntry,
    ShortTermMemory, SpaceToolRegistry, Task, TaskSummary, TaskTerminatedMessage,
    ToolConfirmationRequestMessage, ToolConfirmationResponseMessage, WorkItem,
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

/// 经验收集完成处理系统：将非顶层候选标记为已汇聚，顶层候选推进到治理挂起。
pub(crate) fn experience_collection_completion_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    messages: Query<(Entity, &ExperienceCollectionCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        if let Some(parent_task_id) = msg.parent_task_id {
            // 非顶层：消费父任务 inbox 中的子候选，标记为 Aggregated。
            let ids = store.aggregate_inbox_for_task(parent_task_id);
            debug!(
                event = "ExperienceCollectionAggregated",
                task_id = %msg.task_id,
                parent_task_id = %parent_task_id,
                aggregated_count = ids.len(),
                "aggregated child candidates into parent inbox"
            );
        } else {
            // 顶层：将 root 候选推进到 GovernancePending 并触发治理。
            let ids = store.promote_root_candidates_to_governance(msg.task_id);
            if !ids.is_empty() {
                commands.spawn(ExperienceGovernanceRequestMessage {
                    task_id: msg.task_id,
                    agent_id: msg.agent_id,
                });
                debug!(
                    event = "TopLevelExperienceGovernanceRequested",
                    task_id = %msg.task_id,
                    candidate_count = ids.len(),
                    "spawned top-level experience governance request"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 经验治理系统：顶层唯一最终分流点。
#[allow(clippy::too_many_arguments)]
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    mut upgrade_queue: ResMut<crate::domain::SharedKnowledgeUpgradeQueue>,
    upgrade_service: Res<crate::infrastructure::memory::SharedKnowledgeUpgradeService>,
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    for (entity, request) in &requests {
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

        let is_default = is_default_agent(agent);
        let candidate_ids = store.governance_candidates_for_task(request.task_id);

        if candidate_ids.is_empty() {
            debug!(
                event = "ExperienceGovernanceNoCandidates",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                "no governance-pending candidates to govern, skipping"
            );
            commands.entity(entity).despawn();
            continue;
        }

        // 记录治理者，供确认后写回路由使用。
        for id in &candidate_ids {
            if let Some(c) = store.candidates.get_mut(id) {
                c.governing_agent_id = Some(request.agent_id);
            }
        }

        for candidate_id in &candidate_ids {
            let candidate = match store.candidates.get(candidate_id).cloned() {
                Some(c) => c,
                None => continue,
            };

            match candidate.kind_hint {
                ExperienceKindHint::Discard => {
                    if let Some(c) = store.candidates.get_mut(candidate_id) {
                        c.status = ExperienceCandidateStatus::Rejected;
                    }
                    debug!(
                        event = "ExperienceGovernanceRejected",
                        candidate_id = %candidate_id,
                        task_id = %request.task_id,
                        "discarded candidate"
                    );
                }
                ExperienceKindHint::SharedKnowledge => {
                    upgrade_queue
                        .candidates
                        .push(crate::domain::SharedKnowledgeUpgradeCandidate {
                            candidate_id: uuid::Uuid::new_v4(),
                            content: candidate.payload.content().unwrap_or_default(),
                            kind: crate::domain::LongTermMemoryKind::Fact,
                            scope_tags: Vec::new(),
                            source_candidate_id: candidate.candidate_id,
                            source_agent_id: candidate.producer_agent_id,
                            source_task_id: candidate.producer_task_id,
                            validation_status: crate::domain::KnowledgeValidationStatus::Candidate,
                            created_at: chrono::Utc::now(),
                        });
                    match upgrade_service.persist(&upgrade_queue) {
                        Ok(_) => {
                            if let Some(c) = store.candidates.get_mut(candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                            debug!(
                                event = "ExperienceGovernanceSharedKnowledgeQueued",
                                candidate_id = %candidate_id,
                                task_id = %request.task_id,
                                "queued and persisted shared knowledge upgrade candidate"
                            );
                        }
                        Err(e) => {
                            warn!(
                                event = "ExperienceWritebackFailed",
                                candidate_id = %candidate_id,
                                task_id = %request.task_id,
                                target = "SharedKnowledgeUpgradeQueue",
                                error = %e,
                                "failed to persist shared knowledge upgrade candidate"
                            );
                        }
                    }
                }
                ExperienceKindHint::Executable => {
                    if is_default {
                        spawn_incubation_confirmation(
                            &mut commands,
                            &mut store,
                            request,
                            agent,
                            candidate_id,
                        );
                    } else {
                        if let Some(c) = store.candidates.get_mut(candidate_id) {
                            c.status = ExperienceCandidateStatus::NeedsUserApproval;
                        }
                        spawn_experience_confirmation(
                            &mut commands,
                            request,
                            candidate_id,
                            &candidate,
                        );
                    }
                }
                ExperienceKindHint::Knowledge => {
                    if is_default {
                        spawn_incubation_confirmation(
                            &mut commands,
                            &mut store,
                            request,
                            agent,
                            candidate_id,
                        );
                    } else {
                        let mut persisted = false;
                        if let Some(mut entry) = candidate.as_long_term_memory_entry() {
                            entry.source_candidate_id = Some(candidate.candidate_id);
                            entry.source_task_id = Some(candidate.producer_task_id);
                            entry.agent_id = Some(candidate.producer_agent_id);

                            if let Some(mut memory) = long_memories
                                .iter_mut()
                                .find(|lm| lm.agent_name.as_deref() == Some(&agent.profile.name))
                            {
                                match service.add_entry(&mut memory, entry) {
                                    Ok(_) => persisted = true,
                                    Err(e) => {
                                        warn!(
                                            event = "ExperienceWritebackFailed",
                                            candidate_id = %candidate_id,
                                            task_id = %request.task_id,
                                            target = "LongTermMemory",
                                            error = %e,
                                            "failed to auto-persist knowledge candidate"
                                        );
                                    }
                                }
                            } else {
                                warn!(
                                    event = "ExperienceWritebackFailed",
                                    candidate_id = %candidate_id,
                                    task_id = %request.task_id,
                                    target = "LongTermMemory",
                                    reason = "agent_memory_not_found",
                                    "no LongTermMemory component found for governing agent"
                                );
                            }
                        }
                        if persisted {
                            if let Some(c) = store.candidates.get_mut(candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                            debug!(
                                event = "ExperienceGovernancePersisted",
                                candidate_id = %candidate_id,
                                task_id = %request.task_id,
                                agent_name = %agent.profile.name,
                                "persisted knowledge candidate to long-term memory"
                            );
                        }
                    }
                }
            }
        }

        commands.entity(entity).despawn();
    }
}

fn is_default_agent(agent: &Agent) -> bool {
    agent.capabilities.tags.iter().any(|t| t == "default")
}

fn spawn_experience_confirmation(
    commands: &mut Commands,
    request: &ExperienceGovernanceRequestMessage,
    candidate_id: &uuid::Uuid,
    candidate: &crate::domain::ExperienceCandidate,
) {
    let request_id = uuid::Uuid::new_v4();
    commands.spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id: request.task_id,
        agent_id: request.agent_id,
        tool_name: "experience_governance".to_string(),
        tool_input: serde_json::json!({
            "candidate_id": candidate_id.to_string(),
            "title": candidate.title,
            "kind": format!("{:?}", candidate.kind_hint),
        }),
        options: ConfirmationOption::default_options(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
    });
}

fn spawn_incubation_confirmation(
    commands: &mut Commands,
    store: &mut crate::domain::ExperienceStore,
    request: &ExperienceGovernanceRequestMessage,
    agent: &Agent,
    candidate_id: &uuid::Uuid,
) {
    if let Some(c) = store.candidates.get_mut(candidate_id) {
        c.status = ExperienceCandidateStatus::NeedsUserApproval;
    }
    let candidate = store.candidates.get(candidate_id).cloned();
    if let Some(candidate) = candidate {
        let proposal_id = uuid::Uuid::new_v4();
        let (knowledge_ids, executable_ids, shared_ids) = match candidate.kind_hint {
            ExperienceKindHint::Knowledge => (vec![*candidate_id], vec![], vec![]),
            ExperienceKindHint::Executable => (vec![], vec![*candidate_id], vec![]),
            ExperienceKindHint::SharedKnowledge => (vec![], vec![], vec![*candidate_id]),
            ExperienceKindHint::Discard => (vec![], vec![], vec![]),
        };

        commands.spawn(IncubationProposal {
            proposal_id,
            source_agent_id: request.agent_id,
            source_task_id: request.task_id,
            proposed_agent_profile: crate::domain::AgentProfile {
                name: format!("incubated-{}", proposal_id),
                model: agent.profile.model.clone(),
            },
            knowledge_candidate_ids: knowledge_ids,
            executable_candidate_ids: executable_ids,
            shared_knowledge_candidate_ids: shared_ids,
            status: IncubationProposalStatus::Proposed,
            created_at: chrono::Utc::now(),
        });

        spawn_experience_confirmation(commands, request, candidate_id, &candidate);
    }
}

/// 经验确认结果系统：处理用户对经验候选的确认，触发最终写回。
#[allow(clippy::too_many_arguments)]
pub(crate) fn experience_approval_result_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    asset_service: Res<crate::infrastructure::assets::AgentAssetService>,
    mut upgrade_queue: ResMut<crate::domain::SharedKnowledgeUpgradeQueue>,
    upgrade_service: Res<crate::infrastructure::memory::SharedKnowledgeUpgradeService>,
    mut proposals: Query<&mut IncubationProposal>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
) {
    for (entity, response) in &responses {
        let approved = response.selected_option != "deny";
        store.apply_confirmation_response(response.request_id, &response.selected_option);

        if approved {
            let to_writeback: Vec<_> = store
                .candidates
                .values()
                .filter(|c| c.status == ExperienceCandidateStatus::Approved)
                .cloned()
                .collect();

            for candidate in to_writeback {
                let is_default = candidate
                    .governing_agent_id
                    .and_then(|id| agents.iter().find(|a| a.id == id))
                    .map(is_default_agent)
                    .unwrap_or(false);

                match candidate.kind_hint {
                    ExperienceKindHint::Knowledge => {
                        if is_default {
                            if let Some(mut proposal) = proposals.iter_mut().find(|p| {
                                p.knowledge_candidate_ids.contains(&candidate.candidate_id)
                            }) {
                                proposal.status = IncubationProposalStatus::Approved;
                            }
                            if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                        } else if let Some(mut entry) = candidate.as_long_term_memory_entry() {
                            entry.source_candidate_id = Some(candidate.candidate_id);
                            entry.source_task_id = Some(candidate.producer_task_id);
                            entry.agent_id = Some(candidate.producer_agent_id);

                            let mut persisted = false;
                            let producer_agent =
                                agents.iter().find(|a| a.id == candidate.producer_agent_id);
                            if let Some(agent) = producer_agent
                                && let Some(mut memory) = long_memories.iter_mut().find(|lm| {
                                    lm.agent_name.as_deref() == Some(&agent.profile.name)
                                })
                            {
                                match service.add_entry(&mut memory, entry) {
                                    Ok(_) => persisted = true,
                                    Err(e) => {
                                        warn!(
                                            event = "ExperienceWritebackFailed",
                                            candidate_id = %candidate.candidate_id,
                                            target = "LongTermMemory",
                                            error = %e,
                                            "failed to persist knowledge candidate"
                                        );
                                    }
                                }
                            }
                            if persisted
                                && let Some(c) = store.candidates.get_mut(&candidate.candidate_id)
                            {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                        }
                    }
                    ExperienceKindHint::Executable => {
                        if is_default {
                            if let Some(mut proposal) = proposals.iter_mut().find(|p| {
                                p.executable_candidate_ids.contains(&candidate.candidate_id)
                            }) {
                                proposal.status = IncubationProposalStatus::Approved;
                            }
                            if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                        } else if let Some(agent) =
                            agents.iter().find(|a| a.id == candidate.producer_agent_id)
                            && let crate::domain::ExperienceCandidatePayload::Executable {
                                intent,
                                when_to_use,
                                asset_refs,
                            } = &candidate.payload
                        {
                            let draft = crate::infrastructure::assets::SkillPackageDraft {
                                skill_id: format!("{}", candidate.candidate_id),
                                title: candidate.title.clone(),
                                problem: intent.clone(),
                                when_to_use: when_to_use.clone(),
                                steps: "参见 skill.md 与 scripts/ 目录".to_string(),
                                asset_refs: asset_refs.clone(),
                                dependency_refs: candidate.dependency_refs.clone(),
                                risks: "首版实现，需人工复核".to_string(),
                                source_task_id: Some(candidate.producer_task_id),
                                source_candidate_id: Some(candidate.candidate_id),
                            };
                            match asset_service.persist_skill_package(&agent.profile.name, &draft) {
                                Ok(_) => {
                                    if let Some(c) =
                                        store.candidates.get_mut(&candidate.candidate_id)
                                    {
                                        c.status = ExperienceCandidateStatus::Persisted;
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        event = "ExperienceWritebackFailed",
                                        candidate_id = %candidate.candidate_id,
                                        target = "SkillPackage",
                                        error = %e,
                                        "failed to persist skill package"
                                    );
                                }
                            }
                        }
                    }
                    ExperienceKindHint::SharedKnowledge => {
                        if let Some(existing) = upgrade_queue
                            .candidates
                            .iter_mut()
                            .find(|u| u.source_candidate_id == candidate.candidate_id)
                        {
                            existing.validation_status =
                                crate::domain::KnowledgeValidationStatus::Approved;
                        }
                        match upgrade_service.persist(&upgrade_queue) {
                            Ok(_) => {
                                if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                    c.status = ExperienceCandidateStatus::Persisted;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    event = "ExperienceWritebackFailed",
                                    candidate_id = %candidate.candidate_id,
                                    target = "SharedKnowledgeUpgradeQueue",
                                    error = %e,
                                    "failed to persist shared knowledge approval"
                                );
                            }
                        }
                    }
                    ExperienceKindHint::Discard => {}
                }

                debug!(
                    event = "ExperienceCandidateFinalWriteback",
                    candidate_id = %candidate.candidate_id,
                    kind = ?candidate.kind_hint,
                    is_default = is_default,
                    "finalized experience candidate after user approval"
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
    fn is_default_agent_detects_by_tag_not_name() {
        let default_agent = crate::domain::Agent {
            id: uuid::Uuid::new_v4(),
            profile: crate::domain::AgentProfile {
                name: "custom-default".to_string(),
                model: "test".to_string(),
            },
            capabilities: crate::domain::AgentCapabilities {
                tags: vec!["default".to_string(), "llm".to_string()],
                description: "default agent".to_string(),
            },
            kind: crate::domain::AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions::default(),
        };

        assert!(is_default_agent(&default_agent));
    }

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

    #[test]
    fn experience_collection_completion_aggregates_child_candidates() {
        use crate::domain::{ExperienceStore, TaskId};

        let parent_task_id: TaskId = uuid::Uuid::new_v4();
        let child_task_id: TaskId = uuid::Uuid::new_v4();
        let parent_agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();

        // 子层候选进入父层 inbox
        let child_candidate = crate::domain::ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            child_task_id,
            uuid::Uuid::new_v4(),
            "child fact".to_string(),
            "content".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        );
        store.queue_for_parent(parent_task_id, parent_agent_id, child_candidate);

        // 汇聚：消费 inbox
        let ids = store.aggregate_inbox_for_task(parent_task_id);
        assert!(!ids.is_empty());
        assert_eq!(
            store.candidates.get(&ids[0]).unwrap().status,
            crate::domain::ExperienceCandidateStatus::Aggregated
        );

        // 顶层：暂存 root 候选并推进到治理
        let root_candidate = crate::domain::ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            parent_task_id,
            parent_agent_id,
            "root fact".to_string(),
            "root content".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        );
        store.stage_root_candidate(root_candidate);
        let governance_ids = store.promote_root_candidates_to_governance(parent_task_id);
        assert!(!governance_ids.is_empty());
        assert_eq!(
            store.candidates.get(&governance_ids[0]).unwrap().status,
            crate::domain::ExperienceCandidateStatus::GovernancePending
        );
    }

    #[test]
    fn approved_executable_becomes_persisted() {
        use crate::domain::{
            ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
            ExperienceKindHint,
        };

        let mut store = crate::domain::ExperienceStore::default();
        let candidate = ExperienceCandidate {
            candidate_id: uuid::Uuid::new_v4(),
            producer_task_id: uuid::Uuid::new_v4(),
            producer_agent_id: uuid::Uuid::new_v4(),
            title: "test skill".to_string(),
            kind_hint: ExperienceKindHint::Executable,
            payload: ExperienceCandidatePayload::Executable {
                intent: "run smoke test".to_string(),
                when_to_use: "after changes".to_string(),
                asset_refs: vec![],
            },
            dependency_refs: vec![],
            status: ExperienceCandidateStatus::NeedsUserApproval,
            governing_agent_id: None,
        };
        store.stage_root_candidate(candidate);
        store.apply_confirmation_response(uuid::Uuid::new_v4(), "approve");

        assert!(
            store
                .candidates
                .values()
                .any(|c| c.status == ExperienceCandidateStatus::Approved),
            "approved executable should be marked Approved"
        );
    }
}
