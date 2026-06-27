use anyhow::Result;
use async_trait::async_trait;
use crossbeam_channel::Sender;
use serde::Serialize;
use uuid::Uuid;

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
    pub confirmation: Option<InboundConfirmation>,
}

#[derive(Clone, Debug)]
pub struct InboundConfirmation {
    pub request_id: Uuid,
    pub option: String,
}

impl ChannelInboundMessage {
    pub fn to_external_input(&self) -> crate::domain::ExternalInput {
        if let Some(ref confirmation) = self.confirmation {
            return crate::domain::ExternalInput::Confirmation {
                request_id: confirmation.request_id,
                option: confirmation.option.clone(),
            };
        }

        crate::domain::ExternalInput::TextWithChannel {
            channel: ChannelId {
                frontend: match self.channel_name.as_str() {
                    "telegram" => FrontendKind::Telegram,
                    "qq" => FrontendKind::QQ,
                    "feishu" => FrontendKind::Feishu,
                    _ => panic!("unknown channel name: {}", self.channel_name),
                },
                user_id: self.chat_id.clone(),
                thread_id: self.thread_id.clone(),
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
    pub parse_mode: Option<ChannelParseMode>,
    pub reply_markup: Option<ReplyMarkup>,
    pub attachments: Vec<ChannelAttachment>,
}

#[derive(Clone, Debug, Serialize)]
pub enum ChannelParseMode {
    Html,
    Markdown,
}

#[derive(Clone, Debug, Serialize)]
pub enum ReplyMarkup {
    #[serde(rename = "inline_keyboard")]
    InlineKeyboard(Vec<Vec<InlineKeyboardButton>>),
}

#[derive(Clone, Debug, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    pub callback_data: String,
}

#[derive(Clone, Debug)]
pub struct ChannelAttachment {
    pub kind: AttachmentKind,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
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

    fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> {
        vec![]
    }

    fn supports_html(&self) -> bool {
        false
    }

    fn supports_inline_keyboard(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_with_thread_id_not_equal_to_without() {
        let a = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let b = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: Some("t1".to_string()),
        };
        assert_ne!(a, b);
    }

    #[test]
    fn channel_inbound_to_external_input() {
        let msg = ChannelInboundMessage {
            channel_name: "telegram".to_string(),
            sender_id: "123".to_string(),
            chat_id: "456".to_string(),
            thread_id: None,
            content: "hello".to_string(),
            timestamp_secs: 0,
            confirmation: None,
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

    #[test]
    fn reply_markup_serializes_to_telegram_inline_keyboard() {
        let markup = ReplyMarkup::InlineKeyboard(vec![vec![InlineKeyboardButton {
            text: "允许".to_string(),
            callback_data: "req-id:allow".to_string(),
        }]]);
        let json = serde_json::to_value(&markup).expect("serialize");
        assert!(
            json.get("inline_keyboard").is_some(),
            "expected key 'inline_keyboard', got {}",
            json
        );
        assert!(
            json.get("InlineKeyboard").is_none(),
            "must not use Rust variant name as key"
        );
    }
}
