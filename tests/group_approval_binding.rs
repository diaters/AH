//! 群聊审批绑定发起人集成测试
//!
//! 群聊中通道以群为粒度。验证任务审批只接受任务发起人（或通道配置的 `approvers`）的回复：
//! 其他群成员回复审批选项时被拒绝并提示，审批继续等待；发起人回复后工具照常执行。

mod common;

use std::sync::{Arc, Mutex};

use common::mock_executor::CannedExecutor;
use crossbeam_channel::{Sender, unbounded};
use harness::{
    app::build_harness_app, domain::Agent, domain::AgentCapabilities, domain::AgentExecutionOutput,
    domain::AgentExecutor, domain::AgentKind, domain::AgentProfile, domain::AgentToolPermissions,
    domain::ChannelId, domain::EngineEvent, domain::ExternalInput, domain::Frontend,
    domain::FrontendKind, domain::LlmToolCall, domain::LongTermMemory, domain::OutputContent,
    domain::Task, llm::ExecutorRegistry, systems::HarnessConfig,
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

fn test_config(approvers: Vec<&str>) -> HarnessConfig {
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
        channels: harness::channels::config::ChannelConfigs {
            qq: Some(harness::channels::config::QqConfig {
                app_id: "id".to_string(),
                app_secret: "secret".to_string(),
                allowed_users: Vec::new(),
                approvers: approvers.into_iter().map(str::to_string).collect(),
                pairing_enabled: false,
                pairing_code: None,
            }),
            ..Default::default()
        },
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

struct Harness {
    app: bevy_app::App,
    input: Sender<ExternalInput>,
    events: Arc<Mutex<Vec<EngineEvent>>>,
    group: ChannelId,
}

impl Harness {
    fn new(approvers: Vec<&str>) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (input, input_rx) = unbounded();
        let runtime = Arc::new(Runtime::new().expect("tokio runtime should be created"));
        let executor: Arc<dyn AgentExecutor> = Arc::new(CannedExecutor::new(vec![
            tool_calls_output(vec![shell_exec_call("call_1", "echo approved-run")]),
        ]));
        let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
        let (channel_manager, _) = harness::channels::ChannelManager::empty();
        let mut app = build_harness_app(
            test_config(approvers),
            runtime,
            executor_registry,
            input_rx,
            vec![Box::new(CapturingQQFrontend {
                events: events.clone(),
            })],
            channel_manager,
        );
        app.update();
        spawn_default_agent(&mut app);
        Self {
            app,
            input,
            events,
            group: qq_channel("group:g1"),
        }
    }

    fn say(&mut self, sender: &str, content: &str) {
        self.input
            .send(ExternalInput::TextWithChannel {
                channel: self.group.clone(),
                content: content.to_string(),
                sender: Some(sender.to_string()),
            })
            .unwrap();
        for _ in 0..15 {
            self.app.update();
        }
    }

    fn pending_confirmation(&mut self) -> bool {
        self.app
            .world_mut()
            .query::<&Task>()
            .iter(self.app.world())
            .any(|t| t.pending_confirmation_id.is_some())
    }

    fn texts_containing(&self, needle: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, EngineEvent::Text { content, .. } if content.contains(needle)))
            .count()
    }
}

#[test]
fn only_initiator_can_approve_in_group() {
    let mut h = Harness::new(Vec::new());
    h.say("member-a", "run the command");
    assert!(h.pending_confirmation(), "shell_exec 应等待审批");

    h.say("member-b", "1");
    assert!(h.pending_confirmation(), "非发起人的回复不应放行审批");
    assert_eq!(h.texts_containing("仅任务发起人或审批人"), 1);

    h.say("member-a", "1");
    assert!(!h.pending_confirmation(), "发起人回复后审批应完成");
    assert_eq!(h.texts_containing("仅任务发起人或审批人"), 1);
}

#[test]
fn configured_approver_can_approve_in_group() {
    let mut h = Harness::new(vec!["admin"]);
    h.say("member-a", "run the command");
    assert!(h.pending_confirmation(), "shell_exec 应等待审批");

    h.say("admin", "1");
    assert!(!h.pending_confirmation(), "审批人回复后审批应完成");
    assert_eq!(h.texts_containing("仅任务发起人或审批人"), 0);
}
