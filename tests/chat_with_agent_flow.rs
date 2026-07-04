//! chat_with_agent 工具集成测试

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentToolPermissions, ChannelId, ExecutorFuture, FrontendKind,
    HarnessConfig, Task, TaskStatus, ToolPermission, build_harness_app,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

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

/// 验证 chat_with_agent 工具调用可以创建子任务并带上 ChatSession 组件。
#[test]
fn chat_with_agent_creates_chat_subtask() {
    let runtime = Arc::new(Runtime::new().unwrap());
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

    // 创建 Persistent Agent 作为 chat target
    let reviewer_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: reviewer_id,
            profile: AgentProfile {
                name: "reviewer".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["review".to_string()],
                description: "reviewer agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        harness::LongTermMemory::default(),
    ));

    // 创建父 Agent（Allow 权限以直接执行工具）
    let parent_agent_id = Uuid::new_v4();
    let perms = AgentToolPermissions {
        default_permission: ToolPermission::Allow,
        ..Default::default()
    };
    app.world_mut().spawn((
        Agent {
            id: parent_agent_id,
            profile: AgentProfile {
                name: "parent-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".to_string()],
                description: "parent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: perms,
        },
        harness::LongTermMemory::default(),
    ));

    let parent_task_id = Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: parent_task_id,
            content: "review doc".to_string(),
            creator: Uuid::nil(),
            delegate: Some(parent_agent_id),
            status: TaskStatus::Ready,
            pending_confirmation_id: None,
            input_summary: "review doc".to_string(),
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

    // 模拟父 Agent 调用 chat_with_agent 工具
    app.world_mut().spawn(harness::ToolExecutionRequestMessage {
        request: harness::AgentExecutionRequest {
            task_id: parent_task_id,
            agent_id: parent_agent_id,
            request_kind: harness::AgentRequestKind::LlmCompletion,
            prompt: "call chat_with_agent".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "chat_with_agent".to_string(),
        tool_input: serde_json::json!({
            "agent": "reviewer",
            "message": "please review this API design"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_chat_1".to_string()),
        pending_confirmation_options: None,
    });

    // 先运行一帧初始化
    app.update();

    // 运行足够帧让工具分发、子任务创建完成
    for _ in 0..5 {
        app.update();
    }

    // 验证子任务存在、带有 ChatSession 且状态为 Waiting(ChatAgent) 或 Ready
    let chat_tasks: Vec<(harness::Task, harness::ChatSession)> = {
        let world = app.world_mut();
        let mut query = world.query::<(&harness::Task, &harness::ChatSession)>();
        query
            .iter(world)
            .filter(|(t, _)| t.parent_task_id == Some(parent_task_id))
            .map(|(t, s)| (t.clone(), s.clone()))
            .collect()
    };

    assert_eq!(
        chat_tasks.len(),
        1,
        "exactly one chat subtask should be created"
    );
    assert!(
        chat_tasks[0].1.child_agent_name == "reviewer",
        "ChatSession should have the correct child_agent_name"
    );
    assert!(
        chat_tasks[0].1.parent_tool_call_id == "call_chat_1",
        "ChatSession should have the correct parent_tool_call_id"
    );
    assert!(
        chat_tasks[0].0.delegate == Some(reviewer_id),
        "chat subtask should delegate to the reviewer agent"
    );
    assert!(
        chat_tasks[0].0.multi_turn,
        "chat subtask should have multi_turn=true"
    );
}

/// 验证 chat_with_agent 可以通过 handle 继续已有对话。
#[test]
fn chat_with_agent_multi_round_via_handle() {
    let runtime = Arc::new(Runtime::new().unwrap());
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

    // 创建 Persistent Agent 作为 chat target
    let reviewer_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: reviewer_id,
            profile: AgentProfile {
                name: "reviewer".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["review".to_string()],
                description: "reviewer agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        harness::LongTermMemory::default(),
    ));

    // 创建父 Agent（Allow 权限）
    let parent_agent_id = Uuid::new_v4();
    let perms = AgentToolPermissions {
        default_permission: ToolPermission::Allow,
        ..Default::default()
    };
    app.world_mut().spawn((
        Agent {
            id: parent_agent_id,
            profile: AgentProfile {
                name: "parent-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".to_string()],
                description: "parent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: perms,
        },
        harness::LongTermMemory::default(),
    ));

    let parent_task_id = Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: parent_task_id,
            content: "review doc".to_string(),
            creator: Uuid::nil(),
            delegate: Some(parent_agent_id),
            status: TaskStatus::Ready,
            pending_confirmation_id: None,
            input_summary: "review doc".to_string(),
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

    app.update();

    // 第一轮：创建对话
    app.world_mut().spawn(harness::ToolExecutionRequestMessage {
        request: harness::AgentExecutionRequest {
            task_id: parent_task_id,
            agent_id: parent_agent_id,
            request_kind: harness::AgentRequestKind::LlmCompletion,
            prompt: "call chat_with_agent".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "chat_with_agent".to_string(),
        tool_input: serde_json::json!({
            "agent": "reviewer",
            "message": "first round message"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_round_1".to_string()),
        pending_confirmation_options: None,
    });

    for _ in 0..10 {
        app.update();
    }

    // 找到创建的对话子任务
    let handle: Uuid = {
        let world = app.world_mut();
        let mut query = world.query::<(&harness::Task, &harness::ChatSession)>();
        query
            .iter(world)
            .find(|(t, _)| t.parent_task_id == Some(parent_task_id))
            .map(|(t, _)| t.id)
            .expect("chat subtask should exist after first round")
    };

    // 第二轮：通过 handle 继续对话
    app.world_mut().spawn(harness::ToolExecutionRequestMessage {
        request: harness::AgentExecutionRequest {
            task_id: parent_task_id,
            agent_id: parent_agent_id,
            request_kind: harness::AgentRequestKind::LlmCompletion,
            prompt: "continue chat".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
        tool_name: "chat_with_agent".to_string(),
        tool_input: serde_json::json!({
            "handle": handle.to_string(),
            "message": "second round message"
        }),
        pending_confirmation_id: None,
        tool_call_id: Some("call_round_2".to_string()),
        pending_confirmation_options: None,
    });

    for _ in 0..10 {
        app.update();
    }

    // 验证子任务仍然只有一个（没有创建新的）
    let chat_tasks_after: Vec<Uuid> = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::Task>();
        query
            .iter(world)
            .filter(|t| t.parent_task_id == Some(parent_task_id))
            .map(|t| t.id)
            .collect()
    };

    assert_eq!(
        chat_tasks_after.len(),
        1,
        "should still have exactly one chat subtask after multi-round"
    );
    assert_eq!(
        chat_tasks_after[0], handle,
        "chat subtask id should not change between rounds"
    );
}

/// 验证 chat_round_completion_system 不再直接恢复父任务到 Ready，
/// 而是保持 Waiting(SubTaskBatch) 等待 tool_calling_orchestrator_system 收集结果。
#[test]
fn chat_round_completion_preserves_parent_waiting_status() {
    let runtime = Arc::new(Runtime::new().unwrap());
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

    let reviewer_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: reviewer_id,
            profile: AgentProfile {
                name: "reviewer".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["review".to_string()],
                description: "reviewer agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        harness::LongTermMemory::default(),
    ));

    let parent_agent_id = Uuid::new_v4();
    let perms = AgentToolPermissions {
        default_permission: ToolPermission::Allow,
        ..Default::default()
    };
    app.world_mut().spawn((
        Agent {
            id: parent_agent_id,
            profile: AgentProfile {
                name: "parent-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["general".to_string()],
                description: "parent agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: perms,
        },
        harness::LongTermMemory::default(),
    ));

    let parent_task_id = Uuid::new_v4();
    let batch_id = Uuid::new_v4();
    let child_task_id = Uuid::new_v4();

    // 直接创建一个处于 Waiting(SubTaskBatch) 的父任务
    app.world_mut().spawn((
        Task {
            id: parent_task_id,
            content: "test".to_string(),
            creator: parent_agent_id,
            delegate: Some(parent_agent_id),
            status: TaskStatus::Waiting(harness::WaitingReason::SubTaskBatch { batch_id }),
            pending_confirmation_id: None,
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
            parent_task_id: None,
            batch_id: None,
            origin_channel: default_channel(),
            last_evaluated_turn: None,
        },
        harness::ShortTermMemory::default(),
    ));

    // 模拟子任务完成，发出 ChatRoundReadyMessage
    app.world_mut().spawn(harness::ChatRoundReadyMessage {
        child_task_id,
        parent_task_id,
        parent_agent_id,
        batch_id,
        parent_tool_call_id: "call_chat_test".to_string(),
        response: "test response".to_string(),
        child_agent_name: "reviewer".to_string(),
    });

    // 运行一帧让 chat_round_completion_system 处理
    app.update();

    // 验证父任务状态仍然是 Waiting(SubTaskBatch)，而非 Ready
    let parent_status: harness::TaskStatus = {
        let world = app.world_mut();
        let mut query = world.query::<&harness::Task>();
        query
            .iter(world)
            .find(|t| t.id == parent_task_id)
            .map(|t| t.status.clone())
            .expect("parent task should exist")
    };

    assert!(
        matches!(
            parent_status,
            harness::TaskStatus::Waiting(harness::WaitingReason::SubTaskBatch { .. })
        ),
        "parent task should still be Waiting(SubTaskBatch) after chat_round_completion, got {:?}",
        parent_status
    );

    // 验证 Tool 调用已记录到父任务的 ShortTermMemory
    let tool_call_recorded: bool = {
        let world = app.world_mut();
        let mut query = world.query::<(&harness::Task, &harness::ShortTermMemory)>();
        query
            .iter(world)
            .find(|(t, _)| t.id == parent_task_id)
            .map(|(_, stm)| {
                stm.entries.iter().any(|e| {
                    e.metadata
                        .tool_calls
                        .iter()
                        .any(|tc| tc.id.as_deref() == Some("call_chat_test"))
                })
            })
            .unwrap_or(false)
    };

    assert!(
        tool_call_recorded,
        "chat_with_agent tool call should be recorded in parent task's ShortTermMemory"
    );
}
