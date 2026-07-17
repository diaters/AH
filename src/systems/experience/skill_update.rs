//! 持久Agent吸收路径：经验候选路由到 skill-updater / LTM。
//!
//! 当 task 的 delegate 是持久Agent时，候选不进入父任务 inbox，而是按 kind_hint 分流：
//! - 注入 skill 路径：skill 类候选 → spawn SkillUpdateRequestMessage，
//!   knowledge 类候选 → 写回 LTM（占位）
//! - 未注入 skill 路径：仍走 governance，由治理层走用户确认（评审 D12）
//!
//! 参考模式：
//! - collection.rs 的 experience_collection_completion_system
//! - profile_generation.rs 的 profile_generation_workitem_system

use crate::prelude::*;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::{
    Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentId, AgentRequestKind,
    ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
    ExperienceCollectionCompletedMessage, ExperienceGovernanceRequestMessage, ExperienceKindFilter,
    ExperienceKindHint, ExperienceStore, MessageDispatchedHookPending, SkillUpdateContext,
    SkillUpdateRequestMessage, SpaceToolRegistry, Task, TaskId, WorkItem,
    WorkItemLifecycleHookPending,
};
use crate::infrastructure::skills::{SkillId, SkillRegistry};
use crate::user_plugins::hook_point::HookPoint;

/// 持久Agent吸收路径：候选不进父 inbox，按 kind 分流。
///
/// 循环防护（ADR-004 §3.7）：`kind_filter` 在入口先过滤候选，被过滤的候选置为 `Discarded`。
/// 注入 skill 路径：
/// - skill 类候选 → spawn skill update workitem（占位，任务 20 替换）
/// - knowledge 类候选 → 写回 LTM（占位）
///
/// 未注入 skill 路径：候选置为 GovernancePending 并 spawn ExperienceGovernanceRequestMessage，
/// 由治理层走用户确认（评审 D12）。
pub fn route_persistent_agent_experience(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    msg: &ExperienceCollectionCompletedMessage,
    task: &Task,
    injected_skill: Option<SkillId>,
    policy: Option<ExperienceKindFilter>,
    candidate_ids: &[Uuid],
) {
    // 先应用 kind_filter：被过滤的候选置为 Discarded，仅保留允许的候选。
    let filtered_ids: Vec<Uuid> = candidate_ids
        .iter()
        .filter(|cid| {
            let allowed = match policy {
                Some(ExperienceKindFilter::KnowledgeOnly) => store
                    .candidates
                    .get(cid)
                    .map(|c| c.kind_hint == ExperienceKindHint::Knowledge)
                    .unwrap_or(false),
                Some(ExperienceKindFilter::SkillOnly) => store
                    .candidates
                    .get(cid)
                    .map(|c| c.kind_hint == ExperienceKindHint::Skill)
                    .unwrap_or(false),
                Some(ExperienceKindFilter::All) | None => true,
            };
            if !allowed && let Some(c) = store.candidates.get_mut(*cid) {
                c.status = ExperienceCandidateStatus::Discarded;
            }
            allowed
        })
        .copied()
        .collect();

    // 记录治理者：对保留的候选统一写入 governing_agent_id，便于后续审计与写回链路使用。
    // 与 governance system 的行为对齐（collection.rs 原有逻辑在分流前设置治理者）。
    for id in &filtered_ids {
        if let Some(c) = store.candidates.get_mut(id) {
            c.governing_agent_id = Some(msg.governing_agent_id);
        }
    }

    if let Some(skill_id) = injected_skill {
        // 持久Agent + 注入了 skill：按 kind_hint 分流到 skill-updater / LTM。
        for candidate_id in &filtered_ids {
            let kind_hint = store
                .candidates
                .get(candidate_id)
                .map(|c| c.kind_hint.clone());
            match kind_hint {
                Some(ExperienceKindHint::Skill) => {
                    spawn_skill_update_workitem(
                        commands,
                        *candidate_id,
                        skill_id.clone(),
                        msg.governing_agent_id,
                        msg.task_id,
                    );
                }
                Some(ExperienceKindHint::Knowledge) => {
                    writeback_to_long_term_memory_for_persistent_agent(
                        store,
                        *candidate_id,
                        msg.governing_agent_id,
                    );
                }
                None => {
                    // 候选已被异步删除？记录 warn 但不阻断。
                    tracing::warn!(
                        event = "ExperienceCandidateMissing",
                        error_type = "CandidateNotFound",
                        error = "candidate_id not found in store",
                        task_id = %msg.task_id,
                        candidate_id = %candidate_id,
                        "candidate disappeared during routing, skipping"
                    );
                }
            }
        }
    } else {
        // 持久Agent + 未注入 skill → 仍经 governance 走用户确认（评审 D12）。
        for candidate_id in &filtered_ids {
            if let Some(c) = store.candidates.get_mut(candidate_id) {
                c.status = ExperienceCandidateStatus::GovernancePending;
            }
        }
        commands.spawn(ExperienceGovernanceRequestMessage {
            task_id: msg.task_id,
            agent_id: msg.governing_agent_id,
        });
    }

    // 保留 task 参数以便未来扩展使用（plan 要求保留）。
    let _ = task;
}

/// spawn SkillUpdateRequestMessage，由 skill_update_workitem_system 消费构造 skill-updater WorkItem。
fn spawn_skill_update_workitem(
    commands: &mut Commands,
    candidate_id: Uuid,
    skill_id: SkillId,
    governing_agent_id: AgentId,
    task_id: TaskId,
) {
    commands.spawn(SkillUpdateRequestMessage {
        task_id,
        skill_id,
        experience_candidate_id: candidate_id,
        governing_agent_id,
    });
}

/// skill 更新 WorkItem 创建系统：将 skill 更新请求转换为独立 WorkItem 分配给 skill-updater Agent。
#[allow(dead_code)] // 任务 22 系统注册时启用
pub(crate) fn skill_update_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &SkillUpdateRequestMessage)>,
    agents: Query<&Agent>,
    store: Res<ExperienceStore>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,
) {
    for (entity, request) in &requests {
        // 1. 查找 skill-updater Agent（按 tags 匹配 "skill-updater"）
        let skill_updater = agents
            .iter()
            .find(|a| a.capabilities.tags.iter().any(|t| t == "skill-updater"));

        let skill_updater_id = match skill_updater {
            Some(a) => a.id,
            None => {
                warn!(
                    event = "SkillUpdaterNotFound",
                    task_id = %request.task_id,
                    skill_id = %request.skill_id.as_string(),
                    "skill-updater agent not found, skipping skill update"
                );
                // 候选状态保持原 GovernanceResolved，不强制降级
                commands.entity(entity).despawn();
                continue;
            }
        };

        // 2. 从 SkillRegistry 取 skill 内容
        let Some(skill_entry) = skill_registry.get(&request.skill_id) else {
            warn!(
                event = "SkillNotFoundInRegistry",
                task_id = %request.task_id,
                skill_id = %request.skill_id.as_string(),
                error = "skill_id not found in SkillRegistry",
                error_type = "SkillNotFound",
                "skill not found in registry, skipping skill update"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 3. 从 ExperienceStore 取候选原文
        let Some(candidate) = store.candidates.get(&request.experience_candidate_id) else {
            warn!(
                event = "ExperienceCandidateNotFound",
                task_id = %request.task_id,
                candidate_id = %request.experience_candidate_id,
                error = "candidate_id not found in store",
                error_type = "CandidateNotFound",
                "experience candidate not found, skipping skill update"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 4. 构造 prompt（含原 skill instructions + 候选原文 + 版本号）
        let prompt = format!(
            "## 任务\n\n根据以下经验候选，为现有 skill 提交结构化 diff 更新。\n\n\
             ## 原 skill（version {}）\n\n{}\n\n\
             ## 经验候选\n\n### {}\n\n{}\n\n\
             ## 要求\n\n\
             1. 调用 submit_skill_update 工具提交更新\n\
             2. base_version 必须为 {}\n\
             3. new_version 必须为 {}（base_version + 1）\n\
             4. operations 必须是有效的 diff 操作（replace_section / add_section / remove_section / replace_frontmatter）",
            skill_entry.version,
            skill_entry.instructions,
            candidate.title,
            candidate_payload_text(candidate),
            skill_entry.version,
            skill_entry.version + 1,
        );

        // 5. 从 registry 过滤工具，仅保留 submit_skill_update
        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| tool.name == "submit_skill_update")
            .cloned()
            .collect();

        // 6. 构建 conversation（无历史对话，仅作为 WorkItem 上下文占位）
        let conversation = Vec::new();

        // 7. 创建 WorkItem 并分配给 skill-updater，直接启动并派发执行请求
        let mut work_item = WorkItem::skill_update(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.governing_agent_id,
        );
        // 若 Agent 配置了 system_prompt（来自 agents.toml），覆盖 WorkItem 的默认 system_prompt
        if let Some(agent_system_prompt) = skill_updater.and_then(|a| a.system_prompt.as_ref()) {
            work_item.input.context.system_prompt = Some(agent_system_prompt.clone());
        }
        work_item.assign(skill_updater_id);
        work_item.start();

        let work_item_id = work_item.id;
        let exec_prompt = work_item.input.prompt.clone();
        let exec_system_prompt = work_item.input.context.system_prompt.clone();
        let exec_tools = work_item.input.context.tools.clone();
        let exec_conversation = work_item.input.context.conversation.clone();

        debug!(
            event = "SkillUpdateWorkItemCreated",
            task_id = %request.task_id,
            skill_id = %request.skill_id.as_string(),
            base_version = skill_entry.version,
            agent_id = %skill_updater_id,
            "spawning skill update work item"
        );

        commands.spawn((
            work_item,
            SkillUpdateContext {
                skill_id: request.skill_id.clone(),
                base_version: skill_entry.version,
                experience_candidate_id: request.experience_candidate_id,
                governing_agent_id: request.governing_agent_id,
            },
            WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted),
        ));
        commands.spawn((
            AgentExecutionRequestMessage {
                request: AgentExecutionRequest {
                    task_id: request.task_id,
                    agent_id: skill_updater_id,
                    request_kind: AgentRequestKind::LlmCompletion,
                    prompt: exec_prompt,
                    system_prompt: exec_system_prompt,
                    tools: exec_tools,
                    conversation: exec_conversation,
                    work_item_id: Some(work_item_id),
                    model_override: None,
                },
            },
            MessageDispatchedHookPending,
        ));
        commands.entity(entity).despawn();
    }
}

/// 从 ExperienceCandidate 取候选文本用于 prompt 构造。
fn candidate_payload_text(candidate: &ExperienceCandidate) -> String {
    match &candidate.payload {
        ExperienceCandidatePayload::Knowledge { content } => content.clone(),
        ExperienceCandidatePayload::Skill {
            name,
            description,
            instructions,
            ..
        } => {
            format!(
                "技能名：{}\n描述：{}\n指令：{}",
                name, description, instructions
            )
        }
    }
}

/// 占位：写回长期记忆。直接置为 WritebackPending，实际写回逻辑由后续任务接入。
fn writeback_to_long_term_memory_for_persistent_agent(
    store: &mut ExperienceStore,
    candidate_id: Uuid,
    governing_agent_id: AgentId,
) {
    // 占位：直接置为 WritebackPending，实际写回逻辑由后续任务接入。
    if let Some(c) = store.candidates.get_mut(&candidate_id) {
        c.status = ExperienceCandidateStatus::WritebackPending;
    }
    info!(
        event = "PersistentAgentLtmWritebackPlaceholder",
        candidate_id = %candidate_id,
        governing_agent_id = %governing_agent_id,
        "writeback to LTM for persistent agent (TODO impl)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Agent;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ExperienceCandidate,
        ExperienceKindHint, TaskId, TaskRoutingPolicy,
    };
    use crate::domain::{ChannelId, FrontendKind};

    /// 构造测试用的 skill/knowledge 候选。
    fn make_candidate(kind: ExperienceKindHint) -> ExperienceCandidate {
        match kind {
            ExperienceKindHint::Knowledge => ExperienceCandidate::knowledge(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                "title".to_string(),
                "content".to_string(),
            ),
            ExperienceKindHint::Skill => ExperienceCandidate::skill(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                "skill-title".to_string(),
                "skill-name".to_string(),
                "skill-desc".to_string(),
                "skill-instr".to_string(),
                Vec::new(),
            ),
        }
    }

    /// 构造 store 与指定 kind 序列的候选，返回 (store, candidate_ids)。
    fn setup_store_with_candidates(kinds: &[ExperienceKindHint]) -> (ExperienceStore, Vec<Uuid>) {
        let mut store = ExperienceStore::default();
        let mut ids = Vec::new();
        for kind in kinds {
            let c = make_candidate(kind.clone());
            ids.push(c.candidate_id);
            store.candidates.insert(c.candidate_id, c);
        }
        (store, ids)
    }

    /// 构造 ExperienceCollectionCompletedMessage。
    fn make_msg(
        task_id: TaskId,
        parent_task_id: Option<TaskId>,
    ) -> ExperienceCollectionCompletedMessage {
        ExperienceCollectionCompletedMessage {
            task_id,
            parent_task_id,
            agent_id: Uuid::new_v4(),
            governing_agent_id: Uuid::new_v4(),
        }
    }

    /// 构造最小化的 Persistent Agent。
    fn make_persistent_agent() -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: "test-persistent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["persistent".to_string()],
                description: "test persistent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    /// 构造测试用 Task（仅填关键字段，保留 plan 要求的 task 参数签名）。
    fn make_task() -> Task {
        Task {
            id: Uuid::new_v4(),
            content: "test task".to_string(),
            creator: Uuid::nil(),
            delegate: None,
            status: crate::domain::TaskStatus::Done,
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

    /// 在给定 World / Store 上执行 route_persistent_agent_experience 后 flush 应用 Commands。
    fn run_router(
        world: &mut World,
        store: &mut ExperienceStore,
        msg: &ExperienceCollectionCompletedMessage,
        task: &Task,
        injected_skill: Option<SkillId>,
        policy: Option<ExperienceKindFilter>,
        candidate_ids: &[Uuid],
    ) {
        {
            let mut commands = world.commands();
            route_persistent_agent_experience(
                &mut commands,
                store,
                msg,
                task,
                injected_skill,
                policy,
                candidate_ids,
            );
        }
        world.flush();
    }

    #[test]
    fn kind_filter_discards_disallowed_kind() {
        // policy=KnowledgeOnly，存在 skill/knowledge 两类候选；
        // skill 候选应被置为 Discarded，knowledge 候选保留原状态。
        let (mut store, ids) = setup_store_with_candidates(&[
            ExperienceKindHint::Skill,
            ExperienceKindHint::Knowledge,
        ]);
        let skill_id = ids[0];
        let knowledge_id = ids[1];

        let task = make_task();
        let msg = make_msg(task.id, None);
        let mut world = World::new();

        run_router(
            &mut world,
            &mut store,
            &msg,
            &task,
            None,
            Some(ExperienceKindFilter::KnowledgeOnly),
            &ids,
        );

        assert_eq!(
            store.candidates.get(&skill_id).unwrap().status,
            ExperienceCandidateStatus::Discarded,
            "skill candidate should be discarded under KnowledgeOnly filter"
        );
        assert_ne!(
            store.candidates.get(&knowledge_id).unwrap().status,
            ExperienceCandidateStatus::Discarded,
            "knowledge candidate should not be discarded"
        );
    }

    #[test]
    fn kind_filter_none_allows_all() {
        // policy=None，所有候选都应保留（不被 Discarded）。
        let (mut store, ids) = setup_store_with_candidates(&[
            ExperienceKindHint::Skill,
            ExperienceKindHint::Knowledge,
        ]);

        let task = make_task();
        let msg = make_msg(task.id, None);
        let mut world = World::new();

        run_router(&mut world, &mut store, &msg, &task, None, None, &ids);

        for id in &ids {
            assert_ne!(
                store.candidates.get(id).unwrap().status,
                ExperienceCandidateStatus::Discarded,
                "no candidate should be discarded when policy is None"
            );
        }
    }

    #[test]
    fn injected_skill_routes_knowledge_candidates_to_ltm() {
        // 注入 skill，knowledge 候选应被置为 WritebackPending。
        let (mut store, ids) = setup_store_with_candidates(&[ExperienceKindHint::Knowledge]);
        let knowledge_id = ids[0];

        let task = make_task();
        let msg = make_msg(task.id, None);
        let mut world = World::new();

        run_router(
            &mut world,
            &mut store,
            &msg,
            &task,
            Some(SkillId::new("owner", "skill")),
            None,
            &ids,
        );

        assert_eq!(
            store.candidates.get(&knowledge_id).unwrap().status,
            ExperienceCandidateStatus::WritebackPending,
            "knowledge candidate should be WritebackPending under injected skill path"
        );
    }

    #[test]
    fn injected_skill_routes_skill_candidates_to_workitem() {
        // 注入 skill，skill 候选应触发占位日志（不 panic 即视为通过占位调用）。
        // 候选状态不应被改为 WritebackPending 或 GovernancePending。
        let (mut store, ids) = setup_store_with_candidates(&[ExperienceKindHint::Skill]);
        let skill_candidate_id = ids[0];

        let task = make_task();
        let msg = make_msg(task.id, None);
        let mut world = World::new();

        run_router(
            &mut world,
            &mut store,
            &msg,
            &task,
            Some(SkillId::new("owner", "skill")),
            None,
            &ids,
        );

        // spawn_skill_update_workitem 为占位实现，不修改候选状态；
        // 这里只验证它没有被错误地置为 WritebackPending / GovernancePending。
        let status = store
            .candidates
            .get(&skill_candidate_id)
            .unwrap()
            .status
            .clone();
        assert_ne!(
            status,
            ExperienceCandidateStatus::WritebackPending,
            "skill candidate should not be marked WritebackPending"
        );
        assert_ne!(
            status,
            ExperienceCandidateStatus::GovernancePending,
            "skill candidate should not be marked GovernancePending"
        );
        assert_eq!(
            status,
            ExperienceCandidateStatus::Submitted,
            "skill candidate status should remain Submitted (placeholder no-op)"
        );
    }

    #[test]
    fn no_injected_skill_routes_to_governance() {
        // 未注入 skill：所有候选应被置为 GovernancePending 且 spawn 了
        // ExperienceGovernanceRequestMessage。
        let (mut store, ids) = setup_store_with_candidates(&[
            ExperienceKindHint::Skill,
            ExperienceKindHint::Knowledge,
        ]);

        let task = make_task();
        let msg = make_msg(task.id, None);
        let mut world = World::new();

        run_router(&mut world, &mut store, &msg, &task, None, None, &ids);

        for id in &ids {
            assert_eq!(
                store.candidates.get(id).unwrap().status,
                ExperienceCandidateStatus::GovernancePending,
                "all candidates should be GovernancePending under no-injected-skill path"
            );
        }

        let governance_msg_count = world
            .query::<&ExperienceGovernanceRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            governance_msg_count, 1,
            "exactly one ExperienceGovernanceRequestMessage should be spawned"
        );
    }

    #[test]
    fn retained_candidates_record_governing_agent_id() {
        // 验证：保留下来的候选（未被 kind_filter 丢弃）应统一写入 governing_agent_id，
        // 与 governance system 的行为对齐，便于后续审计与写回链路使用。
        let (mut store, ids) = setup_store_with_candidates(&[
            ExperienceKindHint::Knowledge,
            ExperienceKindHint::Skill,
        ]);
        let knowledge_id = ids[0];
        let skill_id_candidate = ids[1];

        let task = make_task();
        let mut msg = make_msg(task.id, None);
        // 用固定 governing_agent_id 便于断言
        let governing_agent_id = Uuid::new_v4();
        msg.governing_agent_id = governing_agent_id;

        let mut world = World::new();

        // policy=None 让所有候选保留
        run_router(&mut world, &mut store, &msg, &task, None, None, &ids);

        assert_eq!(
            store
                .candidates
                .get(&knowledge_id)
                .unwrap()
                .governing_agent_id,
            Some(governing_agent_id),
            "knowledge candidate should record governing_agent_id"
        );
        assert_eq!(
            store
                .candidates
                .get(&skill_id_candidate)
                .unwrap()
                .governing_agent_id,
            Some(governing_agent_id),
            "skill candidate should record governing_agent_id"
        );
    }

    #[test]
    fn discarded_candidates_do_not_get_governing_agent_id() {
        // 验证：被 kind_filter 丢弃的候选不应被写入 governing_agent_id
        // （governing_agent_id 设置仅对保留候选生效）。
        let (mut store, ids) = setup_store_with_candidates(&[
            ExperienceKindHint::Skill, // 将被 KnowledgeOnly 过滤
            ExperienceKindHint::Knowledge,
        ]);
        let skill_candidate_id = ids[0];

        let task = make_task();
        let mut msg = make_msg(task.id, None);
        let governing_agent_id = Uuid::new_v4();
        msg.governing_agent_id = governing_agent_id;

        let mut world = World::new();

        run_router(
            &mut world,
            &mut store,
            &msg,
            &task,
            None,
            Some(ExperienceKindFilter::KnowledgeOnly),
            &ids,
        );

        let discarded = store.candidates.get(&skill_candidate_id).unwrap();
        assert_eq!(
            discarded.status,
            ExperienceCandidateStatus::Discarded,
            "skill candidate should be discarded under KnowledgeOnly filter"
        );
        assert_ne!(
            discarded.governing_agent_id,
            Some(governing_agent_id),
            "discarded candidate should not record governing_agent_id"
        );
    }

    /// 确保 Agent 类型可正常构造（保留类型锚点，避免 unused import 误判）。
    #[test]
    fn make_persistent_agent_constructs_correctly() {
        let agent = make_persistent_agent();
        assert_eq!(agent.kind, AgentKind::Persistent);
    }
}
