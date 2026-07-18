//! Dispatch Plugin
//!
//! 提供任务派发相关的系统。

use crate::prelude::*;

use crate::systems::{
    HarnessSet, agent_started_hook_system, agent_stopped_hook_system, approval_dispatch_system,
    approval_result_system, brain_decision_system, brain_dispatch_system, dispatch_system,
    evaluation_trigger_system, on_approval_requested_hook_system, on_approval_resolved_hook_system,
    on_message_dispatched_hook_system, subtask_dispatch_preparation_system,
    tool_confirmation_result_system, workitem_lifecycle_hook_system,
};

/// 派发 Plugin
///
/// 负责任务到 Agent 的派发决策。
pub struct DispatchPlugin;

impl Plugin for DispatchPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                // Brain 派发系统（处理无 PendingDispatch 的旧路径，由 Brain 选 Agent）
                // 注：保留 .before(subtask_dispatch_preparation_system) 以确保 brain_dispatch
                // 优先处理 SubTask 路径（brain_dispatch 仍保留旧 SubTask 直派发逻辑）。
                // subtask_dispatch_preparation_system 仅在 brain_dispatch 未处理时附加 PendingDispatch。
                brain_decision_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::ingest_execution_results_system),
                brain_dispatch_system
                    .in_set(HarnessSet::Dispatch)
                    .before(subtask_dispatch_preparation_system),
                // 统一派发系统（处理带 PendingDispatch 的 Task / WorkItem）
                dispatch_system.in_set(HarnessSet::Dispatch),
                // SubTask 派发前置系统（为 SubTask 附加 PendingDispatch）
                subtask_dispatch_preparation_system
                    .in_set(HarnessSet::Dispatch)
                    .before(dispatch_system),
                // WorkItem 生命周期 hook companion 系统
                workitem_lifecycle_hook_system.in_set(HarnessSet::Dispatch),
                // on_message_dispatched 观察 hook companion 系统
                on_message_dispatched_hook_system.in_set(HarnessSet::Dispatch),
                // Agent 生命周期 hook companion 系统
                // 两者均放在 Maintenance 集合中：
                // - agent_started_hook_system 使用 Added<Agent>，与 agent_factory_system 在同一帧即可触发
                // - agent_stopped_hook_system 必须在 agent_factory_system（handle_termination 插入标记）之后运行
                agent_started_hook_system.in_set(HarnessSet::Maintenance),
                agent_stopped_hook_system
                    .in_set(HarnessSet::Maintenance)
                    .after(crate::systems::agent_factory_system),
                // 评估系统
                evaluation_trigger_system.in_set(HarnessSet::Dispatch),
                // 审批系统
                approval_dispatch_system.in_set(HarnessSet::Dispatch),
                // on_approval_requested 观察 hook companion 系统
                on_approval_requested_hook_system
                    .in_set(HarnessSet::Dispatch)
                    .after(approval_dispatch_system),
                approval_result_system.in_set(HarnessSet::Transform),
                // on_approval_resolved 观察 hook companion 系统
                on_approval_resolved_hook_system
                    .in_set(HarnessSet::Transform)
                    .after(approval_result_system),
                // 用户确认结果系统
                tool_confirmation_result_system
                    .in_set(HarnessSet::Dispatch)
                    .after(crate::systems::tool_dispatch_system),
            ),
        );
    }
}
