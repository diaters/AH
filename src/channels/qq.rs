//! QQ 官方 Bot API 通道实现

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::sync::RwLock;

use crate::channels::config::QqConfig;
use crate::channels::traits::{Channel, ChannelError};

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

/// 修复 QQ CDN URL 缺失协议前缀的问题（//cdn.example.com → https://cdn.example.com）
#[allow(dead_code)]
fn fix_qq_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// 根据 content_type 或文件扩展名推断附件 marker 类型。
#[allow(dead_code)]
fn infer_attachment_marker(content_type: &str, filename: &str) -> &'static str {
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("image/") {
        return "IMAGE";
    }
    if ct.starts_with("audio/") || ct.contains("voice") {
        return "VOICE";
    }
    if ct.starts_with("video/") {
        return "VIDEO";
    }
    let lower = filename.to_ascii_lowercase();
    if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".heic")
        || lower.ends_with(".heif")
        || lower.ends_with(".svg")
    {
        return "IMAGE";
    }
    if lower.ends_with(".mp3")
        || lower.ends_with(".wav")
        || lower.ends_with(".silk")
        || lower.ends_with(".ogg")
        || lower.ends_with(".flac")
        || lower.ends_with(".m4a")
    {
        return "VOICE";
    }
    if lower.ends_with(".mp4")
        || lower.ends_with(".mov")
        || lower.ends_with(".mkv")
        || lower.ends_with(".avi")
        || lower.ends_with(".webm")
    {
        return "VIDEO";
    }
    "DOCUMENT"
}

const DEDUP_CAPACITY: usize = 10_000;

/// QQ API 媒体文件类型枚举（数值对应 API file_type 字段）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QqMediaFileType {
    Image = 1,
    Video = 2,
    Voice = 3,
    File = 4,
}

/// 根据 marker 字符串与目标路径扩展名映射到 QQMediaFileType。
/// AUDIO/VOICE 非原生格式（非 wav/mp3/silk）降级为 File。
#[allow(dead_code)]
fn marker_kind_to_qq_file_type(marker: &str, target: &str) -> Option<QqMediaFileType> {
    match marker.trim().to_ascii_uppercase().as_str() {
        "IMAGE" | "PHOTO" => Some(QqMediaFileType::Image),
        "DOCUMENT" | "FILE" => Some(QqMediaFileType::File),
        "VIDEO" => Some(QqMediaFileType::Video),
        "AUDIO" | "VOICE" => {
            let ext = std::path::Path::new(target.split('?').next().unwrap_or(target))
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if matches!(ext.to_ascii_lowercase().as_str(), "wav" | "mp3" | "silk") {
                Some(QqMediaFileType::Voice)
            } else {
                Some(QqMediaFileType::File)
            }
        }
        _ => None,
    }
}

/// 将 ChannelFrontend 生成的 HTML 审批内容转换为 QQ markdown。
/// - `<pre>...</pre>` → ``` ... ``` 代码块
/// - `<b>...</b>` / `<strong>...</strong>` → **...**
/// - `<i>...</i>` / `<em>...</em>` → *...*
/// - `<code>...</code>` → `...`
/// - `<br>` → 换行
/// - 其他 HTML 特殊字符 `&lt;` `&gt;` `&amp;` `&quot;` `&#39;` 反转义为原字符
/// - 未识别的标签剥除标签保留内容
fn html_to_markdown_for_qq(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // 查找闭合 >
            if let Some(close_rel) = text[i + 1..].find('>') {
                let close = i + 1 + close_rel;
                let tag = &text[i + 1..close];
                let tag_lower = tag.to_ascii_lowercase();
                let trimmed = tag_lower.trim_end_matches('/');
                if trimmed == "br" {
                    out.push('\n');
                    i = close + 1;
                    continue;
                }
                // 处理 <pre>、<b>、<strong>、<i>、<em>、<code> 等
                if let Some(tag_name) = trimmed.split_whitespace().next() {
                    let (prefix, suffix, skip_inner_newline) = match tag_name {
                        "pre" => ("```\n", "```", false),
                        "b" | "strong" => ("**", "**", true),
                        "i" | "em" => ("*", "*", true),
                        "code" => ("`", "`", true),
                        _ => ("", "", true),
                    };
                    if !prefix.is_empty() {
                        // 找到对应闭合标签
                        let close_tag = format!("/{tag_name}");
                        if let Some(close_end_rel) = text[close + 1..]
                            .find(&format!("<{close_tag}>"))
                            .or_else(|| text[close + 1..].find(&format!("<{close_tag} ")))
                        {
                            let inner_start = close + 1;
                            let inner_end = close + 1 + close_end_rel;
                            let inner = &text[inner_start..inner_end];
                            out.push_str(prefix);
                            out.push_str(&html_to_markdown_for_qq(inner));
                            if !skip_inner_newline && !inner.ends_with('\n') {
                                out.push('\n');
                            }
                            out.push_str(suffix);
                            i = inner_end + close_tag.len() + 2; // 跳过 <...>
                            continue;
                        }
                    }
                    // 未识别标签剥除
                    i = close + 1;
                    continue;
                }
            }
            out.push('<');
            i += 1;
        } else if text[i..].starts_with("&lt;") {
            out.push('<');
            i += 4;
        } else if text[i..].starts_with("&gt;") {
            out.push('>');
            i += 4;
        } else if text[i..].starts_with("&amp;") {
            out.push('&');
            i += 5;
        } else if text[i..].starts_with("&quot;") {
            out.push('"');
            i += 6;
        } else if text[i..].starts_with("&#39;") {
            out.push('\'');
            i += 5;
        } else {
            let c = text[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// 生成 QQ API 请求用的 msg_seq（0~65535）。
fn next_msg_seq() -> u32 {
    let time_part = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32)
        % 100_000_000;
    let random = (uuid::Uuid::new_v4().as_u128() & 0xFFFF) as u32;
    (time_part ^ random) % 65536
}

#[derive(Debug, serde::Deserialize)]
struct QqUploadResponse {
    file_info: String,
    #[allow(dead_code)]
    file_uuid: Option<String>,
    ttl: Option<u64>,
}

/// 解析 QQ 上传类接口返回，兼容顶层或 data 包裹格式。
fn parse_upload_response_body(raw_body: &str) -> Result<QqUploadResponse, ChannelError> {
    let root: serde_json::Value =
        serde_json::from_str(raw_body).map_err(|e| ChannelError::Api {
            code: 0,
            message: format!("QQ upload response json decode failed: {e}"),
        })?;
    let data = root.get("data").unwrap_or(&root);
    let file_info = data
        .get("file_info")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ChannelError::Api {
            code: 0,
            message: format!("QQ upload response missing file_info; body={raw_body}"),
        })?
        .to_string();
    let file_uuid = data
        .get("file_uuid")
        .and_then(serde_json::Value::as_str)
        .map(std::string::ToString::to_string);
    let ttl = data
        .get("ttl")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            data.get("ttl")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<u64>().ok())
        });
    Ok(QqUploadResponse {
        file_info,
        file_uuid,
        ttl,
    })
}

/// 将 ReplyMarkup 转译为编号列表文本（Task 6 完整实现）。
fn render_buttons_as_numbered_list(
    markup: &crate::channels::traits::ReplyMarkup,
    base_content: &str,
) -> String {
    // 占位：Task 6 实现完整逻辑
    let _ = markup;
    base_content.to_string()
}

/// QQ 通道实现
#[allow(dead_code)]
pub struct QqChannel {
    config: QqConfig,
    config_path: Option<PathBuf>,
    runtime_allowed_users: Arc<RwLock<HashSet<String>>>,
    client: Client,
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    dedup: Arc<RwLock<HashSet<String>>>,
    workspace_dir: Option<PathBuf>,
    api_base: String,
    auth_url: String,
}

impl QqChannel {
    pub fn new(config: QqConfig) -> Self {
        Self::new_with_path(config, None)
    }

    pub fn new_with_path(config: QqConfig, config_path: Option<PathBuf>) -> Self {
        Self {
            config,
            config_path,
            runtime_allowed_users: Arc::new(RwLock::new(HashSet::new())),
            client: Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
            dedup: Arc::new(RwLock::new(HashSet::new())),
            workspace_dir: None,
            api_base: QQ_API_BASE.to_string(),
            auth_url: QQ_AUTH_URL.to_string(),
        }
    }

    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// 白名单匹配：runtime_allowed_users 优先，然后按 allowed_users 通配符 `*` 或精确 openid 匹配。
    #[allow(dead_code)]
    async fn is_user_allowed(&self, user_openid: &str) -> bool {
        if self
            .runtime_allowed_users
            .read()
            .await
            .contains(user_openid)
        {
            return true;
        }
        if self.config.allowed_users.iter().any(|u| u == "*") {
            return true;
        }
        if self.config.allowed_users.is_empty() {
            return false;
        }
        self.config
            .allowed_users
            .iter()
            .any(|allowed| allowed == user_openid)
    }

    /// 加入运行时白名单（/bind 配对通过时调用）。
    #[allow(dead_code)]
    async fn runtime_allow(&self, user_openid: &str) {
        self.runtime_allowed_users
            .write()
            .await
            .insert(user_openid.to_string());
    }

    /// 消息去重检查：msg_id 已存在返回 true，否则插入并返回 false。
    /// 容量达上限时淘汰一半旧条目。
    #[allow(dead_code)]
    async fn is_duplicate(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }
        let mut dedup = self.dedup.write().await;
        if dedup.contains(msg_id) {
            return true;
        }
        if dedup.len() >= DEDUP_CAPACITY {
            let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }
        dedup.insert(msg_id.to_string());
        false
    }

    /// 下载附件到本地工作目录，文件名加 UUID 后缀避免冲突。
    #[allow(dead_code)]
    async fn download_attachment(
        &self,
        url: &str,
        dir: &std::path::Path,
        filename: &str,
    ) -> Result<std::path::PathBuf, ChannelError> {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| ChannelError::Api {
                code: 0,
                message: e.to_string(),
            })?;
        let stem = std::path::Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = std::path::Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let unique = &uuid::Uuid::new_v4().to_string()[..8];
        let safe_name = if ext.is_empty() {
            format!("{stem}_{unique}")
        } else {
            format!("{stem}_{unique}.{ext}")
        };
        let dest = dir.join(&safe_name);
        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(ChannelError::Api {
                code: resp.status().as_u16() as i32,
                message: format!("download failed: {url}"),
            });
        }
        let bytes = resp.bytes().await?;
        tokio::fs::write(&dest, &bytes)
            .await
            .map_err(|e| ChannelError::Api {
                code: 0,
                message: e.to_string(),
            })?;
        Ok(dest)
    }

    /// 从 QQ 入向事件 payload 组装消息内容，处理附件下载与 marker 生成。
    #[allow(dead_code)]
    async fn compose_message_content(&self, payload: &serde_json::Value) -> Option<String> {
        let text = payload
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim();
        let mut markers: Vec<String> = Vec::new();
        let mut voice_transcripts: Vec<String> = Vec::new();

        if let Some(attachments) = payload.get("attachments").and_then(|a| a.as_array()) {
            for att in attachments {
                let url = match att.get("url").and_then(|u| u.as_str()) {
                    Some(u) if !u.trim().is_empty() => fix_qq_url(u),
                    _ => continue,
                };
                let content_type = att
                    .get("content_type")
                    .and_then(|ct| ct.as_str())
                    .unwrap_or("");
                let filename = att
                    .get("filename")
                    .and_then(|f| f.as_str())
                    .unwrap_or("attachment");
                let marker_type = infer_attachment_marker(content_type, filename);

                let is_voice = content_type == "voice"
                    || content_type.starts_with("audio/")
                    || marker_type == "VOICE";
                let (download_url, save_filename) = if is_voice {
                    if let Some(wav_url) = att
                        .get("voice_wav_url")
                        .and_then(|u| u.as_str())
                        .filter(|u| !u.trim().is_empty())
                    {
                        let fixed = fix_qq_url(wav_url);
                        let wav_name =
                            std::path::Path::new(fixed.split('?').next().unwrap_or(&fixed))
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("voice.wav")
                                .to_string();
                        (fixed, wav_name)
                    } else {
                        (url.clone(), filename.to_string())
                    }
                } else {
                    (url.clone(), filename.to_string())
                };

                let location = if let Some(ref ws) = self.workspace_dir {
                    let dir = ws.join("qq_files");
                    match self
                        .download_attachment(&download_url, &dir, &save_filename)
                        .await
                    {
                        Ok(local_path) => local_path.display().to_string(),
                        Err(e) => {
                            tracing::warn!(event = "QqDownloadFailed", url = %download_url, error = %e, "failed to download attachment");
                            download_url.clone()
                        }
                    }
                } else {
                    download_url.clone()
                };

                if is_voice {
                    markers.push(format!("[{marker_type}:{location}]"));
                    if let Some(asr_text) = att
                        .get("asr_refer_text")
                        .and_then(|t| t.as_str())
                        .map(|t| t.trim())
                        .filter(|t| !t.is_empty())
                    {
                        voice_transcripts.push(asr_text.to_string());
                    }
                } else {
                    markers.push(format!("[{marker_type}:{location}]"));
                }
            }
        }

        let voice_text = match voice_transcripts.len() {
            0 => String::new(),
            1 => format!(
                "<VOICE_TRANSCRIPTION>{}</VOICE_TRANSCRIPTION>",
                voice_transcripts[0]
            ),
            _ => voice_transcripts
                .iter()
                .enumerate()
                .map(|(i, t)| format!("<VOICE_TRANSCRIPTION_{i}>{t}</VOICE_TRANSCRIPTION_{i}>"))
                .collect::<Vec<_>>()
                .join("\n"),
        };

        let mut parts: Vec<&str> = Vec::new();
        if !text.is_empty() {
            parts.push(text);
        }
        if !voice_text.is_empty() {
            parts.push(&voice_text);
        }
        let markers_joined = markers.join("\n");
        if !markers_joined.is_empty() {
            parts.push(&markers_joined);
        }
        if parts.is_empty() {
            return None;
        }
        Some(parts.join("\n"))
    }

    /// 测试用：覆盖 API base URL。
    #[cfg(test)]
    #[allow(dead_code)]
    fn with_api_base(mut self, base: String) -> Self {
        self.api_base = base;
        self
    }

    /// 测试用：覆盖 OAuth2 端点 URL。
    #[cfg(test)]
    fn with_auth_url(mut self, url: String) -> Self {
        self.auth_url = url;
        self
    }

    /// 测试用：预置 token 避免真实 OAuth2 调用。
    #[cfg(test)]
    #[allow(dead_code)]
    async fn set_token_for_test(&self, token: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut cache = self.token_cache.write().await;
        *cache = Some((token.to_string(), now + 3600));
    }

    /// 向 QQ OAuth2 端点获取 access_token，返回 (token, 过期时间戳)。
    async fn fetch_access_token(&self) -> Result<(String, u64), ChannelError> {
        let body = json!({
            "appId": self.config.app_id,
            "clientSecret": self.config.app_secret,
        });
        let resp = self.client.post(&self.auth_url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        let data: serde_json::Value = resp.json().await?;
        let token = data
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or(ChannelError::Auth)?
            .to_string();
        let expires_in = data
            .get("expires_in")
            .and_then(|e| {
                e.as_u64()
                    .or_else(|| e.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .unwrap_or(7200);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let expiry = now + expires_in.saturating_sub(60);
        Ok((token, expiry))
    }

    /// 获取有效 access_token，过期时重新获取。
    #[allow(dead_code)]
    async fn get_token(&self) -> Result<String, ChannelError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        {
            let cache = self.token_cache.read().await;
            if let Some((ref token, expiry)) = *cache
                && now < expiry
            {
                return Ok(token.clone());
            }
        }
        let (token, expiry) = self.fetch_access_token().await?;
        {
            let mut cache = self.token_cache.write().await;
            *cache = Some((token.clone(), expiry));
        }
        Ok(token)
    }

    /// 解析 recipient 字符串为 (scope, id)。
    /// scope="groups" 对应群消息，scope="users" 对应 C2C。
    /// 保留 QQ openid 中合法字符（字母、数字、`_`、`-`），避免截断 UUID 形式的 openid。
    fn resolve_recipient(recipient: &str) -> (&'static str, String) {
        if let Some(group_id) = recipient.strip_prefix("group:") {
            return ("groups", group_id.to_string());
        }
        let raw_uid = recipient.strip_prefix("user:").unwrap_or(recipient);
        let user_id: String = raw_uid
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        ("users", user_id)
    }

    /// 发送 markdown 文本消息（msg_type=2）。
    async fn send_text_markdown(&self, recipient: &str, content: &str) -> Result<(), ChannelError> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);
        let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);
        let body = json!({
            "markdown": { "content": content },
            "msg_type": 2,
            "msg_seq": next_msg_seq(),
        });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        Ok(())
    }

    /// 发送富媒体消息（msg_type=7）。
    async fn send_media_message(
        &self,
        recipient: &str,
        file_info: &str,
    ) -> Result<(), ChannelError> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);
        let url = format!("{}/v2/{scope}/{id}/messages", self.api_base);
        let body = json!({
            "msg_type": 7,
            "media": { "file_info": file_info },
            "msg_seq": next_msg_seq(),
        });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        Ok(())
    }

    /// 上传媒体到 QQ API，返回 (file_info, ttl)。
    /// - url 模式：传 url=Some(...)，file_data=None
    /// - base64 模式：传 file_data=Some(...)，url=None
    async fn upload_media(
        &self,
        recipient: &str,
        file_type: QqMediaFileType,
        url: Option<&str>,
        file_data: Option<&str>,
        file_name: Option<&str>,
    ) -> Result<(String, Option<u64>), ChannelError> {
        let token = self.get_token().await?;
        let (scope, id) = Self::resolve_recipient(recipient);
        let api_url = format!("{}/v2/{scope}/{id}/files", self.api_base);
        let mut body = json!({
            "file_type": file_type as u8,
            "srv_send_msg": false,
        });
        if let Some(u) = url {
            body["url"] = json!(u);
        }
        if let Some(d) = file_data {
            body["file_data"] = json!(d);
        }
        if file_type == QqMediaFileType::File
            && let Some(name) = file_name
        {
            body["file_name"] = json!(name);
        }
        let resp = self
            .client
            .post(&api_url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::Api {
                code: status.as_u16() as i32,
                message: text,
            });
        }
        let raw_body = resp.text().await.unwrap_or_default();
        let upload_resp = parse_upload_response_body(&raw_body)?;
        Ok((upload_resp.file_info, upload_resp.ttl))
    }

    /// 发送单个附件：根据 target 路径分发到 URL / base64 上传。
    async fn send_attachment(
        &self,
        recipient: &str,
        attachment: &crate::channels::traits::ChannelAttachment,
    ) -> Result<(), ChannelError> {
        use crate::channels::traits::AttachmentKind;
        use base64::Engine as _;

        let target = attachment.target.trim();
        let file_name = std::path::Path::new(target.split('?').next().unwrap_or(target))
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);

        let qq_file_type = match attachment.kind {
            AttachmentKind::Image => QqMediaFileType::Image,
            AttachmentKind::Document => QqMediaFileType::File,
            AttachmentKind::Video => QqMediaFileType::Video,
            AttachmentKind::Audio | AttachmentKind::Voice => {
                let ext = std::path::Path::new(target.split('?').next().unwrap_or(target))
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if matches!(ext.to_ascii_lowercase().as_str(), "wav" | "mp3" | "silk") {
                    QqMediaFileType::Voice
                } else {
                    QqMediaFileType::File
                }
            }
        };

        if target.starts_with("http://") || target.starts_with("https://") {
            // 安全：仅允许 HTTPS URL，避免 SSRF（QQ API 服务器作为代理拉取附件 URL）。
            if target.starts_with("http://") {
                return Err(ChannelError::Api {
                    code: 0,
                    message: format!("QQ attachment URL must be HTTPS: {target}"),
                });
            }
            let (file_info, _) = self
                .upload_media(
                    recipient,
                    qq_file_type,
                    Some(target),
                    None,
                    file_name.as_deref(),
                )
                .await?;
            self.send_media_message(recipient, &file_info).await?;
        } else {
            let path = std::path::Path::new(target);
            if !path.exists() {
                return Err(ChannelError::Api {
                    code: 0,
                    message: format!("QQ attachment path not found: {target}"),
                });
            }
            let file_bytes = tokio::fs::read(path).await.map_err(|e| ChannelError::Api {
                code: 0,
                message: e.to_string(),
            })?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&file_bytes);
            let (file_info, _) = self
                .upload_media(
                    recipient,
                    qq_file_type,
                    None,
                    Some(&b64),
                    file_name.as_deref(),
                )
                .await?;
            self.send_media_message(recipient, &file_info).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for QqChannel {
    fn name(&self) -> &str {
        "qq"
    }

    async fn send(
        &self,
        message: &crate::channels::traits::ChannelOutboundMessage,
    ) -> Result<(), ChannelError> {
        use crate::channels::traits::{ChannelParseMode, extract_attachments};

        let (text, inline_attachments) = if message.reply_markup.is_some() {
            (message.content.clone(), vec![])
        } else {
            extract_attachments(&message.content)
        };

        let all_attachments: Vec<_> = message
            .attachments
            .iter()
            .chain(inline_attachments.iter())
            .filter(|a| !a.target.trim().is_empty())
            .cloned()
            .collect();

        let final_text = if let Some(ref markup) = message.reply_markup {
            render_buttons_as_numbered_list(markup, &text)
        } else {
            text.clone()
        };

        let content_to_send = match message.parse_mode {
            // ChannelFrontend 用 Html 模式发送审批请求（含 <pre> 等标签）。
            // QQ 不支持 HTML 渲染，将 HTML 转换为 markdown（代码块/粗体/斜体）后发送。
            Some(ChannelParseMode::Html) => html_to_markdown_for_qq(&final_text),
            Some(ChannelParseMode::Markdown) | None => final_text,
        };

        if !content_to_send.trim().is_empty() {
            self.send_text_markdown(&message.recipient, &content_to_send)
                .await?;
        }

        for attachment in &all_attachments {
            if let Err(e) = self.send_attachment(&message.recipient, attachment).await {
                tracing::warn!(
                    event = "QqSendAttachmentFailed",
                    target = %attachment.target,
                    error = %e,
                    "QQ attachment send failed, degrading to text"
                );
                let fallback = format!(
                    "{}: {}",
                    match attachment.kind {
                        crate::channels::traits::AttachmentKind::Image => "Image",
                        crate::channels::traits::AttachmentKind::Document => "File",
                        crate::channels::traits::AttachmentKind::Video => "Video",
                        crate::channels::traits::AttachmentKind::Audio => "Audio",
                        crate::channels::traits::AttachmentKind::Voice => "Voice",
                    },
                    attachment.target
                );
                self.send_text_markdown(&message.recipient, &fallback)
                    .await?;
            }
        }
        Ok(())
    }

    async fn listen(
        &self,
        _tx: crossbeam_channel::Sender<crate::channels::traits::ChannelInboundMessage>,
    ) -> Result<(), ChannelError> {
        // 占位实现，后续 Task 填充
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.fetch_access_token().await.is_ok()
    }

    fn supported_attachment_kinds(&self) -> Vec<crate::channels::traits::AttachmentKind> {
        use crate::channels::traits::AttachmentKind;
        vec![
            AttachmentKind::Image,
            AttachmentKind::Document,
            AttachmentKind::Video,
            AttachmentKind::Audio,
            AttachmentKind::Voice,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> QqConfig {
        QqConfig {
            app_id: "test_id".to_string(),
            app_secret: "test_secret".to_string(),
            allowed_users: vec![],
            pairing_enabled: false,
            pairing_code: None,
        }
    }

    #[test]
    fn name_returns_qq() {
        let ch = QqChannel::new(make_config());
        assert_eq!(ch.name(), "qq");
    }

    #[test]
    fn workspace_dir_builder_sets_field() {
        let ch = QqChannel::new(make_config()).with_workspace_dir(PathBuf::from("/tmp/qq"));
        assert_eq!(ch.workspace_dir, Some(PathBuf::from("/tmp/qq")));
    }

    #[tokio::test]
    async fn fetch_access_token_parses_response() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/getAppAccessToken"))
            .and(body_json(serde_json::json!({
                "appId": "test_id",
                "clientSecret": "test_secret",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "mock_token_abc",
                "expires_in": "7200",
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config())
            .with_auth_url(format!("{}/app/getAppAccessToken", mock_server.uri()));
        let (token, expiry) = ch.fetch_access_token().await.expect("fetch_access_token");
        assert_eq!(token, "mock_token_abc");
        // expiry 应在未来 7200-60=7140 秒处
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        assert!(expiry > now + 7000 && expiry < now + 7200);
    }

    #[tokio::test]
    async fn fetch_access_token_returns_auth_error_on_missing_field() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/getAppAccessToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "some_other_field": "no_token"
            })))
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config())
            .with_auth_url(format!("{}/app/getAppAccessToken", mock_server.uri()));
        let result = ch.fetch_access_token().await;
        assert!(matches!(result, Err(ChannelError::Auth)));
    }

    #[tokio::test]
    async fn token_cache_reuse_within_expiry() {
        let ch = QqChannel::new(make_config());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // 手动写入未过期 token
        {
            let mut cache = ch.token_cache.write().await;
            *cache = Some(("cached_token".to_string(), now + 3600));
        }
        let token = ch.get_token().await.expect("get_token");
        assert_eq!(token, "cached_token");
    }

    #[tokio::test]
    async fn token_cache_expired_triggers_refetch() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/getAppAccessToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "fresh_token",
                "expires_in": "3600",
            })))
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config())
            .with_auth_url(format!("{}/app/getAppAccessToken", mock_server.uri()));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // 写入已过期 token
        {
            let mut cache = ch.token_cache.write().await;
            *cache = Some(("old_token".to_string(), now - 1));
        }
        let token = ch.get_token().await.expect("get_token");
        assert_eq!(token, "fresh_token");
    }

    #[test]
    fn fix_qq_url_prepends_https() {
        assert_eq!(
            fix_qq_url("//cdn.example.com/a.png"),
            "https://cdn.example.com/a.png"
        );
        assert_eq!(
            fix_qq_url("https://cdn.example.com/a.png"),
            "https://cdn.example.com/a.png"
        );
    }

    #[test]
    fn infer_attachment_marker_by_content_type() {
        assert_eq!(infer_attachment_marker("image/png", "a"), "IMAGE");
        assert_eq!(infer_attachment_marker("audio/mpeg", "a"), "VOICE");
        assert_eq!(infer_attachment_marker("video/mp4", "a"), "VIDEO");
        assert_eq!(infer_attachment_marker("application/pdf", "a"), "DOCUMENT");
    }

    #[test]
    fn infer_attachment_marker_by_extension_fallback() {
        assert_eq!(infer_attachment_marker("", "photo.jpg"), "IMAGE");
        assert_eq!(infer_attachment_marker("", "song.mp3"), "VOICE");
        assert_eq!(infer_attachment_marker("", "clip.mp4"), "VIDEO");
        assert_eq!(infer_attachment_marker("", "unknown.xyz"), "DOCUMENT");
    }

    #[tokio::test]
    async fn compose_text_only() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({ "content": "  hello world  " });
        assert_eq!(
            ch.compose_message_content(&payload).await,
            Some("hello world".to_string())
        );
    }

    #[tokio::test]
    async fn compose_image_attachment_without_workspace() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "   ",
            "attachments": [{
                "content_type": "image/jpg",
                "url": "https://cdn.example.com/a.jpg"
            }]
        });
        assert_eq!(
            ch.compose_message_content(&payload).await,
            Some("[IMAGE:https://cdn.example.com/a.jpg]".to_string())
        );
    }

    #[tokio::test]
    async fn compose_text_and_multiple_attachments() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "Here is an image",
            "attachments": [
                { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
                { "filename": "b.jpeg", "url": "https://cdn.example.com/b.jpeg" }
            ]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("Here is an image"));
        assert!(result.contains("[IMAGE:https://cdn.example.com/a.png]"));
        assert!(result.contains("[IMAGE:https://cdn.example.com/b.jpeg]"));
    }

    #[tokio::test]
    async fn compose_fixes_double_slash_url() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "",
            "attachments": [{
                "content_type": "image/png",
                "url": "//cdn.example.com/a.png"
            }]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("https://cdn.example.com/a.png"));
        assert!(!result.starts_with("[IMAGE://"));
    }

    #[tokio::test]
    async fn compose_drops_empty_url() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "   ",
            "attachments": [{
                "content_type": "image/png",
                "url": "   "
            }]
        });
        assert_eq!(ch.compose_message_content(&payload).await, None);
    }

    #[tokio::test]
    async fn compose_all_attachment_types() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "",
            "attachments": [
                { "content_type": "image/png", "url": "https://cdn.example.com/a.png" },
                { "content_type": "audio/mpeg", "url": "https://cdn.example.com/b.mp3" },
                { "content_type": "video/mp4", "url": "https://cdn.example.com/c.mp4" },
                { "content_type": "application/pdf", "url": "https://cdn.example.com/d.pdf" }
            ]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("[IMAGE:"));
        assert!(result.contains("[VOICE:"));
        assert!(result.contains("[VIDEO:"));
        assert!(result.contains("[DOCUMENT:"));
    }

    #[tokio::test]
    async fn compose_voice_with_asr_transcription() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "语音消息",
            "attachments": [{
                "content_type": "voice",
                "url": "https://cdn.example.com/v.silk",
                "asr_refer_text": "你好世界"
            }]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("语音消息"));
        assert!(result.contains("[VOICE:"));
        assert!(result.contains("<VOICE_TRANSCRIPTION>你好世界</VOICE_TRANSCRIPTION>"));
    }

    #[tokio::test]
    async fn compose_voice_prefers_wav_url() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "",
            "attachments": [{
                "content_type": "voice",
                "url": "https://cdn.example.com/v.silk",
                "voice_wav_url": "https://cdn.example.com/v.wav?sign=abc"
            }]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        // WAV URL 应优先使用
        assert!(result.contains("[VOICE:https://cdn.example.com/v.wav?sign=abc]"));
    }

    #[tokio::test]
    async fn compose_multiple_voice_transcriptions() {
        let ch = QqChannel::new(make_config());
        let payload = serde_json::json!({
            "content": "",
            "attachments": [
                { "content_type": "voice", "url": "https://cdn.example.com/v1.silk", "asr_refer_text": "第一段" },
                { "content_type": "voice", "url": "https://cdn.example.com/v2.silk", "asr_refer_text": "第二段" }
            ]
        });
        let result = ch.compose_message_content(&payload).await.unwrap();
        assert!(result.contains("<VOICE_TRANSCRIPTION_0>第一段</VOICE_TRANSCRIPTION_0>"));
        assert!(result.contains("<VOICE_TRANSCRIPTION_1>第二段</VOICE_TRANSCRIPTION_1>"));
    }

    // --- Allowlist matching tests ---

    #[tokio::test]
    async fn user_allowed_by_wildcard() {
        let mut cfg = make_config();
        cfg.allowed_users = vec!["*".to_string()];
        let ch = QqChannel::new(cfg);
        assert!(ch.is_user_allowed("anyone").await);
    }

    #[tokio::test]
    async fn user_allowed_by_specific_openid() {
        let mut cfg = make_config();
        cfg.allowed_users = vec!["user123".to_string()];
        let ch = QqChannel::new(cfg);
        assert!(ch.is_user_allowed("user123").await);
        assert!(!ch.is_user_allowed("other").await);
    }

    #[tokio::test]
    async fn empty_allowlist_denies_all() {
        let ch = QqChannel::new(make_config());
        assert!(!ch.is_user_allowed("anyone").await);
    }

    #[tokio::test]
    async fn runtime_allow_overrides_empty_config() {
        let ch = QqChannel::new(make_config());
        ch.runtime_allow("runtime_user").await;
        assert!(ch.is_user_allowed("runtime_user").await);
        assert!(!ch.is_user_allowed("other").await);
    }

    // --- Message dedup tests ---

    #[tokio::test]
    async fn dedup_first_occurrence_returns_false() {
        let ch = QqChannel::new(make_config());
        assert!(!ch.is_duplicate("msg1").await);
    }

    #[tokio::test]
    async fn dedup_second_occurrence_returns_true() {
        let ch = QqChannel::new(make_config());
        assert!(!ch.is_duplicate("msg1").await);
        assert!(ch.is_duplicate("msg1").await);
    }

    #[tokio::test]
    async fn dedup_empty_msg_id_returns_false() {
        let ch = QqChannel::new(make_config());
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[tokio::test]
    async fn dedup_independent_msg_ids() {
        let ch = QqChannel::new(make_config());
        assert!(!ch.is_duplicate("msg_a").await);
        assert!(!ch.is_duplicate("msg_b").await);
        assert!(ch.is_duplicate("msg_a").await);
    }

    // --- Helper function tests ---

    #[test]
    fn marker_kind_image_variants() {
        assert_eq!(
            marker_kind_to_qq_file_type("IMAGE", "/a.png"),
            Some(QqMediaFileType::Image)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("PHOTO", "/a.png"),
            Some(QqMediaFileType::Image)
        );
    }

    #[test]
    fn marker_kind_document_and_file() {
        assert_eq!(
            marker_kind_to_qq_file_type("DOCUMENT", "/a.pdf"),
            Some(QqMediaFileType::File)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("FILE", "/a.zip"),
            Some(QqMediaFileType::File)
        );
    }

    #[test]
    fn marker_kind_voice_native_formats() {
        assert_eq!(
            marker_kind_to_qq_file_type("VOICE", "/a.wav"),
            Some(QqMediaFileType::Voice)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("AUDIO", "/a.mp3"),
            Some(QqMediaFileType::Voice)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("VOICE", "/a.silk"),
            Some(QqMediaFileType::Voice)
        );
    }

    #[test]
    fn marker_kind_voice_non_native_degrades_to_file() {
        assert_eq!(
            marker_kind_to_qq_file_type("VOICE", "/a.ogg"),
            Some(QqMediaFileType::File)
        );
        assert_eq!(
            marker_kind_to_qq_file_type("AUDIO", "/a.flac"),
            Some(QqMediaFileType::File)
        );
    }

    #[test]
    fn html_to_markdown_pre_becomes_code_block() {
        let input = "工具：test\n输入：<pre>hello world</pre>";
        let result = html_to_markdown_for_qq(input);
        assert!(result.contains("```\nhello world\n```"));
        assert!(!result.contains("<pre>"));
    }

    #[test]
    fn html_to_markdown_b_becomes_bold() {
        let input = "<b>重要</b>";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "**重要**");
    }

    #[test]
    fn html_to_markdown_i_becomes_italic() {
        let input = "<i>说明</i>";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "*说明*");
    }

    #[test]
    fn html_to_markdown_code_becomes_backtick() {
        let input = "<code>let x = 1;</code>";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "`let x = 1;`");
    }

    #[test]
    fn html_to_markdown_unescapes_entities() {
        let input = "&lt;script&gt; &amp; text&quot;quote&#39;apos";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "<script> & text\"quote'apos");
    }

    #[test]
    fn html_to_markdown_unknown_tag_stripped() {
        let input = "<div>内容</div>";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "内容");
    }

    #[test]
    fn html_to_markdown_br_becomes_newline() {
        let input = "第一行<br>第二行";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "第一行\n第二行");
    }

    #[test]
    fn html_to_markdown_preserves_chinese() {
        let input = "你好 <b>世界</b>";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "你好 **世界**");
    }

    #[test]
    fn html_to_markdown_nested_pre_inside_b() {
        let input = "<b>外层 <code>内层</code></b>";
        let result = html_to_markdown_for_qq(input);
        assert_eq!(result, "**外层 `内层`**");
    }

    #[test]
    fn html_to_markdown_approval_request_example() {
        let input = "🔒 需要你的确认\n\n工具：channel_send\n输入：<pre>{&quot;value&quot;: &quot;&lt;script&gt;\"}</pre>\n\n请选择一个选项：";
        let result = html_to_markdown_for_qq(input);
        assert!(result.contains("```\n{"));
        assert!(result.contains("<script>"));
        assert!(!result.contains("&lt;"));
        assert!(!result.contains("<pre>"));
    }

    #[test]
    fn next_msg_seq_within_range() {
        for _ in 0..100 {
            let seq = next_msg_seq();
            assert!(seq < 65536);
        }
    }

    // --- resolve_recipient tests ---

    #[test]
    fn resolve_recipient_group_prefix() {
        let (scope, id) = QqChannel::resolve_recipient("group:abc123");
        assert_eq!(scope, "groups");
        assert_eq!(id, "abc123");
    }

    #[test]
    fn resolve_recipient_user_prefix() {
        let (scope, id) = QqChannel::resolve_recipient("user:xyz789");
        assert_eq!(scope, "users");
        assert_eq!(id, "xyz789");
    }

    #[test]
    fn resolve_recipient_bare_id() {
        let (scope, id) = QqChannel::resolve_recipient("raw_id_123");
        assert_eq!(scope, "users");
        assert_eq!(id, "raw_id_123");
    }

    #[test]
    fn resolve_recipient_preserves_hyphen_in_uuid_openid() {
        let (scope, id) = QqChannel::resolve_recipient("user:01912345-6789-7abc-8def-0123456789ab");
        assert_eq!(scope, "users");
        assert_eq!(id, "01912345-6789-7abc-8def-0123456789ab");
    }

    #[test]
    fn resolve_recipient_strips_whitespace_in_bare_id() {
        let (scope, id) = QqChannel::resolve_recipient("user:abc def");
        assert_eq!(scope, "users");
        assert_eq!(id, "abcdef");
    }

    // --- parse_upload_response_body tests ---

    #[test]
    fn parse_upload_response_with_data_wrapper() {
        let raw = r#"{"data": {"file_info": "abc", "ttl": 3600}}"#;
        let parsed = parse_upload_response_body(raw).unwrap();
        assert_eq!(parsed.file_info, "abc");
        assert_eq!(parsed.ttl, Some(3600));
    }

    #[test]
    fn parse_upload_response_top_level() {
        let raw = r#"{"file_info": "xyz", "ttl": "7200"}"#;
        let parsed = parse_upload_response_body(raw).unwrap();
        assert_eq!(parsed.file_info, "xyz");
        assert_eq!(parsed.ttl, Some(7200));
    }

    #[test]
    fn parse_upload_response_missing_file_info_errors() {
        let raw = r#"{"data": {}}"#;
        assert!(parse_upload_response_body(raw).is_err());
    }

    // --- Wiremock integration tests ---

    #[tokio::test]
    async fn send_text_markdown_posts_msg_type_2() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/users/USER123/messages"))
            .and(body_partial_json(serde_json::json!({
                "markdown": { "content": "hello" },
                "msg_type": 2,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_1",
                "channel_id": "USER123"
            })))
            .expect(1)
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
        ch.set_token_for_test("fake_token").await;
        ch.send_text_markdown("user:USER123", "hello")
            .await
            .expect("send_text_markdown");
    }

    #[tokio::test]
    async fn send_media_message_posts_msg_type_7() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/users/USER123/messages"))
            .and(body_partial_json(serde_json::json!({
                "msg_type": 7,
                "media": { "file_info": "fi_abc" },
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock_server)
            .await;

        let ch = QqChannel::new(make_config()).with_api_base(mock_server.uri());
        ch.set_token_for_test("fake_token").await;
        ch.send_media_message("user:USER123", "fi_abc")
            .await
            .expect("send_media_message");
    }

    #[tokio::test]
    async fn send_attachment_rejects_http_url() {
        use crate::channels::traits::{AttachmentKind, ChannelAttachment};

        let ch = QqChannel::new(make_config());
        ch.set_token_for_test("fake_token").await;
        let attachment = ChannelAttachment {
            kind: AttachmentKind::Image,
            target: "http://insecure.example.com/a.png".to_string(),
        };
        let result = ch.send_attachment("user:USER123", &attachment).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("must be HTTPS"),
            "expected HTTPS error, got: {err}"
        );
    }
}
