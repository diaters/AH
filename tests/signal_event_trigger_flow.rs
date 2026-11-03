mod common;

use std::sync::{Arc, Mutex};

use common::mock_executor::PanickingExecutor;
use crossbeam_channel::unbounded;
use harness::channels::ChannelManager;
use harness::domain::{
    ChannelId, ConfirmationOption, EngineEvent, EventTaskRoute, ExternalInput, Frontend,
    FrontendKind, Signal, SignalSource, SignalTriggerRegistry, Task,
    ToolConfirmationRequestMessage, UserAction, UserOutputMessage,
};
use harness::{
    app::build_harness_app, domain::AgentExecutor, llm::ExecutorRegistry, systems::HarnessConfig,
};
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

#[test]
fn registered_webhook_creates_task_and_routes_approval() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (input_tx, input_rx) = unbounded();
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PanickingExecutor);
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
        progress: false,
        task_id,
        content: "normal output should be dropped".to_string(),
    });
    app.world_mut().spawn(ToolConfirmationRequestMessage {
        request_id: Uuid::new_v4(),
        task_id,
        agent_id: harness::domain::AgentId::nil(),
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({"command": "date"}),
        options: ConfirmationOption::default_options(),
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

/// 静态 timer 路由已移除：非 `scheduled:` 前缀的定时信号（如旧 `[timer]` 配置的 kind）
/// 不再创建任务。
#[test]
fn non_scheduled_timer_signal_is_dropped_without_creating_task() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (_input_tx, input_rx) = unbounded();
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PanickingExecutor);
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
    app.world_mut().spawn(Signal::timer(
        SignalSource("timer".to_string()),
        "daily_summary",
    ));
    app.update();
    app.update();

    let mut query = app.world_mut().query::<&Task>();
    assert_eq!(query.iter(app.world()).count(), 0);
}

#[test]
fn unregistered_webhook_is_dropped_without_creating_task() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (input_tx, input_rx) = unbounded();
    let runtime = Arc::new(tokio::runtime::Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PanickingExecutor);
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
