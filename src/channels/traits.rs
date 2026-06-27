use anyhow::Result;
use async_trait::async_trait;
use crossbeam_channel::Sender;

use crate::domain::{ChannelId, FrontendKind};

/// 统一入向消息
#[derive(Debug, Clone)]
pub struct ChannelInboundMessage {
    pub channel_name: String,
    pub sender_id: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub content: String,
    pub timestamp_secs: u64,
}

impl ChannelInboundMessage {
    pub fn to_external_input(&self) -> crate::domain::ExternalInput {
        crate::domain::ExternalInput::TextWithChannel {
            channel: ChannelId {
                frontend: match self.channel_name.as_str() {
                    "telegram" => FrontendKind::Telegram,
                    "qq" => FrontendKind::QQ,
                    "feishu" => FrontendKind::Feishu,
                    _ => panic!("unknown channel name: {}", self.channel_name),
                },
                user_id: self.chat_id.clone(),
            },
            content: self.content.clone(),
        }
    }
}

/// 统一出向消息
#[derive(Debug, Clone)]
pub struct ChannelOutboundMessage {
    pub recipient: String,
    pub thread_id: Option<String>,
    pub content: String,
}

/// 通道错误
#[derive(thiserror::Error, Debug)]
pub enum ChannelError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("api error {code}: {message}")]
    Api { code: i32, message: String },
    #[error("auth failed")]
    Auth,
    #[error("rate limited")]
    RateLimited,
    #[error("not configured")]
    NotConfigured,
}

#[async_trait]
pub trait Channel: Send + Sync + 'static {
    fn name(&self) -> &str;

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError>;

    async fn listen(&self, tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError>;

    async fn health_check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_inbound_to_external_input() {
        let msg = ChannelInboundMessage {
            channel_name: "telegram".to_string(),
            sender_id: "123".to_string(),
            chat_id: "456".to_string(),
            thread_id: None,
            content: "hello".to_string(),
            timestamp_secs: 0,
        };
        let input = msg.to_external_input();
        match input {
            crate::domain::ExternalInput::TextWithChannel { channel, content } => {
                assert_eq!(channel.frontend, FrontendKind::Telegram);
                assert_eq!(channel.user_id, "456");
                assert_eq!(content, "hello");
            }
            _ => panic!("unexpected variant"),
        }
    }
}
