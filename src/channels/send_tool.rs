use anyhow::Result;
use serde_json::{json, Value};

use crate::domain::{ToolAction, ToolError, ToolPermission, ToolSchema};

pub struct ChannelSendTool;

impl ChannelSendTool {
    pub fn definition() -> crate::domain::ToolDefinition {
        crate::domain::ToolDefinition {
            name: "channel_send".to_string(),
            description: "向指定 IM 通道（telegram/qq/feishu）发送消息".to_string(),
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
                            "description": "目标 chat_id / open_id / user_id"
                        },
                        "content": {
                            "type": "string",
                            "description": "要发送的内容"
                        }
                    },
                    "required": ["channel", "target", "content"]
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
            .ok_or_else(|| ToolError::InvalidInput("missing target".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing content".into()))?;

        Ok(ToolAction::SendChannelMessage {
            channel: channel.to_string(),
            target: target.to_string(),
            content: content.to_string(),
        })
    }
}
