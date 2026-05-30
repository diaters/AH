//! Tool 执行模块
//!
//! 实现 Tool 的分发、执行和结果处理。

mod approval;
mod builtin;
mod confirmation;
mod dispatch;
mod orchestrator;
mod result;
mod waiting;

pub use approval::{approval_dispatch_system, approval_result_system};
pub use confirmation::{tool_confirmation_request_system, tool_confirmation_result_system};
pub use dispatch::tool_dispatch_system;
pub use result::tool_result_system;
pub use waiting::{check_waiting_tasks_system, on_subtask_completed_check_waiting};

use crate::domain::{
    BuiltinToolExecutors, SpaceToolRegistry, ToolDefinition, ToolExecutorKind, ToolPermission,
    ToolSchema,
};

use self::builtin::{CreateTasksTool, KnowledgeSearchTool, SpawnAgentTool, WaitTasksTool};

/// 注册内置 Tool
pub fn register_builtin_tools(
    registry: &mut SpaceToolRegistry,
    executors: &mut BuiltinToolExecutors,
) {
    registry.register(ToolDefinition {
        name: "knowledge_search".to_string(),
        description: "Search for relevant information in the shared knowledge base. Use this when you need to access global knowledge, user preferences, or context that is not in your personal memory.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query or keywords to look for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 3)",
                        "default": 3
                    }
                },
                "required": ["query"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("knowledge_search".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(KnowledgeSearchTool));

    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent with specified tools and capabilities. The child agent will be bound to the current task and automatically terminated when the task completes.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name for the child agent"
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional model to use. Defaults to parent agent's model."
                    },
                    "description": {
                        "type": "string",
                        "description": "Description of the child agent's capabilities"
                    },
                    "tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of tool names the child agent can use"
                    }
                },
                "required": ["name", "description", "tools"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
        required_tag: Some("brain".to_string()),
    });
    executors.register(Box::new(SpawnAgentTool));

    registry.register(ToolDefinition {
        name: "create_tasks".to_string(),
        description: "Create sub-tasks to delegate work to specialized child agents. Supports creating multiple tasks with dependency ordering. Tasks without dependencies will run in parallel; tasks with dependencies will wait for them to complete.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "description": "List of sub-tasks to create",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "description": "Name for the sub-task/child agent"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Task description/prompt for the child agent"
                                },
                                "tools": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "List of tool names the child agent can use"
                                },
                                "depends_on": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Names of other sub-tasks in this batch that must complete before this one starts"
                                },
                                "model": {
                                    "type": "string",
                                    "description": "Optional model override for the child agent"
                                }
                            },
                            "required": ["name", "content", "tools"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("create_tasks".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(CreateTasksTool));

    registry.register(ToolDefinition {
        name: "wait_tasks".to_string(),
        description: "Wait for child tasks to complete and collect their results. Returns the status and results of all specified tasks when all complete or timeout is reached.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of child task IDs to wait for"
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 300)"
                    }
                },
                "required": ["task_ids"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("wait_tasks".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(WaitTasksTool));
}

// Re-export tests module for backward compatibility
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentExperience, AgentKind, AgentProfile, AgentToolPermissions,
        BuiltinTool, EntryRole, MemoryEntry, SpaceKnowledge, ToolContext,
    };

    #[allow(dead_code)]
    fn test_agent() -> crate::domain::Agent {
        crate::domain::Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "test agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            experience: AgentExperience::default(),
        }
    }

    #[test]
    fn executor_knowledge_search() {
        let mut knowledge = SpaceKnowledge::default();
        knowledge.entries.push(MemoryEntry::new(
            EntryRole::User,
            "The project uses Rust and Bevy framework",
        ));
        knowledge.entries.push(MemoryEntry::new(
            EntryRole::User,
            "The system follows ECS architecture",
        ));

        let ctx = ToolContext {
            knowledge: &knowledge,
            default_wait_tasks_timeout_secs: 300,
        };
        let executor = builtin::KnowledgeSearchTool;

        // Search for "rust"
        let input = serde_json::json!({"query": "rust"});
        let result = executor.execute(&input, &ctx);
        assert!(result.is_ok());
        match result.unwrap() {
            crate::domain::ToolAction::Direct(value) => {
                assert_eq!(value["count"], 1);
            }
            other => panic!("expected Direct action, got {:?}", other),
        }
    }

    #[test]
    fn agent_tool_permissions_default_is_confirm() {
        let perms = AgentToolPermissions::default();
        assert_eq!(
            perms.get_permission("unknown_tool"),
            ToolPermission::Confirm
        );
    }

    #[test]
    fn agent_tool_permissions_override() {
        let mut perms = AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            ..Default::default()
        };
        perms
            .overrides
            .insert("knowledge_search".to_string(), ToolPermission::Allow);

        assert_eq!(
            perms.get_permission("knowledge_search"),
            ToolPermission::Allow
        );
        assert_eq!(perms.get_permission("other"), ToolPermission::Deny);
    }
}
