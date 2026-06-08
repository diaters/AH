//! spawn_agent Tool 实现

use crate::domain::{ToolAction, ToolContext, ToolError};

pub struct SpawnAgentTool;

impl crate::domain::BuiltinTool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let (name, model, description, tools) = parse_spawn_agent_params(input);
        Ok(ToolAction::SpawnAgent {
            name,
            model,
            description,
            tools,
        })
    }
}

/// 解析 spawn_agent tool 输入参数
pub fn parse_spawn_agent_params(
    input: &serde_json::Value,
) -> (String, Option<String>, String, Vec<String>) {
    let name = input
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("child-agent")
        .to_string();

    let model = input
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let tools: Vec<String> = input
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    (name, model, description, tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, SpaceKnowledge};

    #[test]
    fn executor_spawn_agent() {
        let knowledge = SpaceKnowledge::default();
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
        let executor = SpawnAgentTool;
        let input = serde_json::json!({
            "name": "child",
            "model": "gpt-4",
            "description": "A child agent",
            "tools": ["knowledge_search"]
        });
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::SpawnAgent {
                name,
                model,
                description,
                tools,
            } => {
                assert_eq!(name, "child");
                assert_eq!(model, Some("gpt-4".to_string()));
                assert_eq!(description, "A child agent");
                assert_eq!(tools, vec!["knowledge_search"]);
            }
            other => panic!("expected SpawnAgent action, got {:?}", other),
        }
    }
}
