//! 信号转换 System
//!
//! 将 Signal 转换为对应的 Message。

use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    CreateTaskMessage, RetryReadyMessage, Signal, SignalPayload, Task, TaskRoutingPolicy,
    TriggerTaskMessage, UserInputMessage,
};
use crate::ecs::EntityIndex;

/// 信号转换 System
///
/// 将 Signal 转换为对应的 Message。
///
/// 对于 `RetryWakeup` Signal，在 spawn `RetryReadyMessage` 之前清除 `task.next_retry_at`，
/// 防止 `retry_wakeup_system` 在下一帧再次触发（因为 `retry_wakeup_system` 和本系统
/// 在同一 `HarnessSet::Signal` 集合中运行，执行顺序不确定）。
pub fn signal_ingest_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut tasks: Query<&mut Task>,
    signals: Query<(Entity, &Signal)>,
) {
    for (entity, signal) in &signals {
        match &signal.payload {
            SignalPayload::UserInput(content) => {
                debug!(
                    event = "SignalIngested",
                    signal_source = ?signal.source,
                    payload_type = "UserInput",
                    content = %content,
                    content_len = content.len(),
                    "signal converted to UserInputMessage"
                );
                // 用户输入走 UserInputMessage → routing_system 路由，保持确认流程和多轮对话逻辑
                if let Some(channel) = &signal.origin_channel {
                    commands.spawn(UserInputMessage {
                        content: content.clone(),
                        origin_channel: channel.clone(),
                    });
                } else {
                    // 无 origin_channel 的用户输入（理论上不应出现），直接创建任务
                    commands.spawn(CreateTaskMessage {
                        content: content.clone(),
                        origin_channel: None,
                        routing_policy: TaskRoutingPolicy::event(None, None),
                    });
                }
            }
            SignalPayload::RetryWakeup(task_id) => {
                // 立即清除 next_retry_at，防止 retry_wakeup_system 在下一帧再次触发
                // （因为 retry_wakeup_system 和本系统在同一集合中运行，顺序不确定）
                if let Some(task_entity) = index.get_task(task_id)
                    && let Ok(mut task) = tasks.get_mut(task_entity)
                {
                    task.next_retry_at = None;
                }
                debug!(
                    event = "SignalIngested",
                    signal_source = ?signal.source,
                    payload_type = "RetryWakeup",
                    task_id = %task_id,
                    "signal converted to RetryReadyMessage"
                );
                commands.spawn(RetryReadyMessage { task_id: *task_id });
            }
            SignalPayload::SystemWakeup => {
                debug!(
                    event = "SignalIngested",
                    signal_source = ?signal.source,
                    payload_type = "SystemWakeup",
                    "system wakeup signal received"
                );
            }
            SignalPayload::Webhook { kind, .. } => {
                debug!(
                    event = "SignalIngested",
                    signal_source = ?signal.source,
                    payload_type = "Webhook",
                    kind = %kind,
                    "webhook signal converted to TriggerTaskMessage"
                );
                commands.spawn(TriggerTaskMessage {
                    source: signal.source.clone(),
                    trigger: crate::domain::TaskTrigger::Webhook {
                        kind: kind.clone(),
                        body: match &signal.payload {
                            SignalPayload::Webhook { body, .. } => body.clone(),
                            _ => serde_json::Value::Null,
                        },
                    },
                });
            }
            SignalPayload::Timer { kind } => {
                debug!(
                    event = "SignalIngested",
                    signal_source = ?signal.source,
                    payload_type = "Timer",
                    kind = %kind,
                    "timer signal converted to TriggerTaskMessage"
                );
                commands.spawn(TriggerTaskMessage {
                    source: signal.source.clone(),
                    trigger: crate::domain::TaskTrigger::Timer { kind: kind.clone() },
                });
            }
        }

        commands.entity(entity).despawn();
    }
}
