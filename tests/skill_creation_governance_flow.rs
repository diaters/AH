//! /skill 候选治理触发集成测试（2026-08-16 修复）
//!
//! 验证 SkillCreation WorkItem 完成后：
//! 1. 有候选提交 → 候选推进 GovernancePending + WorkItem 保留（供批准后写回）
//! 2. 无候选提交 → WorkItem fail 清理，候选不被推进

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutionResult,
    AgentExecutor, AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, ChannelId,
    ExecutorFuture, ExperienceCandidate, ExperienceCandidateStatus, FrontendKind, HarnessConfig,
    LongTermMemory, ShortTermMemory, SkillCreationContext, Task, WorkItem, WorkItemStatus,
    WorkItemType, build_harness_app, llm::ExecutorRegistry,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

struct TextExecutor;

impl AgentExecutor for TextExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("已创建 skill".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: None,
        agents_config_path: "/nonexistent_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        max_tool_iterations: 5,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "/nonexistent_providers.toml".to_string(),
    }
}

/// 构造 SkillCreation 完成场景，返回 (app, task_id, candidate_id)。
/// - `submit_candidate` 控制是否向 ExperienceStore 预置候选
/// - `sandbox_dir` 指定 SkillCreationContext.sandbox_dir
/// - `spawn_result` 控制是否 spawn AgentExecutionResultMessage（触发治理推进）
fn setup_with(
    submit_candidate: bool,
    sandbox_dir: std::path::PathBuf,
    spawn_result: bool,
) -> (bevy_app::App, uuid::Uuid, uuid::Uuid) {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(TextExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();

    let task = Task::from_user_input_ready("创建新闻 skill", 3, default_channel());
    let task_id = task.id;
    app.world_mut().spawn((task, ShortTermMemory::default()));

    // 治理请求的 agent 需存在，否则 governance 系统会直接 despawn 请求
    let governing_agent_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: governing_agent_id,
            profile: AgentProfile {
                name: "default".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["default".to_string()],
                description: "Default Agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        },
        LongTermMemory::default(),
    ));

    let mut work_item = WorkItem::skill_creation(
        task_id,
        "create skill".to_string(),
        vec![],
        vec![],
        governing_agent_id,
    );
    let work_item_id = work_item.id;
    work_item.status = WorkItemStatus::Running;
    work_item.assigned_agent = Some(governing_agent_id);
    app.world_mut().spawn((
        work_item,
        SkillCreationContext {
            task_id,
            agent_id: governing_agent_id,
            agent_name: "default".to_string(),
            sandbox_dir,
            skill_name: "daily-news".to_string(),
        },
    ));

    let candidate_id = uuid::Uuid::new_v4();
    if submit_candidate {
        let candidate = ExperienceCandidate::skill_new(
            candidate_id,
            task_id,
            governing_agent_id,
            "daily-news skill".to_string(),
            "daily-news".to_string(),
            "获取当天新闻".to_string(),
            "## 步骤\n1. 打开新闻网站".to_string(),
            vec![],
        );
        app.world_mut()
            .resource_mut::<harness::ExperienceStore>()
            .stage_root_candidate(candidate);
    }

    if spawn_result {
        let result = AgentExecutionResult {
            task_id,
            agent_id: governing_agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("已创建 skill".to_string()),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: Some(work_item_id),
            conversation: None,
        };
        app.world_mut()
            .spawn(harness::AgentExecutionResultMessage { result });
    }

    (app, task_id, candidate_id)
}

/// 默认场景：提交候选 + 触发治理推进。
fn setup(submit_candidate: bool) -> (bevy_app::App, uuid::Uuid, uuid::Uuid) {
    setup_with(
        submit_candidate,
        std::path::PathBuf::from("/tmp/test-sandbox"),
        true,
    )
}

/// 隔离的批准场景：不触发治理系统，直接测试批准→写回路径。
fn setup_for_approval() -> (bevy_app::App, uuid::Uuid, uuid::Uuid) {
    setup_with(true, std::path::PathBuf::from("/tmp/test-sandbox"), false)
}

#[test]
fn skill_creation_completion_promotes_candidate_and_requests_governance() {
    let (mut app, _task_id, candidate_id) = setup(true);

    app.update();

    // WorkItem 应保留（供批准后写回系统读取 SkillCreationContext）
    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .filter(|wi| wi.work_type == WorkItemType::SkillCreation)
        .collect();
    assert_eq!(
        work_items.len(),
        1,
        "SkillCreation WorkItem should be kept alive for writeback after completion"
    );

    // 候选越过 Submitted（GovernancePending 或同帧被治理消费为 NeedsUserApproval）
    let store = app.world().resource::<harness::ExperienceStore>();
    let status = store
        .candidates
        .get(&candidate_id)
        .map(|c| c.status.clone());
    assert!(
        matches!(
            status,
            Some(ExperienceCandidateStatus::GovernancePending)
                | Some(ExperienceCandidateStatus::NeedsUserApproval)
        ),
        "candidate should be promoted beyond Submitted, got {:?}",
        status
    );
}

#[test]
fn skill_creation_approval_finds_context_and_triggers_writeback() {
    // 回归测试（2026-08-16 日志中的 SkillCreationContextNotFound bug）：
    // 批准后必须能找到 SkillCreationContext，否则写回永不触发、skill 不会发布。
    let (mut app, task_id, candidate_id) = setup_for_approval();

    // 手动模拟治理决议（写回目标为 SkillCreation，需用户确认）
    let request_id = uuid::Uuid::new_v4();
    app.world_mut()
        .spawn(harness::domain::ExperienceGovernanceDecision {
            candidate_id,
            destination: harness::domain::ExperienceWritebackDestination::SkillCreation,
            requires_user_confirmation: true,
            decision_rationale: "approved by test".to_string(),
            source_task_id: task_id,
        });

    // 绑定 approval request 到候选
    app.world_mut()
        .resource_mut::<harness::ExperienceStore>()
        .bind_approval_request(request_id, candidate_id);

    // 模拟 spawn_experience_confirmation 的配对 ToolExecutionRequestMessage：
    // tool_confirmation_result_system 依赖 pending_confirmation_id + tool_name 识别
    // experience_governance 确认，保留响应实体交给 experience_approval_result_system；
    // 缺少该配对实体会被 tool_confirmation_result_system 提前 despawn 响应（真实 bug 路径）。
    app.world_mut()
        .spawn(harness::domain::ToolExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id,
                agent_id: uuid::Uuid::nil(),
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
            tool_input: serde_json::json!({ "candidate_id": candidate_id.to_string() }),
            pending_confirmation_id: Some(request_id),
            tool_call_id: None,
            pending_confirmation_options: Some(
                harness::domain::ConfirmationOption::default_options(),
            ),
            work_item_entity: None,
            confirmed_once: false,
        });

    // 用户批准
    app.world_mut()
        .spawn(harness::domain::ToolConfirmationResponseMessage {
            request_id,
            selected_option: "allow_once".to_string(),
            feedback: None,
        });

    // 推进若干帧，让 approval 插入写回消息、writeback 系统消费并清理
    for _ in 0..3 {
        app.update();
    }

    // 上下文被找到 → 写回被触发 → WorkItem 被 writeback 系统 despawn。
    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .filter(|wi| wi.work_type == WorkItemType::SkillCreation)
        .collect();
    assert!(
        work_items.is_empty(),
        "writeback should despawn the SkillCreation WorkItem after approval"
    );

    // 沙箱 /tmp/test-sandbox 不存在 → rename 失败 → 候选置为 WritebackFailed；
    // 若上下文缺失（bug），候选停留在 WritebackPending 且 writeback 永不触发。
    let store = app.world().resource::<harness::ExperienceStore>();
    let status = store
        .candidates
        .get(&candidate_id)
        .map(|c| c.status.clone());
    assert!(
        matches!(
            status,
            Some(ExperienceCandidateStatus::WritebackFailed)
                | Some(ExperienceCandidateStatus::Persisted)
        ),
        "writeback should have been attempted after approval (context found), \
         got {:?}",
        status
    );
}

#[test]
fn skill_creation_completion_without_candidate_fails_silently() {
    let (mut app, task_id, _candidate_id) = setup(false);

    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .filter(|wi| wi.work_type == WorkItemType::SkillCreation)
        .collect();
    assert!(
        work_items.is_empty(),
        "SkillCreation WorkItem should be despawned even without submission"
    );

    // 无候选提交 → 不应 spawn 治理请求，候选推进逻辑不触发
    let governance_requests = app
        .world_mut()
        .query::<&harness::domain::ExperienceGovernanceRequestMessage>()
        .iter(app.world())
        .filter(|m| m.task_id == task_id)
        .count();
    assert_eq!(
        governance_requests, 0,
        "no governance request should be spawned without a submitted candidate"
    );
}
