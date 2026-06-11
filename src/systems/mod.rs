mod command;
mod contribution;
mod dispatch;
mod evaluation;
mod execution;
mod frontend_input;
mod frontend_output;
mod ingress;
mod maintenance;
mod memory;
mod routing;
mod summarization;
pub mod tools;
mod transform;

use bevy::ecs::schedule::SystemSet;

pub(crate) use command::command_parse_system;
pub(crate) use contribution::{
    agent_termination_system, experience_approval_result_system, experience_collection_cleanup_system,
    experience_collection_dispatch_system, experience_governance_system, memory_absorption_system,
    memory_contribution_system,
};
pub(crate) use dispatch::{brain_dispatch_system, task_dispatch_system, workitem_dispatch_system};
pub(crate) use evaluation::evaluation_trigger_system;
pub(crate) use execution::agent_execution_system;
pub(crate) use frontend_input::frontend_input_system;
pub(crate) use frontend_output::frontend_output_system;
pub(crate) use ingress::{input_ingress_system, retry_wakeup_system, tick_clock_system};
pub(crate) use maintenance::{agent_factory_system, load_agents_system};
pub(crate) use memory::{
    init_agent_memory_system, long_term_memory_decay_system, memory_compression_system,
};
pub(crate) use routing::{continue_task_system, user_input_routing_system};
pub(crate) use summarization::summarization_dispatch_system;
pub(crate) use tools::{
    NativeProcessBackend, approval_dispatch_system, approval_result_system,
    check_waiting_tasks_system, on_subtask_completed_check_waiting, register_builtin_tools,
    tool_confirmation_request_system, tool_confirmation_result_system, tool_dispatch_system,
    tool_result_system,
};
pub(crate) use transform::{
    brain_decision_system, finish_task_system, ingest_execution_results_system,
    llm_response_system, retry_ready_system, signal_ingest_system, sub_task_batch_block_system,
    sub_task_completion_system, task_termination_system, tool_calling_orchestrator_system,
    user_message_to_task_system,
};

/// 提供贡献提炼逻辑的稳定公开入口，避免暴露内部系统模块结构。
pub fn extract_memory_writebacks(
    contributor_name: &str,
    task_summary: &crate::domain::TaskSummary,
    memories: &[crate::domain::LongTermMemoryEntry],
) -> (
    Vec<crate::domain::LongTermMemoryEntry>,
    Vec<crate::domain::SharedKnowledgeEntry>,
) {
    contribution::extract_memory_writebacks(contributor_name, task_summary, memories)
}

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
