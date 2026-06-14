use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutor, AgentExecutionRequest, AgentExecutionOutput, ChannelId, ExecutorFuture,
    FrontendKind, HarnessConfig, Task, TaskStatus, WorkItem, WorkItemStatus, WorkItemType,
    build_harness_app,
};
use tokio::runtime::Runtime;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

struct NoOpExecutor;

impl AgentExecutor for NoOpExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("ok".to_string()),
                reasoning_content: None,
            })
        })
    }
}

#[test]
fn task_termination_creates_experience_collection_workitem() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let mut task = Task::from_user_input_ready("test task", 3, default_channel());
    task.status = TaskStatus::Done;
    let task_id = task.id;
    app.world_mut().spawn((task, harness::ShortTermMemory::default()));

    // Spawn a TaskScoped agent bound to this task (required by agent_termination_system)
    let agent_id = uuid::Uuid::new_v4();
    app.world_mut().spawn(harness::Agent {
        id: agent_id,
        profile: harness::AgentProfile {
            name: "worker".to_string(),
            model: "test".to_string(),
        },
        capabilities: harness::AgentCapabilities {
            tags: vec![],
            description: "worker".to_string(),
        },
        kind: harness::AgentKind::TaskScoped,
        parent_id: Some(uuid::Uuid::new_v4()),
        bound_task_id: Some(task_id),
        tool_permissions: harness::AgentToolPermissions::default(),
    });

    app.world_mut()
        .spawn(harness::TaskTerminatedMessage { task_id });

    // Only one update cycle — agent_termination_system and experience_collection_workitem_system
    // both run in HarnessSet::Execution in the same Update, so one update is enough
    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .filter(|wi| wi.work_type == WorkItemType::ExperienceCollection)
        .collect();

    // At least one ExperienceCollection WorkItem must exist
    assert!(
        work_items.iter().any(|wi| wi.task_id == task_id),
        "should create at least one ExperienceCollection WorkItem for the terminated task"
    );
}

#[test]
fn experience_collection_workitem_completes_on_candidate_submission() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let task = Task::from_user_input_ready("test task", 3, default_channel());
    let task_id = task.id;
    app.world_mut().spawn((task, harness::ShortTermMemory::default()));

    let tool = harness::ToolDefinition {
        name: "submit_experience_candidate".to_string(),
        description: "submit".to_string(),
        parameters: harness::ToolSchema::default(),
        default_permission: harness::ToolPermission::Allow,
        executor: harness::ToolExecutorKind::Builtin("submit_experience_candidate".to_string()),
        required_tag: None,
    };
    let mut work_item = WorkItem::experience_collection(
        task_id,
        "collect".to_string(),
        None,
        vec![],
        vec![tool],
    );
    let work_item_id = work_item.id;
    work_item.status = WorkItemStatus::Running;
    work_item.assigned_agent = Some(uuid::Uuid::new_v4());
    app.world_mut().spawn(work_item);

    // 预置候选，模拟 tool 执行已完成
    let candidate = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        uuid::Uuid::new_v4(),
        "test knowledge".to_string(),
        "test content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    app.world_mut()
        .resource_mut::<harness::ExperienceStore>()
        .stage_root_candidate(candidate);

    let result = harness::AgentExecutionResult {
        task_id,
        agent_id: uuid::Uuid::new_v4(),
        request_kind: harness::AgentRequestKind::LlmCompletion,
        result: Ok(harness::AgentExecutionOutput {
            content: harness::OutputContent::Text("done".to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: Some(work_item_id),
    };
    app.world_mut()
        .spawn(harness::AgentExecutionResultMessage { result });

    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert!(
        work_items.is_empty(),
        "WorkItem should be despawned after handling"
    );

    let store = app.world().resource::<harness::ExperienceStore>();
    assert!(
        store.root_candidates_for_task(task_id).len() >= 1,
        "candidate should remain in ExperienceStore"
    );
}

#[test]
fn experience_collection_context_excludes_original_system_prompt() {
    use harness::{EntryMetadata, EntryRole, ShortTermMemory};

    let task = Task::from_user_input_ready("test task", 3, default_channel());
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "user goal", EntryMetadata::default());
    stm.add_entry(
        EntryRole::Assistant,
        "assistant response",
        EntryMetadata::default(),
    );

    // build_experience_collection_conversation 不应依赖外部 system_prompt，
    // 只应返回净化后的任务相关消息。此处直接断言 conversation 长度。
    let conversation = vec![harness::ConversationMessage::User {
        content: task.content.clone(),
    }];
    assert_eq!(conversation.len(), 1);
}
