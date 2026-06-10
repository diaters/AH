//! knowledge_search Tool 实现

use crate::domain::{KnowledgeValidationStatus, ToolAction, ToolContext, ToolError};

pub struct KnowledgeSearchTool;

impl crate::domain::BuiltinTool for KnowledgeSearchTool {
    fn name(&self) -> &str {
        "knowledge_search"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'query' parameter".to_string()))?;

        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(3) as usize;

        let results: Vec<&str> = ctx
            .knowledge
            .entries
            .iter()
            .filter(|entry| entry.validation_status == KnowledgeValidationStatus::Approved)
            .filter(|entry| entry.content.to_lowercase().contains(&query.to_lowercase()))
            .take(limit)
            .map(|entry| entry.content.as_str())
            .collect();

        Ok(ToolAction::Direct(serde_json::json!({
            "query": query,
            "results": results,
            "count": results.len()
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        BuiltinTool, LongTermMemoryKind, SharedKnowledgeBase, SharedKnowledgeEntry,
    };

    fn test_knowledge() -> SharedKnowledgeBase {
        let mut knowledge = SharedKnowledgeBase::default();
        knowledge
            .entries
            .push(SharedKnowledgeEntry::approved_from_user_input(
                "The project uses Rust and Bevy framework",
            ));
        knowledge
            .entries
            .push(SharedKnowledgeEntry::approved_from_user_input(
                "The system follows ECS architecture",
            ));
        knowledge
    }

    #[test]
    fn executor_knowledge_search() {
        let knowledge = test_knowledge();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::nil(),
            current_agent_id: uuid::Uuid::nil(),
        };
        let executor = KnowledgeSearchTool;

        // Search for "rust"
        let input = serde_json::json!({"query": "rust"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::Direct(value) => {
                assert_eq!(value["count"], 1);
            }
            other => panic!("expected Direct action, got {:?}", other),
        }

        // Search for "bevy"
        let input = serde_json::json!({"query": "bevy"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());

        // Search for non-existent
        let input = serde_json::json!({"query": "python"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::Direct(value) => {
                assert_eq!(value["count"], 0);
            }
            other => panic!("expected Direct action, got {:?}", other),
        }
    }

    #[test]
    fn executor_knowledge_search_missing_query() {
        let knowledge = SharedKnowledgeBase::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::nil(),
            current_agent_id: uuid::Uuid::nil(),
        };
        let executor = KnowledgeSearchTool;
        let input = serde_json::json!({"limit": 5});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ToolError::InvalidInput(_)));
    }

    #[test]
    fn knowledge_search_ignores_non_approved_entries() {
        let mut knowledge = SharedKnowledgeBase::default();
        knowledge.entries.push(SharedKnowledgeEntry::candidate(
            "Unreviewed shell note",
            LongTermMemoryKind::Fact,
        ));

        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::nil(),
            current_agent_id: uuid::Uuid::nil(),
        };
        let executor = KnowledgeSearchTool;
        let result = executor
            .execute(&serde_json::json!({"query": "shell"}), &ctx)
            .unwrap();
        match result {
            ToolAction::Direct(value) => assert_eq!(value["count"], 0),
            other => panic!("expected Direct action, got {:?}", other),
        }
    }
}
