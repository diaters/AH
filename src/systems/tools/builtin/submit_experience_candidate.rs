//! 经验候选提交工具

use crate::domain::{
    ExperienceCandidateSubmission, ExperienceKindHint, ToolAction, ToolContext, ToolError,
};

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

        match submission.kind {
            ExperienceKindHint::Knowledge => {
                if submission.content.as_deref().unwrap_or("").is_empty() {
                    return Err(ToolError::InvalidInput(
                        "knowledge kind requires non-empty content".to_string(),
                    ));
                }
            }
            ExperienceKindHint::Skill => {
                if submission
                    .skill_description
                    .as_deref()
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(ToolError::InvalidInput(
                        "skill kind requires non-empty skill_description".to_string(),
                    ));
                }
                if submission.instructions.as_deref().unwrap_or("").is_empty() {
                    return Err(ToolError::InvalidInput(
                        "skill kind requires non-empty instructions".to_string(),
                    ));
                }
                // Validate file_refs existence
                let missing: Vec<String> = submission
                    .file_refs
                    .iter()
                    .filter(|f| !std::path::Path::new(&f.path).exists())
                    .map(|f| f.path.clone())
                    .collect();
                if !missing.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "skill file_refs references non-existent files: {}",
                        missing.join(", ")
                    )));
                }
            }
        }

        Ok(ToolAction::SubmitExperienceCandidate(submission))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};

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
    fn submit_knowledge_candidate_returns_submit_action() {
        let ctx = test_ctx();
        let tool = SubmitExperienceCandidateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "title": "shell timeout note",
                    "kind": "knowledge",
                    "content": "shell_stop 默认等待退出"
                }),
                &ctx,
            )
            .unwrap();

        assert!(matches!(action, ToolAction::SubmitExperienceCandidate(_)));
    }

    #[test]
    fn submit_skill_candidate_returns_submit_action() {
        let ctx = test_ctx();
        let tool = SubmitExperienceCandidateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "title": "build checker",
                    "kind": "skill",
                    "skill_description": "检查项目是否能成功编译",
                    "instructions": "1. 运行 cargo check\n2. 报告结果"
                }),
                &ctx,
            )
            .unwrap();

        assert!(matches!(action, ToolAction::SubmitExperienceCandidate(_)));
    }

    #[test]
    fn submit_experience_candidate_rejects_missing_title() {
        let ctx = test_ctx();
        let tool = SubmitExperienceCandidateTool;
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn submit_knowledge_rejects_empty_content() {
        let ctx = test_ctx();
        let tool = SubmitExperienceCandidateTool;
        let result = tool.execute(
            &serde_json::json!({
                "title": "empty knowledge",
                "kind": "knowledge",
                "content": ""
            }),
            &ctx,
        );
        assert!(result.is_err());
        match result {
            Err(ToolError::InvalidInput(msg)) => {
                assert!(
                    msg.contains("content"),
                    "expected content-related error, got: {msg}"
                );
            }
            other => panic!("expected InvalidInput error, got: {:?}", other),
        }
    }
}
