use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, trace};

use crate::domain::{ChannelId, EngineEvent, EventTarget, Frontend, FrontendKind, UserAction};

use super::ChannelOutboundMessage;

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
        let EngineEvent::Text {
            target, content, ..
        } = event
        else {
            return;
        };
        let targets = match target {
            EventTarget::Broadcast => return,
            EventTarget::Directed(v) => v,
        };
        let recipients: Vec<String> = targets
            .iter()
            .filter(|cid| self.matches(cid))
            .map(|cid| cid.user_id.clone())
            .collect();
        if recipients.is_empty() {
            return;
        }
        trace!(
            event = "ChannelFrontendReceive",
            channel = %self.channel_name,
            recipients = recipients.len(),
            content_len = content.len(),
            "routing text to channel outbound queue"
        );
        for recipient in recipients {
            let msg = ChannelOutboundMessage {
                recipient,
                thread_id: None,
                content: content.clone(),
            };
            if let Err(e) = self.outbound_tx.send((self.channel_name.clone(), msg)) {
                error!(event = "ChannelFrontendSendFailed", error = %e, "failed to queue outbound message");
            }
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
        }])));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queues_matching_directed() {
        let (fe, mut rx) = make_frontend(FrontendKind::Telegram);
        fe.push_event(text_event(EventTarget::Directed(vec![ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
        }])));
        let (name, msg) = rx.try_recv().expect("one outbound message");
        assert_eq!(name, "test");
        assert_eq!(msg.recipient, "u1");
        assert_eq!(msg.content, "hello");
        assert!(rx.try_recv().is_err());
    }
}
