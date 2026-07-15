//! Profile 更新端到端集成测试
//!
//! 验证更新场景的完整链路：
//! - LLM 返回 submit_profile_update 工具调用，提议更新 tags/description
//! - 用户审批通过后写回 agents.toml 并同步更新 ECS Agent 组件
//! - skip_profile_update 场景下静默结束，无审批请求，agents.toml 不变
//!
//! 注意：更新流程不预置 ExperienceGovernanceDecision。审批系统在未找到决议时
//! 走 fallback 路径，直接将候选标记为 WritebackPending（不 spawn
//! ExperienceWritebackRequestMessage），从而避免 experience_writeback_system
//! 因 governing_agent_id 不匹配 ECS Agent 而失败、将候选错误标记为
//! WritebackFailed，确保 profile_update_writeback_system 能稳定处理候选。

use std::sync::Arc;

use crossbeam_channel::unbounded;
use tempfile::TempDir;
use tokio::runtime::Runtime;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExistingAgentProfile,
    ExperienceStore, FrontendKind, HarnessConfig, LlmToolCall, OutputContent,
    ProfileGenerationKind, ProfileGenerationRequestMessage, ShortTermMemory, Task, TaskStatus,
    ToolExecutionRequestMessage, WaitingReason, build_harness_app, llm::ExecutorRegistry,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

/// Mock executor：首轮返回 submit_profile_update ToolCalls（更新 tags/description），后续返回 Text。
struct UpdateProposeExecutor;

impl AgentExecutor for UpdateProposeExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let has_messages = request.conversation.as_ref().is_some_and(|c| !c.is_empty());
        let response = if has_messages {
            AgentExecutionOutput {
                content: OutputContent::Text("profile updated".to_string()),
                reasoning_content: None,
            }
        } else {
            AgentExecutionOutput {
                content: OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_update".to_string(),
                    name: "submit_profile_update".to_string(),
                    arguments: r#"{"name":"physics-specialist","tags":["physics","quantum"],"description":"new description"}"#
                        .to_string(),
                }]),
                reasoning_content: None,
            }
        };
        Box::pin(async move { Ok(response) })
    }
}

/// Mock executor：首轮返回 skip_profile_update ToolCalls，后续返回 Text。
struct SkipExecutor;

impl AgentExecutor for SkipExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let has_messages = request.conversation.as_ref().is_some_and(|c| !c.is_empty());
        let response = if has_messages {
            AgentExecutionOutput {
                content: OutputContent::Text("skipped".to_string()),
                reasoning_content: None,
            }
        } else {
            AgentExecutionOutput {
                content: OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_skip".to_string(),
                    name: "skip_profile_update".to_string(),
                    arguments: "{}".to_string(),
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

/// 写入包含 physics-specialist（持久型）+ profile-designer 的 agents.toml。
fn write_agents_toml(path: &std::path::Path) {
    std::fs::write(
        path,
        r#"[[agent]]
name = "physics-specialist"
tags = ["physics"]
description = "old description"

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

/// 验证更新链路：LLM 提议更新 → 审批 → 写回 agents.toml 和 ECS Agent 组件。
#[test]
fn update_flow_modifies_agents_toml_and_ecs() {
    let config_dir = TempDir::new().unwrap();
    let agents_toml = config_dir.path().join("agents.toml");
    write_agents_toml(&agents_toml);

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(UpdateProposeExecutor);
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

    // Spawn 占位 Task（Waiting(Agent) 防止 task_dispatch_system 重复派发），
    // 供 llm_response_system 处理 ToolCalls 时创建 ToolCallingState
    let mut task = Task::from_user_input_ready("profile update", 3, default_channel());
    task.id = task_id;
    task.status = TaskStatus::Waiting(WaitingReason::Agent);
    app.world_mut().spawn((task, ShortTermMemory::default()));

    // 预置候选到 ExperienceStore（更新流程从 Persisted 开始）
    let candidate_id = uuid::Uuid::new_v4();
    let candidate = harness::ExperienceCandidate {
        candidate_id,
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: "physics fact".to_string(),
        kind_hint: harness::ExperienceKindHint::Knowledge,
        payload: harness::ExperienceCandidatePayload::Knowledge {
            content: "E=mc^2".to_string(),
        },
        dependency_refs: vec![],
        status: harness::ExperienceCandidateStatus::Persisted,
        governing_agent_id: Some(agent_id),
        derived_from_candidate_ids: vec![],
    };
    app.world_mut()
        .resource_mut::<ExperienceStore>()
        .stage_root_candidate(candidate);

    // Spawn ProfileGenerationRequestMessage 触发更新评估。
    // existing_profile 携带当前 physics-specialist 的 profile 供 LLM 评估。
    let candidate_ids = app
        .world()
        .resource::<ExperienceStore>()
        .root_candidates_for_task(task_id);
    app.world_mut().spawn(ProfileGenerationRequestMessage {
        task_id,
        agent_id,
        candidate_ids,
        existing_profile: Some(ExistingAgentProfile {
            name: "physics-specialist".to_string(),
            tags: vec!["physics".to_string()],
            description: "old description".to_string(),
        }),
        kind: ProfileGenerationKind::Update,
        feedback: None,
        retry_count: 0,
    });

    // 多轮 update 让 WorkItem → LLM → orchestrator → completion → 审批请求 链路跑完。
    let mut request_id = None;
    for _ in 0..40 {
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
        .spawn(harness::ToolConfirmationResponseMessage {
            request_id,
            selected_option: "approve".to_string(),
            feedback: None,
        });

    // 多轮 update 让审批 → 写回 → 状态推进 跑完
    for _ in 0..40 {
        app.update();
    }

    // 验证 agents.toml 已更新
    let content = std::fs::read_to_string(&agents_toml).unwrap();
    let config: harness::domain::AgentConfig = toml::from_str(&content).unwrap();
    let physics = config
        .agent
        .iter()
        .find(|a| a.name == "physics-specialist")
        .expect("agents.toml 应仍包含 physics-specialist（name 不可变更）");
    assert!(
        physics.tags.contains(&"physics".to_string()),
        "tags 应仍包含 physics"
    );
    assert!(
        physics.tags.contains(&"quantum".to_string()),
        "tags 应新增 quantum"
    );
    assert_eq!(
        physics.description, "new description",
        "description 应已更新为 new description"
    );

    // 验证 ECS Agent 组件更新
    let agent_updated = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::Agent>();
        query.iter(world).any(|a| {
            a.profile.name == "physics-specialist"
                && a.capabilities.tags.contains(&"quantum".to_string())
                && a.capabilities.description == "new description"
        })
    };
    assert!(agent_updated, "ECS Agent 组件应已更新");
}

/// 验证 skip_profile_update 场景：静默结束，无审批请求，agents.toml 不变。
#[test]
fn skip_profile_update_silently_ends() {
    let config_dir = TempDir::new().unwrap();
    let agents_toml = config_dir.path().join("agents.toml");
    write_agents_toml(&agents_toml);

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(SkipExecutor);
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

    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let mut task = Task::from_user_input_ready("profile update skip", 3, default_channel());
    task.id = task_id;
    task.status = TaskStatus::Waiting(WaitingReason::Agent);
    app.world_mut().spawn((task, ShortTermMemory::default()));

    let candidate_id = uuid::Uuid::new_v4();
    let candidate = harness::ExperienceCandidate {
        candidate_id,
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: "physics fact".to_string(),
        kind_hint: harness::ExperienceKindHint::Knowledge,
        payload: harness::ExperienceCandidatePayload::Knowledge {
            content: "E=mc^2".to_string(),
        },
        dependency_refs: vec![],
        status: harness::ExperienceCandidateStatus::Persisted,
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
        existing_profile: Some(ExistingAgentProfile {
            name: "physics-specialist".to_string(),
            tags: vec!["physics".to_string()],
            description: "old description".to_string(),
        }),
        kind: ProfileGenerationKind::Update,
        feedback: None,
        retry_count: 0,
    });

    // 多轮 update：skip 不应生成审批请求
    for _ in 0..30 {
        app.update();
    }

    // 验证没有 profile_generation 审批请求
    let has_approval_request = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .any(|r| r.tool_name == "profile_generation" && r.pending_confirmation_id.is_some())
    };
    assert!(
        !has_approval_request,
        "skip 场景不应生成 profile_generation 审批请求"
    );

    // 验证 agents.toml 未变更
    let content = std::fs::read_to_string(&agents_toml).unwrap();
    let config: harness::domain::AgentConfig = toml::from_str(&content).unwrap();
    let physics = config
        .agent
        .iter()
        .find(|a| a.name == "physics-specialist")
        .expect("agents.toml 应仍包含 physics-specialist");
    assert_eq!(physics.tags, vec!["physics".to_string()], "tags 应保持不变");
    assert_eq!(
        physics.description, "old description",
        "description 应保持不变"
    );
}
