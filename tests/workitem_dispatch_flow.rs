//! WorkItem dispatch flow integration tests
//!
//! Tests for Phase 5 narrow WorkItem dispatch system.

use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app, domain::AgentExecutionOutput, domain::AgentExecutionRequest,
    domain::AgentExecutor, domain::DispatchHint, domain::DispatchKind, domain::ExecutorFuture,
    domain::PendingDispatch, domain::WorkItem, domain::WorkItemStatus, domain::WorkItemType,
    llm::ExecutorRegistry, systems::HarnessConfig,
};
use tokio::runtime::Runtime;

/// Mock executor for testing
struct MockExecutor;

impl AgentExecutor for MockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::domain::OutputContent::Text(format!(
                    "response: {}",
                    request.prompt
                )),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        memory: harness::domain::MemoryConfig::default(),
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
            model: Some("gpt-4.1-mini".to_string()),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        main_agent: harness::systems::DEFAULT_MAIN_AGENT.to_string(),
        agents_config_path: "tests/fixtures/test_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        tool_background_after_secs: 0,
        max_tool_iterations: 5,
        subtask_checkpoint_iterations: harness::systems::DEFAULT_SUBTASK_CHECKPOINT_ITERATIONS,
        subtask_max_runtime_secs: harness::systems::DEFAULT_SUBTASK_MAX_RUNTIME_SECS,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        terminal_task_retention_secs: 3600,
        conversation_idle_secs: 86400,
        schedule_failure_alert_threshold: 3,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "providers.toml".to_string(),
        loop_control: harness::domain::LoopControlConfig::default(),
    }
}

/// Test: Pending Evaluation WorkItem is dispatched to execution request
#[test]
fn pending_evaluation_workitem_is_dispatched_to_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize and load agents from agents.toml (includes "evaluator" agent)
    app.update();

    // Create an evaluation work item
    let task_id = harness::domain::TaskId::new();
    let work_item = WorkItem::evaluation(task_id, "评估任务状态".to_string(), None);
    let work_item_id = work_item.id;
    app.world_mut().spawn((
        work_item,
        PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::Evaluation),
            hint: DispatchHint::default(),
        },
    ));

    // Run systems - the workitem should be dispatched
    app.update();

    // Verify work item status changed to Running (dispatched successfully)
    let states: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(states.len(), 1, "Should have one work item");
    assert_eq!(
        states[0].status(),
        WorkItemStatus::Running,
        "Work item should be in Running status after dispatch"
    );
    assert_eq!(states[0].id, work_item_id);
    assert!(
        states[0].assigned_agent().is_some(),
        "Work item should have assigned agent"
    );
}

/// Test: Pending Summarization WorkItem is dispatched to execution request
#[test]
fn pending_summarization_workitem_is_dispatched_to_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize and load agents from agents.toml (includes "summarizer" agent)
    app.update();

    // Create a summarization work item
    let task_id = harness::domain::TaskId::new();
    let work_item = WorkItem::summarization(
        task_id,
        "Content to summarize".to_string(),
        500,
        harness::domain::SummarizationTrigger::TaskComplete,
    );
    let work_item_id = work_item.id;
    app.world_mut().spawn((
        work_item,
        PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::Summarization),
            hint: DispatchHint::default(),
        },
    ));

    // Run systems - the workitem should be dispatched
    app.update();

    // Verify work item status changed to Running (dispatched successfully)
    let states: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(states.len(), 1, "Should have one work item");
    assert_eq!(
        states[0].status(),
        WorkItemStatus::Running,
        "Work item should be in Running status after dispatch"
    );
    assert_eq!(states[0].id, work_item_id);
    assert!(
        states[0].assigned_agent().is_some(),
        "Work item should have assigned agent"
    );
}

/// Test: WorkItem with no matching agent is marked as Failed
///
/// Uses an `Execution` work item, which the current narrow dispatcher
/// (Evaluation/Summarization only) intentionally ignores.
#[test]
fn workitem_without_matching_agent_is_marked_failed() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
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

    // Create an Execution work item
    // (The narrow dispatcher only handles Evaluation/Summarization, so this is not dispatched)
    let task_id = harness::domain::TaskId::new();
    let work_item = WorkItem::execution(task_id, "Execute this task".to_string());
    app.world_mut().spawn((
        work_item,
        PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::Execution),
            hint: DispatchHint::default(),
        },
    ));

    // Run systems
    for _ in 0..5 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Verify no execution request was created
    let requests: Vec<_> = app
        .world_mut()
        .query::<&harness::domain::AgentExecutionRequestMessage>()
        .iter(app.world())
        .collect();
    assert_eq!(requests.len(), 0, "Should not create execution request");

    // Verify work item is marked as Failed (not stuck in Pending forever)
    let states: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(states.len(), 1, "Should have one work item");
    assert_eq!(
        states[0].status(),
        WorkItemStatus::Failed,
        "Work item should be marked Failed when no matching agent"
    );
}
