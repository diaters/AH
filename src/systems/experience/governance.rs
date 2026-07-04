use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    Agent, AgentExecutionRequest, AgentRequestKind, ConfirmationOption, ConfirmationSource,
    ExperienceCandidate, ExperienceCandidateStatus, ExperienceGovernanceDecision,
    ExperienceGovernanceRequestMessage, ExperienceKindHint, ExperienceStore,
    ExperienceWritebackDestination, ExperienceWritebackRequestMessage, ToolCalledHookPending,
    ToolConfirmationRequestMessage, ToolExecutionRequestMessage,
};

/// 经验治理系统：顶层唯一最终分流点。
///
/// 治理只负责"决定去向"，产出治理决议。不直接写盘。
/// 决议产出后：若无需确认则进入 WritebackPending，若需确认则进入 NeedsUserApproval。
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
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
                ExperienceKindHint::Knowledge => {
                    if is_default {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::IncubationProposal,
                            requires_user_confirmation: true,
                            decision_rationale: "default agent knowledge -> incubation".to_string(),
                            source_task_id: request.task_id,
                        }
                    } else {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::LongTermMemory,
                            requires_user_confirmation: false,
                            decision_rationale: "persistent agent private knowledge".to_string(),
                            source_task_id: request.task_id,
                        }
                    }
                }
                ExperienceKindHint::Skill => {
                    if is_default {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::IncubationProposal,
                            requires_user_confirmation: true,
                            decision_rationale: "default agent skill -> incubation".to_string(),
                            source_task_id: request.task_id,
                        }
                    } else {
                        ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::SkillPackage,
                            requires_user_confirmation: true,
                            decision_rationale: "skill requires user confirmation".to_string(),
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
                requires_user_confirmation = decision.requires_user_confirmation,
                "governance decision made"
            );

            if decision.requires_user_confirmation {
                // 需要用户确认
                if let Some(c) = store.candidates.get_mut(candidate_id) {
                    c.status = ExperienceCandidateStatus::NeedsUserApproval;
                }
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
                commands.spawn(decision);
            } else {
                // 无需确认，直接进入 WritebackPending
                if let Some(c) = store.candidates.get_mut(candidate_id) {
                    c.status = ExperienceCandidateStatus::WritebackPending;
                }
                commands.spawn(ExperienceWritebackRequestMessage {
                    decision: decision.clone(),
                });
            }
        }

        commands.entity(entity).despawn();
    }
}

pub(crate) fn is_default_agent(agent: &Agent) -> bool {
    agent.capabilities.tags.iter().any(|t| t == "default")
}

fn spawn_experience_confirmation(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    request: &ExperienceGovernanceRequestMessage,
    candidate_id: &uuid::Uuid,
    candidate: &ExperienceCandidate,
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
    // 附带 ToolCalledHookPending 标记以对称参与 on_tool_called hook 派发；companion
    // 系统仅在不被拒绝时移除标记，横切到所有工具请求 spawn 点。
    commands.spawn((
        ToolCalledHookPending,
        ToolExecutionRequestMessage {
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
        },
    ));
}

fn spawn_incubation_confirmation(
    commands: &mut Commands,
    store: &mut ExperienceStore,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentCapabilities, AgentKind, AgentProfile};

    #[test]
    fn is_default_agent_detects_by_tag_not_name() {
        let default_agent = Agent {
            id: uuid::Uuid::new_v4(),
            profile: AgentProfile {
                name: "custom-default".to_string(),
                model: "test".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["default".to_string(), "llm".to_string()],
                description: "default agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions::default(),
        };

        assert!(is_default_agent(&default_agent));
    }
}
