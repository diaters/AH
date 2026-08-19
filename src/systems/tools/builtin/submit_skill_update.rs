//! skill 更新提交工具

use crate::domain::{BuiltinTool, SkillUpdateOperation, ToolAction, ToolContext, ToolError};

/// 提交 skill 更新工具
///
/// 由 skill-updater Agent 调用，提交结构化 diff 操作。
/// 实际的 skill 文件 apply 与 registry 刷新在 `skill_update_completion_system`
/// 中完成，本工具仅负责参数解析与校验。
///
/// 仅暴露 `operations` 与 `rationale` 给 LLM；
/// `skill_id` / `base_version` / `new_version` 由 orchestrator 从
/// `SkillUpdateContext` 服务端权威注入，避免 LLM 臆造 skill_id。
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

        Ok(ToolAction::SubmitSkillUpdate {
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
            tool_inflight_timeout_secs: 300,
            current_task_id: crate::domain::TaskId::new(),
            current_agent_id: crate::domain::AgentId::new(),
            current_origin_channel: None,
            current_skill_dir: None,
        }
    }

    #[test]
    fn submit_skill_update_accepts_only_operations_and_rationale() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        // 仅提供 operations + rationale；额外携带 skill_id/base_version/new_version
        // 也应被忽略而非报错（向后兼容 LLM 误填）。
        let action = tool
            .execute(
                &serde_json::json!({
                    "operations": [
                        {"action": "replace_section", "section": "## Usage", "content": "new"}
                    ],
                    "rationale": "更新 Usage 章节",
                    "skill_id": "agent-a/coding",
                    "base_version": 3,
                    "new_version": 4,
                }),
                &ctx,
            )
            .unwrap();

        match action {
            ToolAction::SubmitSkillUpdate {
                operations,
                rationale,
            } => {
                assert_eq!(operations.len(), 1);
                assert_eq!(rationale, "更新 Usage 章节");
            }
            other => panic!("expected SubmitSkillUpdate, got: {:?}", other),
        }
    }

    #[test]
    fn submit_skill_update_returns_submit_action_with_minimal_fields() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let action = tool
            .execute(
                &serde_json::json!({
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
                operations,
                rationale,
            } => {
                assert_eq!(operations.len(), 1);
                assert_eq!(rationale, "更新 Usage 章节");
            }
            other => panic!("expected SubmitSkillUpdate, got: {:?}", other),
        }
    }

    #[test]
    fn submit_skill_update_rejects_empty_rationale() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
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

    #[test]
    fn submit_skill_update_rejects_missing_operations() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "rationale": "test"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("operations")
        ));
    }

    #[test]
    fn submit_skill_update_rejects_missing_rationale() {
        let ctx = test_ctx();
        let tool = SubmitSkillUpdateTool;
        let result = tool.execute(
            &serde_json::json!({
                "operations": []
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("rationale")
        ));
    }
}
