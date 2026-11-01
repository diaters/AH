//! stop_background_job Tool：异步 ReAct 的 stop()，停止本任务的后台作业。

use crate::domain::{ToolAction, ToolContext, ToolError};

pub struct StopBackgroundJobTool;

impl crate::domain::BuiltinTool for StopBackgroundJobTool {
    fn name(&self) -> &str {
        "stop_background_job"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let job_id = input
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'job_id'".to_string()))?;
        let job_id = uuid::Uuid::parse_str(job_id.trim())
            .map_err(|_| ToolError::InvalidInput(format!("invalid job_id: {job_id}")))?;
        Ok(ToolAction::StopBackgroundJob { job_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::BuiltinTool;

    fn ctx() -> ToolContext {
        ToolContext {
            default_wait_tasks_timeout_secs: 300,
            tool_background_after_secs: 0,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: crate::domain::TaskId::nil(),
            current_agent_id: crate::domain::AgentId::nil(),
            current_origin_channel: None,
        }
    }

    #[test]
    fn parses_job_id() {
        let id = uuid::Uuid::new_v4();
        let action = StopBackgroundJobTool
            .execute(&serde_json::json!({ "job_id": id.to_string() }), &ctx())
            .unwrap();
        assert!(matches!(action, ToolAction::StopBackgroundJob { job_id } if job_id == id));
    }

    #[test]
    fn rejects_missing_or_invalid_job_id() {
        assert!(
            StopBackgroundJobTool
                .execute(&serde_json::json!({}), &ctx())
                .is_err()
        );
        assert!(
            StopBackgroundJobTool
                .execute(&serde_json::json!({ "job_id": "nope" }), &ctx())
                .is_err()
        );
    }
}
