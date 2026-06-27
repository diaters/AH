use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use crossbeam_channel::Sender;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::channels::config::TelegramConfig;

use super::traits::{
    Channel, ChannelError, ChannelInboundMessage, ChannelOutboundMessage, ChannelParseMode,
    InboundConfirmation,
};

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

    async fn post(&self, method: &str, payload: &serde_json::Value) -> Result<(), ChannelError> {
        let url = self.api_url(method);
        let resp = self.client.post(&url).json(payload).send().await?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: 0,
                message: text,
            });
        }
        Ok(())
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
        let text_parts = match message.parse_mode {
            Some(ChannelParseMode::Html) | None => {
                let chunks = split_markdown_semantic(&message.content);
                chunks
                    .into_iter()
                    .map(|chunk| markdown_to_telegram_html(&chunk))
                    .collect::<Vec<_>>()
            }
            Some(ChannelParseMode::Markdown) => {
                split_text(&message.content, TELEGRAM_MAX_TEXT_LENGTH)
            }
        };

        for part in text_parts {
            let mut payload = json!({
                "chat_id": message.recipient,
                "text": part,
                "parse_mode": "HTML",
            });
            if let Some(thread_id) = &message.thread_id
                && let Ok(id) = thread_id.parse::<i64>()
            {
                payload["message_thread_id"] = json!(id);
            }
            if let Some(ref reply_markup) = message.reply_markup {
                payload["reply_markup"] = json!(reply_markup);
            }

            let result = self.post("sendMessage", &payload).await;
            if let Err(ref e) = result {
                if is_parse_mode_error(e) {
                    // Fallback to plain text
                    let mut fallback = json!({
                        "chat_id": message.recipient,
                        "text": strip_tags(&part),
                    });
                    if let Some(thread_id) = &message.thread_id
                        && let Ok(id) = thread_id.parse::<i64>()
                    {
                        fallback["message_thread_id"] = json!(id);
                    }
                    self.post("sendMessage", &fallback).await?;
                } else {
                    result?;
                }
            }
        }

        // Attachments are handled in Task 5.

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
                if let Some(callback_query) = update.callback_query {
                    self.last_update_id
                        .store(update.update_id, Ordering::SeqCst);

                    if !self.is_allowed(&callback_query.from) {
                        warn!(
                            event = "TelegramCallbackUserDenied",
                            user_id = %callback_query.from.id,
                            "callback query from user not in allowed list"
                        );
                        let answer_payload = json!({
                            "callback_query_id": callback_query.id,
                            "text": "无权限",
                        });
                        if let Err(e) = self.post("answerCallbackQuery", &answer_payload).await {
                            warn!(
                                event = "TelegramAnswerCallbackFailed",
                                callback_query_id = %callback_query.id,
                                error = %e,
                                "failed to answer callback query for denied user"
                            );
                        }
                        continue;
                    }

                    if let Some(data) = callback_query.data
                        && let Some((request_id, option)) = parse_callback_data(&data)
                    {
                        // Answer callback query to stop client loading spinner
                        let answer_payload = json!({
                            "callback_query_id": callback_query.id,
                        });
                        if let Err(e) = self.post("answerCallbackQuery", &answer_payload).await {
                            warn!(
                                event = "TelegramAnswerCallbackFailed",
                                callback_query_id = %callback_query.id,
                                error = %e,
                                "failed to answer callback query"
                            );
                        }

                        // Optionally reply with a confirmation note
                        if let Some(ref message) = callback_query.message {
                            let note = format!("已选择：{}", option);
                            let note_payload = json!({
                                "chat_id": message.chat.id,
                                "text": note,
                                "message_thread_id": message.message_thread_id,
                            });
                            if let Err(e) = self.post("sendMessage", &note_payload).await {
                                warn!(
                                    event = "TelegramSendMessageFailed",
                                    callback_query_id = %callback_query.id,
                                    chat_id = %message.chat.id,
                                    error = %e,
                                    "failed to send callback confirmation note"
                                );
                            }
                        }

                        if let Some(ref message) = callback_query.message {
                            let inbound = ChannelInboundMessage {
                                channel_name: self.name().to_string(),
                                sender_id: callback_query.from.id.to_string(),
                                chat_id: message.chat.id.to_string(),
                                thread_id: message.message_thread_id.map(|id| id.to_string()),
                                content: String::new(),
                                timestamp_secs: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0),
                                confirmation: Some(InboundConfirmation { request_id, option }),
                            };
                            let _ = tx.send(inbound);
                        }
                    }
                    continue;
                }

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
                        confirmation: None,
                    });
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
}

const TELEGRAM_MAX_TEXT_LENGTH: usize = 4096;

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn markdown_to_telegram_html(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // fenced code block
        if chars[i..].starts_with(&['`', '`', '`']) {
            let rest: String = chars[i + 3..].iter().collect();
            let (lang, body_start) = if let Some(nl) = rest.find('\n') {
                (rest[..nl].trim(), nl + 1)
            } else {
                ("", 0)
            };
            let body_and_tail = &rest[body_start..];
            if let Some(end) = body_and_tail.find("```") {
                let body_end = body_start + end;
                let body = body_and_tail[..end].trim_end();
                out.push_str("<pre><code");
                if !lang.is_empty() {
                    out.push_str(&format!(" class=\"language-{}\"", escape_html(lang)));
                }
                out.push('>');
                out.push_str(&escape_html(body));
                out.push_str("</code></pre>");
                i += 3 + body_end + 3;
                continue;
            }
        }

        // inline code
        if chars[i] == '`' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '`' {
                j += 1;
            }
            if j < chars.len() {
                let code: String = chars[i + 1..j].iter().collect();
                out.push_str("<code>");
                out.push_str(&escape_html(&code));
                out.push_str("</code>");
                i = j + 1;
                continue;
            }
        }

        // bold ** or __
        if chars[i..].starts_with(&['*', '*']) || chars[i..].starts_with(&['_', '_']) {
            let marker = chars[i];
            if let Some(end) = find_closing_pair(&chars, i + 2, marker, marker) {
                let inner: String = chars[i + 2..end].iter().collect();
                out.push_str("<b>");
                out.push_str(&markdown_to_telegram_html(&inner));
                out.push_str("</b>");
                i = end + 2;
                continue;
            }
        }

        // italic * or _
        if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            if let Some(end) = find_closing_single(&chars, i + 1, marker) {
                let inner: String = chars[i + 1..end].iter().collect();
                out.push_str("<i>");
                out.push_str(&markdown_to_telegram_html(&inner));
                out.push_str("</i>");
                i = end + 1;
                continue;
            }
        }

        // strikethrough ~~
        if chars[i..].starts_with(&['~', '~'])
            && let Some(end) = find_closing_pair(&chars, i + 2, '~', '~')
        {
            let inner: String = chars[i + 2..end].iter().collect();
            out.push_str("<s>");
            out.push_str(&markdown_to_telegram_html(&inner));
            out.push_str("</s>");
            i = end + 2;
            continue;
        }

        // link [text](url)
        if chars[i] == '['
            && let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']')
        {
            let close_bracket = close_bracket + i + 1;
            if close_bracket + 1 < chars.len()
                && chars[close_bracket + 1] == '('
                && let Some(close_paren) = chars[close_bracket + 2..].iter().position(|&c| c == ')')
            {
                let close_paren = close_paren + close_bracket + 2;
                let text: String = chars[i + 1..close_bracket].iter().collect();
                let url: String = chars[close_bracket + 2..close_paren].iter().collect();
                out.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    escape_html(&url),
                    escape_html(&text)
                ));
                i = close_paren + 1;
                continue;
            }
        }

        // headings -> bold
        if i == 0 || chars[i - 1] == '\n' {
            let mut j = i;
            while j < chars.len() && chars[j] == '#' {
                j += 1;
            }
            if j > i && j < chars.len() && chars[j] == ' ' {
                let mut k = j + 1;
                while k < chars.len() && chars[k] != '\n' {
                    k += 1;
                }
                let heading: String = chars[j + 1..k].iter().collect();
                out.push_str("<b>");
                out.push_str(&escape_html(&heading));
                out.push_str("</b>\n");
                i = k + 1;
                continue;
            }
        }

        out.push_str(&escape_html(&chars[i].to_string()));
        i += 1;
    }

    out
}

fn find_closing_pair(chars: &[char], start: usize, a: char, b: char) -> Option<usize> {
    if start + 1 >= chars.len() {
        return None;
    }
    (start..chars.len() - 1).find(|&i| chars[i] == a && chars[i + 1] == b)
}

fn find_closing_single(chars: &[char], start: usize, m: char) -> Option<usize> {
    chars[start..]
        .iter()
        .position(|&c| c == m)
        .map(|p| p + start)
}

fn split_markdown_semantic(text: &str) -> Vec<String> {
    // Split by double newline (paragraphs) and fenced code blocks.
    // Preserve code blocks as atomic units.
    let mut chunks = vec![];
    let mut current = String::new();
    let mut in_code = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code {
                // Closing fence: finish the code block as one atomic chunk.
                current.push_str(line);
                current.push('\n');
                chunks.push(current.trim_end().to_string());
                current.clear();
                in_code = false;
            } else {
                // Opening fence: flush any preceding text first.
                if !current.trim().is_empty() {
                    chunks.push(current.trim_end().to_string());
                    current.clear();
                }
                in_code = true;
                current.push_str(line);
                current.push('\n');
            }
        } else {
            current.push_str(line);
            current.push('\n');
            if !in_code && line.trim().is_empty() {
                if current.trim().is_empty() {
                    current.clear();
                } else {
                    chunks.push(current.trim_end().to_string());
                    current.clear();
                }
            }
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim_end().to_string());
    }
    chunks
}

fn is_parse_mode_error(err: &ChannelError) -> bool {
    match err {
        ChannelError::Api { message, .. } => {
            message.to_lowercase().contains("parse") || message.to_lowercase().contains("can't")
        }
        _ => false,
    }
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
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

fn parse_callback_data(data: &str) -> Option<(Uuid, String)> {
    let (uuid_part, option_part) = data.split_once(':')?;
    if option_part.is_empty() {
        return None;
    }
    let request_id = Uuid::parse_str(uuid_part).ok()?;
    Some((request_id, option_part.to_string()))
}

#[derive(Debug, Deserialize)]
struct TelegramGetUpdatesResponse {
    result: Vec<TelegramUpdate>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
    callback_query: Option<TelegramCallbackQuery>,
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
struct TelegramCallbackQuery {
    id: String,
    from: TelegramUser,
    message: Option<TelegramCallbackMessage>,
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TelegramCallbackMessage {
    message_id: i64,
    chat: TelegramChat,
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

    #[test]
    fn parse_callback_query_data() {
        let data = "01912345-6789-7abc-8def-0123456789ab:allow_once";
        let (request_id, option) = parse_callback_data(data).unwrap();
        assert_eq!(
            request_id.to_string(),
            "01912345-6789-7abc-8def-0123456789ab"
        );
        assert_eq!(option, "allow_once");
    }

    #[test]
    fn parse_callback_query_data_rejects_invalid_uuid() {
        assert!(parse_callback_data("not-a-uuid:allow_once").is_none());
    }

    #[test]
    fn parse_callback_query_data_rejects_missing_separator() {
        assert!(parse_callback_data("01912345-6789-7abc-8def-0123456789ab").is_none());
    }

    #[test]
    fn parse_callback_query_data_rejects_empty_option() {
        assert!(parse_callback_data("01912345-6789-7abc-8def-0123456789ab:").is_none());
    }

    #[test]
    fn callback_user_allowlist_denied() {
        let ch = TelegramChannel::new(cfg(vec!["alice".to_string()]));
        let user = TelegramUser {
            id: 999,
            username: Some("mallory".to_string()),
        };
        assert!(!ch.is_allowed(&user));
    }

    #[test]
    fn markdown_bold_to_telegram_html() {
        let input = "**hello**";
        assert_eq!(markdown_to_telegram_html(input), "<b>hello</b>");
    }

    #[test]
    fn markdown_bold_with_underscores() {
        assert_eq!(markdown_to_telegram_html("__hello__"), "<b>hello</b>");
    }

    #[test]
    fn markdown_italic_with_asterisk() {
        assert_eq!(markdown_to_telegram_html("*hello*"), "<i>hello</i>");
    }

    #[test]
    fn markdown_italic_with_underscore() {
        assert_eq!(markdown_to_telegram_html("_hello_"), "<i>hello</i>");
    }

    #[test]
    fn markdown_strikethrough() {
        assert_eq!(markdown_to_telegram_html("~~hello~~"), "<s>hello</s>");
    }

    #[test]
    fn markdown_inline_code() {
        assert_eq!(markdown_to_telegram_html("`code`"), "<code>code</code>");
    }

    #[test]
    fn markdown_inline_code_escapes_html() {
        assert_eq!(
            markdown_to_telegram_html("`<b>alert</b>`"),
            "<code>&lt;b&gt;alert&lt;/b&gt;</code>"
        );
    }

    #[test]
    fn markdown_fenced_code_block() {
        let input = "```rust\nfn main() {}\n```";
        assert_eq!(
            markdown_to_telegram_html(input),
            "<pre><code class=\"language-rust\">fn main() {}</code></pre>"
        );
    }

    #[test]
    fn markdown_fenced_code_block_without_language() {
        let input = "```\nplain\n```";
        assert_eq!(
            markdown_to_telegram_html(input),
            "<pre><code>plain</code></pre>"
        );
    }

    #[test]
    fn markdown_link() {
        assert_eq!(
            markdown_to_telegram_html("[text](https://example.com)"),
            "<a href=\"https://example.com\">text</a>"
        );
    }

    #[test]
    fn markdown_heading_to_bold() {
        assert_eq!(markdown_to_telegram_html("# Title"), "<b>Title</b>\n");
    }

    #[test]
    fn markdown_heading_multilevel_to_bold() {
        assert_eq!(markdown_to_telegram_html("### Title"), "<b>Title</b>\n");
    }

    #[test]
    fn markdown_plain_text_escaped() {
        assert_eq!(
            markdown_to_telegram_html("1 < 2 and 2 > 1"),
            "1 &lt; 2 and 2 &gt; 1"
        );
    }

    #[test]
    fn escape_html_escapes_special_chars() {
        assert_eq!(escape_html("&<>"), "&amp;&lt;&gt;");
    }

    #[test]
    fn split_markdown_semantic_preserves_code_blocks() {
        let input = "para1\n\n```\ncode\n```\n\npara2";
        let chunks = split_markdown_semantic(input);
        assert_eq!(chunks, vec!["para1", "```\ncode\n```", "para2"]);
    }

    #[test]
    fn split_markdown_semantic_splits_paragraphs() {
        let input = "first\n\nsecond";
        assert_eq!(split_markdown_semantic(input), vec!["first", "second"]);
    }

    #[test]
    fn strip_tags_removes_html_tags() {
        assert_eq!(strip_tags("<b>hello</b>"), "hello");
    }

    #[test]
    fn is_parse_mode_error_detects_parse_error() {
        let err = ChannelError::Api {
            code: 400,
            message: "can't parse message text".to_string(),
        };
        assert!(is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_detects_parse_keyword() {
        let err = ChannelError::Api {
            code: 400,
            message: "Parse error in HTML".to_string(),
        };
        assert!(is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_false_for_network() {
        let err = ChannelError::NotConfigured;
        assert!(!is_parse_mode_error(&err));
    }
}
