use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutionResult, AgentExecutionResultMessage,
    AgentExecutor, AgentRequestKind, ChannelId, EngineEvent, EntryMetadata, EntryRole,
    ExecutorFuture, FailureReason, Frontend, FrontendKind, HarnessConfig, OffTrackPolicy,
    ShortTermMemory, Task, TaskStatus, UserAction, WaitingReason, WorkItem, WorkItemType,
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

/// Mock Frontend：捕获 push_event 中的 EngineEvent 用于断言
#[derive(Clone)]
struct MockFrontend {
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl MockFrontend {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn captured_text_events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                EngineEvent::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Frontend for MockFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::Tui
    }

    fn push_event(&self, event: EngineEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
}

#[test]
fn turn_limit_creates_evaluation_workitem() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty(),
    );

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
    });

    // 配置评估：启用，最大 2 轮
    app.world_mut()
        .insert_resource(harness::TaskEvaluationConfig {
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

    assert_eq!(
        work_items.len(),
        1,
        "should create exactly one evaluation work item"
    );
    assert_eq!(
        work_items[0].task_id, task_id,
        "work item should be associated with the task"
    );
    assert_eq!(
        work_items[0].work_type,
        WorkItemType::Evaluation,
        "work item should be of Evaluation type"
    );

    // 验证：任务状态应该变为 Waiting(Evaluator)
    let tasks: Vec<_> = app.world_mut().query::<&Task>().iter(app.world()).collect();

    assert_eq!(tasks.len(), 1);
    println!("Task status: {:?}", tasks[0].status);
    assert_eq!(
        tasks[0].status,
        TaskStatus::Waiting(WaitingReason::Evaluator),
        "task should be waiting for evaluator, but got {:?}",
        tasks[0].status
    );
}

/// 辅助函数：创建带 STM 和 evaluator 的测试环境
fn setup_eval_test_app(
    stm_entries: u32,
    max_turns: u32,
    offtrack_policy: OffTrackPolicy,
) -> (App, uuid::Uuid) {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty(),
    );
    app.update();

    let mut task = Task::from_user_input_ready("test task", 3, default_channel());
    task.status = TaskStatus::Running;
    let task_id = task.id;

    let mut stm = ShortTermMemory::default();
    for i in 0..stm_entries {
        let role = if i % 2 == 0 {
            EntryRole::User
        } else {
            EntryRole::Assistant
        };
        stm.add_entry(role, format!("msg {}", i), EntryMetadata::default());
    }

    app.world_mut().spawn((task, stm));

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
    });

    app.world_mut()
        .insert_resource(harness::TaskEvaluationConfig {
            enabled: true,
            max_turns: Some(max_turns),
            evaluator_agent_name: "evaluator".to_string(),
            offtrack_policy,
        });

    (app, task_id)
}

/// 辅助函数：创建一个手动控制的评估测试场景
/// 任务处于 Waiting(Evaluator)，WorkItem 已存在，无 agent 干扰
fn setup_manual_eval_scenario(
    offtrack_policy: OffTrackPolicy,
) -> (App, uuid::Uuid, uuid::Uuid, MockFrontend) {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    // 使用不存在的 agents 配置，避免加载默认 agent 干扰测试
    let mut cfg = test_config();
    cfg.agents_config_path = "/nonexistent_agents.toml".to_string();
    let mock_frontend = MockFrontend::new();
    let mut app = build_harness_app(
        cfg,
        runtime,
        executor,
        input_rx,
        vec![Box::new(mock_frontend.clone())],
        harness::channels::ChannelManager::empty(),
    );
    app.update();

    let mut task = Task::from_user_input_ready("test task", 3, default_channel());
    task.status = TaskStatus::Waiting(WaitingReason::Evaluator);
    task.last_evaluated_turn = Some(2);
    let task_id = task.id;

    let mut stm = ShortTermMemory::default();
    for i in 0..4u32 {
        let role = if i % 2 == 0 {
            EntryRole::User
        } else {
            EntryRole::Assistant
        };
        stm.add_entry(role, format!("msg {}", i), EntryMetadata::default());
    }
    app.world_mut().spawn((task, stm));

    // 注意：不添加 evaluator agent，避免 dispatch 系统自动处理
    let work_item_id = uuid::Uuid::new_v4();
    let mut work_item = WorkItem::evaluation(task_id, "test evaluation".to_string(), None);
    work_item.id = work_item_id;
    app.world_mut().spawn(work_item);

    app.world_mut()
        .insert_resource(harness::TaskEvaluationConfig {
            enabled: true,
            max_turns: Some(2),
            evaluator_agent_name: "evaluator".to_string(),
            offtrack_policy,
        });

    (app, task_id, work_item_id, mock_frontend)
}

#[test]
fn evaluation_failure_does_not_retrigger_at_same_progress() {
    let (mut app, task_id) = setup_eval_test_app(4, 2, OffTrackPolicy::AskUser);

    // 第一轮：触发评估
    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(
        work_items.len(),
        1,
        "first update should create 1 Evaluation WorkItem"
    );
    let work_item_entity = app
        .world_mut()
        .query::<(Entity, &WorkItem)>()
        .iter(app.world())
        .find(|(_, wi)| wi.work_type == WorkItemType::Evaluation)
        .map(|(e, _)| e)
        .unwrap();

    // 验证 task 进入 Waiting(Evaluator) 且 last_evaluated_turn 已设置
    let task: &Task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .unwrap();
    assert_eq!(task.status, TaskStatus::Waiting(WaitingReason::Evaluator));
    assert_eq!(task.last_evaluated_turn, Some(2));

    // 手动模拟评估失败：despawn WorkItem，恢复任务到 Ready 再到 Running
    app.world_mut().despawn(work_item_entity);
    // 还需要清理可能存在的 result entities
    let result_entities: Vec<Entity> = app
        .world_mut()
        .query::<(Entity, &AgentExecutionResultMessage)>()
        .iter(app.world())
        .map(|(e, _)| e)
        .collect();
    for e in result_entities {
        app.world_mut().despawn(e);
    }

    // 恢复任务状态到 Running
    let mut task_mut = app
        .world_mut()
        .query::<&mut Task>()
        .iter_mut(app.world_mut())
        .find(|t| t.id == task_id)
        .unwrap();
    task_mut.status = TaskStatus::Running;

    // 第二轮：相同进度不应再次触发
    app.update();

    let work_items_after: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(
        work_items_after.len(),
        0,
        "second update should NOT create new WorkItem at same progress"
    );
}

#[test]
fn progress_advance_allows_retrigger() {
    let (mut app, task_id) = setup_eval_test_app(8, 2, OffTrackPolicy::AskUser);

    // 设置 last_evaluated_turn = Some(2)，但当前 turn_count = 4
    let mut task_mut = app
        .world_mut()
        .query::<&mut Task>()
        .iter_mut(app.world_mut())
        .find(|t| t.id == task_id)
        .unwrap();
    task_mut.last_evaluated_turn = Some(2);

    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .collect();
    assert_eq!(
        work_items.len(),
        1,
        "should create Evaluation WorkItem when progress advanced past last_evaluated_turn"
    );
}

#[test]
fn offtrack_autocorrect_injects_governance_context() {
    let (mut app, task_id, work_item_id, _frontend) =
        setup_manual_eval_scenario(OffTrackPolicy::AutoCorrect);

    // 模拟 OffTrack 结果
    let eval_json = r#"{"decision":"OffTrack","reasoning":"task is drifting","suggested_action":"refocus on original goal"}"#;
    let result = AgentExecutionResult {
        task_id,
        agent_id: uuid::Uuid::nil(),
        request_kind: AgentRequestKind::LlmCompletion,
        result: Ok(AgentExecutionOutput {
            content: harness::OutputContent::Text(eval_json.to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: Some(work_item_id),
    };
    app.world_mut()
        .spawn(AgentExecutionResultMessage { result });

    // 运行 llm_response_system
    app.update();

    // 验证：task.status == Ready
    let task: &Task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Ready,
        "AutoCorrect should restore task to Ready"
    );

    // 验证：STM 中存在 EntryRole::Summary 条目
    let stm: &ShortTermMemory = app
        .world_mut()
        .query::<&ShortTermMemory>()
        .iter(app.world())
        .next()
        .unwrap();
    let summary_entry = stm
        .entries
        .iter()
        .find(|e| e.role == EntryRole::Summary && e.content.contains("[Evaluation AutoCorrect]"));
    assert!(
        summary_entry.is_some(),
        "STM should contain AutoCorrect governance context entry"
    );
    let entry = summary_entry.unwrap();
    assert!(entry.content.contains("refocus on original goal"));
    assert!(entry.metadata.keywords.contains(&"evaluation".to_string()));
    assert!(entry.metadata.keywords.contains(&"offtrack".to_string()));
    assert!(entry.metadata.keywords.contains(&"autocorrect".to_string()));
}

#[test]
fn offtrack_askuser_waits_for_user_and_emits_system_message() {
    let (mut app, task_id, work_item_id, frontend) =
        setup_manual_eval_scenario(OffTrackPolicy::AskUser);

    let eval_json = r#"{"decision":"OffTrack","reasoning":"task is drifting","suggested_action":"ask user for guidance"}"#;
    let result = AgentExecutionResult {
        task_id,
        agent_id: uuid::Uuid::nil(),
        request_kind: AgentRequestKind::LlmCompletion,
        result: Ok(AgentExecutionOutput {
            content: harness::OutputContent::Text(eval_json.to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: Some(work_item_id),
    };
    app.world_mut()
        .spawn(AgentExecutionResultMessage { result });

    app.update();

    // 验证：task.status == Waiting(User)
    let task: &Task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "AskUser should set task to Waiting(User)"
    );

    // 验证：MockFrontend 捕获到包含「任务偏航」的系统通知
    // （frontend_output_system 在同帧 despawn SystemOutputMessage，所以通过前端事件验证）
    let text_events = frontend.captured_text_events();
    let has_offtrack_notification = text_events
        .iter()
        .any(|content| content.contains("任务偏航"));
    assert!(
        has_offtrack_notification,
        "MockFrontend should capture system notification containing '任务偏航', got: {:?}",
        text_events
    );
}

#[test]
fn offtrack_fail_sets_error_and_status() {
    let (mut app, task_id, work_item_id, _frontend) =
        setup_manual_eval_scenario(OffTrackPolicy::Fail);

    let eval_json = r#"{"decision":"OffTrack","reasoning":"task went off track completely","suggested_action":null}"#;
    let result = AgentExecutionResult {
        task_id,
        agent_id: uuid::Uuid::nil(),
        request_kind: AgentRequestKind::LlmCompletion,
        result: Ok(AgentExecutionOutput {
            content: harness::OutputContent::Text(eval_json.to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: Some(work_item_id),
    };
    app.world_mut()
        .spawn(AgentExecutionResultMessage { result });

    app.update();

    let task: &Task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Failed(FailureReason::AgentError),
        "Fail policy should set task to Failed(AgentError)"
    );
    assert!(
        task.last_error
            .as_ref()
            .is_some_and(|e| e.contains("Evaluation OffTrack")),
        "last_error should contain 'Evaluation OffTrack', got: {:?}",
        task.last_error
    );
}

/// 回归测试：Summary 条目不应虚增对话轮数
#[test]
fn summary_entries_do_not_inflate_turn_count() {
    // 构造 STM：User + Assistant + Summary + Summary（1 轮对话 + 2 个治理条目）
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "user message 1", EntryMetadata::default());
    stm.add_entry(
        EntryRole::Assistant,
        "assistant response 1",
        EntryMetadata::default(),
    );
    stm.add_entry(
        EntryRole::Summary,
        "[Evaluation AutoCorrect] refocus",
        EntryMetadata::default(),
    );
    stm.add_entry(
        EntryRole::Summary,
        "[Summarization] compressed history",
        EntryMetadata::default(),
    );

    // 断言：dialog_turn_count 只计 User + Assistant 配对
    assert_eq!(
        stm.dialog_turn_count(),
        1,
        "dialog_turn_count should be 1 (Summary entries must not inflate count)"
    );

    // 断言：entries 总数是 4，但 turn_count 仍是 1
    assert_eq!(stm.entries.len(), 4, "should have 4 total entries");

    // 再验证：max_turns = 1 时，同进度不会重复触发评估
    let last_evaluated_turn: Option<u32> = Some(1);
    let turn_count = stm.dialog_turn_count();
    let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
    assert!(
        should_skip,
        "should skip evaluation at same dialog progress (turn_count={}, last={:?})",
        turn_count, last_evaluated_turn
    );
}

/// 回归测试：AskUser 策略将评估结论写入 STM，恢复后 agent 能看到偏航上下文
#[test]
fn offtrack_askuser_injects_stm_context() {
    let (mut app, task_id, work_item_id, _frontend) =
        setup_manual_eval_scenario(OffTrackPolicy::AskUser);

    // 模拟 OffTrack 评估结果
    let eval_json = r#"{"decision":"OffTrack","reasoning":"task is drifting","suggested_action":"ask user for guidance"}"#;
    let result = AgentExecutionResult {
        task_id,
        agent_id: uuid::Uuid::nil(),
        request_kind: AgentRequestKind::LlmCompletion,
        result: Ok(AgentExecutionOutput {
            content: harness::OutputContent::Text(eval_json.to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: Some(work_item_id),
    };
    app.world_mut()
        .spawn(AgentExecutionResultMessage { result });

    app.update();

    // 验证：STM 中存在 [Evaluation AskUser] 条目
    let stm: &ShortTermMemory = app
        .world_mut()
        .query::<&ShortTermMemory>()
        .iter(app.world())
        .next()
        .unwrap();

    let askuser_entry = stm
        .entries
        .iter()
        .find(|e| e.role == EntryRole::Summary && e.content.contains("[Evaluation AskUser]"));
    assert!(
        askuser_entry.is_some(),
        "STM should contain AskUser governance context entry, entries: {:?}",
        stm.entries
            .iter()
            .map(|e| (&e.role, &e.content))
            .collect::<Vec<_>>()
    );

    let entry = askuser_entry.unwrap();
    assert!(
        entry.content.contains("任务偏航"),
        "entry should contain reasoning"
    );
    assert!(
        entry.content.contains("ask user for guidance"),
        "entry should contain suggested_action"
    );
    assert!(entry.metadata.keywords.contains(&"evaluation".to_string()));
    assert!(entry.metadata.keywords.contains(&"offtrack".to_string()));
    assert!(entry.metadata.keywords.contains(&"askuser".to_string()));

    // 验证：任务状态是 Waiting(User)，而非 Ready
    let task: &Task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == task_id)
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "AskUser should set task to Waiting(User)"
    );
}
