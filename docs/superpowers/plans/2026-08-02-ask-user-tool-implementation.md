# ask_user 工具实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 实现 `ask_user` 工具，让 LLM 能在工具调用循环中向用户提出开放文本问题，并把用户回复作为工具结果返回。

**架构：** 声明式 Sync 工具（仿 `chat_with_agent` 模式），executor 只解析参数返回 `ToolAction::AskUser`；orchestrator 负责推送问题到 `output_channel`、挂载 `AskUserPending` 组件、把 task 切到 `Waiting(AskUser)`；`user_input_routing_system` 新增分支消费用户回复，spawn `ToolExecutionResultMessage`，恢复 task 至 `Waiting(ToolExecution)` 让 LLM loop 续跑。

**技术栈：** Rust + Bevy ECS + `serde_json`，遵循 [async-tool-bridge.md §5.2](../../async-tool-bridge.md) "声明式 Sync 工具不上桥"分类。

**规格来源：** [docs/superpowers/specs/2026-08-02-ask-user-tool-design.md](../specs/2026-08-02-ask-user-tool-design.md)

**规格偏离说明（已在计划中校正）：**
- `EngineEvent::Text` 实际结构包含 `role: MessageRole` 字段（规格代码示例遗漏），计划代码已补齐
- `WaitingForTasksInfo` / `WaitingForSessionInfo` 实际位于 `src/domain/task.rs`（规格说在 `message.rs`），`AskUserPending` 同位放 `task.rs`
- 补充 `frontend_output.rs` 中 `waiting_reason_to_kind` 映射 `AskUser` → `WaitingReasonKind::User`（规格未提，但 `WaitingReason` 新增变体必须配映射否则编译失败）
- 补充 `summarize_tool_input` 中 `ask_user` 分支（规格未提，但 `ask_user` 是 `Allow` 权限会触发 `ToolCallStarted` 事件，缺该分支会回退到默认 JSON 序列化）

---

## 文件结构

### 创建

- `src/systems/tools/builtin/ask_user.rs` — `AskUserTool` 实现 + 单元测试，仿 `chat_with_agent.rs` 风格
- `tests/ask_user_e2e_test.rs` — 端到端集成测试，验证 LLM 调用 → 用户回复 → follow-up 触发

### 修改

- `src/domain/space.rs` — `ToolAction` 枚举新增 `AskUser { question: String }` 变体
- `src/domain/message.rs` — `WaitingReason` 枚举新增 `AskUser` 变体
- `src/domain/task.rs` — 新增 `AskUserPending` 组件（与 `WaitingForTasksInfo` / `WaitingForSessionInfo` 同位）
- `src/domain/mod.rs` — 导出 `AskUserPending`
- `src/systems/tools/builtin/mod.rs` — 声明 `ask_user` 模块 + 导出 `AskUserTool`
- `src/systems/tools/mod.rs` — 在 `register_builtin_tools` 中注册 `ask_user` 工具定义与执行器
- `src/systems/tools/orchestrator.rs` — `handle_tool_action` 新增 `AskUser` arm + 函数签名新增 `frontend_registry: &FrontendRegistry` 参数 + 单元测试
- `src/systems/tools/dispatch.rs` — 调用 `handle_tool_action` 时传入 `frontend_registry`
- `src/systems/tools/confirmation.rs` — 调用 `handle_tool_action` 时传入 `frontend_registry`
- `src/systems/tools/approval.rs` — 调用 `handle_tool_action` 时传入 `frontend_registry`
- `src/systems/routing.rs` — `user_input_routing_system` 新增 `Waiting(AskUser)` 分支 + Query 扩展 + 单元测试
- `src/systems/frontend_output.rs` — `waiting_reason_to_kind` 新增 `AskUser` 映射 + 扩展测试用例
- `src/domain/frontend.rs` — `summarize_tool_input` 新增 `ask_user` 分支 + 单元测试
- `docs/current-state.md` — 工具列表补充 `ask_user`
- `docs/async-tool-bridge.md` — §5.2 声明式 Sync 工具列表补充 `ask_user`

---

## 任务依赖

```text
任务 1（领域类型） ─┬─→ 任务 2（工具实现） ─→ 任务 3（工具注册）
                  │
                  └─→ 任务 4（orchestrator + 调用点） ─→ 任务 5（routing 分支）
                                                          │
                                                          └─→ 任务 8（E2E）

任务 6（waiting_reason_to_kind 映射）— 依赖任务 1
任务 7（summarize_tool_input）— 独立
任务 9（文档同步）— 最后
```

任务 1 是基础（其他任务依赖新领域类型编译通过）；任务 4 必须与 3 个调用点同步修改以保持构建绿色；任务 6 必须与任务 1 同步（`WaitingReason` 新增变体后 `match` 必须穷尽）；任务 7 独立可并行；任务 8 依赖前 5 个完成。

---

## 任务 1：领域类型新增

**文件：**
- 修改：`src/domain/space.rs:189-259`（`ToolAction` 枚举）
- 修改：`src/domain/message.rs:20-38`（`WaitingReason` 枚举）
- 修改：`src/domain/task.rs:140-168`（在 `WaitingForSessionInfo` 之后新增 `AskUserPending`）
- 修改：`src/domain/mod.rs:140-144`（导出 `AskUserPending`）
- 测试：`src/domain/frontend.rs`（扩展 `waiting_reason_to_kind_mappings` 测试）—— 此处仅为编译需要的占位，真正映射在任务 6 完成

### 步骤

- [ ] **步骤 1：在 `src/domain/space.rs` 的 `ToolAction` 枚举末尾新增 `AskUser` 变体**

在 `SubmitSkillUpdate { ... }` 变体后（约第 258 行，闭合 `}` 之前）追加：

```rust
    /// 向用户提出问题并等待开放文本回复。
    /// executor 只负责解析参数，问题呈现与等待状态由 orchestrator 完成。
    AskUser {
        /// 向用户展示的问题文本
        question: String,
    },
```

- [ ] **步骤 2：在 `src/domain/message.rs` 的 `WaitingReason` 枚举新增 `AskUser` 变体**

在 `ChatAgent,` 后（约第 37 行）追加：

```rust
    /// ask_user 工具等待用户开放文本回复
    AskUser,
```

- [ ] **步骤 3：在 `src/domain/task.rs` 的 `WaitingForSessionInfo` 之后新增 `AskUserPending` 组件**

在 `WaitingForSessionInfo` 结构体定义之后（约第 168 行后）追加：

```rust
/// Task 等待用户回复 ask_user 问题的状态信息
/// 此组件添加到发起 ask_user 的 Task Entity 上
#[derive(Component, Debug, Clone)]
pub struct AskUserPending {
    /// Tool call ID（用于返回结果给 LLM）
    pub tool_call_id: Option<String>,
    /// 发起问询的 Agent ID
    pub agent_id: AgentId,
}
```

- [ ] **步骤 4：在 `src/domain/mod.rs` 的 task 模块导出中追加 `AskUserPending`**

修改 `pub use task::{ ... };` 语句，在 `WaitingForSessionInfo, WaitingForTasksInfo,` 之后追加 `AskUserPending,`：

```rust
// task
pub use task::{
    AskUserPending, NewlyCreatedTask, PreviousTaskStatus, Task, TaskRoutingPolicy, TaskStatus,
    ToolCalledHookPending, ToolReturnedHookPending, WaitingForSessionInfo, WaitingForTasksInfo,
};
```

- [ ] **步骤 5：在 `src/systems/frontend_output.rs` 的 `waiting_reason_to_kind` 函数补充 `AskUser` 映射（保证 match 穷尽）**

修改 `waiting_reason_to_kind`（约第 297-309 行），在 `WaitingReason::User | WaitingReason::Approval => WaitingReasonKind::User,` 分支中加入 `WaitingReason::AskUser`：

```rust
fn waiting_reason_to_kind(reason: &WaitingReason) -> WaitingReasonKind {
    match reason {
        WaitingReason::Agent => WaitingReasonKind::Agent,
        WaitingReason::ToolExecution
        | WaitingReason::Session { .. }
        | WaitingReason::SubTaskBatch { .. } => WaitingReasonKind::Tool,
        WaitingReason::User | WaitingReason::Approval | WaitingReason::AskUser => {
            WaitingReasonKind::User
        }
        WaitingReason::RetryBackoff => WaitingReasonKind::Retry,
        WaitingReason::Evaluator | WaitingReason::Summarization | WaitingReason::ChatAgent => {
            WaitingReasonKind::Other
        }
    }
}
```

> **理由：** `AskUser` 语义上等同 `User`（等待用户输入），归为 `WaitingReasonKind::User` 让前端展示与 `Waiting(User)` 一致。

- [ ] **步骤 6：在 `src/systems/frontend_output.rs` 的 `waiting_reason_to_kind_mappings` 测试中追加 `AskUser` 用例**

修改测试（约第 1132-1159 行），在 `cases` 数组中追加：

```rust
        (WaitingReason::AskUser, WaitingReasonKind::User),
```

完整的 cases 数组：

```rust
        let cases = [
            (WaitingReason::Agent, WaitingReasonKind::Agent),
            (WaitingReason::User, WaitingReasonKind::User),
            (WaitingReason::Approval, WaitingReasonKind::User),
            (WaitingReason::AskUser, WaitingReasonKind::User),
            (WaitingReason::RetryBackoff, WaitingReasonKind::Retry),
            (WaitingReason::Evaluator, WaitingReasonKind::Other),
            (
                WaitingReason::Session {
                    handle_id: Uuid::new_v4(),
                },
                WaitingReasonKind::Tool,
            ),
            (
                WaitingReason::SubTaskBatch {
                    batch_id: Uuid::new_v4(),
                },
                WaitingReasonKind::Tool,
            ),
        ];
```

- [ ] **步骤 7：运行编译与单元测试验证**

运行：`cargo build --all-features 2>&1 | tail -30`
预期：编译通过（可能伴随 `unused variable` 之类的无害警告，因为新类型尚未被使用）

运行：`cargo test --all-features waiting_reason_to_kind_mappings 2>&1 | tail -20`
预期：PASS

- [ ] **步骤 8：Commit**

```bash
git add src/domain/space.rs src/domain/message.rs src/domain/task.rs src/domain/mod.rs src/systems/frontend_output.rs
git commit -m "feat(domain): add ToolAction::AskUser, WaitingReason::AskUser, AskUserPending"
```

---

## 任务 2：AskUserTool 实现 + 单元测试

**文件：**
- 创建：`src/systems/tools/builtin/ask_user.rs`
- 修改：`src/systems/tools/builtin/mod.rs`（声明 `ask_user` 模块 + 导出 `AskUserTool`）

### 步骤

- [ ] **步骤 1：创建 `src/systems/tools/builtin/ask_user.rs`，先写测试**

```rust
//! ask_user Tool 实现
//!
//! 声明式 Sync 工具：executor 只解析参数并返回 `ToolAction::AskUser`，
//! 问题呈现与跨帧等待由 orchestrator 完成。

use crate::domain::{ToolAction, ToolContext, ToolError};

pub struct AskUserTool;

impl crate::domain::BuiltinTool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let question = input
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'question' parameter".to_string()))?
            .to_string();

        Ok(ToolAction::AskUser { question })
    }
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
            tool_inflight_timeout_secs: 300,
            current_task_id: Uuid::new_v4(),
            current_agent_id: Uuid::new_v4(),
            current_origin_channel: None,
        }
    }

    #[test]
    fn parse_valid_question_returns_ask_user_action() {
        let input = serde_json::json!({"question": "用什么框架?"});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::AskUser { question } => {
                assert_eq!(question, "用什么框架?");
            }
            other => panic!("expected AskUser, got {:?}", other),
        }
    }

    #[test]
    fn parse_missing_question_returns_error() {
        let input = serde_json::json!({});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => {
                assert!(msg.contains("question"), "msg: {msg}");
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn parse_non_string_question_returns_error() {
        let input = serde_json::json!({"question": 123});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_err());
    }

    #[test]
    fn parse_extra_fields_ignored() {
        let input = serde_json::json!({"question": "继续?", "extra": "ignored"});
        let result = AskUserTool.execute(&input, &tool_context());
        assert!(result.is_ok());
        match result.unwrap() {
            ToolAction::AskUser { question } => {
                assert_eq!(question, "继续?");
            }
            other => panic!("expected AskUser, got {:?}", other),
        }
    }
}
```

- [ ] **步骤 2：在 `src/systems/tools/builtin/mod.rs` 声明模块并导出**

在 `mod chat_with_agent;` 之后追加 `mod ask_user;`（按字母序插入到 `chat_with_agent` 之后、`create_tasks` 之前）：

```rust
mod ask_user;
mod chat_with_agent;
mod create_tasks;
```

在 `pub use chat_with_agent::ChatWithAgentTool;` 之前追加：

```rust
pub use ask_user::AskUserTool;
pub use chat_with_agent::ChatWithAgentTool;
```

- [ ] **步骤 3：运行测试验证（应失败——`AskUserTool` 已实现，预期通过）**

实际上步骤 1 已包含实现，所以这一步是验证测试通过：

运行：`cargo test --all-features --lib systems::tools::builtin::ask_user 2>&1 | tail -30`
预期：4 个测试全 PASS

- [ ] **步骤 4：Commit**

```bash
git add src/systems/tools/builtin/ask_user.rs src/systems/tools/builtin/mod.rs
git commit -m "feat(tool): implement ask_user declarative sync tool"
```

---

## 任务 3：注册 ask_user 工具

**文件：**
- 修改：`src/systems/tools/mod.rs:42-48`（导入 `AskUserTool`）
- 修改：`src/systems/tools/mod.rs`（在 `register_builtin_tools` 函数中追加注册块）

### 步骤

- [ ] **步骤 1：修改 `src/systems/tools/mod.rs` 的 use 语句，加入 `AskUserTool`**

修改 use 语句（约第 42-47 行），在 `ChatWithAgentTool,` 之后追加 `AskUserTool,`（按字母序在 `ChatWithAgentTool` 之前）：

```rust
use self::builtin::{
    AskUserTool, ChatWithAgentTool, CreateTasksTool, DeleteScheduledTaskTool,
    ListExperienceCandidatesTool, ListScheduledTasksTool, ScheduleTaskTool, ShellExecTool,
    ShellInputTool, ShellListTool, ShellReadTool, ShellStartTool, ShellStopTool,
    SkipProfileUpdateTool, SubmitExperienceCandidateTool, SubmitProfileUpdateTool,
    SubmitSkillUpdateTool, WaitTasksTool,
};
```

- [ ] **步骤 2：在 `register_builtin_tools` 函数中追加 `ask_user` 注册块**

在 `chat_with_agent` 注册块之前（约第 318 行，`// chat_with_agent tool` 注释之前）插入：

```rust
    // ask_user tool
    registry.register(ToolDefinition {
        name: "ask_user".to_string(),
        description: "向用户提出问题并等待回复。当需要用户提供偏好、确认方向或补充信息时调用。\
                      问题应清晰具体，让用户能直接文本回复。".to_string(),
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
```

- [ ] **步骤 3：编译验证**

运行：`cargo build --all-features 2>&1 | tail -20`
预期：编译通过

- [ ] **步骤 4：Commit**

```bash
git add src/systems/tools/mod.rs
git commit -m "feat(tool): register ask_user tool definition and executor"
```

---

## 任务 4：orchestrator `handle_tool_action` 新增 `AskUser` arm + 签名扩展

**文件：**
- 修改：`src/systems/tools/orchestrator.rs:353-383`（`handle_tool_action` 签名扩展）
- 修改：`src/systems/tools/orchestrator.rs`（在 `match action` 中新增 `AskUser` arm）
- 修改：`src/systems/tools/orchestrator.rs:11-22`（导入 `FrontendRegistry` 与 `AskUserPending` 等）
- 修改：`src/systems/tools/dispatch.rs:246-264`（调用点传入 `frontend_registry`）
- 修改：`src/systems/tools/confirmation.rs:346-364`（调用点传入 `frontend_registry`）
- 修改：`src/systems/tools/approval.rs:324-342`（调用点传入 `frontend_registry`）
- 测试：`src/systems/tools/orchestrator.rs` 的 `#[cfg(test)] mod tests`

### 步骤

- [ ] **步骤 1：在 `src/systems/tools/orchestrator.rs` 顶部导入中追加 `FrontendRegistry` 与 `AskUserPending`**

修改 use 语句（约第 10-22 行），加入：

```rust
use crate::app::{Clock, FrontendRegistry};
```

并把 `WaitingForTasksInfo, WaitingReason, WorkItem,` 一行替换为包含 `AskUserPending, EngineEvent, EventTarget, MessageRole,`：

```rust
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionResult, AgentId, AgentKind, AskUserPending,
    BatchTaskState, ChannelId, ChatRoundStartedMessage, ChatSession, DispatchHint, DispatchKind,
    DispatchStrategy, EntryRole, EngineEvent, EventTarget, ExperienceCandidate,
    ExperienceCandidatePayload, ExperienceCandidateSubmission, ExperienceKindHint,
    ExperienceStore, FrontendKind, MessageRole, OutputContent, PendingDispatch,
    PendingExperienceHooks, ProfileGenerationContext, SessionSummary, ShellSessionResult,
    ShortTermMemory, SkillUpdateContext, SubTaskBatchCreatedMessage, SubTaskBatchState,
    SubTaskConfig, SubTaskDefinition, Task, TaskId, TaskStatus, ToolAction, ToolCallingState,
    ToolError, ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolReturnedHookPending,
    WaitingForTasksInfo, WaitingReason, WorkItem,
};
```

> 注：原 use 语句中 `use crate::app::Clock;` 改为 `use crate::app::{Clock, FrontendRegistry};`；`EngineEvent, EventTarget, MessageRole,` 加入到 `crate::domain::{...}` 中（需确认这些类型当前是否已在其他 use 中——若已在则只追加缺失项）。

- [ ] **步骤 2：扩展 `handle_tool_action` 函数签名，新增 `frontend_registry: &FrontendRegistry` 参数**

修改 `pub fn handle_tool_action<B: SessionBackend>(...)`（约第 353-383 行），在 `calling_states: &Query<(Entity, &ToolCallingState)>,` 之后追加新参数：

```rust
pub fn handle_tool_action<B: SessionBackend>(
    commands: &mut Commands,
    request_entity: Entity,
    task_entity: Entity,
    request: &ToolExecutionRequestMessage,
    action: Result<ToolAction, ToolError>,
    tasks: &mut Query<(Entity, &mut Task)>,
    agents: &Query<&mut Agent>,
    chat_sessions: &Query<&ChatSession>,
    short_term_memories: &mut Query<&mut ShortTermMemory>,
    backend: &B,
    experience_store: &mut ExperienceStore,
    pending_experience_hooks: &mut PendingExperienceHooks,
    parent_agent_id: Option<AgentId>,
    clock: &Clock,
    // 合并 ProfileGenerationContext 与 SkillUpdateContext 查询：
    // 两者都是与 WorkItem 同 entity 的 Component，任一 WorkItem entity 至多只有其中之一。
    // 通过 Option<&...> 在单 SystemParam 中表达"存在与否"，避免触发 Bevy 16 参数上限。
    context_queries: &Query<(
        Entity,
        Option<&ProfileGenerationContext>,
        Option<&SkillUpdateContext>,
        &WorkItem,
    )>,
    skill_loader: &SkillLoader,
    // ToolCallingState 查询：用于在 ProfileGeneration 收尾路径
    // （SubmitProfileUpdate / SkipProfileUpdate）despawn 关联 State，
    // 阻止 tool_calling_orchestrator_system 触发 follow-up LLM 请求。
    // 按 (task_id, work_item_id) 严格匹配，与 find_calling_state 语义一致。
    calling_states: &Query<(Entity, &ToolCallingState)>,
    frontend_registry: &FrontendRegistry,
) {
```

- [ ] **步骤 3：在 `match action` 中新增 `Ok(ToolAction::AskUser { question }) => { ... }` arm**

在 `Ok(ToolAction::StartChatRound { ... }) => { ... }` arm 之前（约第 653 行之前）插入新 arm。该位置在 `SubmitSkillUpdate` / `SkipProfileUpdate` 等其他 arm 之后，符合规格 §3.1 的实现：

```rust
        Ok(ToolAction::AskUser { question }) => {
            let task_id = request.request.task_id;
            let agent_id = request.request.agent_id;
            let tool_call_id = request.tool_call_id.clone();

            // 1. 读取 task 的 output_channel
            let output_channel = tasks
                .get(task_entity)
                .map(|(_, t)| t.routing_policy.output_channel.clone())
                .ok()
                .flatten();

            // 2. 无 output_channel 时返回错误（避免 task 永远卡在 Waiting(AskUser)）
            let Some(channel) = output_channel else {
                spawn_tool_error(
                    commands,
                    request_entity,
                    request,
                    ToolError::InvalidInput(
                        "ask_user requires task with output_channel".to_string(),
                    ),
                );
                return;
            };

            // 3. 通过 EngineEvent::Text 把问题推送到 output_channel
            let event = EngineEvent::Text {
                target: EventTarget::Directed(vec![channel]),
                role: MessageRole::Agent,
                content: question.clone(),
                task_id: Some(task_id),
            };
            for frontend in &frontend_registry.frontends {
                frontend.push_event(event.clone());
            }

            // 4. 在 task entity 上挂 AskUserPending（先 insert，再切 status，保证不变量）
            commands.entity(task_entity).insert(AskUserPending {
                tool_call_id,
                agent_id,
            });

            // 5. task.status = Waiting(AskUser)
            if let Ok((_, mut task)) = tasks.get_mut(task_entity) {
                task.status = TaskStatus::Waiting(WaitingReason::AskUser);
            }

            // 6. despawn ToolExecutionRequestMessage
            commands.entity(request_entity).despawn();
        }
```

> **关键不变量：** 先 `insert(AskUserPending)` 再设 `task.status = Waiting(AskUser)`，保证 `Waiting(AskUser)` 与 `AskUserPending` 原子配对（详见规格 §4.4）。

- [ ] **步骤 4：更新 `src/systems/tools/dispatch.rs` 的 `handle_tool_action` 调用点**

修改调用（约第 246-264 行），在 `&calling_states,` 之后追加 `frontend_registry,`：

```rust
                    handle_tool_action(
                        &mut commands,
                        entity,
                        task_entity,
                        &request,
                        action,
                        &mut tasks,
                        &agents,
                        &chat_sessions,
                        &mut short_term_memories,
                        &*backend,
                        &mut experience_store,
                        &mut pending_experience_hooks,
                        parent_agent_id,
                        &index_clock_loader.1,
                        &context_queries,
                        &index_clock_loader.2,
                        &calling_states,
                        frontend_registry,
                    );
```

> 注：`frontend_registry` 已在 `dispatch.rs:66` 通过 `let frontend_registry = &index_clock_loader.3;` 解构，直接传入即可。

- [ ] **步骤 5：更新 `src/systems/tools/confirmation.rs` 的 `handle_tool_action` 调用点**

修改调用（约第 346-364 行），在 `&calling_states,` 之后追加 `frontend_registry,`：

```rust
                    handle_tool_action(
                        &mut commands,
                        request_entity,
                        task_entity,
                        tool_request,
                        action,
                        &mut tasks,
                        &agents,
                        &chat_sessions,
                        &mut short_term_memories,
                        &*backend,
                        &mut experience_store,
                        &mut pending_experience_hooks,
                        None,
                        &index_clock_loader_frontends.1,
                        &context_queries,
                        &index_clock_loader_frontends.2,
                        &calling_states,
                        frontend_registry,
                    );
```

> 注：`frontend_registry` 已在 `confirmation.rs:67` 通过 `let frontend_registry = &index_clock_loader_frontends.3;` 解构。

- [ ] **步骤 6：更新 `src/systems/tools/approval.rs` 的 `handle_tool_action` 调用点**

修改调用（约第 324-342 行），在 `&calling_states,` 之后追加 `frontend_registry,`：

```rust
                    handle_tool_action(
                        &mut commands,
                        request_entity,
                        task_entity,
                        tool_request,
                        action,
                        &mut tasks,
                        &agents,
                        &chat_sessions,
                        &mut short_term_memories,
                        &*backend,
                        &mut experience_store,
                        &mut pending_experience_hooks,
                        None,
                        &index_clock_loader_frontends.1,
                        &context_queries,
                        &index_clock_loader_frontends.2,
                        &calling_states,
                        frontend_registry,
                    );
```

> 注：`frontend_registry` 已在 `approval.rs:136` 通过 `let frontend_registry = &index_clock_loader_frontends.3;` 解构。

- [ ] **步骤 7：编译验证**

运行：`cargo build --all-features 2>&1 | tail -30`
预期：编译通过

- [ ] **步骤 8：在 `src/systems/tools/orchestrator.rs` 的 `#[cfg(test)] mod tests` 中新增 ask_user 相关测试**

在 `tests` 模块中追加测试辅助函数与 5 个测试用例。先在 tests 模块顶部追加必要的 use 与辅助函数：

```rust
    use crate::app::FrontendRegistry;
    use crate::domain::{ChannelId, FrontendKind, TaskRoutingPolicy};
    use std::sync::{Arc, Mutex};

    /// MockFrontend：捕获推送的 EngineEvent，供测试断言
    struct MockFrontend {
        kind: FrontendKind,
        events: Arc<Mutex<Vec<EngineEvent>>>,
    }

    impl crate::domain::Frontend for MockFrontend {
        fn kind(&self) -> FrontendKind {
            self.kind
        }
        fn push_event(&self, event: EngineEvent) {
            self.events.lock().unwrap().push(event);
        }
        fn poll_actions(&self) -> Vec<crate::domain::UserAction> {
            vec![]
        }
    }

    /// 构造一个有 output_channel 的 task
    fn make_task_with_channel(channel: ChannelId) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: Uuid::new_v4(),
            content: "ask".to_string(),
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        }
    }

    /// 测试系统：调用 handle_tool_action 处理 AskUser action
    #[allow(clippy::too_many_arguments)]
    fn run_ask_user_action_system(
        mut commands: Commands,
        mut tasks: Query<(Entity, &mut Task)>,
        frontend_registry: Res<FrontendRegistry>,
    ) {
        let (task_entity, _) = tasks
            .iter_mut()
            .next()
            .expect("task entity should exist");
        let request_entity = commands.spawn(()).id();
        let request = ToolExecutionRequestMessage {
            request: crate::domain::AgentExecutionRequest {
                task_id: Uuid::new_v4(),
                agent_id: Uuid::nil(),
                request_kind: AgentRequestKind::LlmCompletion,
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                work_item_id: None,
            },
            tool_name: "ask_user".to_string(),
            tool_input: serde_json::json!({"question": "用什么框架?"}),
            tool_call_id: Some("call_123".to_string()),
            pending_confirmation_id: None,
            processed: false,
        };

        // 简化：跳过 backend / context_queries / skill_loader / calling_states 等，
        // 通过空实现或占位传参（测试只关注 AskUser arm，不会触及其他 arm）
        let backend = crate::infrastructure::NativeProcessBackend::default();
        let knowledge = SharedKnowledgeBase::default();
        let mut experience_store = ExperienceStore::default();
        let mut pending_hooks = PendingExperienceHooks::default();
        let agents: Query<&mut Agent> = Query::default();
        let chat_sessions: Query<&ChatSession> = Query::default();
        let mut short_term_memories: Query<&mut ShortTermMemory> = Query::default();
        let context_queries: Query<(
            Entity,
            Option<&ProfileGenerationContext>,
            Option<&SkillUpdateContext>,
            &WorkItem,
        )> = Query::default();
        let skill_loader = SkillLoader::default();
        let calling_states: Query<(Entity, &ToolCallingState)> = Query::default();
        let clock = Clock(chrono::Utc::now());

        handle_tool_action(
            &mut commands,
            request_entity,
            task_entity,
            &request,
            Ok(ToolAction::AskUser {
                question: "用什么框架?".to_string(),
            }),
            &mut tasks,
            &agents,
            &chat_sessions,
            &mut short_term_memories,
            &backend,
            &mut experience_store,
            &mut pending_hooks,
            None,
            &clock,
            &context_queries,
            &skill_loader,
            &calling_states,
            &frontend_registry,
        );
    }
```

> **重要说明：** 上面的测试辅助函数中，`SharedKnowledgeBase` / `SkillLoader` / `NativeProcessBackend` 等类型需要 default 构造。如果某些类型没有 `Default` 实现，需要在测试中改用具体构造或省略测试中不使用的字段——以实际编译错误为准调整。

然后追加 5 个测试用例：

```rust
    #[test]
    fn ask_user_action_sets_task_to_waiting_ask_user() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Telegram,
                events: events.clone(),
            })],
        });
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = make_task_with_channel(channel);
        app.world_mut().spawn(task);
        app.add_systems(Update, run_ask_user_action_system);

        app.update();

        let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
        assert_eq!(
            tasks[0].status,
            TaskStatus::Waiting(WaitingReason::AskUser),
            "task should be in Waiting(AskUser) state"
        );
    }

    #[test]
    fn ask_user_action_attaches_ask_user_pending_component() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Telegram,
                events: events.clone(),
            })],
        });
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = make_task_with_channel(channel);
        app.world_mut().spawn(task);
        app.add_systems(Update, run_ask_user_action_system);

        app.update();

        let pendings: Vec<&AskUserPending> = app
            .world()
            .query::<&AskUserPending>()
            .iter(app.world())
            .collect();
        assert_eq!(pendings.len(), 1, "AskUserPending should be attached");
        assert_eq!(pendings[0].tool_call_id, Some("call_123".to_string()));
    }

    #[test]
    fn ask_user_action_pushes_text_event_to_output_channel() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Telegram,
                events: events.clone(),
            })],
        });
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = make_task_with_channel(channel);
        app.world_mut().spawn(task);
        app.add_systems(Update, run_ask_user_action_system);

        app.update();

        let events = events.lock().unwrap();
        let text_event = events
            .iter()
            .find_map(|e| match e {
                EngineEvent::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("should emit EngineEvent::Text");
        assert_eq!(text_event, "用什么框架?");
    }

    #[test]
    fn ask_user_action_without_output_channel_returns_error() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Tui,
                events: events.clone(),
            })],
        });
        // 构造无 output_channel 的 task（event 任务）
        let mut task = make_task_with_channel(ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "u1".to_string(),
            thread_id: None,
        });
        task.routing_policy.output_channel = None;
        app.world_mut().spawn(task);
        app.add_systems(Update, run_ask_user_action_system);

        app.update();

        // 应 spawn 一个 ToolExecutionResultMessage（错误结果），task 不进入 Waiting(AskUser)
        let errors: Vec<&ToolExecutionResultMessage> = app
            .world()
            .query::<&ToolExecutionResultMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(errors.len(), 1, "should spawn error result message");
        assert!(errors[0].tool_output.is_err(), "tool_output should be Err");

        let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
        assert_ne!(
            tasks[0].status,
            TaskStatus::Waiting(WaitingReason::AskUser),
            "task should not enter Waiting(AskUser) when no output_channel"
        );
    }

    #[test]
    fn ask_user_action_despawns_request_entity() {
        let mut app = App::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(MockFrontend {
                kind: FrontendKind::Telegram,
                events: events.clone(),
            })],
        });
        let channel = ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "u1".to_string(),
            thread_id: None,
        };
        let task = make_task_with_channel(channel);
        app.world_mut().spawn(task);
        app.add_systems(Update, run_ask_user_action_system);

        // 在 system 运行前 spawn 一个空的 request entity
        let request_marker = app
            .world_mut()
            .spawn(ToolExecutionRequestMessage {
                request: crate::domain::AgentExecutionRequest {
                    task_id: Uuid::new_v4(),
                    agent_id: Uuid::nil(),
                    request_kind: AgentRequestKind::LlmCompletion,
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    work_item_id: None,
                },
                tool_name: "ask_user".to_string(),
                tool_input: serde_json::json!({}),
                tool_call_id: None,
                pending_confirmation_id: None,
                processed: false,
            })
            .id();

        app.update();

        // 注：上面的 run_ask_user_action_system 内部自己 spawn 了 request_entity
        // 并 despawn 它，外部 spawn 的 request_marker 不在 system 处理范围内。
        // 这个测试主要验证 system 内部 despawn 逻辑不会 panic。
        // 真正的 despawn 验证由 e2e 测试覆盖。
        let _ = request_marker; // 静默 unused warning
    }
```

> **测试说明：** 第 5 个测试 `ask_user_action_despawns_request_entity` 实际上无法在 system 抽象层精确验证 despawn，因为 `run_ask_user_action_system` 内部 spawn 自己的 request entity。真正的 despawn 验证依赖 e2e 测试。如果该测试价值不大，可省略——以实际实现时的判断为准。

- [ ] **步骤 9：运行测试验证**

运行：`cargo test --all-features --lib systems::tools::orchestrator::tests::ask_user 2>&1 | tail -50`
预期：5 个测试 PASS（或 4 个，如果省略第 5 个）

- [ ] **步骤 10：Commit**

```bash
git add src/systems/tools/orchestrator.rs src/systems/tools/dispatch.rs src/systems/tools/confirmation.rs src/systems/tools/approval.rs
git commit -m "feat(orchestrator): handle ToolAction::AskUser with cross-frame waiting"
```

---

## 任务 5：routing `user_input_routing_system` 新增 `Waiting(AskUser)` 分支

**文件：**
- 修改：`src/systems/routing.rs:1-13`（导入扩展）
- 修改：`src/systems/routing.rs:25-29`（Query 扩展为 mut）
- 修改：`src/systems/routing.rs`（新增 `Waiting(AskUser)` 分支与 `ask_user_pendings` Query）
- 测试：`src/systems/routing.rs` 的 `#[cfg(test)] mod tests`

### 步骤

- [ ] **步骤 1：修改 `src/systems/routing.rs` 顶部 use 语句，导入新类型**

修改 use 语句（约第 1-13 行），在 `ToolConfirmationResponseMessage,` 之后追加 `AskUserPending,` 和 `AgentExecutionResult, AgentExecutionOutput, AgentRequestKind, OutputContent, ToolExecutionResultMessage,`：

```rust
use crate::prelude::*;
use tracing::debug;

use crate::ecs::EntityIndex;
use crate::{
    app::Clock,
    domain::{
        Agent, AgentExecutionOutput, AgentExecutionResult, AgentId, AgentKind, AgentRequestKind,
        AskUserPending, ContinueTaskMessage, CreateTaskMessage, DispatchHint, DispatchKind,
        DispatchStrategy, EntryMetadata, EntryRole, OutputContent, PendingDispatch,
        ShortTermMemory, SystemOutputMessage, Task, TaskRoutingPolicy, TaskStatus,
        ToolConfirmationResponseMessage, ToolExecutionResultMessage, UserCommand, UserInputMessage,
        WaitingReason,
    },
};
```

> 注：原 use 已有 `Agent, AgentKind, ContinueTaskMessage, ...`；新加的是 `AgentExecutionOutput, AgentExecutionResult, AgentId, AgentRequestKind, AskUserPending, OutputContent, ToolExecutionResultMessage,`。`AgentId` 可能已通过 prelude 引入——以编译错误为准调整。

- [ ] **步骤 2：修改 `user_input_routing_system` 签名，扩展 Query**

修改函数签名（约第 25-29 行）：

```rust
/// 用户输入路由系统：判断是创建新任务还是继续现有任务
pub(crate) fn user_input_routing_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<(Entity, &mut Task)>,
    ask_user_pendings: Query<&AskUserPending>,
) {
```

> 注：原签名是 `tasks: Query<&Task>`，改为 `Query<(Entity, &mut Task)>`，因为新分支需要修改 task.status；Entity 也用于 `commands.entity(task_entity)` 操作。

- [ ] **步骤 3：在 `user_input_routing_system` 中插入 `Waiting(AskUser)` 分支**

在 confirmation 分支之后、`waiting_tasks.first()` 分支之前（约第 78-89 行之间），插入新分支：

```rust
        // ask_user 等待分支：用户回复 ask_user 工具的问题
        if let Some((task_entity, task)) = tasks.iter().find(|(_, t)| {
            t.status == TaskStatus::Waiting(WaitingReason::AskUser)
                && t.origin_channel == Some(input.origin_channel.clone())
        }) {
            if let Ok(pending) = ask_user_pendings.get(task_entity) {
                commands.spawn(ToolExecutionResultMessage {
                    result: AgentExecutionResult {
                        task_id: task.id,
                        agent_id: pending.agent_id,
                        request_kind: AgentRequestKind::LlmCompletion,
                        result: Ok(AgentExecutionOutput {
                            content: OutputContent::Text("ask_user completed".to_string()),
                            reasoning_content: None,
                        }),
                        prompt: String::new(),
                        system_prompt: None,
                        tools: vec![],
                        reasoning_content: None,
                        work_item_id: None,
                    },
                    tool_name: "ask_user".to_string(),
                    tool_output: Ok(serde_json::json!({"answer": input.content})),
                    tool_call_id: pending.tool_call_id.clone(),
                    processed: false,
                    original_tool_output: None,
                });
                commands.entity(task_entity).remove::<AskUserPending>();
                // 恢复 task 状态为 Waiting(ToolExecution)，让 LLM loop 续跑
                if let Ok((_, mut task)) = tasks.get_mut(task_entity) {
                    task.status = TaskStatus::Waiting(WaitingReason::ToolExecution);
                }
            }
            commands.entity(entity).despawn();
            continue;
        }
```

> **关键：** 由于 `tasks` 现在是 `Query<(Entity, &mut Task)>`，原代码中使用 `tasks.iter().find(...)` 的地方需要相应调整解构。详见步骤 4-5。

- [ ] **步骤 4：调整 confirmation 分支的 task 解构**

原 confirmation 分支（约第 37-78 行）：

```rust
        if let Some(task) = tasks.iter().find(|t| {
            t.status == TaskStatus::Waiting(WaitingReason::User)
                && t.origin_channel == Some(input.origin_channel.clone())
                && t.pending_confirmation_id.is_some()
        }) {
            let pending_id = task.pending_confirmation_id.expect("pending id confirmed above");
            // ... 原 logic 用 task.id ...
        }
```

改为：

```rust
        if let Some((_, task)) = tasks.iter().find(|(_, t)| {
            t.status == TaskStatus::Waiting(WaitingReason::User)
                && t.origin_channel == Some(input.origin_channel.clone())
                && t.pending_confirmation_id.is_some()
        }) {
            let pending_id = task.pending_confirmation_id.expect("pending id confirmed above");
            // ... 原 logic 用 task.id 不变 ...
        }
```

> 注：`tasks.iter()` 在 `Query<(Entity, &mut Task)>` 上返回 `(&Entity, &mut Task)` 元组，所以原 `tasks.iter().find(|t| ...)` 改为 `tasks.iter().find(|(_, t)| ...)`，原 `task.id` 等访问保持不变（只是变量名从 `task` 改为通过元组解构获得）。

- [ ] **步骤 5：调整 `waiting_tasks.first()` 分支的解构**

原代码（约第 80-105 行）：

```rust
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Waiting(WaitingReason::User)
                    && t.origin_channel == Some(input.origin_channel.clone())
            })
            .collect();

        if let Some(task) = waiting_tasks.first() {
            // ... 用 task.id ...
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            // ... create_new_task ...
        }
```

改为：

```rust
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|(_, t)| {
                t.status == TaskStatus::Waiting(WaitingReason::User)
                    && t.origin_channel == Some(input.origin_channel.clone())
            })
            .map(|(_, t)| t.clone())
            .collect();

        if let Some(task) = waiting_tasks.first() {
            // ... 用 task.id ...
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            // ... create_new_task ...
        }
```

> 注：通过 `.map(|(_, t)| t.clone())` 把 `&mut Task` 转 `Task`，避免借用问题。原 `task` 变量是 `&Task`，新代码中是 `&Task`（来自 Vec<Task>），访问方式不变。

- [ ] **步骤 6：编译验证**

运行：`cargo build --all-features 2>&1 | tail -30`
预期：编译通过（原 routing 测试可能因签名变化需要调整）

- [ ] **步骤 7：调整 `routing.rs` 现有测试中的 `tasks` Query 用法**

现有测试中 `app.add_systems(Update, user_input_routing_system);` 注册方式不变。但 `tasks` 签名变为 `Query<(Entity, &mut Task)>` 后，测试中 spawn task 的方式不变，因为 Bevy 会自动按新签名查询。**检查现有测试是否仍通过：**

运行：`cargo test --all-features --lib systems::routing 2>&1 | tail -30`
预期：所有现有 routing 测试仍 PASS

> 如果有失败，通常是因为现有测试代码中 task 没有附带 Entity（不会，因为 spawn 自动生成 Entity）或 task 不可变（不需要变）。如有失败，逐个修复。

- [ ] **步骤 8：在 `routing.rs` 的 `#[cfg(test)] mod tests` 中追加 6 个 ask_user 测试**

在 tests 模块中追加测试辅助函数与 6 个测试用例：

```rust
    fn make_ask_user_waiting_task(channel: ChannelId, agent_id: uuid::Uuid) -> (Task, AskUserPending) {
        let mut task = make_waiting_task(channel);
        task.status = TaskStatus::Waiting(WaitingReason::AskUser);
        let pending = AskUserPending {
            tool_call_id: Some("test_call_id".to_string()),
            agent_id,
        };
        (task, pending)
    }

    #[test]
    fn ask_user_reply_routes_to_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let agent_id = uuid::Uuid::new_v4();
        let (task, pending) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        let task_id = task.id;
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut().entity(task_entity).insert(pending);

        app.world_mut().spawn(UserInputMessage {
            content: "用 React".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let results: Vec<&ToolExecutionResultMessage> = app
            .world()
            .query::<&ToolExecutionResultMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_name, "ask_user");
        assert_eq!(results[0].tool_output, Ok(serde_json::json!({"answer": "用 React"})));
        assert_eq!(results[0].tool_call_id, Some("test_call_id".to_string()));
        assert_eq!(results[0].result.task_id, task_id);
    }

    #[test]
    fn ask_user_reply_removes_pending_component() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let agent_id = uuid::Uuid::new_v4();
        let (task, pending) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut().entity(task_entity).insert(pending);

        app.world_mut().spawn(UserInputMessage {
            content: "用 React".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let pendings: Vec<&AskUserPending> = app
            .world()
            .query::<&AskUserPending>()
            .iter(app.world())
            .collect();
        assert!(pendings.is_empty(), "AskUserPending should be removed");
    }

    #[test]
    fn ask_user_reply_restores_task_to_waiting_tool_execution() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let agent_id = uuid::Uuid::new_v4();
        let (task, pending) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        let task_entity = app.world_mut().spawn(task).id();
        app.world_mut().entity(task_entity).insert(pending);

        app.world_mut().spawn(UserInputMessage {
            content: "用 React".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
        assert_eq!(
            tasks[0].status,
            TaskStatus::Waiting(WaitingReason::ToolExecution),
            "task should be restored to Waiting(ToolExecution)"
        );
    }

    #[test]
    fn cross_channel_input_not_routed_to_ask_user_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let agent_id = uuid::Uuid::new_v4();
        let (task, pending) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        app.world_mut().spawn(task).insert(pending);

        // QQ 通道的输入不应路由到 Telegram 的 ask_user 任务
        app.world_mut().spawn(UserInputMessage {
            content: "hello from QQ".to_string(),
            origin_channel: qq_channel(),
        });

        app.update();

        let results: Vec<&ToolExecutionResultMessage> = app
            .world()
            .query::<&ToolExecutionResultMessage>()
            .iter(app.world())
            .collect();
        assert!(
            results.is_empty(),
            "QQ input should not be routed to Telegram ask_user task"
        );

        // 应该创建新任务
        let creates: Vec<&CreateTaskMessage> = app
            .world()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(creates.len(), 1, "should create new task for QQ input");
    }

    #[test]
    fn command_during_ask_user_still_executes() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let agent_id = uuid::Uuid::new_v4();
        let (task, pending) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        app.world_mut().spawn(task).insert(pending);

        // 输入是命令（/finish）
        app.world_mut().spawn(UserInputMessage {
            content: "/finish".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        // 命令应被 user_input_routing_system 跳过（不处理，留给 command_parse_system）
        let results: Vec<&ToolExecutionResultMessage> = app
            .world()
            .query::<&ToolExecutionResultMessage>()
            .iter(app.world())
            .collect();
        assert!(
            results.is_empty(),
            "command should not be routed as ask_user reply"
        );
    }

    #[test]
    fn multiple_ask_user_tasks_same_channel_picks_first() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let agent_id = uuid::Uuid::new_v4();
        let (task1, pending1) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        let (task2, pending2) = make_ask_user_waiting_task(telegram_channel(), agent_id);
        let task1_id = task1.id;
        let _task2_id = task2.id;

        let task1_entity = app.world_mut().spawn(task1).id();
        app.world_mut().entity(task1_entity).insert(pending1);
        let _task2_entity = app.world_mut().spawn(task2).id();
        app.world_mut().entity(_task2_entity).insert(pending2);

        app.world_mut().spawn(UserInputMessage {
            content: "回复".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let results: Vec<&ToolExecutionResultMessage> = app
            .world()
            .query::<&ToolExecutionResultMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(results.len(), 1, "only one task should be picked");
        assert_eq!(results[0].result.task_id, task1_id, "should pick first task");
    }
```

- [ ] **步骤 9：运行测试验证**

运行：`cargo test --all-features --lib systems::routing 2>&1 | tail -50`
预期：所有现有测试 + 6 个新测试 PASS

- [ ] **步骤 10：Commit**

```bash
git add src/systems/routing.rs
git commit -m "feat(routing): route user replies to Waiting(AskUser) tasks"
```

---

## 任务 6：`summarize_tool_input` 新增 `ask_user` 分支

**文件：**
- 修改：`src/domain/frontend.rs:172-223`（`summarize_tool_input` 函数）
- 测试：`src/domain/frontend.rs` 的 `#[cfg(test)] mod tests`

### 步骤

- [ ] **步骤 1：在 `summarize_tool_input` 函数中新增 `ask_user` 分支**

修改函数（约第 173-223 行），在 `"create_tasks" => ...` 分支之前插入：

```rust
        "ask_user" => tool_input
            .get("question")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.chars().count() > 80 {
                    let truncated: String = s.chars().take(80).collect();
                    format!("{truncated}…")
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_default(),
```

- [ ] **步骤 2：在 `src/domain/frontend.rs` 的 tests 模块中追加单元测试**

在 tests 模块中追加：

```rust
    #[test]
    fn summarize_ask_user_short_question() {
        let input = serde_json::json!({"question": "用什么框架?"});
        assert_eq!(summarize_tool_input("ask_user", &input), "用什么框架?");
    }

    #[test]
    fn summarize_ask_user_long_question_truncated() {
        let long_question = "a".repeat(100);
        let input = serde_json::json!({"question": long_question});
        let result = summarize_tool_input("ask_user", &input);
        assert!(result.ends_with('…'));
        assert_eq!(result.chars().count(), 81);
    }

    #[test]
    fn summarize_ask_user_missing_question_returns_empty() {
        let input = serde_json::json!({});
        assert_eq!(summarize_tool_input("ask_user", &input), "");
    }
```

- [ ] **步骤 3：运行测试验证**

运行：`cargo test --all-features --lib domain::frontend::tests::summarize_ask_user 2>&1 | tail -20`
预期：3 个测试 PASS

- [ ] **步骤 4：Commit**

```bash
git add src/domain/frontend.rs
git commit -m "feat(frontend): summarize ask_user tool input for ToolCallStarted event"
```

---

## 任务 7：端到端集成测试

**文件：**
- 创建：`tests/ask_user_e2e_test.rs`

### 步骤

- [ ] **步骤 1：查看现有 e2e 测试参考**

参考 `tests/` 目录下已有 e2e 测试（如 `tests/chat_with_agent_e2e_test.rs` 或类似文件），了解如何：
- 设置 App + FrontendRegistry + EntityIndex
- spawn task + ToolCallingState + ToolExecutionRequestMessage
- 触发 tool_dispatch_system
- 模拟用户输入并验证 follow-up LLM 请求触发

> **执行者注意：** 此任务依赖项目特定的 e2e 测试基础设施。如果 `tests/` 目录下没有类似 e2e 测试，需要先评估现有测试模式或简化为系统级集成测试。先 `ls tests/` 查看。

- [ ] **步骤 2：创建 `tests/ask_user_e2e_test.rs`**

```rust
//! ask_user 工具端到端集成测试
//!
//! 验证完整流程：
//! 1. ToolExecutionRequestMessage(ask_user) 经 tool_dispatch_system 处理
//! 2. orchestrator 推送 EngineEvent::Text 到 output_channel
//! 3. task 进入 Waiting(AskUser) 状态，挂载 AskUserPending
//! 4. 用户在同通道回复 UserInputMessage
//! 5. user_input_routing_system 识别 Waiting(AskUser)，spawn ToolExecutionResultMessage
//! 6. task 恢复至 Waiting(ToolExecution)
//!
//! 注：本测试不验证真实 LLM follow-up（需 mock provider），仅验证 ECS 状态流转
//! 与消息生成。LLM loop 续跑由 ingest_tool_results_system 触发，已由其他测试覆盖。

use harness::prelude::*;
use harness::domain::{
    AgentExecutionRequest, AgentRequestKind, AskUserPending, ChannelId, FrontendKind,
    TaskRoutingPolicy, TaskStatus, ToolExecutionRequestMessage, ToolExecutionResultMessage,
    UserInputMessage, WaitingReason,
};
use harness::ecs::EntityIndex;
use harness::app::FrontendRegistry;
use std::sync::{Arc, Mutex};

struct MockFrontend {
    kind: FrontendKind,
    events: Arc<Mutex<Vec<harness::domain::EngineEvent>>>,
}

impl harness::domain::Frontend for MockFrontend {
    fn kind(&self) -> FrontendKind {
        self.kind
    }
    fn push_event(&self, event: harness::domain::EngineEvent) {
        self.events.lock().unwrap().push(event);
    }
    fn poll_actions(&self) -> Vec<harness::domain::UserAction> {
        vec![]
    }
}

// 注：具体 crate 名（harness）以 Cargo.toml 中 [package] name 为准。
// 如果 crate 名带下划线或不同前缀，需要调整。

#[test]
fn e2e_ask_user_full_flow() {
    let mut app = App::new();

    // 1. 注册 FrontendRegistry
    let events = Arc::new(Mutex::new(Vec::new()));
    app.insert_resource(FrontendRegistry {
        frontends: vec![Box::new(MockFrontend {
            kind: FrontendKind::Telegram,
            events: events.clone(),
        })],
    });
    app.insert_resource(EntityIndex::default());

    // 2. 注册必要的 systems（按实际 app 模块的注册函数）
    // 注：需要参考 src/app.rs 或 src/lib.rs 中的 app 构建逻辑，
    // 复用现有的 register_* 函数注册 tool_dispatch_system + user_input_routing_system
    // app.add_systems(Update, (tool_dispatch_system, user_input_routing_system).chain());

    // 3. spawn 一个带 output_channel 的 task
    let channel = ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "u1".to_string(),
        thread_id: None,
    };
    let task = Task::from_user_input("test ask_user", 3, channel.clone());
    let task_id = task.id;
    let task_entity = app.world_mut().spawn(task).id();
    app.world_mut()
        .resource_mut::<EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

    // 4. spawn ToolExecutionRequestMessage(ask_user)
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request: AgentExecutionRequest {
            task_id,
            agent_id: uuid::Uuid::nil(),
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            work_item_id: None,
        },
        tool_name: "ask_user".to_string(),
        tool_input: serde_json::json!({"question": "用什么框架?"}),
        tool_call_id: Some("call_abc".to_string()),
        pending_confirmation_id: None,
        processed: false,
    });

    // 5. 第一帧：tool_dispatch_system 处理 ask_user
    app.update();

    // 6. 断言：task 进入 Waiting(AskUser)，挂载 AskUserPending
    let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
    assert_eq!(tasks[0].status, TaskStatus::Waiting(WaitingReason::AskUser));

    let pendings: Vec<&AskUserPending> = app
        .world()
        .query::<&AskUserPending>()
        .iter(app.world())
        .collect();
    assert_eq!(pendings.len(), 1);

    // 7. 断言：FrontendRegistry 收到 EngineEvent::Text
    let emitted = events.lock().unwrap();
    assert!(
        emitted.iter().any(|e| matches!(
            e,
            harness::domain::EngineEvent::Text { content, .. } if content == "用什么框架?"
        )),
        "should push question text to frontend"
    );
    drop(emitted);

    // 8. 模拟用户在同通道回复
    app.world_mut().spawn(UserInputMessage {
        content: "用 React".to_string(),
        origin_channel: channel,
    });

    // 9. 第二帧：user_input_routing_system 处理用户回复
    app.update();

    // 10. 断言：spawn ToolExecutionResultMessage，包含 answer
    let results: Vec<&ToolExecutionResultMessage> = app
        .world()
        .query::<&ToolExecutionResultMessage>()
        .iter(app.world())
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_name, "ask_user");
    assert_eq!(
        results[0].tool_output,
        Ok(serde_json::json!({"answer": "用 React"}))
    );

    // 11. 断言：AskUserPending 被移除，task 恢复至 Waiting(ToolExecution)
    let pendings: Vec<&AskUserPending> = app
        .world()
        .query::<&AskUserPending>()
        .iter(app.world())
        .collect();
    assert!(pendings.is_empty());

    let tasks: Vec<&Task> = app.world().query::<&Task>().iter(app.world()).collect();
    assert_eq!(
        tasks[0].status,
        TaskStatus::Waiting(WaitingReason::ToolExecution)
    );
}
```

> **执行者注意：**
> 1. 上面的 `use harness::prelude::*;` 中的 `harness` 是 crate 名占位符。实际 crate 名以 `Cargo.toml` 中 `[package] name` 字段为准。先 `head Cargo.toml` 查看。
> 2. 测试中 `// app.add_systems(...)` 部分需要根据 `src/app.rs` 中实际的 app 构建函数调整。如果有 `HarnessApp::build()` 或类似的构建器，调用它；否则手动注册需要的 systems。
> 3. 如果完整 e2e 测试需要太多依赖（mock provider、完整 system 注册等），可以简化为只测 `tool_dispatch_system` + `user_input_routing_system` 两个 system 的协作，跳过真实 LLM 调用部分。

- [ ] **步骤 2：运行 e2e 测试**

运行：`cargo test --all-features --test ask_user_e2e_test 2>&1 | tail -50`
预期：测试 PASS（如失败，根据错误调整 app 注册逻辑）

- [ ] **步骤 3：Commit**

```bash
git add tests/ask_user_e2e_test.rs
git commit -m "test: add ask_user tool end-to-end integration test"
```

---

## 任务 8：文档同步

**文件：**
- 修改：`docs/current-state.md`（在 `chat_with_agent` 工具项之后补充 `ask_user`）
- 修改：`docs/async-tool-bridge.md` §5.2（声明式 Sync 工具列表补充 `ask_user`）

### 步骤

- [ ] **步骤 1：在 `docs/current-state.md` 的工具列表中补充 `ask_user`**

在 `chat_with_agent` 工具描述之后（约第 67 行后）追加：

```markdown
- `ask_user` 工具：LLM 在工具调用循环中向用户提出开放文本问题，用户回复作为工具结果返回（声明式 Sync 工具，详见 [async-tool-bridge.md](async-tool-bridge.md#sync-工具分类)）
```

- [ ] **步骤 2：在 `docs/async-tool-bridge.md` §5.2 "声明式 Sync 工具"列表补充 `ask_user`**

修改"声明式 Sync 工具（不上桥）"列表（约第 313-320 行），在 `create_tasks` 之后追加 `ask_user`：

```markdown
- __声明式 Sync 工具（不上桥）__：`execute` 本质是参数解析 + 返回 `ToolAction`
  枚举变体，零 I/O、零 await、零跨帧。真正执行（启 shell、读 session、提交 profile、
  创建 task）在 `tool_dispatch_system` 后续 system 完成。这类工具上桥等于「把参数解析
  丢到 worker 线程再回传 enum」，纯负优化。包含：
  - `shell/{start,read,input,list,stop}`
  - `submit_{profile_update,skill_update,experience_candidate}`
  - `skip_profile_update`
  - `create_tasks`
  - `ask_user`
```

- [ ] **步骤 3：运行 markdownlint 验证**

运行：`markdownlint docs/current-state.md docs/async-tool-bridge.md 2>&1 | tail -10`
预期：无错误（或仅保留与本次改动无关的既有警告）

- [ ] **步骤 4：Commit**

```bash
git add docs/current-state.md docs/async-tool-bridge.md
git commit -m "docs: sync ask_user tool in current-state and async-tool-bridge"
```

---

## 自检

### 1. 规格覆盖度

| 规格章节 | 实现任务 | 状态 |
|---------|---------|------|
| §1.1 工具注册 | 任务 3 步骤 2 | ✅ |
| §1.2 工具实现 | 任务 2 步骤 1 | ✅ |
| §2.1 ToolAction::AskUser | 任务 1 步骤 1 | ✅ |
| §2.2 WaitingReason::AskUser | 任务 1 步骤 2 | ✅ |
| §2.3 AskUserPending 组件 | 任务 1 步骤 3 | ✅ |
| §3.1 handle_tool_action AskUser arm | 任务 4 步骤 3 | ✅ |
| §3.2 handle_tool_action 签名扩展 | 任务 4 步骤 2 + 步骤 4-6 | ✅ |
| §4.1 user_input_routing_system 分支 | 任务 5 步骤 3 | ✅ |
| §4.2 Query 扩展 | 任务 5 步骤 2 | ✅ |
| §4.4 不变量（Waiting(AskUser) ↔ AskUserPending） | 任务 4 步骤 3 注释 + 任务 5 步骤 3 实现 | ✅ |
| §5 前端呈现（复用 EngineEvent::Text） | 任务 4 步骤 3（含 role 字段） | ✅ |
| §6 边界场景 | 任务 5 测试用例覆盖（cross_channel / command / multiple_tasks） | ✅ |
| §7 YAGNI | 计划中未引入超时/取消命令/结构化选项等 | ✅ |
| 测试策略-工具单元测试 | 任务 2 步骤 1（4 个测试） | ✅ |
| 测试策略-orchestrator 单元测试 | 任务 4 步骤 8（5 个测试） | ✅ |
| 测试策略-routing 单元测试 | 任务 5 步骤 8（6 个测试） | ✅ |
| 测试策略-e2e 集成测试 | 任务 7 步骤 2 | ✅ |
| 文档同步-current-state.md | 任务 8 步骤 1 | ✅ |
| 文档同步-async-tool-bridge.md | 任务 8 步骤 2 | ✅ |
| 规格偏离校正-EngineEvent::Text role 字段 | 任务 4 步骤 3 代码 | ✅ |
| 规格偏离校正-AskUserPending 位置（task.rs 而非 message.rs） | 任务 1 步骤 3 | ✅ |
| 规格偏离校正-waiting_reason_to_kind 映射 | 任务 1 步骤 5-6 | ✅ |
| 规格偏离校正-summarize_tool_input 分支 | 任务 6 | ✅ |

无遗漏。

### 2. 占位符扫描

- 无 "TODO" / "待定" / "后续实现"
- 任务 7 步骤 1 中有 `// app.add_systems(...)` 注释——这是给执行者的指引，已在注释中明确说明需根据实际 app 构建函数调整。如果执行者发现完整 e2e 太重，可以简化为系统级集成测试。这是必要的灵活性，不是占位符。
- 任务 4 步骤 8 中第 5 个测试 `ask_user_action_despawns_request_entity` 在注释中说明价值有限可省略——这是诚实标注，不是占位符。建议执行者根据实际价值判断。

无红旗。

### 3. 类型一致性

- `ToolAction::AskUser { question: String }` — 任务 1 定义，任务 2 / 任务 4 使用 ✅
- `WaitingReason::AskUser` — 任务 1 定义，任务 4 / 任务 5 / 任务 1（frontend_output 映射）使用 ✅
- `AskUserPending { tool_call_id: Option<String>, agent_id: AgentId }` — 任务 1 定义，任务 4（insert）/ 任务 5（read + remove）使用 ✅
- `handle_tool_action(... frontend_registry: &FrontendRegistry)` — 任务 4 步骤 2 定义，任务 4 步骤 4-6（三个调用点）使用 ✅
- `user_input_routing_system(... tasks: Query<(Entity, &mut Task)>, ask_user_pendings: Query<&AskUserPending>)` — 任务 5 步骤 2 定义，任务 5 步骤 3 使用 ✅
- `summarize_tool_input("ask_user", ...)` — 任务 6 实现，与 `dispatch.rs:175` 调用一致 ✅
- `EngineEvent::Text { target, role, content, task_id }` — 任务 4 步骤 3 使用，与 `src/domain/frontend.rs:105-110` 实际定义一致 ✅
- `ToolExecutionResultMessage { result, tool_name, tool_output, tool_call_id, processed, original_tool_output }` — 任务 5 步骤 3 使用，与 `spawn_tool_error` 等现有代码模式一致 ✅

类型一致。

---

## 执行交接

计划已完成并保存到 `docs/superpowers/plans/2026-08-02-ask-user-tool-implementation.md`。两种执行方式：

**1. 子代理驱动（推荐）** - 每个任务调度一个新的子代理，任务间进行审查，快速迭代

**2. 内联执行** - 在当前会话中使用 executing-plans 执行任务，批量执行并设有检查点

选哪种方式？
