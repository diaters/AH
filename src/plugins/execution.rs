//! Execution Plugin
//!
//! 提供执行相关的系统。

use crate::prelude::*;

use crate::systems::{
    HarnessSet, agent_execution_system, experience_library, ingest_execution_results_system,
    llm_response_system, model_chain_state_update_system, on_llm_response_hook_system,
    tool_calling_orchestrator_system,
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
                // on_llm_response 观察 hook companion 系统
                on_llm_response_hook_system
                    .in_set(HarnessSet::Transform)
                    .after(ingest_execution_results_system),
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
                // ModelChainState 状态更新
                model_chain_state_update_system.in_set(HarnessSet::Execution),
            ),
        );
        app.add_systems(
            Update,
            (
                // 经验库：管理员回复（检索回填 / 预查投递 / 草稿结果）
                experience_library::experience_librarian_response_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::llm_response_system),
                // 经验库：开局预查 → 检索请求 → 管理员 WorkItem
                experience_library::experience_preflight_trigger_system
                    .in_set(HarnessSet::Execution),
                experience_library::experience_lookup_request_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_library::experience_preflight_trigger_system),
                experience_library::experience_draft_request_system.in_set(HarnessSet::Execution),
                experience_library::experience_draft_notify_system.in_set(HarnessSet::Output),
            ),
        );
    }
}
