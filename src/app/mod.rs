use std::sync::Arc;

use crate::prelude::*;
use anyhow::{Context, Result};
use bevy_app::App;
use chrono::{DateTime, Utc};
use crossbeam_channel::Receiver;
use tokio::{runtime::Runtime, sync::mpsc};

use crate::{
    domain::{
        AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentExecutor,
        AgentSpawnRequestMessage, Frontend, FrontendKind, PendingKnowledgeWriteHooks,
        RetryReadyMessage, SharedKnowledgeBase, Signal, Task, TaskTerminatedMessage,
        ToolCallingState, UserInputMessage, UserOutputMessage,
    },
    llm::{ExecutorRegistry, LlmProviderConfig},
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
    /// TUI 主循环在活跃状态下的轮询间隔（毫秒）
    pub active_poll_ms: u64,
    /// TUI 主循环在空闲状态下的轮询间隔（毫秒）
    pub idle_poll_ms: u64,
    pub channels: crate::channels::config::ChannelConfigs,
    /// IM 通道配置文件路径（用于 Telegram /bind 回写）
    pub channels_config_path: Option<String>,
    /// 触发器配置文件路径（用于 webhook/timer 事件路由）
    pub triggers_config_path: Option<String>,
    /// providers 配置文件路径（用于多 provider 注册）
    pub providers_config_path: String,
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
            active_poll_ms: std::env::var("HARNESS_ACTIVE_POLL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(16),
            idle_poll_ms: std::env::var("HARNESS_IDLE_POLL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(150),
            channels: {
                let path = std::env::var("HARNESS_CHANNELS_CONFIG").ok();
                match path {
                    Some(ref p) if !p.is_empty() => {
                        let text = std::fs::read_to_string(p)
                            .with_context(|| format!("read channels config: {p}"))?;
                        let mut cfg: crate::channels::config::ChannelConfigs =
                            toml::from_str(&text).context("parse channels config")?;
                        cfg.expand_env_vars();
                        cfg
                    }
                    _ => crate::channels::config::ChannelConfigs::default(),
                }
            },
            channels_config_path: std::env::var("HARNESS_CHANNELS_CONFIG").ok(),
            triggers_config_path: std::env::var("HARNESS_TRIGGERS_CONFIG").ok(),
            providers_config_path: std::env::var("HARNESS_PROVIDERS_CONFIG")
                .unwrap_or_else(|_| "providers.toml".to_string()),
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
            active_poll_ms: 16,
            idle_poll_ms: 150,
            channels: crate::channels::config::ChannelConfigs::default(),
            channels_config_path: None,
            triggers_config_path: None,
            providers_config_path: "providers.toml".to_string(),
        }
    }
}

#[derive(Resource)]
pub struct InputReceiver(pub Receiver<crate::domain::ExternalInput>);

#[derive(Resource)]
pub struct FrontendRegistry {
    pub frontends: Vec<Box<dyn Frontend>>,
}

impl FrontendRegistry {
    /// 检查指定类型的 frontend 是否已在注册表中。
    /// 注意：返回 true 仅表示该 frontend 类型已注册，不保证底层 channel 当前可用
    ///（channel 可用性由 ChannelManager 的运行时发送结果覆盖）。
    pub fn has_frontend(&self, kind: FrontendKind) -> bool {
        self.frontends.iter().any(|f| f.kind() == kind)
    }
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
pub struct ModelChainStateUpdateSender(
    pub mpsc::UnboundedSender<crate::domain::ModelChainStateUpdate>,
);

#[derive(Resource)]
pub struct ModelChainStateUpdateReceiver(
    pub mpsc::UnboundedReceiver<crate::domain::ModelChainStateUpdate>,
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
    executor_registry: ExecutorRegistry,
    input_rx: Receiver<crate::domain::ExternalInput>,
    frontends: Vec<Box<dyn Frontend>>,
    channel_manager: crate::channels::ChannelManager,
) -> App {
    let (result_tx, result_rx) = mpsc::unbounded_channel();
    let (state_tx, state_rx) = mpsc::unbounded_channel();
    let mut app = App::new();

    // 创建一个默认的 executor 用于 ExecutorHandle（向后兼容）
    // 注意：在 Task 11 中，当执行系统改造完成后，ExecutorHandle 将被移除
    let default_executor = executor_registry
        .get("default")
        .or_else(|| executor_registry.executors.values().next().cloned())
        .expect("ExecutorRegistry should have at least one executor");

    // 基础 Resource
    app.insert_resource(InputReceiver(input_rx));
    app.insert_resource(FrontendRegistry { frontends });
    app.insert_resource(AsyncRuntime(runtime));
    app.insert_resource(executor_registry);
    app.insert_resource(ExecutorHandle(default_executor)); // 临时保留用于向后兼容
    app.insert_resource(ExecutionResultSender(result_tx));
    app.insert_resource(ExecutionResultReceiver(result_rx));
    app.insert_resource(ModelChainStateUpdateSender(state_tx));
    app.insert_resource(ModelChainStateUpdateReceiver(state_rx));
    app.insert_resource(HarnessSettings(config));
    app.insert_resource(Clock::default());
    app.insert_resource(ShutdownState::default());
    app.insert_resource(channel_manager);

    // Space Resources
    app.insert_resource(SharedKnowledgeBase::default());
    app.insert_resource(PendingKnowledgeWriteHooks::default());

    // Skill 加载器
    app.insert_resource(crate::infrastructure::skills::SkillLoader::default_path());
    // Skill 注册表：由 brain_dispatch / experience_governance 等 system 通过 Res<SkillRegistry> 读取。
    // 当前以空 registry 启动，后续 startup system 可按需补全（skill loader build_registry 接入后）。
    app.insert_resource(crate::infrastructure::skills::SkillRegistry::default());

    // Signal 触发路由（默认空，由 main.rs 根据 triggers.toml 配置覆盖）
    app.insert_resource(crate::domain::SignalTriggerRegistry::default());
    app.insert_resource(crate::triggers::SchedulerState::default());
    app.insert_resource(crate::triggers::SchedulerStateWatcher::default());
    app.insert_resource(crate::triggers::ScheduledTaskRegistry::default());

    // Startup: 先加载插件注册表（含 Tool 注册），再加载持久化 Agent（含插件贡献）
    app.add_systems(Startup, crate::user_plugins::plugin_load_startup_system);
    app.add_systems(Startup, load_agents_system);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_for_poll_intervals() {
        let config = HarnessConfig::default();
        assert_eq!(config.active_poll_ms, 16);
        assert_eq!(config.idle_poll_ms, 150);
    }

    #[test]
    fn frontend_registry_has_frontend_checks_kind() {
        use crate::domain::{EngineEvent, Frontend, FrontendKind, UserAction};

        struct DummyFrontend(FrontendKind);
        impl Frontend for DummyFrontend {
            fn kind(&self) -> FrontendKind {
                self.0.clone()
            }
            fn push_event(&self, _event: EngineEvent) {}
            fn poll_actions(&self) -> Vec<UserAction> {
                vec![]
            }
        }

        let registry = FrontendRegistry {
            frontends: vec![
                Box::new(DummyFrontend(FrontendKind::Tui)),
                Box::new(DummyFrontend(FrontendKind::QQ)),
            ],
        };
        assert!(registry.has_frontend(FrontendKind::Tui));
        assert!(registry.has_frontend(FrontendKind::QQ));
        assert!(!registry.has_frontend(FrontendKind::Telegram));
        assert!(!registry.has_frontend(FrontendKind::Feishu));
    }
}
