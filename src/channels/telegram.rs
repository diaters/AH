use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use crossbeam_channel::Sender;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::channels::config::TelegramConfig;

use super::traits::{
    AttachmentKind, Channel, ChannelAttachment, ChannelError, ChannelInboundMessage,
    ChannelOutboundMessage, ChannelParseMode, InboundConfirmation, ReplyMarkup,
    extract_attachments,
};

/// Telegram 用户待处理反馈记录：用户点击 "reject_with_feedback" 后
/// 等待用户发送文本反馈。key 为 user_id。
#[derive(Debug, Clone)]
struct PendingFeedback {
    request_id: Uuid,
    chat_id: String,
    thread_id: Option<String>,
}

pub struct TelegramChannel {
    config: TelegramConfig,
    config_path: Option<PathBuf>,
    runtime_allowed_users: Arc<RwLock<HashSet<String>>>,
    client: Client,
    base_url: String,
    last_update_id: AtomicI64,
    /// 待处理反馈：用户点击 reject_with_feedback 后等待文本输入
    pending_feedback: Arc<RwLock<HashMap<String, PendingFeedback>>>,
}

impl TelegramChannel {
    pub fn new(config: TelegramConfig) -> Self {
        Self::new_with_path(config, None)
    }

    pub fn new_with_path(config: TelegramConfig, config_path: Option<PathBuf>) -> Self {
        Self {
            config,
            config_path,
            runtime_allowed_users: Arc::new(RwLock::new(HashSet::new())),
            client: Client::new(),
            base_url: "https://api.telegram.org".to_string(),
            last_update_id: AtomicI64::new(0),
            pending_feedback: Arc::new(RwLock::new(HashMap::new())),
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
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        Ok(())
    }

    /// 发送 Telegram API 请求并解析响应 JSON。
    async fn post_json(
        &self,
        method: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, ChannelError> {
        let url = self.api_url(method);
        let resp = self.client.post(&url).json(payload).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        let body: serde_json::Value = resp.json().await?;
        Ok(body)
    }

    /// 白名单匹配：运行时白名单优先，然后按配置匹配 username（忽略大小写）、
    /// user_id，或通配符 `"*"`。空白名单表示拒绝所有用户（必须显式配置才放行）。
    /// 若列表中包含 `"*"`，则允许所有用户。
    fn is_allowed(&self, user: &TelegramUser) -> bool {
        // Runtime allowlist from /bind takes precedence
        if self
            .runtime_allowed_users
            .read()
            .unwrap()
            .contains(&user.id.to_string())
        {
            return true;
        }

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

    fn runtime_allow(&self, user_id: &str) {
        self.runtime_allowed_users
            .write()
            .unwrap()
            .insert(user_id.to_string());
    }

    fn expected_pairing_code(&self) -> String {
        self.config.pairing_code.clone().unwrap_or_default()
    }

    async fn is_writable_toml(path: &Path) -> bool {
        path.extension().map(|e| e == "toml").unwrap_or(false)
            && tokio::fs::metadata(path)
                .await
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false)
    }

    async fn persist_allowed_user(&self, user_id: &str, path: &Path) -> Result<(), ChannelError> {
        use crate::channels::config::ChannelConfigs;
        let mut configs: ChannelConfigs = tokio::fs::read_to_string(path)
            .await
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        let tg = configs.telegram.get_or_insert_with(|| self.config.clone());
        if !tg.allowed_users.iter().any(|u| u == user_id) {
            tg.allowed_users.push(user_id.to_string());
        }
        let content = toml::to_string_pretty(&configs).map_err(|e| ChannelError::Api {
            code: 0,
            message: e.to_string(),
        })?;
        tokio::fs::write(path, content)
            .await
            .map_err(|e| ChannelError::Api {
                code: 0,
                message: e.to_string(),
            })?;

        Ok(())
    }

    fn supported_attachment_kinds(&self) -> Vec<AttachmentKind> {
        vec![
            AttachmentKind::Image,
            AttachmentKind::Document,
            AttachmentKind::Video,
            AttachmentKind::Audio,
            AttachmentKind::Voice,
        ]
    }

    async fn send_attachment(
        &self,
        base: &ChannelOutboundMessage,
        attachment: &ChannelAttachment,
    ) -> Result<(), ChannelError> {
        debug!(
            event = "TelegramSendAttachment",
            chat_id = %base.recipient,
            thread_id = ?base.thread_id,
            kind = ?attachment.kind,
            target = %attachment.target,
            "telegram channel sending attachment"
        );

        if !self.supported_attachment_kinds().contains(&attachment.kind) {
            // Unsupported by this channel, send as text fallback
            let fallback = json!({
                "chat_id": base.recipient,
                "text": format!("Unsupported attachment: {}", attachment.target),
                "message_thread_id": base.thread_id.as_ref().and_then(|t| t.parse::<i64>().ok()),
            });
            if let Err(e) = self.post("sendMessage", &fallback).await {
                warn!(
                    event = "TelegramUnsupportedAttachmentFallbackFailed",
                    chat_id = %base.recipient,
                    target = %attachment.target,
                    error = %e,
                    "failed to send unsupported attachment fallback"
                );
            }
            return Ok(());
        }

        let (method, file_field) = match attachment.kind {
            AttachmentKind::Image => ("sendPhoto", "photo"),
            AttachmentKind::Document => ("sendDocument", "document"),
            AttachmentKind::Video => ("sendVideo", "video"),
            AttachmentKind::Audio => ("sendAudio", "audio"),
            AttachmentKind::Voice => ("sendVoice", "voice"),
        };

        if attachment.target.starts_with("http://") || attachment.target.starts_with("https://") {
            let mut payload = json!({
                "chat_id": base.recipient,
                (file_field): &attachment.target,
                "caption": base.content,
            });
            if let Some(thread_id) = &base.thread_id
                && let Ok(id) = thread_id.parse::<i64>()
            {
                payload["message_thread_id"] = json!(id);
            }
            self.post(method, &payload).await?;
        } else {
            let target = resolve_attachment_path(&attachment.target);
            self.post_multipart(
                method,
                &base.recipient,
                base.thread_id.as_deref(),
                file_field,
                &target,
                &base.content,
            )
            .await?;
        }
        Ok(())
    }

    async fn post_multipart(
        &self,
        method: &str,
        chat_id: &str,
        thread_id: Option<&str>,
        file_field: &str,
        file_path: &Path,
        caption: &str,
    ) -> Result<(), ChannelError> {
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| ChannelError::Api {
                code: 0,
                message: e.to_string(),
            })?;
        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(
            file_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string()),
        );
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part(file_field.to_string(), part);
        if let Some(thread_id) = thread_id
            && let Ok(id) = thread_id.parse::<i64>()
        {
            form = form.text("message_thread_id", id.to_string());
        }
        if !caption.is_empty() {
            form = form.text("caption", caption.to_string());
        }

        let url = self.api_url(method);
        let resp = self.client.post(&url).multipart(form).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        Ok(())
    }

    async fn send_ack_reaction(&self, chat_id: i64, message_id: i64) -> Result<(), ChannelError> {
        // Only emoji from Telegram's setMessageReaction allow-list are valid.
        // "✅" is not supported and causes REACTION_INVALID.
        let reactions = ["👍", "👌", "🎉", "🆗"];
        let idx: usize = message_id.try_into().unwrap_or(0);
        let reaction = reactions[idx % reactions.len()];
        let payload = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": [{"type": "emoji", "emoji": reaction}],
            "is_big": false,
        });
        self.post("setMessageReaction", &payload).await?;
        Ok(())
    }

    async fn extract_incoming_attachment(
        &self,
        msg: &TelegramMessage,
    ) -> Option<IncomingAttachment> {
        let dir = std::env::current_dir().ok()?.join("telegram_files");
        tokio::fs::create_dir_all(&dir).await.ok()?;

        if let Some(doc) = &msg.document {
            return self
                .download_telegram_file(
                    &doc.file_id,
                    &dir,
                    doc.file_name.as_deref(),
                    AttachmentKind::Document,
                )
                .await;
        }

        if let Some(photo) = msg.photo.last() {
            return self
                .download_telegram_file(&photo.file_id, &dir, None, AttachmentKind::Image)
                .await;
        }

        if let Some(voice) = &msg.voice {
            return self
                .download_telegram_file(&voice.file_id, &dir, None, AttachmentKind::Voice)
                .await;
        }

        None
    }

    async fn download_telegram_file(
        &self,
        file_id: &str,
        dir: &Path,
        file_name: Option<&str>,
        kind: AttachmentKind,
    ) -> Option<IncomingAttachment> {
        let get_file_payload = json!({ "file_id": file_id });
        let resp = match self
            .client
            .post(self.api_url("getFile"))
            .json(&get_file_payload)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    event = "TelegramGetFileFailed",
                    file_id = %file_id,
                    operation = "getFile",
                    error = %e,
                    "failed to request getFile"
                );
                return None;
            }
        };
        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    event = "TelegramGetFileParseFailed",
                    file_id = %file_id,
                    operation = "getFile",
                    error = %e,
                    "failed to parse getFile response"
                );
                return None;
            }
        };
        let file_path = match data["result"]["file_path"].as_str() {
            Some(p) => p,
            None => {
                warn!(
                    event = "TelegramFilePathMissing",
                    file_id = %file_id,
                    operation = "getFile",
                    response = %data,
                    "getFile response missing file_path"
                );
                return None;
            }
        };

        let download_url = format!(
            "{}/file/bot{}/{}",
            self.base_url, self.config.bot_token, file_path
        );
        let bytes = match self.client.get(&download_url).send().await {
            Ok(r) => match r.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        event = "TelegramDownloadReadFailed",
                        file_id = %file_id,
                        operation = "download",
                        error = %e,
                        "failed to read downloaded file bytes"
                    );
                    return None;
                }
            },
            Err(e) => {
                warn!(
                    event = "TelegramDownloadFailed",
                    file_id = %file_id,
                    operation = "download",
                    error = %e,
                    "failed to download file"
                );
                return None;
            }
        };

        if bytes.len() > 20 * 1024 * 1024 {
            warn!(event = "TelegramFileTooLarge", file_id = %file_id, "incoming file exceeds 20MB limit");
            return None;
        }

        let local_name = file_name
            .map(sanitize_telegram_filename)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let safe =
                    sanitize_telegram_filename(file_path.rsplit('/').next().unwrap_or("file"));
                if safe.is_empty() || safe == "file" {
                    file_id.to_string()
                } else {
                    format!("{}_{}", file_id, safe)
                }
            });
        let local_path = dir.join(&local_name);
        let mut file = match File::create(&local_path).await {
            Ok(f) => f,
            Err(e) => {
                error!(
                    event = "TelegramFileCreateFailed",
                    file_id = %file_id,
                    operation = "save",
                    path = %local_path.display(),
                    error = %e,
                    "failed to create local file"
                );
                return None;
            }
        };
        if let Err(e) = file.write_all(&bytes).await {
            error!(
                event = "TelegramFileWriteFailed",
                file_id = %file_id,
                operation = "save",
                path = %local_path.display(),
                error = %e,
                "failed to write downloaded file"
            );
            return None;
        }

        Some(IncomingAttachment {
            kind,
            path: local_path,
            name: file_name.map(|s| s.to_string()),
        })
    }
}

#[derive(Debug)]
struct IncomingAttachment {
    kind: AttachmentKind,
    path: PathBuf,
    #[allow(dead_code)]
    name: Option<String>,
}

impl IncomingAttachment {
    fn to_agent_text(&self) -> String {
        let path = self.path.display().to_string();
        match self.kind {
            AttachmentKind::Image => format!("[IMAGE:{}]", path),
            AttachmentKind::Document => format!("[DOCUMENT:{}]", path),
            AttachmentKind::Voice => format!("[VOICE:{}]", path),
            _ => format!("[DOCUMENT:{}]", path),
        }
    }
}

fn resolve_attachment_path(target: &str) -> PathBuf {
    let path = target.strip_prefix("file://").unwrap_or(target);
    let relative = PathBuf::from(path);
    if relative.exists() {
        return relative.canonicalize().unwrap_or(relative);
    }
    PathBuf::from(path)
}

/// 清理 Telegram 入向文件名，避免路径遍历。
/// 仅保留最后一个路径分量，移除路径分隔符、空字符和 `..`，
/// 若结果为空则回退到 `"file"`。
fn sanitize_telegram_filename(name: &str) -> String {
    let base = name
        .replace('\\', "/")
        .split('/')
        .rfind(|s| !s.is_empty() && *s != "..")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".to_string());
    // 进一步去除 NUL 与常见危险字符。
    base.chars()
        .filter(|c| *c != '\0' && !matches!(*c, ':' | '?' | '*' | '"' | '<' | '>' | '|'))
        .collect()
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn send(&self, message: &ChannelOutboundMessage) -> Result<Option<String>, ChannelError> {
        use super::traits::MessageKind;

        // 撤回指令：content 字段为目标 msg_id
        if message.message_kind == MessageKind::Recall {
            if let Err(e) = self.recall_message(&message.recipient, &message.content).await {
                tracing::warn!(
                    event = "ChannelRecallFailed",
                    channel = "telegram",
                    recipient = %message.recipient,
                    msg_id = %message.content,
                    error = %e,
                    "recall failed, falling back to leaving old message"
                );
            }
            return Ok(None);
        }

        // 审批请求消息只展示文本与内联键盘，不应解析或发送附件标记。
        let (text_without_markers, all_attachments) = if message.reply_markup.is_some() {
            (message.content.clone(), vec![])
        } else {
            let (text, inline_attachments) = extract_attachments(&message.content);
            let attachments: Vec<_> = message
                .attachments
                .iter()
                .cloned()
                .chain(inline_attachments)
                .filter(|a| !a.target.trim().is_empty())
                .collect();
            (text, attachments)
        };

        let text_parts = prepare_text_parts(&text_without_markers, message.parse_mode.as_ref());

        debug!(
            event = "TelegramSendStart",
            chat_id = %message.recipient,
            thread_id = ?message.thread_id,
            text_part_count = text_parts.len(),
            attachment_count = all_attachments.len(),
            has_reply_markup = message.reply_markup.is_some(),
            parse_mode = ?message.parse_mode,
            "telegram channel preparing outbound message"
        );

        let mut last_msg_id = None;
        let part_count = text_parts.len();
        for (idx, part) in text_parts.into_iter().enumerate() {
            // reply_markup 只应附加到最后一条消息，避免一条长内容产生多个可交互仪表盘。
            let reply_markup = if idx + 1 == part_count {
                message.reply_markup.as_ref()
            } else {
                None
            };

            let payload = build_send_payload(
                &message.recipient,
                message.thread_id.as_deref(),
                &part,
                message.parse_mode.as_ref(),
                reply_markup,
            );

            let result = self.post_json("sendMessage", &payload).await;
            match result {
                Ok(body) => {
                    last_msg_id = body
                        .get("result")
                        .and_then(|r| r.get("message_id"))
                        .and_then(serde_json::Value::as_i64)
                        .map(|id| id.to_string());
                }
                Err(ref e) if is_parse_mode_error(e) => {
                    let fallback = build_fallback_payload(
                        &message.recipient,
                        message.thread_id.as_deref(),
                        &part,
                        reply_markup,
                    );
                    let body = self.post_json("sendMessage", &fallback).await?;
                    last_msg_id = body
                        .get("result")
                        .and_then(|r| r.get("message_id"))
                        .and_then(serde_json::Value::as_i64)
                        .map(|id| id.to_string());
                }
                Err(e) => return Err(e),
            }
        }

        // New in Task 5: send explicit and inline attachments
        for attachment in &all_attachments {
            self.send_attachment(message, attachment).await?;
        }

        Ok(last_msg_id)
    }

    async fn recall_message(&self, recipient: &str, msg_id: &str) -> Result<(), ChannelError> {
        let payload = json!({
            "chat_id": recipient,
            "message_id": msg_id.parse::<i64>().unwrap_or(0),
        });
        self.post("deleteMessage", &payload).await?;
        Ok(())
    }

    async fn send_typing(&self, recipient: &str) -> Result<(), ChannelError> {
        let payload = json!({
            "chat_id": recipient,
            "action": "typing",
        });
        self.post("sendChatAction", &payload).await?;
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

            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ChannelError::Api {
                    code: status.as_u16() as i32,
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
                        if let Some(ref message) = callback_query.message {
                            let note_payload = json!({
                                "chat_id": message.chat.id,
                                "text": "你没有权限操作此审批请求。",
                                "message_thread_id": message.message_thread_id,
                            });
                            if let Err(e) = self.post("sendMessage", &note_payload).await {
                                warn!(
                                    event = "TelegramDeniedCallbackNotifyFailed",
                                    callback_query_id = %callback_query.id,
                                    chat_id = %message.chat.id,
                                    error = %e,
                                    "failed to notify denied callback user"
                                );
                            }
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

                        // Remove inline keyboard from the original message to prevent
                        // accidental duplicate confirmations.
                        if let Some(ref message) = callback_query.message {
                            let remove_keyboard_payload = json!({
                                "chat_id": message.chat.id,
                                "message_id": message.message_id,
                                "reply_markup": {
                                    "inline_keyboard": []
                                },
                            });
                            if let Err(e) = self
                                .post("editMessageReplyMarkup", &remove_keyboard_payload)
                                .await
                            {
                                warn!(
                                    event = "TelegramRemoveKeyboardFailed",
                                    callback_query_id = %callback_query.id,
                                    chat_id = %message.chat.id,
                                    message_id = %message.message_id,
                                    error = %e,
                                    "failed to remove inline keyboard after confirmation"
                                );
                            }
                        }

                        // Optionally reply with a confirmation note
                        if let Some(ref message) = callback_query.message {
                            let note = if option == "reject_with_feedback" {
                                // 拒绝并反馈：不立即发送 Confirmation，而是记录到 pending_feedback
                                // 等待用户发送文本反馈
                                self.pending_feedback.write().unwrap().insert(
                                    callback_query.from.id.to_string(),
                                    PendingFeedback {
                                        request_id,
                                        chat_id: message.chat.id.to_string(),
                                        thread_id: message
                                            .message_thread_id
                                            .map(|id| id.to_string()),
                                    },
                                );
                                "请输入评审建议（发送 /cancel 取消）：".to_string()
                            } else {
                                format!("已选择：{}", option)
                            };
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

                        // 非 reject_with_feedback 场景：直接发送 InboundConfirmation
                        if option != "reject_with_feedback"
                            && let Some(ref message) = callback_query.message
                        {
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
                                confirmation: Some(InboundConfirmation {
                                    request_id,
                                    option,
                                    label: None,
                                    feedback: None,
                                }),
                            };
                            let _ = tx.send(inbound);
                        }
                    }
                    continue;
                }

                if let Some(msg) = update.message {
                    self.last_update_id
                        .store(update.update_id, Ordering::SeqCst);

                    if let Some(ref content) = msg.text
                        && content.starts_with("/bind ")
                        && self.config.pairing_enabled
                        && self.config.allowed_users.is_empty()
                    {
                        let code = content[6..].trim();
                        let expected = self.expected_pairing_code();
                        let mut persisted = false;
                        let reply = if !expected.is_empty() && code == expected {
                            self.runtime_allow(&msg.from.id.to_string());
                            if let Some(ref path) = self.config_path
                                && Self::is_writable_toml(path).await
                                && self
                                    .persist_allowed_user(&msg.from.id.to_string(), path)
                                    .await
                                    .is_ok()
                            {
                                persisted = true;
                            }
                            if persisted {
                                "已授权并已保存到配置。"
                            } else {
                                "已授权（本次运行有效）。"
                            }
                        } else {
                            "配对码错误。"
                        };
                        let payload = json!({
                            "chat_id": msg.chat.id,
                            "text": reply,
                            "message_thread_id": msg.message_thread_id,
                        });
                        self.post("sendMessage", &payload).await?;
                        continue;
                    }

                    if !self.is_allowed(&msg.from) {
                        warn!(
                            event = "TelegramUserDenied",
                            user_id = %msg.from.id,
                            "user not in allowed list"
                        );
                        continue;
                    }

                    if let Some(attachment) = self.extract_incoming_attachment(&msg).await {
                        let inbound = ChannelInboundMessage {
                            channel_name: self.name().to_string(),
                            sender_id: msg.from.id.to_string(),
                            chat_id: msg.chat.id.to_string(),
                            thread_id: msg.message_thread_id.map(|id| id.to_string()),
                            content: attachment.to_agent_text(),
                            timestamp_secs: msg.date as u64,
                            confirmation: None,
                        };
                        let _ = tx.send(inbound);
                        if let Err(e) = self.send_ack_reaction(msg.chat.id, msg.message_id).await {
                            warn!(
                                event = "TelegramAckReactionFailed",
                                chat_id = %msg.chat.id,
                                message_id = %msg.message_id,
                                error = %e,
                                "failed to send ack reaction"
                            );
                        }
                        continue;
                    }

                    if let Some(text) = msg.text {
                        let should_ack = !text.starts_with("/bind ");

                        // 检查是否有待处理反馈：若有，将文本作为 feedback 发送 Confirmation
                        let user_id = msg.from.id.to_string();
                        let pending = self.pending_feedback.write().unwrap().remove(&user_id);
                        if let Some(pending) = pending {
                            if text.trim() == "/cancel" {
                                // 取消反馈，发送普通拒绝
                                let _ = tx.send(ChannelInboundMessage {
                                    channel_name: self.name().to_string(),
                                    sender_id: user_id,
                                    chat_id: pending.chat_id,
                                    thread_id: pending.thread_id,
                                    content: String::new(),
                                    timestamp_secs: msg.date as u64,
                                    confirmation: Some(InboundConfirmation {
                                        request_id: pending.request_id,
                                        option: "reject".to_string(),
                                        label: None,
                                        feedback: None,
                                    }),
                                });
                                continue;
                            }

                            let feedback = text.trim().to_string();
                            let _ = tx.send(ChannelInboundMessage {
                                channel_name: self.name().to_string(),
                                sender_id: user_id,
                                chat_id: pending.chat_id,
                                thread_id: pending.thread_id,
                                content: String::new(),
                                timestamp_secs: msg.date as u64,
                                confirmation: Some(InboundConfirmation {
                                    request_id: pending.request_id,
                                    option: "reject_with_feedback".to_string(),
                                    label: None,
                                    feedback: Some(feedback),
                                }),
                            });
                            continue;
                        }

                        let _ = tx.send(ChannelInboundMessage {
                            channel_name: self.name().to_string(),
                            sender_id: msg.from.id.to_string(),
                            chat_id: msg.chat.id.to_string(),
                            thread_id: msg.message_thread_id.map(|id| id.to_string()),
                            content: text,
                            timestamp_secs: msg.date as u64,
                            confirmation: None,
                        });
                        if should_ack
                            && let Err(e) =
                                self.send_ack_reaction(msg.chat.id, msg.message_id).await
                        {
                            warn!(
                                event = "TelegramAckReactionFailed",
                                chat_id = %msg.chat.id,
                                message_id = %msg.message_id,
                                error = %e,
                                "failed to send ack reaction"
                            );
                        }
                        continue;
                    }

                    let payload = json!({
                        "chat_id": msg.chat.id,
                        "text": "暂不支持该消息类型。",
                        "message_thread_id": msg.message_thread_id,
                    });
                    if let Err(e) = self.post("sendMessage", &payload).await {
                        warn!(
                            event = "TelegramUnsupportedTypeReplyFailed",
                            chat_id = %msg.chat.id,
                            error = %e,
                            "failed to reply to unsupported message type"
                        );
                    }
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
            let lower = message.to_lowercase();
            [
                "can't parse entities",
                "unexpected",
                "unclosed",
                "tag",
                "entity",
            ]
            .iter()
            .any(|pattern| lower.contains(pattern))
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

fn decode_html_entities(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            let mut entity = String::new();
            let mut terminated = false;
            while let Some(&next) = chars.peek() {
                if next == ';' {
                    chars.next();
                    terminated = true;
                    break;
                }
                entity.push(chars.next().unwrap());
            }
            if terminated {
                match entity.as_str() {
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "amp" => out.push('&'),
                    "quot" => out.push('"'),
                    "#39" => out.push('\''),
                    _ => {
                        out.push('&');
                        out.push_str(&entity);
                        out.push(';');
                    }
                }
            } else {
                out.push('&');
                out.push_str(&entity);
            }
        } else {
            out.push(c);
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

fn prepare_text_parts(content: &str, parse_mode: Option<&ChannelParseMode>) -> Vec<String> {
    match parse_mode {
        Some(ChannelParseMode::Html) => {
            // 内容已经是合法 HTML，直接安全分片即可，避免把 <pre> 等标签二次转义。
            split_html_safely(content, TELEGRAM_MAX_TEXT_LENGTH)
        }
        None => {
            // 默认按 Markdown 处理，再转成 Telegram HTML。
            let chunks = split_markdown_semantic(content);
            chunks
                .into_iter()
                .map(|chunk| markdown_to_telegram_html(&chunk))
                .flat_map(|html| split_html_safely(&html, TELEGRAM_MAX_TEXT_LENGTH))
                .collect()
        }
        Some(ChannelParseMode::Markdown) => split_text(content, TELEGRAM_MAX_TEXT_LENGTH),
    }
}

fn build_send_payload(
    recipient: &str,
    thread_id: Option<&str>,
    text: &str,
    parse_mode: Option<&ChannelParseMode>,
    reply_markup: Option<&ReplyMarkup>,
) -> serde_json::Value {
    let mut payload = json!({
        "chat_id": recipient,
        "text": text,
    });
    if matches!(parse_mode, Some(ChannelParseMode::Html) | None) {
        payload["parse_mode"] = "HTML".into();
    }
    if let Some(thread_id) = thread_id
        && let Ok(id) = thread_id.parse::<i64>()
    {
        payload["message_thread_id"] = json!(id);
    }
    if let Some(reply_markup) = reply_markup {
        payload["reply_markup"] = json!(reply_markup);
    }
    payload
}

fn build_fallback_payload(
    recipient: &str,
    thread_id: Option<&str>,
    html: &str,
    reply_markup: Option<&ReplyMarkup>,
) -> serde_json::Value {
    let mut payload = json!({
        "chat_id": recipient,
        "text": decode_html_entities(&strip_tags(html)),
    });
    if let Some(thread_id) = thread_id
        && let Ok(id) = thread_id.parse::<i64>()
    {
        payload["message_thread_id"] = json!(id);
    }
    if let Some(reply_markup) = reply_markup {
        payload["reply_markup"] = json!(reply_markup);
    }
    payload
}

fn split_html_safely(html: &str, max_len: usize) -> Vec<String> {
    if html.len() <= max_len {
        return vec![html.to_string()];
    }

    let mut result = Vec::new();
    let mut start = 0;

    while start < html.len() {
        let remaining = html.len() - start;
        if remaining <= max_len {
            result.push(html[start..].to_string());
            break;
        }

        let target_end = start + max_len;
        let split_at = find_safe_split(html, start, target_end);

        if split_at == start {
            // Cannot find a safe split point; fall back to plain text splitting.
            let end = html.floor_char_boundary(target_end.min(html.len()));
            result.push(html[start..end].to_string());
            start = end;
        } else {
            result.push(html[start..split_at].to_string());
            start = split_at;
        }
    }

    result
}

fn find_safe_split(html: &str, start: usize, target_end: usize) -> usize {
    let target_end = target_end.min(html.len());

    if let Some(idx) = find_best_split(html, start, target_end, "\n\n", 2) {
        return idx;
    }
    if let Some(idx) = find_sentence_split(html, start, target_end) {
        return idx;
    }
    if let Some(idx) = find_best_split(html, start, target_end, " ", 1) {
        return idx;
    }
    for idx in (start + 1..=target_end).rev() {
        if html.is_char_boundary(idx) && is_outside_tag(html, idx) {
            return idx;
        }
    }
    start
}

fn find_best_split(
    html: &str,
    start: usize,
    target_end: usize,
    pattern: &str,
    offset: usize,
) -> Option<usize> {
    let mut best = None;
    for (idx, _) in html[start..target_end].match_indices(pattern) {
        let abs_idx = start + idx + offset;
        if abs_idx <= target_end && is_outside_tag(html, abs_idx) {
            best = Some(abs_idx);
        }
    }
    best
}

fn find_sentence_split(html: &str, start: usize, target_end: usize) -> Option<usize> {
    let mut best = None;
    for (idx, c) in html[start..target_end].char_indices() {
        if c == '.' || c == '!' || c == '?' {
            let abs_idx = start + idx + c.len_utf8();
            if abs_idx <= target_end && is_outside_tag(html, abs_idx) {
                best = Some(abs_idx);
            }
        }
    }
    best
}

fn is_outside_tag(html: &str, idx: usize) -> bool {
    let mut in_tag = false;
    for c in html[..idx.min(html.len())].chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        }
    }
    !in_tag
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
    message_id: i64,
    from: TelegramUser,
    chat: TelegramChat,
    date: i64,
    text: Option<String>,
    message_thread_id: Option<i64>,
    #[serde(default)]
    document: Option<TelegramDocument>,
    #[serde(default)]
    photo: Vec<TelegramPhotoSize>,
    #[serde(default)]
    voice: Option<TelegramVoice>,
}

#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
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
    use crate::channels::config::ChannelConfigs;
    use crate::channels::traits::{InlineKeyboardButton, MessageKind};
    use tempfile::NamedTempFile;

    fn cfg(users: Vec<String>) -> TelegramConfig {
        TelegramConfig {
            bot_token: "x".to_string(),
            allowed_users: users,
            pairing_enabled: false,
            pairing_code: None,
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
    fn runtime_allowlist_overrides_config() {
        let user = TelegramUser {
            id: 1,
            username: None,
        };
        let channel = TelegramChannel::new_with_path(
            TelegramConfig {
                bot_token: "x".to_string(),
                allowed_users: vec![],
                pairing_enabled: true,
                pairing_code: None,
            },
            Some(PathBuf::from("/dev/null")),
        );
        channel.runtime_allow("1");
        assert!(channel.is_allowed(&user));
    }

    #[test]
    fn runtime_allowlist_does_not_affect_other_users() {
        let allowed_user = TelegramUser {
            id: 1,
            username: None,
        };
        let other_user = TelegramUser {
            id: 2,
            username: None,
        };
        let channel = TelegramChannel::new(cfg(vec![]));
        channel.runtime_allow("1");
        assert!(channel.is_allowed(&allowed_user));
        assert!(!channel.is_allowed(&other_user));
    }

    #[test]
    fn expected_pairing_code_returns_configured_value() {
        let channel = TelegramChannel::new(TelegramConfig {
            bot_token: "x".to_string(),
            allowed_users: vec![],
            pairing_enabled: true,
            pairing_code: Some("secret".to_string()),
        });
        assert_eq!(channel.expected_pairing_code(), "secret");
    }

    #[test]
    fn expected_pairing_code_defaults_to_empty() {
        let channel = TelegramChannel::new(cfg(vec![]));
        assert_eq!(channel.expected_pairing_code(), "");
    }

    #[tokio::test]
    async fn is_writable_toml_true_for_writable_toml() {
        let file = NamedTempFile::with_suffix(".toml").unwrap();
        assert!(TelegramChannel::is_writable_toml(file.path()).await);
    }

    #[tokio::test]
    async fn is_writable_toml_false_for_non_toml_extension() {
        let file = NamedTempFile::with_suffix(".txt").unwrap();
        assert!(!TelegramChannel::is_writable_toml(file.path()).await);
    }

    #[tokio::test]
    async fn persist_allowed_user_appends_to_toml() {
        let file = NamedTempFile::with_suffix(".toml").unwrap();
        tokio::fs::write(
            file.path(),
            r#"[telegram]
bot_token = "x"
allowed_users = ["alice"]
"#,
        )
        .await
        .unwrap();

        let channel = TelegramChannel::new_with_path(
            TelegramConfig {
                bot_token: "x".to_string(),
                allowed_users: vec![],
                pairing_enabled: false,
                pairing_code: None,
            },
            Some(file.path().to_path_buf()),
        );
        channel
            .persist_allowed_user("123", file.path())
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(file.path()).await.unwrap();
        let parsed: ChannelConfigs = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed.telegram.unwrap().allowed_users,
            vec!["alice".to_string(), "123".to_string()]
        );
    }

    #[tokio::test]
    async fn persist_allowed_user_deduplicates() {
        let file = NamedTempFile::with_suffix(".toml").unwrap();
        tokio::fs::write(
            file.path(),
            r#"[telegram]
bot_token = "x"
allowed_users = ["123"]
"#,
        )
        .await
        .unwrap();

        let channel = TelegramChannel::new_with_path(
            TelegramConfig {
                bot_token: "x".to_string(),
                allowed_users: vec![],
                pairing_enabled: false,
                pairing_code: None,
            },
            Some(file.path().to_path_buf()),
        );
        channel
            .persist_allowed_user("123", file.path())
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(file.path()).await.unwrap();
        let parsed: ChannelConfigs = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed.telegram.unwrap().allowed_users,
            vec!["123".to_string()]
        );
    }

    #[tokio::test]
    async fn persist_allowed_user_preserves_qq_section() {
        let file = NamedTempFile::with_suffix(".toml").unwrap();
        tokio::fs::write(
            file.path(),
            r#"[telegram]
bot_token = "tg_token"
allowed_users = ["alice"]

[qq]
app_id = "qq_id"
app_secret = "qq_secret"
allowed_users = ["qq_user"]
"#,
        )
        .await
        .unwrap();

        let channel = TelegramChannel::new_with_path(
            TelegramConfig {
                bot_token: "tg_token".to_string(),
                allowed_users: vec![],
                pairing_enabled: false,
                pairing_code: None,
            },
            Some(file.path().to_path_buf()),
        );
        channel
            .persist_allowed_user("new_user", file.path())
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(file.path()).await.unwrap();
        let parsed: ChannelConfigs = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed.telegram.unwrap().allowed_users,
            vec!["alice".to_string(), "new_user".to_string()]
        );
        let qq = parsed.qq.unwrap();
        assert_eq!(qq.app_id, "qq_id");
        assert_eq!(qq.allowed_users, vec!["qq_user".to_string()]);
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
    fn is_parse_mode_error_detects_cant_parse_entities() {
        let err = ChannelError::Api {
            code: 400,
            message: "Bad Request: can't parse entities: unexpected closing tag".to_string(),
        };
        assert!(is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_detects_unexpected() {
        let err = ChannelError::Api {
            code: 400,
            message: "Bad Request: can't parse entities: unexpected character".to_string(),
        };
        assert!(is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_detects_unclosed() {
        let err = ChannelError::Api {
            code: 400,
            message: "Bad Request: can't parse entities: unclosed tag".to_string(),
        };
        assert!(is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_detects_entity() {
        let err = ChannelError::Api {
            code: 400,
            message: "Bad Request: can't parse entities: entity not found".to_string(),
        };
        assert!(is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_false_for_other_api_error() {
        let err = ChannelError::Api {
            code: 400,
            message: "chat not found".to_string(),
        };
        assert!(!is_parse_mode_error(&err));
    }

    #[test]
    fn is_parse_mode_error_false_for_network() {
        let err = ChannelError::NotConfigured;
        assert!(!is_parse_mode_error(&err));
    }

    #[test]
    fn markdown_parse_mode_sends_plain_text() {
        let payload = build_send_payload(
            "123",
            None,
            "hello **world**",
            Some(&ChannelParseMode::Markdown),
            None,
        );
        assert_eq!(payload["chat_id"], "123");
        assert_eq!(payload["text"], "hello **world**");
        assert!(payload.get("parse_mode").is_none());
    }

    #[test]
    fn html_parse_mode_sets_html() {
        let payload = build_send_payload(
            "123",
            None,
            "<b>hello</b>",
            Some(&ChannelParseMode::Html),
            None,
        );
        assert_eq!(payload["parse_mode"], "HTML");
    }

    #[test]
    fn none_parse_mode_sets_html() {
        let payload = build_send_payload("123", None, "<b>hello</b>", None, None);
        assert_eq!(payload["parse_mode"], "HTML");
    }

    #[test]
    fn split_html_safely_respects_max_length() {
        let inner = "word ".repeat(1500);
        let html = format!("<p>{}</p>", inner);
        let chunks = split_html_safely(&html, TELEGRAM_MAX_TEXT_LENGTH);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= TELEGRAM_MAX_TEXT_LENGTH);
        }
    }

    #[test]
    fn split_html_safely_keeps_short_html_intact() {
        let html = "<b>hello</b> <i>world</i>";
        let chunks = split_html_safely(html, TELEGRAM_MAX_TEXT_LENGTH);
        assert_eq!(chunks, vec![html.to_string()]);
    }

    #[test]
    fn prepare_text_parts_preserves_html_in_html_mode() {
        let html = "🔒 需要你的确认\n\n工具：channel_send\n输入：<pre>{\n  &quot;channel&quot;: &quot;telegram&quot;\n}</pre>";
        let parts = prepare_text_parts(html, Some(&ChannelParseMode::Html));
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], html);
    }

    #[test]
    fn decode_html_entities_decodes_basic_entities() {
        assert_eq!(decode_html_entities("&lt;&gt;&amp;&quot;&#39;"), "<>&\"'");
    }

    #[test]
    fn decode_html_entities_preserves_unknown_entities() {
        assert_eq!(decode_html_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn fallback_strips_tags_and_decodes_entities() {
        let html = "<b>hello</b> &lt;world&gt; &amp; &quot;test&quot; &#39;x&#39;";
        let payload = build_fallback_payload("123", None, html, None);
        assert_eq!(payload["text"], "hello <world> & \"test\" 'x'");
        assert!(payload.get("parse_mode").is_none());
    }

    #[test]
    fn fallback_preserves_reply_markup() {
        let reply_markup = ReplyMarkup::InlineKeyboard(vec![vec![InlineKeyboardButton {
            text: "Yes".to_string(),
            callback_data: "uuid:yes".to_string(),
        }]]);
        let payload = build_fallback_payload("123", None, "<b>hello</b>", Some(&reply_markup));
        assert_eq!(payload["text"], "hello");
        assert!(payload.get("reply_markup").is_some());
        assert_eq!(payload["reply_markup"], json!(reply_markup));
    }

    #[test]
    fn build_send_payload_preserves_reply_markup() {
        let reply_markup = ReplyMarkup::InlineKeyboard(vec![vec![InlineKeyboardButton {
            text: "No".to_string(),
            callback_data: "uuid:no".to_string(),
        }]]);
        let payload = build_send_payload(
            "123",
            Some("42"),
            "text",
            Some(&ChannelParseMode::Html),
            Some(&reply_markup),
        );
        assert_eq!(payload["chat_id"], "123");
        assert_eq!(payload["text"], "text");
        assert_eq!(payload["parse_mode"], "HTML");
        assert_eq!(payload["message_thread_id"], 42);
        assert_eq!(payload["reply_markup"], json!(reply_markup));
    }

    #[test]
    fn parse_attachment_markers() {
        let (text, attachments) =
            extract_attachments("see [IMAGE:/tmp/a.png] and [DOCUMENT:/tmp/b.pdf]");
        assert_eq!(text, "see  and ");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, AttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/a.png");
    }

    #[test]
    fn sanitize_filename_prevents_path_traversal() {
        assert_eq!(sanitize_telegram_filename("../../../etc/passwd"), "passwd");
        assert_eq!(
            sanitize_telegram_filename("..\\..\\windows\\system.ini"),
            "system.ini"
        );
        assert_eq!(
            sanitize_telegram_filename("/tmp/../../secret.txt"),
            "secret.txt"
        );
        assert_eq!(sanitize_telegram_filename(".."), "file");
        assert_eq!(sanitize_telegram_filename(""), "file");
        assert_eq!(sanitize_telegram_filename("normal.txt"), "normal.txt");
    }

    #[tokio::test]
    async fn url_attachment_uses_correct_json_field() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendPhoto"))
            .and(body_json(serde_json::json!({
                "chat_id": "123",
                "photo": "https://example.com/photo.jpg",
                "caption": "caption"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"message_id": 1}
            })))
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec!["u".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        let base = ChannelOutboundMessage {
            recipient: "123".to_string(),
            thread_id: None,
            content: "caption".to_string(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind: MessageKind::Other,
        };
        let attachment = ChannelAttachment {
            kind: AttachmentKind::Image,
            target: "https://example.com/photo.jpg".to_string(),
        };
        channel
            .send_attachment(&base, &attachment)
            .await
            .expect("send_attachment");
    }

    #[tokio::test]
    async fn empty_attachment_target_is_skipped() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        // Text message and document attachment both need endpoints.
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"message_id": 1}
            })))
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendDocument"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"message_id": 2}
            })))
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec!["u".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        channel
            .send(&ChannelOutboundMessage {
                recipient: "123".to_string(),
                thread_id: None,
                content: "see [IMAGE:] and [DOCUMENT:https://example.com/x.pdf]".to_string(),
                parse_mode: None,
                reply_markup: None,
                attachments: vec![],
                message_kind: MessageKind::Other,
            })
            .await
            .expect("send");
    }

    #[tokio::test]
    async fn approval_request_does_not_send_inline_attachments() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendMessage"))
            .and(body_json(serde_json::json!({
                "chat_id": "123",
                "text": "confirm [DOCUMENT:/tmp/video.mp4]",
                "parse_mode": "HTML",
                "reply_markup": {
                    "inline_keyboard": [[{
                        "text": "允许",
                        "callback_data": "req:allow"
                    }]]
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"message_id": 1}
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec!["u".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        channel
            .send(&ChannelOutboundMessage {
                recipient: "123".to_string(),
                thread_id: None,
                content: "confirm [DOCUMENT:/tmp/video.mp4]".to_string(),
                parse_mode: Some(ChannelParseMode::Html),
                reply_markup: Some(ReplyMarkup::InlineKeyboard(vec![vec![
                    InlineKeyboardButton {
                        text: "允许".to_string(),
                        callback_data: "req:allow".to_string(),
                    },
                ]])),
                attachments: vec![],
                message_kind: MessageKind::ApprovalRequest,
            })
            .await
            .expect("send approval request without attachments");
    }

    #[tokio::test]
    async fn send_ack_reaction_uses_message_id_modulo() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/setMessageReaction"))
            .and(body_json(serde_json::json!({
                "chat_id": 123,
                "message_id": 3,
                "reaction": [{"type": "emoji", "emoji": "🆗"}],
                "is_big": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        channel
            .send_ack_reaction(123, 3)
            .await
            .expect("ack reaction");
    }

    #[tokio::test]
    async fn listen_sends_ack_reaction_for_text_message() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Default empty response for subsequent getUpdates calls.
        Mock::given(method("GET"))
            .and(path("/botTOKEN/getUpdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": []
            })))
            .mount(&mock_server)
            .await;

        let update = serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 7,
                "from": {"id": 42, "username": "alice"},
                "chat": {"id": 123},
                "date": 1700000000,
                "text": "hello"
            }
        });
        Mock::given(method("GET"))
            .and(path("/botTOKEN/getUpdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": [update]
            })))
            .with_priority(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/botTOKEN/setMessageReaction"))
            .and(body_json(serde_json::json!({
                "chat_id": 123,
                "message_id": 7,
                "reaction": [{"type": "emoji", "emoji": "🆗"}],
                "is_big": false,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec!["alice".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        let (tx, rx) = crossbeam_channel::bounded(1);

        let handle = tokio::spawn(async move {
            let _ = channel.listen(tx).await;
        });

        let inbound = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(msg) = rx.try_recv() {
                    return msg;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("inbound message");
        assert_eq!(inbound.content, "hello");

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        handle.abort();
    }

    #[tokio::test]
    async fn listen_replies_to_unsupported_message_type() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/botTOKEN/getUpdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": []
            })))
            .mount(&mock_server)
            .await;

        let update = serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 8,
                "from": {"id": 42, "username": "alice"},
                "chat": {"id": 123},
                "date": 1700000000
            }
        });
        Mock::given(method("GET"))
            .and(path("/botTOKEN/getUpdates"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": [update]
            })))
            .with_priority(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendMessage"))
            .and(body_json(serde_json::json!({
                "chat_id": 123,
                "text": "暂不支持该消息类型。",
                "message_thread_id": null,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"message_id": 9}
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec!["alice".to_string()],
            pairing_enabled: false,
            pairing_code: None,
        };
        let channel = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        let (tx, _rx) = crossbeam_channel::bounded(1);

        let handle = tokio::spawn(async move {
            let _ = channel.listen(tx).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        handle.abort();
    }

    #[tokio::test]
    async fn recall_message_calls_delete_message() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/deleteMessage"))
            .and(body_string_contains("chat_id"))
            .and(body_string_contains("message_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true, "result": true})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        };
        let ch = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        ch.recall_message("123456", "789").await.expect("recall");
    }

    #[tokio::test]
    async fn send_typing_calls_send_chat_action() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendChatAction"))
            .and(body_string_contains("\"action\":\"typing\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true, "result": true})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        };
        let ch = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        ch.send_typing("123456").await.expect("typing");
    }

    #[tokio::test]
    async fn send_returns_message_id() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 42, "chat": { "id": 123456 }, "text": "hello" }
            })))
            .mount(&mock_server)
            .await;

        let cfg = TelegramConfig {
            bot_token: "TOKEN".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        };
        let ch = TelegramChannel::new(cfg).with_base_url(mock_server.uri());
        let msg = ChannelOutboundMessage {
            recipient: "123456".to_string(),
            thread_id: None,
            content: "hello".to_string(),
            parse_mode: None,
            reply_markup: None,
            attachments: vec![],
            message_kind: MessageKind::LLMReply,
        };
        let result = ch.send(&msg).await.expect("send");
        assert_eq!(result, Some("42".to_string()));
    }
}
