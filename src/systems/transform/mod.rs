//! Transform 模块
//!
//! 包含数据转换和状态转换相关的 System。

mod brain_decision;
mod chat_round;
mod llm_response;
mod llm_response_hook;
mod signal_ingest;
mod subtask;
mod task_completion_hook;
mod task_creation;
mod task_lifecycle;
mod trigger_task;

pub use brain_decision::brain_decision_system;
pub use chat_round::{
    chat_round_block_system, chat_round_completion_system, chat_session_cleanup_system,
};
pub use llm_response::{llm_response_system, tool_calling_orchestrator_system};
pub use llm_response_hook::on_llm_response_hook_system;
pub use signal_ingest::signal_ingest_system;
pub use subtask::{sub_task_batch_block_system, sub_task_completion_system};
pub use task_completion_hook::{TaskTerminalDispatched, task_completion_hook_system};
pub use task_creation::{on_task_created_hook_system, user_message_to_task_system};
pub use task_lifecycle::{
    finish_task_system, retry_ready_system, task_termination_system, tool_calling_turn_reset_system,
};
pub use trigger_task::trigger_task_routing_system;

use crate::prelude::*;

use crate::{
    app::ExecutionResultReceiver,
    domain::{AgentExecutionResultMessage, LlmResponseHookPending},
};

/// 执行结果接收 System
///
/// 从异步通道接收执行结果并转换为消息实体。
pub fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn((
            AgentExecutionResultMessage { result },
            LlmResponseHookPending,
        ));
    }
}
