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
