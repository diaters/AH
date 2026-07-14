use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{
    Agent, ExperienceCandidateStatus, ExperienceStore, ExperienceWritebackDestination,
    ExperienceWritebackRequestMessage, LongTermMemory, TaskId,
};
use crate::infrastructure::memory::LongTermMemoryService;

fn build_incubated_agent_description(
    store: &crate::domain::ExperienceStore,
    proposal: &crate::domain::IncubationProposal,
) -> String {
    let titles: Vec<String> = proposal
        .knowledge_candidate_ids
        .iter()
        .filter_map(|id| store.candidates.get(id).map(|c| c.title.clone()))
        .collect();

    match titles.len() {
        0 => String::new(),
        1 => titles[0].clone(),
        n => format!(
            "基于 {} 条经验孵化：{}",
            n,
            titles
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("；")
        ),
    }
}

/// 统一写回执行系统：根据治理决议执行正式写回。
#[allow(clippy::too_many_arguments)]
pub(crate) fn experience_writeback_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    asset_service: Res<crate::infrastructure::assets::AgentAssetService>,
    proposal_store: Res<crate::infrastructure::incubation::proposal_store::IncubationProposalStore>,
    agent_registry: Res<crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry>,
    settings: Res<crate::app::HarnessSettings>,
    requests: Query<(Entity, &ExperienceWritebackRequestMessage)>,
) {
    for (entity, request) in &requests {
        let decision = &request.decision;
        let candidate_id = decision.candidate_id;

        let candidate = match store.candidates.get(&candidate_id).cloned() {
            Some(c) => c,
            None => {
                debug!(
                    event = "ExperienceWritebackCandidateNotFound",
                    candidate_id = %candidate_id,
                    "candidate not found for writeback, skipping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        };

        // 标记为 WritebackPending（若尚未被标记）
        if let Some(c) = store.candidates.get_mut(&candidate_id)
            && c.status != ExperienceCandidateStatus::WritebackPending
        {
            c.status = ExperienceCandidateStatus::WritebackPending;
        }

        debug!(
            event = "ExperienceWritebackStarted",
            candidate_id = %candidate_id,
            destination = ?decision.destination,
            "starting experience writeback"
        );

        let result = match decision.destination {
            ExperienceWritebackDestination::LongTermMemory => {
                writeback_to_long_term_memory(&candidate, &agents, &mut long_memories, &mut service)
            }
            ExperienceWritebackDestination::SkillPackage => {
                writeback_to_skill_package(&candidate, &agents, &asset_service)
            }
            ExperienceWritebackDestination::IncubationProposal => {
                // IncubationProposal 写回：执行孵化，创建新 Agent 记录
                writeback_incubation_proposal(
                    decision.source_task_id,
                    &mut store,
                    &proposal_store,
                    &agent_registry,
                    &mut service,
                    &asset_service,
                    &settings.0.agents_config_path,
                )
            }
            ExperienceWritebackDestination::Rejected => Ok(()),
        };

        match result {
            Ok(_) => {
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::Persisted;
                }
                debug!(
                    event = "ExperienceWritebackSucceeded",
                    candidate_id = %candidate_id,
                    destination = ?decision.destination,
                    "experience writeback succeeded"
                );
                info!(
                    event = "ExperienceWritebackCompleted",
                    candidate_id = %candidate_id,
                    destination = ?decision.destination,
                    "经验写回完成"
                );
            }
            Err(error) => {
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::WritebackFailed;
                }
                warn!(
                    event = "ExperienceWritebackFailed",
                    candidate_id = %candidate_id,
                    destination = ?decision.destination,
                    error = %error,
                    "experience writeback failed"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

fn writeback_to_long_term_memory(
    candidate: &crate::domain::ExperienceCandidate,
    agents: &Query<&Agent>,
    long_memories: &mut Query<&mut LongTermMemory>,
    service: &mut LongTermMemoryService,
) -> Result<(), String> {
    let governing_agent_id = candidate
        .governing_agent_id
        .ok_or_else(|| "no governing_agent_id".to_string())?;
    let agent = agents
        .iter()
        .find(|a| a.id == governing_agent_id)
        .ok_or_else(|| format!("agent {} not found", governing_agent_id))?;

    let mut entry = candidate
        .as_long_term_memory_entry()
        .ok_or_else(|| "candidate cannot be converted to LTM entry".to_string())?;
    entry.source_candidate_id = Some(candidate.candidate_id);
    entry.source_task_id = Some(candidate.producer_task_id);
    entry.agent_id = Some(candidate.producer_agent_id);

    let mut memory = long_memories
        .iter_mut()
        .find(|lm| lm.agent_name.as_deref() == Some(&agent.profile.name))
        .ok_or_else(|| {
            format!(
                "no LongTermMemory component found for agent {}",
                agent.profile.name
            )
        })?;

    service
        .add_entry(&mut memory, entry)
        .map_err(|e| e.to_string())
}

fn writeback_to_skill_package(
    candidate: &crate::domain::ExperienceCandidate,
    agents: &Query<&Agent>,
    asset_service: &crate::infrastructure::assets::AgentAssetService,
) -> Result<(), String> {
    let governing_agent_id = candidate
        .governing_agent_id
        .ok_or_else(|| "no governing_agent_id".to_string())?;
    let agent = agents
        .iter()
        .find(|a| a.id == governing_agent_id)
        .ok_or_else(|| format!("agent {} not found", governing_agent_id))?;

    let crate::domain::ExperienceCandidatePayload::Skill {
        name,
        description,
        instructions,
        file_refs,
    } = &candidate.payload
    else {
        return Err("candidate payload is not skill".to_string());
    };

    let draft = crate::infrastructure::assets::SkillPackageDraft {
        skill_id: format!("{}", candidate.candidate_id),
        title: candidate.title.clone(),
        name: name.clone(),
        description: description.clone(),
        instructions: instructions.clone(),
        file_refs: file_refs.clone(),
        source_task_id: Some(candidate.producer_task_id),
        source_candidate_id: Some(candidate.candidate_id),
    };
    asset_service
        .persist_skill_package(&agent.profile.name, &draft)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn writeback_incubation_proposal(
    task_id: TaskId,
    store: &mut ExperienceStore,
    proposal_store: &crate::infrastructure::incubation::proposal_store::IncubationProposalStore,
    agent_registry: &crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry,
    service: &mut crate::infrastructure::memory::LongTermMemoryService,
    asset_service: &crate::infrastructure::assets::AgentAssetService,
    config_path: &str,
) -> Result<(), String> {
    // 按 task_id 查找任务级 proposal
    let proposal = store
        .proposals
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("no IncubationProposal found for task {}", task_id))?;

    let profile = proposal.proposed_agent_profile.clone();

    match proposal.status {
        crate::domain::IncubationProposalStatus::Executing => {
            debug!(
                event = "IncubationExecutionInProgress",
                task_id = %task_id,
                "incubation writeback already in progress"
            );
            return Ok(());
        }
        crate::domain::IncubationProposalStatus::Executed => {
            debug!(
                event = "IncubationExecutionAlreadyDone",
                task_id = %task_id,
                "incubation proposal already executed"
            );
            return Ok(());
        }
        crate::domain::IncubationProposalStatus::Approved => {
            // continue below
        }
        other => {
            return Err(format!(
                "incubation proposal for task {} is not approved (status: {:?})",
                task_id, other
            ));
        }
    }

    // 推进状态为 Executing
    if let Some(proposal) = store.proposals.get_mut(&task_id) {
        proposal.status = crate::domain::IncubationProposalStatus::Executing;
        proposal.updated_at = chrono::Utc::now();
    }

    debug!(
        event = "IncubationExecutionStarted",
        task_id = %task_id,
        "starting incubation writeback"
    );

    // 持久化 proposal
    let proposal = store
        .proposals
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("no IncubationProposal found for task {}", task_id))?;
    proposal_store
        .persist(&proposal)
        .map_err(|e| e.to_string())?;

    // 把知识候选写入目标 Agent 的 LTM
    let candidate_entries: Vec<crate::domain::LongTermMemoryEntry> = proposal
        .knowledge_candidate_ids
        .iter()
        .filter_map(|id| store.candidates.get(id))
        .filter_map(|candidate| {
            let mut entry = candidate.as_long_term_memory_entry()?;
            entry.source_candidate_id = Some(candidate.candidate_id);
            entry.source_task_id = Some(candidate.producer_task_id);
            entry.agent_id = Some(candidate.producer_agent_id);
            Some(entry)
        })
        .collect();

    if !candidate_entries.is_empty() {
        let mut memory = crate::domain::LongTermMemory::with_name(profile.name.clone());
        memory.entries = service.load_entries(&profile.name);
        for entry in candidate_entries {
            service
                .add_entry(&mut memory, entry)
                .map_err(|e| e.to_string())?;
        }
    }

    // 处理 Skill 候选
    let mut skill_paths: Vec<String> = Vec::new();
    for skill_id in &proposal.skill_candidate_ids {
        if let Some(candidate) = store.candidates.get(skill_id)
            && let crate::domain::ExperienceCandidatePayload::Skill {
                name,
                description,
                instructions,
                file_refs,
            } = &candidate.payload
        {
            let draft = crate::infrastructure::assets::SkillPackageDraft {
                skill_id: format!("{}", candidate.candidate_id),
                title: candidate.title.clone(),
                name: name.clone(),
                description: description.clone(),
                instructions: instructions.clone(),
                file_refs: file_refs.clone(),
                source_task_id: Some(candidate.producer_task_id),
                source_candidate_id: Some(candidate.candidate_id),
            };
            match asset_service.persist_skill_package(&profile.name, &draft) {
                Ok(path) => skill_paths.push(path),
                Err(e) => {
                    tracing::warn!(
                        event = "IncubationSkillPersistFailed",
                        skill_id = %skill_id,
                        error = %e,
                        "failed to persist skill package during incubation"
                    );
                }
            }
        }
    }

    // 创建新 Agent 记录
    let description = build_incubated_agent_description(store, &proposal);
    let record = crate::infrastructure::incubation::agent_registry::IncubatedAgentRecord {
        name: profile.name.clone(),
        model: profile.model.clone(),
        models: vec![],
        tags: vec!["incubated".to_string()],
        description,
        tools: None,
        skills: if skill_paths.is_empty() {
            None
        } else {
            Some(skill_paths)
        },
    };
    let result = agent_registry
        .append(config_path, &record)
        .map_err(|e| e.to_string());

    match result {
        Ok(()) => {
            if let Some(proposal) = store.proposals.get_mut(&task_id) {
                proposal.status = crate::domain::IncubationProposalStatus::Executed;
                proposal.updated_at = chrono::Utc::now();
            }
            debug!(
                event = "IncubationExecutionSucceeded",
                task_id = %task_id,
                "incubation writeback succeeded"
            );
            Ok(())
        }
        Err(e) => {
            if let Some(proposal) = store.proposals.get_mut(&task_id) {
                proposal.status = crate::domain::IncubationProposalStatus::ExecutionFailed;
                proposal.updated_at = chrono::Utc::now();
            }
            warn!(
                event = "IncubationExecutionFailed",
                task_id = %task_id,
                error = %e,
                "incubation writeback failed"
            );
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentProfile, ExperienceCandidate, ExperienceStore, IncubationProposalStatus,
    };
    use crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry;
    use crate::infrastructure::incubation::proposal_store::IncubationProposalStore;
    use crate::infrastructure::memory::{
        JsonFileMemoryStore, LongTermMemoryService, MemoryRepository,
    };
    use tempfile::TempDir;

    fn make_memory_service(dir: &TempDir) -> LongTermMemoryService {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        LongTermMemoryService::new(MemoryRepository::new(Box::new(store)))
    }

    #[test]
    fn description_builds_from_candidate_titles() {
        let mut store = crate::domain::ExperienceStore::default();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let c1 = crate::domain::ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "公式推导".to_string(),
            "content1".to_string(),
        );
        let c2 = crate::domain::ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "数值验证".to_string(),
            "content2".to_string(),
        );
        store.stage_root_candidate(c1.clone());
        store.stage_root_candidate(c2.clone());

        let profile = crate::domain::AgentProfile {
            name: "incubated-test".to_string(),
            model: "test".to_string(),
        };
        store.merge_into_proposal(task_id, agent_id, profile.clone(), &c1);
        store.merge_into_proposal(task_id, agent_id, profile.clone(), &c2);
        let proposal = store.proposals.get(&task_id).unwrap().clone();

        let description = build_incubated_agent_description(&store, &proposal);
        assert_eq!(description, "基于 2 条经验孵化：公式推导；数值验证");
    }

    #[test]
    fn incubation_writeback_persists_knowledge_to_ltm_and_agents_toml() {
        let memory_dir = TempDir::new().unwrap();
        let proposal_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let asset_dir = TempDir::new().unwrap();
        let config_path = config_dir.path().join("agents.toml");

        let mut memory_service = make_memory_service(&memory_dir);
        let proposal_store = IncubationProposalStore::new(proposal_dir.path().join("proposals"));
        let registry = IncubatedAgentRegistry;
        let asset_service =
            crate::infrastructure::assets::AgentAssetService::new(asset_dir.path().join("agents"));

        let mut store = ExperienceStore::default();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "天体表面重力加速度计算流程".to_string(),
            "使用万有引力公式 g = G·M/R²".to_string(),
        );
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate.clone());

        let profile = AgentProfile {
            name: "incubated-test-flow".to_string(),
            model: "gpt-4.1-mini".to_string(),
        };
        store.merge_into_proposal(task_id, agent_id, profile.clone(), &candidate);
        store.proposals.get_mut(&task_id).unwrap().status = IncubationProposalStatus::Approved;

        let result = writeback_incubation_proposal(
            task_id,
            &mut store,
            &proposal_store,
            &registry,
            &mut memory_service,
            &asset_service,
            config_path.to_str().unwrap(),
        );

        assert!(result.is_ok(), "writeback failed: {:?}", result);

        let loaded = memory_service.load_entries(&profile.name);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "使用万有引力公式 g = G·M/R²");
        assert_eq!(loaded[0].source_candidate_id, Some(candidate_id));

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::domain::AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].name, profile.name);
        assert_eq!(config.agent[0].model, Some(profile.model));
        assert_eq!(config.agent[0].description, "天体表面重力加速度计算流程");
    }
}
