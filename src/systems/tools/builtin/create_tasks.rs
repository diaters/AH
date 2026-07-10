//! create_tasks Tool 实现

use std::collections::{HashMap, HashSet};

use crate::domain::{SubTaskDefinition, ToolAction, ToolContext, ToolError};

pub struct CreateTasksTool;

impl crate::domain::BuiltinTool for CreateTasksTool {
    fn name(&self) -> &str {
        "create_tasks"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let definitions = parse_create_tasks_params(input).map_err(ToolError::InvalidInput)?;
        Ok(ToolAction::CreateBatch(definitions))
    }
}

/// 解析 create_tasks tool 输入参数，包含循环依赖检测
pub fn parse_create_tasks_params(
    input: &serde_json::Value,
) -> Result<Vec<SubTaskDefinition>, String> {
    let tasks_array = input
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing or invalid 'tasks' array".to_string())?;

    if tasks_array.is_empty() {
        return Err("tasks array must not be empty".to_string());
    }

    let mut definitions = Vec::new();
    let mut names = HashSet::new();

    for task_val in tasks_array {
        let name = task_val
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "each task must have a 'name' field".to_string())?
            .to_string();

        if !names.insert(name.clone()) {
            return Err(format!("duplicate task name: '{}'", name));
        }

        let content = task_val
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("task '{}' missing 'content' field", name))?
            .to_string();

        let tools: Vec<String> = task_val
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let depends_on: Vec<String> = task_val
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| d.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let model = task_val
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        definitions.push(SubTaskDefinition {
            name,
            content,
            tools,
            depends_on,
            model,
        });
    }

    // 验证 depends_on 引用的 name 在 tasks 中存在
    for def in &definitions {
        for dep in &def.depends_on {
            if !names.contains(dep.as_str()) {
                return Err(format!(
                    "task '{}' depends_on '{}' which does not exist in this batch",
                    def.name, dep
                ));
            }
        }
    }

    // 检测循环依赖（DFS）
    detect_cycle(&definitions)?;

    Ok(definitions)
}

/// DFS 循环依赖检测
fn detect_cycle(definitions: &[SubTaskDefinition]) -> Result<(), String> {
    let name_to_idx: HashMap<&str, usize> = definitions
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum VisitState {
        Unvisited,
        Visiting,
        Visited,
    }

    let mut states = vec![VisitState::Unvisited; definitions.len()];

    fn dfs(
        node: usize,
        states: &mut [VisitState],
        name_to_idx: &HashMap<&str, usize>,
        definitions: &[SubTaskDefinition],
    ) -> Result<(), String> {
        states[node] = VisitState::Visiting;
        for dep in &definitions[node].depends_on {
            if let Some(&dep_idx) = name_to_idx.get(dep.as_str()) {
                match states[dep_idx] {
                    VisitState::Visiting => {
                        return Err(format!(
                            "circular dependency detected involving '{}'",
                            definitions[node].name
                        ));
                    }
                    VisitState::Unvisited => {
                        dfs(dep_idx, states, name_to_idx, definitions)?;
                    }
                    VisitState::Visited => {}
                }
            }
        }
        states[node] = VisitState::Visited;
        Ok(())
    }

    for i in 0..definitions.len() {
        if states[i] == VisitState::Unvisited {
            dfs(i, &mut states, &name_to_idx, definitions)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};

    #[test]
    fn executor_create_tasks() {
        let knowledge = SharedKnowledgeBase::default();
        let experience_store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &experience_store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::nil(),
            current_agent_id: uuid::Uuid::nil(),
            current_origin_channel: None,
        };
        let executor = CreateTasksTool;
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "task-a",
                    "content": "do something",
                    "tools": ["shell_exec"]
                },
                {
                    "name": "task-b",
                    "content": "do something else",
                    "tools": ["shell_exec"],
                    "depends_on": ["task-a"]
                }
            ]
        });
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::CreateBatch(defs) => {
                assert_eq!(defs.len(), 2);
                assert_eq!(defs[0].name, "task-a");
                assert!(defs[0].depends_on.is_empty());
                assert_eq!(defs[1].name, "task-b");
                assert_eq!(defs[1].depends_on, vec!["task-a"]);
            }
            other => panic!("expected CreateBatch action, got {:?}", other),
        }
    }

    #[test]
    fn parse_create_tasks_params_basic() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "task-a",
                    "content": "do something",
                    "tools": ["shell_exec"]
                },
                {
                    "name": "task-b",
                    "content": "do something else",
                    "tools": ["shell_exec"],
                    "depends_on": ["task-a"]
                }
            ]
        });

        let result = parse_create_tasks_params(&input);
        assert!(
            result.is_ok(),
            "should parse valid tasks: {:?}",
            result.err()
        );
        let defs = result.unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "task-a");
        assert!(defs[0].depends_on.is_empty());
        assert_eq!(defs[1].name, "task-b");
        assert_eq!(defs[1].depends_on, vec!["task-a"]);
    }

    #[test]
    fn parse_create_tasks_params_empty_tasks() {
        let input = serde_json::json!({"tasks": []});
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not be empty"));
    }

    #[test]
    fn parse_create_tasks_params_duplicate_name() {
        let input = serde_json::json!({
            "tasks": [
                {"name": "dup", "content": "first", "tools": []},
                {"name": "dup", "content": "second", "tools": []}
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("duplicate"));
    }

    #[test]
    fn parse_create_tasks_params_missing_dependency() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "only-task",
                    "content": "do something",
                    "tools": ["shell_exec"],
                    "depends_on": ["nonexistent"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn parse_create_tasks_params_cycle_detection() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "task-a",
                    "content": "first",
                    "tools": ["shell_exec"],
                    "depends_on": ["task-b"]
                },
                {
                    "name": "task-b",
                    "content": "second",
                    "tools": ["shell_exec"],
                    "depends_on": ["task-a"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("circular dependency"));
    }

    #[test]
    fn parse_create_tasks_params_self_cycle() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "self-ref",
                    "content": "bad",
                    "tools": ["shell_exec"],
                    "depends_on": ["self-ref"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("circular dependency"));
    }

    #[test]
    fn parse_create_tasks_params_optional_fields() {
        let input = serde_json::json!({
            "tasks": [
                {
                    "name": "minimal",
                    "content": "just content",
                    "tools": ["shell_exec"]
                }
            ]
        });
        let result = parse_create_tasks_params(&input);
        assert!(result.is_ok());
        let defs = result.unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "minimal");
        assert!(defs[0].depends_on.is_empty());
        assert!(defs[0].model.is_none());
    }
}
