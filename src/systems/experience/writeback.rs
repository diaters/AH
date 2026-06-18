use bevy::prelude::*;
use tracing::{debug, warn};

use crate::domain::{
    Agent, ExperienceCandidateStatus, ExperienceStore, ExperienceWritebackDestination,
    ExperienceWritebackRequestMessage, LongTermMemory, SharedKnowledgeUpgradeQueue, TaskId,
};
use crate::infrastructure::memory::LongTermMemoryService;

/// 统一写回执行系统：根据治理决议执行正式写回。
#[allow(clippy::too_many_arguments)]
pub(crate) fn experience_writeback_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    asset_service: Res<crate::infrastructure::assets::AgentAssetService>,
    mut upgrade_queue: ResMut<SharedKnowledgeUpgradeQueue>,
    upgrade_service: Res<crate::infrastructure::memory::SharedKnowledgeUpgradeService>,
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
            ExperienceWritebackDestination::SharedKnowledgeUpgrade => {
                writeback_to_shared_knowledge_upgrade(
                    &candidate,
                    &mut upgrade_queue,
                    &upgrade_service,
                )
            }
            ExperienceWritebackDestination::IncubationProposal => {
                // IncubationProposal 写回：执行孵化，创建新 Agent 记录
                writeback_incubation_proposal(
                    decision.source_task_id,
                    &mut store,
                    &proposal_store,
                    &agent_registry,
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

    let crate::domain::ExperienceCandidatePayload::Executable {
        intent,
        when_to_use,
        asset_refs,
    } = &candidate.payload
    else {
        return Err("candidate payload is not executable".to_string());
    };

    let draft = crate::infrastructure::assets::SkillPackageDraft {
        skill_id: format!("{}", candidate.candidate_id),
        title: candidate.title.clone(),
        problem: intent.clone(),
        when_to_use: when_to_use.clone(),
        steps: "参见 skill.md 与 scripts/ 目录".to_string(),
        asset_refs: asset_refs.clone(),
        dependency_refs: candidate.dependency_refs.clone(),
        risks: candidate.risk_reason.clone(),
        source_task_id: Some(candidate.producer_task_id),
        source_candidate_id: Some(candidate.candidate_id),
    };
    asset_service
        .persist_skill_package(&agent.profile.name, &draft)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn writeback_to_shared_knowledge_upgrade(
    candidate: &crate::domain::ExperienceCandidate,
    upgrade_queue: &mut SharedKnowledgeUpgradeQueue,
    upgrade_service: &crate::infrastructure::memory::SharedKnowledgeUpgradeService,
) -> Result<(), String> {
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
    upgrade_service
        .persist(upgrade_queue)
        .map_err(|e| e.to_string())
}

fn writeback_incubation_proposal(
    task_id: TaskId,
    store: &mut ExperienceStore,
    proposal_store: &crate::infrastructure::incubation::proposal_store::IncubationProposalStore,
    agent_registry: &crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry,
    config_path: &str,
) -> Result<(), String> {
    // 按 task_id 查找任务级 proposal
    let proposal = store
        .proposals
        .get(&task_id)
        .cloned()
        .ok_or_else(|| format!("no IncubationProposal found for task {}", task_id))?;

    let profile = proposal.proposed_agent_profile.clone();
    let rationale = proposal.incubation_rationale.clone();

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

    // 创建新 Agent 记录
    let record = crate::infrastructure::incubation::agent_registry::IncubatedAgentRecord {
        name: profile.name.clone(),
        model: profile.model.clone(),
        tags: vec!["incubated".to_string()],
        description: rationale,
        tools: None,
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
