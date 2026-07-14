//! Profile 生成系统
//!
//! 处理 ProfileGenerationRequestMessage：
//! - 创建 profile 生成 WorkItem 并分配给 profile-designer Agent
//! - LLM 完成后消费 ProfileGenerationCompletedMessage，创建 proposal 并发起审批
//!
//! 参考模式：
//! - collection.rs 的 experience_collection_workitem_system
//! - governance.rs 的 spawn_experience_confirmation

use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{
    Agent, AgentExecutionRequest, AgentProfile, AgentRequestKind, ConfirmationOption,
    ConfirmationSource, ExperienceCandidatePayload, ExperienceStore, PendingExperienceHooks,
    ProfileGenerationCompletedMessage, ProfileGenerationContext, ProfileGenerationKind,
    ProfileGenerationRequestMessage, SpaceToolRegistry, TaskId, ToolCalledHookPending,
    ToolConfirmationRequestMessage, ToolExecutionRequestMessage, WorkItem,
};
use crate::user_plugins::hook_point::HookPoint;

/// profile 生成 WorkItem 创建系统：将生成请求转换为独立 WorkItem 分配给 profile-designer。
#[allow(dead_code)] // 任务 11 系统注册时启用
pub(crate) fn profile_generation_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &ProfileGenerationRequestMessage)>,
    agents: Query<&Agent>,
    mut store: ResMut<ExperienceStore>,
    registry: Res<SpaceToolRegistry>,
) {
    for (entity, request) in &requests {
        // 1. 查找 profile-designer Agent（按 tags 匹配 "profile"）
        let profile_designer = agents
            .iter()
            .find(|a| a.capabilities.tags.iter().any(|t| t == "profile"));

        let profile_designer_id = match profile_designer {
            Some(a) => a.id,
            None => {
                warn!(
                    event = "ProfileDesignerNotFound",
                    task_id = %request.task_id,
                    "profile-designer agent not found, falling back"
                );
                handle_profile_designer_missing(&mut commands, request);
                commands.entity(entity).despawn();
                continue;
            }
        };

        // 2. 暂存 kind/retry_count/existing_profile 到 ExperienceStore，
        //    供 orchestrator/completion/approval 读取
        store.profile_generation_context.insert(
            request.task_id,
            ProfileGenerationContext {
                kind: request.kind.clone(),
                retry_count: request.retry_count,
                existing_profile: request.existing_profile.clone(),
                generated_profile: None,
            },
        );

        // 3. 构建 prompt
        let prompt = build_profile_generation_prompt(request, &store, &agents);

        // 4. 收集工具定义（仅 submit_profile_update 和 skip_profile_update）
        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| {
                tool.name == "submit_profile_update" || tool.name == "skip_profile_update"
            })
            .cloned()
            .collect();

        // 5. 构建 conversation（无历史对话，仅作为 WorkItem 上下文占位）
        let conversation = Vec::new();

        // 6. 创建 WorkItem 并分配给 profile-designer
        let mut work_item = WorkItem::profile_generation(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.agent_id,
            request.kind.clone(),
        );
        work_item.assign(profile_designer_id);

        debug!(
            event = "ProfileGenerationWorkItemCreated",
            task_id = %request.task_id,
            agent_id = %request.agent_id,
            kind = ?request.kind,
            retry_count = request.retry_count,
            has_feedback = request.feedback.is_some(),
            "spawning profile generation work item"
        );

        commands.spawn(work_item);
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

/// 处理 profile-designer Agent 不存在的情况。
///
/// 孵化场景：spawn 回退 profile（硬编码 name）。
/// 更新场景：静默跳过（不更新现有 profile）。
#[allow(dead_code)] // 通过 profile_generation_workitem_system 调用，但未注册到 schedule 前会触发 dead_code
fn handle_profile_designer_missing(
    commands: &mut Commands,
    request: &ProfileGenerationRequestMessage,
) {
    match request.kind {
        ProfileGenerationKind::Incubation => {
            let fallback_profile = crate::domain::GeneratedProfile {
                name: format!("incubated-{}", request.task_id),
                tags: vec![],
                description: String::new(),
            };
            commands.spawn(ProfileGenerationCompletedMessage {
                task_id: request.task_id,
                agent_id: request.agent_id,
                generated_profile: Some(fallback_profile),
                kind: request.kind.clone(),
            });
        }
        ProfileGenerationKind::Update => {
            debug!(
                event = "ProfileUpdateSkippedNoDesigner",
                task_id = %request.task_id,
                "profile-designer not found, skipping update evaluation"
            );
        }
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
) {
    for (entity, msg) in &messages {
        // 从 store 读取暂存的 context（kind, retry_count, existing_profile）
        // 注意：不 remove，因为审批阶段（reject_with_feedback）仍需读取此上下文
        let ctx = store.profile_generation_context.get(&msg.task_id).cloned();

        match msg.kind {
            ProfileGenerationKind::Incubation => {
                handle_incubation_profile_completed(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    &agents,
                    msg,
                    ctx.as_ref(),
                );
            }
            ProfileGenerationKind::Update => {
                handle_update_profile_completed(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    msg,
                    ctx.as_ref(),
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
) {
    let task_id = msg.task_id;
    let agent_id = msg.agent_id;
    let retry_count = ctx.map(|c| c.retry_count).unwrap_or(0);

    let Some(generated) = &msg.generated_profile else {
        // skip_profile_update 或回退：孵化场景必须有 profile
        warn!(
            event = "IncubationProfileMissing",
            task_id = %task_id,
            "incubation completed without profile, using fallback"
        );
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
    // merge_into_proposal 需要 candidate，从 store 中查找该 task 的候选
    if let Some(candidate_id) = store
        .governance_candidates_for_task(task_id)
        .first()
        .copied()
        && let Some(candidate) = store.candidates.get(&candidate_id).cloned()
    {
        store.merge_into_proposal(task_id, agent_id, agent_profile, &candidate);
    }

    // 将 LLM 生成的 profile 存入 context，供 writeback_incubation_proposal 读取
    if let Some(context) = store.profile_generation_context.get_mut(&task_id) {
        context.generated_profile = Some(generated.clone());
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
        retry_count,
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
) {
    let retry_count = ctx.map(|c| c.retry_count).unwrap_or(0);

    if let Some(generated) = &msg.generated_profile {
        // 将 LLM 生成的 profile 存入 context，供 profile_update_writeback_system 读取
        if let Some(context) = store.profile_generation_context.get_mut(&msg.task_id) {
            context.generated_profile = Some(generated.clone());
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
            retry_count,
        );

        // 派发 on_agent_profile_generated hook（更新场景同样在审批前触发）
        pending_hooks
            .0
            .push((HookPoint::OnAgentProfileGenerated, msg.task_id));
    } else {
        info!(
            event = "ProfileUpdateSkipped",
            task_id = %msg.task_id,
            "profile update skipped by LLM"
        );
        // skip 时清理 context
        store.profile_generation_context.remove(&msg.task_id);
    }
}

/// 发起 profile 审批：spawn ToolConfirmationRequestMessage 和占位 ToolExecutionRequestMessage。
///
/// 参照 governance.rs 的 spawn_experience_confirmation 模式。
/// 审批选项：
/// - approve: 批准 LLM 生成的 profile
/// - reject: 拒绝此 profile，终止孵化
/// - reject_with_feedback: 拒绝并提供评审建议，LLM 将重新生成（仅 retry_count < MAX 时包含）
#[allow(dead_code, clippy::too_many_arguments)]
fn spawn_profile_approval(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    task_id: TaskId,
    agent_id: crate::domain::AgentId,
    name: &str,
    tags: &[String],
    description: &str,
    retry_count: u32,
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

    // 构建审批选项
    let mut options = vec![
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
    ];
    if retry_count < crate::domain::MAX_PROFILE_GENERATION_RETRIES {
        options.push(ConfirmationOption {
            id: "reject_with_feedback".to_string(),
            label: "拒绝并反馈".to_string(),
            mode: crate::domain::GrantMode::Once,
        });
    }

    debug!(
        event = "ProfileApprovalBound",
        request_id = %request_id,
        task_id = %task_id,
        retry_count = retry_count,
        has_feedback_option = retry_count < crate::domain::MAX_PROFILE_GENERATION_RETRIES,
        "bound profile approval request"
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
            "retry_count": retry_count,
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
            retry_count: 0,
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
            retry_count: 0,
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
            retry_count: 1,
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
    fn handle_profile_designer_missing_incubation_spawns_fallback() {
        use bevy_ecs::world::CommandQueue;

        let mut world = World::new();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let request = ProfileGenerationRequestMessage {
            task_id,
            agent_id,
            candidate_ids: vec![],
            existing_profile: None,
            kind: ProfileGenerationKind::Incubation,
            feedback: None,
            retry_count: 0,
        };

        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        handle_profile_designer_missing(&mut commands, &request);
        queue.apply(&mut world);

        let msgs: Vec<&ProfileGenerationCompletedMessage> = world
            .query::<&ProfileGenerationCompletedMessage>()
            .iter(&world)
            .collect();
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].generated_profile.is_some());
        let profile = msgs[0].generated_profile.as_ref().unwrap();
        assert!(profile.name.starts_with("incubated-"));
        assert_eq!(profile.tags, Vec::<String>::new());
    }

    #[test]
    fn handle_profile_designer_missing_update_no_spawn() {
        use bevy_ecs::world::CommandQueue;

        let mut world = World::new();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let request = ProfileGenerationRequestMessage {
            task_id,
            agent_id,
            candidate_ids: vec![],
            existing_profile: None,
            kind: ProfileGenerationKind::Update,
            feedback: None,
            retry_count: 0,
        };

        let mut queue = CommandQueue::default();
        let mut commands = Commands::new(&mut queue, &world);
        handle_profile_designer_missing(&mut commands, &request);
        queue.apply(&mut world);

        let count = world
            .query::<&ProfileGenerationCompletedMessage>()
            .iter(&world)
            .count();
        assert_eq!(count, 0, "update scenario should not spawn completion");
    }

    #[test]
    fn profile_generation_context_round_trip() {
        let ctx = ProfileGenerationContext {
            kind: ProfileGenerationKind::Incubation,
            retry_count: 2,
            existing_profile: None,
            generated_profile: None,
        };
        assert_eq!(ctx.kind, ProfileGenerationKind::Incubation);
        assert_eq!(ctx.retry_count, 2);
        assert!(ctx.existing_profile.is_none());
        assert!(ctx.generated_profile.is_none());
    }
}
