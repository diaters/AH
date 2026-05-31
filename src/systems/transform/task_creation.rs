//! 任务创建 System
//!
//! 从消息创建任务实体。

use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::HarnessSettings,
    domain::{
        ChannelId, CreateTaskMessage, EntryMetadata, EntryRole, FrontendKind, ShortTermMemory, Task,
    },
};

/// 用户消息转任务 System
///
/// 将用户消息转换为任务实体。
pub fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    messages: Query<(Entity, &CreateTaskMessage)>,
) {
    for (entity, message) in &messages {
        // 创建多轮对话任务（Pending 状态）并附带 ShortTermMemory
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, &message.content, EntryMetadata::default());
        let stm_tokens = stm.estimated_tokens;

        let task = Task::from_user_input(
            message.content.clone(),
            settings.0.max_retries,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
            },
        );
        debug!(
            event = "TaskCreated",
            task_id = %task.id,
            content = %message.content,
            content_len = message.content.len(),
            multi_turn = task.multi_turn,
            max_retries = task.max_retries,
            stm_initial_entries = 1,
            stm_initial_tokens = stm_tokens,
            "new task spawned from user message"
        );

        commands.spawn((task, stm));
        commands.entity(entity).despawn();
    }
}
