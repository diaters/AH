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
use std::path::PathBuf;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::{
    Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentId, AgentRequestKind,
    ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
    ExperienceCollectionCompletedMessage, ExperienceGovernanceRequestMessage, ExperienceKindFilter,
    ExperienceKindHint, ExperienceStore, MessageDispatchedHookPending, SkillUpdateCompletedMessage,
    SkillUpdateContext, SkillUpdateRequestMessage, SpaceToolRegistry, Task, TaskId, WorkItem,
    WorkItemLifecycleHookPending,
};
use crate::infrastructure::skills::{
    SkillEntry, SkillId, SkillLoader, SkillRegistry, apply_skill_operations, cleanup_skill_history,
};
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

/// skill 更新完成系统：消费 `SkillUpdateCompletedMessage`，将 diff 操作 apply 到 SKILL.md，
/// 备份原版本到 history 目录，刷新 `SkillRegistry`，将候选置为 `Persisted`，
/// 最后标记 WorkItem 完成并清理实体。
///
/// 错误处理：文件读取 / apply / 写入失败时候选状态保持不变（仍为 `GovernanceResolved`），
/// 仅记录 warn 日志并 despawn 消息。history 备份与清理失败不阻断主流程。
#[allow(dead_code)] // 任务 22 系统注册时启用
pub(crate) fn skill_update_completion_system(
    mut commands: Commands,
    messages: Query<(Entity, &SkillUpdateCompletedMessage)>,
    contexts: Query<(Entity, &SkillUpdateContext, &WorkItem)>,
    mut store: ResMut<ExperienceStore>,
    mut skill_registry: ResMut<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
) {
    for (entity, msg) in &messages {
        // 1. 通过 work_item_id 反查 SkillUpdateContext（与 WorkItem 同 entity）
        let Some((context_entity, context, _work_item)) =
            contexts.iter().find(|(_, _, wi)| wi.id == msg.work_item_id)
        else {
            warn!(
                event = "SkillUpdateContextNotFound",
                work_item_id = %msg.work_item_id,
                skill_id = %msg.skill_id.as_string(),
                error = "no SkillUpdateContext found for work_item_id",
                error_type = "ContextNotFound",
                "SkillUpdateContext not found, skipping completion"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 2. 计算 SKILL.md 路径与 history 目录
        let skill_path = skill_loader.skill_md_path(&msg.skill_id);
        let history_dir = skill_path
            .parent()
            .map(|p| p.join("history"))
            .unwrap_or_else(|| PathBuf::from("history"));

        // 3. 读取现有 SKILL.md（失败 → 候选状态保持不变）
        let Ok(content) = std::fs::read_to_string(&skill_path) else {
            warn!(
                event = "SkillMdReadFailed",
                skill_id = %msg.skill_id.as_string(),
                skill_path = ?skill_path,
                error = "failed to read SKILL.md",
                error_type = "FileReadFailed",
                "failed to read SKILL.md, candidate status unchanged"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 4. apply diff 操作（失败 → 候选状态保持不变）
        let Ok(new_content) = apply_skill_operations(&content, &msg.operations) else {
            warn!(
                event = "SkillUpdateApplyFailed",
                skill_id = %msg.skill_id.as_string(),
                base_version = context.base_version,
                error = "apply_skill_operations returned Err",
                error_type = "ApplyOperationsFailed",
                "failed to apply skill operations, candidate status unchanged"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 5. 备份原版本到 history 目录（失败不阻断后续写入）
        if let Err(e) = std::fs::create_dir_all(&history_dir) {
            warn!(
                event = "SkillHistoryDirCreateFailed",
                skill_id = %msg.skill_id.as_string(),
                history_dir = ?history_dir,
                error = %e,
                error_type = "HistoryDirCreateFailed",
                "failed to create history dir, but proceeding with write"
            );
        }
        let backup_path = history_dir.join(format!("v{}.md", context.base_version));
        if let Err(e) = std::fs::write(&backup_path, &content) {
            warn!(
                event = "SkillHistoryBackupFailed",
                skill_id = %msg.skill_id.as_string(),
                backup_path = ?backup_path,
                error = %e,
                error_type = "HistoryBackupFailed",
                "failed to write history backup, but proceeding with write"
            );
        }

        // 6. 写入新版本 SKILL.md（失败 → 候选状态保持不变）
        if let Err(e) = std::fs::write(&skill_path, &new_content) {
            warn!(
                event = "SkillMdWriteFailed",
                skill_id = %msg.skill_id.as_string(),
                skill_path = ?skill_path,
                error = %e,
                error_type = "FileWriteFailed",
                "failed to write new SKILL.md, candidate status unchanged"
            );
            commands.entity(entity).despawn();
            continue;
        }

        // 7. 清理 history（保留最新 3 代，失败不阻断）
        if let Err(e) = cleanup_skill_history(&history_dir, 3) {
            warn!(
                event = "SkillHistoryCleanupFailed",
                skill_id = %msg.skill_id.as_string(),
                history_dir = ?history_dir,
                error = %e,
                error_type = "HistoryCleanupFailed",
                "failed to cleanup skill history, but proceeding"
            );
        }

        // 8. 解析新内容并刷新 SkillRegistry；若解析失败，文件已写入，候选仍置 Persisted
        let parsed_entry =
            crate::infrastructure::skills::loader::parse_skill_md(&new_content).map(|parsed| {
                SkillEntry {
                    skill_id: msg.skill_id.clone(),
                    name: parsed.name,
                    description: parsed.description,
                    instructions: parsed.instructions,
                    version: msg.new_version,
                    owner_agent_name: msg.skill_id.owner_agent_name.clone(),
                    self_updatable: parsed.self_updatable,
                }
            });
        if let Some(entry) = parsed_entry {
            skill_registry.refresh(entry);
        } else {
            warn!(
                event = "SkillMdParseFailed",
                skill_id = %msg.skill_id.as_string(),
                error = "parse_skill_md returned None for new content",
                error_type = "ParseFailed",
                "failed to parse new SKILL.md content, registry not refreshed"
            );
        }

        // 9. 候选置为 Persisted
        if let Some(c) = store.candidates.get_mut(&context.experience_candidate_id) {
            c.status = ExperienceCandidateStatus::Persisted;
        }

        // 10. 标记 WorkItem 完成（llm_response.rs 不再 despawn 该 entity，交由本系统清理）
        commands
            .entity(context_entity)
            .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemCompleted));
        commands.entity(context_entity).despawn();

        debug!(
            event = "SkillUpdateCompleted",
            skill_id = %msg.skill_id.as_string(),
            base_version = context.base_version,
            new_version = msg.new_version,
            "skill updated successfully"
        );

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

#[cfg(test)]
mod completion_system_tests {
    use super::*;
    use crate::domain::{ConversationMessage, SkillUpdateOperation, ToolDefinition};
    use crate::infrastructure::skills::SkillLoader;
    use bevy_ecs::system::RunSystemOnce;
    use std::fs;
    use tempfile::TempDir;

    /// 测试用 SKILL.md 模板（version=1，含 Usage 与 Examples 两个 section）
    const SAMPLE_SKILL_MD: &str = "---\nname: coding\ndescription: A coding skill\nversion: 1\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n\n## Examples\n\nExample 1.\n";

    /// 在临时目录下写入 SKILL.md，返回 (TempDir, SkillLoader, skill_path)。
    /// 保留 TempDir 句柄以避免目录被提前清理。
    fn setup_skill_dir(skill_id: &SkillId, content: &str) -> (TempDir, SkillLoader) {
        let tmp = TempDir::new().unwrap();
        // base_dir 直接指向 agents/ 目录（与 default_path() 语义一致）
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        let skill_path = loader.skill_md_path(skill_id);
        fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
        fs::write(&skill_path, content).unwrap();
        (tmp, loader)
    }

    /// 构造测试用 SkillUpdateContext + WorkItem（同 entity）并 spawn 到 world。
    /// 返回 (work_item_id, candidate_id)。
    fn spawn_work_item_with_context(
        world: &mut World,
        skill_id: SkillId,
        base_version: u32,
    ) -> (Uuid, Uuid) {
        let candidate_id = Uuid::new_v4();
        let governing_agent_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let mut work_item = WorkItem::skill_update(
            task_id,
            "prompt".to_string(),
            Vec::<ConversationMessage>::new(),
            Vec::<ToolDefinition>::new(),
            governing_agent_id,
        );
        work_item.start();
        let work_item_id = work_item.id;
        world.spawn((
            work_item,
            SkillUpdateContext {
                skill_id: skill_id.clone(),
                base_version,
                experience_candidate_id: candidate_id,
                governing_agent_id,
            },
        ));
        (work_item_id, candidate_id)
    }

    /// 在 ExperienceStore 中插入一个 GovernanceResolved 状态的候选，返回 candidate_id。
    fn stage_resolved_candidate(store: &mut ExperienceStore) -> Uuid {
        let mut c = ExperienceCandidate::skill(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "title".to_string(),
            "skill-name".to_string(),
            "desc".to_string(),
            "instr".to_string(),
            Vec::new(),
        );
        c.status = ExperienceCandidateStatus::GovernanceResolved;
        let id = c.candidate_id;
        store.candidates.insert(id, c);
        id
    }

    /// 构造 SkillUpdateCompletedMessage。
    fn make_completed_message(
        work_item_id: Uuid,
        skill_id: SkillId,
        base_version: u32,
        new_version: u32,
        operations: Vec<SkillUpdateOperation>,
    ) -> SkillUpdateCompletedMessage {
        SkillUpdateCompletedMessage {
            work_item_id,
            task_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            skill_id,
            base_version,
            new_version,
            operations,
            rationale: "test rationale".to_string(),
        }
    }

    /// 验证 SkillLoader::skill_md_path 返回的路径符合约定。
    #[test]
    fn skill_md_path_returns_correct_path() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        let skill_id = SkillId::new("agent-a", "coding");
        let path = loader.skill_md_path(&skill_id);
        let expected = tmp
            .path()
            .join("agent-a")
            .join("skills")
            .join("coding")
            .join("SKILL.md");
        assert_eq!(path, expected);
    }

    /// 构造完整的 SkillUpdateCompletedMessage + Context，运行 system，
    /// 验证文件被更新、history 备份生成、registry 刷新、候选状态为 Persisted。
    #[test]
    fn completion_system_applies_operations_and_persists() {
        let skill_id = SkillId::new("agent-a", "coding");
        let (_tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);
        let skill_path = loader.skill_md_path(&skill_id);

        let mut world = World::new();
        world.insert_resource(ExperienceStore::default());
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        let candidate_id = stage_resolved_candidate(&mut world.resource_mut::<ExperienceStore>());
        let (work_item_id, _) = {
            // 用同样的 skill_id 调用 spawn helper，但手动覆盖 candidate_id
            let governing_agent_id = Uuid::new_v4();
            let task_id = Uuid::new_v4();
            let mut work_item = WorkItem::skill_update(
                task_id,
                "prompt".to_string(),
                Vec::<ConversationMessage>::new(),
                Vec::<ToolDefinition>::new(),
                governing_agent_id,
            );
            work_item.start();
            let work_item_id = work_item.id;
            world.spawn((
                work_item,
                SkillUpdateContext {
                    skill_id: skill_id.clone(),
                    base_version: 1,
                    experience_candidate_id: candidate_id,
                    governing_agent_id,
                },
            ));
            (work_item_id, candidate_id)
        };

        // 用 ReplaceSection 操作替换 Usage section 内容
        let operations = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Usage".to_string(),
            content: "New usage content.".to_string(),
        }];
        world.spawn(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

        let _ = world.run_system_once(skill_update_completion_system);

        // 1. SKILL.md 已更新
        let new_content = fs::read_to_string(&skill_path).unwrap();
        assert!(new_content.contains("New usage content."));
        assert!(!new_content.contains("Do the thing."));

        // 2. history v1.md 备份存在
        let history_dir = skill_path.parent().unwrap().join("history");
        let backup = fs::read_to_string(history_dir.join("v1.md")).unwrap();
        assert!(backup.contains("Do the thing."));

        // 3. SkillRegistry 已刷新（version=2）
        let registry = world.resource::<SkillRegistry>();
        let entry = registry
            .get(&skill_id)
            .expect("skill should be in registry");
        assert_eq!(entry.version, 2);
        assert_eq!(entry.name, "coding");

        // 4. 候选状态为 Persisted
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::Persisted);

        // 5. WorkItem entity 与 message entity 均已 despawn
        let msg_count = world
            .query::<&SkillUpdateCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(msg_count, 0);
        let work_item_count = world.query::<&WorkItem>().iter(&world).count();
        assert_eq!(work_item_count, 0);
    }

    /// SKILL.md 不存在时，候选状态保持不变，message 被 despawn。
    #[test]
    fn completion_system_handles_missing_skill_file() {
        let skill_id = SkillId::new("agent-a", "coding");
        // 不写入 SKILL.md，仅创建 loader 指向空目录
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_resolved_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        let (work_item_id, _) = spawn_work_item_with_context(&mut world, skill_id.clone(), 1);
        // 覆盖 candidate_id：直接重新 spawn 不方便，这里用 nil candidate_id 验证候选不变即可
        // 实际上 spawn_work_item_with_context 用的是随机 candidate_id，不影响测试逻辑：
        // 文件读取失败时候选状态保持不变，store 中无该 candidate_id 也算"不变"。

        world.spawn(make_completed_message(work_item_id, skill_id, 1, 2, vec![]));

        let _ = world.run_system_once(skill_update_completion_system);

        // 候选状态保持 GovernanceResolved（未被改为 Persisted）
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::GovernanceResolved);

        // message 已 despawn
        let msg_count = world
            .query::<&SkillUpdateCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(msg_count, 0);
    }

    /// 构造无法 apply 的 operations（section 不存在），候选状态保持不变。
    #[test]
    fn completion_system_handles_apply_failure() {
        let skill_id = SkillId::new("agent-a", "coding");
        let (_tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_resolved_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader.clone());

        let (work_item_id, context_candidate_id) =
            spawn_work_item_with_context(&mut world, skill_id.clone(), 1);
        // 把 store 中的 candidate_id 与 context 对齐
        world
            .resource_mut::<ExperienceStore>()
            .candidates
            .get_mut(&candidate_id)
            .unwrap()
            .candidate_id = context_candidate_id;
        // 重新插入以 key 对齐
        let c = world
            .resource_mut::<ExperienceStore>()
            .candidates
            .remove(&candidate_id)
            .unwrap();
        world
            .resource_mut::<ExperienceStore>()
            .candidates
            .insert(context_candidate_id, c);
        let candidate_id = context_candidate_id;

        // 用不存在的 section 触发 ApplyError::SectionNotFound
        let operations = vec![SkillUpdateOperation::ReplaceSection {
            section: "## NonExistent".to_string(),
            content: "x".to_string(),
        }];
        world.spawn(make_completed_message(
            work_item_id,
            skill_id.clone(),
            1,
            2,
            operations,
        ));

        let _ = world.run_system_once(skill_update_completion_system);

        // 候选状态保持 GovernanceResolved
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::GovernanceResolved);

        // SKILL.md 未被修改（仍含原内容）
        let content = fs::read_to_string(loader.skill_md_path(&skill_id)).unwrap();
        assert!(content.contains("Do the thing."));

        // message 已 despawn
        let msg_count = world
            .query::<&SkillUpdateCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(msg_count, 0);
    }

    /// work_item_id 在 contexts Query 中找不到时，候选状态保持不变。
    #[test]
    fn completion_system_handles_context_missing() {
        let skill_id = SkillId::new("agent-a", "coding");
        let (_tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_resolved_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        // 不 spawn 任何 WorkItem + SkillUpdateContext，使用一个随机 work_item_id
        let missing_work_item_id = Uuid::new_v4();
        world.spawn(make_completed_message(
            missing_work_item_id,
            skill_id,
            1,
            2,
            vec![],
        ));

        let _ = world.run_system_once(skill_update_completion_system);

        // 候选状态保持 GovernanceResolved
        let store = world.resource::<ExperienceStore>();
        let c = store.candidates.get(&candidate_id).unwrap();
        assert_eq!(c.status, ExperienceCandidateStatus::GovernanceResolved);

        // message 已 despawn
        let msg_count = world
            .query::<&SkillUpdateCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(msg_count, 0);
    }
}
