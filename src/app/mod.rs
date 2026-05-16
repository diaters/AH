use std::sync::Arc;

use anyhow::Result;
use bevy::{app::App, prelude::*};
use chrono::{DateTime, Utc};
use crossbeam_channel::{Receiver, Sender};
use tokio::{runtime::Runtime, sync::mpsc};

use crate::{
    domain::{
        AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentExecutor,
        AgentSpawnRequestMessage, OutputMessage, RetryReadyMessage, Signal, Task,
        TaskEvaluationConfig, TaskTerminatedMessage, UserInputMessage, UserOutputMessage,
    },
    llm::LlmProviderConfig,
    systems::{
        HarnessSet, agent_execution_system, agent_factory_system, brain_decision_system,
        brain_dispatch_system, continue_task_system, ingest_execution_results_system,
        input_ingress_system, llm_response_system, retry_ready_system, retry_wakeup_system,
        signal_ingest_system, task_dispatch_system, task_termination_system, tick_clock_system,
        user_input_routing_system, user_message_to_task_system, user_output_system,
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
            llm: LlmProviderConfig {
                provider: crate::llm::LlmProviderKind::OpenAi,
                model: "gpt-4.1-mini".to_string(),
                api_key: "test-api-key".to_string(),
                api_base: None,
                org_id: None,
                project_id: None,
            },
            brain: None,
            agents_config_path: "agents.toml".to_string(),
        }
    }
}

#[derive(Resource)]
pub struct InputReceiver(pub Receiver<crate::domain::ExternalInput>);

#[derive(Resource, Clone)]
pub struct OutputSender(pub Sender<OutputMessage>);

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
    /// 近期全量保留轮数
    pub recent_turns: u32,
    /// 中期摘要触发阈值
    pub compression_threshold: u32,
    /// 摘要覆盖轮数
    pub summary_window: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            recent_turns: 5,
            compression_threshold: 10,
            summary_window: 5,
        }
    }
}

pub fn build_harness_app(
    config: HarnessConfig,
    runtime: Arc<Runtime>,
    executor: Arc<dyn AgentExecutor>,
    input_rx: Receiver<crate::domain::ExternalInput>,
    output_tx: Sender<OutputMessage>,
) -> App {
    let (result_tx, result_rx) = mpsc::unbounded_channel();
    let mut app = App::new();

    app.insert_resource(InputReceiver(input_rx));
    app.insert_resource(OutputSender(output_tx));
    app.insert_resource(AsyncRuntime(runtime));
    app.insert_resource(ExecutorHandle(executor));
    app.insert_resource(ExecutionResultSender(result_tx));
    app.insert_resource(ExecutionResultReceiver(result_rx));
    app.insert_resource(HarnessSettings(config));
    app.insert_resource(Clock::default());
    app.insert_resource(ShutdownState::default());
    app.insert_resource(MemoryConfig::default());
    app.insert_resource(TaskEvaluationConfig::default());

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
            input_ingress_system.in_set(HarnessSet::Ingress),
            retry_wakeup_system.in_set(HarnessSet::Signal),
            signal_ingest_system.in_set(HarnessSet::Signal),
            ingest_execution_results_system.in_set(HarnessSet::Transform),
            brain_decision_system
                .in_set(HarnessSet::Transform)
                .after(ingest_execution_results_system),
            user_input_routing_system.in_set(HarnessSet::Transform),
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
            brain_dispatch_system
                .in_set(HarnessSet::Dispatch)
                .before(task_dispatch_system),
            task_dispatch_system.in_set(HarnessSet::Dispatch),
            agent_execution_system.in_set(HarnessSet::Execution),
            user_output_system.in_set(HarnessSet::Output),
            agent_factory_system.in_set(HarnessSet::Maintenance),
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

    active_tasks == 0
        && pending_signals == 0
        && pending_user_inputs == 0
        && pending_retry_ready == 0
        && pending_requests == 0
        && pending_results == 0
        && pending_outputs == 0
        && pending_spawn_requests == 0
        && pending_terminated == 0
}
