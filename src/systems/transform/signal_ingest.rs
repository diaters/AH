//! 信号转换 System
//!
//! 将 Signal 转换为对应的 Message。

use crate::prelude::*;
use tracing::debug;

use crate::domain::{RetryReadyMessage, Signal, SignalPayload, UserInputMessage};

/// 信号转换 System
///
/// 将 Signal 转换为对应的 Message。
pub fn signal_ingest_system(mut commands: Commands, signals: Query<(Entity, &Signal)>) {
    for (entity, signal) in &signals {
        match &signal.payload {
            SignalPayload::UserInput(content) => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "UserInput",
                    content = %content,
                    content_len = content.len(),
                    "signal converted to UserInputMessage"
                );
                commands.spawn(UserInputMessage {
                    content: content.clone(),
                    origin_channel: signal.origin_channel.clone(),
                });
            }
            SignalPayload::RetryWakeup(task_id) => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "RetryWakeup",
                    task_id = %task_id,
                    "signal converted to RetryReadyMessage"
                );
                commands.spawn(RetryReadyMessage { task_id: *task_id });
            }
            SignalPayload::SystemWakeup => {
                debug!(
                    event = "SignalIngested",
                    signal_type = ?signal.kind,
                    payload_type = "SystemWakeup",
                    "system wakeup signal received"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}
