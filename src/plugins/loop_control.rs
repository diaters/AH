//! Loop Control Plugin
//!
//! 引导式插入（steer）、回合收件箱唤醒与请求期上下文注入。

use crate::prelude::*;

use crate::{
    domain::LoopControlConfig,
    systems::{
        HarnessSet, agent_execution_system, context_attachment_system, continue_task_system,
        inbox_ingest_system, inbox_wake_system, llm_response_system, user_input_routing_system,
    },
};

/// Loop 控制 Plugin
pub struct LoopControlPlugin;

impl Plugin for LoopControlPlugin {
    fn build(&self, app: &mut App) {
        // 默认兜底；运行时由 app 装配阶段按环境变量覆盖。
        app.init_resource::<LoopControlConfig>();

        app.add_systems(
            Update,
            (
                inbox_ingest_system
                    .in_set(HarnessSet::Transform)
                    .after(user_input_routing_system),
                inbox_wake_system
                    .in_set(HarnessSet::Transform)
                    .after(inbox_ingest_system)
                    .after(llm_response_system)
                    .before(continue_task_system),
                context_attachment_system
                    .in_set(HarnessSet::Execution)
                    .before(agent_execution_system),
            ),
        );
    }
}
