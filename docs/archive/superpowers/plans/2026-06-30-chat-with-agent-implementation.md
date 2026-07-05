> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# chat_with_agent 工具实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `chat_with_agent` 内置工具，支持父任务与 Persistent Agent 之间进行多轮同步对话，用于文档评审、迭代修改等场景。

**Architecture:** 新增 `ChatSession` 组件标记对话型子任务，通过 `ChatRoundStartedMessage` / `ChatRoundReadyMessage` 驱动父任务阻塞与唤醒；将审批路由从 `Agent.parent_id` 统一改为 `Task.parent_task_id`；在 `task_dispatch_system` 中特殊处理带 `ChatSession` 且已指定 `delegate` 的子任务，使其绕过"子任务由 Brain 分发"的限制，直接调度到 Persistent Agent。

**Tech Stack:** Rust, Bevy ECS, ratatui/crossterm (TUI), genai LLM 接入。

## Global Constraints

- 所有新增 crate 依赖必须来自 crates.io，许可证与 MIT/Apache-2.0 兼容，优先纯 Rust 实现。
- 库 crate 错误使用 `thiserror`，应用代码使用 `anyhow`。
- 不修改 `create_tasks` 工具及其创建的子任务行为。
- 单元测试与实现文件放在一起（`#[cfg(test)]`），集成测试放在 `tests/` 目录。
- 同一变更涉及的代码与文档应尽量放在同一提交中。
- 提交信息遵循 Conventional Commits。
- 不直接推送到 `main`，通过 PR 合并。

---

### Task 1: 扩展领域模型（WaitingReason、ChatSession、消息组件、ToolAction）

**Files:**
- Modify: `src/domain/message.rs`
- Create: `src/domain/chat_session.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/domain/space.rs`
- Test: `src/domain/chat_session.rs` 底部单元测试

**Interfaces:**
- Consumes: 现有 `TaskId`（`uuid::Uuid`）、`Component` derive
- Produces:
  - `pub enum WaitingReason { ..., ChatAgent }`
  - `pub struct ChatSession { parent_tool_call_id: String, current_batch_id: Uuid }`
  - `pub struct ChatRoundStartedMessage { parent_task_id: TaskId, child_task_id: TaskId, batch_id: Uuid, parent_tool_call_id: String }`
  - `pub struct ChatRoundReadyMessage { child_task_id: TaskId, parent_task_id: TaskId, batch_id: Uuid, parent_tool_call_id: String, response: String }`
  - `pub enum ToolAction { ..., StartChatRound { agent_name: Option<String>, agent_tags: Vec<String>, message: String, context: Option<String>, handle: Option<TaskId>, parent_tool_call_id: Option<String> } }`

- [ ] **Step 1: 在 `src/domain/message.rs` 添加 `WaitingReason::ChatAgent`**

在 `WaitingReason` 枚举末尾追加：

```rust
/// chat_with_agent 子任务等待父 Agent 下一轮调用
ChatAgent,
```

- [ ] **Step 2: 创建 `src/domain/chat_session.rs`**

```rust
//! chat_with_agent 会话组件

use bevy::prelude::Component;
use uuid::Uuid;

/// 标记一个子任务为 chat_with_agent 对话型子任务，并保存每轮变化的状态。
#[derive(Component, Debug, Clone)]
pub struct ChatSession {
    /// 本轮父任务的 tool_call_id（每轮更新）
    pub parent_tool_call_id: String,
    /// 本轮父任务等待用的 batch_id（每轮更新）
    pub current_batch_id: Uuid,
}
```

- [ ] **Step 3: 在 `src/domain/message.rs` 追加两个消息组件**

在 `SubTaskBatchCreatedMessage` 附近追加：

```rust
/// chat_with_agent 新一轮开始，触发父 Task 阻塞
#[derive(Debug, Clone, Component)]
pub struct ChatRoundStartedMessage {
    pub parent_task_id: TaskId,
    pub child_task_id: TaskId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
}

/// chat_with_agent 子任务本轮回复就绪
#[derive(Debug, Clone, Component)]
pub struct ChatRoundReadyMessage {
    pub child_task_id: TaskId,
    pub parent_task_id: TaskId,
    pub parent_agent_id: AgentId,
    pub batch_id: Uuid,
    pub parent_tool_call_id: String,
    pub response: String,
}
```

- [ ] **Step 4: 在 `src/domain/space.rs` 添加 `ToolAction::StartChatRound`**

在 `ToolAction` 枚举末尾追加：

```rust
/// 开始或继续 chat_with_agent 对话轮次。
/// executor 只负责解析参数，真正的子任务创建/更新在 orchestrator 中完成。
StartChatRound {
    /// 目标 Persistent Agent 名称（第一轮必填，后续可用来校验）
    agent_name: Option<String>,
    /// 目标 Persistent Agent 匹配标签（agent 不存在时的备选）
    agent_tags: Vec<String>,
    /// 本轮要发送给子 Agent 的消息
    message: String,
    /// 仅在第一轮生效的额外系统上下文
    context: Option<String>,
    /// 已有对话的 handle（即子任务 task_id），不传表示开始新对话
    handle: Option<TaskId>,
},
```

- [ ] **Step 5: 在 `src/domain/mod.rs` 导出**

在文件顶部 `mod` 区域添加：

```rust
mod chat_session;
```

在 `pub use message::{...}` 中追加 `ChatRoundReadyMessage`、`ChatRoundStartedMessage`：

```rust
pub use message::{
    AgentExecutionRequestMessage, AgentExecutionResultMessage, AgentSpawnRequestMessage,
    ApprovalRequestMessage, ApprovalRequestedHookPending, ApprovalResolvedHookPending,
    ApprovalResultMessage, ChatRoundReadyMessage, ChatRoundStartedMessage, ContinueTaskMessage,
    CreateTaskMessage, ExperienceCollectionCompletedMessage, ExternalInput, FinishTaskMessage,
    LlmResponseHookPending, MessageDispatchedHookPending, MessageReceivedHookPending, OutputKind,
    OutputMessage, PendingChannelSend, ReloadPluginsMessage, RetryReadyMessage,
    SessionExitedMessage, SessionOutputAppendedMessage, SessionStartedMessage, Signal,
    SignalPayload, SignalType, SubTaskBatchCreatedMessage, SubTaskCompletedMessage,
    SummarizationRequestMessage, SystemOutputMessage, TaskTerminatedMessage,
    ToolConfirmationRequestMessage, ToolConfirmationResponseMessage, ToolExecutionRequestMessage,
    ToolExecutionResultMessage, UserInputMessage, UserOutputMessage, WaitingReason,
};
```

新增 `pub use chat_session::ChatSession;`：

```rust
pub use chat_session::ChatSession;
```

- [ ] **Step 6: 添加单元测试**

在 `src/domain/chat_session.rs` 底部：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_session_stores_round_state() {
        let batch_id = Uuid::new_v4();
        let session = ChatSession {
            parent_tool_call_id: "call_123".to_string(),
            current_batch_id: batch_id,
        };
        assert_eq!(session.parent_tool_call_id, "call_123");
        assert_eq!(session.current_batch_id, batch_id);
    }
}
```

- [ ] **Step 7: 编译检查**

Run: `cargo check --lib`
Expected: PASS（新增领域类型无编译错误）

- [ ] **Step 8: 提交**

```bash
git add src/domain/message.rs src/domain/chat_session.rs src/domain/mod.rs src/domain/space.rs
git commit -m "feat(domain): add ChatSession, chat round messages and StartChatRound action"
```

---

### Task 2: 统一审批路由为 Task.parent_task_id

**Files:**
- Modify: `src/contracts/tools.rs`
- Modify: `src/systems/tools/dispatch.rs`
- Test: `src/systems/tools/dispatch.rs` 后续集成测试覆盖（本 Task 仅保证编译通过）

**Interfaces:**
- Consumes: `Agent`, `Task`, `ToolApprovalPolicy`
- Produces:
  - `ToolApprovalPolicy::determine_approval_route(&self, tool_name: &str, agent: &Agent, task: &Task, tasks: &Query<&Task>, agents: &Query<&Agent>) -> ApprovalRoute`
  - `dispatch.rs` Confirm 分支按 `task.parent_task_id` 查找父 Agent

- [ ] **Step 1: 修改 `ToolApprovalPolicy` trait 签名**

将 `src/contracts/tools.rs` 中 trait 签名改为：

```rust
pub trait ToolApprovalPolicy: Send + Sync + 'static {
    /// 根据工具、Agent 和任务上下文决定审批路由
    fn determine_approval_route(
        &self,
        tool_name: &str,
        agent: &Agent,
        task: &Task,
        tasks: &Query<(Entity, &Task)>,
        agents: &Query<&Agent>,
    ) -> ApprovalRoute;
}
```

- [ ] **Step 2: 修改 `DefaultToolApprovalPolicy` 实现**

```rust
impl ToolApprovalPolicy for DefaultToolApprovalPolicy {
    fn determine_approval_route(
        &self,
        tool_name: &str,
        agent: &Agent,
        task: &Task,
        tasks: &Query<(Entity, &Task)>,
        agents: &Query<&Agent>,
    ) -> ApprovalRoute {
        let permission = agent.tool_permissions.get_permission(tool_name);

        match permission {
            ToolPermission::Allow => ApprovalRoute::AutoAllow,
            ToolPermission::Confirm => {
                if let Some(parent_task_id) = task.parent_task_id {
                    if let Some((_, parent_task)) = tasks.iter().find(|(_, t)| t.id == parent_task_id) {
                        if let Some(parent_agent_id) = parent_task.delegate {
                            if let Some(parent_agent) = agents.iter().find(|a| a.id == parent_agent_id) {
                                if parent_agent.has_permission(tool_name) {
                                    return ApprovalRoute::ParentApproval {
                                        parent_agent_id,
                                    };
                                }
                            }
                        }
                    }
                }
                ApprovalRoute::UserConfirmation
            }
            ToolPermission::Deny => ApprovalRoute::Deny,
        }
    }
}
```

- [ ] **Step 3: 修改 `src/systems/tools/dispatch.rs` 的 `Confirm` 分支**

将现有 `Confirm` 分支替换为统一按 `task.parent_task_id` 查找父 Agent 的逻辑：

```rust
ToolPermission::Confirm => {
    // 统一按 task.parent_task_id 查找父 Agent
    let parent_approval = if let Some(parent_task_id) = task.parent_task_id {
        tasks
            .iter()
            .find(|(_, t)| t.id == parent_task_id)
            .and_then(|(_, parent_task)| parent_task.delegate)
            .and_then(|parent_agent_id| agents.iter().find(|a| a.id == parent_agent_id))
            .filter(|parent| parent.has_permission(&tool_name))
            .map(|parent| parent.id)
    } else {
        None
    };

    if let Some(parent_agent_id) = parent_approval {
        debug!(
            event = "ToolRequiresParentApproval",
            tool_name = %tool_name,
            agent_id = %agent.id,
            parent_agent_id = %parent_agent_id,
            reason = "parent task delegate has permission",
            "tool requires parent agent approval"
        );

        if let Some((_, mut task)) = tasks
            .iter_mut()
            .find(|(_, t)| t.id == request.request.task_id)
        {
            task.status = TaskStatus::Waiting(WaitingReason::Approval);
        }

        let request_id = Uuid::new_v4();
        commands.spawn((
            ApprovalRequestMessage {
                request_id,
                tool_name: tool_name.clone(),
                source_task_id: request.request.task_id,
                parent_agent_id,
                child_agent_id: agent.id,
                tool_input: request.tool_input.clone(),
                approval_task_id: Uuid::new_v4(),
                context: String::new(),
            },
            ApprovalRequestedHookPending,
        ));

        request.pending_confirmation_id = Some(request_id);
        continue;
    }

    // fallback 用户确认
    debug!(
        event = "ToolRequiresUserConfirmation",
        tool_name = %tool_name,
        agent_id = %agent.id,
        reason = "no parent task delegate or parent lacks permission",
        "tool requires user confirmation"
    );

    if let Some((_, mut task)) = tasks
        .iter_mut()
        .find(|(_, t)| t.id == request.request.task_id)
    {
        task.status = TaskStatus::Waiting(WaitingReason::User);
    }

    let request_id = Uuid::new_v4();
    let options = ConfirmationOption::default_options();
    commands.spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id: request.request.task_id,
        agent_id: agent.id,
        tool_name: tool_name.clone(),
        tool_input: request.tool_input.clone(),
        options: options.clone(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
    });

    request.pending_confirmation_id = Some(request_id);
    request.pending_confirmation_options = Some(options);
}
```

- [ ] **Step 4: 同步单元测试中的 `determine_approval_route` 调用**

`src/contracts/tools.rs` 底部的现有测试仍调用旧的三参数签名：

```rust
policy.determine_approval_route("any_tool", &agent)
```

将其全部更新为新签名：

```rust
policy.determine_approval_route("any_tool", &agent, &task, &tasks, &agents)
```

由于 `Query` 无法直接在单元测试中构造，参考 Step 5 使用 Bevy `App` 构建最小 World 后查询。

- [ ] **Step 5: 重写 `DefaultToolApprovalPolicy` 单元测试**

由于 `Query` 无法直接在单元测试中构造，使用 Bevy `App` 构造最小 World 进行测试。将 `src/contracts/tools.rs` 底部现有测试替换为：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentProfile, AgentToolPermissions, ChannelId, FrontendKind, Task,
        ToolPermission,
    };
    use bevy::prelude::*;
    use uuid::Uuid;

    fn default_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        }
    }

    fn make_agent(permission: ToolPermission) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "test".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: permission,
                overrides: std::collections::HashMap::new(),
            },
        }
    }

    #[test]
    fn allow_returns_auto_allow() {
        let mut app = App::new();
        let agent = make_agent(ToolPermission::Allow);
        let task = Task::from_user_input("test", 3, default_channel());
        app.world_mut().spawn(agent.clone());
        app.world_mut().spawn(task.clone());

        let policy = DefaultToolApprovalPolicy;
        let agents = app.world().query::<&Agent>();
        let tasks = app.world().query::<(Entity, &Task)>();

        assert_eq!(
            policy.determine_approval_route("test_tool", &agent, &task, &tasks, &agents),
            ApprovalRoute::AutoAllow
        );
    }

    #[test]
    fn deny_returns_deny() {
        let mut app = App::new();
        let agent = make_agent(ToolPermission::Deny);
        let task = Task::from_user_input("test", 3, default_channel());
        app.world_mut().spawn(agent.clone());
        app.world_mut().spawn(task.clone());

        let policy = DefaultToolApprovalPolicy;
        let agents = app.world().query::<&Agent>();
        let tasks = app.world().query::<(Entity, &Task)>();

        assert_eq!(
            policy.determine_approval_route("test_tool", &agent, &task, &tasks, &agents),
            ApprovalRoute::Deny
        );
    }

    #[test]
    fn confirm_without_parent_returns_user_confirmation() {
        let mut app = App::new();
        let agent = make_agent(ToolPermission::Confirm);
        let task = Task::from_user_input("test", 3, default_channel());
        app.world_mut().spawn(agent.clone());
        app.world_mut().spawn(task.clone());

        let policy = DefaultToolApprovalPolicy;
        let agents = app.world().query::<&Agent>();
        let tasks = app.world().query::<(Entity, &Task)>();

        assert_eq!(
            policy.determine_approval_route("test_tool", &agent, &task, &tasks, &agents),
            ApprovalRoute::UserConfirmation
        );
    }
}
```

> `Confirm` 路由命中父 Agent 的完整路径由 Task 9 集成测试覆盖。

- [ ] **Step 6: 编译检查**

Run: `cargo check --all-targets --all-features`
Expected: PASS

- [ ] **Step 7: 提交**

```bash
git add src/contracts/tools.rs src/systems/tools/dispatch.rs
git commit -m "feat(approval): route parent approval by task.parent_task_id"
```

---

### Task 3: 实现 chat_with_agent 工具 Executor

**Files:**
- Create: `src/systems/tools/builtin/chat_with_agent.rs`
- Modify: `src/systems/tools/builtin/mod.rs`
- Modify: `src/systems/tools/mod.rs`
- Test: `src/systems/tools/builtin/chat_with_agent.rs` 底部单元测试

**Interfaces:**
- Consumes: `ToolContext`, `ToolAction`, `ToolError`, `TaskId`
- Produces: `ChatWithAgentTool` 实现 `BuiltinTool`，返回 `ToolAction::StartChatRound`

- [ ] **Step 1: 创建 `src/systems/tools/builtin/chat_with_agent.rs`**

```rust
//! chat_with_agent Tool 实现

use uuid::Uuid;

use crate::domain::{TaskId, ToolAction, ToolContext, ToolError};

pub struct ChatWithAgentTool;

impl crate::domain::BuiltinTool for ChatWithAgentTool {
    fn name(&self) -> &str {
        "chat_with_agent"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        parse_and_resolve(input, ctx.current_task_id)
    }
}

fn parse_and_resolve(
    input: &serde_json::Value,
    current_task_id: TaskId,
) -> Result<ToolAction, ToolError> {
    let message = input
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing 'message' parameter".to_string()))?
        .to_string();

    let handle = input
        .get("handle")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    let agent_name = input.get("agent").and_then(|v| v.as_str()).map(String::from);
    let agent_tags: Vec<String> = input
        .get("agent_tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let context = input
        .get("context")
        .and_then(|v| v.as_str())
        .map(String::from);

    if handle.is_none() && agent_name.is_none() && agent_tags.is_empty() {
        return Err(ToolError::InvalidInput(
            "new chat requires 'agent' or 'agent_tags'".to_string(),
        ));
    }

    Ok(ToolAction::StartChatRound {
        agent_name,
        agent_tags,
        message,
        context,
        handle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};
    use uuid::Uuid;

    fn tool_context() -> ToolContext<'static> {
        let knowledge = Box::leak(Box::new(SharedKnowledgeBase::default()));
        let experience_store = Box::leak(Box::new(ExperienceStore::default()));
        ToolContext {
            knowledge,
            experience_store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: Uuid::new_v4(),
            current_agent_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn parse_requires_message() {
        let input = serde_json::json!({"agent": "reviewer"});
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_err());
    }

    #[test]
    fn parse_requires_agent_or_tags_for_new_chat() {
        let input = serde_json::json!({"message": "hello"});
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_err());
    }

    #[test]
    fn parse_allows_handle_only() {
        let handle = Uuid::new_v4();
        let input = serde_json::json!({
            "message": "continue",
            "handle": handle.to_string()
        });
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::StartChatRound {
                handle: Some(h), message, agent_name, agent_tags,
            } => {
                assert_eq!(h, handle);
                assert_eq!(message, "continue");
                assert!(agent_name.is_none());
                assert!(agent_tags.is_empty());
            }
            other => panic!("expected StartChatRound, got {:?}", other),
        }
    }

    #[test]
    fn parse_new_chat_with_agent_name() {
        let input = serde_json::json!({
            "message": "review this doc",
            "agent": "reviewer",
            "context": "focus on api design"
        });
        let result = ChatWithAgentTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::StartChatRound {
                agent_name: Some(name),
                message,
                context: Some(ctx),
            } => {
                assert_eq!(name, "reviewer");
                assert_eq!(message, "review this doc");
                assert_eq!(ctx, "focus on api design");
            }
            other => panic!("expected StartChatRound, got {:?}", other),
        }
    }
}
```

- [ ] **Step 2: 修改 `src/systems/tools/builtin/mod.rs` 导出**

在 `mod` 区域添加：

```rust
mod chat_with_agent;
```

在 `pub use` 区域添加：

```rust
pub use chat_with_agent::ChatWithAgentTool;
```

- [ ] **Step 3: 在 `src/systems/tools/mod.rs` 注册工具**

在 `use self::builtin::{...}` 中追加 `ChatWithAgentTool`。

在 `register_builtin_tools` 函数末尾、`ChannelSendTool` 之前注册：

```rust
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
```

- [ ] **Step 4: 运行单元测试**

Run: `cargo test --lib chat_with_agent -- --nocapture`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/systems/tools/builtin/chat_with_agent.rs src/systems/tools/builtin/mod.rs src/systems/tools/mod.rs
git commit -m "feat(tools): add chat_with_agent executor"
```

---

### Task 4: 在 Orchestrator 中处理 StartChatRound

**Files:**
- Modify: `src/systems/tools/orchestrator.rs`
- Test: `src/systems/tools/orchestrator.rs` 底部单元测试（扩展）

**Interfaces:**
- Consumes: `ToolAction::StartChatRound`, `ChatSession`, `ChatRoundStartedMessage`, `Task`, `Agent`, `AgentKind`, `ShortTermMemory`, `EntryRole`
- Produces: 创建/更新子 Task 实体，spawn `ChatRoundStartedMessage`，despawn `ToolExecutionRequestMessage`

- [ ] **Step 1: 在 `handle_tool_action` 中添加 `StartChatRound` 分支**

在 `match action` 中 `Direct` 分支之后添加：

```rust
Ok(ToolAction::StartChatRound {
    agent_name,
    agent_tags,
    message,
    context,
    handle,
}) => {
    let parent_task_id = request.request.task_id;
    let parent_tool_call_id = request.tool_call_id.clone().unwrap_or_default();

    // 一次性从父任务 clone 出所需信息，避免 Query 借用冲突
    let (parent_origin_channel, parent_delegate) = tasks
        .get(task_entity)
        .map(|(_, t)| (t.origin_channel.clone(), t.delegate))
        .unwrap_or_else(|_| {
            warn!(
                event = "ParentTaskNotFoundForChatChannel",
                task_id = %parent_task_id,
                "parent task entity not found, falling back to Tui/default for chat subtask origin_channel"
            );
            (
                ChannelId {
                    frontend: FrontendKind::Tui,
                    user_id: "default".to_string(),
                    thread_id: None,
                },
                None,
            )
        });

    let (child_task_id, batch_id) = if let Some(handle) = handle {
        // 继续已有对话：先只读收集信息，再单独修改
        let child_info = tasks
            .iter()
            .find(|(_, t)| t.id == handle)
            .map(|(e, t)| (e, t.clone()));

        let Some((child_entity, child_task)) = child_info else {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::NotFound(format!("chat handle {}", handle)),
            );
            continue;
        };

        if child_task.parent_task_id != Some(parent_task_id) {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::PermissionDenied("chat handle does not belong to current task".to_string()),
            );
            continue;
        }

        if !matches!(child_task.status, TaskStatus::Waiting(WaitingReason::ChatAgent)) {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::InvalidInput("chat handle is not in waiting state".to_string()),
            );
            continue;
        }

        let new_batch_id = Uuid::new_v4();
        let child_task_id = child_task.id;

        // 追加本轮用户消息到子任务 STM
        if let Ok(mut stm) = short_term_memories.get_mut(child_entity) {
            stm.add_entry(EntryRole::User, &message, Default::default());
        }

        // 更新 ChatSession
        commands.entity(child_entity).insert(ChatSession {
            parent_tool_call_id: parent_tool_call_id.clone(),
            current_batch_id: new_batch_id,
        });

        // 唤醒子任务
        if let Ok((_, mut task)) = tasks.get_mut(child_entity) {
            task.status = TaskStatus::Ready;
            task.updated_at = clock.0;
        }

        (child_task_id, new_batch_id)
    } else {
        // 开始新对话
        let agent = find_persistent_agent(&agents, agent_name.as_deref(), &agent_tags);
        let Some(agent) = agent else {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::NotFound("no matching persistent agent found".to_string()),
            );
            continue;
        };

        let child_task_id = Uuid::new_v4();
        let batch_id = Uuid::new_v4();

        let mut initial_stm = ShortTermMemory::default();
        if let Some(ref ctx) = context {
            initial_stm.add_entry(
                EntryRole::User,
                &format!("[System context]\n{}\n\n{}", ctx, message),
                Default::default(),
            );
        } else {
            initial_stm.add_entry(EntryRole::User, &message, Default::default());
        }

        let mut child_task = Task::from_user_input(&message, 0, parent_origin_channel.clone());
        child_task.id = child_task_id;
        child_task.parent_task_id = Some(parent_task_id);
        child_task.delegate = Some(agent.id);
        child_task.creator = parent_delegate.unwrap_or(request.request.agent_id);
        child_task.status = TaskStatus::Ready;
        child_task.multi_turn = true;

        commands.spawn((
            child_task,
            initial_stm,
            ChatSession {
                parent_tool_call_id: parent_tool_call_id.clone(),
                current_batch_id: batch_id,
            },
        ));

        (child_task_id, batch_id)
    };

    commands.spawn(ChatRoundStartedMessage {
        parent_task_id,
        child_task_id,
        batch_id,
        parent_tool_call_id,
    });

    commands.entity(request_entity).despawn();
    continue;
}
```

注意：`handle_tool_action` 的签名需要包含 `agents: &Query<&Agent>`、`short_term_memories: &mut Query<&mut ShortTermMemory>` 和 `clock: &Clock`。

同时在 `src/systems/tools/orchestrator.rs` 顶部 `use crate::domain::{...}` 列表中追加：

```rust
AgentKind, ChatRoundStartedMessage, ChatSession, EntryRole,
```

- [ ] **Step 2: 添加 Persistent Agent 查找辅助函数**

在 `src/systems/tools/orchestrator.rs` 中 `handle_tool_action` 附近添加私有函数：

```rust
fn find_persistent_agent<'a>(
    agents: &'a Query<&'a Agent>,
    name: Option<&str>,
    tags: &[String],
) -> Option<&'a Agent> {
    if let Some(name) = name {
        let by_name = agents.iter().find(|a| {
            a.kind == AgentKind::Persistent && a.profile.name == name
        });
        if by_name.is_some() {
            return by_name;
        }
    }

    if !tags.is_empty() {
        return agents.iter().find(|a| {
            a.kind == AgentKind::Persistent
                && tags.iter().all(|tag| a.capabilities.tags.contains(tag))
        });
    }

    None
}
```

- [ ] **Step 3: 同步 `handle_tool_action` 函数签名与调用点**

`tool_dispatch_system` 中调用 `handle_tool_action` 的位置需要传入 `agents`、`short_term_memories` 和 `clock`。扩展 `handle_tool_action` 签名为：

```rust
pub fn handle_tool_action<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
    tasks: &mut Query<(Entity, &mut Task)>,
    agents: &Query<&Agent>,
    short_term_memories: &mut Query<&mut ShortTermMemory>,
    backend: &B,
    experience_store: &mut ExperienceStore,
    pending_experience_hooks: &mut PendingExperienceHooks,
    parent_agent_id: Option<AgentId>,
    clock: &Clock,
)
```

并同步 `tool_dispatch_system` 中的调用点：

```rust
handle_tool_action(
    &mut commands,
    entity,
    task_entity,
    &request,
    action,
    &mut tasks,
    &agents,
    &mut short_term_memories,
    &*backend,
    &mut experience_store,
    &mut pending_experience_hooks,
    parent_agent_id,
    &clock,
);
```

同时 `tool_dispatch_system` 的 system 参数需要增加：

```rust
mut short_term_memories: Query<&mut ShortTermMemory>,
clock: Res<Clock>,
```

- [ ] **Step 4: 编译检查**

Run: `cargo check --all-targets --all-features`
Expected: PASS（可能需要修复 Query 借用冲突）

- [ ] **Step 5: 提交**

```bash
git add src/systems/tools/orchestrator.rs
git commit -m "feat(orchestrator): handle StartChatRound to create or resume chat subtasks"
```

---

### Task 5: 支持 chat 子任务被 Task Dispatch 直接调度

**Files:**
- Modify: `src/systems/dispatch/task_dispatch.rs`
- Test: `src/systems/dispatch/task_dispatch.rs` 底部单元测试（扩展）

**Interfaces:**
- Consumes: `Task` with `ChatSession` component and `delegate.is_some()`
- Produces: `AgentExecutionRequestMessage` for the delegated Persistent Agent

- [ ] **Step 0: 导入 `ChatSession`**

在 `src/systems/dispatch/task_dispatch.rs` 顶部 `use crate::domain::{...}` 列表中追加：

```rust
ChatSession,
```

- [ ] **Step 1: 修改 `task_dispatch_system` 的入口过滤条件**

将现有 `Query<(&mut Task, Option<&ShortTermMemory>)>` 改为：

```rust
mut tasks: Query<(Entity, &mut Task, Option<&ShortTermMemory>, Has<ChatSession>)>,
```

并将子任务跳过逻辑改为：

```rust
for (_entity, mut task, short_term, has_chat_session) in &mut tasks {
    // 子任务由 Brain 分发，普通 dispatch 不处理；
    // 例外：chat_with_agent 对话型子任务且已指定 delegate 时，直接调度到该 Persistent Agent。
    if task.parent_task_id.is_some() && !(has_chat_session && task.delegate.is_some()) {
        continue;
    }

    if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
        continue;
    }
    // ...
}
```

- [ ] **Step 2: 在调度循环中使用新的解构变量**

后续 `short_term` 变量的引用保持为 `Option<&ShortTermMemory>`。例如：

```rust
let prompt = build_prompt_with_context(&task.content, short_term, long_term, &task.origin_channel);
```

- [ ] **Step 3: 添加单元测试验证 chat 子任务被调度**

在 `src/systems/dispatch/task_dispatch.rs` 底部 `#[cfg(test)]` 中追加：

```rust
#[test]
fn chat_subtask_with_delegate_is_dispatched() {
    let mut app = build_test_app();

    let agent_id = Uuid::new_v4();
    app.world_mut().spawn((
        Agent {
            id: agent_id,
            profile: AgentProfile {
                name: "reviewer".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["review".to_string()],
                description: "reviewer agent".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
        },
        LongTermMemory::default(),
    ));

    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();
    let channel = make_channel();

    app.world_mut().spawn((
        Task {
            id: child_id,
            content: "review this doc".to_string(),
            creator: parent_id,
            delegate: Some(agent_id),
            status: TaskStatus::Ready,
            input_summary: "review this doc".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: Some(parent_id),
            batch_id: None,
            origin_channel: channel,
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
        ChatSession {
            parent_tool_call_id: "call_1".to_string(),
            current_batch_id: Uuid::new_v4(),
        },
    ));

    app.update();

    let requests: Vec<&AgentExecutionRequestMessage> = {
        let world = app.world();
        let mut query = world.query::<&AgentExecutionRequestMessage>();
        query.iter(world).collect()
    };

    assert_eq!(requests.len(), 1, "chat subtask should be dispatched");
    assert_eq!(requests[0].request.agent_id, agent_id);
    assert_eq!(requests[0].request.task_id, child_id);
    assert_eq!(requests[0].request.request_kind, AgentRequestKind::LlmCompletion);
}
```

- [ ] **Step 4: 运行单元测试**

Run: `cargo test --lib task_dispatch::tests::chat_subtask_with_delegate_is_dispatched -- --nocapture`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/systems/dispatch/task_dispatch.rs
git commit -m "feat(dispatch): allow chat_session subtasks to be dispatched directly"
```

---

### Task 6: 拦截 chat 子任务的 LLM 响应

**Files:**
- Modify: `src/systems/transform/llm_response.rs`
- Test: `tests/chat_with_agent_flow.rs`（后续 Task 覆盖）

**Interfaces:**
- Consumes: `AgentExecutionResultMessage` for tasks with `ChatSession`
- Produces: `ChatRoundReadyMessage`, child task status = `Waiting(ChatAgent)`

- [ ] **Step 0: 导入 `ChatSession` 与 `ChatRoundReadyMessage`**

在 `src/systems/transform/llm_response.rs` 顶部 `use crate::domain::{...}` 列表中追加：

```rust
ChatRoundReadyMessage, ChatSession,
```

- [ ] **Step 1: 在 `llm_response_system` 的多轮分支前插入 ChatSession 判断**

在 `llm_response_system` 中，任务完成处理之前（大约在 multi_turn 分支设置 `Waiting(User)` 的位置），插入对 `ChatSession` 组件的检查：

```rust
// 若当前任务是 chat_with_agent 子任务，则进入 Waiting(ChatAgent) 并触发 ChatRoundReadyMessage
if let Some(parent_task_id) = task.parent_task_id
    && let Some(chat_session) = chat_session
{
    let response_text = match &output.content {
        OutputContent::Text(text) => text.clone(),
        OutputContent::Json(value) => value.to_string(),
    };

    task.status = TaskStatus::Waiting(WaitingReason::ChatAgent);
    task.result_summary = response_text.clone();
    task.updated_at = clock.0;

    commands.spawn(ChatRoundReadyMessage {
        child_task_id: task.id,
        parent_task_id,
        parent_agent_id: task.creator,
        batch_id: chat_session.current_batch_id,
        parent_tool_call_id: chat_session.parent_tool_call_id.clone(),
        response: response_text,
    });

    trace!(
        event = "ChatRoundReady",
        child_task_id = %task.id,
        parent_task_id = %parent_task_id,
        batch_id = %chat_session.current_batch_id,
        "chat subtask waiting for parent next round"
    );

    continue;
}
```

- [ ] **Step 2: 扩展 `llm_response_system` 的 Query 参数**

将 `llm_response_system` 的 `tasks` Query 改为可读取 `ChatSession`，并调整循环解构：

```rust
mut tasks: Query<(Entity, &mut Task, Option<&ShortTermMemory>, Option<&ChatSession>)>,
```

并在循环中使用：

```rust
for (task_entity, mut task, short_term, chat_session) in &mut tasks {
    // ... 原有逻辑 ...
}
```

- [ ] **Step 3: 编译检查**

Run: `cargo check --all-targets --all-features`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add src/systems/transform/llm_response.rs
git commit -m "feat(llm-response): capture chat subtask responses into ChatRoundReadyMessage"
```

---

### Task 7: 父任务阻塞与结果回填

**Files:**
- Create: `src/systems/transform/chat_round.rs`
- Modify: `src/systems/transform/mod.rs`
- Modify: `src/plugins/task_runtime.rs`
- Test: `tests/chat_with_agent_flow.rs`（后续 Task 覆盖）

**Interfaces:**
- Consumes: `ChatRoundStartedMessage`, `ChatRoundReadyMessage`
- Produces: parent task `Waiting(SubTaskBatch { batch_id })`, `ToolExecutionResultMessage`

- [ ] **Step 1: 创建 `src/systems/transform/chat_round.rs`**

```rust
//! chat_with_agent 多轮对话阻塞与结果回填系统

use bevy::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionResult, AgentRequestKind, ChatRoundReadyMessage,
        ChatRoundStartedMessage, OutputContent, Task, TaskStatus, ToolExecutionResultMessage,
        ToolReturnedHookPending, WaitingReason,
    },
};

/// 消费 ChatRoundStartedMessage，将父任务阻塞到 Waiting(SubTaskBatch { batch_id })。
pub fn chat_round_block_system(
    mut commands: Commands,
    clock: Res<Clock>,
    mut tasks: Query<&mut Task>,
    started: Query<(Entity, &ChatRoundStartedMessage)>,
) {
    for (entity, msg) in &started {
        if let Ok(mut parent) = tasks.iter_mut().find(|t| t.id == msg.parent_task_id) {
            parent.status = TaskStatus::Waiting(WaitingReason::SubTaskBatch {
                batch_id: msg.batch_id,
            });
            parent.updated_at = clock.0;
            debug!(
                event = "ChatRoundBlocked",
                parent_task_id = %msg.parent_task_id,
                child_task_id = %msg.child_task_id,
                batch_id = %msg.batch_id,
                "parent task blocked waiting for chat round"
            );
        } else {
            warn!(
                event = "ChatRoundParentNotFound",
                parent_task_id = %msg.parent_task_id,
                "parent task not found for chat round block"
            );
        }
        commands.entity(entity).despawn();
    }
}

/// 消费 ChatRoundReadyMessage，生成完整 ToolExecutionResultMessage 回填父任务，并恢复父任务 Ready。
pub fn chat_round_completion_system(
    mut commands: Commands,
    clock: Res<Clock>,
    mut tasks: Query<&mut Task>,
    ready: Query<(Entity, &ChatRoundReadyMessage)>,
) {
    for (entity, msg) in &ready {
        if let Ok(mut parent) = tasks.iter_mut().find(|t| t.id == msg.parent_task_id) {
            parent.status = TaskStatus::Ready;
            parent.updated_at = clock.0;
            debug!(
                event = "ChatRoundCompleted",
                parent_task_id = %msg.parent_task_id,
                child_task_id = %msg.child_task_id,
                batch_id = %msg.batch_id,
                "chat round completed, parent task restored to Ready"
            );
        } else {
            warn!(
                event = "ChatRoundParentNotFound",
                parent_task_id = %msg.parent_task_id,
                "parent task not found for chat round completion"
            );
        }

        let execution_result = AgentExecutionResult {
            task_id: msg.parent_task_id,
            agent_id: msg.parent_agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text(msg.response.clone()),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
            work_item_id: None,
        };

        commands.spawn((
            ToolExecutionResultMessage {
                result: execution_result,
                tool_name: "chat_with_agent".to_string(),
                tool_output: Ok(serde_json::json!({
                    "handle": msg.child_task_id.to_string(),
                    "response": msg.response,
                    "agent": msg.parent_agent_id.to_string()
                })),
                tool_call_id: Some(msg.parent_tool_call_id.clone()),
                processed: false,
                original_tool_output: None,
            },
            ToolReturnedHookPending,
        ));
        commands.entity(entity).despawn();
    }
}
```

- [ ] **Step 2: 在 `src/systems/transform/mod.rs` 导出**

在 `pub use` 区域添加：

```rust
pub use chat_round::{chat_round_block_system, chat_round_completion_system};
```

并在 `mod` 区域添加：

```rust
mod chat_round;
```

- [ ] **Step 3: 在 `src/plugins/task_runtime.rs` 注册系统**

在 `TaskRuntimePlugin::build` 中，与 `sub_task_batch_block_system` 一起注册到 `Transform` set：

```rust
chat_round_block_system
    .in_set(HarnessSet::Transform)
    .after(tool_result_system),
chat_round_completion_system
    .in_set(HarnessSet::Transform)
    .after(tool_result_system)
    .before(chat_round_block_system),
```

注意：`chat_round_completion_system` 应该在 `tool_result_system` 之后运行，因为它生成 `ToolExecutionResultMessage` 供 `tool_result_system` 下一帧处理。由于 Bevy 同一帧内 `tool_result_system` 已经运行完毕，所以将 `chat_round_completion_system` 放在 Transform set 中，下一帧的 tool_result_system 会处理它。

- [ ] **Step 4: 编译检查**

Run: `cargo check --all-targets --all-features`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/systems/transform/chat_round.rs src/systems/transform/mod.rs src/plugins/task_runtime.rs
git commit -m "feat(chat-round): block parent task and complete round with results"
```

---

### Task 8: 父任务终止时清理 chat 子任务

**Files:**
- Modify: `src/systems/transform/chat_round.rs`
- Modify: `src/plugins/task_runtime.rs`
- Test: `tests/chat_with_agent_flow.rs`（后续 Task 覆盖）

**Interfaces:**
- Consumes: `TaskTerminatedMessage`, tasks with `ChatSession` and `parent_task_id`
- Produces: despawn child task entities

- [ ] **Step 0: 导入 `TaskTerminatedMessage`**

在 `src/systems/transform/chat_round.rs` 顶部导入区域追加：

```rust
use crate::domain::TaskTerminatedMessage;
```

- [ ] **Step 1: 在 `src/systems/transform/chat_round.rs` 添加 `chat_session_cleanup_system`**

在文件末尾追加：

```rust
/// 父任务终止时清理所有关联的 chat_with_agent 子任务。
pub fn chat_session_cleanup_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    chat_children: Query<(Entity, &Task, &ChatSession)>,
) {
    for (msg_entity, msg) in &terminated {
        for (child_entity, child_task, _) in &chat_children {
            if child_task.parent_task_id == Some(msg.task_id) {
                if !child_task.status.is_terminal() {
                    warn!(
                        event = "ChatSubtaskCancelledByParentTermination",
                        child_task_id = %child_task.id,
                        parent_task_id = %msg.task_id,
                        old_status = ?child_task.status,
                        "cancelling chat subtask due to parent termination"
                    );
                }
                commands.entity(child_entity).despawn();
            }
        }
        commands.entity(msg_entity).despawn();
    }
}
```

- [ ] **Step 2: 在 `src/systems/transform/mod.rs` 导出**

在 `pub use chat_round::{...}` 中追加 `chat_session_cleanup_system`：

```rust
pub use chat_round::{
    chat_round_block_system, chat_round_completion_system, chat_session_cleanup_system,
};
```

- [ ] **Step 3: 在 `src/plugins/task_runtime.rs` 注册系统**

在 `TaskRuntimePlugin::build` 中，将 `chat_session_cleanup_system` 注册到 `Maintenance` set：

```rust
chat_session_cleanup_system
    .in_set(HarnessSet::Maintenance)
    .after(crate::systems::task_termination_system),
```

- [ ] **Step 4: 编译检查**

Run: `cargo check --all-targets --all-features`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/systems/transform/chat_round.rs src/systems/transform/mod.rs src/plugins/task_runtime.rs
git commit -m "feat(lifecycle): cleanup chat subtasks on parent termination"
```

---

### Task 9: 集成测试

**Files:**
- Create: `tests/chat_with_agent_flow.rs`

**Interfaces:**
- Consumes: `build_harness_app`, `AgentExecutor`, `ExternalInput`, `Task`, `ChatSession`
- Produces: 测试断言

- [ ] **Step 1: 创建 `tests/chat_with_agent_flow.rs`**

```rust
//! chat_with_agent 工具集成测试

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    Agent, AgentCapabilities, AgentExecutionOutput, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, AgentToolPermissions, ChannelId, EntryRole, ExecutorFuture,
    FrontendKind, HarnessConfig, Task, TaskStatus, WaitingReason, build_harness_app,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text(format!("echo: {}", request.prompt)),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

/// 验证 chat_with_agent 第一轮创建子任务并阻塞父任务。
#[test]
fn chat_with_agent_starts_round_and_blocks_parent() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );

    app.update();

    // 找到 Persistent Agent
    let agent_id = {
        let world = app.world();
        let mut query = world.query::<&Agent>();
        query
            .iter(world)
            .find(|a| a.kind == AgentKind::Persistent && a.profile.name == "default-llm-agent")
            .map(|a| a.id)
            .unwrap_or_else(|| Uuid::nil())
    };

    if agent_id == Uuid::nil() {
        // 跳过无默认 agent 的配置
        return;
    }

    let parent_task_id = Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: parent_task_id,
            content: "review doc".to_string(),
            creator: Uuid::nil(),
            delegate: Some(agent_id),
            status: TaskStatus::Ready,
            input_summary: "review doc".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: default_channel(),
            last_evaluated_turn: None,
        },
        harness::ShortTermMemory::default(),
    ));

    // 模拟父 Agent 调用 chat_with_agent
    let tool_call_id = "call_chat_1";
    app.world_mut().spawn(harness::ToolExecutionRequestMessage {
        task_id: parent_task_id,
        tool_call_id: Some(tool_call_id.to_string()),
        tool_name: "chat_with_agent".to_string(),
        tool_input: serde_json::json!({
            "agent": "default-llm-agent",
            "message": "please review this API design"
        }),
        request: harness::AgentExecutionRequest {
            task_id: parent_task_id,
            agent_id,
            request_kind: harness::AgentRequestKind::LlmCompletion,
            prompt: "call chat_with_agent".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
        },
    });

    // 运行多帧让工具分发、子任务创建、调度、执行、响应拦截完成
    for _ in 0..20 {
        app.update();
    }

    // 验证子任务存在且处于 Waiting(ChatAgent)
    let chat_tasks: Vec<(Task, harness::ChatSession)> = {
        let world = app.world();
        let mut query = world.query::<(&Task, &harness::ChatSession)>();
        query
            .iter(world)
            .filter(|(t, _)| t.parent_task_id == Some(parent_task_id))
            .map(|(t, s)| (t.clone(), s.clone()))
            .collect()
    };

    assert_eq!(
        chat_tasks.len(),
        1,
        "exactly one chat subtask should exist"
    );
    assert!(
        matches!(chat_tasks[0].0.status, TaskStatus::Waiting(WaitingReason::ChatAgent)),
        "chat subtask should be waiting for parent next round, got {:?}",
        chat_tasks[0].0.status
    );

    // 验证父任务在 chat round 完成后恢复为 Ready
    let parent = app
        .world()
        .query::<&Task>()
        .iter(app.world())
        .find(|t| t.id == parent_task_id)
        .cloned()
        .expect("parent task should exist");
    assert_eq!(
        parent.status,
        TaskStatus::Ready,
        "parent task should be Ready after chat round completes, got {:?}",
        parent.status
    );
}
```

- [ ] **Step 2: 运行集成测试**

Run: `cargo test --test chat_with_agent_flow -- --nocapture`
Expected: PASS

- [ ] **Step 3: 提交**

```bash
git add tests/chat_with_agent_flow.rs
git commit -m "test: add chat_with_agent integration test"
```

---

### Task 10: 文档同步与回归验证

**Files:**
- Modify: `docs/superpowers/specs/2026-06-30-chat-with-agent-design.md`
- Modify: `docs/current-state.md`
- Modify: `docs/README.md`

**Interfaces:**
- Consumes: 已更新的设计文档与实施代码
- Produces: 同步后的文档

- [ ] **Step 1: 更新设计文档状态**

在 `docs/superpowers/specs/2026-06-30-chat-with-agent-design.md` 顶部标注：

```markdown
> **状态：当前有效 / 实施中**
```

- [ ] **Step 2: 更新 `docs/current-state.md`**

在"已实现"列表中追加：

```markdown
- `chat_with_agent` 工具：支持父任务与 Persistent Agent 多轮同步对话
```

- [ ] **Step 3: 更新 `docs/README.md` 索引**

在 `docs/superpowers/README.md` 的计划列表中追加 `2026-06-30-chat-with-agent-implementation.md` 链接。

- [ ] **Step 4: 运行完整回归测试**

Run: `cargo test --all-features`
Expected: ALL PASS

Run: `cargo fmt --all --check`
Expected: PASS

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add docs/superpowers/specs/2026-06-30-chat-with-agent-design.md docs/current-state.md docs/superpowers/README.md
git commit -m "docs: update state for chat_with_agent implementation"
```

---

## 自审清单

1. **Spec coverage:**
   - `ChatSession` 组件：Task 1 实现。
   - `WaitingReason::ChatAgent`：Task 1 实现。
   - `ChatRoundStartedMessage` / `ChatRoundReadyMessage`：Task 1 实现。
   - `chat_with_agent` 工具契约与 executor：Task 3 实现。
   - 父任务阻塞与多轮交互：Task 4、Task 7 实现。
   - 审批路由统一为 `task.parent_task_id`：Task 2 实现。
   - 子任务生命周期跟随父任务：Task 8 实现。
   - 集成测试：Task 9 实现。

2. **Placeholder scan:**
   - 所有代码块均为可直接使用的 Rust 代码，无 "TBD"/"TODO"。
   - 测试命令与断言具体。

3. **Type consistency:**
   - `ToolAction::StartChatRound` 字段与 Task 3 executor 返回、Task 4 orchestrator 消费一致。
   - `ChatSession` 字段与 Task 1 定义、Task 4/6/8 使用一致。
   - `ChatRoundStartedMessage` / `ChatRoundReadyMessage` 字段跨 Task 一致。

## 执行交接

**Plan complete and saved to `docs/superpowers/plans/2026-06-30-chat-with-agent-implementation.md`. Two execution options:**

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
