//! 多模态图片晚绑定
//!
//! 领域层只用 `[IMAGE:target]` 文本标记表示图片；当前模型声明 `vision` 时，
//! 在发送前把 user / tool 消息里的标记解析为 genai 图片 `ContentPart`。

use base64::Engine as _;
use genai::chat::{Binary, ChatMessage, ChatRole, ContentPart, MessageContent};
use tracing::{debug, warn};

use crate::domain::{
    AttachmentKind, MAX_IMAGE_BYTES, TaskId, extract_attachments, image_mime_type,
};

/// 单次请求最多附带的图片数，超出时只保留最近的图片。
pub(crate) const MAX_IMAGES_PER_REQUEST: usize = 10;

/// 把消息中的图片标记解析为图片 ContentPart。
pub(crate) fn attach_images(task_id: &TaskId, messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    attach_images_with_limit(task_id, messages, MAX_IMAGES_PER_REQUEST)
}

/// 规则：
/// - 只扫描 user 消息文本与 tool 消息结果；assistant / system 中的标记是出站附件或说明，忽略。
/// - 按出现顺序只保留最后 `max_images` 张，更早的图片保持原文本标记。
/// - user 消息保留原文，在其后追加图片；tool 结果不能携带图片，改为在整块连续
///   tool 消息之后插入一条合成 user 消息，满足 "tool_calls 后必须紧跟全部 tool 消息" 的约束。
/// - 单张图片不可用时追加说明文本，不让整个请求失败。
fn attach_images_with_limit(
    task_id: &TaskId,
    messages: Vec<ChatMessage>,
    max_images: usize,
) -> Vec<ChatMessage> {
    let found: Vec<(usize, String)> = messages
        .iter()
        .enumerate()
        .flat_map(|(index, msg)| image_targets(msg).into_iter().map(move |t| (index, t)))
        .collect();
    if found.is_empty() {
        return messages;
    }

    let dropped_by_limit = found.len().saturating_sub(max_images);
    let mut targets_by_message: Vec<Vec<String>> = vec![Vec::new(); messages.len()];
    for (index, target) in found.into_iter().skip(dropped_by_limit) {
        targets_by_message[index].push(target);
    }

    let mut attached = 0;
    let mut out = Vec::with_capacity(messages.len() + 1);
    let mut tool_block_targets: Vec<String> = Vec::new();
    for (mut msg, targets) in messages.into_iter().zip(targets_by_message) {
        let is_tool = matches!(msg.role, ChatRole::Tool);
        if !is_tool && !tool_block_targets.is_empty() {
            let targets = std::mem::take(&mut tool_block_targets);
            out.push(tool_image_message(task_id, &targets, &mut attached));
        }
        if is_tool {
            tool_block_targets.extend(targets);
        } else {
            append_images(task_id, &mut msg.content, &targets, &mut attached);
        }
        out.push(msg);
    }
    if !tool_block_targets.is_empty() {
        out.push(tool_image_message(
            task_id,
            &tool_block_targets,
            &mut attached,
        ));
    }

    debug!(
        event = "LlmImagesAttached",
        task_id = %task_id,
        attached,
        dropped_by_limit,
        "images attached to LLM request"
    );
    out
}

fn image_targets(msg: &ChatMessage) -> Vec<String> {
    let texts: Vec<&str> = match msg.role {
        ChatRole::User => msg.content.texts(),
        ChatRole::Tool => msg
            .content
            .tool_responses()
            .into_iter()
            .map(|r| r.content.as_str())
            .collect(),
        ChatRole::System | ChatRole::Assistant => return Vec::new(),
    };
    texts
        .into_iter()
        .flat_map(|text| extract_attachments(text).1)
        .filter(|a| a.kind == AttachmentKind::Image && !a.target.is_empty())
        .map(|a| a.target)
        .collect()
}

fn tool_image_message(task_id: &TaskId, targets: &[String], attached: &mut usize) -> ChatMessage {
    let header = targets
        .iter()
        .map(|t| format!("[Tool result image] {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut content = MessageContent::from_text(header);
    append_images(task_id, &mut content, targets, attached);
    ChatMessage::user(content)
}

fn append_images(
    task_id: &TaskId,
    content: &mut MessageContent,
    targets: &[String],
    attached: &mut usize,
) {
    for target in targets {
        match load_image(target) {
            Ok(binary) => {
                content.push(binary);
                *attached += 1;
            }
            Err(reason) => {
                warn!(
                    event = "LlmImageSkipped",
                    task_id = %task_id,
                    target = %target,
                    reason = %reason,
                    "image not attached to LLM request"
                );
                content.push(ContentPart::Text(format!(
                    "[image unavailable: {target} ({reason})]"
                )));
            }
        }
    }
}

/// 本地路径 / `file://` 读取为 base64；HTTP(S) URL 交给 provider 拉取。
fn load_image(target: &str) -> Result<Binary, String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        // QQ 等平台的临时 URL 常无扩展名，content_type 对 URL 来源仅作提示
        let mime = image_mime_type(target).unwrap_or("image/jpeg");
        return Ok(Binary::from_url(mime, target, None));
    }

    let path = target.strip_prefix("file://").unwrap_or(target);
    let mime = image_mime_type(path).ok_or("unsupported image format")?;
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read file: {e}"))?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "larger than {} MiB",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read file: {e}"))?;
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string);
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Binary::from_base64(mime, encoded, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use genai::chat::ToolResponse;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn write_png(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"\x89PNG fake").unwrap();
        path
    }

    fn binary_count(msg: &ChatMessage) -> usize {
        msg.content.binaries().len()
    }

    fn joined_text(msg: &ChatMessage) -> String {
        msg.content.texts().join("\n")
    }

    fn tool_msg(id: &str, content: &str) -> ChatMessage {
        ChatMessage::tool(ToolResponse::new(id, content))
    }

    #[test]
    fn messages_without_markers_are_unchanged() {
        let messages = vec![ChatMessage::user("hello")];
        let out = attach_images(&TaskId::nil(), messages);
        assert_eq!(out.len(), 1);
        assert_eq!(binary_count(&out[0]), 0);
        assert_eq!(joined_text(&out[0]), "hello");
    }

    #[test]
    fn user_message_markers_become_images_and_keep_text() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let b = write_png(dir.path(), "b.png");
        let content = format!(
            "看看这两张\n[IMAGE:{}]\n[IMAGE:{}]",
            a.display(),
            b.display()
        );

        let out = attach_images(&TaskId::nil(), vec![ChatMessage::user(content.clone())]);

        assert_eq!(out.len(), 1);
        assert_eq!(binary_count(&out[0]), 2);
        assert_eq!(joined_text(&out[0]), content, "原文含标记应保留");
        let bin = &out[0].content.binaries()[0];
        assert_eq!(bin.content_type, "image/png");
    }

    #[test]
    fn only_latest_images_within_limit_are_attached() {
        let dir = TempDir::new().unwrap();
        let old = write_png(dir.path(), "old.png");
        let new1 = write_png(dir.path(), "new1.png");
        let new2 = write_png(dir.path(), "new2.png");
        let messages = vec![
            ChatMessage::user(format!("[IMAGE:{}]", old.display())),
            ChatMessage::assistant("ok"),
            ChatMessage::user(format!(
                "[IMAGE:{}] [IMAGE:{}]",
                new1.display(),
                new2.display()
            )),
        ];

        let out = attach_images_with_limit(&TaskId::nil(), messages, 2);

        assert_eq!(binary_count(&out[0]), 0, "超出上限的早期图片保持文本标记");
        assert_eq!(binary_count(&out[2]), 2);
    }

    #[test]
    fn tool_images_are_appended_after_whole_tool_block() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let messages = vec![
            ChatMessage::user("截个图看看"),
            ChatMessage::assistant("calling"),
            tool_msg(
                "call_1",
                &format!(r#"{{"image":"[IMAGE:{}]"}}"#, a.display()),
            ),
            tool_msg("call_2", "plain output"),
            ChatMessage::assistant("done"),
        ];

        let out = attach_images(&TaskId::nil(), messages);

        assert_eq!(out.len(), 6);
        assert!(matches!(out[2].role, ChatRole::Tool));
        assert!(matches!(out[3].role, ChatRole::Tool));
        assert!(
            matches!(out[4].role, ChatRole::User),
            "合成 user 消息应在整块 tool 之后"
        );
        assert_eq!(binary_count(&out[4]), 1);
        assert!(joined_text(&out[4]).contains(&a.display().to_string()));
        assert!(matches!(out[5].role, ChatRole::Assistant));
    }

    #[test]
    fn trailing_tool_block_gets_image_message_at_end() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let messages = vec![
            ChatMessage::assistant("calling"),
            tool_msg("call_1", &format!("[IMAGE:{}]", a.display())),
        ];

        let out = attach_images(&TaskId::nil(), messages);

        assert_eq!(out.len(), 3);
        assert!(matches!(out[2].role, ChatRole::User));
        assert_eq!(binary_count(&out[2]), 1);
    }

    #[test]
    fn unusable_images_degrade_to_text_note() {
        let dir = TempDir::new().unwrap();
        let big = dir.path().join("big.png");
        std::fs::write(
            &big,
            vec![0u8; (crate::domain::MAX_IMAGE_BYTES + 1) as usize],
        )
        .unwrap();
        let bmp = dir.path().join("x.bmp");
        std::fs::write(&bmp, b"BM").unwrap();
        let content = format!(
            "[IMAGE:/nonexistent/a.png] [IMAGE:{}] [IMAGE:{}] [IMAGE:]",
            big.display(),
            bmp.display()
        );

        let out = attach_images(&TaskId::nil(), vec![ChatMessage::user(content)]);

        assert_eq!(binary_count(&out[0]), 0);
        let text = joined_text(&out[0]);
        assert_eq!(text.matches("[image unavailable:").count(), 3, "{text}");
    }

    #[test]
    fn file_url_and_http_url_are_supported() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let content = format!(
            "[IMAGE:file://{}] [IMAGE:https://example.com/pic.jpg?x=1]",
            a.display()
        );

        let out = attach_images(&TaskId::nil(), vec![ChatMessage::user(content)]);

        let bins = out[0].content.binaries();
        assert_eq!(bins.len(), 2);
        assert!(bins[1].is_image());
        assert!(
            matches!(&bins[1].source, genai::chat::BinarySource::Url(u) if u == "https://example.com/pic.jpg?x=1")
        );
    }

    #[test]
    fn assistant_and_system_markers_are_ignored() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let marker = format!("[IMAGE:{}]", a.display());
        let messages = vec![
            ChatMessage::system(marker.clone()),
            ChatMessage::assistant(marker),
        ];

        let out = attach_images(&TaskId::nil(), messages);

        assert!(out.iter().all(|m| binary_count(m) == 0));
    }
}
