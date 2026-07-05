mod command;
mod dispatch;
mod evaluation;
mod execution;
pub mod experience;
mod frontend_input;
mod frontend_output;
mod ingress;
mod ingress_hook;
mod knowledge_hook;
mod maintenance;
mod memory;
mod memory_hook;
mod routing;
mod summarization;
pub mod tools;
mod transform;

use bevy_ecs::schedule::SystemSet;

pub(crate) use command::{command_parse_system, reload_plugins_system};
pub(crate) use dispatch::{
    agent_started_hook_system, agent_stopped_hook_system, brain_dispatch_system,
    on_message_dispatched_hook_system, task_dispatch_system, workitem_dispatch_system,
    workitem_lifecycle_hook_system,
};
pub(crate) use evaluation::evaluation_trigger_system;
pub(crate) use execution::agent_execution_system;
pub(crate) use experience::{
    experience_approval_result_system, experience_collection_completion_system,
    experience_collection_workitem_system, experience_consolidation_trigger_system,
    experience_governance_system, experience_writeback_system, on_experience_hook_system,
    task_terminated_experience_trigger_system,
};
pub(crate) use frontend_input::frontend_input_system;
pub(crate) use frontend_output::frontend_output_system;
pub(crate) use ingress::{input_ingress_system, retry_wakeup_system, tick_clock_system};
pub(crate) use ingress_hook::on_message_received_hook_system;
pub(crate) use knowledge_hook::on_shared_knowledge_write_hook_system;
pub(crate) use maintenance::{agent_factory_system, load_agents_system};
pub(crate) use memory::{
    init_agent_memory_system, long_term_memory_decay_system, memory_compression_system,
};
pub(crate) use memory_hook::{on_ltm_evicted_hook_system, on_ltm_write_hook_system};
pub(crate) use routing::{continue_task_system, user_input_routing_system};
pub(crate) use summarization::summarization_dispatch_system;
pub(crate) use tools::{
    NativeProcessBackend, approval_dispatch_system, approval_result_system,
    channel_send_dispatch_system, check_waiting_tasks_system, on_approval_requested_hook_system,
    on_approval_resolved_hook_system, on_subtask_completed_check_waiting,
    on_tool_called_hook_system, on_tool_returned_hook_system, register_builtin_tools,
    tool_confirmation_request_system, tool_confirmation_result_system, tool_dispatch_system,
    tool_result_system,
};
pub use transform::TaskTerminalDispatched;
pub(crate) use transform::{
    brain_decision_system, chat_round_block_system, chat_round_completion_system,
    chat_session_cleanup_system, finish_task_system, ingest_execution_results_system,
    llm_response_system, on_llm_response_hook_system, on_task_created_hook_system,
    retry_ready_system, signal_ingest_system, sub_task_batch_block_system,
    sub_task_completion_system, task_completion_hook_system, task_termination_system,
    tool_calling_orchestrator_system, tool_calling_turn_reset_system,
    user_message_to_task_system,
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
