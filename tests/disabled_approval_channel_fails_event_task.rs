//! 未启用审批前端的事件任务失败集成测试
//!
//! 验证事件任务的 approval_channel 指向未注册的 QQ frontend 时，任务被标记为
//! Failed(Unknown) 并记录正确的错误信息。

use std::sync::{Arc, Mutex};

use crossbeam_channel::unbounded;
use harness::domain::{
    ChannelId, ConfirmationOption, ConfirmationSource, EngineEvent, FailureReason, Frontend,
    FrontendKind, Task, TaskRoutingPolicy, TaskStatus, ToolConfirmationRequestMessage, UserAction,
};
use harness::{
    AgentExecutionRequest, AgentExecutor, ExecutorFuture, HarnessConfig, build_harness_app,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

struct MockTelegramFrontend {
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl Frontend for MockTelegramFrontend {
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
fn disabled_approval_channel_marks_event_task_failed() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (_input_tx, input_rx) = unbounded();
    let runtime = Arc::new(Runtime::new().expect("runtime"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoopExecutor);
    let (channel_manager, _) = harness::channels::ChannelManager::empty();

    let mut app = build_harness_app(
        HarnessConfig::default(),
        runtime,
        executor,
        input_rx,
        vec![Box::new(MockTelegramFrontend {
            events: events.clone(),
        })],
        channel_manager,
    );

    let approval_channel = ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "reviewer".to_string(),
        thread_id: None,
    };

    let task = Task::from_trigger(
        "event task with disabled approval channel".to_string(),
        3,
        TaskRoutingPolicy::event(Some(approval_channel), Some("test".to_string())),
    );
    let task_id = task.id;
    app.world_mut().spawn(task);

    app.world_mut().spawn(ToolConfirmationRequestMessage {
        request_id: Uuid::new_v4(),
        task_id,
        agent_id: Uuid::nil(),
        tool_name: "shell_exec".to_string(),
        tool_input: serde_json::json!({"command": "date"}),
        options: ConfirmationOption::default_options(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
        approval_context: Some("test".to_string()),
    });

    app.update();

    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|task| task.id == task_id)
        .expect("task should remain for failure inspection")
        .clone();

    assert_eq!(
        task.status,
        TaskStatus::Failed(FailureReason::Unknown),
        "任务应进入 Failed(Unknown) 状态"
    );
    assert_eq!(
        task.last_error.as_deref(),
        Some("approval channel frontend 'qq' is not enabled"),
        "last_error 应指出 QQ frontend 未启用"
    );

    // 未注册 QQ frontend，不应发出 ApprovalRequest 事件。
    let events = events.lock().unwrap();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EngineEvent::ApprovalRequest { .. })),
        "不应向未注册的 QQ frontend 发出审批请求"
    );
}
