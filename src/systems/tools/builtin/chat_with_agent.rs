//! chat_with_agent Tool 实现

use uuid::Uuid;

use crate::domain::{TaskId, ToolAction, ToolContext, ToolError};

pub struct ChatWithAgentTool;

impl crate::domain::BuiltinTool for ChatWithAgentTool {
    fn name(&self) -> &str {
        "chat_with_agent"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        parse_and_resolve(input, ctx.current_task_id)
    }
}

fn parse_and_resolve(
    input: &serde_json::Value,
    current_task_id: TaskId,
) -> Result<ToolAction, ToolError> {
    let message = input
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing 'message' parameter".to_string()))?
        .to_string();

    let handle = input
        .get("handle")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    let agent_name = input
        .get("agent")
        .and_then(|v| v.as_str())
        .map(String::from);
    let agent_tags: Vec<String> = input
        .get("agent_tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let context = input
        .get("context")
        .and_then(|v| v.as_str())
        .map(String::from);

    if handle.is_none() && agent_name.is_none() && agent_tags.is_empty() {
        return Err(ToolError::InvalidInput(
            "new chat requires 'agent' or 'agent_tags'".to_string(),
        ));
    }

    let _ = current_task_id; // used by orchestrator for validation

    Ok(ToolAction::StartChatRound {
        agent_name,
        agent_tags,
        message,
        context,
        handle,
    })
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
            current_skill_dir: None,
        }
    }

    #[test]
    fn parse_requires_message() {
        let input = serde_json::json!({"agent": "reviewer"});
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_err());
    }

    #[test]
    fn parse_requires_agent_or_tags_for_new_chat() {
        let input = serde_json::json!({"message": "hello"});
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_err());
    }

    #[test]
    fn parse_allows_handle_only() {
        let handle = Uuid::new_v4();
        let input = serde_json::json!({
            "message": "continue",
            "handle": handle.to_string()
        });
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::StartChatRound {
                handle: Some(h),
                message,
                agent_name,
                agent_tags,
                ..
            } => {
                assert_eq!(h, handle);
                assert_eq!(message, "continue");
                assert!(agent_name.is_none());
                assert!(agent_tags.is_empty());
            }
            other => panic!("expected StartChatRound, got {:?}", other),
        }
    }

    #[test]
    fn parse_new_chat_with_agent_name() {
        let input = serde_json::json!({
            "message": "review this doc",
            "agent": "reviewer",
            "context": "focus on api design"
        });
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::StartChatRound {
                agent_name: Some(name),
                message,
                context: Some(ctx),
                ..
            } => {
                assert_eq!(name, "reviewer");
                assert_eq!(message, "review this doc");
                assert_eq!(ctx, "focus on api design");
            }
            other => panic!("expected StartChatRound, got {:?}", other),
        }
    }
}
