//! profile 提交工具

use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};

/// 提交 profile 更新工具
///
/// 由 profile-designer Agent 调用，提交生成或更新后的 Agent profile。
/// 实际的 profile 提取与 proposal 创建在 `profile_generation_completion_system`
/// 中完成，本工具仅负责参数解析与校验。
pub struct SubmitProfileUpdateTool;

impl BuiltinTool for SubmitProfileUpdateTool {
    fn name(&self) -> &str {
        "submit_profile_update"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing name".to_string()))?
            .to_string();

        let tags: Vec<String> = input
            .get("tags")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidInput("missing tags".to_string()))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing description".to_string()))?
            .to_string();

        if name.is_empty() {
            return Err(ToolError::InvalidInput(
                "name must not be empty".to_string(),
            ));
        }
        if tags.is_empty() {
            return Err(ToolError::InvalidInput(
                "tags must not be empty".to_string(),
            ));
        }
        if description.is_empty() {
            return Err(ToolError::InvalidInput(
                "description must not be empty".to_string(),
            ));
        }

        Ok(ToolAction::SubmitProfileUpdate {
            name,
            tags,
            description,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ExperienceStore, SharedKnowledgeBase};

    fn test_ctx() -> ToolContext<'static> {
        static KNOWLEDGE: std::sync::OnceLock<SharedKnowledgeBase> = std::sync::OnceLock::new();
        static STORE: std::sync::OnceLock<ExperienceStore> = std::sync::OnceLock::new();
        let knowledge = KNOWLEDGE.get_or_init(SharedKnowledgeBase::default);
        let store = STORE.get_or_init(ExperienceStore::default);
        ToolContext {
            knowledge,
            experience_store: store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: crate::domain::TaskId::new(),
            current_agent_id: crate::domain::AgentId::new(),
            current_origin_channel: None,
            current_skill_dir: None,
        }
    }

    #[test]
    fn submit_profile_update_returns_submit_action() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "name": "build-helper",
                    "tags": ["build", "rust"],
                    "description": "协助构建与编译相关任务"
                }),
                &ctx,
            )
            .unwrap();

        match action {
            ToolAction::SubmitProfileUpdate {
                name,
                tags,
                description,
            } => {
                assert_eq!(name, "build-helper");
                assert_eq!(tags, vec!["build".to_string(), "rust".to_string()]);
                assert_eq!(description, "协助构建与编译相关任务");
            }
            other => panic!("expected SubmitProfileUpdate, got: {:?}", other),
        }
    }

    #[test]
    fn submit_profile_update_rejects_missing_name() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "tags": ["build"],
                "description": "desc"
            }),
            &ctx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn submit_profile_update_rejects_missing_tags() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "agent",
                "description": "desc"
            }),
            &ctx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn submit_profile_update_rejects_missing_description() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "agent",
                "tags": ["build"]
            }),
            &ctx,
        );
        assert!(result.is_err());
    }

    #[test]
    fn submit_profile_update_rejects_empty_name() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "",
                "tags": ["build"],
                "description": "desc"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("name")
        ));
    }

    #[test]
    fn submit_profile_update_rejects_empty_tags() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "agent",
                "tags": [],
                "description": "desc"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("tags")
        ));
    }

    #[test]
    fn submit_profile_update_rejects_empty_description() {
        let ctx = test_ctx();
        let tool = SubmitProfileUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "agent",
                "tags": ["build"],
                "description": ""
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("description")
        ));
    }
}
