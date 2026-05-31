//! Execution Plugin
//!
//! 提供执行相关的系统。

use bevy::prelude::*;

use crate::systems::{
    agent_execution_system, ingest_execution_results_system, llm_response_system,
    memory_contribution_system, tool_calling_orchestrator_system, HarnessSet,
};

/// 执行 Plugin
///
/// 负责 LLM 调用和执行结果处理。
pub struct ExecutionPlugin;

impl Plugin for ExecutionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                // 执行结果接收
                ingest_execution_results_system.in_set(HarnessSet::Transform),
                // LLM 响应处理
                llm_response_system
                    .in_set(HarnessSet::Transform)
                    .after(ingest_execution_results_system),
                // Tool 调用协调
                tool_calling_orchestrator_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::sub_task_batch_block_system),
                // Agent 执行
                agent_execution_system.in_set(HarnessSet::Execution),
                // 记忆贡献
                memory_contribution_system.in_set(HarnessSet::Execution),
            ),
        );
    }
}
