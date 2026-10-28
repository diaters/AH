//! QQ 纯图片消息合并
//!
//! QQ 移动端每张图片单独作为一条消息发出，随后才是文字。纯图片消息先按
//! `(recipient, sender)` 缓存，等同一发送者的下一条常规消息到达后合并为一条入站消息。

use std::collections::HashMap;

use crate::domain::{AttachmentKind, ChannelAttachment, extract_attachments};

/// 每个会话发送者最多缓存的图片数，超出时丢弃最早一张。
pub(super) const MAX_PENDING_IMAGES: usize = 20;

#[derive(Debug, Default)]
pub(super) struct PendingImages {
    buffers: HashMap<(String, String), Vec<String>>,
}

impl PendingImages {
    /// 若 `content` 是纯图片消息（去掉标记后无文字且附件全为图片），缓存其图片标记并返回 true。
    pub(super) fn try_buffer(&mut self, recipient: &str, sender: &str, content: &str) -> bool {
        let (text, attachments) = extract_attachments(content);
        let image_only = text.trim().is_empty()
            && !attachments.is_empty()
            && attachments.iter().all(|a| a.kind == AttachmentKind::Image);
        if !image_only {
            return false;
        }

        let buffer = self
            .buffers
            .entry((recipient.to_string(), sender.to_string()))
            .or_default();
        buffer.extend(attachments.iter().map(ChannelAttachment::to_marker));
        if buffer.len() > MAX_PENDING_IMAGES {
            let overflow = buffer.len() - MAX_PENDING_IMAGES;
            let dropped: Vec<String> = buffer.drain(..overflow).collect();
            tracing::warn!(
                event = "QqPendingImageDropped",
                recipient = %recipient,
                sender = %sender,
                dropped = ?dropped,
                "pending image buffer full, dropped oldest images"
            );
        }
        true
    }

    /// 把缓存图片合并进常规消息内容并清空缓存；无缓存时原样返回。
    ///
    /// 格式：`<文字>\n<缓存图片标记...>\n<本条附件标记...>`，按到达顺序排列。
    pub(super) fn merge_into(&mut self, recipient: &str, sender: &str, content: &str) -> String {
        let Some(buffered) = self
            .buffers
            .remove(&(recipient.to_string(), sender.to_string()))
        else {
            return content.to_string();
        };

        let (text, attachments) = extract_attachments(content);
        let markers = buffered
            .into_iter()
            .chain(attachments.iter().map(ChannelAttachment::to_marker));
        std::iter::once(text.trim().to_string())
            .filter(|t| !t.is_empty())
            .chain(markers)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: &str = "user:u1";

    #[test]
    fn single_image_merges_with_next_text() {
        let mut pending = PendingImages::default();
        assert!(pending.try_buffer(CHAT, "u1", "[IMAGE:/f/a.png]"));

        let merged = pending.merge_into(CHAT, "u1", "这是什么");

        assert_eq!(merged, "这是什么\n[IMAGE:/f/a.png]");
        assert_eq!(
            pending.merge_into(CHAT, "u1", "下一条"),
            "下一条",
            "合并后清空"
        );
    }

    #[test]
    fn multiple_images_keep_arrival_order_before_own_attachments() {
        let mut pending = PendingImages::default();
        assert!(pending.try_buffer(CHAT, "u1", "[IMAGE:/f/1.png]"));
        assert!(pending.try_buffer(CHAT, "u1", "[IMAGE:/f/2.png]"));

        let merged = pending.merge_into(CHAT, "u1", "对比一下\n[DOCUMENT:/f/3.pdf]");

        assert_eq!(
            merged,
            "对比一下\n[IMAGE:/f/1.png]\n[IMAGE:/f/2.png]\n[DOCUMENT:/f/3.pdf]"
        );
    }

    #[test]
    fn image_with_text_or_other_attachments_is_not_buffered() {
        let mut pending = PendingImages::default();
        assert!(!pending.try_buffer(CHAT, "u1", "看这个\n[IMAGE:/f/a.png]"));
        assert!(!pending.try_buffer(CHAT, "u1", "[DOCUMENT:/f/a.pdf]"));
        assert!(!pending.try_buffer(
            CHAT,
            "u1",
            "<VOICE_TRANSCRIPTION>你好</VOICE_TRANSCRIPTION>\n[VOICE:/f/a.wav]"
        ));
        assert!(!pending.try_buffer(CHAT, "u1", "纯文字"));
    }

    #[test]
    fn group_members_are_isolated() {
        let mut pending = PendingImages::default();
        let group = "group:g1";
        assert!(pending.try_buffer(group, "alice", "[IMAGE:/f/a.png]"));
        assert!(pending.try_buffer(group, "bob", "[IMAGE:/f/b.png]"));

        assert_eq!(
            pending.merge_into(group, "bob", "bob 的问题"),
            "bob 的问题\n[IMAGE:/f/b.png]"
        );
        assert_eq!(
            pending.merge_into(group, "alice", "alice 的问题"),
            "alice 的问题\n[IMAGE:/f/a.png]"
        );
    }

    #[test]
    fn overflow_drops_oldest_images() {
        let mut pending = PendingImages::default();
        for i in 0..=MAX_PENDING_IMAGES {
            assert!(pending.try_buffer(CHAT, "u1", &format!("[IMAGE:/f/{i}.png]")));
        }

        let merged = pending.merge_into(CHAT, "u1", "q");

        assert!(!merged.contains("[IMAGE:/f/0.png]"));
        assert!(merged.contains(&format!("[IMAGE:/f/{MAX_PENDING_IMAGES}.png]")));
        assert_eq!(merged.matches("[IMAGE:").count(), MAX_PENDING_IMAGES);
    }
}
