//! 多模态图片晚绑定
//!
//! 领域层只用 `[IMAGE:target]` 文本标记表示图片；当前模型声明 `vision` 时，
//! 在发送前把 user / tool 消息里的标记解析为 genai 图片 `ContentPart`。

use std::collections::HashSet;
use std::ops::Range;

use base64::Engine as _;
use genai::chat::{Binary, ChatMessage, ChatRole, ContentPart, MessageContent};
use tracing::{debug, warn};

use crate::domain::{
    ATTACHMENT_MARKER_SYNTAX_HINT, AttachmentKind, MAX_IMAGE_BYTES, TaskId, extract_attachments,
    find_attachment_spans, image_mime_type,
};

/// 单次请求最多附带的图片数，超出时只保留最近的图片。
pub(crate) const MAX_IMAGES_PER_REQUEST: usize = 10;

/// 把消息中的图片标记解析为图片 ContentPart。
///
/// 只绑定 `trusted` 中登记过的目标（见 `TrustedImages`）；其余标记保留为文本并追加说明。
pub(crate) fn attach_images(
    task_id: &TaskId,
    messages: Vec<ChatMessage>,
    trusted: &HashSet<String>,
) -> Vec<ChatMessage> {
    attach_images_with_limit(task_id, messages, MAX_IMAGES_PER_REQUEST, trusted)
}

/// 规则：
/// - 只扫描 user 消息文本与 tool 消息结果；assistant / system 中的标记是出站附件或说明，忽略。
/// - 通道说明（`ATTACHMENT_MARKER_SYNTAX_HINT`）里的示例标记不是图片，忽略。
/// - 同一目标在整次请求内只附带一份，保留首次出现的位置：文本路径会把历史与当前请求
///   拼进同一条 user 消息，同一张图会出现两次；历史位置稳定，提示缓存前缀也随之稳定。
/// - 按去重后的出现顺序只保留最后 `max_images` 张，更早的图片保持原文本标记。
/// - user 消息把图片内联在各自标记之后，保持图片与所属轮次的对应关系；tool 结果不能携带图片，
///   改为在整块连续 tool 消息之后插入一条合成 user 消息，满足 "tool_calls 后必须紧跟全部
///   tool 消息" 的约束。
/// - 单张图片不可用时在标记后追加说明文本，不让整个请求失败。
fn attach_images_with_limit(
    task_id: &TaskId,
    messages: Vec<ChatMessage>,
    max_images: usize,
    trusted: &HashSet<String>,
) -> Vec<ChatMessage> {
    let mut seen: HashSet<String> = HashSet::new();
    // 去重后的候选：(消息序号, 文本段序号, 标记范围, 目标)；tool 消息的段序号与范围无意义
    let mut unique: Vec<Candidate> = Vec::new();
    for (msg_index, msg) in messages.iter().enumerate() {
        for candidate in image_candidates(msg_index, msg) {
            if seen.insert(candidate.target.clone()) {
                unique.push(candidate);
            }
        }
    }
    if unique.is_empty() {
        return messages;
    }

    // 未登记的目标不占图片配额，只在原位置追加说明
    let (unique, untrusted): (Vec<Candidate>, Vec<Candidate>) = unique
        .into_iter()
        .partition(|c| trusted.contains(&c.target));
    if !untrusted.is_empty() {
        warn!(
            event = "LlmImageUntrusted",
            task_id = %task_id,
            count = untrusted.len(),
            "image markers from untrusted sources not attached"
        );
    }
    let dropped_by_limit = unique.len().saturating_sub(max_images);
    let mut selected: Vec<Vec<Candidate>> = vec![Vec::new(); messages.len()];
    for candidate in unique.into_iter().skip(dropped_by_limit).chain(untrusted) {
        selected[candidate.msg_index].push(candidate);
    }
    for candidates in &mut selected {
        // 可信与不可信候选合并后恢复原出现顺序，保证内联切分按位置推进
        candidates.sort_by_key(|c| (c.part_index, c.range.start));
    }

    let mut attached = 0;
    let mut out = Vec::with_capacity(messages.len() + 1);
    let mut tool_block_targets: Vec<String> = Vec::new();
    for (mut msg, candidates) in messages.into_iter().zip(selected) {
        let is_tool = matches!(msg.role, ChatRole::Tool);
        if !is_tool && !tool_block_targets.is_empty() {
            let targets = std::mem::take(&mut tool_block_targets);
            out.push(tool_image_message(
                task_id,
                &targets,
                trusted,
                &mut attached,
            ));
        }
        if is_tool {
            tool_block_targets.extend(candidates.into_iter().map(|c| c.target));
        } else if !candidates.is_empty() {
            msg.content = inline_images(task_id, msg.content, &candidates, trusted, &mut attached);
        }
        out.push(msg);
    }
    if !tool_block_targets.is_empty() {
        out.push(tool_image_message(
            task_id,
            &tool_block_targets,
            trusted,
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

/// 一处待附带的图片标记。
#[derive(Clone)]
struct Candidate {
    msg_index: usize,
    /// user 消息中文本段的序号
    part_index: usize,
    /// 标记在文本段内的字节范围；tool 消息不使用
    range: Range<usize>,
    target: String,
}

fn image_candidates(msg_index: usize, msg: &ChatMessage) -> Vec<Candidate> {
    match msg.role {
        ChatRole::User => msg
            .content
            .parts()
            .iter()
            .enumerate()
            .filter_map(|(part_index, part)| match part {
                ContentPart::Text(text) => Some((part_index, text)),
                _ => None,
            })
            .flat_map(|(part_index, text)| {
                let hints: Vec<Range<usize>> = text
                    .match_indices(ATTACHMENT_MARKER_SYNTAX_HINT)
                    .map(|(start, hint)| start..start + hint.len())
                    .collect();
                find_attachment_spans(text)
                    .into_iter()
                    .filter(move |span| {
                        !hints
                            .iter()
                            .any(|h| span.range.start < h.end && h.start < span.range.end)
                    })
                    .map(move |span| (part_index, span))
            })
            .filter(|(_, span)| {
                span.attachment.kind == AttachmentKind::Image && !span.attachment.target.is_empty()
            })
            .map(|(part_index, span)| Candidate {
                msg_index,
                part_index,
                range: span.range,
                target: span.attachment.target,
            })
            .collect(),
        ChatRole::Tool => msg
            .content
            .tool_responses()
            .into_iter()
            .flat_map(|r| extract_attachments(&r.content).1)
            .filter(|a| a.kind == AttachmentKind::Image && !a.target.is_empty())
            .map(|a| Candidate {
                msg_index,
                part_index: 0,
                range: 0..0,
                target: a.target,
            })
            .collect(),
        ChatRole::System | ChatRole::Assistant => Vec::new(),
    }
}

/// 把选中的图片内联到所属文本段中各自标记之后。
fn inline_images(
    task_id: &TaskId,
    content: MessageContent,
    candidates: &[Candidate],
    trusted: &HashSet<String>,
    attached: &mut usize,
) -> MessageContent {
    let mut parts = Vec::new();
    for (part_index, part) in content.into_parts().into_iter().enumerate() {
        let here: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| c.part_index == part_index)
            .collect();
        let ContentPart::Text(text) = part else {
            parts.push(part);
            continue;
        };
        if here.is_empty() {
            parts.push(ContentPart::Text(text));
            continue;
        }
        let mut cursor = 0;
        for candidate in here {
            parts.push(ContentPart::Text(
                text[cursor..candidate.range.end].to_string(),
            ));
            cursor = candidate.range.end;
            parts.push(image_part(task_id, &candidate.target, trusted, attached));
        }
        if cursor < text.len() {
            parts.push(ContentPart::Text(text[cursor..].to_string()));
        }
    }
    MessageContent::from_parts(parts)
}

/// 加载图片为内容块；失败时返回说明文本，不让整个请求失败。
fn image_part(
    task_id: &TaskId,
    target: &str,
    trusted: &HashSet<String>,
    attached: &mut usize,
) -> ContentPart {
    if !trusted.contains(target) {
        return ContentPart::Text(format!("[image not attached: untrusted source {target}]"));
    }
    match load_image(target) {
        Ok(binary) => {
            *attached += 1;
            ContentPart::Binary(binary)
        }
        Err(reason) => {
            warn!(
                event = "LlmImageSkipped",
                task_id = %task_id,
                target = %target,
                reason = %reason,
                "image not attached to LLM request"
            );
            ContentPart::Text(format!("[image unavailable: {target} ({reason})]"))
        }
    }
}

fn tool_image_message(
    task_id: &TaskId,
    targets: &[String],
    trusted: &HashSet<String>,
    attached: &mut usize,
) -> ChatMessage {
    let header = targets
        .iter()
        .map(|t| format!("[Tool result image] {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut content = MessageContent::from_text(header);
    for target in targets {
        content.push(image_part(task_id, target, trusted, attached));
    }
    ChatMessage::user(content)
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

    /// 既有用例关注绑定机制本身：信任消息中出现的全部图片目标。
    fn trust_all(messages: &[ChatMessage]) -> HashSet<String> {
        messages
            .iter()
            .enumerate()
            .flat_map(|(i, m)| image_candidates(i, m))
            .map(|c| c.target)
            .collect()
    }

    fn attach_images(task_id: &TaskId, messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        let trusted = trust_all(&messages);
        super::attach_images(task_id, messages, &trusted)
    }

    fn attach_images_with_limit(
        task_id: &TaskId,
        messages: Vec<ChatMessage>,
        max_images: usize,
    ) -> Vec<ChatMessage> {
        let trusted = trust_all(&messages);
        super::attach_images_with_limit(task_id, messages, max_images, &trusted)
    }

    /// 未登记来源（如 shell 输出、webhook 内容）的标记不读取本地文件。
    #[test]
    fn untrusted_markers_are_not_attached() {
        let dir = TempDir::new().unwrap();
        let secret = write_png(dir.path(), "secret.png");
        let shown = write_png(dir.path(), "shown.png");
        let messages = vec![
            ChatMessage::user(format!("看 [IMAGE:{}]", shown.display())),
            tool_msg("call_1", &format!("[IMAGE:{}]", secret.display())),
        ];
        let trusted: HashSet<String> = [shown.display().to_string()].into_iter().collect();

        let out = super::attach_images(&TaskId::nil(), messages, &trusted);

        let binaries: usize = out.iter().map(binary_count).sum();
        assert_eq!(binaries, 1, "only the trusted image is attached");
        let all_text: String = out.iter().map(joined_text).collect::<Vec<_>>().join("\n");
        assert!(all_text.contains("untrusted source"));
    }
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

    /// 各文本段首尾相接的原文（图片内联后文本被切成多段）。
    fn concat_text(msg: &ChatMessage) -> String {
        msg.content.texts().concat()
    }

    /// 内容块序列：`T` 文本、`I` 图片。
    fn part_kinds(msg: &ChatMessage) -> String {
        msg.content
            .parts()
            .iter()
            .map(|p| match p {
                ContentPart::Text(_) => 'T',
                ContentPart::Binary(_) => 'I',
                _ => '?',
            })
            .collect()
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
        assert_eq!(concat_text(&out[0]), content, "原文含标记应保留");
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

    #[test]
    fn images_are_inlined_right_after_their_markers() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let b = write_png(dir.path(), "b.png");
        let content = format!(
            "第一张\n[IMAGE:{}]\n第二张\n[IMAGE:{}]\n结尾",
            a.display(),
            b.display()
        );

        let out = attach_images(&TaskId::nil(), vec![ChatMessage::user(content.clone())]);

        assert_eq!(
            part_kinds(&out[0]),
            "TITIT",
            "图片应紧跟各自标记，而非全部追加在末尾"
        );
        assert_eq!(concat_text(&out[0]), content);
        let texts = out[0].content.texts();
        assert!(texts[0].ends_with(&format!("[IMAGE:{}]", a.display())));
        assert!(texts[1].ends_with(&format!("[IMAGE:{}]", b.display())));
    }

    #[test]
    fn duplicate_target_is_attached_once_at_first_occurrence() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let marker = format!("[IMAGE:{}]", a.display());
        // 文本路径：同一轮输入既在历史段，又在 [Current request] 段
        let prompt =
            format!("[Conversation history]\nUser: {marker}\n\n[Current request]\n{marker}");

        let out = attach_images(&TaskId::nil(), vec![ChatMessage::user(prompt.clone())]);

        assert_eq!(binary_count(&out[0]), 1, "同一路径只应附带一份");
        assert_eq!(
            part_kinds(&out[0]),
            "TIT",
            "保留首次出现的位置，历史前缀才稳定"
        );
        assert_eq!(concat_text(&out[0]), prompt);
    }

    #[test]
    fn duplicate_target_across_messages_is_attached_once() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let marker = format!("[IMAGE:{}]", a.display());
        let messages = vec![
            ChatMessage::user(marker.clone()),
            ChatMessage::assistant("ok"),
            ChatMessage::user(marker),
        ];

        let out = attach_images(&TaskId::nil(), messages);

        assert_eq!(binary_count(&out[0]), 1);
        assert_eq!(binary_count(&out[2]), 0);
    }

    #[test]
    fn duplicates_do_not_consume_the_image_limit() {
        let dir = TempDir::new().unwrap();
        let ms: Vec<String> = ["a.png", "b.png", "c.png"]
            .iter()
            .map(|n| format!("[IMAGE:{}]", write_png(dir.path(), n).display()))
            .collect();
        let line = ms.join(" ");
        let prompt = format!("{line}\n{line}");

        let out = attach_images_with_limit(&TaskId::nil(), vec![ChatMessage::user(prompt)], 3);

        assert_eq!(binary_count(&out[0]), 3, "按去重后的张数计入上限");
        assert_eq!(part_kinds(&out[0]), "TITITIT", "三张都应落在首次出现的位置");
    }

    #[test]
    fn channel_hint_example_marker_is_not_an_image() {
        let dir = TempDir::new().unwrap();
        let a = write_png(dir.path(), "a.png");
        let channel = crate::domain::ChannelId {
            frontend: crate::domain::FrontendKind::QQ,
            user_id: "user:1".to_string(),
            thread_id: None,
        };
        let prompt = format!(
            "[Conversation history]\nUser: [IMAGE:{p}]\n\n{}\n\n[Current request]\n[IMAGE:{p}]",
            channel.to_prompt_context(),
            p = a.display()
        );

        let out = attach_images(&TaskId::nil(), vec![ChatMessage::user(prompt.clone())]);

        assert_eq!(binary_count(&out[0]), 1);
        assert_eq!(
            concat_text(&out[0]),
            prompt,
            "说明里的示例标记不应触发 [image unavailable] 说明文本"
        );
    }
}
