//! ask_user 工具端到端集成测试
//!
//! 验证 ask_user 工具的完整流程：
//! 1. ToolExecutionRequestMessage → orchestrator 推送问题、挂载 AskUserPending、task 切到 Waiting(AskUser)
//! 2. 同通道用户回复 → user_input_routing_system spawn ToolExecutionResultMessage（含 {"answer": ...}）、
//!    移除 AskUserPending、task 恢复到 Waiting(ToolExecution)
//!
//! 关于 ToolExecutionResultMessage 的断言策略：
//! `user_input_routing_system` 与 `tool_result_system` 同处 `HarnessSet::Transform` 且无显式相互依赖，
//! 执行顺序不确定——routing spawn 的 ToolExecutionResultMessage 可能在同帧内被 tool_result_system
//! 消费并 despawn。因此本测试通过 ShortTermMemory 中记录的工具调用结果（由 tool_result_system 在
//! 收到 ToolExecutionResultMessage 时写入）作为 message 被 spawn 且内容正确的等价证据，配合
//! task 状态流转与 AskUserPending 组件生命周期共同覆盖完整流程。

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities,
    domain::AgentExecutionRequest, domain::AgentExecutor, domain::AgentKind, domain::AgentProfile,
    domain::AgentRequestKind, domain::AgentToolPermissions, domain::ChannelId,
    domain::ExternalInput, domain::FrontendKind, domain::LongTermMemory, domain::ShortTermMemory,
    domain::Task, domain::TaskRoutingPolicy, domain::TaskStatus,
    domain::ToolExecutionRequestMessage, domain::ToolPermission, domain::WaitingReason,
    llm::ExecutorRegistry, systems::HarnessConfig,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

use common::mock_executor::EchoExecutor;

mod common;

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
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
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
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "providers.toml".to_string(),
    }
}

/// 验证 ask_user 工具从发起提问到用户回复的完整 ECS 状态流转。
#[test]
fn e2e_ask_user_full_flow() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    // 创建父 Agent（Allow 权限以直接执行工具，无需审批）
    let parent_agent_id = Uuid::new_v4();
    let perms = AgentToolPermissions {
        default_permission: ToolPermission::Allow,
        ..Default::default()
    };
    let parent_agent_entity = app
        .world_mut()
        .spawn((
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
                system_prompt: None,
            },
            LongTermMemory::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(parent_agent_id, parent_agent_entity);

    // 创建父 Task。TaskRoutingPolicy::conversational 会设置 output_channel = Some(channel)，
    // ask_user orchestrator 依赖 output_channel 推送问题。
    let parent_task_id = Uuid::new_v4();
    let parent_task_entity = app
        .world_mut()
        .spawn((
            Task {
                id: parent_task_id,
                content: "test ask_user".to_string(),
                creator: Uuid::nil(),
                delegate: Some(parent_agent_id),
                status: TaskStatus::Ready,
                pending_confirmation_id: None,
                input_summary: "test ask_user".to_string(),
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
                origin_channel: Some(default_channel()),
                routing_policy: TaskRoutingPolicy::conversational(default_channel()),
                last_evaluated_turn: None,
            },
            ShortTermMemory::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(parent_task_id, parent_task_entity);

    // 先运行一帧完成初始化（plugin_load_startup_system 等注册 builtin tools）
    app.update();

    // 模拟 LLM 发起 ask_user 工具调用
    let question = "用什么框架?".to_string();
    let tool_call_id = "call_ask_1".to_string();
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id: parent_task_id,
            agent_id: parent_agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "call ask_user".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
        },
        tool_name: "ask_user".to_string(),
        tool_input: serde_json::json!({"question": question}),
        pending_confirmation_id: None,
        tool_call_id: Some(tool_call_id.clone()),
        pending_confirmation_options: None,
        work_item_entity: None,
        confirmed_once: false,
    });

    // 运行足够帧让 tool_dispatch_system → orchestrator 完成
    for _ in 0..10 {
        app.update();
    }

    // 断言阶段 1：task 进入 Waiting(AskUser)，且挂载了 AskUserPending
    let (task_status, has_ask_user_pending): (TaskStatus, bool) = {
        let world = app.world_mut();
        let mut query = world.query::<(&Task, Option<&harness::domain::AskUserPending>)>();
        query
            .iter(world)
            .find(|(t, _)| t.id == parent_task_id)
            .map(|(t, p)| (t.status.clone(), p.is_some()))
            .expect("parent task should exist after ask_user dispatch")
    };
    assert_eq!(
        task_status,
        TaskStatus::Waiting(WaitingReason::AskUser),
        "task should be Waiting(AskUser) after ask_user tool call"
    );
    assert!(
        has_ask_user_pending,
        "task entity should have AskUserPending component attached"
    );

    // 通过 input_tx 发送同通道用户回复，模拟用户在前端回答问题
    let user_reply = "我推荐用 React".to_string();
    input_tx
        .send(ExternalInput::TextWithChannel {
            channel: default_channel(),
            content: user_reply.clone(),
        })
        .unwrap();

    // 运行足够帧让 input_ingress → signal_ingest → user_input_routing → tool_result_system 完成
    for _ in 0..10 {
        app.update();
    }

    // 断言阶段 2：ToolExecutionResultMessage 被 spawn 且 tool_output 含 {"answer": <用户回复>}。
    // tool_result_system 只在收到 ToolExecutionResultMessage 时才往 STM 写工具调用记录，
    // 且记录的 tool_name / output 直接来自 message 字段，因此 STM 记录是 message 被 spawn
    // 且内容正确的等价证据（message entity 本身可能因 system 顺序不确定被同帧 despawn）。
    let stm_records_answer: bool = {
        let world = app.world_mut();
        let mut query = world.query::<(&Task, &ShortTermMemory)>();
        query
            .iter(world)
            .find(|(t, _)| t.id == parent_task_id)
            .map(|(_, stm)| {
                stm.entries
                    .iter()
                    .flat_map(|e| e.metadata.tool_calls.iter())
                    .any(|tc| {
                        if tc.id.as_deref() != Some(&tool_call_id) || tc.tool_name != "ask_user" {
                            return false;
                        }
                        // tool_result_system 把 tool_output 序列化后写入 ToolCall.input 字段
                        serde_json::from_str::<serde_json::Value>(&tc.input)
                            .ok()
                            .map(|v| {
                                v.get("answer")
                                    .and_then(|a| a.as_str())
                                    .is_some_and(|a| a == user_reply)
                            })
                            .unwrap_or(false)
                    })
            })
            .unwrap_or(false)
    };
    assert!(
        stm_records_answer,
        "STM should record ask_user tool call with tool_output {{\"answer\": \"{user_reply}\"}} \
         (proves ToolExecutionResultMessage was spawned with correct content)"
    );

    // 断言阶段 3：AskUserPending 组件被移除
    let still_pending: bool = {
        let world = app.world_mut();
        let mut query = world.query::<(&Task, Option<&harness::domain::AskUserPending>)>();
        query
            .iter(world)
            .find(|(t, _)| t.id == parent_task_id)
            .map(|(_, p)| p.is_some())
            .unwrap_or(false)
    };
    assert!(
        !still_pending,
        "AskUserPending component should be removed after user reply"
    );

    // 断言阶段 4：task 恢复到 Waiting(ToolExecution)，LLM loop 可续跑
    let final_status: TaskStatus = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query
            .iter(world)
            .find(|t| t.id == parent_task_id)
            .map(|t| t.status.clone())
            .expect("parent task should still exist after user reply")
    };
    assert_eq!(
        final_status,
        TaskStatus::Waiting(WaitingReason::ToolExecution),
        "task should be restored to Waiting(ToolExecution) after user reply"
    );
}
