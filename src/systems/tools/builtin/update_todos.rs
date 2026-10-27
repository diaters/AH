//! update_todos 工具：Agent 自报任务进度清单（纯展示数据，见 ADR-007）。
//!
//! worker 只做纯校验（条目上限、内容非空、最多一项 in_progress），把全量清单包成
//! `ToolEffect::UpdateTodos`，由 `commit_tool_effects_system` 在主线程写入 `TaskTodoList`
//! 并通知前端。清单不参与任何调度决策。

use crate::domain::{
    BuiltinTool, OwnedToolContext, TodoItem, ToolAction, ToolActionKind, ToolContext, ToolEffect,
    ToolError, ToolFuture, ToolWorkerOutput, validate_todos,
};

pub struct UpdateTodosTool;

/// 解析并校验入参中的全量清单。
pub(crate) fn parse_todos(input: &serde_json::Value) -> Result<Vec<TodoItem>, ToolError> {
    let raw = input
        .get("todos")
        .cloned()
        .ok_or_else(|| ToolError::InvalidInput("missing required parameter: todos".to_string()))?;
    let items: Vec<TodoItem> = serde_json::from_value(raw)
        .map_err(|e| ToolError::InvalidInput(format!("invalid todos: {e}")))?;
    validate_todos(&items).map_err(ToolError::InvalidInput)?;
    Ok(items)
}

impl BuiltinTool for UpdateTodosTool {
    fn name(&self) -> &str {
        "update_todos"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "update_todos is async-only".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            let task_id = ctx.current_task_id.ok_or_else(|| {
                ToolError::InternalState("update_todos requires a current task".to_string())
            })?;
            let items = parse_todos(&input)?;
            Ok(ToolWorkerOutput::Effect(ToolEffect::UpdateTodos {
                task_id,
                items,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TodoStatus;

    #[test]
    fn parse_accepts_snake_case_statuses() {
        let items = parse_todos(&serde_json::json!({
            "todos": [
                { "content": "read code", "status": "completed" },
                { "content": "write tests", "status": "in_progress" },
                { "content": "open PR", "status": "pending" }
            ]
        }))
        .unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].status, TodoStatus::InProgress);
    }

    #[test]
    fn parse_rejects_invalid_lists() {
        assert!(parse_todos(&serde_json::json!({})).is_err());
        assert!(
            parse_todos(&serde_json::json!({
                "todos": [{ "content": "x", "status": "doing" }]
            }))
            .is_err()
        );
        assert!(
            parse_todos(&serde_json::json!({
                "todos": [
                    { "content": "a", "status": "in_progress" },
                    { "content": "b", "status": "in_progress" }
                ]
            }))
            .is_err()
        );
    }

    #[test]
    fn empty_list_clears_todos() {
        assert!(
            parse_todos(&serde_json::json!({ "todos": [] }))
                .unwrap()
                .is_empty()
        );
    }
}
