mod dispatch;
mod evaluation;
mod execution;
mod ingress;
mod maintenance;
mod output;
mod routing;
mod transform;

use bevy::ecs::schedule::SystemSet;

pub(crate) use dispatch::{brain_dispatch_system, task_dispatch_system};
pub(crate) use evaluation::{evaluation_result_system, evaluation_trigger_system};
pub(crate) use execution::agent_execution_system;
pub(crate) use ingress::{input_ingress_system, retry_wakeup_system, tick_clock_system};
pub(crate) use maintenance::agent_factory_system;
pub(crate) use output::user_output_system;
pub(crate) use routing::{continue_task_system, user_input_routing_system};
pub(crate) use transform::{
    brain_decision_system, ingest_execution_results_system, llm_response_system,
    retry_ready_system, signal_ingest_system, task_termination_system, user_message_to_task_system,
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
