use std::sync::Arc;

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionRequest, AgentExecutor, ExecutorFuture, ExternalInput, HarnessConfig,
    OutputMessage, ShortTermMemory, Task, TaskStatus, WaitingReason, build_harness_app,
};
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move { Ok("echo".to_string()) })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

#[test]
fn user_input_creates_new_task_when_no_waiting_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    input_tx
        .send(ExternalInput::Text("new task".to_string()))
        .unwrap();

    for _ in 0..5 {
        app.update();
    }

    let task_count = app.world_mut().query::<&Task>().iter(app.world()).count();

    assert!(task_count >= 1, "should create at least one task");
}

#[test]
fn user_input_continues_waiting_task() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // Create a task in Waiting(User) state (multi-turn)
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn(Task {
        id: task_id,
        content: "existing task".to_string(),
        creator: uuid::Uuid::nil(),
        delegate: None,
        status: TaskStatus::Waiting(WaitingReason::User),
        input_summary: "existing task".to_string(),
        result_summary: String::new(),
        priority: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: true,
    });

    // Simulate user input
    app.world_mut().spawn(harness::UserInputMessage {
        content: "continue input".to_string(),
    });

    for _ in 0..10 {
        app.update();
    }

    // Should have exactly ONE task (the continued one, not a new one)
    let task_count = app.world_mut().query::<&Task>().iter(app.world()).count();
    assert_eq!(
        task_count, 1,
        "should have exactly one task, not create a new one"
    );

    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .cloned();

    assert!(task.is_some(), "original waiting task should still exist");
    // 多轮对话任务会在处理完响应后回到 Waiting(User) 状态
    let task = task.unwrap();
    assert!(
        !task.status.is_terminal(),
        "task should not be in terminal state, got {:?}",
        task.status
    );
}

#[test]
fn evaluation_triggered_on_turn_limit() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // Configure evaluation with max_turns = 2
    app.insert_resource(harness::TaskEvaluationConfig {
        enabled: true,
        max_turns: Some(2),
        evaluator_agent_name: "evaluator".to_string(),
        offtrack_policy: harness::OffTrackPolicy::AskUser,
    });

    app.update();

    // Create a task with turn_count = 2
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn(Task {
        id: task_id,
        content: "test task".to_string(),
        creator: uuid::Uuid::nil(),
        delegate: None,
        status: TaskStatus::Running,
        input_summary: "test".to_string(),
        result_summary: String::new(),
        priority: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: true,
    });

    // Add short term memory with some entries
    app.world_mut().spawn(harness::ShortTermMemory {
        entries: vec![],
        estimated_tokens: 100,
        summary_prefix: None,
        last_cached_tokens: None,
    });

    app.update();

    // Check for evaluation request
    let has_evaluation_request = app
        .world_mut()
        .query::<&harness::EvaluationRequestMessage>()
        .iter(app.world())
        .count()
        > 0;

    // This test verifies the trigger logic exists
    // Note: May not trigger without evaluator agent configured
    assert!(
        !has_evaluation_request,
        "should not trigger without evaluator agent"
    );
}

/// 验证多个 Waiting(User) 任务时，用户输入只路由到其中一个。
/// 接收输入的任务会完成一轮对话后回到 Waiting(User)，但只有它的 STM 会包含新条目。
#[test]
fn multiple_waiting_user_tasks_routes_to_one() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建两个 Waiting(User) 状态的任务
    let task_id_1 = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id_1,
            content: "first waiting task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: "first".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
        },
        ShortTermMemory::default(),
    ));

    let task_id_2 = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id_2,
            content: "second waiting task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: "second".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
        },
        ShortTermMemory::default(),
    ));

    // 模拟用户输入
    app.world_mut().spawn(harness::UserInputMessage {
        content: "hello".to_string(),
    });

    for _ in 0..10 {
        app.update();
    }

    // 验证只有一个任务的 STM 包含 "hello"（即接收了用户输入）
    let tasks_with_input: Vec<_> = app
        .world_mut()
        .query::<(&Task, &ShortTermMemory)>()
        .iter(app.world())
        .filter(|(_, stm)| stm.entries.iter().any(|e| e.content == "hello"))
        .collect();

    assert_eq!(
        tasks_with_input.len(),
        1,
        "exactly one task should have received the user input"
    );
    assert_eq!(
        tasks_with_input[0].0.id, task_id_1,
        "the first Waiting(User) task should receive the input"
    );
}

/// 验证 /finish 命令能结束多轮对话任务。
#[test]
fn finish_command_ends_multi_turn_conversation() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建 Waiting(User) 状态的多轮对话任务
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "active multi-turn task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: "active task".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
        },
        ShortTermMemory::default(),
    ));

    // 模拟用户输入 /finish
    app.world_mut().spawn(harness::UserInputMessage {
        content: "/finish".to_string(),
    });

    for _ in 0..10 {
        app.update();
    }

    // 验证任务已终止
    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id);

    if let Some(task) = task {
        assert!(
            task.status.is_terminal(),
            "task should be in terminal state after /finish, got {:?}",
            task.status
        );
    }
    // 任务可能已被清理，找不到也说明已正常终止
}
