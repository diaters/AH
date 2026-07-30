use crate::prelude::*;
use tracing::{debug, warn};

use crate::domain::{
    Agent, AgentExecutionRequest, AgentRequestKind, ConfirmationOption, ConfirmationSource,
    ExperienceCandidate, ExperienceCandidateStatus, ExperienceGovernanceDecision,
    ExperienceGovernanceRequestMessage, ExperienceKindHint, ExperienceStore,
    ExperienceWritebackDestination, ExperienceWritebackRequestMessage, SkillUpdateRequestMessage,
    Task, TaskInjectedSkill, ToolCalledHookPending, ToolConfirmationRequestMessage,
    ToolExecutionRequestMessage,
};
use crate::ecs::EntityIndex;
use crate::infrastructure::skills::SkillRegistry;

/// 经验治理系统：顶层唯一最终分流点。
///
/// 治理只负责"决定去向"，产出治理决议。不直接写盘。
/// 决议产出后：若无需确认则进入 WritebackPending，若需确认则进入 NeedsUserApproval。
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,
    tasks: Query<(&Task, Option<&TaskInjectedSkill>)>,
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    for (entity, request) in &requests {
        // 经 EntityIndex O(1) 解析 AgentId → Entity（替代全量线性扫描）
        let agent = match index
            .get_agent(&request.agent_id)
            .and_then(|e| agents.get(e).ok())
        {
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

            let decision: Option<ExperienceGovernanceDecision> = match candidate.kind_hint {
                ExperienceKindHint::Knowledge => {
                    let d = if is_default {
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
                    };
                    Some(d)
                }
                ExperienceKindHint::Skill => {
                    if is_default {
                        // 保留原 default agent skill → incubation 语义（修正 plan 疏漏）
                        Some(ExperienceGovernanceDecision {
                            candidate_id: *candidate_id,
                            destination: ExperienceWritebackDestination::IncubationProposal,
                            requires_user_confirmation: true,
                            decision_rationale: "default agent skill -> incubation".to_string(),
                            source_task_id: request.task_id,
                        })
                    } else {
                        // 非默认 agent：检查 task 是否注入了 skill，按 self_updatable 分流
                        // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                        let injected_skill = index
                            .get_task(&request.task_id)
                            .and_then(|e| tasks.get(e).ok())
                            .and_then(|(_, is)| is)
                            .and_then(|is| is.skill_id.clone());

                        if let Some(skill_id) = injected_skill.as_ref()
                            && let Some(entry) = skill_registry.get(skill_id)
                        {
                            if entry.self_updatable {
                                Some(ExperienceGovernanceDecision {
                                    candidate_id: *candidate_id,
                                    destination: ExperienceWritebackDestination::SkillUpdate,
                                    requires_user_confirmation: false,
                                    decision_rationale: format!(
                                        "self_updatable skill {} -> skill-updater",
                                        skill_id.as_string()
                                    ),
                                    source_task_id: request.task_id,
                                })
                            } else {
                                // 不可自更新的 skill 产生 Skill 经验：
                                // 不强行降级为 Knowledge（payload 形态不匹配，会导致 writeback 失败），
                                // 也不走 skill-updater（会自指循环）。
                                // 直接标记 Discarded 并记录 warn，让 LLM 在下一轮重新评估。
                                // 真正需要变更该 skill 的，应通过 IncubationProposal 提案新 skill。
                                warn!(
                                    event = "SkillCandidateDiscardedNotSelfUpdatable",
                                    task_id = %request.task_id,
                                    candidate_id = %candidate_id,
                                    skill_id = %skill_id.as_string(),
                                    error = "skill is not self_updatable, cannot route to skill-updater",
                                    error_type = "SkillNotSelfUpdatable",
                                    "skill candidate discarded; consider IncubationProposal for new skill"
                                );
                                if let Some(c) = store.candidates.get_mut(candidate_id) {
                                    c.status = ExperienceCandidateStatus::Discarded;
                                }
                                None
                            }
                        } else if let Some(skill_id) = injected_skill.as_ref() {
                            // skill 在 registry 中找不到：保守回退到 SkillPackage
                            debug!(
                                event = "GovernanceSkillNotFoundInRegistry",
                                task_id = %request.task_id,
                                candidate_id = %candidate_id,
                                skill_id = %skill_id.as_string(),
                                "skill not found in registry, fallback to SkillPackage"
                            );
                            Some(ExperienceGovernanceDecision {
                                candidate_id: *candidate_id,
                                destination: ExperienceWritebackDestination::SkillPackage,
                                requires_user_confirmation: true,
                                decision_rationale:
                                    "skill not in registry, fallback to SkillPackage".to_string(),
                                source_task_id: request.task_id,
                            })
                        } else {
                            // 未注入 skill：保留原 SkillPackage 逻辑
                            Some(ExperienceGovernanceDecision {
                                candidate_id: *candidate_id,
                                destination: ExperienceWritebackDestination::SkillPackage,
                                requires_user_confirmation: true,
                                decision_rationale: "skill requires user confirmation".to_string(),
                                source_task_id: request.task_id,
                            })
                        }
                    }
                }
            };

            let Some(decision) = decision else {
                // 已在分支内处理（如 Discarded），跳过后续决议处理
                continue;
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
            } else if decision.destination == ExperienceWritebackDestination::SkillUpdate {
                // SkillUpdate destination：spawn SkillUpdateRequestMessage，
                // 由 skill_update_workitem_system 消费构造 skill-updater WorkItem。
                // 候选状态保持 GovernanceResolved，等 skill_update_completion_system 完成后再置 Persisted。
                // 经 EntityIndex O(1) 解析 TaskId → Entity（替代全量线性扫描）
                let injected_skill = index
                    .get_task(&request.task_id)
                    .and_then(|e| tasks.get(e).ok())
                    .and_then(|(_, is)| is)
                    .and_then(|is| is.skill_id.clone());

                match injected_skill {
                    Some(skill_id) => {
                        debug!(
                            event = "SkillUpdateRequestSpawned",
                            task_id = %request.task_id,
                            candidate_id = %candidate_id,
                            skill_id = %skill_id.as_string(),
                            governing_agent_id = %request.agent_id,
                            "spawning SkillUpdateRequestMessage for skill-updater"
                        );
                        commands.spawn(SkillUpdateRequestMessage {
                            task_id: request.task_id,
                            skill_id,
                            experience_candidate_id: *candidate_id,
                            governing_agent_id: request.agent_id,
                        });
                    }
                    None => {
                        warn!(
                            event = "SkillUpdateDestinationMissingInjectedSkill",
                            task_id = %request.task_id,
                            candidate_id = %candidate_id,
                            error = "decision.destination == SkillUpdate but task has no TaskInjectedSkill",
                            error_type = "MissingInjectedSkill",
                            "cannot spawn SkillUpdateRequestMessage without injected skill, skipping"
                        );
                    }
                }
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
        approval_context: None,
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
                model_override: None,
            },
            tool_name: "experience_governance".to_string(),
            tool_input: serde_json::json!({
                "candidate_id": candidate_id.to_string(),
            }),
            pending_confirmation_id: Some(request_id),
            tool_call_id: None,
            pending_confirmation_options: Some(ConfirmationOption::default_options()),
            work_item_entity: None,
            confirmed_once: false,
        },
    ));
}

fn spawn_incubation_confirmation(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    request: &ExperienceGovernanceRequestMessage,
    _agent: &Agent,
    candidate_id: &uuid::Uuid,
) {
    // 将候选标记为 ProfileGenerationPending，等待 profile 生成完成后再发起审批
    if let Some(c) = store.candidates.get_mut(candidate_id) {
        c.status = ExperienceCandidateStatus::ProfileGenerationPending;
    }

    // 收集该任务所有 ProfileGenerationPending 候选，作为 LLM 输入
    let candidate_ids: Vec<uuid::Uuid> = store
        .candidates
        .values()
        .filter(|c| {
            c.status == ExperienceCandidateStatus::ProfileGenerationPending
                && c.producer_task_id == request.task_id
        })
        .map(|c| c.candidate_id)
        .collect();

    if candidate_ids.is_empty() {
        debug!(
            event = "IncubationConfirmationNoCandidates",
            task_id = %request.task_id,
            "no ProfileGenerationPending candidates, skipping profile generation request"
        );
        return;
    }

    // Spawn ProfileGenerationRequestMessage（孵化场景）
    // 实际 profile 由 profile-designer Agent 生成，由 profile_generation_completion_system
    // 创建 proposal 并发起审批
    commands.spawn(crate::domain::ProfileGenerationRequestMessage {
        task_id: request.task_id,
        agent_id: request.agent_id,
        candidate_ids,
        existing_profile: None,
        kind: crate::domain::ProfileGenerationKind::Incubation,
        feedback: None,
        exception_count: 0,
    });

    debug!(
        event = "IncubationProfileGenerationRequested",
        task_id = %request.task_id,
        agent_id = %request.agent_id,
        "spawned profile generation request for incubation"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ChannelId,
        ExperienceCandidate, ExperienceKindHint, FrontendKind, ProfileGenerationRequestMessage,
        TaskRoutingPolicy, TaskStatus,
    };
    use crate::ecs::EntityIndex;
    use crate::infrastructure::skills::{SkillEntry, SkillId};

    /// 构造测试用 Agent（tags 决定是否为 default agent）。
    fn make_agent(id: uuid::Uuid, name: &str, tags: &[&str]) -> Agent {
        Agent {
            id,
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: tags.iter().map(|t| t.to_string()).collect(),
                description: "test agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    /// 构造测试用 Task（仅填关键字段，task_id 由调用者指定）。
    fn make_task(task_id: crate::domain::TaskId) -> Task {
        Task {
            id: task_id,
            content: "test task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Done,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "test".to_string(),
                thread_id: None,
            }),
            routing_policy: TaskRoutingPolicy::event(None, None),
            last_evaluated_turn: None,
        }
    }

    /// 构造测试用 SkillEntry。
    fn make_skill_entry(skill_id: SkillId, self_updatable: bool) -> SkillEntry {
        SkillEntry {
            owner_agent_name: skill_id.owner_agent_name.clone(),
            skill_id,
            name: "test-skill".to_string(),
            description: "test desc".to_string(),
            instructions: "test instructions".to_string(),
            version: 1,
            self_updatable,
        }
    }

    /// 构造测试用 Skill 类候选，状态为 GovernancePending，producer_task_id 关联到指定 task。
    fn make_skill_candidate(
        candidate_id: uuid::Uuid,
        producer_task_id: crate::domain::TaskId,
        producer_agent_id: crate::domain::AgentId,
    ) -> ExperienceCandidate {
        let mut c = ExperienceCandidate::skill(
            candidate_id,
            producer_task_id,
            producer_agent_id,
            "test skill".to_string(),
            "test-skill".to_string(),
            "desc".to_string(),
            "instructions".to_string(),
            Vec::new(),
        );
        c.status = ExperienceCandidateStatus::GovernancePending;
        c
    }

    /// 构造最小化 Bevy App，仅注册 governance system 与必备 Resource。
    fn make_governance_app() -> App {
        let mut app = App::new();
        app.add_systems(Update, experience_governance_system);
        app.insert_resource(ExperienceStore::default());
        app.insert_resource(SkillRegistry::default());
        app.insert_resource(EntityIndex::default());
        app
    }

    /// 在 EntityIndex 中注册 agent。
    fn register_agent(app: &mut App, agent_id: crate::domain::AgentId, agent: Agent) {
        let entity = app.world_mut().spawn(agent).id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .agents
            .insert(agent_id, entity);
    }

    /// 在 EntityIndex 中注册 task。
    fn register_task(
        app: &mut App,
        task_id: crate::domain::TaskId,
        task: Task,
        injected_skill: Option<TaskInjectedSkill>,
    ) {
        let entity = if let Some(is) = injected_skill {
            app.world_mut().spawn((task, is)).id()
        } else {
            app.world_mut().spawn(task).id()
        };
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
    }

    /// 在 store 中插入候选并返回其 ID。
    fn stage_candidate(app: &mut App, candidate: ExperienceCandidate) -> uuid::Uuid {
        let id = candidate.candidate_id;
        app.world_mut()
            .resource_mut::<ExperienceStore>()
            .candidates
            .insert(id, candidate);
        id
    }

    /// 查询 world 中所有 ExperienceWritebackRequestMessage 的 destination。
    fn writeback_destinations(app: &mut App) -> Vec<ExperienceWritebackDestination> {
        let mut q = app
            .world_mut()
            .query::<&ExperienceWritebackRequestMessage>();
        q.iter(app.world())
            .map(|m| m.decision.destination)
            .collect()
    }

    /// 查询 world 中所有 ExperienceGovernanceDecision 的 destination（确认路径下 spawn）。
    fn governance_decision_destinations(app: &mut App) -> Vec<ExperienceWritebackDestination> {
        let mut q = app.world_mut().query::<&ExperienceGovernanceDecision>();
        q.iter(app.world()).map(|d| d.destination).collect()
    }

    /// 查询 ProfileGenerationRequestMessage 数量（用于 IncubationProposal 分支验证）。
    fn profile_generation_count(app: &mut App) -> usize {
        let mut q = app.world_mut().query::<&ProfileGenerationRequestMessage>();
        q.iter(app.world()).count()
    }

    #[test]
    fn is_default_agent_detects_by_tag_not_name() {
        let default_agent = make_agent(uuid::Uuid::new_v4(), "custom-default", &["default", "llm"]);
        assert!(is_default_agent(&default_agent));
    }

    /// 1. self_updatable=true 的注入 skill → SkillUpdate destination（无需用户确认）。
    #[test]
    fn governance_routes_self_updatable_skill_to_skill_update_destination() {
        let mut app = make_governance_app();

        let agent_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let candidate_id = uuid::Uuid::new_v4();
        let skill_id = SkillId::new("owner-agent", "test-skill");

        // 非默认 agent（经 EntityIndex 注册）
        register_agent(&mut app, agent_id, make_agent(agent_id, "worker", &["llm"]));
        // task 注入了 skill（经 EntityIndex 注册）
        register_task(
            &mut app,
            task_id,
            make_task(task_id),
            Some(TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            }),
        );
        // skill registry 中 skill self_updatable=true
        app.world_mut()
            .resource_mut::<SkillRegistry>()
            .upsert(make_skill_entry(skill_id, true));
        // Skill 类候选
        stage_candidate(
            &mut app,
            make_skill_candidate(candidate_id, task_id, agent_id),
        );
        // 触发治理
        app.world_mut()
            .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

        app.update();

        // SkillUpdate destination 改为 spawn SkillUpdateRequestMessage（由 skill_update_workitem_system 消费），
        // 候选状态保持 GovernanceResolved，不进入 WritebackPending。
        let store = app.world().resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::GovernanceResolved,
        );
        // 不应 spawn 任何 WritebackRequestMessage
        let destinations = writeback_destinations(&mut app);
        assert!(
            destinations.is_empty(),
            "SkillUpdate path should not spawn WritebackRequestMessage"
        );
        // 不应 spawn 任何 ExperienceGovernanceDecision（确认路径）
        let decisions = governance_decision_destinations(&mut app);
        assert!(decisions.is_empty());
        // 应 spawn 1 个 SkillUpdateRequestMessage
        let mut q = app.world_mut().query::<&SkillUpdateRequestMessage>();
        let skill_update_count = q.iter(app.world()).count();
        assert_eq!(
            skill_update_count, 1,
            "should spawn exactly one SkillUpdateRequestMessage"
        );
    }

    /// 2. self_updatable=false 的注入 skill → 候选标记 Discarded（不强行降级 payload）。
    #[test]
    fn governance_discards_non_self_updatable_skill_candidate() {
        let mut app = make_governance_app();

        let agent_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let candidate_id = uuid::Uuid::new_v4();
        let skill_id = SkillId::new("owner-agent", "locked-skill");

        register_agent(&mut app, agent_id, make_agent(agent_id, "worker", &["llm"]));
        register_task(
            &mut app,
            task_id,
            make_task(task_id),
            Some(TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            }),
        );
        // skill self_updatable=false
        app.world_mut()
            .resource_mut::<SkillRegistry>()
            .upsert(make_skill_entry(skill_id, false));
        stage_candidate(
            &mut app,
            make_skill_candidate(candidate_id, task_id, agent_id),
        );
        app.world_mut()
            .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

        app.update();

        let destinations = writeback_destinations(&mut app);
        let decisions = governance_decision_destinations(&mut app);

        // 候选被 Discarded，kind_hint 保持 Skill（不降级 payload）
        let store = app.world().resource::<ExperienceStore>();
        let candidate = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(candidate.kind_hint, ExperienceKindHint::Skill);
        assert_eq!(candidate.status, ExperienceCandidateStatus::Discarded);
        // 不应 spawn WritebackRequestMessage
        assert!(
            destinations.is_empty(),
            "Discarded path should not spawn WritebackRequestMessage"
        );
        // 不应 spawn 任何 ExperienceGovernanceDecision
        assert!(decisions.is_empty());
        // 不应 spawn SkillUpdateRequestMessage
        let mut q = app.world_mut().query::<&SkillUpdateRequestMessage>();
        let skill_update_count = q.iter(app.world()).count();
        assert_eq!(
            skill_update_count, 0,
            "Discarded path should not spawn SkillUpdateRequestMessage"
        );
    }

    /// 3. 非默认 agent + 未注入 skill → SkillPackage（需要用户确认）。
    #[test]
    fn governance_routes_non_injected_skill_to_skill_package() {
        let mut app = make_governance_app();

        let agent_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let candidate_id = uuid::Uuid::new_v4();

        register_agent(&mut app, agent_id, make_agent(agent_id, "worker", &["llm"]));
        // task 不注入 skill（经 EntityIndex 注册）
        register_task(&mut app, task_id, make_task(task_id), None);
        stage_candidate(
            &mut app,
            make_skill_candidate(candidate_id, task_id, agent_id),
        );
        app.world_mut()
            .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

        app.update();

        let destinations = writeback_destinations(&mut app);
        let decisions = governance_decision_destinations(&mut app);

        // 候选状态 NeedsUserApproval，destination=SkillPackage
        let store = app.world().resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::NeedsUserApproval,
        );
        assert_eq!(
            decisions,
            vec![ExperienceWritebackDestination::SkillPackage],
        );
        // 非确认路径不应触发
        assert!(destinations.is_empty());
    }

    /// 4. default agent + Skill 候选 → IncubationProposal（保留原 default agent 语义）。
    #[test]
    fn governance_routes_default_agent_skill_to_incubation() {
        let mut app = make_governance_app();

        let agent_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let candidate_id = uuid::Uuid::new_v4();
        let skill_id = SkillId::new("default", "some-skill");

        // default agent（经 EntityIndex 注册）
        register_agent(
            &mut app,
            agent_id,
            make_agent(agent_id, "default-agent", &["default"]),
        );
        // 即使 task 注入了 skill，default agent 也应走 IncubationProposal（保留 plan 疏漏修正）
        register_task(
            &mut app,
            task_id,
            make_task(task_id),
            Some(TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            }),
        );
        app.world_mut()
            .resource_mut::<SkillRegistry>()
            .upsert(make_skill_entry(skill_id, true));
        stage_candidate(
            &mut app,
            make_skill_candidate(candidate_id, task_id, agent_id),
        );
        app.world_mut()
            .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

        app.update();

        let destinations = writeback_destinations(&mut app);
        let decisions = governance_decision_destinations(&mut app);
        let pg_count = profile_generation_count(&mut app);

        // 候选状态 ProfileGenerationPending（由 spawn_incubation_confirmation 设置）
        let store = app.world().resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::ProfileGenerationPending,
        );
        assert_eq!(
            decisions,
            vec![ExperienceWritebackDestination::IncubationProposal],
        );
        assert_eq!(pg_count, 1, "IncubationProposal 应触发 profile 生成请求");
        // 非确认路径不应触发
        assert!(destinations.is_empty());
    }

    /// 5. 注入了 skill 但 skill 不在 registry 中 → 保守回退 SkillPackage。
    #[test]
    fn governance_skill_not_in_registry_falls_back_to_skill_package() {
        let mut app = make_governance_app();

        let agent_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let candidate_id = uuid::Uuid::new_v4();
        let skill_id = SkillId::new("owner-agent", "missing-skill");

        register_agent(&mut app, agent_id, make_agent(agent_id, "worker", &["llm"]));
        register_task(
            &mut app,
            task_id,
            make_task(task_id),
            Some(TaskInjectedSkill {
                skill_id: Some(skill_id.clone()),
            }),
        );
        // 故意不向 registry 添加 skill
        stage_candidate(
            &mut app,
            make_skill_candidate(candidate_id, task_id, agent_id),
        );
        app.world_mut()
            .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

        app.update();

        let destinations = writeback_destinations(&mut app);
        let decisions = governance_decision_destinations(&mut app);

        // 候选状态 NeedsUserApproval（SkillPackage 需要确认），destination=SkillPackage
        let store = app.world().resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::NeedsUserApproval,
        );
        // kind_hint 不应被降级（保持 Skill）
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().kind_hint,
            ExperienceKindHint::Skill,
        );
        assert_eq!(
            decisions,
            vec![ExperienceWritebackDestination::SkillPackage],
        );
        assert!(destinations.is_empty());
    }
}
