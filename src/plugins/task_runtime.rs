//! Task Runtime Plugin
//!
//! 提供任务生命周期管理相关的系统。

use bevy::prelude::*;

use crate::{
    app::MemoryConfig,
    domain::TaskEvaluationConfig,
    systems::{
        finish_task_system, retry_ready_system, sub_task_batch_block_system,
        sub_task_completion_system, task_termination_system, HarnessSet,
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

        // 注册 Task 生命周期系统
        app.add_systems(
            Update,
            (
                retry_ready_system.in_set(HarnessSet::Transform),
                finish_task_system.in_set(HarnessSet::Transform),
                task_termination_system.in_set(HarnessSet::Transform),
                sub_task_completion_system.in_set(HarnessSet::Transform),
                sub_task_batch_block_system.in_set(HarnessSet::Transform),
            ),
        );
    }
}
