//! Dispatch Plugin
//!
//! 提供任务派发相关的系统。

use bevy::prelude::*;

use crate::systems::{
    HarnessSet, approval_dispatch_system, approval_result_system, brain_decision_system,
    brain_dispatch_system, evaluation_result_system, evaluation_trigger_system,
    task_dispatch_system, tool_confirmation_result_system, workitem_dispatch_system,
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
                // Brain 派发系统
                brain_decision_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::ingest_execution_results_system),
                brain_dispatch_system
                    .in_set(HarnessSet::Dispatch)
                    .before(task_dispatch_system),
                // 任务派发系统
                task_dispatch_system.in_set(HarnessSet::Dispatch),
                // WorkItem 派发系统
                workitem_dispatch_system
                    .in_set(HarnessSet::Dispatch)
                    .after(task_dispatch_system),
                // 评估系统
                evaluation_trigger_system.in_set(HarnessSet::Dispatch),
                evaluation_result_system.in_set(HarnessSet::Transform),
                // 审批系统
                approval_dispatch_system.in_set(HarnessSet::Dispatch),
                approval_result_system.in_set(HarnessSet::Transform),
                // 用户确认结果系统
                tool_confirmation_result_system
                    .in_set(HarnessSet::Dispatch)
                    .after(crate::systems::tool_dispatch_system),
            ),
        );
    }
}
