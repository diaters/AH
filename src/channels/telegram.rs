use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use crossbeam_channel::Sender;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use crate::channels::config::TelegramConfig;

use super::traits::{Channel, ChannelError, ChannelInboundMessage, ChannelOutboundMessage};

pub struct TelegramChannel {
    config: TelegramConfig,
    client: Client,
    base_url: String,
    last_update_id: AtomicI64,
}

impl TelegramChannel {
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            config,
            client: Client::new(),
            base_url: "https://api.telegram.org".to_string(),
            last_update_id: AtomicI64::new(0),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.config.bot_token, method)
    }

    /// 白名单匹配：username（忽略大小写）、user_id，或通配符 `"*"`。
    /// 空白名单表示拒绝所有用户（必须显式配置才放行）。
    /// 若列表中包含 `"*"`，则允许所有用户。
    fn is_allowed(&self, user: &TelegramUser) -> bool {
        if self
            .config
            .allowed_users
            .iter()
            .any(|allowed| allowed == "*")
        {
            return true;
        }
        if self.config.allowed_users.is_empty() {
            return false;
        }
        self.config.allowed_users.iter().any(|allowed| {
            if let Some(username) = &user.username
                && username.eq_ignore_ascii_case(allowed)
            {
                return true;
            }
            if let Ok(id) = allowed.parse::<i64>()
                && user.id == id
            {
                return true;
            }
            false
        })
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<(), ChannelError> {
        for chunk in split_text(&message.content, 4096) {
            let url = self.api_url("sendMessage");
            let mut payload = json!({
                "chat_id": message.recipient,
                "text": chunk,
            });
            if let Some(thread_id) = &message.thread_id
                && let Ok(id) = thread_id.parse::<i64>()
            {
                payload["message_thread_id"] = json!(id);
            }
            let resp = self.client.post(&url).json(&payload).send().await?;
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ChannelError::Api {
                    code: 0,
                    message: text,
                });
            }
        }
        Ok(())
    }

    async fn listen(&self, tx: Sender<ChannelInboundMessage>) -> Result<(), ChannelError> {
        loop {
            let url = self.api_url("getUpdates");
            let offset = self.last_update_id.load(Ordering::SeqCst) + 1;
            let resp = self
                .client
                .get(&url)
                .query(&[("offset", offset.to_string()), ("limit", "100".to_string())])
                .send()
                .await?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ChannelError::Api {
                    code: 0,
                    message: text,
                });
            }

            let data: TelegramGetUpdatesResponse = resp.json().await?;
            for update in data.result {
                if let Some(msg) = update.message {
                    self.last_update_id
                        .store(update.update_id, Ordering::SeqCst);

                    if !self.is_allowed(&msg.from) {
                        warn!(
                            event = "TelegramUserDenied",
                            user_id = %msg.from.id,
                            "user not in allowed list"
                        );
                        continue;
                    }

                    let _ = tx.send(ChannelInboundMessage {
                        channel_name: self.name().to_string(),
                        sender_id: msg.from.id.to_string(),
                        chat_id: msg.chat.id.to_string(),
                        thread_id: msg.message_thread_id.map(|id| id.to_string()),
                        content: msg.text.unwrap_or_default(),
                        timestamp_secs: msg.date as u64,
                    });
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
}

fn split_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        let end = text.floor_char_boundary(end);
        chunks.push(text[start..end].to_string());
        start = end;
    }
    chunks
}

#[derive(Debug, Deserialize)]
struct TelegramGetUpdatesResponse {
    result: Vec<TelegramUpdate>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    from: TelegramUser,
    chat: TelegramChat,
    date: i64,
    text: Option<String>,
    message_thread_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(users: Vec<String>) -> TelegramConfig {
        TelegramConfig {
            bot_token: "x".to_string(),
            allowed_users: users,
        }
    }

    #[test]
    fn allowed_user_by_username() {
        let ch = TelegramChannel::new(cfg(vec!["alice".to_string()]));
        let user = TelegramUser {
            id: 1,
            username: Some("Alice".to_string()),
        };
        assert!(ch.is_allowed(&user));
    }

    #[test]
    fn allowed_user_by_id() {
        let ch = TelegramChannel::new(cfg(vec!["123".to_string()]));
        let user = TelegramUser {
            id: 123,
            username: None,
        };
        assert!(ch.is_allowed(&user));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = TelegramChannel::new(cfg(vec![]));
        let user = TelegramUser {
            id: 1,
            username: Some("anyone".to_string()),
        };
        assert!(!ch.is_allowed(&user));
    }

    #[test]
    fn wildcard_allows_all() {
        let ch = TelegramChannel::new(cfg(vec!["*".to_string()]));
        let user = TelegramUser {
            id: 1,
            username: Some("anyone".to_string()),
        };
        assert!(ch.is_allowed(&user));
    }

    #[test]
    fn split_text_respects_char_boundary() {
        let s = "a".repeat(4097);
        let chunks = split_text(&s, 4096);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4096);
    }
}
