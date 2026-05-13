mod dispatch;
mod execution;
mod ingress;
mod maintenance;
mod output;
mod transform;

use bevy::ecs::schedule::SystemSet;

pub(crate) use dispatch::task_dispatch_system;
pub(crate) use execution::agent_execution_system;
pub(crate) use ingress::{input_ingress_system, retry_wakeup_system, tick_clock_system};
pub(crate) use maintenance::{agent_factory_system, spawn_default_agent_system};
pub(crate) use output::user_output_system;
pub(crate) use transform::{
    ingest_execution_results_system, llm_response_system, retry_ready_system,
    signal_ingest_system, user_message_to_task_system,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, SystemSet)]
pub enum HarnessSet {
    Ingress,
    Signal,
    Transform,
    Dispatch,
    Execution,
    Output,
    Maintenance,
}
