//! Frontend Plugin
//!
//! 提供前端交互相关的系统。

use bevy::prelude::*;

use crate::systems::{
    HarnessSet, command_parse_system, continue_task_system, finish_task_system,
    frontend_input_system, frontend_output_system, input_ingress_system,
    on_message_received_hook_system, on_shared_knowledge_write_hook_system,
    on_task_created_hook_system, reload_plugins_system, retry_wakeup_system, signal_ingest_system,
    tick_clock_system, tool_confirmation_request_system, user_input_routing_system,
    user_message_to_task_system,
};

/// 前端 Plugin
///
/// 负责前端输入/输出和用户交互。
pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                // 时钟更新
                tick_clock_system.in_set(HarnessSet::Ingress),
                // 前端输入处理
                frontend_input_system.in_set(HarnessSet::Ingress),
                // 输入入口
                input_ingress_system.in_set(HarnessSet::Ingress),
                // on_message_received 观察 hook companion 系统
                on_message_received_hook_system
                    .in_set(HarnessSet::Ingress)
                    .after(input_ingress_system),
                // 重试唤醒
                retry_wakeup_system.in_set(HarnessSet::Signal),
                // 信号转换
                signal_ingest_system.in_set(HarnessSet::Signal),
                // 命令解析
                command_parse_system.in_set(HarnessSet::Transform),
                // /reload-plugins 伴生系统（消费 ReloadPluginsMessage）
                reload_plugins_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // on_shared_knowledge_write 观察 hook companion 系统
                on_shared_knowledge_write_hook_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // 任务完成
                finish_task_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // 用户输入路由
                user_input_routing_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // 用户消息转任务
                user_message_to_task_system
                    .in_set(HarnessSet::Transform)
                    .after(user_input_routing_system),
                // 派发 on_task_created hook（独占 &mut World，在 task 创建之后）
                on_task_created_hook_system
                    .in_set(HarnessSet::Transform)
                    .after(user_message_to_task_system),
                // 继续任务
                continue_task_system
                    .in_set(HarnessSet::Transform)
                    .after(user_input_routing_system),
                // 前端输出
                frontend_output_system.in_set(HarnessSet::Output),
                // Tool 确认请求
                tool_confirmation_request_system.in_set(HarnessSet::Output),
            ),
        );
    }
}
