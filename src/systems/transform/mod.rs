//! Transform 模块
//!
//! 包含数据转换和状态转换相关的 System。

mod brain_decision;
mod llm_response;
mod signal_ingest;
mod subtask;
mod task_creation;
mod task_lifecycle;

pub use brain_decision::brain_decision_system;
pub use llm_response::{llm_response_system, tool_calling_orchestrator_system};
pub use signal_ingest::signal_ingest_system;
pub use subtask::{sub_task_batch_block_system, sub_task_completion_system};
pub use task_creation::user_message_to_task_system;
pub use task_lifecycle::{finish_task_system, retry_ready_system, task_termination_system};

use bevy::prelude::*;

use crate::{app::ExecutionResultReceiver, domain::AgentExecutionResultMessage};

/// 执行结果接收 System
///
/// 从异步通道接收执行结果并转换为消息实体。
pub fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn(AgentExecutionResultMessage { result });
    }
}
