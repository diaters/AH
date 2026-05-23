use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionRequest, AgentExecutor, AgentExperience, AgentKind,
    AgentProfile, AgentToolPermissions, EntryRole, ExecutorFuture, HarnessConfig, LongTermMemory,
    OutputMessage, ShortTermMemory, Task, TaskStatus, WaitingReason, build_harness_app,
};
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move { Ok("echo response".to_string()) })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

#[test]
fn multi_turn_task_lifecycle() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化 app
    app.update();

    // Create a task in Waiting(User) state
    let task_id = uuid::Uuid::new_v4();
    let entity_id = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "multi-turn task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                input_summary: "multi-turn task".to_string(),
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
        ))
        .id();

    // Simulate user input
    app.world_mut().spawn(harness::UserInputMessage {
        content: "continue with this input".to_string(),
    });

    // Run several frames
    for _ in 0..10 {
        app.update();
    }

    // 验证多轮对话流程：
    // 1. 用户输入 → 任务进入 Ready
    // 2. 调度 → 任务进入 Waiting(Agent)
    // 3. 执行 → 任务进入 Running
    // 4. 响应 → 任务回到 Waiting(User)（因为 multi_turn: true）

    // 任务应该回到 Waiting(User) 状态，等待下一轮用户输入
    let task = app
        .world_mut()
        .get::<Task>(entity_id)
        .cloned()
        .expect("task should exist");

    assert_eq!(
        task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "multi-turn task should return to Waiting(User) after response"
    );

    // 验证 ShortTermMemory 记录了用户输入和 Agent 响应
    let stm = app
        .world_mut()
        .get::<ShortTermMemory>(entity_id)
        .cloned()
        .expect("short-term memory should exist");

    assert_eq!(
        stm.entries.len(),
        2,
        "should have user input and assistant response"
    );
    assert_eq!(stm.entries[0].role, EntryRole::User);
    assert_eq!(stm.entries[1].role, EntryRole::Assistant);
}

#[test]
fn short_term_memory_tracks_turns() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // Create a task with short-term memory
    let task_id = uuid::Uuid::new_v4();
    let entity_id = {
        let entity = app.world_mut().spawn((
            Task {
                id: task_id,
                content: "test".to_string(),
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
            },
            ShortTermMemory::default(),
        ));
        entity.id()
    };

    // Add entries to the short-term memory
    {
        let mut stm = app
            .world_mut()
            .get_mut::<ShortTermMemory>(entity_id)
            .unwrap();
        stm.add_entry(harness::EntryRole::User, "hello", Default::default());
        stm.add_entry(
            harness::EntryRole::Assistant,
            "hi there",
            Default::default(),
        );
    }

    app.update();

    // Verify memory entries
    let stored = app
        .world_mut()
        .query::<&ShortTermMemory>()
        .iter(app.world())
        .find(|_| true);

    assert!(stored.is_some());
    let stored = stored.unwrap();
    assert_eq!(stored.entries.len(), 2);
    assert!(stored.estimated_tokens > 0);
}

#[test]
fn agent_has_long_term_memory() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // Run one frame to initialize the app and load persistent agents from config
    app.update();

    // Spawn a persistent agent manually
    let agent_id = uuid::Uuid::new_v4();
    app.world_mut().spawn(Agent {
        id: agent_id,
        profile: AgentProfile {
            name: "test-agent".to_string(),
            model: "gpt-4".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["test".to_string()],
            description: "test agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: AgentExperience::default(),
    });

    // Run another frame to trigger init_agent_memory_system for the new agent
    app.update();

    // Verify the newly spawned agent has long-term memory
    let has_memory = app
        .world_mut()
        .query::<(&Agent, &LongTermMemory)>()
        .iter(app.world())
        .any(|(a, _)| a.id == agent_id);

    assert!(
        has_memory,
        "the spawned agent should have long-term memory after init_agent_memory_system runs"
    );
}

#[test]
fn memory_contribution_on_agent_termination() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // Initialize the app first
    app.update();

    // Create parent agent with memory
    let parent_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: parent_id,
            profile: AgentProfile {
                name: "parent".to_string(),
                model: "gpt-4".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".to_string()],
                description: "parent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        },
        LongTermMemory::default(),
    ));

    // Create child task-scoped agent with memory
    let child_id = uuid::Uuid::new_v4();
    let task_id = uuid::Uuid::new_v4();
    let child_entity_id = {
        let entity = app.world_mut().spawn((
            Agent {
                id: child_id,
                profile: AgentProfile {
                    name: "child".to_string(),
                    model: "gpt-4".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec!["general".to_string()],
                    description: "child agent".to_string(),
                },
                kind: AgentKind::TaskScoped,
                parent_id: Some(parent_id),
                bound_task_id: Some(task_id),
                tool_permissions: AgentToolPermissions::default(),
                experience: AgentExperience::default(),
            },
            LongTermMemory::default(),
        ));
        entity.id()
    };

    // Add some memory to the child agent
    {
        let mut long_memory = app
            .world_mut()
            .get_mut::<LongTermMemory>(child_entity_id)
            .unwrap();
        long_memory.add_archive("learned something important");
    }

    // Create a task for the terminated message to reference
    app.world_mut().spawn(Task {
        id: task_id,
        content: "test task".to_string(),
        creator: parent_id,
        delegate: Some(child_id),
        status: TaskStatus::Done,
        input_summary: "test".to_string(),
        result_summary: "completed".to_string(),
        priority: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: true,
    });

    // Trigger termination by spawning TaskTerminatedMessage
    app.world_mut()
        .spawn(harness::TaskTerminatedMessage { task_id });

    // Run frames to allow systems to process
    for _ in 0..10 {
        app.update();
    }

    // Verify that either:
    // 1. MemoryContributionRequestMessage was generated, or
    // 2. MemoryAbsorptionMessage was generated (contribution processed), or
    // 3. Child agent was despawned and memory was absorbed
    let contribution_requests = app
        .world_mut()
        .query::<&harness::MemoryContributionRequestMessage>()
        .iter(app.world())
        .count();

    let absorption_messages = app
        .world_mut()
        .query::<&harness::MemoryAbsorptionMessage>()
        .iter(app.world())
        .count();

    // Check if child agent still exists
    let child_exists = app
        .world_mut()
        .query::<&Agent>()
        .iter(app.world())
        .any(|a| a.id == child_id);

    // Check if parent has absorbed memory
    let parent_memory = app
        .world_mut()
        .query::<(&Agent, &LongTermMemory)>()
        .iter(app.world())
        .find(|(a, _)| a.id == parent_id)
        .map(|(_, m)| m.entries.len());

    // At least one of these should indicate the contribution flow worked
    assert!(
        contribution_requests > 0
            || absorption_messages > 0
            || !child_exists
            || parent_memory.is_some_and(|len| len > 0),
        "contribution flow should have processed: requests={}, absorptions={}, child_exists={}, parent_memory={:?}",
        contribution_requests,
        absorption_messages,
        child_exists,
        parent_memory
    );
}

#[test]
fn multi_turn_memory_records_user_and_assistant() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建 Waiting(User) 状态的任务和 ShortTermMemory
    let task_id = uuid::Uuid::new_v4();
    let entity_id = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "initial task".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                input_summary: "initial task".to_string(),
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
        ))
        .id();

    // 模拟用户继续输入
    app.world_mut().spawn(harness::UserInputMessage {
        content: "what is the weather?".to_string(),
    });

    // 运行多个 frame 处理输入和响应
    for _ in 0..10 {
        app.update();
    }

    // 验证 ShortTermMemory 记录了用户输入和 Agent 响应
    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert_eq!(
        stm.entries.len(),
        2,
        "should have recorded user input and assistant response"
    );
    assert_eq!(stm.entries[0].role, EntryRole::User);
    assert_eq!(stm.entries[0].content, "what is the weather?");
    assert_eq!(stm.entries[1].role, EntryRole::Assistant);
}

#[test]
fn multi_turn_full_conversation_flow() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 通过 CreateTaskMessage 创建任务，走完整系统流程
    app.world_mut().spawn(harness::CreateTaskMessage {
        content: "hello".to_string(),
    });

    // 运行直到任务进入 Waiting(User)
    for _ in 0..5 {
        app.update();
    }

    // 找到创建的任务
    let (entity_id, task) = app
        .world_mut()
        .query::<(Entity, &Task)>()
        .iter(app.world())
        .find(|(_, t)| t.status == TaskStatus::Waiting(WaitingReason::User))
        .map(|(e, t)| (e, t.clone()))
        .expect("should have a task in Waiting(User)");

    assert_eq!(
        task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "task should be waiting for user after first response"
    );

    // 验证 ShortTermMemory 记录了用户输入和 Agent 响应
    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert_eq!(
        stm.entries.len(),
        2,
        "should have recorded user input and assistant response"
    );
    assert_eq!(stm.entries[0].role, EntryRole::User);
    assert_eq!(stm.entries[0].content, "hello");
    assert_eq!(stm.entries[1].role, EntryRole::Assistant);

    // 模拟用户继续输入
    app.world_mut().spawn(harness::UserInputMessage {
        content: "tell me more".to_string(),
    });

    for _ in 0..5 {
        app.update();
    }

    // 验证 ShortTermMemory 记录了第二轮
    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert!(
        stm.entries.len() >= 4,
        "should have recorded 2x user input and 2x assistant response"
    );

    // 验证最后一条是 Assistant 响应
    let last_entry = stm.entries.last().unwrap();
    assert_eq!(last_entry.role, EntryRole::Assistant);
}

#[test]
fn prompt_includes_conversation_history() {
    let runtime = Arc::new(Runtime::new().unwrap());

    // 使用一个 Executor 来捕获请求内容
    use std::sync::Mutex;
    struct CapturingExecutor {
        captured: Arc<Mutex<Option<String>>>,
    }
    impl AgentExecutor for CapturingExecutor {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            *self.captured.lock().unwrap() = Some(request.prompt.clone());
            Box::pin(async move { Ok("response".to_string()) })
        }
    }

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let executor: Arc<dyn AgentExecutor> = Arc::new(CapturingExecutor {
        captured: captured.clone(),
    });

    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建带历史对话的任务
    let task_id = uuid::Uuid::new_v4();
    let _entity_id = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "current question".to_string(),
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
                multi_turn: true,
            },
            ShortTermMemory {
                entries: vec![
                    harness::MemoryEntry::new(EntryRole::User, "previous question"),
                    harness::MemoryEntry::new(EntryRole::Assistant, "previous answer"),
                ],
                summary_prefix: None,
                estimated_tokens: 100,
                last_cached_tokens: None,
            },
        ))
        .id();

    app.update();

    // 验证 prompt 包含历史对话
    let captured_prompt = captured.lock().unwrap().clone();
    assert!(
        captured_prompt.is_some(),
        "executor should have received a request"
    );
    let prompt = captured_prompt.unwrap();
    assert!(
        prompt.contains("[Conversation history]"),
        "prompt should include conversation history section"
    );
    assert!(
        prompt.contains("previous question"),
        "prompt should include previous user message"
    );
    assert!(
        prompt.contains("previous answer"),
        "prompt should include previous assistant message"
    );
    assert!(
        prompt.contains("[Current request]"),
        "prompt should include current request section"
    );
    assert!(
        prompt.contains("current question"),
        "prompt should include current question"
    );
}

/// 验证首轮用户输入被记录到 ShortTermMemory。
/// 通过 CreateTaskMessage → user_message_to_task_system 流程创建任务，
/// 确保 user_message_to_task_system 将用户输入写入 STM。
#[test]
fn initial_user_input_recorded_in_short_term_memory() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 通过 CreateTaskMessage 创建任务，走 user_message_to_task_system 流程
    app.world_mut().spawn(harness::CreateTaskMessage {
        content: "hello world".to_string(),
    });

    // 运行直到任务进入 Waiting(User)
    for _ in 0..5 {
        app.update();
    }

    // 找到创建的任务
    let (entity_id, task) = app
        .world_mut()
        .query::<(Entity, &Task)>()
        .iter(app.world())
        .find(|(_, t)| t.status == TaskStatus::Waiting(WaitingReason::User))
        .map(|(e, t)| (e, t.clone()))
        .expect("should have a task in Waiting(User)");

    assert_eq!(
        task.status,
        TaskStatus::Waiting(WaitingReason::User),
        "task should be waiting for user after first response"
    );

    // 验证 STM 同时包含用户输入和 Assistant 响应
    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert_eq!(
        stm.entries.len(),
        2,
        "should have both user input and assistant response, got {:?}",
        stm.entries
            .iter()
            .map(|e| (e.role, &e.content))
            .collect::<Vec<_>>()
    );
    assert_eq!(stm.entries[0].role, EntryRole::User);
    assert_eq!(stm.entries[0].content, "hello world");
    assert_eq!(stm.entries[1].role, EntryRole::Assistant);
}

/// 验证三轮对话中 STM 条目顺序正确：User/Assistant 交替出现。
#[test]
fn three_turn_conversation_maintains_correct_order() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 第一轮：通过 CreateTaskMessage 创建任务
    app.world_mut().spawn(harness::CreateTaskMessage {
        content: "first question".to_string(),
    });

    for _ in 0..5 {
        app.update();
    }

    // 第一轮结束后验证
    let (entity_id, _task) = app
        .world_mut()
        .query::<(Entity, &Task)>()
        .iter(app.world())
        .find(|(_, t)| t.status == TaskStatus::Waiting(WaitingReason::User))
        .map(|(e, t)| (e, t.clone()))
        .expect("should have a task in Waiting(User)");

    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert_eq!(
        stm.entries.len(),
        2,
        "first turn: should have User + Assistant"
    );
    assert_eq!(stm.entries[0].role, EntryRole::User);
    assert_eq!(stm.entries[0].content, "first question");
    assert_eq!(stm.entries[1].role, EntryRole::Assistant);

    // 第二轮：继续对话
    app.world_mut().spawn(harness::UserInputMessage {
        content: "second question".to_string(),
    });

    for _ in 0..5 {
        app.update();
    }

    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert_eq!(
        stm.entries.len(),
        4,
        "second turn: should have 4 entries (2x User + 2x Assistant)"
    );
    // 验证顺序：User, Assistant, User, Assistant
    assert_eq!(stm.entries[2].role, EntryRole::User);
    assert_eq!(stm.entries[2].content, "second question");
    assert_eq!(stm.entries[3].role, EntryRole::Assistant);

    // 第三轮：继续对话
    app.world_mut().spawn(harness::UserInputMessage {
        content: "third question".to_string(),
    });

    for _ in 0..5 {
        app.update();
    }

    let stm = app.world_mut().get::<ShortTermMemory>(entity_id).unwrap();
    assert_eq!(
        stm.entries.len(),
        6,
        "third turn: should have 6 entries (3x User + 3x Assistant)"
    );
    // 验证整体顺序：User/Assistant 交替
    for (i, entry) in stm.entries.iter().enumerate() {
        let expected_role = if i % 2 == 0 {
            EntryRole::User
        } else {
            EntryRole::Assistant
        };
        assert_eq!(
            entry.role, expected_role,
            "entry {} should be {:?}, got {:?}",
            i, expected_role, entry.role
        );
    }
    assert_eq!(stm.entries[4].content, "third question");
}

/// 验证继续对话后，LLM 收到的 prompt 中历史顺序和当前请求正确。
/// BUG: 历史顺序错误（Assistant 在 User 之前），且 [Current request]
/// 仍显示首轮内容而非当前用户输入。
#[test]
fn second_dispatch_prompt_includes_correct_history() {
    struct HistoryCapturingExecutor {
        captured: Arc<Mutex<Vec<String>>>,
    }
    impl AgentExecutor for HistoryCapturingExecutor {
        fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
            self.captured.lock().unwrap().push(request.prompt.clone());
            Box::pin(async move { Ok("response".to_string()) })
        }
    }

    let runtime = Arc::new(Runtime::new().unwrap());
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executor: Arc<dyn AgentExecutor> = Arc::new(HistoryCapturingExecutor {
        captured: captured.clone(),
    });
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建 Waiting(User) 状态的任务，并预填充对话历史
    let task_id = uuid::Uuid::new_v4();
    let _entity_id = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "original question".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                input_summary: String::new(),
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
            ShortTermMemory {
                entries: vec![
                    harness::MemoryEntry::new(EntryRole::User, "previous question"),
                    harness::MemoryEntry::new(EntryRole::Assistant, "previous answer"),
                ],
                summary_prefix: None,
                estimated_tokens: 100,
                last_cached_tokens: None,
            },
        ))
        .id();

    // 模拟用户继续对话
    app.world_mut().spawn(harness::UserInputMessage {
        content: "follow-up question".to_string(),
    });

    for _ in 0..10 {
        app.update();
    }

    // 获取第二轮 dispatch 的 prompt
    let prompts = captured.lock().unwrap();
    assert!(
        !prompts.is_empty(),
        "executor should have received at least one request"
    );

    let second_prompt = prompts.last().unwrap();

    // 验证历史中 User 在 Assistant 之前
    let user_pos = second_prompt.find("User: previous question");
    let assistant_pos = second_prompt.find("Assistant: previous answer");
    assert!(
        user_pos.is_some(),
        "prompt should contain previous user message"
    );
    assert!(
        assistant_pos.is_some(),
        "prompt should contain previous assistant message"
    );
    assert!(
        user_pos < assistant_pos,
        "User message should appear before Assistant message in history"
    );

    // 验证 [Current request] 反映当前用户输入
    assert!(
        second_prompt.contains("follow-up question"),
        "prompt should contain the current user input 'follow-up question', got: {}",
        second_prompt
    );
}

/// 验证继续对话时 task.content 被更新为当前用户输入。
/// BUG: continue_task_system 不更新 task.content，导致 [Current request]
/// 始终显示首轮输入内容。
#[test]
fn task_content_updates_on_continue() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建 Waiting(User) 状态的任务
    let task_id = uuid::Uuid::new_v4();
    let entity_id = app
        .world_mut()
        .spawn((
            Task {
                id: task_id,
                content: "original question".to_string(),
                creator: uuid::Uuid::nil(),
                delegate: None,
                status: TaskStatus::Waiting(WaitingReason::User),
                input_summary: String::new(),
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
        ))
        .id();

    // 模拟用户继续输入
    app.world_mut().spawn(harness::UserInputMessage {
        content: "new follow-up question".to_string(),
    });

    for _ in 0..10 {
        app.update();
    }

    // 验证 task.content 已更新为当前用户输入
    let task = app.world_mut().get::<Task>(entity_id).unwrap();
    assert_eq!(
        task.content, "new follow-up question",
        "task.content should be updated to the latest user input"
    );
}
