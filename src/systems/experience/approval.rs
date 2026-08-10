use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    ExperienceCandidateStatus, ExperienceGovernanceDecision, ExperienceStore,
    ExperienceWritebackDestination, ExperienceWritebackRequestMessage, IncubationProposalStatus,
    PendingExperienceHooks, ProfileGenerationContext, ProfileGenerationRequestMessage,
    ToolConfirmationResponseMessage, WorkItem,
};
use crate::user_plugins::hook_point::HookPoint;

/// 经验确认结果系统：处理用户对经验候选的确认，触发统一写回。
///
/// 审批只负责"放行"，不直接写盘。批准后将候选置为 WritebackPending 并
/// 查找之前暂存的治理决议，生成写回请求。
pub(crate) fn experience_approval_result_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut pending_hooks: ResMut<PendingExperienceHooks>,
    pending_decisions: Query<(Entity, &ExperienceGovernanceDecision)>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
    profile_contexts: Query<(Entity, &ProfileGenerationContext, &WorkItem)>,
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
            // 推入待派发队列，由 companion 系统触发 on_experience_candidate_approved hook。
            pending_hooks
                .0
                .push((HookPoint::OnExperienceCandidateApproved, candidate_id));

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
            // 检测 reject_with_feedback：在通用拒绝处理之前拦截，触发 LLM 重新生成。
            // 注意：reject_with_feedback 不再受 exception_count 上限约束。
            // exception_count 仅累计 LLM 异常（未调工具 / 互斥冲突 / Err），
            // 用户反馈属于正常交互，透传 exception_count 不变。
            if response.selected_option == "reject_with_feedback"
                && let Some(feedback) = response.feedback.as_ref()
                && let Some(task_id) = store
                    .candidates
                    .get(&candidate_id)
                    .map(|c| c.producer_task_id)
                && let Some(ctx) = profile_contexts
                    .iter()
                    .find(|(_, _, wi)| wi.task_id == task_id)
                    .map(|(_, ctx, _)| ctx.clone())
            {
                // 候选回到 ProfileGenerationPending，等待重新生成
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.status = ExperienceCandidateStatus::ProfileGenerationPending;
                }

                // 收集该任务所有 ProfileGenerationPending 候选
                let candidate_ids: Vec<uuid::Uuid> = store
                    .candidates
                    .values()
                    .filter(|c| {
                        c.status == ExperienceCandidateStatus::ProfileGenerationPending
                            && c.producer_task_id == task_id
                    })
                    .map(|c| c.candidate_id)
                    .collect();

                let agent_id = store
                    .candidates
                    .get(&candidate_id)
                    .map(|c| c.producer_agent_id)
                    .unwrap_or_default();

                // Spawn 重新生成请求，exception_count 透传不变（用户反馈不是 LLM 异常）
                commands.spawn(ProfileGenerationRequestMessage {
                    task_id,
                    agent_id,
                    candidate_ids,
                    existing_profile: ctx.existing_profile.clone(),
                    kind: ctx.kind.clone(),
                    feedback: Some(feedback.clone()),
                    exception_count: ctx.exception_count,
                });

                debug!(
                    event = "ProfileRegenerationRequested",
                    task_id = %task_id,
                    candidate_id = %candidate_id,
                    exception_count = ctx.exception_count,
                    "user rejected with feedback, spawning regeneration request (exception_count preserved)"
                );

                commands.entity(entity).despawn();
                continue;
            }

            // 通用拒绝处理
            if let Some(c) = store.candidates.get_mut(&candidate_id) {
                c.status = ExperienceCandidateStatus::Rejected;
            }

            // 推入待派发队列，由 companion 系统触发 on_experience_candidate_rejected hook。
            pending_hooks
                .0
                .push((HookPoint::OnExperienceCandidateRejected, candidate_id));

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
    use crate::domain::{
        ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
        ExperienceKindHint, ExperienceStore, PendingExperienceHooks, ProfileGenerationContext,
        ProfileGenerationKind, ProfileGenerationRequestMessage, ToolConfirmationResponseMessage,
        WorkItem,
    };
    use bevy_ecs::system::RunSystemOnce;

    #[test]
    fn approved_skill_becomes_persisted() {
        let mut store = ExperienceStore::default();
        let request_id = uuid::Uuid::new_v4();
        let candidate = ExperienceCandidate {
            candidate_id: uuid::Uuid::new_v4(),
            producer_task_id: uuid::Uuid::new_v4(),
            producer_agent_id: uuid::Uuid::new_v4(),
            title: "test skill".to_string(),
            kind_hint: ExperienceKindHint::Skill,
            payload: ExperienceCandidatePayload::Skill {
                name: "test-skill".to_string(),
                description: "run smoke test".to_string(),
                instructions: "1. Run test".to_string(),
                file_refs: vec![],
                is_new: false,
            },
            dependency_refs: vec![],
            status: ExperienceCandidateStatus::NeedsUserApproval,
            governing_agent_id: None,
            derived_from_candidate_ids: vec![],
        };
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);
        store.apply_confirmation_response(request_id, "approve");

        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::Approved,
            "approved skill should be marked Approved"
        );
    }

    /// 构建测试用 World：插入 ExperienceStore 和 PendingExperienceHooks 资源。
    fn make_test_world(store: ExperienceStore) -> World {
        let mut world = World::new();
        world.insert_resource(store);
        world.insert_resource(PendingExperienceHooks::default());
        world
    }

    /// 构建测试用候选：处于 NeedsUserApproval 状态。
    fn make_test_candidate(task_id: uuid::Uuid, agent_id: uuid::Uuid) -> ExperienceCandidate {
        ExperienceCandidate {
            candidate_id: uuid::Uuid::new_v4(),
            producer_task_id: task_id,
            producer_agent_id: agent_id,
            title: "test knowledge".to_string(),
            kind_hint: ExperienceKindHint::Knowledge,
            payload: ExperienceCandidatePayload::Knowledge {
                content: "test content".to_string(),
            },
            dependency_refs: vec![],
            status: ExperienceCandidateStatus::NeedsUserApproval,
            governing_agent_id: None,
            derived_from_candidate_ids: vec![],
        }
    }

    #[test]
    fn reject_with_feedback_spawns_regeneration_request() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let request_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id);
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);

        let mut world = make_test_world(store);
        // 通过 spawn WorkItem + ProfileGenerationContext Component 注入上下文（exception_count = 0）
        world.spawn((
            WorkItem::profile_generation(
                task_id,
                String::new(),
                vec![],
                vec![],
                uuid::Uuid::nil(),
                ProfileGenerationKind::Incubation,
            ),
            ProfileGenerationContext {
                kind: ProfileGenerationKind::Incubation,
                exception_count: 0,
                existing_profile: None,
                generated_profile: None,
            },
        ));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "reject_with_feedback".to_string(),
            feedback: Some("name 太长了".to_string()),
        });

        world
            .run_system_once(experience_approval_result_system)
            .unwrap();

        // 验证：spawn 了 ProfileGenerationRequestMessage
        let regen_msgs: Vec<&ProfileGenerationRequestMessage> = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .collect();
        assert_eq!(regen_msgs.len(), 1, "should spawn one regeneration request");
        let regen = regen_msgs[0];
        assert_eq!(regen.task_id, task_id);
        assert_eq!(
            regen.exception_count, 0,
            "exception_count should be preserved (not incremented) for reject_with_feedback"
        );
        assert_eq!(regen.kind, ProfileGenerationKind::Incubation);
        assert_eq!(
            regen.feedback.as_deref(),
            Some("name 太长了"),
            "feedback should be propagated"
        );
        assert!(
            regen.candidate_ids.contains(&candidate_id),
            "candidate_ids should include the original candidate"
        );

        // 验证：候选回到 ProfileGenerationPending
        let store = world.resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::ProfileGenerationPending,
            "candidate should be back to ProfileGenerationPending"
        );
    }

    #[test]
    fn reject_with_feedback_preserves_exception_count() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let request_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id);
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);

        let mut world = make_test_world(store);
        // 通过 spawn WorkItem + ProfileGenerationContext Component 注入上下文（exception_count = 2，模拟之前有 LLM 异常）
        world.spawn((
            WorkItem::profile_generation(
                task_id,
                String::new(),
                vec![],
                vec![],
                uuid::Uuid::nil(),
                ProfileGenerationKind::Incubation,
            ),
            ProfileGenerationContext {
                kind: ProfileGenerationKind::Incubation,
                exception_count: 2,
                existing_profile: None,
                generated_profile: None,
            },
        ));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "reject_with_feedback".to_string(),
            feedback: Some("tags 不够精确".to_string()),
        });

        world
            .run_system_once(experience_approval_result_system)
            .unwrap();

        let regen_msgs: Vec<&ProfileGenerationRequestMessage> = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .collect();
        assert_eq!(regen_msgs.len(), 1);
        assert_eq!(
            regen_msgs[0].exception_count, 2,
            "exception_count should be preserved at 2 (reject_with_feedback does not increment)"
        );
    }

    #[test]
    fn reject_with_feedback_always_allowed() {
        // 验证：即使 exception_count 很高，reject_with_feedback 仍触发重新生成
        // （exception_count 仅限制 LLM 异常重试，不限制用户反馈次数）
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let request_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id);
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);

        let mut world = make_test_world(store);
        // 通过 spawn WorkItem + ProfileGenerationContext Component 注入上下文（exception_count 已达上限值）
        world.spawn((
            WorkItem::profile_generation(
                task_id,
                String::new(),
                vec![],
                vec![],
                uuid::Uuid::nil(),
                ProfileGenerationKind::Incubation,
            ),
            ProfileGenerationContext {
                kind: ProfileGenerationKind::Incubation,
                exception_count: crate::domain::MAX_PROFILE_EXCEPTIONS,
                existing_profile: None,
                generated_profile: None,
            },
        ));
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "reject_with_feedback".to_string(),
            feedback: Some("still not good".to_string()),
        });

        world
            .run_system_once(experience_approval_result_system)
            .unwrap();

        // 验证：仍 spawn 了重新生成请求（不受 exception_count 上限约束）
        let regen_count = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            regen_count, 1,
            "reject_with_feedback should always be allowed regardless of exception_count"
        );

        // 验证：候选回到 ProfileGenerationPending（不是 Rejected）
        let store = world.resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::ProfileGenerationPending,
            "candidate should be back to ProfileGenerationPending, not Rejected"
        );
    }

    #[test]
    fn reject_with_feedback_without_context_falls_to_plain_reject() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        let request_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = make_test_candidate(task_id, agent_id);
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);

        // 不存入 profile 生成上下文（模拟上下文缺失）

        let mut world = make_test_world(store);
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "reject_with_feedback".to_string(),
            feedback: Some("feedback".to_string()),
        });

        world
            .run_system_once(experience_approval_result_system)
            .unwrap();

        // 验证：未 spawn 重新生成请求
        let regen_count = world
            .query::<&ProfileGenerationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            regen_count, 0,
            "should not spawn regeneration without context"
        );

        // 验证：候选被标记为 Rejected
        let store = world.resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::Rejected,
            "candidate should be Rejected when context is missing"
        );
    }
}
