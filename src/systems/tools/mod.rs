//! Tool 执行模块
//!
//! 实现 Tool 的分发、执行和结果处理。

mod approval;
mod approval_hook;
mod async_dispatch;
pub mod backend;
pub mod builtin;
mod cancel_monitor;
mod channel_send_dispatch;
mod confirmation;
mod dispatch;
mod effect_commit;
mod ingest_tool_results;
mod skill_dir_resolver;
mod orchestrator;
mod result;
mod tool_called_hook;
mod tool_returned_hook;
mod waiting;

pub use approval::{approval_dispatch_system, approval_result_system};
pub use approval_hook::{on_approval_requested_hook_system, on_approval_resolved_hook_system};
pub use async_dispatch::async_tool_dispatch_system;
pub use backend::NativeProcessBackend;
pub use cancel_monitor::cancel_monitor_system;
pub use channel_send_dispatch::channel_send_dispatch_system;
pub use confirmation::tool_confirmation_result_system;
pub use dispatch::tool_dispatch_system;
pub use effect_commit::commit_tool_effects_system;
pub use ingest_tool_results::ingest_tool_results_system;
pub use result::tool_result_system;
pub use tool_called_hook::on_tool_called_hook_system;
pub use tool_returned_hook::on_tool_returned_hook_system;
pub use waiting::{check_waiting_tasks_system, on_subtask_completed_check_waiting};
pub use skill_dir_resolver::resolve_current_skill_dir;

use crate::domain::{
    BuiltinToolExecutors, SpaceToolRegistry, ToolDefinition, ToolExecutorKind, ToolPermission,
    ToolSchema,
};

use self::builtin::{
    AskUserTool, ChatWithAgentTool, CreateTasksTool, DeleteScheduledTaskTool,
    ListExperienceCandidatesTool, ListScheduledTasksTool, ReadSkillFileTool, ScheduleTaskTool,
    ShellExecTool, ShellInputTool, ShellListTool, ShellReadTool, ShellStartTool, ShellStopTool,
    SkipProfileUpdateTool, SubmitExperienceCandidateTool, SubmitProfileUpdateTool, SubmitSkillTool,
    SubmitSkillUpdateTool, WaitTasksTool, WriteSkillFileTool,
};
use crate::channels::send_tool::ChannelSendTool;

/// 注册内置 Tool
pub fn register_builtin_tools(
    registry: &mut SpaceToolRegistry,
    executors: &mut BuiltinToolExecutors,
) {
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
        description: "提交经验候选。knowledge=事实性知识（知道什么），skill=可复用操作步骤（会做什么，含具体命令/指令/流程）。kind=skill 时 instructions 必须是 markdown 格式且至少包含 1 个 `## Section` 二级标题，落盘前框架会做结构校验。".to_string(),
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
                        "description": "经验类型：knowledge=纯事实性知识，skill=可复用操作步骤（含命令/指令/SOP）"
                    },
                    "content": {
                        "type": "string",
                        "description": "knowledge 类的事实性知识正文"
                    },
                    "skill_description": {
                        "type": "string",
                        "description": "skill 类的简要描述，说明这个技能做什么+何时触发"
                    },
                    "instructions": {
                        "type": "string",
                        "description": "skill 类的分步指令正文，必须是 markdown 格式，至少包含 1 个 `## Section` 二级标题（如 `## Usage`），可用 `### Subsection` 三级标题组织子章节；不要使用 `####` 或更深层级。落盘前框架会做结构校验，不符合则候选置 WritebackFailed。"
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

    // ask_user tool
    registry.register(ToolDefinition {
        name: "ask_user".to_string(),
        description: "向用户提出问题并等待回复。当需要用户提供偏好、确认方向或补充信息时调用。\
                      问题应清晰具体，让用户能直接文本回复。"
            .to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "向用户提出的问题文本"
                    }
                },
                "required": ["question"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("ask_user".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(AskUserTool));

    // chat_with_agent tool
    registry.register(ToolDefinition {
        name: "chat_with_agent".to_string(),
        description: "与一个持久化 Agent 开始或继续多轮对话。第一轮不传 handle，后续轮次传入 handle。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "目标 Persistent Agent 名称。第一轮必填；后续若提供可用来校验。"
                    },
                    "agent_tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "agent 不存在时的备选匹配标签。第一轮至少提供 agent 或 agent_tags 之一。"
                    },
                    "message": {
                        "type": "string",
                        "description": "本轮要发送给子 Agent 的消息。"
                    },
                    "handle": {
                        "type": "string",
                        "description": "已有对话的 handle（即子任务 task_id）。不传表示开始新对话。"
                    },
                    "context": {
                        "type": "string",
                        "description": "仅在第一轮生效的额外系统上下文。"
                    }
                },
                "required": ["message"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("chat_with_agent".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ChatWithAgentTool));

    // Channel send tool
    registry.register(ChannelSendTool::definition());
    executors.register(Box::new(ChannelSendTool));

    // schedule_task tool
    registry.register(ToolDefinition {
        name: "schedule_task".to_string(),
        description: "安排一个未来由 AI 执行的任务。支持一次性触发（once:ISO时间）或周期性 cron（cron:5字段表达式），结果会发送到指定输出通道。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "任务要执行的提示词/内容"
                    },
                    "schedule": {
                        "type": "string",
                        "description": "调度表达式。一次性: 'once:2026-07-07T09:00:00' 或 'once:2026-07-07T09:00:00+08:00'；周期性: 'cron:0 9 * * 1-5'（5字段：分 时 日 月 周）"
                    },
                    "output_channel": {
                        "type": "string",
                        "enum": ["tui", "telegram", "qq", "feishu", "web"],
                        "description": "可选，显式指定输出通道类型"
                    },
                    "target": {
                        "type": "string",
                        "description": "可选，输出通道内的目标标识（如 Telegram chat_id）；output_channel 提供时必填"
                    }
                },
                "required": ["content", "schedule"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("schedule_task".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ScheduleTaskTool));

    // list_scheduled_tasks tool —— pilot 首个异步工具（list 双账本只读）
    registry.register(ToolDefinition {
        name: "list_scheduled_tasks".to_string(),
        description: "列出当前空间内的动态定时任务（由 schedule_task 工具创建）。返回每个任务的 kind、content、output_channel、is_once、created_at、next_fire_time 等字段；next_fire_time 对 Once 任务显示原始触发时间，对 Cron 任务显示下次触发点。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("list_scheduled_tasks".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ListScheduledTasksTool));

    // delete_scheduled_task tool —— 写路径首个客户（声明式效果 → commit 落账）
    registry.register(ToolDefinition {
        name: "delete_scheduled_task".to_string(),
        description: "删除指定 kind 的动态定时任务（由 schedule_task 工具创建）。返回 {deleted: kind, existed: bool}——existed 表示删除时任务是否还存在（幂等可观测：删不存在的 kind 不会报错，仅 existed=false）。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "要删除的动态定时任务的 kind 字符串（即 list_scheduled_tasks 返回结果中的 kind 字段）"
                    }
                },
                "required": ["kind"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("delete_scheduled_task".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(DeleteScheduledTaskTool));

    // Profile update tools (仅 profile-designer 可用)
    registry.register(ToolDefinition {
        name: "submit_profile_update".to_string(),
        description: "提交生成或更新后的 Agent profile。孵化场景 name 作为最终 Agent 名称；更新场景 name 仅作参考，系统会强制使用原 name（不可变更）。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Agent 角色名，简洁有力"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "核心能力标签列表"},
                    "description": {"type": "string", "description": "Agent 职责描述，一到两句话概括"}
                },
                "required": ["name", "tags", "description"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("submit_profile_update".to_string()),
        required_tag: Some("profile".to_string()),
    });
    executors.register(Box::new(SubmitProfileUpdateTool));

    registry.register(ToolDefinition {
        name: "skip_profile_update".to_string(),
        description: "明确表示现有 Agent profile 不需要更新。仅在更新场景下使用。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({"type": "object", "properties": {}, "required": []}),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("skip_profile_update".to_string()),
        required_tag: Some("profile".to_string()),
    });
    executors.register(Box::new(SkipProfileUpdateTool));

    // Skill update tool (仅 skill-updater 可用)
    registry.register(ToolDefinition {
        name: "submit_skill_update".to_string(),
        description: "提交 skill 更新的结构化 diff 操作。基于原 skill 内容和经验候选，提交 operations 数组。skill_id、base_version、new_version 由系统自动从当前 skill update 上下文注入，无需填写。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "operations": {
                        "type": "array",
                        "description": "结构化 diff 操作数组，按顺序 apply 到 skill 文件",
                        "items": {
                            "type": "object",
                            "properties": {
                                "action": {
                                    "type": "string",
                                    "enum": [
                                        "replace_section", "add_section", "remove_section",
                                        "replace_subsection", "add_subsection", "remove_subsection",
                                        "replace_body", "replace_frontmatter",
                                        "replace_file", "create_file", "delete_file"
                                    ],
                                    "description": "操作类型"
                                },
                                "section": {
                                    "type": "string",
                                    "description": "目标章节标题（完整匹配，如 '## Usage'）。section 级操作必填"
                                },
                                "subsection": {
                                    "type": "string",
                                    "description": "目标子章节标题（如 '### Advanced'）。subsection 级操作必填"
                                },
                                "after": {
                                    "type": "string",
                                    "description": "新增章节/子章节插入位置（在某章节之后）。add_section/add_subsection 必填"
                                },
                                "content": {
                                    "type": "string",
                                    "description": "新内容（markdown 文本）。替换/新增操作必填"
                                },
                                "field": {
                                    "type": "string",
                                    "enum": ["name", "description", "self_updatable"],
                                    "description": "frontmatter 字段名。replace_frontmatter 必填"
                                },
                                "value": {
                                    "type": "string",
                                    "description": "frontmatter 字段新值（self_updatable 接受 'true'/'false'）。replace_frontmatter 必填"
                                },
                                "path": {
                                    "type": "string",
                                    "description": "目标文件路径（相对于 skill 目录，如 'download.md'）。section 级操作默认作用于 SKILL.md；指定 path 可作用于其他 .md 文件（必须已存在且后缀为 .md）。replace_file/create_file/delete_file 的 path 必填且不可为 SKILL.md"
                                }
                            },
                            "required": ["action"]
                        }
                    },
                    "rationale": {"type": "string", "description": "本次更新的理由说明"}
                },
                "required": ["operations", "rationale"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("submit_skill_update".to_string()),
        required_tag: Some("skill-updater".to_string()),
    });
    executors.register(Box::new(SubmitSkillUpdateTool));

    // ADR-006：read_skill_file tool (仅 skill-updater 可用)
    registry.register(ToolDefinition {
        name: "read_skill_file".to_string(),
        description: "读取当前 skill 目录下的子文件内容。SKILL.md 的内容已在 prompt 中提供，无需再读取。用于了解子流程文档、脚本、模板等内容，以便提交精确的 diff 更新。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "相对于 skill 目录的文件路径（如 'download.md'、'scripts/redmine_download.py'）"
                    }
                },
                "required": ["path"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("read_skill_file".to_string()),
        required_tag: Some("skill-updater".to_string()),
    });
    executors.register(Box::new(ReadSkillFileTool));

    // Skill creation tools (仅 skill-creator 可用)
    registry.register(ToolDefinition {
        name: "submit_skill".to_string(),
        description: "提交 skill 创建候选。提供 skill 名称与描述，由 skill-creator Agent 调用。"
            .to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "skill 名称，简洁唯一标识"
                    },
                    "description": {
                        "type": "string",
                        "description": "skill 描述，说明这个技能做什么以及何时触发"
                    }
                },
                "required": ["name", "description"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("submit_skill".to_string()),
        required_tag: Some("skill-creator".to_string()),
    });
    executors.register(Box::new(SubmitSkillTool));

    registry.register(ToolDefinition {
        name: "write_skill_file".to_string(),
        description: "在 skill 沙盒目录下写入文件。用于创建 SKILL.md 或辅助文件（脚本、模板等）。路径必须相对沙盒目录，禁止 .. 遍历，文件后缀必须在白名单内。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "相对于 skill 沙盒目录的文件路径（如 'SKILL.md'、'scripts/run.py'）"
                    },
                    "content": {
                        "type": "string",
                        "description": "文件内容"
                    }
                },
                "required": ["path", "content"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("write_skill_file".to_string()),
        required_tag: Some("skill-creator".to_string()),
    });
    executors.register(Box::new(WriteSkillFileTool));
}

/// 注册插件贡献的 Tool
///
/// 扫描 PluginRegistry 中所有已加载插件的 tools 声明，
/// 以 `plugin_id:tool_id` 命名空间注册到 SpaceToolRegistry 和 BuiltinToolExecutors。
/// Schema 文件无法解析或校验不通过的工具会被跳过并记录警告日志。
pub fn register_plugin_tools(
    registry: &mut SpaceToolRegistry,
    executors: &mut BuiltinToolExecutors,
    plugin_registry: &crate::user_plugins::registry::PluginRegistry,
) {
    use crate::user_plugins::tool_executor::{RhaiPluginAsyncWrapper, RhaiToolExecutor};
    use tracing::warn;

    for plugin in plugin_registry.plugins() {
        for tool_def in &plugin.manifest.tools {
            let namespaced = format!("{}:{}", plugin.manifest.id, tool_def.id);

            // 读取 schema 文件
            let schema_path = plugin.root_dir.join(&tool_def.schema);
            let schema_str = match std::fs::read_to_string(&schema_path) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        event = "PluginToolSchemaReadFailed",
                        plugin_id = %plugin.manifest.id,
                        tool_id = %tool_def.id,
                        error = %e,
                        "skipping plugin tool: cannot read schema file"
                    );
                    continue;
                }
            };

            let schema_value: serde_json::Value = match serde_json::from_str(&schema_str) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        event = "PluginToolSchemaParseFailed",
                        plugin_id = %plugin.manifest.id,
                        tool_id = %tool_def.id,
                        error = %e,
                        "skipping plugin tool: schema is not valid JSON"
                    );
                    continue;
                }
            };

            // 校验 schema 是否为合法 JSON Schema
            if let Err(e) = jsonschema::validator_for(&schema_value) {
                warn!(
                    event = "PluginToolSchemaInvalid",
                    plugin_id = %plugin.manifest.id,
                    tool_id = %tool_def.id,
                    error = %e,
                    "skipping plugin tool: schema validation failed"
                );
                continue;
            }

            let default_permission = tool_def
                .default_permission
                .unwrap_or(ToolPermission::Confirm);

            registry.register(ToolDefinition {
                name: namespaced.clone(),
                description: tool_def.description.clone(),
                parameters: ToolSchema {
                    schema: schema_value,
                },
                default_permission,
                executor: ToolExecutorKind::Builtin(namespaced.clone()),
                required_tag: None,
            });

            // loader 阶段已预编译 AST 并存入 tool_asts；此处取回用于执行器。
            // 若 AST 缺失（理论上不应发生——loader 在 schema 无效时已 continue），
            // 跳过该工具并 warn，保持与 schema 失败一致的降级语义。
            let ast = match plugin.tool_asts.get(&tool_def.id) {
                Some(ast) => ast.clone(),
                None => {
                    warn!(
                        event = "PluginToolAstMissing",
                        plugin_id = %plugin.manifest.id,
                        tool_id = %tool_def.id,
                        "skipping plugin tool: pre-compiled AST not found"
                    );
                    continue;
                }
            };

            let executor = RhaiToolExecutor::new(
                &plugin.manifest.id,
                &tool_def.id,
                ast,
                tool_def.timeout_secs,
            );
            executors.register(Box::new(RhaiPluginAsyncWrapper::new(executor)));

            tracing::info!(
                event = "PluginToolRegistered",
                namespaced = %namespaced,
                "plugin tool registered"
            );
        }
    }
}

// AgentToolPermissions 的查询行为由 effective_permission 统一覆盖，
// 见 src/domain/agent.rs tests::effective_permission_*。
// 构造/默认值行为见 default_permission_explicit_defaults_to_false。
