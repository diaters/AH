//! Tool 执行模块
//!
//! 实现 Tool 的分发、执行和结果处理。

mod approval;
pub mod backend;
mod builtin;
mod confirmation;
mod dispatch;
mod orchestrator;
mod result;
mod waiting;

pub use approval::{approval_dispatch_system, approval_result_system};
pub use backend::NativeProcessBackend;
pub use confirmation::{tool_confirmation_request_system, tool_confirmation_result_system};
pub use dispatch::tool_dispatch_system;
pub use result::tool_result_system;
pub use waiting::{check_waiting_tasks_system, on_subtask_completed_check_waiting};

use crate::domain::{
    BuiltinToolExecutors, SpaceToolRegistry, ToolDefinition, ToolExecutorKind, ToolPermission,
    ToolSchema,
};

use self::builtin::{
    CreateTasksTool, KnowledgeSearchTool, ListExperienceCandidatesTool, ShellExecTool,
    ShellInputTool, ShellListTool, ShellReadTool, ShellStartTool, ShellStopTool, SpawnAgentTool,
    SubmitExperienceCandidateTool, WaitTasksTool,
};

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

    // Shell tools
    registry.register(ToolDefinition {
        name: "shell_exec".to_string(),
        description: "同步执行一次性 shell 命令并等待结果，适合 build、test、lint、文件操作等短任务。长时间运行或需要交互的命令请使用 shell_start。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "cwd": { "type": "string", "description": "命令工作目录" },
                    "timeout_secs": { "type": "integer", "description": "超时时间（秒），默认使用系统配置" },
                    "tail_lines": { "type": "integer", "description": "返回的最新输出行数" }
                },
                "required": ["command"]
            }),
        },
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("shell_exec".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ShellExecTool));

    registry.register(ToolDefinition {
        name: "shell_start".to_string(),
        description: "异步启动长时间运行或可交互的 shell 会话，适用于 server、watcher、daemon 等持续型任务。返回 session_id 供后续读取、输入或停止。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "cwd": { "type": "string", "description": "命令工作目录" }
                },
                "required": ["command"]
            }),
        },
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("shell_start".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ShellStartTool));

    registry.register(ToolDefinition {
        name: "shell_read".to_string(),
        description: "读取指定 shell 会话的最新状态和输出快照。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "shell_start 返回的 session_id" },
                    "tail_lines": { "type": "integer", "description": "返回的最新输出行数" }
                },
                "required": ["session_id"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("shell_read".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ShellReadTool));

    registry.register(ToolDefinition {
        name: "shell_list".to_string(),
        description: "列出当前可见的活动 shell 会话。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("shell_list".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ShellListTool));

    registry.register(ToolDefinition {
        name: "shell_input".to_string(),
        description: "向交互式 shell 会话发送输入。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "shell_start 返回的 session_id" },
                    "input": { "type": "string", "description": "要发送的输入内容" },
                    "append_newline": { "type": "boolean", "description": "是否自动追加换行，默认 true" }
                },
                "required": ["session_id", "input"]
            }),
        },
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("shell_input".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ShellInputTool));

    registry.register(ToolDefinition {
        name: "shell_stop".to_string(),
        description: "停止指定的 shell 会话。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "shell_start 返回的 session_id" }
                },
                "required": ["session_id"]
            }),
        },
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("shell_stop".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ShellStopTool));

    // Experience candidate tools
    registry.register(ToolDefinition {
        name: "submit_experience_candidate".to_string(),
        description: "提交经验候选。knowledge 类提交可复用知识，skill 类提交可复用技能包（对齐 Agent Skills 规范）。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "简明标题，概括此经验的核心要点"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["knowledge", "skill"],
                        "description": "经验类型：knowledge=可复用知识，skill=可复用技能包"
                    },
                    "content": {
                        "type": "string",
                        "description": "knowledge 类的经验正文"
                    },
                    "skill_description": {
                        "type": "string",
                        "description": "skill 类的简要描述，说明做什么+何时触发"
                    },
                    "instructions": {
                        "type": "string",
                        "description": "skill 类的分步指令正文"
                    },
                    "file_refs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "文件路径（绝对路径或相对于项目根目录的相对路径）"
                                },
                                "role": {
                                    "type": "string",
                                    "enum": ["script", "reference", "asset"],
                                    "description": "文件角色，默认根据扩展名自动推断"
                                }
                            },
                            "required": ["path"]
                        },
                        "description": "skill 关联的资源文件列表"
                    }
                },
                "required": ["title", "kind"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("submit_experience_candidate".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(SubmitExperienceCandidateTool));

    registry.register(ToolDefinition {
        name: "list_experience_candidates".to_string(),
        description: "List experience candidates in the current task's inbox. Use this to review pending experience candidates submitted by child agents.".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("list_experience_candidates".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ListExperienceCandidatesTool));
}

// Re-export tests module for backward compatibility
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, BuiltinTool,
        ExperienceStore, SharedKnowledgeBase, SharedKnowledgeEntry, ToolContext,
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
        }
    }

    #[test]
    fn executor_knowledge_search() {
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
