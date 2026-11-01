//! Tool 执行模块
//!
//! 实现 Tool 的分发、执行和结果处理。

mod approval;
mod approval_gate;
mod approval_hook;
mod async_dispatch;
pub mod backend;
mod background;
pub mod builtin;
mod cancel_monitor;
mod channel_send_dispatch;
mod confirmation;
mod dispatch;
mod effect_commit;
mod ingest_tool_results;
mod orchestrator;
mod result;
mod tool_called_hook;
mod tool_calling;
mod tool_returned_hook;
mod waiting;

pub use approval::{approval_dispatch_system, approval_result_system};
pub use approval_gate::approval_sender_gate_system;
pub use approval_hook::{on_approval_requested_hook_system, on_approval_resolved_hook_system};
pub use async_dispatch::async_tool_dispatch_system;
pub use backend::NativeProcessBackend;
pub use background::tool_auto_background_system;
pub use cancel_monitor::cancel_monitor_system;
pub use channel_send_dispatch::channel_send_dispatch_system;
pub use confirmation::tool_confirmation_result_system;
pub use dispatch::tool_dispatch_system;
pub use effect_commit::commit_tool_effects_system;
pub use ingest_tool_results::ingest_tool_results_system;
pub use result::tool_result_system;
pub use tool_calling::tool_calling_orchestrator_system;

pub use tool_called_hook::on_tool_called_hook_system;
pub(crate) use tool_calling::{
    CallingStateInfo, ToolLoopPolicy, find_calling_state, handle_tool_calls_response,
};
pub use tool_returned_hook::on_tool_returned_hook_system;
pub use waiting::{WaitingIndex, check_waiting_tasks_system};

use bevy_ecs::change_detection::Mut;
use bevy_ecs::world::World;

use crate::domain::{
    BuiltinToolExecutors, FrontendKind, SpaceToolRegistry, ToolDefinition, ToolExecutorKind,
    ToolPermission, ToolSchema,
};

use self::builtin::{
    AskUserTool, ChatWithAgentTool, CreateTasksTool, DeleteScheduledTaskTool,
    ListScheduledTasksTool, ScheduleTaskTool, SearchExperienceTool, ShellExecTool, ShellInputTool,
    ShellListTool, ShellReadTool, ShellStartTool, ShellStopTool, StopBackgroundJobTool,
    UpdateScheduledTaskTool, UpdateTodosTool, ViewImageTool, WaitTasksTool,
};
use crate::channels::send_tool::ChannelSendTool;

/// 注册内置 Tool
pub fn register_builtin_tools(
    registry: &mut SpaceToolRegistry,
    executors: &mut BuiltinToolExecutors,
) {
    registry.register(ToolDefinition {
        name: "create_tasks".to_string(),
        description: "Create sub-tasks to delegate work to specialized child agents. Supports creating multiple tasks with dependency ordering. Tasks without dependencies will run in parallel; tasks with dependencies will wait for them to complete. Sub-tasks run in the background and return a job_id: keep working, or end your turn if you depend on their results; a <task-notification> with all results is delivered automatically when the batch finishes and starts a new turn. Do not poll: a watchdog supervises sub-task progress, nudges stalled sub-tasks and cancels hopeless ones, so you do not need to monitor them.".to_string(),
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
        description: "Wait briefly for child tasks to complete and collect their results. If they are still running after the foreground budget, returns running_in_background with current statuses; the full results then arrive as a <task-notification>, so never call wait_tasks repeatedly.".to_string(),
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

    registry.register(ToolDefinition {
        name: "stop_background_job".to_string(),
        description: "停止本任务的后台作业：自动转后台的工具调用，或 create_tasks 派出的子任务批次（取消其中未完成的子任务）。job_id 来自 running_in_background / batch_created 结果。被停止的作业不再发送完成通知。shell_start 会话请用 shell_stop。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "后台作业 ID" }
                },
                "required": ["job_id"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("stop_background_job".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(StopBackgroundJobTool));

    // Shell tools
    registry.register(ToolDefinition {
        name: "shell_exec".to_string(),
        description: "执行一次性 shell 命令并等待结果，适合 build、test、lint、文件操作。命令超过前台预算仍未结束会自动转后台并返回 running_in_background 与 job_id，结果稍后以 <task-notification> 送达；此时不要重复执行或轮询。返回 command_succeeded、exit_code、timed_out；工具返回不代表命令成功。命令结束或超时会清理同组后台后代，nohup 和 & 不能让它们持续运行。长期监控、服务或耗时观察请用 shell_start，并以前台方式运行；不要改用外部服务管理器规避 Harness 生命周期，除非用户明确要求独立部署。".to_string(),
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
        description: "启动受管的长期或交互 shell 会话，命令以前台方式运行，不加 nohup、& 或自守护。返回 session_id 供读取、输入或停止，Harness 退出时清理进程。持续监控且需要告警时设置 notify_on_output=true：stdout 完整非空行经 Harness 直接发送到任务输出通道并回传发送结果，无需另建定时任务读文件或让 Agent 重复转发。stdout 只写可直接发送的告警/恢复信息并及时 flush，诊断写 stderr；默认不转发输出，退出默认通知。先确认目标系统与采样接口；明确十进制 MB/s 或二进制 MiB/s、接口范围与收发口径。速率按实际单调时间差计算，不能把固定 sleep 时长当采样间隔。先用可控输入分别验证采样、阈值和恢复，再验证会话存活与采样健康；测试要覆盖实际投递回执，打印告警不等于通知成功。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的 shell 命令" },
                    "cwd": { "type": "string", "description": "命令工作目录" },
                    "notify_on_exit": { "type": "boolean", "description": "会话退出后是否通知（默认 true；常驻服务可设为 false）" },
                    "notify_on_output": { "type": "boolean", "description": "stdout 完整行直接发给用户并通知任务（默认 false；stderr 不转发，需有任务输出通道）" }
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

    // 图片查看：返回 [IMAGE:...] 标记，视觉模型下一轮请求中会看到真实图片
    registry.register(ToolDefinition {
        name: "view_image".to_string(),
        description: "查看本地图片文件（png/jpeg/gif/webp，不超过 5 MiB）。适合查看截图、下载的图片或生成的图表。仅当当前模型支持视觉输入时才能看到图片内容，否则只返回路径信息。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "图片路径，相对路径基于工作目录" }
                },
                "required": ["path"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("view_image".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ViewImageTool));

    // 经验库检索：判断交给经验管理员，主 Agent 只拿到相关经验原文
    registry.register(ToolDefinition {
        name: "search_experience".to_string(),
        description: "在经验库中查找以往任务沉淀的试错经验（验证过的做法与走不通的路线）。遇到报错，或准备尝试一个不确定能否成功的方案之前，先调用本工具，避免重复已经失败过的尝试。由经验管理员判断哪些经验适用并返回原文；没有相关经验时会明确说明。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "situation": {
                        "type": "string",
                        "description": "用自然语言描述当前要做的事、所处环境和遇到的现象；有报错时附上关键报错原文"
                    }
                },
                "required": ["situation"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("search_experience".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(SearchExperienceTool));

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
        description: "安排一个未来由 AI 执行的任务。支持一次性触发（once:ISO时间）或周期性 cron（cron:5字段表达式），notify=reply 自动回复到指定输出通道，explicit 仅在 TUI 显示最终回复（需要通知时调用 channel_send）；失败始终通知。report_of 可关联被观察的定时任务 UUID。定时任务会持久保存，进程重启后恢复；停机期间错过的运行在 7 天内只补跑最近一次。定时任务独立于当前任务存在，当前任务结束后仍会继续运行，可用 update_scheduled_task 暂停或修改、delete_scheduled_task 删除。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "任务要执行的提示词/内容"
                    },
                    "name": {
                        "type": "string",
                        "description": "可选，便于管理的唯一名称（如 forum-watch）；缺省取 id 前 8 位，重名会被拒绝"
                    },
                    "notify": {
                        "type": "string", "enum": ["reply", "explicit"],
                        "description": "reply 自动推送最终回复；explicit 仅在 TUI 显示，调用 channel_send 才通知。失败始终通知"
                    },
                    "report_of": {
                        "type": ["string", "null"],
                        "description": "被观察定时任务的 UUID，注入其最近24小时统计和健康；null 清除关联"
                    },
                    "schedule": {
                        "type": "string",
                        "description": "调度表达式。一次性: 'once:2026-07-07T09:00:00' 或 'once:2026-07-07T09:00:00+08:00'；周期性: 'cron:0 9 * * 1-5'（5字段：分 时 日 月 周）"
                    },
                    "output_channel": {
                        "type": "string",
                        "enum": FrontendKind::ALL.iter().map(|k| k.channel_name()).collect::<Vec<_>>(),
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
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("schedule_task".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(ScheduleTaskTool));

    // list_scheduled_tasks tool —— pilot 首个异步工具（list 双账本只读）
    registry.register(ToolDefinition {
        name: "list_scheduled_tasks".to_string(),
        description: "列出当前空间内的动态定时任务（由 schedule_task 工具创建）。返回每个任务的 kind、name、content、schedule、output_channel、is_once、enabled、created_at、last_fired_at、last_success_at、consecutive_failures、alerting、notify、report_of、next_fire_time 等字段；next_fire_time 对 Once 任务显示原始触发时间，对 Cron 任务显示下次触发点，已暂停的任务不返回该字段。".to_string(),
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
        description: "删除指定的动态定时任务（由 schedule_task 工具创建），按 kind 或 name 定位。返回 {deleted, existed}——existed 表示删除时任务是否还存在（幂等可观测：删不存在的任务不会报错，仅 existed=false）。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "要删除的定时任务的 kind（list_scheduled_tasks 返回的 kind 字段）；与 name 二选一"
                    },
                    "name": {
                        "type": "string",
                        "description": "要删除的定时任务名称；与 kind 二选一"
                    }
                }
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("delete_scheduled_task".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(DeleteScheduledTaskTool));

    // update_scheduled_task tool —— 修改 / 暂停恢复 / 立即运行（声明式效果 → commit 落账）
    registry.register(ToolDefinition {
        name: "update_scheduled_task".to_string(),
        description: "修改指定的动态定时任务，按 kind 或 name 定位：可修改 content、schedule、notify（reply/explicit）、report_of（UUID，null 清除），用 enabled 暂停（false）或恢复（true），用 run_now 立即手动运行一次（暂停中也可运行；上一次运行未结束时会被跳过）。至少提供一项修改。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "定时任务的 kind；与 name 二选一"
                    },
                    "name": {
                        "type": "string",
                        "description": "定时任务名称；与 kind 二选一"
                    },
                    "content": {
                        "type": "string",
                        "description": "可选，新的任务提示词"
                    },
                    "notify": {
                        "type": "string", "enum": ["reply", "explicit"],
                        "description": "reply 自动推送最终回复；explicit 仅在 TUI 显示，调用 channel_send 才通知。失败始终通知"
                    },
                    "report_of": {
                        "type": ["string", "null"],
                        "description": "被观察定时任务的 UUID，注入其最近24小时统计和健康；null 清除关联"
                    },
                    "schedule": {
                        "type": "string",
                        "description": "可选，新的调度表达式，格式同 schedule_task"
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "可选，false 暂停、true 恢复"
                    },
                    "run_now": {
                        "type": "boolean",
                        "description": "可选，为 true 时立即手动运行一次"
                    }
                }
            }),
        },
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("update_scheduled_task".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(UpdateScheduledTaskTool));

    registry.register(ToolDefinition {
        name: "update_todos".to_string(),
        description: "维护当前任务的进度清单，向用户展示你正在做哪一步。每次提交完整列表（覆盖旧列表），同一时间最多一项 in_progress；多步骤任务开始时建立清单，每完成一步及时更新；提交空列表可清空。仅用于展示进度，不会执行任何工作——需要委派或并行执行请使用 create_tasks。".to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "完整的进度清单（最多 30 项）",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "步骤描述（祈使句，简短）" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("update_todos".to_string()),
        required_tag: None,
    });
    executors.register(Box::new(UpdateTodosTool));
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

/// 从 World 中的 `PluginRegistry` 资源拉取并注册插件工具。
///
/// 方向反转的落点：user_plugins 侧只负责加载 registry 与技能贡献，
/// 工具注册知识归 tools 系统，由装配方（Startup / reload）主动调用。
pub fn register_plugin_tools_in_world(world: &mut World) {
    if !world.contains_resource::<crate::user_plugins::registry::PluginRegistry>() {
        return;
    }
    world.resource_scope(
        |world: &mut World, registry: Mut<crate::user_plugins::registry::PluginRegistry>| {
            world.resource_scope(
                |world: &mut World, mut space: Mut<crate::domain::SpaceToolRegistry>| {
                    if let Some(mut execs) =
                        world.get_resource_mut::<crate::domain::BuiltinToolExecutors>()
                    {
                        register_plugin_tools(&mut space, &mut execs, &registry);
                    }
                },
            );
        },
    );
}

/// Startup 系统：在 `plugin_load_startup_system` 之后注册插件工具。
pub fn register_plugin_tools_startup_system(world: &mut World) {
    register_plugin_tools_in_world(world);
}

// AgentToolPermissions 的查询行为由 effective_permission 统一覆盖，
// 见 src/domain/agent.rs tests::effective_permission_*。
// 构造/默认值行为见 default_permission_explicit_defaults_to_false。
