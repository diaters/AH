//! O2 子 Agent 权限继承集成测试
//!
//! 验证 `handle_spawn_request` 实际行为：spawn 出的子 Agent 继承父 Agent 的
//! Confirm 权限（而非降级为 Allow）。这是 O2 修复的端到端验证。

mod common;

use std::sync::Arc;

use common::mock_executor::PromptEchoExecutor;
use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutor, AgentKind, AgentProfile, AgentSpawnRequestMessage,
    AgentToolPermissions, ChannelId, ExternalInput, FrontendKind, HarnessConfig, LongTermMemory,
    Task, TaskRoutingPolicy, TaskStatus, ToolPermission, build_harness_app, llm::ExecutorRegistry,
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

/// 验证子 Agent 实际继承父 Agent 的 Confirm 权限（而非降级为 Allow）。
#[test]
fn child_agent_inherits_confirm_permission_from_parent() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(PromptEchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded::<ExternalInput>();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 第一次 update：让 Startup 系统运行（ToolRuntimePlugin 注册 shell_exec 等内置工具）
    app.update();

    // 构造父 Agent：default=Deny(explicit=true)，overrides 显式授予 shell_exec: Confirm
    let parent_id = Uuid::new_v4();
    let mut parent_overrides = std::collections::HashMap::new();
    parent_overrides.insert("shell_exec".to_string(), ToolPermission::Confirm);
    let parent_entity = app
        .world_mut()
        .spawn((
            Agent {
                id: parent_id,
                profile: AgentProfile {
                    name: "parent".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec!["llm".to_string()],
                    description: "parent agent".to_string(),
                },
                kind: AgentKind::Persistent,
                parent_id: None,
                bound_task_id: None,
                tool_permissions: AgentToolPermissions {
                    default_permission: ToolPermission::Deny,
                    default_permission_explicit: true,
                    overrides: parent_overrides,
                },
                system_prompt: None,
            },
            LongTermMemory::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(parent_id, parent_entity);

    // 构造父 Task：Pending 状态，由父 Agent 创建
    let task_id = Uuid::new_v4();
    let task_entity = app
        .world_mut()
        .spawn(Task {
            id: task_id,
            content: "test".to_string(),
            creator: parent_id,
            delegate: None,
            status: TaskStatus::Pending,
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
        })
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 发送 AgentSpawnRequestMessage：请求 shell_exec 工具
    app.world_mut().spawn(AgentSpawnRequestMessage {
        parent_agent_id: parent_id,
        task_id,
        name: "child-agent".to_string(),
        model: None,
        description: "child agent".to_string(),
        tools: vec!["shell_exec".to_string()],
        task_prompt: "do something".to_string(),
        task_system_prompt: None,
    });

    // 触发 agent_factory_system（HarnessSet::Maintenance）
    app.update();

    // 查找 spawn 出的子 Agent，验证 overrides["shell_exec"] == Confirm
    let child_agent = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .find(|a| a.kind == AgentKind::TaskScoped && a.parent_id == Some(parent_id))
            .cloned()
            .expect("应 spawn 出 TaskScoped 子 Agent")
    };

    let inherited = child_agent
        .tool_permissions
        .overrides
        .get("shell_exec")
        .expect("子 Agent overrides 应包含 shell_exec");
    assert_eq!(
        *inherited,
        ToolPermission::Confirm,
        "子 Agent 应继承父 Agent 的 Confirm 权限，而非降级为 Allow"
    );

    // 额外校验：子 Agent 的 default_permission 应为 Deny（隔离原则）
    assert_eq!(
        child_agent.tool_permissions.default_permission,
        ToolPermission::Deny,
        "子 Agent 默认权限应为 Deny（最小权限原则）"
    );
    assert!(
        child_agent.tool_permissions.default_permission_explicit,
        "子 Agent default_permission_explicit 应为 true（显式 Deny）"
    );
}
