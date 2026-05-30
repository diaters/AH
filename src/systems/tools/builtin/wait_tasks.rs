//! wait_tasks Tool 实现

use crate::domain::{TaskId, ToolAction, ToolContext, ToolError};

pub struct WaitTasksTool;

impl crate::domain::BuiltinTool for WaitTasksTool {
    fn name(&self) -> &str {
        "wait_tasks"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let task_ids = parse_wait_tasks_ids(input)?;
        let timeout_secs = parse_wait_tasks_timeout(input, ctx.default_wait_tasks_timeout_secs);

        Ok(ToolAction::WaitForTasks {
            task_ids,
            timeout_secs,
        })
    }
}

fn parse_wait_tasks_ids(input: &serde_json::Value) -> Result<Vec<TaskId>, ToolError> {
    let ids_value = input
        .get("task_ids")
        .ok_or_else(|| ToolError::InvalidInput("missing 'task_ids' parameter".to_string()))?;

    let ids_array = ids_value
        .as_array()
        .ok_or_else(|| ToolError::InvalidInput("'task_ids' must be an array".to_string()))?;

    let mut task_ids = Vec::new();
    for id_str in ids_array.iter().filter_map(|v| v.as_str()) {
        let id = uuid::Uuid::parse_str(id_str)
            .map_err(|_| ToolError::InvalidInput(format!("invalid task id: {}", id_str)))?;
        task_ids.push(id);
    }

    if task_ids.is_empty() {
        return Err(ToolError::InvalidInput(
            "'task_ids' cannot be empty".to_string(),
        ));
    }

    Ok(task_ids)
}

fn parse_wait_tasks_timeout(input: &serde_json::Value, default: u64) -> u64 {
    input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, SpaceKnowledge};
    use uuid::Uuid;

    #[test]
    fn test_wait_tasks_tool_parsing() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
        };
        let executor = WaitTasksTool;

        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let input = serde_json::json!({
            "task_ids": [id1.to_string(), id2.to_string()],
            "timeout_secs": 600
        });

        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::WaitForTasks { task_ids, timeout_secs } => {
                assert_eq!(task_ids.len(), 2);
                assert_eq!(timeout_secs, 600);
            }
            other => panic!("expected WaitForTasks action, got {:?}", other),
        }
    }

    #[test]
    fn test_wait_tasks_default_timeout() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
        };
        let executor = WaitTasksTool;

        let id = Uuid::new_v4();
        let input = serde_json::json!({
            "task_ids": [id.to_string()]
        });

        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::WaitForTasks { timeout_secs, .. } => {
                assert_eq!(timeout_secs, 300);
            }
            other => panic!("expected WaitForTasks action, got {:?}", other),
        }
    }

    #[test]
    fn test_wait_tasks_missing_task_ids() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
        };
        let executor = WaitTasksTool;

        let input = serde_json::json!({
            "timeout_secs": 100
        });

        let result = executor.execute(&input, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_wait_tasks_empty_task_ids() {
        let knowledge = SpaceKnowledge::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
        };
        let executor = WaitTasksTool;

        let input = serde_json::json!({
            "task_ids": []
        });

        let result = executor.execute(&input, &ctx);
        assert!(result.is_err());
    }
}
