//! Loop Control Plugin
//!
//! 引导式插入（steer）、回合收件箱唤醒与请求期上下文注入。

use crate::prelude::*;

use crate::{
    domain::{InflightLlmRequests, LoopControlConfig},
    systems::{
        HarnessSet, MonitorDeliveries, agent_execution_system, background_job_cleanup_system,
        background_job_monitor_system, cancel_monitor_system, clear_stop_requested_system,
        command_parse_system, context_attachment_system, continue_task_system, inbox_ingest_system,
        inbox_status_sync_system, inbox_wake_system, llm_response_system, monitor_output_system,
        stop_task_system, task_termination_system, tool_calling_turn_reset_system,
        user_input_routing_system,
    },
};

/// Loop 控制 Plugin
pub struct LoopControlPlugin;

impl Plugin for LoopControlPlugin {
    fn build(&self, app: &mut App) {
        // 默认兜底；运行时由 app 装配阶段按环境变量覆盖。
        app.init_resource::<LoopControlConfig>();
        app.init_resource::<InflightLlmRequests>();
        app.init_resource::<MonitorDeliveries>();

        app.add_systems(
            Update,
            (
                // 收件箱写入先于 LLM 响应处理：同帧到达的后台通知能被完成判定看到
                inbox_ingest_system
                    .in_set(HarnessSet::Transform)
                    .after(user_input_routing_system)
                    .before(llm_response_system),
                inbox_wake_system
                    .in_set(HarnessSet::Transform)
                    .after(inbox_ingest_system)
                    .after(llm_response_system)
                    .before(continue_task_system),
                context_attachment_system
                    .in_set(HarnessSet::Execution)
                    .before(agent_execution_system),
                stop_task_system
                    .in_set(HarnessSet::Transform)
                    .after(command_parse_system)
                    .after(llm_response_system)
                    .before(tool_calling_turn_reset_system),
                clear_stop_requested_system
                    .in_set(HarnessSet::Maintenance)
                    .after(cancel_monitor_system),
                monitor_output_system
                    .in_set(HarnessSet::Transform)
                    .before(inbox_ingest_system),
                // 后台任务完成检测：先于收件箱写入，使通知同帧入箱
                background_job_monitor_system
                    .in_set(HarnessSet::Transform)
                    .before(inbox_ingest_system),
                // 收件箱积压数量同步到前端（所有 drain 出口之后）
                inbox_status_sync_system.in_set(HarnessSet::Output),
                // 终态清理：保证终态 Task 不留存活后台任务
                background_job_cleanup_system
                    .in_set(HarnessSet::Maintenance)
                    .after(task_termination_system),
            ),
        );
    }
}
