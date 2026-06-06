use std::sync::Arc;

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ChannelId, ExecutorFuture,
    FrontendKind, HarnessConfig, Task, TaskStatus, WaitingReason, WorkItem, WorkItemType,
    build_harness_app,
};
use tokio::runtime::Runtime;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
    }
}

struct MockExecutor;

impl AgentExecutor for MockExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("mock response".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

#[test]
fn turn_limit_creates_evaluation_workitem() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);

    // 初始化应用
    app.update();

    // 创建一个 Running 状态的任务，并添加 STM 条目（2 轮 = 4 条目）
    let mut task = Task::from_user_input_ready("test task", 3, default_channel());
    task.status = TaskStatus::Running;
    let task_id = task.id;

    let mut stm = harness::ShortTermMemory::default();
    // 第一轮
    stm.add_entry(
        harness::EntryRole::User,
        "user message 1",
        harness::EntryMetadata::default(),
    );
    stm.add_entry(
        harness::EntryRole::Assistant,
        "assistant response 1",
        harness::EntryMetadata::default(),
    );
    // 第二轮
    stm.add_entry(
        harness::EntryRole::User,
        "user message 2",
        harness::EntryMetadata::default(),
    );
    stm.add_entry(
        harness::EntryRole::Assistant,
        "assistant response 2",
        harness::EntryMetadata::default(),
    );

    app.world_mut().spawn((task, stm));

    // 添加评估器 Agent
    app.world_mut().spawn(harness::Agent {
        id: uuid::Uuid::new_v4(),
        profile: harness::AgentProfile {
            name: "evaluator".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: harness::AgentCapabilities {
            tags: vec!["evaluation".to_string()],
            description: "evaluator agent".to_string(),
        },
        kind: harness::AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: harness::AgentToolPermissions::default(),
        experience: harness::AgentExperience::default(),
    });

    // 配置评估：启用，最大 2 轮
    app.world_mut().insert_resource(harness::TaskEvaluationConfig {
        enabled: true,
        max_turns: Some(2),
        evaluator_agent_name: "evaluator".to_string(),
        offtrack_policy: harness::OffTrackPolicy::AskUser,
    });

    // 运行系统
    app.update();

    // 验证：应该创建一个 Evaluation WorkItem
    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();

    assert_eq!(work_items.len(), 1, "should create exactly one evaluation work item");
    assert_eq!(work_items[0].task_id, task_id, "work item should be associated with the task");
    assert_eq!(
        work_items[0].work_type,
        WorkItemType::Evaluation,
        "work item should be of Evaluation type"
    );

    // 验证：任务状态应该变为 Waiting(Evaluator)
    let tasks: Vec<_> = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .collect();

    assert_eq!(tasks.len(), 1);
    println!("Task status: {:?}", tasks[0].status);
    assert_eq!(
        tasks[0].status,
        TaskStatus::Waiting(WaitingReason::Evaluator),
        "task should be waiting for evaluator, but got {:?}",
        tasks[0].status
    );
}
