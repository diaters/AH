//! Transform 模块
//!
//! 包含数据转换和状态转换相关的 System。

mod chat_round;
mod llm_response;
mod llm_response_hook;
mod signal_ingest;
mod subtask;
mod task_completion_hook;
mod task_creation;
mod task_lifecycle;
mod trigger_task;

pub use chat_round::{
    chat_round_block_system, chat_round_completion_system, chat_session_cleanup_system,
};
pub use llm_response::llm_response_system;
pub use llm_response_hook::on_llm_response_hook_system;
pub use signal_ingest::signal_ingest_system;
pub use subtask::{sub_task_batch_block_system, sub_task_completion_system};
pub use task_completion_hook::{TaskTerminalDispatched, task_completion_hook_system};
pub use task_creation::{on_task_created_hook_system, user_message_to_task_system};
pub use task_lifecycle::{
    clear_task_system, finish_task_system, init_previous_task_status_system, retry_ready_system,
    task_termination_system, tool_calling_turn_reset_system,
};
pub use trigger_task::trigger_task_routing_system;

use crate::prelude::*;

use tracing::debug;

use crate::{
    domain::ExecutionResultReceiver,
    domain::{AgentExecutionResultMessage, LlmResponseHookPending, TurnGeneration},
    ecs::EntityIndex,
};

/// 执行结果接收 System
///
/// 从异步通道接收执行结果并转换为消息实体。结果落地单点同时裁决回合代次：
/// 代次小于任务当前 `TurnGeneration` 的结果属于 `/stop` 之前的回合，直接丢弃。
pub fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
    index: Option<Res<EntityIndex>>,
    generations: Query<&TurnGeneration>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        let current_generation = index
            .as_ref()
            .and_then(|index| index.get_task(&result.task_id))
            .and_then(|entity| generations.get(entity).ok())
            .map(|g| g.0);
        if let (Some(result_generation), Some(current)) =
            (result.turn_generation, current_generation)
            && result_generation < current
        {
            debug!(
                event = "StaleExecutionResultDropped",
                task_id = %result.task_id,
                result_generation = result_generation,
                current_generation = current,
                "dropping llm result from a stopped turn"
            );
            continue;
        }
        commands.spawn((
            AgentExecutionResultMessage { result },
            LlmResponseHookPending,
        ));
    }
}
