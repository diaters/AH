//! Frontend Plugin
//!
//! 提供前端交互相关的系统。

use crate::prelude::*;

use crate::systems::{
    HarnessSet, clear_task_system, command_parse_system, continue_task_system, finish_task_system,
    frontend_input_system, frontend_output_system, input_ingress_system,
    on_message_received_hook_system, on_task_created_hook_system, once_schedule_completion_system,
    reload_plugins_system, reload_triggers_message_consumer_system, retry_wakeup_system,
    schedule_catch_up_startup_system, schedule_persist_system, signal_ingest_system,
    tick_clock_system, trigger_task_routing_system, user_input_routing_system,
    user_message_to_task_system,
};

/// 前端 Plugin
///
/// 负责前端输入/输出和用户交互。
pub struct FrontendPlugin;

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<crate::systems::ScheduleSubscriptions>();
        app.add_systems(
            Update,
            (
                crate::systems::client_schedule_request_system.in_set(HarnessSet::Transform),
                crate::systems::client_schedule_snapshot_system
                    .in_set(HarnessSet::Output)
                    .after(schedule_persist_system)
                    .after(once_schedule_completion_system),
            ),
        );
        app.add_systems(
            Update,
            (
                crate::systems::interaction_request_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::conversation_input_order_system)
                    .before(command_parse_system)
                    .before(user_input_routing_system),
                crate::systems::interaction_state_system
                    .in_set(HarnessSet::Output)
                    .after(frontend_output_system),
            ),
        );
        app.init_resource::<crate::domain::InputSequencer>();
        app.init_resource::<crate::systems::ScheduleDeliveryTracker>();
        app.add_systems(
            Update,
            crate::systems::schedule_run_accounting_system
                .in_set(HarnessSet::Output)
                .after(frontend_output_system)
                .before(once_schedule_completion_system)
                .before(schedule_persist_system),
        );
        // 启动补跑：为错过运行的 cron 定时任务生成一次补跑触发
        app.add_systems(Startup, schedule_catch_up_startup_system);
        app.add_systems(
            Update,
            crate::systems::conversation_input_order_system
                .in_set(HarnessSet::Transform)
                .before(command_parse_system)
                .before(user_input_routing_system),
        );
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
                // 触发器任务路由（消费 TriggerTaskMessage）
                trigger_task_routing_system
                    .in_set(HarnessSet::Signal)
                    .after(signal_ingest_system),
                // 命令解析
                command_parse_system.in_set(HarnessSet::Transform),
                // /reload-plugins 伴生系统（消费 ReloadPluginsMessage）
                reload_plugins_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // /reload-triggers 伴生系统（消费 ReloadTriggersMessage）
                reload_triggers_message_consumer_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // 任务完成
                finish_task_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system),
                // 任务清除（/clear，不触发终态处理链路）
                clear_task_system
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
                // 动态定时任务定义变化时写盘：变更检测保证晚于本系统的修改（如 Maintenance
                // 阶段的工具效果落账）在下一帧写入，不会遗漏
                schedule_persist_system.in_set(HarnessSet::Output),
                // 一次性定时任务的运行进入终态后删除定义（先于写盘，同帧落盘）
                once_schedule_completion_system
                    .in_set(HarnessSet::Output)
                    .before(schedule_persist_system),
            ),
        );
    }
}
