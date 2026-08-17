//! Summarization flow integration tests
//!
//! Tests for Phase 4.3 LLM-based memory summarization feature.

use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities, domain::AgentExecutionOutput,
    domain::AgentExecutionRequest, domain::AgentExecutor, domain::AgentKind, domain::AgentProfile,
    domain::AgentRequestKind, domain::AgentToolPermissions, domain::ChannelId,
    domain::DispatchHint, domain::DispatchKind, domain::DispatchStrategy, domain::ExecutorFuture,
    domain::FrontendKind, domain::LongTermMemory, domain::PendingDispatch, domain::ShortTermMemory,
    domain::Task, domain::TaskRoutingPolicy, domain::TaskStatus, domain::WaitingReason,
    llm::ExecutorRegistry, systems::HarnessConfig,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
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
                    Ok(AgentExecutionOutput { content: harness::domain::OutputContent::Text("这是一个测试摘要。".to_string()), reasoning_content: None })
                }
                AgentRequestKind::LlmCompletion => Ok(AgentExecutionOutput { content: harness::domain::OutputContent::Text(format!("response: {}", request.prompt)), reasoning_content: None }),
                AgentRequestKind::BrainDecision => {
                    Ok(AgentExecutionOutput { content: harness::domain::OutputContent::Text(r#"{"selected_agent_name":"default-llm-agent","delegate_prompt":"test","reasoning":"test"}"#.to_string()), reasoning_content: None })
                }
                AgentRequestKind::ToolExecution { .. } => {
                    Err(harness::domain::ExecutionError::Unknown("Not supported".to_string()))
                }
                AgentRequestKind::Evaluation => {
                    Ok(AgentExecutionOutput { content: harness::domain::OutputContent::Text(r#"{"decision":"Continue","reasoning":"test"}"#.to_string()), reasoning_content: None })
                }
            }
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
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

/// Helper function to spawn a default agent for tests
fn spawn_default_agent(app: &mut bevy_app::App) {
    // Spawn default LLM agent
    app.world_mut().spawn((
        Agent {
            id: uuid::Uuid::new_v4(),
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
    ));

    // Spawn summarizer agent for summarization work items
    app.world_mut().spawn((
        Agent {
            id: uuid::Uuid::new_v4(),
            profile: AgentProfile {
                name: "summarizer".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["summarization".to_string(), "memory".to_string()],
                description: "Summarizer Agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        },
        LongTermMemory::default(),
    ));
}

/// Test: Task completion does NOT trigger summarization (trigger was removed)
/// STM has no consumer after terminal state.
#[test]
fn task_completion_does_not_trigger_summarization() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let summarization_called = executor.summarization_called.clone();
    let _executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();
    spawn_default_agent(&mut app);

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
                pending_confirmation_id: None,
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
                origin_channel: Some(default_channel()),
                routing_policy: TaskRoutingPolicy::conversational(default_channel()),
                last_evaluated_turn: None,
            },
            ShortTermMemory {
                entries: vec![
                    harness::domain::MemoryEntry::new(
                        harness::domain::EntryRole::User,
                        "user message",
                    ),
                    harness::domain::MemoryEntry::new(
                        harness::domain::EntryRole::Assistant,
                        "assistant response",
                    ),
                ],
                summary_prefix: None,
                estimated_tokens: 100,
                last_cached_tokens: None,
            },
            PendingDispatch {
                kind: DispatchKind::Task,
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: Some("default-llm-agent".to_string()),
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ))
        .id();

    // Run until task completes
    for _ in 0..25 {
        app.update();
        thread::sleep(Duration::from_millis(30));
    }

    // Verify task is done
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();
    assert_eq!(task.status, TaskStatus::Done, "Task should be Done");

    // Verify summarization was NOT triggered on task completion
    // (TaskComplete trigger was removed as STM has no consumer after terminal state)
    assert!(
        !summarization_called.load(std::sync::atomic::Ordering::SeqCst),
        "Summarization should NOT be triggered on task completion"
    );
}

/// Test: Multi-turn task does not trigger summarization mid-conversation
#[test]
fn multi_turn_task_does_not_trigger_summarization_mid_conversation() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let _executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let summarization_called = executor.summarization_called.clone();
    let executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();
    spawn_default_agent(&mut app);

    // Create a multi-turn task in Waiting(User) state
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "multi-turn task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: None,
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
            origin_channel: Some(default_channel()),
            routing_policy: TaskRoutingPolicy::conversational(default_channel()),
            last_evaluated_turn: None,
        },
        ShortTermMemory {
            entries: vec![
                harness::domain::MemoryEntry::new(harness::domain::EntryRole::User, "user message"),
                harness::domain::MemoryEntry::new(
                    harness::domain::EntryRole::Assistant,
                    "assistant response",
                ),
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
    let executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();
    spawn_default_agent(&mut app);

    // Create a task that will complete (single-turn)
    // 注：统一 dispatch_system 要求 Task 携带 PendingDispatch 才会派发，
    // 这里附加 PendingDispatch(DirectDelegate) 直接委派给 default-llm-agent。
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("complete this task", 3, default_channel()),
            ShortTermMemory {
                entries: vec![harness::domain::MemoryEntry::new(
                    harness::domain::EntryRole::User,
                    "hello",
                )],
                summary_prefix: None,
                estimated_tokens: 50,
                last_cached_tokens: None,
            },
            PendingDispatch {
                kind: DispatchKind::Task,
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: Some("default-llm-agent".to_string()),
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ))
        .id();

    // Run until task completes and summarization finishes
    for _ in 0..30 {
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
/// but does NOT trigger summarization on task completion (TaskComplete trigger was removed)
#[test]
fn execution_populates_memory_but_does_not_trigger_summarization() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let _executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let summarization_called = executor.summarization_called.clone();
    let executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize
    app.update();
    spawn_default_agent(&mut app);

    // Create a single-turn task with pre-populated ShortTermMemory
    // (simulating what happens after llm_response_system runs)
    // 注：统一 dispatch_system 要求 Task 携带 PendingDispatch 才会派发。
    let entity_id = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test", 3, default_channel()),
            ShortTermMemory {
                entries: vec![harness::domain::MemoryEntry::new(
                    harness::domain::EntryRole::User,
                    "user input",
                )],
                summary_prefix: None,
                estimated_tokens: 50,
                last_cached_tokens: None,
            },
            PendingDispatch {
                kind: DispatchKind::Task,
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: Some("default-llm-agent".to_string()),
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ))
        .id();

    // Run until task completes
    for _ in 0..30 {
        app.update();
        thread::sleep(Duration::from_millis(30));
    }

    // Verify task completed
    let task = app.world_mut().get::<Task>(entity_id).cloned();
    assert!(task.is_some());
    let task = task.unwrap();
    assert_eq!(task.status, TaskStatus::Done, "Task should be Done");

    // Verify summarization was NOT triggered (TaskComplete trigger was removed:
    // STM has no consumer after terminal state, so no summarization needed)
    assert!(
        !summarization_called.load(std::sync::atomic::Ordering::SeqCst),
        "Summarization should NOT be triggered on task completion even with memory entries"
    );
}

/// Test: Summarization request creates WorkItem instead of execution request
#[test]
fn summarization_request_creates_workitem_instead_of_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor = Arc::new(SummarizationMockExecutor::new());
    let executor_registry = ExecutorRegistry::from_single_executor(executor.clone(), "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

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
