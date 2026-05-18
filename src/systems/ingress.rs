use bevy::prelude::*;
use chrono::Utc;

use crate::{
    app::{Clock, InputReceiver, ShutdownState},
    domain::{
        ExternalInput, Signal, SignalPayload, Task, TaskStatus, ToolConfirmationResponseMessage,
        WaitingReason,
    },
};

/// 更新统一时钟资源，避免各系统直接读取系统时间。
pub(crate) fn tick_clock_system(mut clock: ResMut<Clock>) {
    clock.0 = Utc::now();
}

/// 将外部线程输入转为 ECS 内部 Signal 或确认响应。
pub(crate) fn input_ingress_system(
    receiver: Res<InputReceiver>,
    mut shutdown: ResMut<ShutdownState>,
    mut commands: Commands,
) {
    while let Ok(input) = receiver.0.try_recv() {
        match input {
            ExternalInput::Text(content) => {
                commands.spawn(Signal::user_input(content));
            }
            ExternalInput::Shutdown => {
                shutdown.requested = true;
            }
            ExternalInput::Confirmation { request_id, option } => {
                commands.spawn(ToolConfirmationResponseMessage {
                    request_id,
                    selected_option: option,
                });
            }
        }
    }
}

/// 监测到达回退时间的任务，并生成重试唤醒 Signal。
pub(crate) fn retry_wakeup_system(clock: Res<Clock>, mut commands: Commands, tasks: Query<&Task>) {
    for task in &tasks {
        if task.status != TaskStatus::Waiting(WaitingReason::RetryBackoff) {
            continue;
        }

        if task
            .next_retry_at
            .is_some_and(|next_retry_at| next_retry_at <= clock.0)
        {
            commands.spawn(Signal {
                kind: crate::domain::SignalType::RetryWakeup,
                payload: SignalPayload::RetryWakeup(task.id),
            });
        }
    }
}
