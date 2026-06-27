use bevy::prelude::*;
use tracing::{debug, warn};

use crate::channels::{ChannelManager, ChannelOutboundMessage};
use crate::domain::{
    AgentExecutionOutput, AgentExecutionResult, OutputContent, PendingChannelSend, Task,
    ToolExecutionResultMessage, ToolReturnedHookPending,
};

/// 消费 PendingChannelSend，调用 ChannelManager 发送并回写工具结果。
pub fn channel_send_dispatch_system(
    mut commands: Commands,
    channel_manager: Res<ChannelManager>,
    pending: Query<(Entity, &PendingChannelSend)>,
    tasks: Query<&Task>,
) {
    for (entity, send) in &pending {
        // 未指定目标时，回退到当前任务的来源会话 chat_id。
        let recipient = match &send.recipient {
            Some(r) => r.clone(),
            None => {
                let fallback = tasks
                    .iter()
                    .find(|t| t.id == send.task_id)
                    .map(|t| t.origin_channel.user_id.clone());
                match fallback {
                    Some(id) => id,
                    None => {
                        warn!(
                            event = "ChannelSendNoRecipient",
                            task_id = %send.task_id,
                            "channel_send missing target and no origin channel available"
                        );
                        commands.entity(entity).despawn();
                        commands.entity(send.request_entity).despawn();
                        continue;
                    }
                }
            }
        };

        let thread_id = tasks
            .iter()
            .find(|t| t.id == send.task_id)
            .and_then(|t| t.origin_channel.thread_id.clone());

        let attachment_count = send.attachments.len();
        let result = channel_manager.send(
            send.channel.clone(),
            ChannelOutboundMessage {
                recipient: recipient.clone(),
                thread_id: thread_id.clone(),
                content: send.content.clone(),
                parse_mode: None,
                reply_markup: None,
                attachments: send.attachments.clone(),
            },
        );

        let (output_text, tool_output) = match result {
            Ok(()) => {
                debug!(
                    event = "ChannelSendQueued",
                    task_id = %send.task_id,
                    agent_id = %send.agent_id,
                    channel = %send.channel,
                    recipient = %recipient,
                    thread_id = ?thread_id,
                    content_len = send.content.len(),
                    attachment_count = attachment_count,
                    "channel_send message queued for delivery"
                );
                (
                    format!("channel_send queued: {}", send.channel),
                    serde_json::json!({ "status": "queued", "channel": send.channel }),
                )
            }
            Err(e) => (
                format!("channel_send failed: {e}"),
                serde_json::json!({ "status": "error", "error": e.to_string() }),
            ),
        };

        commands.entity(entity).despawn();
        commands.entity(send.request_entity).despawn();

        commands.spawn((
            ToolExecutionResultMessage {
                result: AgentExecutionResult {
                    task_id: send.task_id,
                    agent_id: send.agent_id,
                    request_kind: crate::domain::AgentRequestKind::ToolExecution {
                        tool_name: "channel_send".to_string(),
                    },
                    result: Ok(AgentExecutionOutput {
                        content: OutputContent::Text(output_text),
                        reasoning_content: None,
                    }),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                },
                tool_name: "channel_send".to_string(),
                tool_output: Ok(tool_output),
                tool_call_id: send.tool_call_id.clone(),
                processed: false,
                original_tool_output: None,
            },
            ToolReturnedHookPending,
        ));
    }
}
