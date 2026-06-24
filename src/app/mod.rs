use std::sync::Arc;

use anyhow::Result;
use bevy::{app::App, prelude::*};
use chrono::{DateTime, Utc};
use crossbeam_channel::Receiver;
use tokio::{runtime::Runtime, sync::mpsc};

use crate::{
    domain::{
        AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentExecutor,
        AgentSpawnRequestMessage, Frontend, PendingKnowledgeWriteHooks, RetryReadyMessage,
        SharedKnowledgeBase, Signal, Task, TaskTerminatedMessage, ToolCallingState,
        UserInputMessage, UserOutputMessage,
    },
    llm::LlmProviderConfig,
    plugins::DefaultRuntimePluginGroup,
    systems::{HarnessSet, agent_factory_system, load_agents_system},
};

#[derive(Debug, Clone)]
pub struct BrainConfig {
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub max_retries: u32,
    pub max_tool_iterations: u32,
    pub llm: LlmProviderConfig,
    pub brain: Option<BrainConfig>,
    pub agents_config_path: String,
    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
    /// shell 工具默认返回的最新输出行数
    pub shell_default_tail_lines: usize,
    /// shell 工具允许返回的最大输出行数
    pub shell_max_tail_lines: usize,
    /// shell.exec 默认超时时间（秒）
    pub shell_default_exec_timeout_secs: u64,
    /// shell.stop(wait_for_exit=true) 默认超时时间（秒）
    pub shell_default_stop_timeout_secs: u64,
    /// 每个 session stream 的最大缓存字节数
    pub shell_max_buffer_bytes_per_stream: usize,
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self> {
        let llm = LlmProviderConfig::from_env("gpt-4.1-mini")?;

        let brain =
            if std::env::var("HARNESS_BRAIN_ENABLED").is_ok_and(|v| v.to_lowercase() == "true") {
                Some(BrainConfig { enabled: true })
            } else {
                None
            };

        let agents_config_path =
            std::env::var("HARNESS_AGENTS_CONFIG").unwrap_or_else(|_| "agents.toml".to_string());

        Ok(Self {
            max_retries: std::env::var("HARNESS_MAX_RETRIES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            max_tool_iterations: std::env::var("HARNESS_MAX_TOOL_ITERATIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            llm,
            brain,
            agents_config_path,
            default_wait_tasks_timeout_secs: std::env::var(
                "HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
            shell_default_tail_lines: std::env::var("HARNESS_SHELL_DEFAULT_TAIL_LINES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(200),
            shell_max_tail_lines: std::env::var("HARNESS_SHELL_MAX_TAIL_LINES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500),
            shell_default_exec_timeout_secs: std::env::var(
                "HARNESS_SHELL_DEFAULT_EXEC_TIMEOUT_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
            shell_default_stop_timeout_secs: std::env::var(
                "HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
            shell_max_buffer_bytes_per_stream: std::env::var(
                "HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM",
            )
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64 * 1024),
        })
    }
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            max_tool_iterations: 5,
            llm: LlmProviderConfig {
                provider: crate::llm::LlmProviderKind::OpenAi,
                model: "gpt-4.1-mini".to_string(),
                api_key: Some("test-api-key".to_string()),
                api_base: None,
            },
            brain: None,
            agents_config_path: "agents.toml".to_string(),
            default_wait_tasks_timeout_secs: 300, // 5 minutes default
            shell_default_tail_lines: 200,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 300,
            shell_default_stop_timeout_secs: 10,
            shell_max_buffer_bytes_per_stream: 64 * 1024,
        }
    }
}

#[derive(Resource)]
pub struct InputReceiver(pub Receiver<crate::domain::ExternalInput>);

#[derive(Resource)]
pub struct FrontendRegistry {
    pub frontends: Vec<Box<dyn Frontend>>,
}

#[derive(Resource, Clone)]
pub struct AsyncRuntime(pub Arc<Runtime>);

#[derive(Resource, Clone)]
pub struct ExecutorHandle(pub Arc<dyn AgentExecutor>);

#[derive(Resource)]
pub struct ExecutionResultSender(pub mpsc::UnboundedSender<crate::domain::AgentExecutionResult>);

#[derive(Resource)]
pub struct ExecutionResultReceiver(
    pub mpsc::UnboundedReceiver<crate::domain::AgentExecutionResult>,
);

#[derive(Resource)]
pub struct HarnessSettings(pub HarnessConfig);

#[derive(Resource)]
pub struct Clock(pub DateTime<Utc>);

impl Default for Clock {
    fn default() -> Self {
        Self(Utc::now())
    }
}

#[derive(Resource, Default)]
pub struct ShutdownState {
    pub requested: bool,
}

/// 记忆配置
#[derive(Debug, Clone, Resource)]
pub struct MemoryConfig {
    /// 压缩触发阈值（token 数）
    pub compression_threshold_tokens: u32,
    /// 保留最近 N 轮不压缩
    pub preserve_recent_turns: u32,
    /// LLM 摘要目标 token 数
    pub summary_target_tokens: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            compression_threshold_tokens: 8000,
            preserve_recent_turns: 2,
            summary_target_tokens: 1000,
        }
    }
}

pub fn build_harness_app(
    config: HarnessConfig,
    runtime: Arc<Runtime>,
    executor: Arc<dyn AgentExecutor>,
    input_rx: Receiver<crate::domain::ExternalInput>,
    frontends: Vec<Box<dyn Frontend>>,
) -> App {
    let (result_tx, result_rx) = mpsc::unbounded_channel();
    let mut app = App::new();

    // 基础 Resource
    app.insert_resource(InputReceiver(input_rx));
    app.insert_resource(FrontendRegistry { frontends });
    app.insert_resource(AsyncRuntime(runtime));
    app.insert_resource(ExecutorHandle(executor));
    app.insert_resource(ExecutionResultSender(result_tx));
    app.insert_resource(ExecutionResultReceiver(result_rx));
    app.insert_resource(HarnessSettings(config));
    app.insert_resource(Clock::default());
    app.insert_resource(ShutdownState::default());

    // Space Resources
    app.insert_resource(SharedKnowledgeBase::default());
    app.insert_resource(PendingKnowledgeWriteHooks::default());

    // Skill 加载器
    app.insert_resource(crate::infrastructure::skills::SkillLoader::default_path());

    // Startup: Load persistent agents before any systems run
    app.add_systems(Startup, load_agents_system);
    app.add_systems(Startup, crate::user_plugins::plugin_load_startup_system);

    // Configure SystemSets
    app.configure_sets(
        Update,
        (
            HarnessSet::Ingress,
            HarnessSet::Signal,
            HarnessSet::Transform,
            HarnessSet::Dispatch,
            HarnessSet::Execution,
            HarnessSet::Output,
            HarnessSet::Maintenance,
        )
            .chain(),
    );

    // 注册 PluginGroup
    app.add_plugins(DefaultRuntimePluginGroup);

    // agent_factory_system 在 HarnessSet::Maintenance 中运行，
    // agent_termination_system 和 experience_collection_cleanup_system
    // 由 ExecutionPlugin 在 HarnessSet::Execution 和 HarnessSet::Maintenance 中注册。
    app.add_systems(
        Update,
        (agent_factory_system.in_set(HarnessSet::Maintenance),),
    );

    app
}

pub fn app_is_idle(world: &mut World) -> bool {
    let active_tasks = world
        .query::<&Task>()
        .iter(world)
        .filter(|task| !task.status.is_terminal())
        .count();
    let pending_signals = world.query::<&Signal>().iter(world).count();
    let pending_user_inputs = world.query::<&UserInputMessage>().iter(world).count();
    let pending_retry_ready = world.query::<&RetryReadyMessage>().iter(world).count();
    let pending_requests = world
        .query::<&AgentExecutionRequestMessage>()
        .iter(world)
        .count();
    let pending_results = world
        .query::<&AgentExecutionResultMessage>()
        .iter(world)
        .count();
    let pending_outputs = world.query::<&UserOutputMessage>().iter(world).count();
    let pending_spawn_requests = world
        .query::<&AgentSpawnRequestMessage>()
        .iter(world)
        .count();
    let pending_terminated = world.query::<&TaskTerminatedMessage>().iter(world).count();
    let pending_tool_calling = world.query::<&ToolCallingState>().iter(world).count();

    active_tasks == 0
        && pending_signals == 0
        && pending_user_inputs == 0
        && pending_retry_ready == 0
        && pending_requests == 0
        && pending_results == 0
        && pending_outputs == 0
        && pending_spawn_requests == 0
        && pending_terminated == 0
        && pending_tool_calling == 0
}
