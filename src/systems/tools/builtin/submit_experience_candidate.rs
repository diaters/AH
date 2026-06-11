//! 经验候选提交工具

use crate::domain::{ExperienceCandidateSubmission, ToolAction, ToolContext, ToolError};

/// 提交经验候选工具
///
/// 允许 Agent 在任务结束后提交可复用的经验候选。
pub struct SubmitExperienceCandidateTool;

impl crate::domain::BuiltinTool for SubmitExperienceCandidateTool {
    fn name(&self) -> &str {
        "submit_experience_candidate"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let title = input
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing title".to_string()))?;

        let submission = ExperienceCandidateSubmission::from_json(
            ctx.current_task_id,
            ctx.current_agent_id,
            title,
            input,
        )?;

        Ok(ToolAction::SubmitExperienceCandidate(submission))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};

    #[test]
    fn submit_experience_candidate_returns_submit_action() {
        let knowledge = SharedKnowledgeBase::default();
        let store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = SubmitExperienceCandidateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "title": "shell timeout note",
                    "kind_hint": "knowledge",
                    "payload": {
                        "content": "shell_stop 默认等待退出",
                        "memory_kind": "Fact"
                    },
                    "dependency_refs": []
                }),
                &ctx,
            )
            .unwrap();

        assert!(matches!(
            action,
            ToolAction::SubmitExperienceCandidate(_)
        ));
    }

    #[test]
    fn submit_experience_candidate_rejects_missing_title() {
        let knowledge = SharedKnowledgeBase::default();
        let store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = SubmitExperienceCandidateTool;
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }
}