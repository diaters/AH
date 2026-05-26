use std::sync::Arc;

use anyhow::Result;
use bevy::{app::App, prelude::*};
use chrono::{DateTime, Utc};
use crossbeam_channel::Receiver;
use tokio::{runtime::Runtime, sync::mpsc};

use crate::{
    domain::{
        AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentExecutor,
        AgentSpawnRequestMessage, BuiltinToolExecutors, Frontend, RetryReadyMessage, Signal,
        SpaceAgentRegistry, SpaceKnowledge, SpacePreferences, SpaceRuntimeContext,
        SpaceToolRegistry, Task, TaskEvaluationConfig, TaskTerminatedMessage, ToolCallingState,
        UserInputMessage, UserOutputMessage,
    },
    llm::LlmProviderConfig,
    systems::{
        HarnessSet, agent_execution_system, agent_factory_system, agent_termination_system,
        approval_dispatch_system, approval_result_system, brain_decision_system,
        brain_dispatch_system, command_parse_system, continue_task_system,
        evaluation_result_system, evaluation_trigger_system, finish_task_system,
        frontend_input_system, frontend_output_system, ingest_execution_results_system,
        init_agent_memory_system, input_ingress_system, llm_response_system, load_agents_system,
        memory_absorption_system, memory_compression_system, memory_contribution_system,
        register_builtin_tools, retry_ready_system, retry_wakeup_system, signal_ingest_system,
        sub_task_batch_block_system, sub_task_completion_system, summarization_dispatch_system,
        summarization_result_system, task_dispatch_system, task_termination_system,
        tick_clock_system, tool_calling_orchestrator_system, tool_confirmation_request_system,
        tool_confirmation_result_system, tool_dispatch_system, tool_result_system,
        user_input_routing_system, user_message_to_task_system,
    },
};

#[derive(Debug, Clone)]
pub struct BrainConfig {
    pub enabled: bool,
    pub model: String,
    pub agent_name: String,
}

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub max_retries: u32,
    pub max_tool_iterations: u32,
    pub llm: LlmProviderConfig,
    pub brain: Option<BrainConfig>,
    pub agents_config_path: String,
}

impl HarnessConfig {
    pub fn from_env() -> Result<Self> {
        let llm = LlmProviderConfig::from_env("gpt-4.1-mini")?;

        let brain = if std::env::var("HARNESS_BRAIN_ENABLED")
            .is_ok_and(|v| v.to_lowercase() == "true")
        {
            Some(BrainConfig {
                enabled: true,
                model: std::env::var("HARNESS_BRAIN_MODEL").unwrap_or_else(|_| llm.model.clone()),
                agent_name: std::env::var("HARNESS_BRAIN_AGENT_NAME")
                    .unwrap_or_else(|_| "brain".to_string()),
            })
        } else {
            None
        };

        let agents_config_path =
            std::env::var("HARNESS_AGENTS_CONFIG").unwrap_or_else(|_| "agents.toml".to_string());

        Ok(Self {
            max_retries: 3,
            max_tool_iterations: 5,
            llm,
            brain,
            agents_config_path,
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

    app.insert_resource(InputReceiver(input_rx));
    app.insert_resource(FrontendRegistry { frontends });
    app.insert_resource(AsyncRuntime(runtime));
    app.insert_resource(ExecutorHandle(executor));
    app.insert_resource(ExecutionResultSender(result_tx));
    app.insert_resource(ExecutionResultReceiver(result_rx));
    app.insert_resource(HarnessSettings(config));
    app.insert_resource(Clock::default());
    app.insert_resource(ShutdownState::default());
    app.insert_resource(MemoryConfig::default());
    app.insert_resource(TaskEvaluationConfig::default());

    // Space Resources
    app.insert_resource(SpaceKnowledge::default());
    app.insert_resource(SpacePreferences::default());
    app.insert_resource(SpaceAgentRegistry::default());
    app.insert_resource(SpaceRuntimeContext::default());

    // Tool Registry with builtin tools
    let mut tool_registry = SpaceToolRegistry::default();
    let mut tool_executors = BuiltinToolExecutors::default();
    register_builtin_tools(&mut tool_registry, &mut tool_executors);
    app.insert_resource(tool_registry);
    app.insert_resource(tool_executors);

    // Startup: Load persistent agents before any systems run
    app.add_systems(Startup, load_agents_system);

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

    app.add_systems(
        Update,
        (
            tick_clock_system.in_set(HarnessSet::Ingress),
            frontend_input_system.in_set(HarnessSet::Ingress),
            input_ingress_system.in_set(HarnessSet::Ingress),
            retry_wakeup_system.in_set(HarnessSet::Signal),
            signal_ingest_system.in_set(HarnessSet::Signal),
            ingest_execution_results_system.in_set(HarnessSet::Transform),
            brain_decision_system
                .in_set(HarnessSet::Transform)
                .after(ingest_execution_results_system),
            command_parse_system.in_set(HarnessSet::Transform),
            finish_task_system
                .in_set(HarnessSet::Transform)
                .after(command_parse_system),
            user_input_routing_system
                .in_set(HarnessSet::Transform)
                .after(command_parse_system),
            user_message_to_task_system
                .in_set(HarnessSet::Transform)
                .after(user_input_routing_system),
            continue_task_system
                .in_set(HarnessSet::Transform)
                .after(user_input_routing_system),
            retry_ready_system.in_set(HarnessSet::Transform),
            llm_response_system
                .in_set(HarnessSet::Transform)
                .after(ingest_execution_results_system),
            task_termination_system
                .in_set(HarnessSet::Transform)
                .after(llm_response_system),
            sub_task_completion_system
                .in_set(HarnessSet::Transform)
                .after(task_termination_system),
            evaluation_result_system.in_set(HarnessSet::Transform),
            tool_result_system
                .in_set(HarnessSet::Transform)
                .after(ingest_execution_results_system),
            sub_task_batch_block_system
                .in_set(HarnessSet::Transform)
                .after(tool_result_system),
            tool_calling_orchestrator_system
                .in_set(HarnessSet::Transform)
                .after(sub_task_batch_block_system),
        ),
    );

    app.add_systems(
        Update,
        (
            brain_dispatch_system
                .in_set(HarnessSet::Dispatch)
                .before(task_dispatch_system),
            task_dispatch_system.in_set(HarnessSet::Dispatch),
            tool_dispatch_system.in_set(HarnessSet::Dispatch),
            evaluation_trigger_system.in_set(HarnessSet::Dispatch),
            agent_execution_system.in_set(HarnessSet::Execution),
            frontend_output_system.in_set(HarnessSet::Output),
        ),
    );

    app.add_systems(
        Update,
        (summarization_result_system
            .in_set(HarnessSet::Transform)
            .after(llm_response_system),),
    );

    app.add_systems(
        Update,
        (
            agent_termination_system
                .in_set(HarnessSet::Maintenance)
                .before(agent_factory_system),
            agent_factory_system.in_set(HarnessSet::Maintenance),
            memory_compression_system.in_set(HarnessSet::Maintenance),
            init_agent_memory_system.in_set(HarnessSet::Maintenance),
            memory_contribution_system.in_set(HarnessSet::Execution),
            memory_absorption_system.in_set(HarnessSet::Maintenance),
            summarization_dispatch_system
                .in_set(HarnessSet::Maintenance)
                .after(agent_factory_system),
            // 审批系统
            approval_dispatch_system.in_set(HarnessSet::Dispatch),
            approval_result_system.in_set(HarnessSet::Transform),
            // 用户确认系统
            tool_confirmation_request_system.in_set(HarnessSet::Output),
            tool_confirmation_result_system
                .in_set(HarnessSet::Dispatch)
                .after(tool_dispatch_system),
        ),
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
