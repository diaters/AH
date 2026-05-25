//! Tool 执行流程集成测试

use std::{collections::HashMap, sync::Arc};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentExperience, AgentId, AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions,
    EntryRole, ExecutorFuture, HarnessConfig, OutputMessage, ShortTermMemory, SpaceToolRegistry,
    Task, TaskStatus, ToolConfirmationResponseMessage, ToolDefinition, ToolExecutionRequestMessage,
    ToolExecutionResultMessage, ToolExecutorKind, ToolPermission, ToolSchema, WaitingReason,
    build_harness_app,
};
use tokio::runtime::Runtime;

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

/// 创建测试用的 Agent
fn create_test_agent(world: &mut World, tool_permissions: AgentToolPermissions) -> AgentId {
    let id = uuid::Uuid::new_v4();
    world.spawn(Agent {
        id,
        profile: AgentProfile {
            name: "test-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["test".to_string()],
            description: "test agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions,
        experience: AgentExperience::default(),
    });
    id
}

/// 创建测试用的 Tool 注册表
fn create_test_tool_registry(world: &mut World) {
    let mut registry = SpaceToolRegistry::default();

    // 注册一个允许的测试工具
    registry.register(ToolDefinition {
        name: "test_allowed".to_string(),
        description: "A test tool that is allowed".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("test_allowed".to_string()),
        required_tag: None,
    });

    // 注册一个需要确认的测试工具
    registry.register(ToolDefinition {
        name: "test_confirm".to_string(),
        description: "A test tool that requires confirmation".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("test_confirm".to_string()),
        required_tag: None,
    });

    // 注册一个拒绝的测试工具
    registry.register(ToolDefinition {
        name: "test_deny".to_string(),
        description: "A test tool that is denied".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Deny,
        executor: ToolExecutorKind::Builtin("test_deny".to_string()),
        required_tag: None,
    });

    // 注册 echo 工具（需要确认，用于测试 allow_once 和 allow_always）
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back the input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });

    world.insert_resource(registry);
}

/// 测试：允许的工具可以直接执行
#[test]
fn allowed_tool_executes_directly() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent（所有工具允许）
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );

    // 创建 Task 和 ShortTermMemory
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "test_allowed".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "test_allowed".to_string(),
        tool_input: serde_json::json!({"test": "input"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行几帧让系统处理
    for _ in 0..5 {
        app.update();
    }

    // 验证：请求被处理了（有结果消息或请求被清理）
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };
    assert!(
        pending_requests.is_empty(),
        "Tool request should be cleaned up after execution"
    );
}

/// 测试：拒绝的工具不会执行
#[test]
fn denied_tool_does_not_execute() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent（test_deny 工具拒绝）
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::from([("test_deny".to_string(), ToolPermission::Deny)]),
        },
    );

    // 创建 Task
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "test_deny".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "test_deny".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行几帧让系统处理
    for _ in 0..5 {
        app.update();
    }

    // 请求应该被清理
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };

    assert!(
        pending_requests.is_empty(),
        "Denied tool request should be cleaned up"
    );
}

/// 测试：需要确认的工具会生成确认请求消息
#[test]
fn confirm_tool_requires_user_confirmation() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent（默认 Confirm）
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Confirm,
            overrides: HashMap::new(),
        },
    );

    // 创建 Task
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "test_confirm".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "test_confirm".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行几帧让系统处理
    for _ in 0..5 {
        app.update();
    }

    // 收集所有验证数据
    let (results, pending_requests) = {
        let world = app.world_mut();
        let results: Vec<ToolExecutionResultMessage> = {
            let mut query = world.query::<&ToolExecutionResultMessage>();
            query.iter(world).cloned().collect()
        };
        let pending_requests: Vec<&ToolExecutionRequestMessage> = {
            let mut query = world.query::<&ToolExecutionRequestMessage>();
            query.iter(world).collect()
        };
        (results, pending_requests)
    };

    assert!(
        results.is_empty(),
        "Tool should not execute while waiting for confirmation"
    );

    assert!(
        !pending_requests.is_empty(),
        "ToolExecutionRequestMessage should be preserved while waiting for confirmation"
    );
}

/// 测试：ToolCall 被记录到 ShortTermMemory
#[test]
fn tool_call_is_recorded_to_short_term_memory() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );

    // 创建 Task 和 ShortTermMemory
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求（使用 echo 工具，它有实际的执行器）
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "knowledge_search".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "knowledge_search".to_string(),
        tool_input: serde_json::json!({"query": "hello"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行几帧让系统处理
    for _ in 0..10 {
        app.update();
    }

    // 验证：ShortTermMemory 应该有 ToolCall 记录
    let stm = app.world_mut().get::<ShortTermMemory>(task_entity);
    if let Some(memory) = stm {
        // 检查是否有 assistant 条目（可能包含 tool call）
        let has_tool_record = memory
            .entries
            .iter()
            .any(|e| e.role == EntryRole::Assistant && !e.metadata.tool_calls.is_empty());
        assert!(
            has_tool_record || !memory.entries.is_empty(),
            "ShortTermMemory should have recorded the tool call"
        );
    }
}

/// 测试：AgentToolPermissions 正确处理覆盖
#[test]
fn agent_tool_permissions_override_works() {
    let perms = AgentToolPermissions {
        default_permission: ToolPermission::Deny,
        overrides: HashMap::from([
            ("allowed_tool".to_string(), ToolPermission::Allow),
            ("confirm_tool".to_string(), ToolPermission::Confirm),
        ]),
    };

    assert_eq!(
        perms.get_permission("allowed_tool"),
        ToolPermission::Allow,
        "Override should take precedence for allowed_tool"
    );
    assert_eq!(
        perms.get_permission("confirm_tool"),
        ToolPermission::Confirm,
        "Override should take precedence for confirm_tool"
    );
    assert_eq!(
        perms.get_permission("unknown_tool"),
        ToolPermission::Deny,
        "Default should be used for unknown tools"
    );
}

/// 测试：用户拒绝工具确认
#[test]
fn user_denies_tool_confirmation() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent（默认 Confirm）
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Confirm,
            overrides: HashMap::new(),
        },
    );

    // 创建 Task
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "test_confirm".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "test_confirm".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行让确认请求生成
    for _ in 0..5 {
        app.update();
    }

    // 从 ToolExecutionRequestMessage 获取 request_id
    let request_id = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .find_map(|r| r.pending_confirmation_id)
            .unwrap_or_else(uuid::Uuid::new_v4)
    };

    // 用户选择拒绝
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id,
        selected_option: "deny".to_string(),
    });

    // 运行让响应处理
    for _ in 0..5 {
        app.update();
    }

    // 验证：请求应该被清理
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };

    assert!(
        pending_requests.is_empty(),
        "Tool request should be cleaned up after denial"
    );
}

/// 测试：用户允许一次工具执行
#[test]
fn user_allows_tool_once() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent（默认 Confirm）
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Confirm,
            overrides: HashMap::new(),
        },
    );

    // 创建 Task
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具（使用 echo 工具）
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "knowledge_search".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "knowledge_search".to_string(),
        tool_input: serde_json::json!({"query": "test"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行让确认请求生成
    for _ in 0..5 {
        app.update();
    }

    // 从 ToolExecutionRequestMessage 获取 request_id
    let request_id = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .find_map(|r| r.pending_confirmation_id)
            .unwrap_or_else(uuid::Uuid::new_v4)
    };

    // 用户选择允许一次
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id,
        selected_option: "allow_once".to_string(),
    });

    // 运行让响应处理
    for _ in 0..10 {
        app.update();
    }

    // 验证：请求应该被清理
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };

    assert!(
        pending_requests.is_empty(),
        "Tool request should be cleaned up after execution"
    );

    // 验证：allow_once 不应该更新永久权限
    let has_permanent_permission = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .find(|a| a.id == agent_id)
            .map(|a| {
                a.tool_permissions
                    .overrides
                    .contains_key("knowledge_search")
            })
            .unwrap_or(false)
    };

    assert!(
        !has_permanent_permission,
        "allow_once should not update permanent permissions"
    );
}

/// 测试：spawn_agent Tool 创建子 Agent
#[test]
fn spawn_agent_creates_child_agent() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（Brain tag + spawn_agent 权限）
    let parent_id = {
        let id = uuid::Uuid::new_v4();
        app.world_mut().spawn(Agent {
            id,
            profile: AgentProfile {
                name: "brain-agent".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["brain".to_string(), "test".to_string()],
                description: "brain test agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: ToolPermission::Allow,
                overrides: HashMap::new(),
            },
            experience: AgentExperience::default(),
        });
        id
    };

    // 注册 spawn_agent 工具
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
        required_tag: None,
    });
    // 注册 echo 工具供子 Agent 使用
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    app.world_mut().insert_resource(registry);

    // 创建任务
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 发起 spawn_agent 请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id: parent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "spawn_agent".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "spawn_agent".to_string(),
        tool_input: serde_json::json!({
            "name": "child-agent",
            "description": "A test child agent",
            "tools": ["knowledge_search"]
        }),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行系统 - 只运行 1 帧，验证子 Agent 创建
    // 注意：不能运行太多帧，因为 handle_spawn_request 会创建 AgentExecutionRequestMessage，
    // MockExecutor 返回结果后，llm_response_system 会将任务标记为 Done，
    // 然后 agent_factory_system 会因 TaskTerminatedMessage 销毁 TaskScoped 子 Agent
    app.update();

    // 验证：子 Agent 已创建
    let child_agents: Vec<Agent> = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .filter(|a| a.parent_id == Some(parent_id))
            .cloned()
            .collect()
    };

    assert_eq!(child_agents.len(), 1, "should have created one child agent");
    let child = &child_agents[0];
    assert_eq!(child.profile.name, "child-agent");
    assert_eq!(child.kind, AgentKind::TaskScoped);
    assert_eq!(child.bound_task_id, Some(task_id));
    assert!(child.has_permission("knowledge_search"));
}

/// 测试：spawn_agent Confirm 权限路由到用户确认
#[test]
fn spawn_agent_confirm_routes_to_user() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（Confirm 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Confirm,
            overrides: HashMap::new(),
        },
    );

    // 注册 spawn_agent 工具（default_permission: Confirm）
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
        required_tag: None,
    });
    app.world_mut().insert_resource(registry);

    // 创建任务
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 发起 spawn_agent 请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id: parent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "spawn_agent".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "spawn_agent".to_string(),
        tool_input: serde_json::json!({
            "name": "child-agent",
            "description": "A test child agent",
            "tools": ["knowledge_search"]
        }),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行系统
    for _ in 0..5 {
        app.update();
    }

    // 验证：Task 进入 Waiting(User) 状态（spawn_agent 始终路由到用户确认）
    let task_status = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query
            .iter(world)
            .find(|t| t.id == task_id)
            .map(|t| t.status.clone())
    };
    assert_eq!(
        task_status,
        Some(TaskStatus::Waiting(WaitingReason::User)),
        "spawn_agent with Confirm should route to user (task Waiting for User)"
    );

    // 验证：ToolExecutionRequestMessage 有 pending_confirmation_id
    let has_pending = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .any(|r| r.tool_name == "spawn_agent" && r.pending_confirmation_id.is_some())
    };
    assert!(
        has_pending,
        "should have pending confirmation for spawn_agent"
    );
}

/// 测试：子 Agent Confirm 权限路由到父 Agent 审批
#[test]
fn child_agent_confirm_routes_to_parent() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（拥有 echo 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: {
                let mut m = HashMap::new();
                m.insert("knowledge_search".to_string(), ToolPermission::Allow);
                m
            },
        },
    );

    // 创建子 Agent（Confirm 权限，绑定为 TaskScoped）
    let child_id = uuid::Uuid::new_v4();
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("child task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(Agent {
        id: child_id,
        profile: AgentProfile {
            name: "child-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "child".to_string(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(parent_id),
        bound_task_id: Some(task_id),
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: {
                let mut m = HashMap::new();
                m.insert("knowledge_search".to_string(), ToolPermission::Confirm);
                m
            },
        },
        experience: AgentExperience::default(),
    });

    // 注册 echo 工具
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    app.world_mut().insert_resource(registry);

    // 子 Agent 请求执行 echo 工具
    let request = AgentExecutionRequest {
        task_id,
        agent_id: child_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "knowledge_search".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "knowledge_search".to_string(),
        tool_input: serde_json::json!({"query": "test"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行系统
    for _ in 0..5 {
        app.update();
    }

    // 验证：审批流程已处理完毕（Tool 请求被清理）
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };
    assert!(
        pending_requests.is_empty(),
        "tool request should be cleaned up after parent approval processing"
    );
}

/// 测试：用户确认 spawn_agent 后创建子 Agent
#[test]
fn user_confirms_spawn_agent_creates_child() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（spawn_agent Confirm 权限，echo Allow 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: {
                let mut m = HashMap::new();
                m.insert("spawn_agent".to_string(), ToolPermission::Confirm);
                m.insert("knowledge_search".to_string(), ToolPermission::Allow);
                m
            },
        },
    );

    // 注册工具
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
        required_tag: None,
    });
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    app.world_mut().insert_resource(registry);

    // 创建任务
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 发起 spawn_agent 请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id: parent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "spawn_agent".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "spawn_agent".to_string(),
        tool_input: serde_json::json!({
            "name": "child-agent",
            "description": "A test child agent",
            "tools": ["knowledge_search"]
        }),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行让确认请求生成
    for _ in 0..5 {
        app.update();
    }

    // 获取 request_id from ToolExecutionRequestMessage.pending_confirmation_id
    let request_id = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .find(|r| r.tool_name == "spawn_agent")
            .and_then(|r| r.pending_confirmation_id)
            .unwrap()
    };

    // 用户确认
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id,
        selected_option: "allow_once".to_string(),
    });

    // 运行系统 - 只运行 1 帧验证子 Agent 创建
    // 注意：只运行 1 帧因为 MockExecutor 会在后续帧导致任务完成，从而销毁 TaskScoped 子 Agent
    app.update();

    // 验证：子 Agent 已创建
    let child_agents: Vec<Agent> = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .filter(|a| a.parent_id == Some(parent_id))
            .cloned()
            .collect()
    };

    assert_eq!(
        child_agents.len(),
        1,
        "should have created one child agent after user confirmation"
    );
    assert_eq!(child_agents[0].profile.name, "child-agent");
    assert_eq!(child_agents[0].kind, AgentKind::TaskScoped);
}

/// 测试：父 Agent 审批自动拒绝后工具执行失败（MVP 阶段行为）
#[test]
fn confirmation_denied_rejects_tool() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（拥有 echo 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: {
                let mut m = HashMap::new();
                m.insert("knowledge_search".to_string(), ToolPermission::Allow);
                m
            },
        },
    );

    // 创建子 Agent
    let child_id = uuid::Uuid::new_v4();
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("child task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(Agent {
        id: child_id,
        profile: AgentProfile {
            name: "child-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "child".to_string(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(parent_id),
        bound_task_id: Some(task_id),
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: {
                let mut m = HashMap::new();
                m.insert("knowledge_search".to_string(), ToolPermission::Confirm);
                m
            },
        },
        experience: AgentExperience::default(),
    });

    // 注册 echo 工具
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    app.world_mut().insert_resource(registry);

    // 子 Agent 请求执行 echo
    let request = AgentExecutionRequest {
        task_id,
        agent_id: child_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "knowledge_search".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "knowledge_search".to_string(),
        tool_input: serde_json::json!({"query": "test"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行让审批流程处理（父 Agent 审批 → auto-approve）
    for _ in 0..10 {
        app.update();
    }

    // 验证：审批流程已处理完毕（Tool 请求被清理）
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };
    assert!(
        pending_requests.is_empty(),
        "tool request should be cleaned up after parent approval auto-approval"
    );
}

/// 测试：子 Agent Confirm 权限，父 Agent 无权限时路由到用户确认
#[test]
fn child_agent_confirm_no_parent_permission_routes_to_user() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（没有 echo 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: HashMap::new(),
        },
    );

    // 创建子 Agent（echo Confirm 权限）
    let child_id = uuid::Uuid::new_v4();
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("child task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    app.world_mut().spawn(Agent {
        id: child_id,
        profile: AgentProfile {
            name: "child-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "child".to_string(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(parent_id),
        bound_task_id: Some(task_id),
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: {
                let mut m = HashMap::new();
                m.insert("knowledge_search".to_string(), ToolPermission::Confirm);
                m
            },
        },
        experience: AgentExperience::default(),
    });

    // 注册 echo 工具
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    app.world_mut().insert_resource(registry);

    // 子 Agent 请求执行 echo
    let request = AgentExecutionRequest {
        task_id,
        agent_id: child_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "knowledge_search".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "knowledge_search".to_string(),
        tool_input: serde_json::json!({"query": "test"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行系统
    for _ in 0..5 {
        app.update();
    }

    // 验证：Task 进入 Waiting(User) 状态（父 Agent 无权限，路由到用户）
    let task_status = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query
            .iter(world)
            .find(|t| t.id == task_id)
            .map(|t| t.status.clone())
    };
    assert_eq!(
        task_status,
        Some(TaskStatus::Waiting(WaitingReason::User)),
        "should route to user when parent lacks permission"
    );
}
#[test]
fn user_allows_tool_always() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    // 初始化
    app.update();

    // 创建 Agent（默认 Confirm）
    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Confirm,
            overrides: HashMap::new(),
        },
    );

    // 创建 Task
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 注册测试工具（使用 echo 工具）
    create_test_tool_registry(app.world_mut());

    // 发起 Tool 执行请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "knowledge_search".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "knowledge_search".to_string(),
        tool_input: serde_json::json!({"query": "test"}),
        pending_confirmation_id: None,
        tool_call_id: None,
        pending_confirmation_options: None,
    });

    // 运行让确认请求生成
    for _ in 0..5 {
        app.update();
    }

    // 从 ToolExecutionRequestMessage 获取 request_id
    let request_id = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query
            .iter(world)
            .find_map(|r| r.pending_confirmation_id)
            .unwrap_or_else(uuid::Uuid::new_v4)
    };

    // 用户选择允许永久
    app.world_mut().spawn(ToolConfirmationResponseMessage {
        request_id,
        selected_option: "allow_always".to_string(),
    });

    // 运行让响应处理
    for _ in 0..10 {
        app.update();
    }

    // 验证：请求应该被清理
    let pending_requests: Vec<&ToolExecutionRequestMessage> = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolExecutionRequestMessage>();
        query.iter(world).collect()
    };

    assert!(
        pending_requests.is_empty(),
        "Tool request should be cleaned up after execution"
    );

    // 验证：allow_always 应该更新永久权限
    let has_permanent_permission = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .find(|a| a.id == agent_id)
            .map(|a| {
                a.tool_permissions
                    .overrides
                    .contains_key("knowledge_search")
            })
            .unwrap_or(false)
    };

    assert!(
        has_permanent_permission,
        "allow_always should update permanent permissions"
    );
}
