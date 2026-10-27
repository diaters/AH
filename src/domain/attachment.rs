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

impl ChannelAttachment {
    /// 渲染为 `[KIND:target]` 标记，是 `extract_attachments` 的逆操作。
    pub fn to_marker(&self) -> String {
        let kind = match self.kind {
            AttachmentKind::Image => "IMAGE",
            AttachmentKind::Document => "DOCUMENT",
            AttachmentKind::Video => "VIDEO",
            AttachmentKind::Audio => "AUDIO",
            AttachmentKind::Voice => "VOICE",
        };
        format!("[{kind}:{}]", self.target)
    }
}

/// 附件 marker 语法的 LLM 说明文本（单点维护）。
///
/// 所有向 LLM 解释 `[IMAGE:path]` 等标记语法的 prompt 片段都引用本常量，
/// 与 `extract_attachments` 的解析实现保持同一出处，避免说明与解析脱节。
pub const ATTACHMENT_MARKER_SYNTAX_HINT: &str = "Use markers like [IMAGE:/path/to/file.png], [DOCUMENT:/path/to/file.pdf], [VIDEO:...], [AUDIO:...], [VOICE:...]. The target path may be relative or absolute, a file:// URL, or an HTTP(S) URL.";

/// 发给模型的单张图片体积上限（取主流 provider 的保守下限）。
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// 按扩展名判定模型可接受的图片 MIME 类型；不在白名单内返回 `None`。
///
/// 白名单为主流视觉模型共同支持的 png / jpeg / gif / webp。URL 会先去掉查询串。
pub fn image_mime_type(target: &str) -> Option<&'static str> {
    let path = target.split(['?', '#']).next().unwrap_or(target);
    let ext = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

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
    fn to_marker_round_trips_through_extract() {
        let content = "[IMAGE:/a.png][VOICE:/b.wav][DOCUMENT:https://x/c.pdf]";
        let (_, attachments) = extract_attachments(content);
        let rendered: String = attachments
            .iter()
            .map(ChannelAttachment::to_marker)
            .collect();
        assert_eq!(rendered, content);
    }

    #[test]
    fn image_mime_type_accepts_whitelist_only() {
        assert_eq!(image_mime_type("/tmp/a.PNG"), Some("image/png"));
        assert_eq!(image_mime_type("b.jpeg"), Some("image/jpeg"));
        assert_eq!(
            image_mime_type("https://x.com/c.webp?sig=1"),
            Some("image/webp")
        );
        assert_eq!(image_mime_type("/tmp/d.bmp"), None);
        assert_eq!(image_mime_type("/tmp/noext"), None);
    }

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
