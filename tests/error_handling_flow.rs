//! Error handling and edge case integration tests
//!
//! Tests for retry mechanism, error handling, and boundary conditions.

use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutionError,
    ExecutorFuture, ExternalInput, FrontendKind, HarnessConfig, Task, TaskStatus, WaitingReason,
    build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}
use tokio::runtime::Runtime;

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
        agents_config_path: "agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        max_tool_iterations: 5,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
    }
}

/// Test: Task enters RetryBackoff on retryable errors
#[test]
fn task_enters_retry_backoff_on_rate_limit_error() {
    struct RateLimitExecutor;

    impl AgentExecutor for RateLimitExecutor {
        fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                Err(ExecutionError::RateLimited {
                    message: "rate limited".to_string(),
                    retry_after_secs: Some(1),
                })
            })
        }
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(RateLimitExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Create a task
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test retry", 3, default_channel()),
            harness::ShortTermMemory::default(),
        ))
        .id();

    // Run updates - task should enter RetryBackoff
    for _ in 0..5 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Verify task is in RetryBackoff state
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();

    assert_eq!(
        task.status,
        TaskStatus::Waiting(WaitingReason::RetryBackoff),
        "Task should be in RetryBackoff after rate limit error"
    );
    assert_eq!(task.retry_count, 1, "Retry count should be incremented");
    assert!(
        task.next_retry_at.is_some(),
        "Should have next_retry_at set"
    );
}

/// Test: Non-retryable error causes immediate failure
#[test]
fn non_retryable_error_causes_immediate_failure() {
    struct NonRetryableErrorExecutor;

    impl AgentExecutor for NonRetryableErrorExecutor {
        fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                // Authentication error is not retryable
                Err(ExecutionError::Authentication(
                    "invalid API key".to_string(),
                ))
            })
        }
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NonRetryableErrorExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Create a task
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test non-retryable", 3, default_channel()),
            harness::ShortTermMemory::default(),
        ))
        .id();

    // Run updates
    for _ in 0..10 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Verify task failed immediately (no retry)
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();

    assert!(
        matches!(task.status, TaskStatus::Failed(_)),
        "Task should fail immediately on non-retryable error"
    );

    // Verify retry count is 0 (no retries attempted)
    assert_eq!(
        task.retry_count, 0,
        "Non-retryable error should not trigger retries"
    );
}

/// Test: Empty user input creates task but handles gracefully
#[test]
fn empty_user_input_creates_task() {
    let runtime = Arc::new(Runtime::new().unwrap());

    struct EchoExecutor;
    impl AgentExecutor for EchoExecutor {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(format!("echo: {}", request.prompt)),
                    reasoning_content: None,
                })
            })
        }
    }
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);

    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Send empty input
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: "".to_string(),
        })
        .expect("should send");

    // Run updates
    for _ in 0..10 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Task should be created (even with empty content)
    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    // At least one task should exist
    assert!(
        !tasks.is_empty(),
        "Task should be created even with empty input"
    );
}

/// Test: Large input is handled without crashing
#[test]
fn large_input_is_handled() {
    let runtime = Arc::new(Runtime::new().unwrap());

    struct EchoExecutor;
    impl AgentExecutor for EchoExecutor {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(format!(
                        "processed {} chars",
                        request.prompt.len()
                    )),
                    reasoning_content: None,
                })
            })
        }
    }
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);

    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Create large input (100KB)
    let large_content = "x".repeat(100_000);
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: large_content.clone(),
        })
        .expect("should send");

    // Run updates
    for _ in 0..15 {
        app.update();
        thread::sleep(Duration::from_millis(30));
    }

    // Task should be created and processed
    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert!(!tasks.is_empty(), "Task should be created for large input");

    // Task should be in a valid state (not crashed)
    let task = tasks.first().unwrap();
    assert!(
        !matches!(task.status, TaskStatus::Failed(_)),
        "Task with large input should not fail"
    );
}

/// Test: Multiple concurrent tasks are handled correctly
#[test]
fn multiple_concurrent_tasks_are_handled() {
    let runtime = Arc::new(Runtime::new().unwrap());

    struct EchoExecutor;
    impl AgentExecutor for EchoExecutor {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                // Small delay to simulate concurrent execution
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(format!(
                        "response for task {}",
                        request.task_id
                    )),
                    reasoning_content: None,
                })
            })
        }
    }
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);

    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Create multiple tasks simultaneously
    let task_count = 5;
    for i in 0..task_count {
        app.world_mut().spawn((
            Task::from_user_input_ready(format!("task {}", i), 3, default_channel()),
            harness::ShortTermMemory::default(),
        ));
    }

    // Run updates until all tasks complete
    for _ in 0..20 {
        app.update();
        thread::sleep(Duration::from_millis(30));
    }

    // Verify all tasks completed
    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), task_count, "Should have {} tasks", task_count);

    let done_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    assert_eq!(
        done_count, task_count,
        "All {} tasks should be Done",
        task_count
    );
}

/// Test: Task in Waiting(User) state waits for user input
#[test]
fn waiting_task_waits_for_user_input() {
    let runtime = Arc::new(Runtime::new().unwrap());

    struct EchoExecutor;
    impl AgentExecutor for EchoExecutor {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(format!("response: {}", request.prompt)),
                    reasoning_content: None,
                })
            })
        }
    }
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);

    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Create a multi-turn task in Waiting(User) state
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "multi-turn".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: "waiting".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: default_channel(),
            last_evaluated_turn: None,
        },
        harness::ShortTermMemory::default(),
    ));

    // Run updates without providing user input
    for _ in 0..5 {
        app.update();
    }

    // Task should still be waiting
    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert!(
        matches!(tasks[0].status, TaskStatus::Waiting(WaitingReason::User)),
        "Task should remain in Waiting(User) state"
    );
}

/// Test: Task failure sets error message
#[test]
fn task_failure_sets_error_message() {
    struct FailExecutor;

    impl AgentExecutor for FailExecutor {
        fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
            Box::pin(async move {
                // Use non-retryable error to ensure immediate failure
                Err(ExecutionError::Authentication(
                    "invalid credentials".to_string(),
                ))
            })
        }
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(FailExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();

    // Create a task
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test failure", 3, default_channel()),
            harness::ShortTermMemory::default(),
        ))
        .id();

    // Run updates
    for _ in 0..10 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Verify task failed with error message
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();

    assert!(
        matches!(task.status, TaskStatus::Failed(_)),
        "Task should be Failed"
    );
    assert!(task.last_error.is_some(), "Task should have error message");
    assert!(
        task.last_error.unwrap().contains("invalid credentials"),
        "Error message should contain original error"
    );
}
