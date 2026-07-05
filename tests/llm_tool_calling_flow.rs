//! LLM Tool Calling 集成测试
//!
//! 验证 LLM → ToolCalls → ToolExecution → FollowUp → Text 完整循环。

use std::{collections::HashMap, sync::Arc};

use crossbeam_channel::unbounded;
use harness::prelude::*;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, AgentId,
    AgentKind, AgentProfile, AgentRequestKind, AgentToolPermissions, ChannelId, FrontendKind,
    HarnessConfig, LlmToolCall, ShortTermMemory, SpaceToolRegistry, Task, TaskStatus,
    ToolCallingState, ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema, WaitingReason,
    build_harness_app,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}
use tokio::runtime::Runtime;
use uuid::Uuid;

/// Mock executor: 第一次返回 ToolCalls，后续返回 Text
struct ToolCallingMockExecutor;

impl AgentExecutor for ToolCallingMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let has_conversation = request.conversation.is_some();
        let response = if has_conversation {
            AgentExecutionOutput {
                content: harness::OutputContent::Text(
                    "final answer based on tool results".to_string(),
                ),
                reasoning_content: None,
            }
        } else {
            AgentExecutionOutput {
                content: harness::OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_test123".to_string(),
                    name: "knowledge_search".to_string(),
                    arguments: r#"{"query":"hello"}"#.to_string(),
                }]),
                reasoning_content: None,
            }
        };
        Box::pin(async move { Ok(response) })
    }
}

/// Mock executor: 始终返回 ToolCalls（用于测试迭代上限）
struct InfiniteToolCallExecutor;

impl AgentExecutor for InfiniteToolCallExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let iteration = request
            .conversation
            .as_ref()
            .map(|c| {
                c.iter()
                    .filter(|m| matches!(m, harness::ConversationMessage::Tool { .. }))
                    .count()
            })
            .unwrap_or(0);
        let call_id = format!("call_iter_{}", iteration);
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::ToolCalls(vec![LlmToolCall {
                    id: call_id,
                    name: "knowledge_search".to_string(),
                    arguments: r#"{"query":"loop"}"#.to_string(),
                }]),
                reasoning_content: None,
            })
        })
    }
}

/// Mock executor: 持续返回 ToolCalls，但 conversation 中出现 TOOL_BUDGET_EXHAUSTED 后返回 Text
struct BudgetAwareMockExecutor;

impl AgentExecutor for BudgetAwareMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let has_budget_exhausted = request.conversation.as_ref().is_some_and(|conv| {
            conv.iter().any(|m| {
                matches!(m, harness::ConversationMessage::Tool { content, .. }
                        if content.contains("TOOL_BUDGET_EXHAUSTED"))
            })
        });
        let iteration = request
            .conversation
            .as_ref()
            .map(|c| {
                c.iter()
                    .filter(|m| matches!(m, harness::ConversationMessage::Tool { .. }))
                    .count()
            })
            .unwrap_or(0);
        let call_id = format!("call_iter_{}", iteration);

        if has_budget_exhausted {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::Text(
                        "我已达到工具调用上限，请决定是否继续。".to_string(),
                    ),
                    reasoning_content: None,
                })
            })
        } else {
            Box::pin(async move {
                Ok(AgentExecutionOutput {
                    content: harness::OutputContent::ToolCalls(vec![LlmToolCall {
                        id: call_id,
                        name: "knowledge_search".to_string(),
                        arguments: r#"{"query":"loop"}"#.to_string(),
                    }]),
                    reasoning_content: None,
                })
            })
        }
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_tool_iterations: 3,
        ..HarnessConfig::default()
    }
}

fn create_test_agent(world: &mut World, tool_permissions: AgentToolPermissions) -> AgentId {
    let id = Uuid::new_v4();
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
    });
    id
}

fn create_test_tool_registry(world: &mut World) {
    let mut registry = SpaceToolRegistry::default();

    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Echo back the input".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });

    world.insert_resource(registry);
}

fn get_all_tools(world: &World) -> Vec<ToolDefinition> {
    world
        .get_resource::<SpaceToolRegistry>()
        .map(|registry| registry.iter().cloned().collect())
        .unwrap_or_default()
}

/// Helper: spawn a task+STM with Waiting(Agent) status to prevent
/// task_dispatch_system from creating a duplicate request.
fn spawn_task_with_stm(world: &mut World) -> (Entity, Task) {
    let mut task = Task::from_user_input_ready("test prompt", 3, default_channel());
    task.status = TaskStatus::Waiting(WaitingReason::Agent);
    let entity = world.spawn((task.clone(), ShortTermMemory::default())).id();
    (entity, task)
}

/// 测试：LLM 返回 ToolCalls → 工具执行 → 后续请求 → 文本响应 完整循环
#[test]
fn llm_tool_calling_complete_loop() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(ToolCallingMockExecutor);
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

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "use the echo tool".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..10 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    assert!(
        matches!(task.status, TaskStatus::Done),
        "Task should be Done after tool calling loop, got {:?}",
        task.status
    );

    let has_calling_state = {
        let world = app.world_mut();
        let mut query = world.query::<&ToolCallingState>();
        query.iter(world).count()
    };
    assert_eq!(
        has_calling_state, 0,
        "ToolCallingState should be cleaned up after loop completes"
    );
}

/// 测试：Tool Calling 迭代次数达到上限时任务失败
#[test]
fn tool_calling_exceeds_max_iterations() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(InfiniteToolCallExecutor);
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

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "keep calling tools".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..30 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    assert!(
        matches!(task.status, TaskStatus::Failed(_)),
        "Task should be Failed after exceeding max iterations, got {:?}",
        task.status
    );
    assert!(
        task.last_error
            .as_ref()
            .is_some_and(|e| e.contains("absolute hard limit") || e.contains("max iterations")),
        "Error should mention limit, got: {:?}",
        task.last_error
    );
}

/// 测试：Tool Calling 循环中 STM 记录了工具执行结果
#[test]
fn tool_calling_records_to_short_term_memory() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(ToolCallingMockExecutor);
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

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "use knowledge_search tool".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..15 {
        app.update();
    }

    let stm = app.world().get::<ShortTermMemory>(task_entity).unwrap();
    let tool_call_entries: Vec<_> = stm
        .entries
        .iter()
        .filter(|e| !e.metadata.tool_calls.is_empty())
        .collect();
    assert!(
        !tool_call_entries.is_empty(),
        "STM should contain tool call records after tool calling loop"
    );
}

/// 测试：普通任务达到软限制后不失败，而是让 LLM 总结
#[test]
fn tool_calling_soft_limit_returns_synthetic_result() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(BudgetAwareMockExecutor);
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

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    // CRITICAL: set multi_turn = true so task enters Waiting(User) after text response
    app.world_mut()
        .get_mut::<Task>(task_entity)
        .unwrap()
        .multi_turn = true;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "keep calling tools".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..50 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    assert!(
        matches!(task.status, TaskStatus::Waiting(WaitingReason::User)),
        "Task should be Waiting(User) after soft limit, got {:?}",
        task.status
    );
}

/// 测试：绝对硬上限（HARD_LIMIT_MULTIPLIER * max_iterations）时强制失败任务
#[test]
fn tool_calling_hard_limit_forces_failure() {
    let runtime = Arc::new(Runtime::new().unwrap());
    // InfiniteToolCallExecutor 始终返回 ToolCalls，即使收到 TOOL_BUDGET_EXHAUSTED 也不产出 Text
    let executor: Arc<dyn AgentExecutor> = Arc::new(InfiniteToolCallExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(), // max_tool_iterations: 3, hard limit = 6
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    let agent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );
    create_test_tool_registry(app.world_mut());

    let tools = get_all_tools(app.world());

    let (task_entity, task) = spawn_task_with_stm(app.world_mut());
    let task_id = task.id;

    let request = AgentExecutionRequest {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "keep calling tools forever".to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionRequestMessage { request });

    for _ in 0..50 {
        app.update();
    }

    let task = app.world().get::<Task>(task_entity).unwrap();
    assert!(
        matches!(task.status, TaskStatus::Failed(_)),
        "Task should be Failed after exceeding absolute hard limit, got {:?}",
        task.status
    );
    assert!(
        task.last_error
            .as_ref()
            .is_some_and(|e| e.contains("absolute hard limit")),
        "Error should mention absolute hard limit, got: {:?}",
        task.last_error
    );
}
