//! Execution Plugin
//!
//! 提供执行相关的系统。

use crate::prelude::*;

use crate::systems::{
    HarnessSet, agent_execution_system, experience_approval_result_system,
    experience_collection_completion_system, experience_collection_workitem_system,
    experience_consolidation_trigger_system, experience_governance_system,
    experience_writeback_system, ingest_execution_results_system, llm_response_system,
    model_chain_state_update_system, on_experience_hook_system, on_llm_response_hook_system,
    profile_generation_completion_system, profile_generation_workitem_system,
    profile_update_trigger_system, profile_update_writeback_system,
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
                // profile 生成：消费治理产出的 ProfileGenerationRequestMessage，创建 WorkItem
                profile_generation_workitem_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_governance_system),
                // profile 生成完成：消费 LLM 响应，创建 proposal 并发起审批
                profile_generation_completion_system
                    .in_set(HarnessSet::Execution)
                    .after(crate::systems::llm_response_system)
                    .before(experience_approval_result_system),
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
                // profile 更新触发：LTM/SkillPackage 写回成功后触发 Update 类型 profile 生成
                profile_update_trigger_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_writeback_system),
                // profile 更新写回：审批通过后更新 agents.toml 和 ECS Agent.capabilities
                profile_update_writeback_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_approval_result_system),
                // 经验候选相关 hook companion 系统
                on_experience_hook_system
                    .in_set(HarnessSet::Execution)
                    .after(experience_approval_result_system),
            ),
        );
    }
}
