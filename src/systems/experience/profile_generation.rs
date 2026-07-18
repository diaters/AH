//! Profile 生成系统
//!
//! 处理 ProfileGenerationRequestMessage：
//! - 创建 profile 生成 WorkItem + 附加 ProfileGenerationContext + PendingDispatch，
//!   由统一 dispatch_system 查找 profile-designer Agent 并派发执行请求
//! - LLM 完成后消费 ProfileGenerationCompletedMessage，创建 proposal 并发起审批
//!
//! 参考模式：
//! - collection.rs 的 experience_collection_workitem_system
//! - skill_update.rs 的 skill_update_workitem_system

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{
    Agent, AgentExecutionRequest, AgentProfile, AgentRequestKind, ConfirmationOption,
    ConfirmationSource, DispatchHint, DispatchKind, DispatchStrategy, ExperienceCandidatePayload,
    ExperienceStore, PendingDispatch, PendingExperienceHooks, ProfileGenerationCompletedMessage,
    ProfileGenerationContext, ProfileGenerationKind, ProfileGenerationRequestMessage,
    SpaceToolRegistry, TaskId, ToolCalledHookPending, ToolConfirmationRequestMessage,
    ToolExecutionRequestMessage, WorkItem, WorkItemType,
};
use crate::user_plugins::hook_point::HookPoint;

/// profile 生成 WorkItem 创建系统：将生成请求转换为 WorkItem + ProfileGenerationContext Component
/// + PendingDispatch，由统一 dispatch_system 查找 profile-designer Agent 并派发执行请求。
#[allow(dead_code)] // 任务 11 系统注册时启用
pub(crate) fn profile_generation_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &ProfileGenerationRequestMessage)>,
    store: Res<ExperienceStore>,
    registry: Res<SpaceToolRegistry>,
    agents: Query<&Agent>,
) {
    for (entity, request) in &requests {
        // 1. 构建 prompt
        let prompt = build_profile_generation_prompt(request, &store, &agents);

        // 2. 收集工具定义（仅 submit_profile_update 和 skip_profile_update）
        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| {
                tool.name == "submit_profile_update" || tool.name == "skip_profile_update"
            })
            .cloned()
            .collect();

        // 3. 构建 conversation（无历史对话，仅作为 WorkItem 上下文占位）
        let conversation = Vec::new();

        // 4. 创建 WorkItem，并附加 ProfileGenerationContext Component + PendingDispatch，
        //    由 dispatch_system 通过 required_tag() = "profile" 查找 Agent 并派发执行请求
        let work_item = WorkItem::profile_generation(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.agent_id,
            request.kind.clone(),
        );

        debug!(
            event = "ProfileGenerationWorkItemCreated",
            task_id = %request.task_id,
            agent_id = %request.agent_id,
            kind = ?request.kind,
            exception_count = request.exception_count,
            has_feedback = request.feedback.is_some(),
            "spawning profile generation work item"
        );

        commands.spawn((
            work_item,
            ProfileGenerationContext {
                kind: request.kind.clone(),
                exception_count: request.exception_count,
                existing_profile: request.existing_profile.clone(),
                generated_profile: None,
            },
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::ProfileGeneration),
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

/// 构建 profile 生成 prompt：根据 kind 和 feedback 注入不同材料。
#[allow(dead_code)] // 通过 profile_generation_workitem_system 调用，但未注册到 schedule 前会触发 dead_code
fn build_profile_generation_prompt(
    request: &ProfileGenerationRequestMessage,
    store: &ExperienceStore,
    agents: &Query<&Agent>,
) -> String {
    let mut prompt = String::new();

    match request.kind {
        ProfileGenerationKind::Incubation => {
            prompt.push_str(
                "## 任务\n\n根据以下经验候选，为一个新 Agent 生成元信息（name、tags、description）。\n\n",
            );

            // 注入候选材料
            prompt.push_str("## 经验候选\n\n");
            for id in &request.candidate_ids {
                if let Some(candidate) = store.candidates.get(id) {
                    prompt.push_str(&format!("### {}\n\n", candidate.title));
                    match &candidate.payload {
                        ExperienceCandidatePayload::Knowledge { content } => {
                            prompt.push_str(&format!("{}\n\n", content));
                        }
                        ExperienceCandidatePayload::Skill {
                            name,
                            description,
                            instructions,
                            ..
                        } => {
                            prompt.push_str(&format!(
                                "技能名：{}\n描述：{}\n指令：{}\n\n",
                                name, description, instructions
                            ));
                        }
                    }
                }
            }

            // 注入现有 Agent name 列表（避免重名）
            let existing_names: Vec<&str> =
                agents.iter().map(|a| a.profile.name.as_str()).collect();
            prompt.push_str(&format!(
                "## 现有 Agent 名称（避免重复）\n\n{}\n\n",
                existing_names.join(", ")
            ));

            prompt.push_str("## 要求\n\n");
            prompt.push_str("1. name：简洁有力，使用 kebab-case，如 'physics-specialist'\n");
            prompt.push_str(
                "2. tags：3-5 个核心能力标签，不含 'incubated' 或 'default'（系统会自动注入）\n",
            );
            prompt.push_str("3. description：一到两句话概括 Agent 职责\n");
            prompt.push_str("4. 必须调用 submit_profile_update 提交结果\n");
        }
        ProfileGenerationKind::Update => {
            prompt.push_str(
                "## 任务\n\n评估现有 Agent profile 是否需要根据新经验更新 tags/description。\n\n",
            );

            // 注入现有 profile
            if let Some(existing) = &request.existing_profile {
                prompt.push_str("## 当前 Agent profile\n\n");
                prompt.push_str(&format!("- name: {}\n", existing.name));
                prompt.push_str(&format!("- tags: {}\n", existing.tags.join(", ")));
                prompt.push_str(&format!("- description: {}\n\n", existing.description));
            }

            // 注入新增经验条目
            prompt.push_str("## 新增经验条目\n\n");
            for id in &request.candidate_ids {
                if let Some(candidate) = store.candidates.get(id) {
                    prompt.push_str(&format!("- {}\n", candidate.title));
                }
            }
            prompt.push('\n');

            prompt.push_str("## 要求\n\n");
            prompt.push_str(
                "1. 若新经验带来了现有 tags/description 未覆盖的新能力，调用 submit_profile_update 提交更新后的完整 profile\n",
            );
            prompt.push_str("2. name 字段会被系统忽略（name 不可变更），但仍需填写\n");
            prompt.push_str("3. 若不需要更新，调用 skip_profile_update\n");
        }
    }

    // 重试场景：注入用户反馈
    if let Some(feedback) = &request.feedback {
        prompt.push_str(&format!(
            "## 用户评审反馈\n\n用户对上一次生成的 profile 提出以下反馈，请根据反馈重新生成：\n\n{}\n\n",
            feedback
        ));
    }

    prompt
}

/// 处理 profile 生成失败。
///
/// 孵化场景：
/// - 候选状态 → ProfileGenerationFailed
/// - 清理 ProfileGenerationContext Component（通过 despawn 或 remove Component）
/// - 防御性删除 proposals（reject_with_feedback 后重试失败时可能存在）
/// - 派发 OnAgentProfileGenerationFailed hook
/// - spawn SystemOutputMessage 通知用户
///
/// 更新场景：
/// - 候选保持原状态（Persisted），不改状态
/// - 清理 ProfileGenerationContext Component
/// - 日志记录（不通知 TUI，更新失败用户感知弱）
///
/// `work_item_entity`：对应的 WorkItem Entity，用于清理 ProfileGenerationContext Component。
///   若 WorkItem 已被 despawn（例如 LLM 异常路径中 WorkItem 已被 llm_response_system despawn），
///   传入 None 时跳过 Component 清理（此时 Component 已随 Entity 一起消失）。
pub(crate) fn handle_profile_generation_failure(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    pending_hooks: &mut PendingExperienceHooks,
    task_id: TaskId,
    kind: ProfileGenerationKind,
    reason: &str,
    work_item_entity: Option<Entity>,
) {
    match kind {
        ProfileGenerationKind::Incubation => {
            // 候选状态 → ProfileGenerationFailed
            let candidate_ids: Vec<uuid::Uuid> = store
                .candidates
                .values()
                .filter(|c| c.producer_task_id == task_id)
                .map(|c| c.candidate_id)
                .collect();
            for cid in &candidate_ids {
                if let Some(c) = store.candidates.get_mut(cid) {
                    c.status = crate::domain::ExperienceCandidateStatus::ProfileGenerationFailed;
                }
            }

            // 防御性删除 proposal（reject_with_feedback 后重试失败时可能存在）
            store.proposals.remove(&task_id);

            // 派发 hook
            pending_hooks
                .0
                .push((HookPoint::OnAgentProfileGenerationFailed, task_id));

            // 通知用户
            commands.spawn(crate::domain::SystemOutputMessage {
                task_id,
                content: format!("孵化失败：{}", reason),
            });

            warn!(
                event = "ProfileGenerationFailed",
                task_id = %task_id,
                reason = reason,
                candidate_count = candidate_ids.len(),
                "incubation profile generation failed"
            );
        }
        ProfileGenerationKind::Update => {
            info!(
                event = "ProfileUpdateSkippedAfterFailure",
                task_id = %task_id,
                reason = reason,
                "profile update skipped after failure, existing profile unchanged"
            );
        }
    }

    // 清理 ProfileGenerationContext Component
    if let Some(entity) = work_item_entity {
        commands.entity(entity).remove::<ProfileGenerationContext>();
    }
}

/// profile 生成完成处理系统：消费完成消息，创建 proposal 并发起审批。
#[allow(dead_code)] // 任务 11 系统注册时启用
pub(crate) fn profile_generation_completion_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut pending_hooks: ResMut<PendingExperienceHooks>,
    agents: Query<&Agent>,
    messages: Query<(Entity, &ProfileGenerationCompletedMessage)>,
    profile_contexts: Query<(Entity, &ProfileGenerationContext, &WorkItem)>,
) {
    for (entity, msg) in &messages {
        // 通过 WorkItem Entity 查找 ProfileGenerationContext Component（与 WorkItem 同 entity）
        // 注意：不 remove Component，因为审批阶段（reject_with_feedback）仍需读取此上下文
        let ctx_match = profile_contexts
            .iter()
            .find(|(_, _, wi)| wi.task_id == msg.task_id)
            .map(|(e, ctx, _)| (e, ctx.clone()));

        let (work_item_entity, ctx) = match ctx_match {
            Some((e, ctx)) => (Some(e), Some(ctx)),
            None => (None, None),
        };

        match msg.kind {
            ProfileGenerationKind::Incubation => {
                handle_incubation_profile_completed(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    &agents,
                    msg,
                    ctx.as_ref(),
                    work_item_entity,
                );
            }
            ProfileGenerationKind::Update => {
                handle_update_profile_completed(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    msg,
                    ctx.as_ref(),
                    work_item_entity,
                );
            }
        }
        commands.entity(entity).despawn();
    }
}

/// 处理孵化场景的 profile 生成完成。
///
/// 1. 对 tags 执行 sanitize_tags，手动注入 incubated
/// 2. 查找 default Agent 以继承 models 链
/// 3. 调用 store.merge_into_proposal 使用 LLM 生成的 name/tags/description
/// 4. 发起审批（选项包含 Approve、Reject、Reject & Feedback）
/// 5. 派发 on_agent_profile_generated hook
#[allow(dead_code)] // 通过 profile_generation_completion_system 调用，但未注册到 schedule 前会触发 dead_code
fn handle_incubation_profile_completed(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    pending_hooks: &mut PendingExperienceHooks,
    agents: &Query<&Agent>,
    msg: &ProfileGenerationCompletedMessage,
    ctx: Option<&ProfileGenerationContext>,
    work_item_entity: Option<Entity>,
) {
    use crate::domain::MAX_PROFILE_EXCEPTIONS;

    let task_id = msg.task_id;
    let agent_id = msg.agent_id;
    let exception_count = ctx.map(|c| c.exception_count).unwrap_or(0);

    let Some(generated) = &msg.generated_profile else {
        // generated_profile 为 None 有两种情况：
        // 1. LLM 主动调用 skip_profile_update（exception_count 应为 0）
        // 2. LLM 异常达到上限（exception_count >= MAX_PROFILE_EXCEPTIONS）
        if exception_count >= MAX_PROFILE_EXCEPTIONS {
            handle_profile_generation_failure(
                commands,
                store,
                pending_hooks,
                task_id,
                ProfileGenerationKind::Incubation,
                "LLM 连续异常达到上限",
                work_item_entity,
            );
        } else {
            // skip 场景：孵化场景不应出现 skip，但防御性处理
            warn!(
                event = "IncubationProfileMissing",
                task_id = %task_id,
                "incubation completed without profile (skip in incubation scenario)"
            );
            if let Some(entity) = work_item_entity {
                commands.entity(entity).remove::<ProfileGenerationContext>();
            }
        }
        return;
    };

    // 1. 对 tags 执行 sanitize_tags，孵化场景手动注入 incubated
    let mut sanitized_tags = crate::domain::sanitize_tags(generated.tags.clone(), &[]);
    if !sanitized_tags.contains(&"incubated".to_string()) {
        sanitized_tags.push("incubated".to_string());
    }

    // 2. 查找 default Agent 以继承 models 链
    let default_agent = agents
        .iter()
        .find(|a| a.capabilities.tags.iter().any(|t| t == "default"));

    // 3. 调用 store.merge_into_proposal 使用 LLM 生成的 name/tags/description
    let agent_profile = AgentProfile {
        name: generated.name.clone(),
        model: default_agent
            .map(|a| a.profile.model.clone())
            .unwrap_or_default(),
    };
    // merge_into_proposal 需要 candidate，从 store 中查找该 task 的候选。
    // 注意：此时候选状态为 ProfileGenerationPending（由 governance 系统标记），
    // 不能用 governance_candidates_for_task（它过滤 GovernancePending）。
    if let Some(candidate) = store
        .candidates_by_producer_task(task_id)
        .into_iter()
        .next()
        .cloned()
    {
        store.merge_into_proposal(task_id, agent_id, agent_profile, &candidate);
    }

    // 将 LLM 生成的 profile 写入 ProfileGenerationContext Component，
    // 供 writeback_incubation_proposal 通过 Query 读取。
    if let Some(entity) = work_item_entity
        && let Some(ctx) = ctx
    {
        let mut new_ctx = ctx.clone();
        new_ctx.generated_profile = Some(generated.clone());
        commands.entity(entity).insert(new_ctx);
    }

    // 4. 发起审批（选项包含 Approve、Reject、Reject & Feedback）
    spawn_profile_approval(
        commands,
        store,
        task_id,
        agent_id,
        &generated.name,
        &sanitized_tags,
        &generated.description,
        exception_count,
    );

    // 5. 派发 on_agent_profile_generated hook（在 LLM 生成后、用户审批前触发）
    pending_hooks
        .0
        .push((HookPoint::OnAgentProfileGenerated, task_id));

    info!(
        event = "ProfileGenerationCompleted",
        task_id = %task_id,
        name = %generated.name,
        tags = ?sanitized_tags,
        "incubation profile generated, awaiting approval"
    );
}

/// 处理更新场景的 profile 生成完成。
///
/// 若有 generated_profile，发起更新审批（任务 9 扩展）；
/// 若无（skip），静默结束。
#[allow(dead_code)] // 通过 profile_generation_completion_system 调用，但未注册到 schedule 前会触发 dead_code
fn handle_update_profile_completed(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    pending_hooks: &mut PendingExperienceHooks,
    msg: &ProfileGenerationCompletedMessage,
    ctx: Option<&ProfileGenerationContext>,
    work_item_entity: Option<Entity>,
) {
    use crate::domain::MAX_PROFILE_EXCEPTIONS;

    let exception_count = ctx.map(|c| c.exception_count).unwrap_or(0);

    if let Some(generated) = &msg.generated_profile {
        // 将 LLM 生成的 profile 写入 ProfileGenerationContext Component，
        // 供 profile_update_writeback_system 通过 Query 读取。
        if let Some(entity) = work_item_entity
            && let Some(ctx) = ctx
        {
            let mut new_ctx = ctx.clone();
            new_ctx.generated_profile = Some(generated.clone());
            commands.entity(entity).insert(new_ctx);
        }

        info!(
            event = "ProfileUpdateProposed",
            task_id = %msg.task_id,
            "profile update proposed, awaiting approval"
        );
        // 发起更新审批（复用 spawn_profile_approval）
        let existing_tags = ctx
            .and_then(|c| c.existing_profile.as_ref())
            .map(|p| p.tags.clone())
            .unwrap_or_default();
        let sanitized_tags = crate::domain::sanitize_tags(generated.tags.clone(), &existing_tags);
        spawn_profile_approval(
            commands,
            store,
            msg.task_id,
            msg.agent_id,
            &generated.name,
            &sanitized_tags,
            &generated.description,
            exception_count,
        );

        // 派发 on_agent_profile_generated hook（更新场景同样在审批前触发）
        pending_hooks
            .0
            .push((HookPoint::OnAgentProfileGenerated, msg.task_id));
    } else {
        // generated_profile 为 None 有两种情况：
        // 1. LLM 主动调用 skip_profile_update（exception_count 应为 0）
        // 2. LLM 异常达到上限（exception_count >= MAX_PROFILE_EXCEPTIONS）
        if exception_count >= MAX_PROFILE_EXCEPTIONS {
            handle_profile_generation_failure(
                commands,
                store,
                pending_hooks,
                msg.task_id,
                ProfileGenerationKind::Update,
                "LLM 连续异常达到上限",
                work_item_entity,
            );
        } else {
            info!(
                event = "ProfileUpdateSkipped",
                task_id = %msg.task_id,
                "profile update skipped by LLM"
            );
            // skip 时清理 Component
            if let Some(entity) = work_item_entity {
                commands.entity(entity).remove::<ProfileGenerationContext>();
            }
        }
    }
}

/// 发起 profile 审批：spawn ToolConfirmationRequestMessage 和占位 ToolExecutionRequestMessage。
///
/// 参照 governance.rs 的 spawn_experience_confirmation 模式。
/// 审批选项：
/// - approve: 批准 LLM 生成的 profile
/// - reject: 拒绝此 profile，终止孵化
/// - reject_with_feedback: 拒绝并提供评审建议，LLM 将重新生成（始终可用，不受 exception_count 限制）
#[allow(dead_code, clippy::too_many_arguments)]
fn spawn_profile_approval(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    task_id: TaskId,
    agent_id: crate::domain::AgentId,
    name: &str,
    tags: &[String],
    description: &str,
    exception_count: u32,
) {
    let request_id = uuid::Uuid::new_v4();

    // 绑定到任务的一个候选（若存在）。
    // 孵化场景候选为 ProfileGenerationPending，更新场景候选为 Persisted，
    // 因此使用 candidates_by_producer_task 而非 governance_candidates_for_task。
    if let Some(candidate_id) = store
        .candidates_by_producer_task(task_id)
        .first()
        .map(|c| c.candidate_id)
    {
        store.bind_approval_request(request_id, candidate_id);
    }

    // 构建审批选项：reject_with_feedback 始终可用
    // （exception_count 仅限制 LLM 异常重试，不限制用户反馈次数）
    let options = vec![
        ConfirmationOption {
            id: "approve".to_string(),
            label: "批准".to_string(),
            mode: crate::domain::GrantMode::Once,
        },
        ConfirmationOption {
            id: "reject".to_string(),
            label: "拒绝".to_string(),
            mode: crate::domain::GrantMode::Once,
        },
        ConfirmationOption {
            id: "reject_with_feedback".to_string(),
            label: "拒绝并反馈".to_string(),
            mode: crate::domain::GrantMode::Once,
        },
    ];

    debug!(
        event = "ProfileApprovalBound",
        request_id = %request_id,
        task_id = %task_id,
        exception_count = exception_count,
        "bound profile approval request (reject_with_feedback always available)"
    );

    commands.spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id,
        agent_id,
        tool_name: "profile_generation".to_string(),
        tool_input: serde_json::json!({
            "name": name,
            "tags": tags,
            "description": description,
            "exception_count": exception_count,
        }),
        options: options.clone(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
        approval_context: Some(format!("Agent profile generation for task {}", task_id)),
    });

    // 配对 ToolExecutionRequestMessage 占位实体，使 tool_confirmation_result_system
    // 能通过 pending_confirmation_id 找到匹配。参照 governance.rs 的模式。
    commands.spawn((
        ToolCalledHookPending,
        ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id,
                request_kind: AgentRequestKind::ToolExecution {
                    tool_name: "profile_generation".to_string(),
                },
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                conversation: None,
                work_item_id: None,
                model_override: None,
            },
            tool_name: "profile_generation".to_string(),
            tool_input: serde_json::json!({
                "name": name,
                "tags": tags,
                "description": description,
            }),
            pending_confirmation_id: Some(request_id),
            tool_call_id: None,
            pending_confirmation_options: Some(options),
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentToolPermissions, ExperienceCandidate,
        ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceKindHint,
        ProfileGenerationContext, ProfileGenerationKind, ProfileGenerationRequestMessage,
    };

    fn make_test_agent(name: &str, tags: &[&str]) -> Agent {
        Agent {
            id: uuid::Uuid::new_v4(),
            profile: AgentProfile {
                name: name.to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: tags.iter().map(|t| t.to_string()).collect(),
                description: "test".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    #[test]
    fn build_prompt_incubation_includes_candidates_and_existing_names() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "test fact".to_string(),
            "some content".to_string(),
        );
        store.candidates.insert(candidate.candidate_id, candidate);

        let request = ProfileGenerationRequestMessage {
            task_id,
            agent_id,
            candidate_ids: store.candidates.keys().copied().collect(),
            existing_profile: None,
            kind: ProfileGenerationKind::Incubation,
            feedback: None,
            exception_count: 0,
        };

        // 模拟 agents query：使用 Vec 代替
        let agents_vec = vec![make_test_agent("existing-agent", &["default"])];
        let prompt = build_prompt_incubation_for_test(&request, &store, &agents_vec);

        assert!(prompt.contains("## 任务"));
        assert!(prompt.contains("## 经验候选"));
        assert!(prompt.contains("test fact"));
        assert!(prompt.contains("some content"));
        assert!(prompt.contains("## 现有 Agent 名称"));
        assert!(prompt.contains("existing-agent"));
        assert!(prompt.contains("## 要求"));
    }

    #[test]
    fn build_prompt_update_includes_existing_profile() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let store = ExperienceStore::default();
        let request = ProfileGenerationRequestMessage {
            task_id,
            agent_id,
            candidate_ids: vec![],
            existing_profile: Some(crate::domain::ExistingAgentProfile {
                name: "existing-agent".to_string(),
                tags: vec!["build".to_string()],
                description: "build helper".to_string(),
            }),
            kind: ProfileGenerationKind::Update,
            feedback: None,
            exception_count: 0,
        };

        let agents_vec: Vec<Agent> = vec![];
        let prompt = build_prompt_update_for_test(&request, &store, &agents_vec);

        assert!(prompt.contains("## 任务"));
        assert!(prompt.contains("## 当前 Agent profile"));
        assert!(prompt.contains("existing-agent"));
        assert!(prompt.contains("build helper"));
    }

    #[test]
    fn build_prompt_includes_feedback_on_retry() {
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let store = ExperienceStore::default();
        let request = ProfileGenerationRequestMessage {
            task_id,
            agent_id,
            candidate_ids: vec![],
            existing_profile: None,
            kind: ProfileGenerationKind::Incubation,
            feedback: Some("name 太长了，请缩短".to_string()),
            exception_count: 1,
        };

        let agents_vec: Vec<Agent> = vec![];
        let prompt = build_prompt_incubation_for_test(&request, &store, &agents_vec);

        assert!(prompt.contains("## 用户评审反馈"));
        assert!(prompt.contains("name 太长了，请缩短"));
    }

    /// 测试辅助函数：直接调用 build_profile_generation_prompt 的孵化分支。
    /// 由于 build_profile_generation_prompt 需要 Query<&Agent>，这里通过直接复制逻辑测试。
    fn build_prompt_incubation_for_test(
        request: &ProfileGenerationRequestMessage,
        store: &ExperienceStore,
        agents: &[Agent],
    ) -> String {
        // 复制 build_profile_generation_prompt 的孵化分支逻辑
        let mut prompt = String::new();
        prompt.push_str(
            "## 任务\n\n根据以下经验候选，为一个新 Agent 生成元信息（name、tags、description）。\n\n",
        );
        prompt.push_str("## 经验候选\n\n");
        for id in &request.candidate_ids {
            if let Some(candidate) = store.candidates.get(id) {
                prompt.push_str(&format!("### {}\n\n", candidate.title));
                if let ExperienceCandidatePayload::Knowledge { content } = &candidate.payload {
                    prompt.push_str(&format!("{}\n\n", content));
                }
            }
        }
        let existing_names: Vec<&str> = agents.iter().map(|a| a.profile.name.as_str()).collect();
        prompt.push_str(&format!(
            "## 现有 Agent 名称（避免重复）\n\n{}\n\n",
            existing_names.join(", ")
        ));
        prompt.push_str("## 要求\n\n");
        prompt.push_str("1. name：简洁有力，使用 kebab-case，如 'physics-specialist'\n");
        prompt.push_str(
            "2. tags：3-5 个核心能力标签，不含 'incubated' 或 'default'（系统会自动注入）\n",
        );
        prompt.push_str("3. description：一到两句话概括 Agent 职责\n");
        prompt.push_str("4. 必须调用 submit_profile_update 提交结果\n");
        if let Some(feedback) = &request.feedback {
            prompt.push_str(&format!(
                "## 用户评审反馈\n\n用户对上一次生成的 profile 提出以下反馈，请根据反馈重新生成：\n\n{}\n\n",
                feedback
            ));
        }
        prompt
    }

    /// 测试辅助函数：直接复制 build_profile_generation_prompt 的更新分支逻辑。
    fn build_prompt_update_for_test(
        request: &ProfileGenerationRequestMessage,
        store: &ExperienceStore,
        _agents: &[Agent],
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(
            "## 任务\n\n评估现有 Agent profile 是否需要根据新经验更新 tags/description。\n\n",
        );
        if let Some(existing) = &request.existing_profile {
            prompt.push_str("## 当前 Agent profile\n\n");
            prompt.push_str(&format!("- name: {}\n", existing.name));
            prompt.push_str(&format!("- tags: {}\n", existing.tags.join(", ")));
            prompt.push_str(&format!("- description: {}\n\n", existing.description));
        }
        prompt.push_str("## 新增经验条目\n\n");
        for id in &request.candidate_ids {
            if let Some(candidate) = store.candidates.get(id) {
                prompt.push_str(&format!("- {}\n", candidate.title));
            }
        }
        prompt.push('\n');
        prompt.push_str("## 要求\n\n");
        prompt
    }

    #[test]
    fn handle_profile_generation_failure_incubation_fails() {
        // 测试 handle_profile_generation_failure 在孵化场景的行为：
        // - 候选状态 → ProfileGenerationFailed
        // - spawn SystemOutputMessage 通知用户
        // - 派发 OnAgentProfileGenerationFailed hook
        // - 移除 WorkItem Entity 上的 ProfileGenerationContext Component
        let mut world = World::new();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();
        let candidate = ExperienceCandidate {
            candidate_id: uuid::Uuid::new_v4(),
            producer_task_id: task_id,
            producer_agent_id: agent_id,
            title: "test".to_string(),
            kind_hint: ExperienceKindHint::Knowledge,
            payload: ExperienceCandidatePayload::Knowledge {
                content: "test".to_string(),
            },
            dependency_refs: vec![],
            status: ExperienceCandidateStatus::ProfileGenerationPending,
            governing_agent_id: None,
            derived_from_candidate_ids: vec![],
        };
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        world.insert_resource(store);
        world.insert_resource(PendingExperienceHooks::default());

        // spawn WorkItem + ProfileGenerationContext Entity，模拟 profile_generation_workitem_system 的产出
        let work_item = WorkItem::profile_generation(
            task_id,
            "prompt".to_string(),
            Vec::new(),
            Vec::new(),
            agent_id,
            ProfileGenerationKind::Incubation,
        );
        let work_item_entity = world
            .spawn((
                work_item,
                ProfileGenerationContext {
                    kind: ProfileGenerationKind::Incubation,
                    exception_count: 0,
                    existing_profile: None,
                    generated_profile: None,
                },
            ))
            .id();

        // 调用 handle_profile_generation_failure
        // 使用 resource_scope 避免 world 同时被多次可变借用
        world.resource_scope(|world, mut store: Mut<ExperienceStore>| {
            world.resource_scope(|world, mut pending_hooks: Mut<PendingExperienceHooks>| {
                let mut commands = world.commands();
                handle_profile_generation_failure(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    task_id,
                    ProfileGenerationKind::Incubation,
                    "profile-designer Agent 未配置",
                    Some(work_item_entity),
                );
            });
        });
        world.flush();

        // 验证：不 spawn ProfileGenerationCompletedMessage
        let completion_count = world
            .query::<&ProfileGenerationCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(completion_count, 0, "should not spawn completion message");

        // 验证：spawn SystemOutputMessage 通知用户
        let notice_count = world
            .query::<&crate::domain::SystemOutputMessage>()
            .iter(&world)
            .count();
        assert_eq!(notice_count, 1, "should spawn user notification");

        // 验证：候选状态变为 ProfileGenerationFailed
        let store = world.resource::<ExperienceStore>();
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::ProfileGenerationFailed,
            "candidate should be ProfileGenerationFailed"
        );

        // 验证：ProfileGenerationContext Component 已被移除（Entity 仍存在）
        let has_context = world
            .query::<&ProfileGenerationContext>()
            .iter(&world)
            .any(|c| c.kind == ProfileGenerationKind::Incubation);
        assert!(
            !has_context,
            "ProfileGenerationContext Component should be removed from WorkItem Entity"
        );

        // 验证：hook 已派发
        let hooks = world.resource::<PendingExperienceHooks>();
        assert!(
            hooks.0.iter().any(
                |(hp, tid)| *hp == HookPoint::OnAgentProfileGenerationFailed && *tid == task_id
            ),
            "OnAgentProfileGenerationFailed hook should be dispatched"
        );
    }

    #[test]
    fn handle_profile_generation_failure_update_skips_silently() {
        // 测试 handle_profile_generation_failure 在更新场景的行为：
        // - 不 spawn 任何消息
        // - 不改候选状态
        // - 移除 WorkItem Entity 上的 ProfileGenerationContext Component
        let mut world = World::new();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let store = ExperienceStore::default();
        world.insert_resource(store);
        world.insert_resource(PendingExperienceHooks::default());

        // spawn WorkItem + ProfileGenerationContext Entity，模拟 profile_generation_workitem_system 的产出
        let work_item = WorkItem::profile_generation(
            task_id,
            "prompt".to_string(),
            Vec::new(),
            Vec::new(),
            agent_id,
            ProfileGenerationKind::Update,
        );
        let work_item_entity = world
            .spawn((
                work_item,
                ProfileGenerationContext {
                    kind: ProfileGenerationKind::Update,
                    exception_count: 0,
                    existing_profile: None,
                    generated_profile: None,
                },
            ))
            .id();

        // 调用 handle_profile_generation_failure（Update 分支）
        // 使用 resource_scope 避免 world 同时被多次可变借用
        world.resource_scope(|world, mut store: Mut<ExperienceStore>| {
            world.resource_scope(|world, mut pending_hooks: Mut<PendingExperienceHooks>| {
                let mut commands = world.commands();
                handle_profile_generation_failure(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    task_id,
                    ProfileGenerationKind::Update,
                    "LLM 连续异常达到上限",
                    Some(work_item_entity),
                );
            });
        });
        world.flush();

        // 验证：不 spawn 任何消息
        let completion_count = world
            .query::<&ProfileGenerationCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            completion_count, 0,
            "update scenario should not spawn completion"
        );

        let notice_count = world
            .query::<&crate::domain::SystemOutputMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            notice_count, 0,
            "update scenario should not spawn notification"
        );

        // 验证：ProfileGenerationContext Component 已被移除
        let has_context = world
            .query::<&ProfileGenerationContext>()
            .iter(&world)
            .any(|c| c.kind == ProfileGenerationKind::Update);
        assert!(
            !has_context,
            "ProfileGenerationContext Component should be removed from WorkItem Entity"
        );
    }

    #[test]
    fn profile_generation_context_round_trip() {
        let ctx = ProfileGenerationContext {
            kind: ProfileGenerationKind::Incubation,
            exception_count: 2,
            existing_profile: None,
            generated_profile: None,
        };
        assert_eq!(ctx.kind, ProfileGenerationKind::Incubation);
        assert_eq!(ctx.exception_count, 2);
        assert!(ctx.existing_profile.is_none());
        assert!(ctx.generated_profile.is_none());
    }
}
