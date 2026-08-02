//! ask_user Tool 实现
//!
//! 声明式 Sync 工具：executor 只解析参数并返回 `ToolAction::AskUser`，
//! 问题呈现与跨帧等待由 orchestrator 完成。

use crate::domain::{ToolAction, ToolContext, ToolError};

pub struct AskUserTool;

impl crate::domain::BuiltinTool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let question = input
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'question' parameter".to_string()))?
            .to_string();

        Ok(ToolAction::AskUser { question })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};
    use uuid::Uuid;

    fn tool_context() -> ToolContext<'static> {
        let knowledge = Box::leak(Box::new(SharedKnowledgeBase::default()));
        let experience_store = Box::leak(Box::new(ExperienceStore::default()));
        ToolContext {
            knowledge,
            experience_store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: Uuid::new_v4(),
            current_agent_id: Uuid::new_v4(),
            current_origin_channel: None,
        }
    }

    #[test]
    fn parse_valid_question_returns_ask_user_action() {
        let input = serde_json::json!({"question": "用什么框架?"});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::AskUser { question } => {
                assert_eq!(question, "用什么框架?");
            }
            other => panic!("expected AskUser, got {:?}", other),
        }
    }

    #[test]
    fn parse_missing_question_returns_error() {
        let input = serde_json::json!({});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("question"), "msg: {msg}");
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn parse_non_string_question_returns_error() {
        let input = serde_json::json!({"question": 123});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_err());
    }

    #[test]
    fn parse_extra_fields_ignored() {
        let input = serde_json::json!({"question": "继续?", "extra": "ignored"});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::AskUser { question } => {
                assert_eq!(question, "继续?");
            }
            other => panic!("expected AskUser, got {:?}", other),
        }
    }
}
