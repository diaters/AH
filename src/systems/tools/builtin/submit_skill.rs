//! skill 创建提交工具

use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};

/// 提交 skill 创建候选工具
///
/// 由 skill-creator Agent 调用，提交 skill 名称与描述。
/// 实际的 skill 文件写入与 registry 刷新在后续任务中完成，
/// 本工具仅负责参数解析与校验。
pub struct SubmitSkillTool;

impl BuiltinTool for SubmitSkillTool {
    fn name(&self) -> &str {
        "submit_skill"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if name.is_empty() {
            return Err(ToolError::InvalidInput(
                "name is required and must not be empty".to_string(),
            ));
        }
        // Sanitize skill name: reject path separators and traversal sequences
        // to prevent path traversal in writeback (skill_name flows into fs::rename target).
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return Err(ToolError::InvalidInput(
                "skill name must not contain path separators or '..'".to_string(),
            ));
        }
        if description.is_empty() {
            return Err(ToolError::InvalidInput(
                "description is required and must not be empty".to_string(),
            ));
        }

        Ok(ToolAction::SubmitSkillCandidate {
            name: name.to_string(),
            description: description.to_string(),
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
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
            current_origin_channel: None,
            current_skill_dir: None,
        }
    }

    #[test]
    fn submit_skill_returns_submit_action() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "name": "my-skill",
                    "description": "A test skill"
                }),
                &ctx,
            )
            .unwrap();

        match action {
            ToolAction::SubmitSkillCandidate { name, description } => {
                assert_eq!(name, "my-skill");
                assert_eq!(description, "A test skill");
            }
            other => panic!("expected SubmitSkillCandidate, got: {:?}", other),
        }
    }

    #[test]
    fn submit_skill_rejects_empty_name() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "",
                "description": "A test skill"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("name")
        ));
    }

    #[test]
    fn submit_skill_rejects_empty_description() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "my-skill",
                "description": ""
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("description")
        ));
    }

    #[test]
    fn submit_skill_rejects_missing_name() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "description": "A test skill"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("name")
        ));
    }

    #[test]
    fn submit_skill_rejects_missing_description() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "my-skill"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("description")
        ));
    }

    #[test]
    fn submit_skill_rejects_path_traversal_name() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;

        for evil_name in &["../../evil", "sub/evil", "sub\\evil", ".."] {
            let result = tool.execute(
                &serde_json::json!({
                    "name": *evil_name,
                    "description": "test"
                }),
                &ctx,
            );
            assert!(
                matches!(result, Err(ToolError::InvalidInput(ref msg)) if msg.contains("path separator") || msg.contains("'..'")),
                "expected rejection for name '{}', got {:?}",
                evil_name,
                result
            );
        }
    }
}
