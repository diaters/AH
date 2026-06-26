//! Summarization flow integration tests
//!
//! Tests for Phase 4.3 LLM-based memory summarization feature.

use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, AgentRequestKind, ChannelId,
    ExecutorFuture, FrontendKind, HarnessConfig, ShortTermMemory, Task, TaskStatus, WaitingReason,
    build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
    }
}
use tokio::runtime::Runtime;

/// Mock executor that returns different responses based on request kind
struct SummarizationMockExecutor {
    /// Track if summarization was requested
    summarization_called: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SummarizationMockExecutor {
    fn new() -> Self {
        Self {
            summarization_called: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl AgentExecutor for SummarizationMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let summarization_called = self.summarization_called.clone();
        Box::pin(async move {
            match request.request_kind {
                AgentRequestKind::Summarization => {
                    summarization_called.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(AgentExecutionOutput { content: harness::OutputContent::Text("这是一个测试摘要。".to_string()), reasoning_content: None })
                }
                AgentRequestKind::LlmCompletion => Ok(AgentExecutionOutput { content: harness::OutputContent::Text(format!("response: {}", request.prompt)), reasoning_content: None }),
                AgentRequestKind::BrainDecision => {
                    Ok(AgentExecutionOutput { content: harness::OutputContent::Text(r#"{"selected_agent_name":"default-llm-agent","delegate_prompt":"test","reasoning":"test"}"#.to_string()), reasoning_content: None })
                }
                AgentRequestKind::ToolExecution { .. } => {
                    Err(harness::ExecutionError::Unknown("Not supported".to_string()))
                }
                AgentRequestKind::Evaluation => {
                    Ok(AgentExecutionOutput { content: harness::OutputContent::Text(r#"{"decision":"Continue","reasoning":"test"}"#.to_string()), reasoning_content: None })
                }
            }
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
    }
}

/// Test: Task completion triggers summarization when ShortTermMemory has entries
#[test]
fn task_completion_triggers_summarization() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let summarization_called = executor.summarization_called.clone();
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor.clone(), input_rx, vec![]);

    // Initialize
    app.update();

    // Create a single-turn task with ShortTermMemory containing entries
    let task_id = uuid::Uuid::new_v4();
    let entity_id = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "test task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Ready,
                input_summary: String::new(),
                result_summary: String::new(),
                priority: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                retry_count: 0,
                max_retries: 3,
                next_retry_at: None,
                last_error: None,
                multi_turn: false,
                parent_task_id: None,
                batch_id: None,
                origin_channel: default_channel(),
                last_evaluated_turn: None,
            },
            ShortTermMemory {
                entries: vec![
                    harness::MemoryEntry::new(harness::EntryRole::User, "user message"),
                    harness::MemoryEntry::new(harness::EntryRole::Assistant, "assistant response"),
                ],
                summary_prefix: None,
                estimated_tokens: 100,
                last_cached_tokens: None,
            },
        ))
        .id();

    // Run until task completes
    for _ in 0..15 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Verify task is done
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();
    assert_eq!(task.status, TaskStatus::Done, "Task should be Done");

    // Verify summarization was triggered
    assert!(
        summarization_called.load(std::sync::atomic::Ordering::SeqCst),
        "Summarization should be triggered on task completion"
    );
}

/// Test: Multi-turn task does not trigger summarization mid-conversation
#[test]
fn multi_turn_task_does_not_trigger_summarization_mid_conversation() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let summarization_called = executor.summarization_called.clone();
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    // Initialize
    app.update();

    // Create a multi-turn task in Waiting(User) state
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "multi-turn task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            input_summary: "waiting for user".to_string(),
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
        ShortTermMemory {
            entries: vec![
                harness::MemoryEntry::new(harness::EntryRole::User, "user message"),
                harness::MemoryEntry::new(harness::EntryRole::Assistant, "assistant response"),
            ],
            summary_prefix: None,
            estimated_tokens: 100,
            last_cached_tokens: None,
        },
    ));

    // Run a few frames
    for _ in 0..5 {
        app.update();
    }

    // Summarization should NOT be called for a task waiting for user
    assert!(
        !summarization_called.load(std::sync::atomic::Ordering::SeqCst),
        "Summarization should not be triggered for waiting task"
    );
}

/// Test: Summarization does not change terminal task status
#[test]
fn summarization_preserves_terminal_task_status() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    // Initialize
    app.update();

    // Create a task that will complete (single-turn)
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("complete this task", 3, default_channel()),
            ShortTermMemory {
                entries: vec![harness::MemoryEntry::new(harness::EntryRole::User, "hello")],
                summary_prefix: None,
                estimated_tokens: 50,
                last_cached_tokens: None,
            },
        ))
        .id();

    // Run until task completes and summarization finishes
    for _ in 0..20 {
        app.update();
        thread::sleep(Duration::from_millis(30));

        // Check if task is still Done after each update
        if let Some(task) = app.world_mut().get::<Task>(entity_id).cloned()
            && task.status == TaskStatus::Done
        {
            // Run a few more updates to ensure summarization doesn't change status
            for _ in 0..5 {
                app.update();
                thread::sleep(Duration::from_millis(20));
            }
            break;
        }
    }

    // Final check: task must still be Done
    let task = app
        .world_mut()
        .get::<Task>(entity_id)
        .cloned()
        .expect("Task should exist");
    assert_eq!(
        task.status,
        TaskStatus::Done,
        "Task status should remain Done after summarization"
    );
}

/// Test: ShortTermMemory without entries before execution gets entries during execution
/// and triggers summarization
#[test]
fn execution_populates_memory_and_triggers_summarization() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let summarization_called = executor.summarization_called.clone();
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor.clone(), input_rx, vec![]);

    // Initialize
    app.update();

    // Create a single-turn task with pre-populated ShortTermMemory
    // (simulating what happens after llm_response_system runs)
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test", 3, default_channel()),
            ShortTermMemory {
                entries: vec![harness::MemoryEntry::new(
                    harness::EntryRole::User,
                    "user input",
                )],
                summary_prefix: None,
                estimated_tokens: 50,
                last_cached_tokens: None,
            },
        ))
        .id();

    // Run until task completes
    for _ in 0..20 {
        app.update();
        thread::sleep(Duration::from_millis(30));
    }

    // Verify task completed
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();
    assert_eq!(task.status, TaskStatus::Done, "Task should be Done");

    // Verify summarization was triggered (memory had entries after execution)
    assert!(
        summarization_called.load(std::sync::atomic::Ordering::SeqCst),
        "Summarization should be triggered when memory has entries after task completion"
    );
}

/// Test: Summarization request creates WorkItem instead of execution request
#[test]
fn summarization_request_creates_workitem_instead_of_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    let task =
        harness::domain::Task::from_user_input_ready("complete this task", 3, default_channel());
    let task_id = task.id;
    app.world_mut().spawn(task);

    app.world_mut()
        .spawn(harness::domain::SummarizationRequestMessage {
            task_id,
            trigger: harness::domain::SummarizationTrigger::UserCommand,
            content_to_summarize: "abc".to_string(),
            target_tokens: 64,
        });

    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&harness::domain::WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(work_items.len(), 1);
    assert_eq!(
        work_items[0].work_type,
        harness::domain::WorkItemType::Summarization
    );
}
