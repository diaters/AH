use bevy::prelude::*;

use crate::channels::{ChannelManager, ChannelOutboundMessage};
use crate::domain::{
    AgentExecutionOutput, AgentExecutionResult, OutputContent, PendingChannelSend,
    ToolExecutionResultMessage, ToolReturnedHookPending,
};

/// 消费 PendingChannelSend，调用 ChannelManager 发送并回写工具结果。
pub fn channel_send_dispatch_system(
    mut commands: Commands,
    channel_manager: Res<ChannelManager>,
    pending: Query<(Entity, &PendingChannelSend)>,
) {
    for (entity, send) in &pending {
        let result = channel_manager.send(
            send.channel.clone(),
            ChannelOutboundMessage {
                recipient: send.recipient.clone(),
                thread_id: None,
                content: send.content.clone(),
            },
        );

        let (output_text, tool_output) = match result {
            Ok(()) => (
                format!("channel_send queued: {}", send.channel),
                serde_json::json!({ "status": "queued", "channel": send.channel }),
            ),
            Err(e) => (
                format!("channel_send failed: {e}"),
                serde_json::json!({ "status": "error", "error": e.to_string() }),
            ),
        };

        commands.entity(entity).despawn();

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
