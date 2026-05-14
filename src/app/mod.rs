use std::sync::Arc;

use anyhow::Result;
use bevy::{app::App, prelude::*};
use chrono::{DateTime, Utc};
use crossbeam_channel::{Receiver, Sender};
use tokio::{runtime::Runtime, sync::mpsc};

use crate::{
    domain::{
        AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentExecutor, OutputMessage,
        RetryReadyMessage, Signal, Task, UserInputMessage, UserOutputMessage,
    },
    llm::LlmProviderConfig,
    systems::{
        agent_execution_system, agent_factory_system, brain_decision_system,
        brain_dispatch_system, ingest_execution_results_system, input_ingress_system,
        llm_response_system, retry_ready_system, retry_wakeup_system, signal_ingest_system,
        spawn_default_agent_system, task_dispatch_system, tick_clock_system,
        user_message_to_task_system, user_output_system, HarnessSet,
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
    pub default_agent_name: String,
    pub max_retries: u32,
    pub llm: LlmProviderConfig,
    pub brain: Option<BrainConfig>,
}

impl HarnessConfig {
    /// 从环境变量加载运行配置，并补齐 MVP 默认值。
    pub fn from_env() -> Result<Self> {
        let llm = LlmProviderConfig::from_env("gpt-4.1-mini")?;

        let brain = if std::env::var("HARNESS_BRAIN_ENABLED")
            .is_ok_and(|v| v.to_lowercase() == "true")
        {
            Some(BrainConfig {
                enabled: true,
                model: std::env::var("HARNESS_BRAIN_MODEL")
                    .unwrap_or_else(|_| llm.model.clone()),
                agent_name: std::env::var("HARNESS_BRAIN_AGENT_NAME")
                    .unwrap_or_else(|_| "brain".to_string()),
            })
        } else {
            None
        };

        Ok(Self {
            default_agent_name: "default-llm-agent".to_string(),
            max_retries: 3,
            llm,
            brain,
        })
    }
}

impl Default for HarnessConfig {
    /// 提供适用于测试和本地组装的默认配置。
    fn default() -> Self {
        Self {
            default_agent_name: "default-llm-agent".to_string(),
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
pub struct ExecutionResultReceiver(pub mpsc::UnboundedReceiver<crate::domain::AgentExecutionResult>);

#[derive(Resource)]
pub struct HarnessSettings(pub HarnessConfig);

#[derive(Resource)]
pub struct Clock(pub DateTime<Utc>);

impl Default for Clock {
    /// 为调度系统提供统一时间源。
    fn default() -> Self {
        Self(Utc::now())
    }
}

#[derive(Resource, Default)]
pub struct ShutdownState {
    pub requested: bool,
}

/// 构造符合 MVP 设计的 Harness 应用实例。
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

    app.add_systems(Startup, spawn_default_agent_system);
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
            user_message_to_task_system.in_set(HarnessSet::Transform),
            retry_ready_system.in_set(HarnessSet::Transform),
            llm_response_system
                .in_set(HarnessSet::Transform)
                .after(ingest_execution_results_system),
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

/// 判断应用是否已经没有活跃任务与暂存消息。
pub fn app_is_idle(world: &mut World) -> bool {
    let active_tasks = world
        .query::<&Task>()
        .iter(world)
        .filter(|task| !task.status.is_terminal())
        .count();
    let pending_signals = world.query::<&Signal>().iter(world).count();
    let pending_user_inputs = world.query::<&UserInputMessage>().iter(world).count();
    let pending_retry_ready = world.query::<&RetryReadyMessage>().iter(world).count();
    let pending_requests = world.query::<&AgentExecutionRequestMessage>().iter(world).count();
    let pending_results = world.query::<&AgentExecutionResultMessage>().iter(world).count();
    let pending_outputs = world.query::<&UserOutputMessage>().iter(world).count();

    active_tasks == 0
        && pending_signals == 0
        && pending_user_inputs == 0
        && pending_retry_ready == 0
        && pending_requests == 0
        && pending_results == 0
        && pending_outputs == 0
}
