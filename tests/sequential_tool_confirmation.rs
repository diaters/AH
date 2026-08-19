//! 顺序工具审批集成测试
//!
//! 验证当 LLM 连续请求多个需要确认的工具时，审批请求按顺序弹出，
//! `allow_always` 可跳过后续审批，且 QQ 文本回复能正确解析为确认选项。

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use common::mock_executor::CannedExecutor;
use harness::prelude::*;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities, domain::AgentExecutionOutput,
    domain::AgentKind, domain::AgentProfile, domain::AgentToolPermissions, domain::ApprovalOption,
    domain::ChannelId, domain::EngineEvent, domain::EventTarget, domain::ExternalInput,
    domain::Frontend, domain::FrontendKind, domain::LlmToolCall, domain::LongTermMemory,
    domain::OutputContent, domain::ToolConfirmationResponseMessage,
    domain::ToolExecutionResultMessage, llm::ExecutorRegistry, systems::HarnessConfig,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn test_runtime() -> Arc<Runtime> {
    Arc::new(Runtime::new().expect("tokio runtime should be created"))
}

fn tui_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

fn qq_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "qq-test-group".to_string(),
        thread_id: None,
    }
}

fn tool_calls_output(calls: Vec<LlmToolCall>) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::ToolCalls(calls),
        reasoning_content: None,
    }
}

fn shell_exec_call(id: &str, command: &str) -> LlmToolCall {
    LlmToolCall {
        id: id.to_string(),
        name: "shell_exec".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
            model: Some("gpt-4.1-mini".to_string()),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: Some(harness::systems::BrainConfig { enabled: true }),
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

/// Helper function to spawn a default agent for tests
fn spawn_default_agent(app: &mut App) {
    // Brain agent（与 default-llm-agent 共存，供 BrainLlm 派发路径查找）
    let brain_id = harness::domain::AgentId::new();
    let brain_entity = app
        .world_mut()
        .spawn((
            Agent {
                id: brain_id,
                profile: AgentProfile {
                    name: "brain".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec!["brain".to_string()],
                    description: "Brain Agent".to_string(),
                },
                kind: AgentKind::Persistent,
                parent_id: None,
                bound_task_id: None,
                tool_permissions: AgentToolPermissions::default(),
                system_prompt: None,
            },
            LongTermMemory::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(brain_id, brain_entity);

    let default_id = harness::domain::AgentId::new();
    let default_entity = app
        .world_mut()
        .spawn((
            Agent {
                id: default_id,
                profile: AgentProfile {
                    name: "default-llm-agent".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec!["llm".to_string(), "default".to_string()],
                    description: "Default LLM Agent".to_string(),
                },
                kind: AgentKind::Persistent,
                parent_id: None,
                bound_task_id: None,
                tool_permissions: AgentToolPermissions::default(),
                system_prompt: None,
            },
            LongTermMemory::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(default_id, default_entity);
}

/// 测试资源：外部输入发送端。
#[derive(Resource)]
struct TestInputSender(crossbeam_channel::Sender<ExternalInput>);

/// 测试资源：前端事件捕获。
#[derive(Resource, Clone, Default)]
struct TestFrontendEvents(Arc<Mutex<Vec<EngineEvent>>>);

/// 测试资源：对 canned executor 的引用，便于测试设置回复序列。
#[derive(Resource, Clone)]
struct TestExecutorHandle(Arc<CannedExecutor>);

/// 测试资源：捕获 Tool 执行结果消息。
#[derive(Resource, Clone, Default)]
struct TestToolResults(Arc<Mutex<Vec<ToolExecutionResultMessage>>>);

fn capture_tool_results_system(
    results: Query<&ToolExecutionResultMessage, Added<ToolExecutionResultMessage>>,
    captured: ResMut<TestToolResults>,
) {
    for result in &results {
        captured.0.lock().unwrap().push(result.clone());
    }
}

/// 捕获所有前端事件的 MockFrontend。
struct CapturingFrontend {
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl Frontend for CapturingFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Tui
    }

    fn push_event(&self, event: EngineEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn poll_actions(&self) -> Vec<harness::domain::UserAction> {
        vec![]
    }
}

fn build_test_app() -> App {
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let runtime = test_runtime();
    let executor = Arc::new(CannedExecutor::new(Vec::new()));
    let executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let events = Arc::new(Mutex::new(Vec::new()));

    let frontend: Box<dyn Frontend> = Box::new(CapturingFrontend {
        events: events.clone(),
    });

    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![frontend],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize and spawn default agent
    app.update();
    spawn_default_agent(&mut app);

    app.insert_resource(TestInputSender(input_tx));
    app.insert_resource(TestFrontendEvents(events));
    app.insert_resource(TestExecutorHandle(executor));
    app.insert_resource(TestToolResults::default());
    // capture_tool_results_system 必须在 ingest_tool_results_system 之后运行
    // （用 Added<ToolExecutionResultMessage> 只捕获新 spawn 的结果），且在
    // tool_result_system 之前运行（tool_result_system 处理完会 despawn 结果）。
    // ingest → on_tool_returned_hook → tool_result 是既定顺序，capture 排在
    // ingest 之后，会在 on_tool_returned_hook 之前或之后运行——两者都在
    // tool_result 之前，所以 Added 能在 despawn 之前抓到结果。
    app.add_systems(
        Update,
        capture_tool_results_system.after(harness::systems::ingest_tool_results_system),
    );

    app
}

fn set_canned_responses(app: &mut App, responses: Vec<AgentExecutionOutput>) {
    app.world()
        .resource::<TestExecutorHandle>()
        .0
        .set_responses(responses);
}

fn run_ticks(app: &mut App, n: usize) {
    for _ in 0..n {
        app.update();
    }
}

/// 跑 N 次 update，每次之间 yield 20ms 让 async worker（shell_exec 上桥后
/// 经 spawn_blocking 跑子进程）有机会把结果送回通道。
///
/// shell_exec 改 Async 后，dispatch 不再阻塞主线程，但 ingest 需要 worker
/// 跑完才能落地结果——测试必须在 update 之间 sleep 让 worker 推进。
fn update_with_yield(app: &mut App, n: usize) {
    for _ in 0..n {
        app.update();
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn inject_user_input(app: &mut App, content: &str) {
    let sender = app.world().resource::<TestInputSender>().0.clone();
    sender
        .send(ExternalInput::TextWithChannel {
            channel: tui_channel(),
            content: content.to_string(),
        })
        .unwrap();
}

fn inject_qq_text(app: &mut App, content: &str) {
    let sender = app.world().resource::<TestInputSender>().0.clone();
    sender
        .send(ExternalInput::TextWithChannel {
            channel: qq_channel(),
            content: content.to_string(),
        })
        .unwrap();
}

fn inject_confirmation(app: &mut App, request_id: &Uuid, option: &str) {
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id: *request_id,
        selected_option: option.to_string(),
        feedback: None,
    });
}

fn collect_approval_requests(app: &mut App) -> Vec<Uuid> {
    let events = app
        .world()
        .resource::<TestFrontendEvents>()
        .0
        .lock()
        .unwrap();
    events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::ApprovalRequest { request_id, .. } => Some(*request_id),
            _ => None,
        })
        .collect()
}

#[derive(Debug)]
struct CapturedApprovalRequest {
    target: ChannelId,
    options: Vec<ApprovalOption>,
}

fn collect_qq_approval_requests(app: &mut App) -> Vec<CapturedApprovalRequest> {
    let events = app
        .world()
        .resource::<TestFrontendEvents>()
        .0
        .lock()
        .unwrap();
    events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::ApprovalRequest {
                target, options, ..
            } => {
                if let EventTarget::Directed(channels) = target {
                    channels
                        .iter()
                        .find(|c| c.frontend == FrontendKind::QQ)
                        .cloned()
                        .map(|target| CapturedApprovalRequest {
                            target,
                            options: options.clone(),
                        })
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

fn collect_tool_results(app: &mut App) -> Vec<ToolExecutionResultMessage> {
    app.world()
        .resource::<TestToolResults>()
        .0
        .lock()
        .unwrap()
        .clone()
}

#[test]
fn two_shell_execs_confirmed_sequentially() {
    let mut app = build_test_app();
    set_canned_responses(
        &mut app,
        vec![tool_calls_output(vec![
            shell_exec_call("call_a", "echo a"),
            shell_exec_call("call_b", "echo b"),
        ])],
    );

    inject_user_input(&mut app, "请执行 echo a 和 echo b");
    run_ticks(&mut app, 10);

    let approval_requests = collect_approval_requests(&mut app);
    assert_eq!(approval_requests.len(), 1, "应只弹出一个审批请求");

    inject_confirmation(&mut app, &approval_requests[0], "allow_once");
    run_ticks(&mut app, 10);

    let approval_requests = collect_approval_requests(&mut app);
    assert_eq!(approval_requests.len(), 2, "确认第一个后应弹出第二个");
}

#[test]
fn allow_always_skips_remaining_confirmations() {
    let mut app = build_test_app();
    set_canned_responses(
        &mut app,
        vec![tool_calls_output(vec![
            shell_exec_call("call_ok_1", "echo ok"),
            shell_exec_call("call_ok_2", "echo ok"),
        ])],
    );

    inject_user_input(&mut app, "请执行两次 echo ok");
    run_ticks(&mut app, 10);

    let first = collect_approval_requests(&mut app)
        .into_iter()
        .next()
        .expect("应有一个审批请求");

    inject_confirmation(&mut app, &first, "allow_always");
    run_ticks(&mut app, 20);

    let approval_requests = collect_approval_requests(&mut app);
    assert_eq!(
        approval_requests.len(),
        1,
        "allow_always 后不应再弹出新审批"
    );
}

#[test]
fn qq_text_confirmation_resolves_tool() {
    let mut app = build_test_app();
    set_canned_responses(
        &mut app,
        vec![tool_calls_output(vec![shell_exec_call(
            "call_qq", "echo qq",
        )])],
    );

    inject_qq_text(&mut app, "请执行 echo qq");
    run_ticks(&mut app, 10);

    let qq_approvals = collect_qq_approval_requests(&mut app);
    let approval = qq_approvals
        .iter()
        .find(|a| a.options.iter().any(|o| o.id == "allow_once"))
        .expect("QQ 应收到带选项的审批请求");
    assert_eq!(approval.target.frontend, FrontendKind::QQ);
    assert!(!approval.options.is_empty(), "审批选项不应为空");

    inject_qq_text(&mut app, "2");
    // shell_exec 上桥后异步执行：用户确认 → async_tool_dispatch_system 认领 →
    // spawn_blocking 跑子进程 → 通道回传 → ingest 落地。每次 update 之间 yield
    // 20ms 让 worker 推进，否则 ingest 拿不到结果。
    update_with_yield(&mut app, 30);

    let results = collect_tool_results(&mut app);
    assert!(!results.is_empty(), "工具应被执行");
    assert!(
        results.iter().any(|r| {
            r.tool_name == "shell_exec"
                && matches!(
                    &r.tool_output,
                    Ok(v) if v
                        .get("output")
                        .and_then(|s| s.as_str())
                        .map(|s| s.contains("qq"))
                        .unwrap_or(false)
                )
        }),
        "应执行 echo qq"
    );
}
