//! WorkItem dispatch flow integration tests
//!
//! Tests for Phase 5 narrow WorkItem dispatch system.

use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ExecutorFuture, HarnessConfig,
    WorkItem, WorkItemStatus, build_harness_app,
};
use tokio::runtime::Runtime;

/// Mock executor for testing
struct MockExecutor;

impl AgentExecutor for MockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text(format!("response: {}", request.prompt)),
                reasoning_content: None,
            })
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
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
    }
}

/// Test: Pending Evaluation WorkItem is dispatched to execution request
#[test]
fn pending_evaluation_workitem_is_dispatched_to_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize and load agents from agents.toml (includes "evaluator" agent)
    app.update();

    // Create an evaluation work item
    let task_id = uuid::Uuid::new_v4();
    let work_item = WorkItem::evaluation(task_id, "评估任务状态".to_string(), None);
    let work_item_id = work_item.id;
    app.world_mut().spawn(work_item);

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
        states[0].status,
        WorkItemStatus::Running,
        "Work item should be in Running status after dispatch"
    );
    assert_eq!(states[0].id, work_item_id);
    assert!(
        states[0].assigned_agent.is_some(),
        "Work item should have assigned agent"
    );
}

/// Test: Pending Summarization WorkItem is dispatched to execution request
#[test]
fn pending_summarization_workitem_is_dispatched_to_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // Initialize and load agents from agents.toml (includes "summarizer" agent)
    app.update();

    // Create a summarization work item
    let task_id = uuid::Uuid::new_v4();
    let work_item = WorkItem::summarization(
        task_id,
        "Content to summarize".to_string(),
        500,
        harness::SummarizationTrigger::TaskComplete,
    );
    let work_item_id = work_item.id;
    app.world_mut().spawn(work_item);

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
        states[0].status,
        WorkItemStatus::Running,
        "Work item should be in Running status after dispatch"
    );
    assert_eq!(states[0].id, work_item_id);
    assert!(
        states[0].assigned_agent.is_some(),
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

    // Create an Execution work item
    // (The narrow dispatcher only handles Evaluation/Summarization, so this is not dispatched)
    let task_id = uuid::Uuid::new_v4();
    let work_item = WorkItem::execution(
        task_id,
        "Execute this task".to_string(),
        harness::contracts::TagSet::from_tags(["nonexistent-tag"]),
    );
    app.world_mut().spawn(work_item);

    // Run systems
    for _ in 0..5 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    // Verify no execution request was created
    let requests: Vec<_> = app
        .world_mut()
        .query::<&harness::AgentExecutionRequestMessage>()
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
        states[0].status,
        WorkItemStatus::Failed,
        "Work item should be marked Failed when no matching agent"
    );
}

/// Test: Pending ExperienceCollection WorkItem is dispatched to collector agent
#[test]
fn pending_experience_collection_workitem_is_dispatched_to_collector() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let task_id = uuid::Uuid::new_v4();
    let tool = harness::ToolDefinition {
        name: "submit_experience_candidate".to_string(),
        description: "submit".to_string(),
        parameters: harness::ToolSchema::default(),
        default_permission: harness::ToolPermission::Allow,
        executor: harness::ToolExecutorKind::Builtin("submit_experience_candidate".to_string()),
        required_tag: None,
    };
    let work_item = WorkItem::experience_collection(
        task_id,
        "collect experience".to_string(),
        None,
        vec![],
        vec![tool],
        uuid::Uuid::new_v4(),
    );
    let work_item_id = work_item.id;
    app.world_mut().spawn(work_item);

    app.update();

    let states: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].status, WorkItemStatus::Running);
    assert_eq!(states[0].id, work_item_id);
    assert!(states[0].assigned_agent.is_some());
}

/// Test: ExperienceCollection WorkItem without collector agent is marked Failed
#[test]
fn experience_collection_workitem_without_collector_is_failed() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut cfg = test_config();
    cfg.agents_config_path = "/nonexistent_agents.toml".to_string();
    let mut app = build_harness_app(
        cfg,
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let task_id = uuid::Uuid::new_v4();
    let tool = harness::ToolDefinition {
        name: "submit_experience_candidate".to_string(),
        description: "submit".to_string(),
        parameters: harness::ToolSchema::default(),
        default_permission: harness::ToolPermission::Allow,
        executor: harness::ToolExecutorKind::Builtin("submit_experience_candidate".to_string()),
        required_tag: None,
    };
    let work_item = WorkItem::experience_collection(
        task_id,
        "collect experience".to_string(),
        None,
        vec![],
        vec![tool],
        uuid::Uuid::new_v4(),
    );
    app.world_mut().spawn(work_item);

    for _ in 0..5 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let states: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(states.len(), 1);
    assert_eq!(
        states[0].status,
        WorkItemStatus::Failed,
        "ExperienceCollection WorkItem should be Failed when no collector agent"
    );
}
