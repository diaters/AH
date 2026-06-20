//! Execution Plugin
//!
//! 提供执行相关的系统。

use bevy::prelude::*;

use crate::systems::{
    HarnessSet, agent_execution_system, experience_approval_result_system,
    experience_collection_completion_system, experience_collection_workitem_system,
    experience_consolidation_trigger_system, experience_governance_system,
    experience_writeback_system, ingest_execution_results_system, llm_response_system,
    task_terminated_experience_trigger_system, tool_calling_orchestrator_system,
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
                // 经验收集：任务终态触发收集请求
                task_terminated_experience_trigger_system.in_set(HarnessSet::Execution),
                // 经验收集：将请求转换为 WorkItem
                experience_collection_workitem_system
                    .in_set(HarnessSet::Execution)
                    .after(task_terminated_experience_trigger_system),
                // 经验收集完成后汇聚与治理触发
                experience_collection_completion_system
                    .in_set(HarnessSet::Execution)
                    .after(crate::systems::llm_response_system)
                    .before(experience_governance_system),
                // 经验合并：对同类候选去重合并
                experience_consolidation_trigger_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_collection_completion_system),
                // 经验治理：决定候选的持久化路径
                experience_governance_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_collection_completion_system),
                // 统一写回：执行治理决议的实际持久化
                experience_writeback_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_governance_system),
                // 经验确认结果：处理用户对经验候选的确认
                experience_approval_result_system
                    .in_set(HarnessSet::Execution)
                    .after(crate::systems::tool_confirmation_result_system)
                    .after(experience_governance_system)
                    .before(experience_writeback_system),
            ),
        );
    }
}
