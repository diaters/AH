use crate::prelude::*;
use chrono::Utc;
use tracing::{debug, info, trace};

use crate::{
    contracts::Clock,
    domain::{
        ExternalInput, MessageReceivedHookPending, Signal, Task, TaskStatus,
        ToolConfirmationResponseMessage, WaitingReason,
    },
    domain::{InputReceiver, InputSequencer, ShutdownState},
};

/// 更新统一时钟资源，避免各系统直接读取系统时间。
pub(crate) fn tick_clock_system(
    mut clock: ResMut<Clock>,
    mut schedule: Option<ResMut<crate::systems::demand::DemandSchedule>>,
) {
    let old_tick = clock.0;
    let monotonic = std::time::Instant::now();
    clock.0 = Utc::now();
    if let Some(schedule) = schedule.as_deref_mut() {
        schedule.sample_clock(clock.0, monotonic);
    }
    trace!(
        event = "TickClock",
        old_tick = %old_tick.format("%Y-%m-%d %H:%M:%S%.3f"),
        new_tick = %clock.0.format("%Y-%m-%d %H:%M:%S%.3f"),
        "clock ticked"
    );
}

/// 将外部线程输入转为 ECS 内部 Signal 或确认响应。
pub(crate) fn input_ingress_system(
    receiver: Res<InputReceiver>,
    mut shutdown: ResMut<ShutdownState>,
    mut sequence: ResMut<InputSequencer>,
    mut commands: Commands,
    mut buffered: Option<ResMut<crate::contracts::BufferedIngress>>,
) {
    let pending = buffered
        .as_deref_mut()
        .map(|b| std::mem::take(&mut b.inputs))
        .unwrap_or_default();
    for input in pending.into_iter().chain(receiver.0.try_iter().take(256)) {
        match input {
            ExternalInput::TextWithChannel {
                channel,
                content,
                sender,
            } => {
                debug!(
                    event = "ExternalInputReceived",
                    kind = "TextWithChannel",
                    content_len = content.len(),
                    "received external text input"
                );
                info!(
                    event = "ExternalInputReceived",
                    kind = "TextWithChannel",
                    channel = ?channel,
                    content_len = content.len(),
                    "收到外部输入：通道={:?}，内容长度 {}",
                    channel,
                    content.len()
                );
                commands.spawn((
                    Signal::user_input_with_channel(content, channel).with_sender(sender),
                    MessageReceivedHookPending,
                    sequence.next_sequence(),
                ));
            }
            ExternalInput::Shutdown => {
                debug!(
                    event = "ExternalInputReceived",
                    kind = "Shutdown",
                    "received shutdown signal"
                );
                shutdown.requested = true;
            }
            ExternalInput::Confirmation {
                request_id,
                option,
                feedback,
                sender,
            } => {
                debug!(
                    event = "ExternalInputReceived",
                    kind = "Confirmation",
                    request_id = %request_id,
                    option = %option,
                    "received tool confirmation"
                );
                commands.spawn((
                    ToolConfirmationResponseMessage {
                        request_id,
                        selected_option: option,
                        feedback,
                        sender,
                    },
                    MessageReceivedHookPending,
                ));
            }
            ExternalInput::Webhook { source, kind, body } => {
                debug!(
                    event = "ExternalInputReceived",
                    kind = "Webhook",
                    source = ?source,
                    webhook_kind = %kind,
                    "received webhook event"
                );
                commands.spawn((
                    Signal::webhook(source, kind, body),
                    MessageReceivedHookPending,
                ));
            }
            ExternalInput::Timer { source, kind } => {
                debug!(
                    event = "ExternalInputReceived",
                    kind = "Timer",
                    source = ?source,
                    timer_kind = %kind,
                    "received timer event"
                );
                commands.spawn((Signal::timer(source, kind), MessageReceivedHookPending));
            }
        }
    }
}

/// 监测到达回退时间的任务，并生成重试唤醒 Signal。
pub(crate) fn retry_wakeup_system(
    clock: Res<Clock>,
    mut commands: Commands,
    tasks: Query<(Entity, &Task)>,
    schedule: Option<Res<crate::systems::demand::DemandSchedule>>,
) {
    for (entity, task) in &tasks {
        if task.status != TaskStatus::Waiting(WaitingReason::RetryBackoff) {
            continue;
        }

        if let Some(next_retry_at) = task.next_retry_at
            && schedule
                .as_deref()
                .map_or(next_retry_at <= clock.0, |schedule| {
                    schedule.expired_at(
                        crate::systems::demand::DeadlineKey::Retry(entity),
                        next_retry_at,
                        clock.0,
                    )
                })
        {
            debug!(
                event = "RetryWakeupTriggered",
                task_id = %task.id,
                retry_count = task.retry_count,
                max_retries = task.max_retries,
                next_retry_at = %next_retry_at.format("%Y-%m-%d %H:%M:%S%.3f"),
                current_time = %clock.0.format("%Y-%m-%d %H:%M:%S%.3f"),
                last_error = ?task.last_error,
                "retry backoff elapsed, waking up task"
            );
            commands.spawn(Signal::retry_wakeup(task.id));
        }
    }
}
