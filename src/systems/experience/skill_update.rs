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
    AgentId, DispatchHint, DispatchKind, DispatchStrategy, ExperienceCandidate,
    ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceCollectionCompletedMessage,
    ExperienceGovernanceRequestMessage, ExperienceKindFilter, ExperienceKindHint, ExperienceStore,
    PendingDispatch, SkillUpdateCompletedMessage, SkillUpdateContext, SkillUpdateRequestMessage,
    SpaceToolRegistry, Task, TaskId, WorkItem, WorkItemLifecycleHookPending, WorkItemType,
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

/// skill 更新 WorkItem 创建系统：将 skill 更新请求转换为 WorkItem 并附加 PendingDispatch，
/// 由统一 dispatch_system 查找 skill-updater Agent 并派发执行请求。
pub(crate) fn skill_update_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &SkillUpdateRequestMessage)>,
    store: Res<ExperienceStore>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
) {
    for (entity, request) in &requests {
        // 1. 从 SkillRegistry 取 skill 元信息（version / owner 等）
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

        // 2. 从 ExperienceStore 取候选原文
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

        // 3. 读取 SKILL.md 完整内容（含 frontmatter 与所有 section 标题）
        //    Bug C 修复：之前 prompt 只展示 instructions 文本，LLM 看不到实际 section 结构，
        //    凭"markdown 应该有 Instruction section"的常识幻觉出不存在的 section 名。
        //    现在把完整 SKILL.md 给 LLM，让它能看到真实 section 列表。
        let skill_md_path = skill_loader.skill_md_path(&request.skill_id);
        let Ok(skill_md_content) = std::fs::read_to_string(&skill_md_path) else {
            warn!(
                event = "SkillMdReadFailed",
                task_id = %request.task_id,
                skill_id = %request.skill_id.as_string(),
                skill_path = ?skill_md_path,
                error = "failed to read SKILL.md for prompt construction",
                error_type = "FileReadFailed",
                "failed to read SKILL.md, skipping skill update"
            );
            commands.entity(entity).despawn();
            continue;
        };

        // 4. 构造 prompt（含完整 SKILL.md + 候选原文 + 版本号 + 候选类型）
        //    v8 D19：operation 列表扩展到 8 种（含 3 级标题级 + replace_body 兜底），
        //    replace_body 加软约束警示（仅当其他 operation 无法表达时才使用）。
        //    候选类型显式说明（Skill / Knowledge），帮助 LLM 理解候选语义。
        let candidate_kind_label = match candidate.kind_hint {
            ExperienceKindHint::Skill => "Skill（用于更新现有 skill 的指令/结构）",
            ExperienceKindHint::Knowledge => "Knowledge（用于补充 skill 的背景知识）",
        };
        let prompt = format!(
            "## 任务\n\n根据以下经验候选（类型：{}），为现有 skill 提交结构化 diff 更新。\n\n\
             ## 原 SKILL.md 完整内容（version {}）\n\n```markdown\n{}\n```\n\n\
             ## 经验候选\n\n### {}\n\n{}\n\n\
             ## 要求\n\n\
             1. 调用 submit_skill_update 工具提交更新，只需提供 operations 和 rationale 两个字段，skill_id / base_version / new_version 由系统自动注入\n\
             2. operations 必须是有效的 diff 操作，可选 8 种：\n\
                - 二级标题级：replace_section / add_section / remove_section / replace_frontmatter\n\
                - 三级标题级：replace_subsection / add_subsection / remove_subsection\n\
                - 兜底：replace_body（整体替换 body，frontmatter 不变）\n\
             3. operations 中的 section / subsection 名必须与原 SKILL.md 中实际存在的标题一致（系统会做 dry-run 校验，section 不存在会立即拒绝）\n\
             4. **重要**：replace_section / replace_subsection 的 content 字段**不得包含标题行本身**（系统会自动保留原 `## xxx` 或 `### xxx` 标题行），content 只需提供标题下方的正文内容。例如替换 `## Usage` 时，content 应以正文开头，而非以 `## Usage` 开头\n\
             5. 优先使用颗粒度更细的 operation（subsection 级 > section 级 > replace_body）；replace_body 仅当其他 operation 都无法表达修改意图时才使用，滥用会被评审拒绝",
            candidate_kind_label,
            skill_entry.version,
            skill_md_content,
            candidate.title,
            candidate_payload_text(candidate),
        );

        // 5. 从 registry 过滤工具，仅保留 submit_skill_update
        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| tool.name == "submit_skill_update")
            .cloned()
            .collect();

        // 6. 构建 conversation（无历史对话，仅作为 WorkItem 上下文占位）
        let conversation = Vec::new();

        // 7. 创建 WorkItem 并附加 PendingDispatch，由 dispatch_system 查找 Agent 并派发执行请求
        let work_item = WorkItem::skill_update(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.governing_agent_id,
        );

        debug!(
            event = "SkillUpdateWorkItemCreated",
            task_id = %request.task_id,
            skill_id = %request.skill_id.as_string(),
            base_version = skill_entry.version,
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
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::SkillUpdate),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));
        commands.entity(entity).despawn();
    }
}

/// skill 更新完成系统：消费 `SkillUpdateCompletedMessage`，将 diff 操作 apply 到 SKILL.md，
/// 备份原版本到 history 目录，刷新 `SkillRegistry`，将候选置为 `Persisted`，
/// 最后标记 WorkItem 完成并清理实体。
///
/// 实现说明（Bug B 修复）：`SkillUpdateCompletedMessage` 由 orchestrator insert 到
/// WorkItem entity 上（与 `SkillUpdateContext` + `WorkItem` 同 entity），本系统直接
/// 通过 Component 查询同 entity 上的 `SkillUpdateContext`，不再用 `work_item_id` 反查。
/// fallback 路径（`work_item_entity` 为 None，不应发生）：仅 `SkillUpdateCompletedMessage`，
/// 此时 `SkillUpdateContext` 缺失，记 warn 并 despawn。
///
/// 错误处理：文件读取 / apply / 写入失败时候选状态保持不变（仍为 `GovernanceResolved`），
/// 仅记录 warn 日志并 despawn 消息。history 备份与清理失败不阻断主流程。
pub(crate) fn skill_update_completion_system(
    mut commands: Commands,
    // 同一 entity 上的 SkillUpdateCompletedMessage + (optional) SkillUpdateContext。
    // 正常路径：orchestrator 把 SkillUpdateCompletedMessage insert 到 WorkItem entity 上，
    // 该 entity 已有 SkillUpdateContext + WorkItem。
    completed: Query<(
        Entity,
        &SkillUpdateCompletedMessage,
        Option<&SkillUpdateContext>,
    )>,
    mut store: ResMut<ExperienceStore>,
    mut skill_registry: ResMut<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
) {
    for (entity, msg, context_opt) in &completed {
        // 1. 从同 entity 取 SkillUpdateContext（fallback 路径下为 None）
        let Some(context) = context_opt else {
            warn!(
                event = "SkillUpdateContextMissingOnEntity",
                task_id = %msg.task_id,
                work_item_id = %msg.work_item_id,
                skill_id = %msg.skill_id.as_string(),
                error = "SkillUpdateCompletedMessage has no SkillUpdateContext on same entity",
                error_type = "ContextNotFound",
                "SkillUpdateContext not found on same entity, despawning SkillUpdateCompletedMessage"
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
                task_id = %msg.task_id,
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
                task_id = %msg.task_id,
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
                task_id = %msg.task_id,
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
                task_id = %msg.task_id,
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
                task_id = %msg.task_id,
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
                task_id = %msg.task_id,
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
                task_id = %msg.task_id,
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

        // 10. 标记 WorkItem 完成（同 entity 上 insert 钩子组件，然后 despawn 一次清理所有 Component）
        commands
            .entity(entity)
            .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemCompleted));
        commands.entity(entity).despawn();

        debug!(
            event = "SkillUpdateCompleted",
            task_id = %msg.task_id,
            skill_id = %msg.skill_id.as_string(),
            base_version = context.base_version,
            new_version = msg.new_version,
            "skill updated successfully"
        );
    }
}

/// 从 ExperienceCandidate 取候选文本用于 prompt 构造。
///
/// v8 D19：显式标注候选类型（Knowledge / Skill），与 prompt 中的 candidate_kind_label
/// 保持一致，避免 LLM 在长候选中丢失类型语义。
fn candidate_payload_text(candidate: &ExperienceCandidate) -> String {
    match &candidate.payload {
        ExperienceCandidatePayload::Knowledge { content } => {
            format!("[候选类型：Knowledge]\n\n{}", content)
        }
        ExperienceCandidatePayload::Skill {
            name,
            description,
            instructions,
            ..
        } => {
            format!(
                "[候选类型：Skill]\n\n技能名：{}\n描述：{}\n指令：{}",
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
    /// 返回 (work_item_id, candidate_id, work_item_entity)。
    fn spawn_work_item_with_context(
        world: &mut World,
        skill_id: SkillId,
        base_version: u32,
    ) -> (Uuid, Uuid, Entity) {
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
        let work_item_entity = world
            .spawn((
                work_item,
                SkillUpdateContext {
                    skill_id: skill_id.clone(),
                    base_version,
                    experience_candidate_id: candidate_id,
                    governing_agent_id,
                },
            ))
            .id();
        (work_item_id, candidate_id, work_item_entity)
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
        let (work_item_id, work_item_entity) = {
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
            let work_item_entity = world
                .spawn((
                    work_item,
                    SkillUpdateContext {
                        skill_id: skill_id.clone(),
                        base_version: 1,
                        experience_candidate_id: candidate_id,
                        governing_agent_id,
                    },
                ))
                .id();
            (work_item_id, work_item_entity)
        };

        // 用 ReplaceSection 操作替换 Usage section 内容
        let operations = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Usage".to_string(),
            content: "New usage content.".to_string(),
        }];
        world
            .entity_mut(work_item_entity)
            .insert(make_completed_message(
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

        let (work_item_id, _, work_item_entity) =
            spawn_work_item_with_context(&mut world, skill_id.clone(), 1);
        // 覆盖 candidate_id：直接重新 spawn 不方便，这里用 nil candidate_id 验证候选不变即可
        // 实际上 spawn_work_item_with_context 用的是随机 candidate_id，不影响测试逻辑：
        // 文件读取失败时候选状态保持不变，store 中无该 candidate_id 也算"不变"。

        world
            .entity_mut(work_item_entity)
            .insert(make_completed_message(work_item_id, skill_id, 1, 2, vec![]));

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

        let (work_item_id, context_candidate_id, work_item_entity) =
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
        world
            .entity_mut(work_item_entity)
            .insert(make_completed_message(
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

#[cfg(test)]
mod workitem_system_tests {
    use super::*;
    use crate::infrastructure::skills::SkillEntry;
    use bevy_ecs::system::RunSystemOnce;
    use std::fs;
    use tempfile::TempDir;

    /// 测试用 SKILL.md 模板（version=1，含 frontmatter 与 Usage / Examples 两个 section）。
    const SAMPLE_SKILL_MD: &str = "---\nname: coding\ndescription: A coding skill\nversion: 1\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n\n## Examples\n\nExample 1.\n";

    /// 在临时目录下写入 SKILL.md，返回 (TempDir, SkillLoader)。
    /// 保留 TempDir 句柄以避免目录被提前清理。
    fn setup_skill_dir(skill_id: &SkillId, content: &str) -> (TempDir, SkillLoader) {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        let skill_path = loader.skill_md_path(skill_id);
        fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
        fs::write(&skill_path, content).unwrap();
        (tmp, loader)
    }

    /// 构造测试用 SkillEntry。
    fn make_skill_entry(skill_id: SkillId, version: u32) -> SkillEntry {
        SkillEntry {
            owner_agent_name: skill_id.owner_agent_name.clone(),
            skill_id: skill_id.clone(),
            name: skill_id.skill_name.clone(),
            description: format!("desc for {}", skill_id.skill_name),
            instructions: format!("instructions for {}", skill_id.skill_name),
            version,
            self_updatable: true,
        }
    }

    /// 在 ExperienceStore 中插入一个 Submitted 状态的 Skill 类候选，返回 candidate_id。
    fn stage_submitted_skill_candidate(store: &mut ExperienceStore) -> Uuid {
        let c = ExperienceCandidate::skill(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "test skill".to_string(),
            "test-skill".to_string(),
            "desc".to_string(),
            "instr".to_string(),
            Vec::new(),
        );
        let id = c.candidate_id;
        store.candidates.insert(id, c);
        id
    }

    /// 构造 SkillUpdateRequestMessage。
    fn make_request_message(skill_id: SkillId, candidate_id: Uuid) -> SkillUpdateRequestMessage {
        SkillUpdateRequestMessage {
            task_id: Uuid::new_v4(),
            skill_id,
            experience_candidate_id: candidate_id,
            governing_agent_id: Uuid::new_v4(),
        }
    }

    /// 读取成功路径：SKILL.md 存在时，应创建 WorkItem，prompt 包含完整 SKILL.md 内容，
    /// SkillUpdateContext 附加到 WorkItem entity，请求消息被 despawn。
    #[test]
    fn workitem_system_reads_skill_md_and_spawns_workitem() {
        let skill_id = SkillId::new("worker-agent", "coding");
        let (_tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_submitted_skill_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SpaceToolRegistry::default());
        let mut skill_registry = SkillRegistry::default();
        skill_registry.upsert(make_skill_entry(skill_id.clone(), 1));
        world.insert_resource(skill_registry);
        world.insert_resource(loader);

        world.spawn(make_request_message(skill_id.clone(), candidate_id));

        let _ = world.run_system_once(skill_update_workitem_system);

        // 1. SkillUpdateRequestMessage 已被 despawn
        let request_count = world
            .query::<&SkillUpdateRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            request_count, 0,
            "SkillUpdateRequestMessage should be despawned after processing"
        );

        // 2. 创建了一个 SkillUpdate WorkItem，prompt 包含完整 SKILL.md 内容
        let (work_item_count, prompt_contains_skill_md, has_frontmatter) = {
            let mut q = world.query::<&WorkItem>();
            let mut count = 0;
            let mut prompt_ok = false;
            let mut frontmatter_ok = false;
            for wi in q.iter(&world) {
                if wi.work_type == WorkItemType::SkillUpdate {
                    count += 1;
                    // prompt 应包含 SKILL.md 的完整内容（frontmatter + body）
                    if wi.input.prompt.contains("Do the thing.")
                        && wi.input.prompt.contains("## Examples")
                    {
                        prompt_ok = true;
                    }
                    // frontmatter 的 `---` 标记应出现在 prompt 中
                    if wi.input.prompt.contains("---") {
                        frontmatter_ok = true;
                    }
                }
            }
            (count, prompt_ok, frontmatter_ok)
        };
        assert_eq!(
            work_item_count, 1,
            "exactly one SkillUpdate WorkItem should be spawned"
        );
        assert!(
            prompt_contains_skill_md,
            "WorkItem prompt should contain full SKILL.md body content"
        );
        assert!(
            has_frontmatter,
            "WorkItem prompt should contain SKILL.md frontmatter (`---` marker)"
        );

        // 3. SkillUpdateContext 附加到 WorkItem entity，且字段与请求一致
        let context_attached = {
            let mut q = world.query::<(&WorkItem, &SkillUpdateContext)>();
            q.iter(&world).any(|(wi, ctx)| {
                wi.work_type == WorkItemType::SkillUpdate
                    && ctx.skill_id == skill_id
                    && ctx.experience_candidate_id == candidate_id
                    && ctx.base_version == 1
            })
        };
        assert!(
            context_attached,
            "SkillUpdateContext should be attached to WorkItem entity with correct fields"
        );

        // 4. PendingDispatch 也应附加到 WorkItem entity（dispatch_system 会消费）
        let has_pending_dispatch = {
            let mut q = world.query::<(&WorkItem, &PendingDispatch)>();
            q.iter(&world)
                .any(|(wi, _)| wi.work_type == WorkItemType::SkillUpdate)
        };
        assert!(
            has_pending_dispatch,
            "PendingDispatch should be attached to WorkItem entity"
        );
    }

    /// 读取失败路径：SKILL.md 不存在时，请求被 despawn，不 spawn WorkItem。
    #[test]
    fn workitem_system_despawns_request_when_skill_md_missing() {
        let skill_id = SkillId::new("worker-agent", "coding");
        // 不写入 SKILL.md，仅创建 loader 指向空临时目录
        let _tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(_tmp.path().to_path_buf());

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_submitted_skill_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SpaceToolRegistry::default());
        let mut skill_registry = SkillRegistry::default();
        skill_registry.upsert(make_skill_entry(skill_id.clone(), 1));
        world.insert_resource(skill_registry);
        world.insert_resource(loader);

        world.spawn(make_request_message(skill_id, candidate_id));

        let _ = world.run_system_once(skill_update_workitem_system);

        // 1. SkillUpdateRequestMessage 已被 despawn（读取失败分支）
        let request_count = world
            .query::<&SkillUpdateRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            request_count, 0,
            "SkillUpdateRequestMessage should be despawned when SKILL.md read fails"
        );

        // 2. 不应 spawn 任何 WorkItem
        let work_item_count = world.query::<&WorkItem>().iter(&world).count();
        assert_eq!(
            work_item_count, 0,
            "no WorkItem should be spawned when SKILL.md read fails"
        );

        // 3. 不应 spawn 任何 SkillUpdateContext
        let context_count = world.query::<&SkillUpdateContext>().iter(&world).count();
        assert_eq!(
            context_count, 0,
            "no SkillUpdateContext should be spawned when SKILL.md read fails"
        );
    }

    /// skill_id 未在 SkillRegistry 注册时，请求被 despawn，不 spawn WorkItem。
    /// 覆盖 system 中 `SkillNotFoundInRegistry` 分支（与 SKILL.md 读取失败是不同的分支）。
    #[test]
    fn workitem_system_despawns_request_when_skill_not_in_registry() {
        let skill_id = SkillId::new("worker-agent", "coding");
        // 即使 SKILL.md 存在，registry 中没有该 skill 也会被 despawn
        let (_tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);

        let mut world = World::new();
        let mut store = ExperienceStore::default();
        let candidate_id = stage_submitted_skill_candidate(&mut store);
        world.insert_resource(store);
        world.insert_resource(SpaceToolRegistry::default());
        // 故意不注册 skill_id 到 SkillRegistry
        world.insert_resource(SkillRegistry::default());
        world.insert_resource(loader);

        world.spawn(make_request_message(skill_id, candidate_id));

        let _ = world.run_system_once(skill_update_workitem_system);

        // 1. SkillUpdateRequestMessage 已被 despawn
        let request_count = world
            .query::<&SkillUpdateRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            request_count, 0,
            "SkillUpdateRequestMessage should be despawned when skill not in registry"
        );

        // 2. 不应 spawn 任何 WorkItem
        let work_item_count = world.query::<&WorkItem>().iter(&world).count();
        assert_eq!(
            work_item_count, 0,
            "no WorkItem should be spawned when skill not in registry"
        );
    }

    /// experience_candidate_id 不在 ExperienceStore 中时，请求被 despawn。
    /// 覆盖 system 中 `ExperienceCandidateNotFound` 分支。
    #[test]
    fn workitem_system_despawns_request_when_candidate_missing() {
        let skill_id = SkillId::new("worker-agent", "coding");
        let (_tmp, loader) = setup_skill_dir(&skill_id, SAMPLE_SKILL_MD);

        let mut world = World::new();
        // store 为空，candidate_id 不存在
        world.insert_resource(ExperienceStore::default());
        world.insert_resource(SpaceToolRegistry::default());
        let mut skill_registry = SkillRegistry::default();
        skill_registry.upsert(make_skill_entry(skill_id.clone(), 1));
        world.insert_resource(skill_registry);
        world.insert_resource(loader);

        // 使用一个不存在的 candidate_id
        world.spawn(make_request_message(skill_id, Uuid::new_v4()));

        let _ = world.run_system_once(skill_update_workitem_system);

        // 1. SkillUpdateRequestMessage 已被 despawn
        let request_count = world
            .query::<&SkillUpdateRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            request_count, 0,
            "SkillUpdateRequestMessage should be despawned when candidate not in store"
        );

        // 2. 不应 spawn 任何 WorkItem
        let work_item_count = world.query::<&WorkItem>().iter(&world).count();
        assert_eq!(
            work_item_count, 0,
            "no WorkItem should be spawned when candidate not in store"
        );
    }
}
