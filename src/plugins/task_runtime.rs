//! Task Runtime Plugin
//!
//! 提供任务生命周期管理相关的系统。

use crate::prelude::*;

use crate::{
    domain::MemoryConfig,
    domain::TaskEvaluationConfig,
    systems::{
        HarnessSet, chat_round_block_system, chat_round_completion_system,
        chat_session_cleanup_system, init_previous_task_status_system, llm_response_system,
        on_tool_returned_hook_system, retry_ready_system, sub_task_batch_block_system,
        sub_task_completion_system, task_completion_hook_system, task_termination_system,
        tool_calling_orchestrator_system, tool_calling_turn_reset_system, tool_result_system,
    },
};

/// 任务运行时 Plugin
///
/// 管理任务的生命周期：创建、执行、终止、重试。
pub struct TaskRuntimePlugin;

impl Plugin for TaskRuntimePlugin {
    fn build(&self, app: &mut App) {
        // 注册 Task 相关 Resource
        app.init_resource::<MemoryConfig>();
        app.init_resource::<TaskEvaluationConfig>();
        // 终态 hook 派发去重集合（Task 17/18）
        app.init_resource::<crate::systems::TaskTerminalDispatched>();

        // 注册 Task 生命周期系统（保留原始依赖顺序）
        // 注意：finish_task_system 在 FrontendPlugin 中注册（带 after 依赖）
        app.add_systems(
            Update,
            (
                // PreviousTaskStatus 初始化必须在 task_termination_system 之前运行，
                // 确保新创建的 Task 在被 Changed<Task> 检测前已带上初值组件。
                init_previous_task_status_system.in_set(HarnessSet::Transform),
                retry_ready_system.in_set(HarnessSet::Transform),
                task_termination_system
                    .in_set(HarnessSet::Transform)
                    .after(llm_response_system)
                    .after(init_previous_task_status_system),
                // 终态 hook 派发在 task_termination 之后，确保终止清理先跑；
                // 也消费 Changed<Task>，与 task_termination_system 不互相抑制。
                task_completion_hook_system
                    .in_set(HarnessSet::Transform)
                    .after(task_termination_system),
                sub_task_completion_system
                    .in_set(HarnessSet::Transform)
                    .after(task_termination_system),
                sub_task_batch_block_system
                    .in_set(HarnessSet::Transform)
                    .after(tool_result_system),
                chat_round_completion_system
                    .in_set(HarnessSet::Transform)
                    .before(on_tool_returned_hook_system)
                    .before(chat_round_block_system)
                    .before(tool_calling_orchestrator_system),
                chat_round_block_system
                    .in_set(HarnessSet::Transform)
                    .after(tool_result_system),
                chat_session_cleanup_system
                    .in_set(HarnessSet::Maintenance)
                    .after(task_termination_system),
                tool_calling_turn_reset_system.in_set(HarnessSet::Transform),
            ),
        );
    }
}
