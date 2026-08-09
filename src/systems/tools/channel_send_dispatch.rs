use crate::prelude::*;
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
        // 未指定目标时，回退到任务的路由通道：优先 routing_policy.output_channel
        // （scheduled 任务由 build_routing_policy 注入），其次 origin_channel。
        let task = tasks.iter().find(|t| t.id == send.task_id);
        let channel = task.and_then(|t| t.delivery_channel());

        let recipient = match &send.recipient {
            Some(r) => r.clone(),
            None => match channel {
                Some(c) => c.user_id.clone(),
                None => {
                    warn!(
                        event = "ChannelSendNoRecipient",
                        task_id = %send.task_id,
                        routing_policy = ?task.map(|t| &t.routing_policy),
                        "channel_send missing target and no routing channel available"
                    );
                    commands.entity(entity).despawn();
                    commands.entity(send.request_entity).despawn();
                    continue;
                }
            },
        };

        let thread_id = channel.and_then(|c| c.thread_id.clone());

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
                message_kind: crate::channels::traits::MessageKind::LLMReply,
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
                conversation: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::traits::{Channel, ChannelError, ChannelInboundMessage};
    use crate::domain::{ChannelId, FrontendKind, Task, TaskRoutingPolicy, TaskStatus};
    use async_trait::async_trait;
    use bevy_ecs::system::RunSystemOnce;
    use chrono::Utc;
    use crossbeam_channel::{Sender, unbounded};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 记录出向消息接收方的占位通道，用于验证 channel_send 派发路径。
    struct RecordingChannel {
        name: String,
        recipients: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &str {
            &self.name
        }

        async fn send(
            &self,
            message: &ChannelOutboundMessage,
        ) -> Result<Option<String>, ChannelError> {
            self.recipients
                .lock()
                .unwrap()
                .push(message.recipient.clone());
            Ok(None)
        }

        async fn listen(&self, _tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError> {
            Err(ChannelError::NotConfigured)
        }
    }

    /// 构造 scheduled 任务：`origin_channel` 为 None，仅 `routing_policy.output_channel` 指向 QQ 群。
    fn scheduled_task(task_id: uuid::Uuid) -> Task {
        let output = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "group:xxx".to_string(),
            thread_id: None,
        };
        let now = Utc::now();
        Task {
            id: task_id,
            content: "scheduled report".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: None,
            routing_policy: TaskRoutingPolicy::scheduled_task(Some(output), "scheduled task"),
            last_evaluated_turn: None,
        }
    }

    #[tokio::test]
    async fn scheduled_task_without_recipient_falls_back_to_routing_channel() {
        let recipients = Arc::new(Mutex::new(Vec::<String>::new()));
        let channel = Arc::new(RecordingChannel {
            name: "qq".to_string(),
            recipients: recipients.clone(),
        }) as Arc<dyn Channel>;
        let (input_tx, _input_rx) = unbounded::<crate::domain::ExternalInput>();
        let (manager, _handle, _frontends) = ChannelManager::new(vec![channel], input_tx);

        let mut world = World::new();
        world.insert_resource(manager);

        let task_id = uuid::Uuid::new_v4();
        world.spawn(scheduled_task(task_id));

        let request_entity = world.spawn_empty().id();
        world.spawn(PendingChannelSend {
            channel: "qq".to_string(),
            recipient: None,
            content: "report".to_string(),
            attachments: vec![],
            tool_call_id: None,
            task_id,
            agent_id: uuid::Uuid::nil(),
            request_entity,
        });

        world.run_system_once(channel_send_dispatch_system).unwrap();

        // ChannelManager::send 仅入队，等待 supervisor 异步投递到 RecordingChannel。
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !recipients.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("outbound message should be delivered");

        assert_eq!(
            recipients.lock().unwrap().first().map(String::as_str),
            Some("group:xxx"),
            "scheduled task without recipient should fall back to routing_policy.output_channel"
        );

        // 清理：从 world 取回 manager 并关闭 supervisor 任务。
        world
            .remove_resource::<ChannelManager>()
            .unwrap()
            .shutdown();
    }
}
