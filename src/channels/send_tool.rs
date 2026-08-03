use anyhow::Result;
use serde_json::{Value, json};

use crate::channels::traits::extract_attachments;
use crate::domain::{ToolAction, ToolError, ToolPermission, ToolSchema};

const ATTACHMENT_HINT: &str = r#"
Prefer plain text responses over this tool: the system automatically routes your text replies back to the source IM channel of the current task. Only use this tool when:
1. The user explicitly asks you to send files, images, videos, audio, or other attachments. Use markers like [IMAGE:/path/to/file.png], [DOCUMENT:/path/to/file.pdf], [VIDEO:...], [AUDIO:...], [VOICE:...]. The target path may be relative or absolute, a file:// URL, or an HTTP(S) URL. Unsupported attachment types will be sent as plain text links by the channel implementation.
2. You need to send a message to a different channel than the current task's source channel.

The `target` can be omitted; the system will route to the task's routing channel (the output channel configured for the task, falling back to its source conversation).
"#;

pub struct ChannelSendTool;

impl ChannelSendTool {
    pub fn definition() -> crate::domain::ToolDefinition {
        crate::domain::ToolDefinition {
            name: "channel_send".to_string(),
            description: format!(
                "向指定 IM 通道（telegram/qq/feishu）发送消息。若省略 target，则发送到当前任务的路由通道（任务配置的输出通道，回退到来源会话）。{}",
                ATTACHMENT_HINT
            ),
            parameters: ToolSchema {
                schema: json!({
                    "type": "object",
                    "properties": {
                        "channel": {
                            "type": "string",
                            "enum": ["telegram", "qq", "feishu"],
                            "description": "通道名称"
                        },
                        "target": {
                            "type": "string",
                            "description": "目标 chat_id / open_id / user_id；省略时回复到当前任务的路由通道"
                        },
                        "content": {
                            "type": "string",
                            "description": "要发送的内容"
                        }
                    },
                    "required": ["channel", "content"]
                }),
            },
            default_permission: ToolPermission::Confirm,
            executor: crate::domain::ToolExecutorKind::Builtin("channel_send".to_string()),
            required_tag: None,
        }
    }
}

impl crate::domain::BuiltinTool for ChannelSendTool {
    fn name(&self) -> &str {
        "channel_send"
    }

    fn execute(
        &self,
        input: &Value,
        _ctx: &crate::domain::ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let channel = input
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing channel".into()))?;
        let target = input
            .get("target")
            .and_then(|v| v.as_str())
            .map(String::from);
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing content".into()))?;

        let (content_without_markers, attachments) = extract_attachments(content);

        Ok(ToolAction::SendChannelMessage {
            channel: channel.to_string(),
            target,
            content: content_without_markers,
            attachments,
        })
    }
}
