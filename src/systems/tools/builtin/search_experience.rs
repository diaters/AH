//! 经验库检索工具：参数是自然语言情况描述，判断交给经验管理员。

use crate::domain::{ToolAction, ToolContext, ToolError};

/// 情况描述上限，避免把整段日志塞给管理员。
const MAX_SITUATION_CHARS: usize = 4000;

pub struct SearchExperienceTool;

impl crate::domain::BuiltinTool for SearchExperienceTool {
    fn name(&self) -> &str {
        "search_experience"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let situation = input
            .get("situation")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidInput("missing 'situation'".to_string()))?;
        if situation.chars().count() > MAX_SITUATION_CHARS {
            return Err(ToolError::InvalidInput(format!(
                "situation exceeds {MAX_SITUATION_CHARS} characters; keep the key error lines only"
            )));
        }
        Ok(ToolAction::SearchExperience {
            situation: situation.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::BuiltinTool;

    fn ctx() -> ToolContext {
        ToolContext {
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: crate::domain::TaskId::new(),
            current_agent_id: crate::domain::AgentId::new(),
            current_origin_channel: None,
        }
    }

    #[test]
    fn returns_search_action_with_trimmed_situation() {
        let action = SearchExperienceTool
            .execute(
                &serde_json::json!({"situation": "  hdc 安装报错 9568322 "}),
                &ctx(),
            )
            .unwrap();
        assert!(matches!(
            action,
            ToolAction::SearchExperience { situation } if situation == "hdc 安装报错 9568322"
        ));
    }

    #[test]
    fn rejects_missing_or_oversized_situation() {
        assert!(
            SearchExperienceTool
                .execute(&serde_json::json!({}), &ctx())
                .is_err()
        );
        assert!(
            SearchExperienceTool
                .execute(&serde_json::json!({"situation": "  "}), &ctx())
                .is_err()
        );
        let long = "x".repeat(MAX_SITUATION_CHARS + 1);
        assert!(
            SearchExperienceTool
                .execute(&serde_json::json!({"situation": long}), &ctx())
                .is_err()
        );
    }
}
