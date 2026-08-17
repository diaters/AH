//! Profile 拒绝并反馈后重新生成端到端集成测试
//!
//! 验证孵化场景的拒绝并反馈链路：
//! - 首轮 LLM 生成 profile（physics-specialist）
//! - 用户选择 `reject_with_feedback` 并提供反馈
//! - `experience_approval_result_system` 检测拒绝反馈后 spawn 重生成请求
//! - LLM 根据反馈重新生成 profile（quantum-physicist）
//! - 用户审批通过后写回 agents.toml，最终保留重生成的 agent

use std::sync::Arc;

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    app::build_harness_app, domain::AgentExecutionOutput, domain::AgentExecutionRequest,
    domain::AgentExecutor, domain::ChannelId, domain::ExperienceGovernanceDecision,
    domain::ExperienceStore, domain::ExperienceWritebackDestination, domain::FrontendKind,
    domain::LlmToolCall, domain::OutputContent, domain::ProfileGenerationKind,
    domain::ProfileGenerationRequestMessage, domain::ShortTermMemory, domain::Task,
    domain::TaskStatus, domain::ToolExecutionRequestMessage, domain::WaitingReason,
    llm::ExecutorRegistry, systems::HarnessConfig,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

/// Mock executor：区分首轮生成、重新生成与后续调用。
///
/// - 首轮生成（prompt 不含 "用户评审反馈" 且无会话历史）：返回 `physics-specialist`
/// - 重新生成（prompt 含 "用户评审反馈" 且无会话历史）：返回 `quantum-physicist`
/// - 后续调用（conversation 非空）：返回文本 "profile submitted"
struct ProfileDesignerMockExecutor;

impl AgentExecutor for ProfileDesignerMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::domain::ExecutorFuture {
        let has_messages = request.conversation.as_ref().is_some_and(|c| !c.is_empty());
        let is_regeneration = request.prompt.contains("用户评审反馈");
        let response = if has_messages {
            AgentExecutionOutput {
                content: OutputContent::Text("profile submitted".to_string()),
                reasoning_content: None,
            }
        } else if is_regeneration {
            AgentExecutionOutput {
                content: OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_profile_v2".to_string(),
                    name: "submit_profile_update".to_string(),
                    arguments: r#"{"name":"quantum-physicist","tags":["physics","quantum"],"description":"Quantum physics specialist"}"#.to_string(),
                }]),
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

/// 查找与已知 `request_id` 不同的新 profile_generation 审批请求。
///
/// 拒绝并反馈后，旧请求的 `pending_confirmation_id` 被消费，
/// 需要等待新生成的请求出现（其 `request_id` 必然与首轮不同）。
///
/// 需要跳过首轮生成后 follow-up LLM 调用产生的回退审批请求
/// （name 以 "incubated-" 开头），只匹配 LLM 真正重新生成的 profile。
fn find_new_profile_approval_request_id(
    app: &mut bevy_app::App,
    previous_request_id: uuid::Uuid,
) -> Option<uuid::Uuid> {
    let world = app.world_mut();
    let mut query = world.query::<&ToolExecutionRequestMessage>();
    query
        .iter(world)
        .find(|r| {
            r.tool_name == "profile_generation"
                && r.pending_confirmation_id.is_some()
                && r.pending_confirmation_id != Some(previous_request_id)
                && r.tool_input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| !name.starts_with("incubated-"))
        })
        .and_then(|r| r.pending_confirmation_id)
}

/// 验证拒绝并反馈后的重新生成链路：
/// 首轮生成 → 用户拒绝并反馈 → LLM 重新生成 → 用户审批通过 → 写回 agents.toml
#[test]
fn reject_with_feedback_triggers_regeneration_and_writes_new_agent() {
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

    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    // Spawn 一个占位 Task（Waiting(Agent) 防止 task_dispatch_system 重复派发），
    // 供 llm_response_system 处理 ToolCalls 时创建 ToolCallingState
    let mut task = Task::from_user_input_ready("profile generation", 3, default_channel());
    task.id = task_id;
    task.status = TaskStatus::Waiting(WaitingReason::Agent);
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

    // Spawn ProfileGenerationRequestMessage 触发首轮 profile generation WorkItem
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

    // 阶段 1：多轮 update 让首轮 WorkItem → LLM → orchestrator → completion → 审批请求 链路跑完，
    // 拿到首轮审批 request_id。
    let mut first_request_id = None;
    for _ in 0..40 {
        app.update();
        first_request_id = find_profile_approval_request_id(&mut app);
        if first_request_id.is_some() {
            break;
        }
    }
    let first_request_id = first_request_id
        .expect("首轮应该已经生成审批请求（profile_generation with pending_confirmation_id）");

    // 阶段 2：用户拒绝并反馈，触发 experience_approval_result_system spawn 重生成请求
    app.world_mut()
        .spawn(harness::domain::ToolConfirmationResponseMessage {
            request_id: first_request_id,
            selected_option: "reject_with_feedback".to_string(),
            feedback: Some("name too generic, be more specific".to_string()),
        });

    // 阶段 3：多轮 update 让拒绝反馈 → 候选回到 ProfileGenerationPending →
    // spawn 新 ProfileGenerationRequestMessage（exception_count 不变，feedback=Some）→
    // LLM 重新生成 → 新审批请求 链路跑完。
    let mut second_request_id = None;
    for _ in 0..40 {
        app.update();
        second_request_id = find_new_profile_approval_request_id(&mut app, first_request_id);
        if second_request_id.is_some() {
            break;
        }
    }
    let second_request_id =
        second_request_id.expect("拒绝并反馈后应该生成新的审批请求（与首轮 request_id 不同）");

    // 阶段 4：用户审批通过重生成的 profile
    app.world_mut()
        .spawn(harness::domain::ToolConfirmationResponseMessage {
            request_id: second_request_id,
            selected_option: "approve".to_string(),
            feedback: None,
        });

    // 阶段 5：多轮 update 让审批 → 写回 → 状态推进 跑完
    for _ in 0..40 {
        app.update();
    }

    // 验证 agents.toml 包含重生成的 quantum-physicist，不包含被拒绝的 physics-specialist
    let content = std::fs::read_to_string(&agents_toml).unwrap();
    let config: harness::domain::AgentConfig = toml::from_str(&content).unwrap();

    assert!(
        config.agent.iter().any(|a| a.name == "quantum-physicist"),
        "agents.toml 应包含重生成的 quantum-physicist"
    );
    assert!(
        config.agent.iter().all(|a| a.name != "physics-specialist"),
        "agents.toml 不应包含被拒绝的 physics-specialist"
    );

    let quantum = config
        .agent
        .iter()
        .find(|a| a.name == "quantum-physicist")
        .expect("agents.toml 应包含 quantum-physicist");
    assert!(
        quantum.tags.contains(&"incubated".to_string()),
        "重生成的 agent tags 应自动注入 incubated"
    );
    assert_eq!(quantum.description, "Quantum physics specialist");
}
