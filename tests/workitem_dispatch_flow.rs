//! WorkItem dispatch flow integration tests
//!
//! Tests for Phase 5 narrow WorkItem dispatch system.

use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ExecutorFuture,
    HarnessConfig, WorkItem, WorkItemStatus, build_harness_app,
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
    }
}

/// Test: Pending Evaluation WorkItem is dispatched to execution request
#[test]
fn pending_evaluation_workitem_is_dispatched_to_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    // Initialize and load agents from agents.toml (includes "evaluator" agent)
    app.update();

    // Create an evaluation work item
    let task_id = uuid::Uuid::new_v4();
    let work_item = WorkItem::evaluation(
        task_id,
        "评估任务状态".to_string(),
        None,
    );
    let work_item_id = work_item.id;
    app.world_mut().spawn(work_item);

    // Run systems - the workitem should be dispatched
    app.update();

    // Verify work item status changed to Running (dispatched successfully)
    let states: Vec<_> = app.world_mut().query::<&WorkItem>().iter(app.world()).collect();
    assert_eq!(states.len(), 1, "Should have one work item");
    assert_eq!(
        states[0].status, WorkItemStatus::Running,
        "Work item should be in Running status after dispatch"
    );
    assert_eq!(states[0].id, work_item_id);
    assert!(states[0].assigned_agent.is_some(), "Work item should have assigned agent");
}

/// Test: Pending Summarization WorkItem is dispatched to execution request
#[test]
fn pending_summarization_workitem_is_dispatched_to_execution_request() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

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
    let states: Vec<_> = app.world_mut().query::<&WorkItem>().iter(app.world()).collect();
    assert_eq!(states.len(), 1, "Should have one work item");
    assert_eq!(
        states[0].status, WorkItemStatus::Running,
        "Work item should be in Running status after dispatch"
    );
    assert_eq!(states[0].id, work_item_id);
    assert!(states[0].assigned_agent.is_some(), "Work item should have assigned agent");
}

/// Test: WorkItem with no matching agent stays pending
#[test]
fn workitem_without_matching_agent_stays_pending() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    // Initialize
    app.update();

    // Create a Planning work item (no agent has "planning" tag)
    let task_id = uuid::Uuid::new_v4();
    let work_item = WorkItem::new(
        task_id,
        harness::WorkItemType::Planning,
        harness::WorkItemInput::new("Plan this task".to_string()),
        harness::contracts::TagSet::from_tags(["planning"]),
        harness::WorkItemOrigin::UserTask,
        harness::WorkItemWritebackTarget::PlanArtifact,
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

    // Verify work item stays pending
    let states: Vec<_> = app.world_mut().query::<&WorkItem>().iter(app.world()).collect();
    assert_eq!(states.len(), 1, "Should have one work item");
    assert_eq!(
        states[0].status, WorkItemStatus::Pending,
        "Work item should stay pending when no matching agent"
    );
}
