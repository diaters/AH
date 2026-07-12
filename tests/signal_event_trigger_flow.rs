use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use harness::channels::ChannelManager;
use harness::domain::{
    AgentExecutionRequest, ChannelId, ConfirmationOption, ConfirmationSource, EngineEvent,
    EventTaskRoute, ExecutorFuture, ExternalInput, Frontend, FrontendKind, Signal, SignalSource,
    SignalTriggerRegistry, Task, ToolConfirmationRequestMessage, UserAction, UserOutputMessage,
};
use harness::{AgentExecutor, HarnessConfig, build_harness_app, llm::ExecutorRegistry};
use uuid::Uuid;

struct MockFrontend {
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl Frontend for MockFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Telegram
    }

    fn push_event(&self, event: EngineEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
}

struct NoopExecutor;

impl AgentExecutor for NoopExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async { panic!("executor should not run in this test") })
    }
}

#[test]
fn registered_webhook_creates_task_and_routes_approval() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (input_tx, input_rx) = unbounded();
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoopExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (channel_manager, _) = ChannelManager::empty();
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![Box::new(MockFrontend {
            events: events.clone(),
        })],
        channel_manager,
    );
    let mut registry = SignalTriggerRegistry::default();
    registry.register_webhook(
        "github.issue_opened",
        EventTaskRoute {
            prompt_template: "请分析新 issue".to_string(),
            approval_channel: Some(ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "reviewer".to_string(),
                thread_id: Some("ops".to_string()),
            }),
            approval_context: "GitHub issue opened".to_string(),
        },
    );
    app.world_mut().insert_resource(registry);
    input_tx
        .send(ExternalInput::Webhook {
            source: SignalSource("external:github".to_string()),
            kind: "github.issue_opened".to_string(),
            body: serde_json::json!({"title": "bug"}),
        })
        .expect("send webhook input");
    app.update();
    app.update(); // 让 CreateTaskMessage 转成 Task

    let mut query = app.world_mut().query::<&Task>();
    let task = query
        .iter(app.world())
        .find(|task| task.content == "请分析新 issue")
        .expect("task should be created")
        .clone();
    let task_id = task.id;

    app.world_mut().spawn(UserOutputMessage {
        task_id,
        content: "normal output should be dropped".to_string(),
    });
    app.world_mut().spawn(ToolConfirmationRequestMessage {
        request_id: Uuid::new_v4(),
        task_id,
        agent_id: Uuid::nil(),
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({"command": "date"}),
        options: ConfirmationOption::default_options(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
        approval_context: Some("GitHub issue opened".to_string()),
    });
    app.update();

    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EngineEvent::ApprovalRequest { .. }))
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        EngineEvent::Text { content, .. } if content == "normal output should be dropped"
    )));
}

#[test]
fn registered_timer_creates_task_without_output_channel() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (_input_tx, input_rx) = unbounded();
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoopExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (channel_manager, _) = ChannelManager::empty();
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![Box::new(MockFrontend { events })],
        channel_manager,
    );
    let mut registry = SignalTriggerRegistry::default();
    registry.register_timer(
        "daily_summary",
        EventTaskRoute {
            prompt_template: "执行每日摘要".to_string(),
            approval_channel: Some(ChannelId {
                frontend: FrontendKind::Telegram,
                user_id: "reviewer".to_string(),
                thread_id: None,
            }),
            approval_context: "daily summary timer".to_string(),
        },
    );
    app.world_mut().insert_resource(registry);
    app.world_mut().spawn(Signal::timer(
        SignalSource("scheduler:daily".to_string()),
        "daily_summary",
    ));
    app.update();
    app.update();

    let mut query = app.world_mut().query::<&Task>();
    let task = query
        .iter(app.world())
        .find(|task| task.content == "执行每日摘要")
        .expect("timer should create task");
    assert_eq!(task.origin_channel, None);
    assert_eq!(task.routing_policy.output_channel, None);
}

#[test]
fn unregistered_webhook_is_dropped_without_creating_task() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (input_tx, input_rx) = unbounded();
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoopExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (channel_manager, _) = ChannelManager::empty();
    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor_registry,
        input_rx,
        vec![Box::new(MockFrontend { events })],
        channel_manager,
    );
    app.world_mut()
        .insert_resource(SignalTriggerRegistry::default());
    input_tx
        .send(ExternalInput::Webhook {
            source: SignalSource("external:github".to_string()),
            kind: "github.unregistered".to_string(),
            body: serde_json::json!({"title": "bug"}),
        })
        .expect("send webhook input");
    app.update();
    app.update();

    let mut query = app.world_mut().query::<&Task>();
    assert_eq!(query.iter(app.world()).count(), 0);
}
