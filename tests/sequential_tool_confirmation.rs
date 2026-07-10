//! 顺序工具审批集成测试
//!
//! 验证当 LLM 连续请求多个需要确认的工具时，审批请求按顺序弹出，
//! `allow_always` 可跳过后续审批，且 QQ 文本回复能正确解析为确认选项。

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use harness::prelude::*;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, AgentRequestKind, ApprovalOption,
    ChannelId, EngineEvent, EventTarget, ExecutorFuture, ExternalInput, Frontend, FrontendKind,
    HarnessConfig, LlmToolCall, OutputContent, ToolConfirmationResponseMessage,
    ToolExecutionResultMessage, build_harness_app,
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

fn text_output(text: &str) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::Text(text.to_string()),
        reasoning_content: None,
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

/// 按顺序返回预设 LLM 输出的执行器，用于端到端测试。
struct CannedExecutor {
    responses: Mutex<VecDeque<AgentExecutionOutput>>,
}

impl CannedExecutor {
    fn new(responses: Vec<AgentExecutionOutput>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }

    fn set_responses(&self, responses: Vec<AgentExecutionOutput>) {
        *self.responses.lock().unwrap() = responses.into();
    }
}

impl AgentExecutor for CannedExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        // 治理型 WorkItem / 非普通 LLM 请求直接返回占位文本，避免干扰主流程。
        if request.work_item_id.is_some() || request.request_kind != AgentRequestKind::LlmCompletion
        {
            return Box::pin(async move { Ok(text_output("ok")) });
        }

        let response = self.responses.lock().unwrap().pop_front();
        Box::pin(async move { Ok(response.unwrap_or_else(|| text_output("done"))) })
    }
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
    results: Query<&ToolExecutionResultMessage>,
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

    fn poll_actions(&self) -> Vec<harness::UserAction> {
        vec![]
    }
}

fn build_test_app() -> App {
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let runtime = test_runtime();
    let executor = Arc::new(CannedExecutor::new(Vec::new()));
    let events = Arc::new(Mutex::new(Vec::new()));

    let frontend: Box<dyn Frontend> = Box::new(CapturingFrontend {
        events: events.clone(),
    });

    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor.clone(),
        input_rx,
        vec![frontend],
        harness::channels::ChannelManager::empty().0,
    );

    app.insert_resource(TestInputSender(input_tx));
    app.insert_resource(TestFrontendEvents(events));
    app.insert_resource(TestExecutorHandle(executor));
    app.insert_resource(TestToolResults::default());
    app.add_systems(Update, capture_tool_results_system);

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
    run_ticks(&mut app, 20);

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
