//! schedule_task 动态任务无人值守审批集成测试
//!
//! 验证由 schedule_task 创建的一次性动态任务触发后，执行 Agent 调用需要确认的
//! shell_exec 工具时，不发起审批（无人值守运行无人可审批），而是直接拒绝，
//! 并向任务 output_channel 指定的 QQ 用户推送提醒（设计 §7.3）。

mod common;

use std::sync::{Arc, Mutex};

use common::mock_executor::CannedExecutor;
use crossbeam_channel::unbounded;
use harness::domain::{ScheduleDefinition, ScheduleExpr};
use harness::triggers::SchedulerState;
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities, domain::AgentExecutionOutput,
    domain::AgentExecutor, domain::AgentKind, domain::AgentProfile, domain::AgentToolPermissions,
    domain::ChannelId, domain::EngineEvent, domain::EventTarget, domain::Frontend,
    domain::FrontendKind, domain::LlmToolCall, domain::LongTermMemory, domain::OutputContent,
    domain::Task, domain::TaskStatus, domain::TaskTrigger, llm::ExecutorRegistry,
    systems::HarnessConfig,
};
use tokio::runtime::Runtime;

fn qq_channel(user_id: &str) -> ChannelId {
    ChannelId {
        frontend: FrontendKind::QQ,
        user_id: user_id.to_string(),
        thread_id: None,
    }
}

fn tool_calls_output(calls: Vec<LlmToolCall>) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::ToolCalls(calls),
        reasoning_content: None,
    }
}

fn shell_exec_call(id: &str, command: &str) -> LlmToolCall {
    LlmToolCall {
        id: id.to_string(),
        name: "shell_exec".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        approval_mode: harness::systems::ApprovalSetting::new(
            harness::systems::ApprovalMode::Manual,
        ),
        memory: harness::domain::MemoryConfig::default(),
        max_retries: 3,
        llm: harness::llm::LlmProviderConfig {
            provider: harness::domain::LlmProviderKind::OpenAi,
            model: Some("gpt-4.1-mini".to_string()),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        main_agent: harness::systems::DEFAULT_MAIN_AGENT.to_string(),
        agents_config_path: "/nonexistent_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        tool_background_after_secs: 0,
        max_tool_iterations: 5,
        subtask_checkpoint_iterations: harness::systems::DEFAULT_SUBTASK_CHECKPOINT_ITERATIONS,
        subtask_max_runtime_secs: harness::systems::DEFAULT_SUBTASK_MAX_RUNTIME_SECS,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        terminal_task_retention_secs: 3600,
        conversation_idle_secs: 86400,
        schedule_failure_alert_threshold: 3,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "/nonexistent_providers.toml".to_string(),
        loop_control: harness::domain::LoopControlConfig::default(),
    }
}

/// Helper function to spawn a default agent for tests
fn spawn_default_agent(app: &mut bevy_app::App) {
    let default_id = harness::domain::AgentId::new();
    let default_entity = app
        .world_mut()
        .spawn((
            Agent {
                id: default_id,
                profile: AgentProfile {
                    name: "default-llm-agent".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                },
                capabilities: AgentCapabilities {
                    tags: vec!["llm".to_string(), "default".to_string()],
                    description: "Default LLM Agent".to_string(),
                },
                kind: AgentKind::Persistent,
                parent_id: None,
                bound_task_id: None,
                tool_permissions: AgentToolPermissions::default(),
                system_prompt: None,
            },
            LongTermMemory::default(),
        ))
        .id();
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(default_id, default_entity);
}

/// 捕获所有前端事件的 QQ MockFrontend。
struct CapturingQQFrontend {
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl Frontend for CapturingQQFrontend {
    fn kind(&self) -> FrontendKind {
        FrontendKind::QQ
    }

    fn push_event(&self, event: EngineEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn poll_actions(&self) -> Vec<harness::domain::UserAction> {
        vec![]
    }
}

#[test]
fn scheduled_task_confirm_tool_is_denied_and_notified_to_output_channel() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let (_input_tx, input_rx) = unbounded();
    let runtime = Arc::new(Runtime::new().expect("tokio runtime should be created"));
    let executor: Arc<dyn AgentExecutor> =
        Arc::new(CannedExecutor::new(vec![tool_calls_output(vec![
            shell_exec_call("call_1", "echo scheduled"),
        ])]));
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (channel_manager, _) = harness::channels::ChannelManager::empty();

    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![Box::new(CapturingQQFrontend {
            events: events.clone(),
        })],
        channel_manager,
    );

    // Initialize app
    app.update();
    spawn_default_agent(&mut app);

    let output_channel = qq_channel("qq-test-group");
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let def = ScheduleDefinition::new(
        uuid::Uuid::new_v4(),
        None,
        "execute scheduled command".to_string(),
        ScheduleExpr::parse(&format!("once:{at}")).unwrap(),
        Some(output_channel.clone()),
        chrono::Utc::now(),
    );
    let kind = def.kind();
    app.world_mut()
        .resource_mut::<SchedulerState>()
        .dynamic_tasks_mut()
        .push(def);

    app.world_mut().spawn(harness::domain::TriggerTaskMessage {
        source: harness::domain::SignalSource("timer".to_string()),
        trigger: TaskTrigger::Timer { kind: kind.clone() },
    });

    // 运行足够帧数，让任务创建、Agent 执行、工具确认请求生成并推送到前端。
    for _ in 0..10 {
        app.update();
    }

    let events = events.lock().unwrap().clone();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EngineEvent::ApprovalRequest { .. })),
        "无人值守运行不应发起审批请求"
    );

    let notice_targets: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::Text {
                target, content, ..
            } if content.contains("无人值守") => match target {
                EventTarget::Directed(channels) => Some(channels.clone()),
                EventTarget::Broadcast => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        notice_targets,
        vec![vec![output_channel.clone()]],
        "拒绝提醒应推送到 scheduled task 的 output_channel"
    );

    let task = app
        .world_mut()
        .query::<&Task>()
        .iter(app.world())
        .find(|task| task.routing_policy.output_channel() == Some(&output_channel))
        .expect("scheduled task 应已创建");

    assert!(
        !matches!(task.status(), TaskStatus::Failed(_)),
        "任务不应进入 Failed 状态，当前状态: {:?}",
        task.status()
    );
}
