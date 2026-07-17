//! skill 更新提交工具

use crate::domain::{BuiltinTool, SkillUpdateOperation, ToolAction, ToolContext, ToolError};
use crate::infrastructure::skills::SkillId;

/// 提交 skill 更新工具
///
/// 由 skill-updater Agent 调用，提交结构化 diff 操作。
/// 实际的 skill 文件 apply 与 registry 刷新在 `skill_update_completion_system`
/// 中完成，本工具仅负责参数解析与校验。
pub struct SubmitSkillUpdateTool;

impl BuiltinTool for SubmitSkillUpdateTool {
    fn name(&self) -> &str {
        "submit_skill_update"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let skill_id_str = input
            .get("skill_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing skill_id".to_string()))?;

        let skill_id = SkillId::parse(skill_id_str).ok_or_else(|| {
            ToolError::InvalidInput(format!("invalid skill_id: {}", skill_id_str))
        })?;

        let base_version = input
            .get("base_version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::InvalidInput("missing base_version".to_string()))?
            as u32;

        let new_version = input
            .get("new_version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::InvalidInput("missing new_version".to_string()))?
            as u32;

        let operations: Vec<SkillUpdateOperation> = input
            .get("operations")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidInput("missing operations".to_string()))?
            .iter()
            .map(|op| serde_json::from_value(op.clone()))
            .collect::<Result<_, _>>()
            .map_err(|e| ToolError::InvalidInput(format!("invalid operations: {}", e)))?;

        let rationale = input
            .get("rationale")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing rationale".to_string()))?
            .to_string();

        if rationale.is_empty() {
            return Err(ToolError::InvalidInput(
                "rationale must not be empty".to_string(),
            ));
        }

        if new_version != base_version + 1 {
            return Err(ToolError::InvalidInput(format!(
                "new_version must be base_version + 1, got base={} new={}",
                base_version, new_version
            )));
        }

        Ok(ToolAction::SubmitSkillUpdate {
            skill_id,
            base_version,
            new_version,
            operations,
            rationale,
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
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
            current_origin_channel: None,
        }
    }

    #[test]
    fn submit_skill_update_returns_submit_action() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "skill_id": "agent-a/coding",
                    "base_version": 3,
                    "new_version": 4,
                    "operations": [
                        {"action": "replace_section", "section": "## Usage", "content": "new"}
                    ],
                    "rationale": "更新 Usage 章节"
                }),
                &ctx,
            )
            .unwrap();

        match action {
            ToolAction::SubmitSkillUpdate {
                skill_id,
                base_version,
                new_version,
                operations,
                rationale,
            } => {
                assert_eq!(skill_id, SkillId::new("agent-a", "coding"));
                assert_eq!(base_version, 3);
                assert_eq!(new_version, 4);
                assert_eq!(operations.len(), 1);
                assert_eq!(rationale, "更新 Usage 章节");
            }
            other => panic!("expected SubmitSkillUpdate, got: {:?}", other),
        }
    }

    #[test]
    fn submit_skill_update_rejects_missing_skill_id() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "base_version": 1,
                "new_version": 2,
                "operations": [],
                "rationale": "test"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("skill_id")
        ));
    }

    #[test]
    fn submit_skill_update_rejects_invalid_skill_id() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "skill_id": "no-slash",
                "base_version": 1,
                "new_version": 2,
                "operations": [],
                "rationale": "test"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("invalid skill_id")
        ));
    }

    #[test]
    fn submit_skill_update_rejects_wrong_version_increment() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "skill_id": "agent-a/coding",
                "base_version": 3,
                "new_version": 5,
                "operations": [],
                "rationale": "test"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("new_version")
        ));
    }

    #[test]
    fn submit_skill_update_rejects_empty_rationale() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "skill_id": "agent-a/coding",
                "base_version": 1,
                "new_version": 2,
                "operations": [],
                "rationale": ""
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("rationale")
        ));
    }

    #[test]
    fn submit_skill_update_rejects_invalid_operations() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "skill_id": "agent-a/coding",
                "base_version": 1,
                "new_version": 2,
                "operations": [{"action": "unknown_action"}],
                "rationale": "test"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("operations")
        ));
    }
}
