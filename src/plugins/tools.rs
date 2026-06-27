//! Tool Runtime Plugin
//!
//! 提供工具执行相关的系统。

use bevy::prelude::*;

use crate::{
    domain::{BuiltinToolExecutors, ExperienceStore, PendingExperienceHooks, SpaceToolRegistry},
    systems::{
        HarnessSet, NativeProcessBackend, channel_send_dispatch_system, check_waiting_tasks_system,
        on_subtask_completed_check_waiting, on_tool_called_hook_system,
        on_tool_returned_hook_system, register_builtin_tools, tool_dispatch_system,
        tool_result_system,
    },
};

/// 工具运行时 Plugin
///
/// 负责工具的注册、分发和结果处理。
pub struct ToolRuntimePlugin;

impl Plugin for ToolRuntimePlugin {
    fn build(&self, app: &mut App) {
        // 注册 Tool 相关 Resource
        let mut tool_registry = SpaceToolRegistry::default();
        let mut tool_executors = BuiltinToolExecutors::default();
        register_builtin_tools(&mut tool_registry, &mut tool_executors);
        app.insert_resource(tool_registry);
        app.insert_resource(tool_executors);
        app.insert_resource(ExperienceStore::default());
        app.insert_resource(PendingExperienceHooks::default());
        app.insert_resource(NativeProcessBackend::default());

        // 注册 Tool 相关系统
        app.add_systems(
            Update,
            (
                // on_tool_called 前置 hook companion 系统：在 tool_dispatch_system 之前派发 hook，
                // 若插件调用 tool_deny 则替换为 PermissionDenied 错误结果并销毁请求。
                on_tool_called_hook_system
                    .in_set(HarnessSet::Dispatch)
                    .before(tool_dispatch_system),
                // Tool 分发
                tool_dispatch_system.in_set(HarnessSet::Dispatch),
                // on_tool_returned 观察 hook companion 系统：在 tool_result_system 之前派发 hook，
                // 若插件调用 tool_set_result 则替换 tool_output，原始输出保留在审计字段。
                on_tool_returned_hook_system
                    .in_set(HarnessSet::Transform)
                    .before(tool_result_system),
                // Tool 结果处理
                tool_result_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::ingest_execution_results_system),
                // 等待任务检查
                check_waiting_tasks_system
                    .in_set(HarnessSet::Transform)
                    .after(tool_result_system),
                on_subtask_completed_check_waiting
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::sub_task_completion_system),
                // channel_send 工具出向消息派发 companion 系统
                channel_send_dispatch_system
                    .in_set(HarnessSet::Maintenance)
                    .after(tool_dispatch_system),
            ),
        );
    }
}
