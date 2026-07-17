//! 持久Agent吸收路径：经验候选路由到 skill-updater / LTM。
//!
//! 当 task 的 delegate 是持久Agent时，候选不进入父任务 inbox，而是按 kind_hint 分流：
//! - 注入 skill 路径：skill 类候选 → spawn skill update workitem（占位），
//!   knowledge 类候选 → 写回 LTM（占位）
//! - 未注入 skill 路径：仍走 governance，由治理层走用户确认（评审 D12）
//!
//! 参考模式：
//! - collection.rs 的 experience_collection_completion_system
//! - profile_generation.rs 的 profile_generation_workitem_system

use crate::prelude::*;
use tracing::info;
use uuid::Uuid;

use crate::domain::{
    AgentId, ExperienceCandidateStatus, ExperienceCollectionCompletedMessage,
    ExperienceGovernanceRequestMessage, ExperienceKindFilter, ExperienceKindHint, ExperienceStore,
    Task,
};
use crate::infrastructure::skills::SkillId;

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

/// 占位：spawn skill update workitem。任务 20 将替换为 spawn SkillUpdateRequestMessage。
#[allow(dead_code)] // 任务 20 启用
fn spawn_skill_update_workitem(
    commands: &mut Commands,
    candidate_id: Uuid,
    skill_id: SkillId,
    governing_agent_id: AgentId,
    task_id: crate::domain::TaskId,
) {
    // 占位：任务 20 将替换为 spawn SkillUpdateRequestMessage。
    // 当前仅记录日志，commands 参数留作后续实现使用。
    let _ = commands;
    info!(
        event = "SkillUpdateWorkitemSpawnPlaceholder",
        task_id = %task_id,
        candidate_id = %candidate_id,
        skill_id = %skill_id.as_string(),
        governing_agent_id = %governing_agent_id,
        "spawn skill update workitem (TODO impl in task 20)"
    );
}

/// 占位：写回长期记忆。直接置为 WritebackPending，实际写回逻辑由后续任务接入。
#[allow(dead_code)] // 后续任务启用
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

    /// 确保 Agent 类型可正常构造（保留类型锚点，避免 unused import 误判）。
    #[test]
    fn make_persistent_agent_constructs_correctly() {
        let agent = make_persistent_agent();
        assert_eq!(agent.kind, AgentKind::Persistent);
    }
}
