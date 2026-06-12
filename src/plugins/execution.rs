//! Execution Plugin
//!
//! 提供执行相关的系统。

use bevy::prelude::*;

use crate::domain::ExperienceCollectionTracker;
use crate::systems::{
    HarnessSet, agent_execution_system, agent_termination_system,
    experience_approval_result_system, experience_collection_cleanup_system,
    experience_collection_dispatch_system, experience_governance_system,
    ingest_execution_results_system, llm_response_system, memory_contribution_system,
    tool_calling_orchestrator_system,
};

/// 执行 Plugin
///
/// 负责 LLM 调用和执行结果处理。
pub struct ExecutionPlugin;

impl Plugin for ExecutionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ExperienceCollectionTracker::default());

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
                // 经验收集：Agent 终止触发收集请求
                agent_termination_system.in_set(HarnessSet::Execution),
                // 经验收集：派发收集 follow-up 执行请求
                experience_collection_dispatch_system
                    .in_set(HarnessSet::Execution)
                    .after(agent_termination_system),
                // 经验收集后清理：despawn 完成收集的 task-scoped agent
                experience_collection_cleanup_system.in_set(HarnessSet::Maintenance),
                // 经验治理：决定候选的持久化路径
                experience_governance_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_collection_dispatch_system),
                // 经验确认结果：处理用户对经验候选的确认
                experience_approval_result_system.in_set(HarnessSet::Maintenance),
                // 记忆贡献（保留旧链路，待 Task 5 完成后移除）
                memory_contribution_system.in_set(HarnessSet::Execution),
            ),
        );
    }
}
