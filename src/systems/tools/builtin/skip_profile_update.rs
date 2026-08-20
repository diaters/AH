//! profile 跳过工具

use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};

/// 跳过 profile 更新工具
///
/// 由 profile-designer Agent 调用，明确表示现有 Agent profile 不需要更新。
/// 仅在更新场景下使用；孵化场景不应调用本工具。
pub struct SkipProfileUpdateTool;

impl BuiltinTool for SkipProfileUpdateTool {
    fn name(&self) -> &str {
        "skip_profile_update"
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Ok(ToolAction::SkipProfileUpdate)
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
    fn skip_profile_update_returns_skip_action() {
        let ctx = test_ctx();
        let tool = SkipProfileUpdateTool;
        let action = tool.execute(&serde_json::json!({}), &ctx).unwrap();
        assert!(matches!(action, ToolAction::SkipProfileUpdate));
    }

    #[test]
    fn skip_profile_update_ignores_input() {
        let ctx = test_ctx();
        let tool = SkipProfileUpdateTool;
        // 即使传入无关参数，也应返回 SkipProfileUpdate
        let action = tool
            .execute(
                &serde_json::json!({"unused": "value", "name": "ignored"}),
                &ctx,
            )
            .unwrap();
        assert!(matches!(action, ToolAction::SkipProfileUpdate));
    }
}
