//! Profile 生成端到端集成测试
//!
//! 验证孵化场景的完整链路：
//! - LLM 返回 submit_profile_update 工具调用
//! - profile_generation_completion_system 创建审批请求
//! - 用户审批通过后写回 agents.toml

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    app::build_harness_app, domain::AgentExecutionOutput, domain::AgentExecutionRequest,
    domain::AgentExecutor, domain::ChannelId, domain::ExperienceGovernanceDecision,
    domain::ExperienceStore, domain::ExperienceWritebackDestination, domain::FrontendKind,
    domain::LlmToolCall, domain::OutputContent, domain::ProfileGenerationKind,
    domain::ProfileGenerationRequestMessage, domain::ShortTermMemory, domain::Task,
    domain::ToolExecutionRequestMessage, domain::WaitingReason, llm::ExecutorRegistry,
    systems::HarnessConfig,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

/// Mock executor：首轮返回 submit_profile_update ToolCalls，后续返回 Text
struct ProfileDesignerMockExecutor;

impl AgentExecutor for ProfileDesignerMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::domain::ExecutorFuture {
        let has_messages = request.conversation.as_ref().is_some_and(|c| !c.is_empty());
        let response = if has_messages {
            AgentExecutionOutput {
                content: OutputContent::Text("profile submitted".to_string()),
                reasoning_content: None,
            }
        } else {
            AgentExecutionOutput {
                content: OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_profile".to_string(),
                    name: "submit_profile_update".to_string(),
                    arguments: r#"{"name":"physics-specialist","tags":["physics","calculation"],"description":"Physics specialist agent"}"#.to_string(),
                }]),
                reasoning_content: None,
            }
        };
        Box::pin(async move { Ok(response) })
    }
}

fn test_config(agents_config_path: String) -> HarnessConfig {
    HarnessConfig {
        agents_config_path,
        ..HarnessConfig::default()
    }
}

/// 写入包含 default + profile-designer 的 agents.toml
fn write_agents_toml(path: &std::path::Path) {
    std::fs::write(
        path,
        r#"[[agent]]
name = "default"
tags = ["default"]
description = "default agent"

[[agent.models]]
provider = "default"
model = "gpt-4.1-mini"

[agent.tools]
default_permission = "Allow"

[[agent]]
name = "profile-designer"
tags = ["profile"]
description = "profile designer agent"

[[agent.models]]
provider = "default"
model = "gpt-4.1-mini"

[agent.tools]
default_permission = "Allow"
"#,
    )
    .unwrap();
}

/// 从世界中查找 profile_generation 审批请求对应的 request_id。
///
/// `ToolConfirmationRequestMessage` 是瞬态消息（由 `frontend_output_system`
/// 消费后立即 despawn），实际审批状态由配对的 `ToolExecutionRequestMessage`
/// （`pending_confirmation_id.is_some()`）持有。
fn find_profile_approval_request_id(app: &mut bevy_app::App) -> Option<uuid::Uuid> {
    let world = app.world_mut();
    let mut query = world.query::<&ToolExecutionRequestMessage>();
    query
        .iter(world)
        .find(|r| r.tool_name == "profile_generation" && r.pending_confirmation_id.is_some())
        .and_then(|r| r.pending_confirmation_id)
}

/// 验证完整孵化链路：ProfileGenerationRequest → LLM → 审批 → 写回 agents.toml
#[test]
fn incubation_flow_writes_agent_to_toml_after_approval() {
    let config_dir = TempDir::new().unwrap();
    let agents_toml = config_dir.path().join("agents.toml");
    write_agents_toml(&agents_toml);

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(ProfileDesignerMockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(agents_toml.to_str().unwrap().to_string()),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    // 第一帧：加载 agents.toml 中的 Agent
    app.update();

    let task_id = harness::domain::TaskId::new();
    let agent_id = harness::domain::AgentId::new();

    // Spawn 一个占位 Task（Waiting(Agent) 防止 task_dispatch_system 重复派发），
    // 供 llm_response_system 处理 ToolCalls 时创建 ToolCallingState
    let mut task = Task::from_user_input_ready("profile generation", 3, default_channel());
    task.id = task_id;
    task.mark_waiting(WaitingReason::Agent, chrono::Utc::now());
    let task_entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 预置候选到 ExperienceStore
    let candidate_id = uuid::Uuid::new_v4();
    let candidate = harness::domain::ExperienceCandidate {
        candidate_id,
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: "physics fact".to_string(),
        kind_hint: harness::domain::ExperienceKindHint::Knowledge,
        payload: harness::domain::ExperienceCandidatePayload::Knowledge {
            content: "E=mc^2".to_string(),
        },
        dependency_refs: vec![],
        status: harness::domain::ExperienceCandidateStatus::ProfileGenerationPending,
        governing_agent_id: Some(agent_id),
        derived_from_candidate_ids: vec![],
    };
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    // 预置 ExperienceGovernanceDecision：在真实流程中由 experience_governance_system 创建，
    // 此处直接 spawn 以便 experience_approval_result_system 审批通过后能找到匹配的决议，
    // 进而 spawn ExperienceWritebackRequestMessage 触发孵化写回。
    app.world_mut().spawn(ExperienceGovernanceDecision {
        candidate_id,
        destination: ExperienceWritebackDestination::IncubationProposal,
        requires_user_confirmation: true,
        decision_rationale: "test incubation".to_string(),
        source_task_id: task_id,
    });

    // Spawn ProfileGenerationRequestMessage 触发 profile generation WorkItem
    let candidate_ids = app
        .world()
        .resource::<ExperienceStore>()
        .root_candidates_for_task(task_id);
    app.world_mut().spawn(ProfileGenerationRequestMessage {
        task_id,
        agent_id,
        candidate_ids,
        existing_profile: None,
        kind: ProfileGenerationKind::Incubation,
        feedback: None,
        exception_count: 0,
    });

    // 多轮 update 让 WorkItem → LLM → orchestrator → completion → 审批请求 链路跑完。
    // ToolConfirmationRequestMessage 是瞬态消息（frontend_output_system 消费后 despawn），
    // 实际审批状态由配对的 ToolExecutionRequestMessage 持有。
    let mut request_id = None;
    for _ in 0..30 {
        app.update();
        request_id = find_profile_approval_request_id(&mut app);
        if request_id.is_some() {
            break;
        }
    }
    let request_id = request_id.expect(
        "应该已经生成审批请求（ToolExecutionRequestMessage with pending_confirmation_id for profile_generation）",
    );

    // 模拟用户审批通过
    app.world_mut()
        .spawn(harness::domain::ToolConfirmationResponseMessage {
            request_id,
            selected_option: "approve".to_string(),
            feedback: None,
        });

    // 多轮 update 让审批 → 写回 → 状态推进 跑完
    for _ in 0..30 {
        app.update();
    }

    // 验证 agents.toml 包含 LLM 生成的新 Agent
    let content = std::fs::read_to_string(&agents_toml).unwrap();
    let config: harness::domain::AgentConfig = toml::from_str(&content).unwrap();
    let physics = config.agent.iter().find(|a| a.name == "physics-specialist");
    assert!(
        physics.is_some(),
        "agents.toml 应包含 LLM 生成的 physics-specialist"
    );
    let physics = physics.unwrap();
    assert!(
        physics.tags.contains(&"physics".to_string()),
        "tags 应包含 physics"
    );
    assert!(
        physics.tags.contains(&"incubated".to_string()),
        "tags 应自动注入 incubated"
    );
    assert_eq!(physics.description, "Physics specialist agent");
}

/// 验证 profile-designer 缺失时孵化场景进入失败路径（不再回退）。
///
/// 期望行为（Q11 + Q27 决议）：
/// - 不 spawn 任何 profile_generation 审批请求（无回退 name）
/// - spawn SystemOutputMessage 通知用户配置 profile-designer Agent
/// - 候选状态变为 ProfileGenerationFailed
/// - profile_generation_context 已清理
///
/// 失败路径（任务 4.4）：`dispatch_system` 找不到 profile-designer Agent →
/// 标记 WorkItem Failed + `OnWorkItemFailed` hook →
/// `workitem_lifecycle_hook_system` 按 Context Component 分流到
/// `handle_profile_generation_failure`。
#[test]
fn incubation_fails_when_profile_designer_missing() {
    let config_dir = TempDir::new().unwrap();
    let agents_toml = config_dir.path().join("agents.toml");
    // 只写 default，不写 profile-designer
    std::fs::write(
        &agents_toml,
        r#"[[agent]]
name = "default"
tags = ["default"]
description = "default agent"

[[agent.models]]
provider = "default"
model = "gpt-4.1-mini"

[agent.tools]
default_permission = "Allow"
"#,
    )
    .unwrap();

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(ProfileDesignerMockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(agents_toml.to_str().unwrap().to_string()),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();

    let task_id = harness::domain::TaskId::new();
    let agent_id = harness::domain::AgentId::new();

    let candidate_id = uuid::Uuid::new_v4();
    let candidate = harness::domain::ExperienceCandidate {
        candidate_id,
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: "physics fact".to_string(),
        kind_hint: harness::domain::ExperienceKindHint::Knowledge,
        payload: harness::domain::ExperienceCandidatePayload::Knowledge {
            content: "E=mc^2".to_string(),
        },
        dependency_refs: vec![],
        status: harness::domain::ExperienceCandidateStatus::ProfileGenerationPending,
        governing_agent_id: Some(agent_id),
        derived_from_candidate_ids: vec![],
    };
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    let candidate_ids = app
        .world()
        .resource::<ExperienceStore>()
        .root_candidates_for_task(task_id);
    app.world_mut().spawn(ProfileGenerationRequestMessage {
        task_id,
        agent_id,
        candidate_ids,
        existing_profile: None,
        kind: ProfileGenerationKind::Incubation,
        feedback: None,
        exception_count: 0,
    });

    // 多轮 update：handle_profile_designer_missing 应走失败路径，
    // 不应 spawn 任何 profile_generation 审批请求
    for _ in 0..30 {
        app.update();
    }

    // 验证：没有 profile_generation 审批请求
    let has_approval_request = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .any(|r| r.tool_name == "profile_generation" && r.pending_confirmation_id.is_some())
    };
    assert!(
        !has_approval_request,
        "profile-designer 缺失时不应生成 profile_generation 审批请求"
    );

    // 注：SystemOutputMessage 由 frontend_output_system 消费后 despawn，
    // 无法在多轮 update 后检查。spawn 行为已由单元测试
    // `handle_profile_designer_missing_incubation_fails` 覆盖。

    // 验证：候选状态变为 ProfileGenerationFailed
    let candidate_status = app
        .world()
        .resource::<ExperienceStore>()
        .candidates
        .get(&candidate_id)
        .map(|c| c.status.clone());
    assert_eq!(
        candidate_status,
        Some(harness::domain::ExperienceCandidateStatus::ProfileGenerationFailed),
        "候选应被标记为 ProfileGenerationFailed"
    );

    // 验证：profile_generation_context 已清理（迁移后为 Entity Component，通过 Query 检查）
    let context_count = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::domain::ProfileGenerationContext>();
        query.iter(world).count()
    };
    assert_eq!(
        context_count, 0,
        "ProfileGenerationContext Component 应已被清理"
    );
}

/// 计数版 Mock executor：记录 LLM 调用次数，用于验证 follow-up 循环。
struct CountingProfileDesignerExecutor {
    call_count: Arc<AtomicU32>,
}

impl AgentExecutor for CountingProfileDesignerExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::domain::ExecutorFuture {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let has_messages = request.conversation.as_ref().is_some_and(|c| !c.is_empty());
        let response = if has_messages {
            // follow-up：返回 Text（触发 bug 路径，被误判为 no_tool_call）
            AgentExecutionOutput {
                content: OutputContent::Text("profile submitted".to_string()),
                reasoning_content: None,
            }
        } else {
            // 首次：返回 submit_profile_update ToolCalls
            AgentExecutionOutput {
                content: OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_profile".to_string(),
                    name: "submit_profile_update".to_string(),
                    arguments: r#"{"name":"physics-specialist","tags":["physics","calculation"],"description":"Physics specialist agent"}"#.to_string(),
                }]),
                reasoning_content: None,
            }
        };
        Box::pin(async move { Ok(response) })
    }
}

/// 验证 submit_profile_update 成功后不会触发 follow-up LLM 循环。
///
/// 复现日志 harness_2026-07-19_14-51-23.jsonl 的死循环 bug：
/// - 首次 LLM 返回 submit_profile_update ToolCalls（成功）
/// - orchestrator 同时 spawn ProfileGenerationCompletedMessage 和 ToolExecutionResultMessage
/// - tool_calling_orchestrator_system 消费 ToolExecutionResultMessage 触发 follow-up LLM 请求
/// - follow-up LLM 返回 Text（合理行为）
/// - llm_response_system 误判为 no_tool_call → handle_profile_generation_invalid → spawn 重试
/// - 无限循环（exception_count 因 submit 重置而永远到不了上限）
///
/// 修复后预期：
/// - LLM 只被调用 1 次（首次 ToolCalls），不触发 follow-up
/// - 无孤儿 ToolCallingState
/// - 无重试请求残留
#[test]
fn profile_generation_does_not_loop_after_successful_submit() {
    let config_dir = TempDir::new().unwrap();
    let agents_toml = config_dir.path().join("agents.toml");
    write_agents_toml(&agents_toml);

    let call_count = Arc::new(AtomicU32::new(0));
    let executor: Arc<dyn AgentExecutor> = Arc::new(CountingProfileDesignerExecutor {
        call_count: call_count.clone(),
    });
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(agents_toml.to_str().unwrap().to_string()),
        Arc::new(Runtime::new().unwrap()),
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    // 第一帧：加载 agents.toml 中的 Agent
    app.update();

    let task_id = harness::domain::TaskId::new();
    let agent_id = harness::domain::AgentId::new();

    // Spawn 占位 Task（Waiting(Agent) 防止 task_dispatch_system 重复派发）
    let mut task = Task::from_user_input_ready("profile generation", 3, default_channel());
    task.id = task_id;
    task.mark_waiting(WaitingReason::Agent, chrono::Utc::now());
    let task_entity = app
        .world_mut()
        .spawn((task, ShortTermMemory::default()))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 预置候选到 ExperienceStore
    let candidate_id = uuid::Uuid::new_v4();
    let candidate = harness::domain::ExperienceCandidate {
        candidate_id,
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: "physics fact".to_string(),
        kind_hint: harness::domain::ExperienceKindHint::Knowledge,
        payload: harness::domain::ExperienceCandidatePayload::Knowledge {
            content: "E=mc^2".to_string(),
        },
        dependency_refs: vec![],
        status: harness::domain::ExperienceCandidateStatus::ProfileGenerationPending,
        governing_agent_id: Some(agent_id),
        derived_from_candidate_ids: vec![],
    };
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    // Spawn ProfileGenerationRequestMessage 触发 profile generation WorkItem
    app.world_mut().spawn(ProfileGenerationRequestMessage {
        task_id,
        agent_id,
        candidate_ids: vec![candidate_id],
        existing_profile: None,
        kind: ProfileGenerationKind::Incubation,
        feedback: None,
        exception_count: 0,
    });

    // 多轮 update 让 bug 充分暴露（不进行审批，让多余循环自然发生）
    let mut found_approval = false;
    for _ in 0..30 {
        app.update();
        if find_profile_approval_request_id(&mut app).is_some() {
            found_approval = true;
        }
    }
    assert!(found_approval, "应至少生成一次审批请求");

    // 验证 1：LLM 只被调用 1 次（首次 ToolCalls）。
    // 修复前：每次循环 2 次 LLM 调用（首次 ToolCalls + follow-up Text），30 帧 ≥ 4 次。
    // 修复后：仅 1 次 LLM 调用，submit 成功后 ToolCallingState 被 despawn，不触发 follow-up。
    let calls = call_count.load(Ordering::SeqCst);
    assert_eq!(
        calls, 1,
        "成功提交后不应触发 follow-up LLM 调用，LLM 调用次数应为 1，实际为 {}",
        calls
    );

    // 验证 2：没有孤儿 ToolCallingState。
    // 修复前：handle_profile_generation_invalid despawn WorkItem 但不清理 ToolCallingState，
    //         每次循环产生 1 个孤儿 State，30 帧 ≥ 3 个。
    // 修复后：submit 成功后 ToolCallingState 被 despawn，无孤儿。
    let orphan_state_count = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::domain::ToolCallingState>();
        query.iter(world).count()
    };
    assert_eq!(
        orphan_state_count, 0,
        "成功提交后不应有孤儿 ToolCallingState"
    );

    // 验证 3：没有重试请求残留。
    // 修复前：handle_profile_generation_invalid spawn 重试请求，循环过程中会有残留。
    // 修复后：不触发 handle_profile_generation_invalid，无重试请求。
    let pending_retry_count = {
        let world = app.world_mut();
        let mut query = world.query::<&ProfileGenerationRequestMessage>();
        query.iter(world).count()
    };
    assert_eq!(pending_retry_count, 0, "成功提交后不应有重试请求残留");
}
