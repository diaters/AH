use bevy::prelude::*;
use tracing::{debug, warn};

use crate::domain::{
    Agent, AgentExecutionRequest, AgentRequestKind, ConfirmationOption, ConfirmationSource,
    ExperienceCandidateStatus, ExperienceConfirmationPolicy, ExperienceGovernanceDecision,
    ExperienceGovernanceRequestMessage, ExperienceKindHint, ExperienceRiskLevel,
    ExperienceWritebackDestination, ExperienceWritebackRequestMessage, IncubationProposalStatus,
    LongTermMemory, TaskId,
    ToolConfirmationRequestMessage, ToolConfirmationResponseMessage, ToolExecutionRequestMessage,
};
use crate::infrastructure::memory::LongTermMemoryService;

/// 经验治理系统：顶层唯一最终分流点。
///
/// 治理只负责"决定去向"，产出治理决议。不直接写盘。
/// 决议产出后：若无需确认则进入 WritebackPending，若需确认则进入 NeedsUserApproval。
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    agents: Query<&Agent>,
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

            let decision = match candidate.kind_hint {
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
                    continue;
                }
                ExperienceKindHint::SharedKnowledge => {
                    let confirmation_policy = if candidate.risk_level == ExperienceRiskLevel::High {
                        ExperienceConfirmationPolicy::User
                    } else {
                        ExperienceConfirmationPolicy::None
                    };
                    ExperienceGovernanceDecision {
                        candidate_id: *candidate_id,
                        destination: ExperienceWritebackDestination::SharedKnowledgeUpgrade,
                        confirmation_policy,
                        final_risk_level: candidate.risk_level,
                        risk_overridden: false,
                        decision_rationale: "shared knowledge candidate".to_string(),
                        source_task_id: request.task_id,
                    }
                }
                ExperienceKindHint::Executable => {
                    if is_default {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::IncubationProposal,
                            confirmation_policy: ExperienceConfirmationPolicy::User,
                            final_risk_level: candidate.risk_level,
                            risk_overridden: false,
                            decision_rationale: "default agent executable -> incubation"
                                .to_string(),
                            source_task_id: request.task_id,
                        }
                    } else {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::SkillPackage,
                            confirmation_policy: ExperienceConfirmationPolicy::User,
                            final_risk_level: candidate.risk_level,
                            risk_overridden: false,
                            decision_rationale: "executable requires user confirmation".to_string(),
                            source_task_id: request.task_id,
                        }
                    }
                }
                ExperienceKindHint::Knowledge => {
                    if is_default {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::IncubationProposal,
                            confirmation_policy: ExperienceConfirmationPolicy::User,
                            final_risk_level: candidate.risk_level,
                            risk_overridden: false,
                            decision_rationale: "default agent knowledge -> incubation".to_string(),
                            source_task_id: request.task_id,
                        }
                    } else {
                        let confirmation_policy =
                            if candidate.risk_level == ExperienceRiskLevel::High {
                                ExperienceConfirmationPolicy::User
                            } else {
                                ExperienceConfirmationPolicy::None
                            };
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::LongTermMemory,
                            confirmation_policy,
                            final_risk_level: candidate.risk_level,
                            risk_overridden: false,
                            decision_rationale: "persistent agent private knowledge".to_string(),
                            source_task_id: request.task_id,
                        }
                    }
                }
            };

            // 标记候选为 GovernanceResolved
            if let Some(c) = store.candidates.get_mut(candidate_id) {
                c.status = ExperienceCandidateStatus::GovernanceResolved;
            }

            debug!(
                event = "ExperienceGovernanceResolved",
                candidate_id = %candidate_id,
                task_id = %request.task_id,
                destination = ?decision.destination,
                confirmation_policy = ?decision.confirmation_policy,
                "governance decision made"
            );

            match decision.confirmation_policy {
                ExperienceConfirmationPolicy::None => {
                    // 无需确认，直接进入 WritebackPending
                    if let Some(c) = store.candidates.get_mut(candidate_id) {
                        c.status = ExperienceCandidateStatus::WritebackPending;
                    }
                    commands.spawn(ExperienceWritebackRequestMessage {
                        decision: decision.clone(),
                    });
                }
                ExperienceConfirmationPolicy::User => {
                    // 需要用户确认
                    if let Some(c) = store.candidates.get_mut(candidate_id) {
                        c.status = ExperienceCandidateStatus::NeedsUserApproval;
                    }
                    // 对于 IncubationProposal 目标，生成 proposal
                    if decision.destination == ExperienceWritebackDestination::IncubationProposal {
                        spawn_incubation_confirmation(
                            &mut commands,
                            &mut store,
                            request,
                            agent,
                            candidate_id,
                        );
                    } else {
                        spawn_experience_confirmation(
                            &mut commands,
                            &mut store,
                            request,
                            candidate_id,
                            &candidate,
                        );
                    }
                    // 暂存决策，等确认后使用
                    commands.spawn(decision);
                }
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 统一写回执行系统：根据治理决议执行正式写回。
#[allow(clippy::too_many_arguments)]
pub(crate) fn experience_writeback_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    mut service: ResMut<LongTermMemoryService>,
    asset_service: Res<crate::infrastructure::assets::AgentAssetService>,
    mut upgrade_queue: ResMut<crate::domain::SharedKnowledgeUpgradeQueue>,
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
    upgrade_queue: &mut crate::domain::SharedKnowledgeUpgradeQueue,
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
    store: &mut crate::domain::ExperienceStore,
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

fn is_default_agent(agent: &Agent) -> bool {
    agent.capabilities.tags.iter().any(|t| t == "default")
}

fn spawn_experience_confirmation(
    commands: &mut Commands,
    store: &mut crate::domain::ExperienceStore,
    request: &ExperienceGovernanceRequestMessage,
    candidate_id: &uuid::Uuid,
    candidate: &crate::domain::ExperienceCandidate,
) {
    let request_id = uuid::Uuid::new_v4();
    store.bind_approval_request(request_id, *candidate_id);
    debug!(
        event = "ExperienceApprovalBound",
        request_id = %request_id,
        candidate_id = %candidate_id,
        "bound approval request to candidate"
    );

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

    // 配对 ToolExecutionRequestMessage 占位实体，使 tool_confirmation_result_system
    // 能通过 pending_confirmation_id 找到匹配，不提前销毁 ToolConfirmationResponseMessage。
    commands.spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: request.task_id,
            agent_id: request.agent_id,
            request_kind: AgentRequestKind::ToolExecution {
                tool_name: "experience_governance".to_string(),
            },
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "experience_governance".to_string(),
        tool_input: serde_json::json!({
            "candidate_id": candidate_id.to_string(),
        }),
        pending_confirmation_id: Some(request_id),
        tool_call_id: None,
        pending_confirmation_options: Some(ConfirmationOption::default_options()),
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
        // 合并到任务级提案（查找已有或新建）
        store.merge_into_proposal(
            request.task_id,
            request.agent_id,
            crate::domain::AgentProfile {
                name: format!("incubated-{}", request.task_id),
                model: agent.profile.model.clone(),
            },
            &candidate,
        );

        spawn_experience_confirmation(commands, store, request, candidate_id, &candidate);
    }
}

/// 经验确认结果系统：处理用户对经验候选的确认，触发统一写回。
///
/// 审批只负责"放行"，不直接写盘。批准后将候选置为 WritebackPending 并
/// 查找之前暂存的治理决议，生成写回请求。
pub(crate) fn experience_approval_result_system(
    mut commands: Commands,
    mut store: ResMut<crate::domain::ExperienceStore>,
    pending_decisions: Query<(Entity, &ExperienceGovernanceDecision)>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
) {
    for (entity, response) in &responses {
        let candidate_id = match store
            .apply_confirmation_response_precise(response.request_id, &response.selected_option)
        {
            Some(id) => id,
            None => {
                debug!(
                    event = "ExperienceApprovalBindingNotFound",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    "no candidate bound to approval request, skipping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        };

        let approved = matches!(
            response.selected_option.as_str(),
            "allow_once" | "allow_always" | "approve"
        );

        if approved {
            // 查找暂存的治理决议
            let decision = pending_decisions
                .iter()
                .find(|(_, d)| d.candidate_id == candidate_id)
                .map(|(e, d)| (e, d.clone()));

            if let Some((decision_entity, decision)) = decision {
                // 标记候选为 WritebackPending
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::WritebackPending;
                }

                // 对于 IncubationProposal 目标，检查 proposal 状态做源头去重
                if decision.destination == ExperienceWritebackDestination::IncubationProposal {
                    let task_id = Some(decision.source_task_id);

                    // 先读取 proposal 状态（不可变借用），再根据结果做可变操作
                    let proposal_status = task_id
                        .as_ref()
                        .and_then(|tid| store.proposals.get(tid))
                        .map(|p| p.status);

                    match proposal_status {
                        Some(IncubationProposalStatus::Approved)
                        | Some(IncubationProposalStatus::Executing) => {
                            // 已有写回请求在途，候选等待完成
                            if let Some(c) = store.candidates.get_mut(&candidate_id) {
                                c.status = ExperienceCandidateStatus::WritebackPending;
                            }
                            debug!(
                                event = "ExperienceApprovalDeduplicated",
                                candidate_id = %candidate_id,
                                proposal_status = ?proposal_status,
                                "proposal already has writeback in progress, skipping"
                            );
                            commands.entity(decision_entity).despawn();
                            commands.entity(entity).despawn();
                            continue;
                        }
                        Some(IncubationProposalStatus::Executed) => {
                            // 已写回完成，候选直接标记为 Persisted
                            if let Some(c) = store.candidates.get_mut(&candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                            debug!(
                                event = "ExperienceApprovalDeduplicated",
                                candidate_id = %candidate_id,
                                proposal_status = ?proposal_status,
                                "proposal already executed, marking candidate as persisted"
                            );
                            commands.entity(decision_entity).despawn();
                            commands.entity(entity).despawn();
                            continue;
                        }
                        _ => {}
                    }

                    // 首次审批：设置 proposal 为 Approved
                    if let Some(task_id) = task_id
                        && let Some(proposal) = store.proposals.get_mut(&task_id)
                    {
                        proposal.status = IncubationProposalStatus::Approved;
                        proposal.updated_at = chrono::Utc::now();
                    }
                }

                // 生成写回请求
                commands.spawn(ExperienceWritebackRequestMessage {
                    decision: decision.clone(),
                });
                commands.entity(decision_entity).despawn();

                debug!(
                    event = "ExperienceApprovalResolved",
                    candidate_id = %candidate_id,
                    destination = ?decision.destination,
                    "approval resolved, spawning writeback request"
                );
            } else {
                // 没有找到暂存的决议（可能是旧路径），直接标记
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::WritebackPending;
                }
                debug!(
                    event = "ExperienceApprovalNoDecision",
                    candidate_id = %candidate_id,
                    "approved but no pending governance decision found"
                );
            }
        } else {
            // 用户拒绝
            if let Some(c) = store.candidates.get_mut(&candidate_id) {
                c.status = ExperienceCandidateStatus::Rejected;
            }
            // 清理暂存的决议
            if let Some((decision_entity, _)) = pending_decisions
                .iter()
                .find(|(_, d)| d.candidate_id == candidate_id)
            {
                commands.entity(decision_entity).despawn();
            }
            // 拒绝孵化提案中的相关候选
            let task_id = store
                .candidates
                .get(&candidate_id)
                .map(|c| c.producer_task_id);
            if let Some(task_id) = task_id
                && let Some(proposal) = store.proposals.get_mut(&task_id)
            {
                proposal.status = IncubationProposalStatus::Rejected;
                proposal.updated_at = chrono::Utc::now();
            }
            debug!(
                event = "ExperienceCandidateRejected",
                request_id = %response.request_id,
                candidate_id = %candidate_id,
                "user rejected experience candidate"
            );
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let request_id = uuid::Uuid::new_v4();
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
            risk_level: crate::domain::ExperienceRiskLevel::default(),
            risk_reason: String::new(),
            suggested_confirmation: crate::domain::ExperienceConfirmationPolicy::default(),
            derived_from_candidate_ids: vec![],
        };
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);
        store.apply_confirmation_response(request_id, "approve");

        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::Approved,
            "approved executable should be marked Approved"
        );
    }
}
