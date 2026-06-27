use tokio::sync::mpsc::UnboundedSender;
use tracing::trace;

use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, UserAction};

use super::ChannelOutboundMessage;
use super::traits::{ChannelParseMode, InlineKeyboardButton, ReplyMarkup};

/// 将 EngineEvent 路由到对应 IM 通道出向发送队列的 Frontend 实现。
pub struct ChannelFrontend {
    kind: FrontendKind,
    channel_name: String,
    outbound_tx: UnboundedSender<(String, ChannelOutboundMessage)>,
}

impl ChannelFrontend {
    pub fn new(
        kind: FrontendKind,
        channel_name: impl Into<String>,
        outbound_tx: UnboundedSender<(String, ChannelOutboundMessage)>,
    ) -> Self {
        Self {
            kind,
            channel_name: channel_name.into(),
            outbound_tx,
        }
    }

    fn matches(&self, channel_id: &ChannelId) -> bool {
        channel_id.frontend == self.kind
    }
}

impl Frontend for ChannelFrontend {
    fn kind(&self) -> FrontendKind {
        self.kind.clone()
    }

    fn push_event(&self, event: EngineEvent) {
        match event {
            EngineEvent::Text {
                target, content, ..
            } => {
                let targets = match target {
                    EventTarget::Broadcast => return,
                    EventTarget::Directed(v) => v,
                };
                let recipients: Vec<ChannelId> = targets
                    .into_iter()
                    .filter(|cid| self.matches(cid))
                    .collect();
                if recipients.is_empty() {
                    return;
                }
                trace!(
                    event = "ChannelFrontendReceiveText",
                    channel = %self.channel_name,
                    recipients = recipients.len(),
                    content_len = content.len(),
                );
                for channel_id in recipients {
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: content.clone(),
                        parse_mode: None,
                        reply_markup: None,
                        attachments: vec![],
                    };
                    let _ = self.outbound_tx.send((self.channel_name.clone(), msg));
                }
            }
            EngineEvent::ApprovalRequest {
                target,
                request_id,
                tool_name,
                tool_input,
                options,
                ..
            } => {
                let targets = match target {
                    EventTarget::Broadcast => return,
                    EventTarget::Directed(v) => v,
                };
                let recipients: Vec<ChannelId> = targets
                    .into_iter()
                    .filter(|cid| self.matches(cid))
                    .collect();
                if recipients.is_empty() {
                    return;
                }

                let tool_input_str = serde_json::to_string_pretty(&tool_input)
                    .unwrap_or_else(|_| tool_input.to_string());
                let content = format!(
                    "🔒 需要你的确认\n\n工具：{}\n输入：{}\n\n请选择一个选项：",
                    tool_name, tool_input_str
                );
                let buttons: Vec<Vec<InlineKeyboardButton>> = options
                    .chunks(2)
                    .map(|chunk| {
                        chunk
                            .iter()
                            .map(|opt| InlineKeyboardButton {
                                text: opt.label.clone(),
                                callback_data: format!("{}:{}", request_id, opt.id),
                            })
                            .collect()
                    })
                    .collect();

                for channel_id in recipients {
                    let msg = ChannelOutboundMessage {
                        recipient: channel_id.user_id,
                        thread_id: channel_id.thread_id,
                        content: content.clone(),
                        parse_mode: Some(ChannelParseMode::Html),
                        reply_markup: Some(ReplyMarkup::InlineKeyboard(buttons.clone())),
                        attachments: vec![],
                    };
                    let _ = self.outbound_tx.send((self.channel_name.clone(), msg));
                }
            }
            _ => {}
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn make_frontend(
        kind: FrontendKind,
    ) -> (
        ChannelFrontend,
        mpsc::UnboundedReceiver<(String, ChannelOutboundMessage)>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ChannelFrontend::new(kind, "test", tx), rx)
    }

    fn text_event(target: EventTarget) -> EngineEvent {
        EngineEvent::Text {
            target,
            role: crate::domain::MessageRole::Agent,
            content: "hello".to_string(),
        }
    }

    #[test]
    fn ignores_broadcast() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Broadcast));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ignores_non_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "u1".to_string(),
            thread_id: None,
        }])));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queues_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        }])));
        let (name, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(name, "test");
        assert_eq!(msg.recipient, "u1");
        assert_eq!(msg.content, "hello");
        assert!(rx.try_recv().is_err());
    }
}
