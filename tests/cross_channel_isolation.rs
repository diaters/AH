use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    ExternalInput, FrontendKind, HarnessConfig, OutputContent, ShortTermMemory, Task,
    TaskRoutingPolicy, TaskStatus, WaitingReason, build_harness_app, llm::ExecutorRegistry,
};
use tokio::runtime::Runtime;

fn telegram_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "tg-user".to_string(),
        thread_id: None,
    }
}

fn qq_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::QQ,
        user_id: "qq-user".to_string(),
        thread_id: None,
    }
}

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text("echo".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

fn make_waiting_task(channel: ChannelId) -> Task {
    let now = chrono::Utc::now();
    Task {
        id: uuid::Uuid::new_v4(),
        content: "waiting".to_string(),
        creator: uuid::Uuid::nil(),
        delegate: None,
        status: TaskStatus::Waiting(WaitingReason::User),
        pending_confirmation_id: None,
        input_summary: String::new(),
        result_summary: String::new(),
        priority: 0,
        created_at: now,
        updated_at: now,
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: true,
        parent_task_id: None,
        batch_id: None,
        origin_channel: Some(channel.clone()),
        routing_policy: TaskRoutingPolicy::conversational(channel.clone()),
        last_evaluated_turn: None,
    }
}

#[test]
fn cross_channel_plain_text_does_not_takeover_waiting_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    // Telegram 通道的 Waiting(User) 任务
    let tg_task_id = uuid::Uuid::new_v4();
    let mut tg_task = make_waiting_task(telegram_channel());
    tg_task.id = tg_task_id;
    app.world_mut().spawn((tg_task, ShortTermMemory::default()));

    // 从 QQ 通道发送纯文本
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: qq_channel(),
            content: "hello from QQ".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    // 断言：QQ 输入创建了新任务，Telegram 任务仍处于 Waiting(User)
    let tasks: Vec<_> = app.world_mut().query::<&Task>().iter(app.world()).collect();
    let tg_task = tasks
        .iter()
        .find(|t| t.id == tg_task_id)
        .expect("Telegram task should still exist");
    assert_eq!(
        tg_task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "Telegram task should still be Waiting(User), not taken over by QQ input"
    );

    let qq_tasks: Vec<_> = tasks
        .iter()
        .filter(|t| t.origin_channel == Some(qq_channel()))
        .collect();
    assert!(
        !qq_tasks.is_empty(),
        "QQ input should create a new task in QQ channel"
    );
}

#[test]
fn cross_channel_btw_does_not_pick_other_channel_parent() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    // QQ 通道的活跃任务（Waiting(User) 状态，避免被 task_dispatch 自动派发并完成，
    // 否则任务进入终态后 /btw 会无条件走回退分支，无法区分是通道过滤生效还是终态回退）。
    let now = chrono::Utc::now();
    app.world_mut().spawn((
        Task {
            id: uuid::Uuid::new_v4(),
            content: "qq active".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: None,
            input_summary: "qq".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(qq_channel()),
            routing_policy: TaskRoutingPolicy::conversational(qq_channel()),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    // 从 Telegram 通道发起 /btw
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: telegram_channel(),
            content: "/btw new topic".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    // 断言：Telegram 通道无父任务，走 CreateTaskMessage 分支
    let tasks: Vec<_> = app.world_mut().query::<&Task>().iter(app.world()).collect();
    let tg_tasks: Vec<_> = tasks
        .iter()
        .filter(|t| t.origin_channel == Some(telegram_channel()))
        .collect();
    assert!(
        !tg_tasks.is_empty(),
        "Telegram /btw should create a new task in Telegram channel"
    );
    // 没有同通道父任务时回退到 CreateTaskMessage 分支，content 为原始输入（"/btw new topic"）。
    // 关键断言是 Telegram 通道有新建任务，证明 /btw 没有选 QQ 通道的父任务。
    let tg_new_task = tg_tasks
        .iter()
        .find(|t| t.content == "/btw new topic")
        .expect("Telegram /btw task should use the original input as content");
    assert_eq!(tg_new_task.content, "/btw new topic");
    assert_eq!(tg_new_task.origin_channel, Some(telegram_channel()));
}

#[test]
fn cross_channel_finish_does_not_finish_other_channel_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    // QQ 通道的活跃任务（Waiting(User) 状态，避免被 task_dispatch 自动派发并完成，
    // 这样终态判定只受 /finish 命令影响，能更准确地验证跨通道隔离）。
    let qq_task_id = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    app.world_mut().spawn((
        Task {
            id: qq_task_id,
            content: "qq active".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: None,
            input_summary: "qq".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(qq_channel()),
            routing_policy: TaskRoutingPolicy::conversational(qq_channel()),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    // 从 Telegram 通道发起 /finish
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: telegram_channel(),
            content: "/finish".to_string(),
        })
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    // 断言：QQ 任务未终结
    let qq_task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == qq_task_id)
        .expect("QQ task should still exist");
    assert!(
        !qq_task.status.is_terminal(),
        "QQ task should not be terminated by Telegram /finish"
    );
}
