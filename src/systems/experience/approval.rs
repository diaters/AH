use bevy::prelude::*;
use tracing::debug;

use crate::domain::{
    ExperienceCandidateStatus, ExperienceGovernanceDecision, ExperienceStore,
    ExperienceWritebackDestination, ExperienceWritebackRequestMessage, IncubationProposalStatus,
    ToolConfirmationResponseMessage,
};

/// 经验确认结果系统：处理用户对经验候选的确认，触发统一写回。
///
/// 审批只负责"放行"，不直接写盘。批准后将候选置为 WritebackPending 并
/// 查找之前暂存的治理决议，生成写回请求。
pub(crate) fn experience_approval_result_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
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
    use crate::domain::{
        ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
        ExperienceConfirmationPolicy, ExperienceKindHint, ExperienceStore,
    };

    #[test]
    fn approved_executable_becomes_persisted() {
        let mut store = ExperienceStore::default();
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
            suggested_confirmation: ExperienceConfirmationPolicy::default(),
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
