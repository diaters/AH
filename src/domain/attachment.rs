//! 消息附件领域类型
//!
//! 附件标记解析：`[IMAGE:path]` 等标记从消息内容中提取为结构化附件。

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

/// 附件 marker 语法的 LLM 说明文本（单点维护）。
///
/// 所有向 LLM 解释 `[IMAGE:path]` 等标记语法的 prompt 片段都引用本常量，
/// 与 `extract_attachments` 的解析实现保持同一出处，避免说明与解析脱节。
pub const ATTACHMENT_MARKER_SYNTAX_HINT: &str = "Use markers like [IMAGE:/path/to/file.png], [DOCUMENT:/path/to/file.pdf], [VIDEO:...], [AUDIO:...], [VOICE:...]. The target path may be relative or absolute, a file:// URL, or an HTTP(S) URL.";

/// 从内容中解析附件标记，返回剩余文本与附件列表。
///
/// 支持的标记：`[IMAGE:path]`、`[DOCUMENT:path]`、`[VIDEO:path]`、
/// `[AUDIO:path]`、`[VOICE:path]`。路径前后空格会被裁剪。
pub fn extract_attachments(content: &str) -> (String, Vec<ChannelAttachment>) {
    let mut attachments = vec![];
    let mut text = String::new();
    let mut last_end = 0;
    let bytes = content.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(close) = content[i + 1..].find(']')
        {
            let close = close + i + 1;
            let inner = &content[i + 1..close];
            if let Some((kind_str, target)) = inner.split_once(':') {
                let kind = match kind_str.to_uppercase().as_str() {
                    "IMAGE" => Some(AttachmentKind::Image),
                    "DOCUMENT" => Some(AttachmentKind::Document),
                    "VIDEO" => Some(AttachmentKind::Video),
                    "AUDIO" => Some(AttachmentKind::Audio),
                    "VOICE" => Some(AttachmentKind::Voice),
                    _ => None,
                };
                if let Some(kind) = kind {
                    text.push_str(&content[last_end..i]);
                    attachments.push(ChannelAttachment {
                        kind,
                        target: target.trim().to_string(),
                    });
                    last_end = close + 1;
                    i = close + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    text.push_str(&content[last_end..]);
    (text, attachments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_image_marker() {
        let (text, attachments) = extract_attachments("a [IMAGE: /tmp/a.png ] b");
        assert_eq!(text, "a  b");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, AttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/a.png");
    }

    #[test]
    fn keeps_plain_text_untouched() {
        let (text, attachments) = extract_attachments("hello world");
        assert_eq!(text, "hello world");
        assert!(attachments.is_empty());
    }
}
