# ask_user 工具设计

> **状态：当前有效**

## 背景

当前 LLM 在工具调用循环中无法主动向用户提出开放文本问题。现有的 `ToolConfirmation` 机制（[src/domain/message.rs:346-368](src/domain/message.rs)）只支持固定三选项（`allow_once` / `allow_always` / `deny`），无法承载"用什么框架？""倾向于哪个方案？"这类需要开放回答的协作场景。

LLM 需要一个工具，能在执行过程中主动向用户提问，并把用户的开放文本回复作为工具结果返回，从而在后续推理中利用用户输入。

## 设计目标

- LLM 可在任意工具调用循环中调用 `ask_user` 向用户提问
- 用户回复作为工具结果 `Ok({"answer": "<文本>"})` 返回给 LLM
- 复用现有跨帧等待模式（与 `chat_with_agent` / `wait_tasks` 同构）
- 不引入超时与专用取消命令（用户可用 `/finish` / `/clear` 终止任务）

## 核心决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 回答形态 | 纯开放文本 | 主动提问场景多需开放回答，固定选项限制过大 |
| 路由机制 | 新增 `WaitingReason::AskUser` 专用变体 | 与现有 `Waiting(User)` 路径完全隔离，语义清晰 |
| 取消/超时 | 不设超时，不设专用取消命令 | 符合 YAGNI；用户可用现有 `/finish` / `/clear` 终止 |
| 实现路径 | 声明式 Sync 工具 + 新 `ToolAction::AskUser` 变体 | 与 `chat_with_agent` 模式一致，符合现有架构 |
| 权限策略 | `ToolPermission::Allow` | LLM 主动提问无需用户确认"是否允许问" |
| 前端呈现 | 复用 `EngineEvent::Text` | 问题本质是文本，无需新增事件变体 |

## 数据流

```text
LLM 调用 ask_user(question="用什么框架?")
   │
   ▼
tool_dispatch_system（Allow 权限）
   │  executor.execute() → 返回 ToolAction::AskUser { question }
   ▼
handle_tool_action（orchestrator）处理 AskUser arm：
   │  ① 通过 EngineEvent::Text 把问题推送到 task.routing_policy.output_channel
   │  ② 在 task entity 上 insert AskUserPending { tool_call_id, agent_id }
   │  ③ task.status = Waiting(AskUser)
   │  ④ despawn ToolExecutionRequestMessage
   ▼
用户在同通道回复文本（如 "用 React"）
   │
   ▼
user_input_routing_system 识别 Waiting(AskUser) 任务：
   │  ① spawn ToolExecutionResultMessage {
   │       tool_output: Ok({"answer": "用 React"}),
   │       tool_call_id: ask_user_pending.tool_call_id,
   │       ...
   │     }
   │  ② 移除 AskUserPending 组件
   │  ③ 恢复 task 为 Waiting(ToolExecution)
   ▼
ingest_tool_results_system / barrier / restore
   │
   ▼
tool_calling_orchestrator_system 触发 follow-up LLM 请求
   │  LLM 在 tool calling loop 中看到 {"answer": "用 React"}
   ▼
LLM 继续推理（可再调用 ask_user 多轮，或调用其他工具，或终止）
```

## 设计

### 1. 工具定义与实现

#### 1.1 工具注册（[src/systems/tools/mod.rs](src/systems/tools/mod.rs)）

与 `chat_with_agent` / `create_tasks` 同级注册：

```rust
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

#### 1.2 工具实现（`src/systems/tools/builtin/ask_user.rs`）

仿 [chat_with_agent.rs](src/systems/tools/builtin/chat_with_agent.rs) 风格，声明式 Sync 工具：

```rust
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
            .ok_or_else(|| {
                ToolError::InvalidInput("missing 'question' parameter".to_string())
            })?
            .to_string();

        Ok(ToolAction::AskUser { question })
    }
}
```

`kind()` 缺省 `Sync`，符合 [async-tool-bridge.md §5.2](docs/async-tool-bridge.md) "声明式 Sync 工具不上桥"分类——`execute()` 是纯参数解析，零 I/O、零 await、零跨帧。

### 2. 领域类型新增

#### 2.1 `ToolAction::AskUser` 变体（[src/domain/space.rs](src/domain/space.rs)）

在 `ToolAction` 枚举中新增：

```rust
/// 向用户提出问题并等待开放文本回复。
/// executor 只负责解析参数，问题呈现与等待状态由 orchestrator 完成。
AskUser {
    /// 向用户展示的问题文本
    question: String,
},
```

#### 2.2 `WaitingReason::AskUser` 变体（[src/domain/message.rs](src/domain/message.rs)）

在 `WaitingReason` 枚举中新增：

```rust
/// ask_user 工具等待用户开放文本回复
AskUser,
```

与现有 `Agent` / `User` / `Approval` / `ToolExecution` / `ChatAgent` 等变体同级，遵循"每个等待场景独立 reason"的项目惯例。

#### 2.3 `AskUserPending` 组件

挂载到 task entity，保存恢复 LLM loop 所需的最小信息。风格与 `WaitingForTasksInfo` 一致：

```rust
#[derive(Component, Debug, Clone)]
pub struct AskUserPending {
    pub tool_call_id: Option<String>,
    pub agent_id: AgentId,
}
```

无 `timeout_at` 字段（已决定不设超时）。

### 3. orchestrator 处理逻辑

#### 3.1 `handle_tool_action` 新增 `AskUser` arm（[src/systems/tools/orchestrator.rs](src/systems/tools/orchestrator.rs)）

在 `match action` 中新增分支（与 `StartChatRound` 风格一致）：

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
        task_id,
        content: question.clone(),
    };
    for frontend in &frontend_registry.frontends {
        frontend.push_event(event.clone());
    }

    // 4. 在 task entity 上挂 AskUserPending
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

#### 3.2 `handle_tool_action` 签名扩展

`handle_tool_action` 是普通函数（非 system，不受 Bevy 16 参数限制），当前已有 17 个参数但不含 `frontend_registry`。直接新增参数 `frontend_registry: &FrontendRegistry`。

三个调用点（`tool_dispatch_system` / `tool_confirmation_result_system` / `approval_result_system`）的 system 签名中**已有** `FrontendRegistry` 在 `index_clock_loader` / `index_clock_loader_frontends` 元组内（参见 [dispatch.rs:58-63](src/systems/tools/dispatch.rs) / [confirmation.rs:59-64](src/systems/tools/confirmation.rs)），只需在调用 `handle_tool_action` 时从元组解构出 `frontend_registry` 并传入即可，**system 签名无需改动**。

### 4. routing 路由分支

#### 4.1 `user_input_routing_system` 新增 `Waiting(AskUser)` 分支（[src/systems/routing.rs](src/systems/routing.rs)）

在 confirmation 分支之后、`waiting_tasks.first()` 分支之前插入：

```rust
// 优先级：命令 > 工具确认 > ask_user 等待 > 任务等待用户输入继续 > 创建新任务

// ask_user 等待分支
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

#### 4.2 Query 扩展

`user_input_routing_system` 签名扩展：

- `tasks: Query<(Entity, &mut Task)>` — 改为 mut（用于恢复状态）
- `ask_user_pendings: Query<&AskUserPending>` — 新增

#### 4.3 路由优先级总览

```text
1. UserCommand::parse().is_command()         → 跳过，由 command_parse_system 处理
2. Waiting(User) + pending_confirmation_id    → 现有 confirmation 路径（1/2/3 选项）
3. Waiting(AskUser)                           → 【新增】ask_user 回答路径（开放文本）
4. Waiting(User) 无 pending_confirmation_id   → 现有 continue_task 路径
5. 无匹配                                     → 现有 create_new_task 路径
```

`Waiting(AskUser)` 与 `Waiting(User)` 互斥（一个 task 同一时刻只有一个 status），分支 2 和分支 3 不会冲突。

#### 4.4 不变量：`Waiting(AskUser)` ↔ `AskUserPending`

`task.status == Waiting(AskUser)` 与 task entity 上挂载 `AskUserPending` 组件是**原子配对**的——由 orchestrator 的 `AskUser` arm 在同一 `handle_tool_action` 调用中保证：先 `insert(AskUserPending)`，再设 `task.status = Waiting(AskUser)`。`user_input_routing_system` 的 ask_user 分支假设此不变量成立。

若理论不可达的不一致发生（`Waiting(AskUser)` 但无 `AskUserPending`），routing 分支中 `ask_user_pendings.get(task_entity)` 返回 `Err`，此时：
- 不会 spawn `ToolExecutionResultMessage`
- 不会恢复 task 状态
- input entity 仍被 despawn（避免无限累积）
- task 卡在 `Waiting(AskUser)`，与其他 invariant 违反行为一致（如 `WaitingForTasksInfo` 丢失）

这是兜底语义，不额外加 recovery 路径——invariant 违反属于框架 bug，应通过修复 orchestrator 而非 routing 兜底。

### 5. 前端呈现

**复用 `EngineEvent::Text`**，不新增 `EngineEvent::QuestionAsked` 变体。

- 问题本质是一段文本，用户看到后自然知道要回答
- TUI 已有 task 状态显示（`Waiting(AskUser)` 会在 status 面板呈现）
- IM 通道（QQ/Telegram）问题文本本身就是提示
- `frontend_output_system` 已处理 `EngineEvent::Text`，**前端零改动**

### 6. 边界场景处理

| 场景 | 处理方式 |
|------|----------|
| task 无 `output_channel` | orchestrator 返回 `ToolError::InvalidInput`，task 不进入等待状态 |
| 用户发命令（/finish /clear /remember） | `user_input_routing_system` 顶部跳过，由 `command_parse_system` 处理；task 终态后 `AskUserPending` 随 despawn |
| 多个 task 同通道 `Waiting(AskUser)` | 沿用 `waiting_tasks.first()` 语义，取第一个 |
| 多轮 `ask_user` | LLM 收到答案后可再次调用，无次数限制 |
| 用户回复含路径（如 "/path/to/file"） | `UserCommand::parse()` 只识别已知命令，普通路径作为原始文本传给 LLM |

### 7. 不引入的能力（YAGNI）

- ❌ 超时机制
- ❌ 专用取消命令（如 `/skip`）
- ❌ 结构化选项（固定 candidates 列表）
- ❌ 多问题批量提问
- ❌ 问题历史持久化（工具结果经 LLM loop 进入 STM，无需额外存储）

## 测试策略

遵循 AGENTS.md 测试规范与 [async-tool-bridge.md §5.5](docs/async-tool-bridge.md) 测试纪律（一律 `#[test]`，禁 `#[tokio::test]`；跑 system 一律 `world.run_system_once(...)`）。

### 工具单元测试（`src/systems/tools/builtin/ask_user.rs`）

- `parse_valid_question_returns_ask_user_action`
- `parse_missing_question_returns_error`
- `parse_non_string_question_returns_error`
- `parse_extra_fields_ignored`

### orchestrator 单元测试（`src/systems/tools/orchestrator.rs`）

- `ask_user_action_sets_task_to_waiting_ask_user`
- `ask_user_action_attaches_ask_user_pending_component`
- `ask_user_action_pushes_text_event_to_output_channel`
- `ask_user_action_without_output_channel_returns_error`
- `ask_user_action_despawns_request_entity`

### routing 单元测试（`src/systems/routing.rs`）

- `ask_user_reply_routes_to_waiting_task`
- `ask_user_reply_removes_pending_component`
- `ask_user_reply_restores_task_to_waiting_tool_execution`
- `cross_channel_input_not_routed_to_ask_user_task`
- `command_during_ask_user_still_executes`
- `multiple_ask_user_tasks_same_channel_picks_first`

### 端到端集成测试（`tests/ask_user_e2e_test.rs`）

- `e2e_ask_user_full_flow`：LLM 调用 → 用户回复 → LLM 收到工具结果 → follow-up 请求触发

## 文档同步

| 文档 | 更新内容 |
|------|----------|
| `docs/current-state.md` | 在"已实现"工具列表中补充 `ask_user` |
| `docs/async-tool-bridge.md` | 在 §5.2"声明式 Sync 工具"列表补充 `ask_user` |
| `docs/configuration.md` | 无需更新（不引入新配置项） |
| `AGENTS.md` / `CLAUDE.md` | 无需更新（不涉及规范变化） |

## 涉及文件

| 文件 | 变更 |
|------|------|
| `src/domain/space.rs` | 新增 `ToolAction::AskUser` 变体 |
| `src/domain/message.rs` | 新增 `WaitingReason::AskUser` 变体 |
| `src/domain/mod.rs` | 导出 `AskUserPending` |
| `src/domain/space.rs` 或 `src/domain/message.rs` | 定义 `AskUserPending` 组件（建议放 `message.rs`，与 `WaitingForTasksInfo` 同位） |
| `src/systems/tools/builtin/ask_user.rs` | 新建工具实现 |
| `src/systems/tools/builtin/mod.rs` | 导出 `AskUserTool` |
| `src/systems/tools/mod.rs` | 注册 `ask_user` 工具定义与执行器 |
| `src/systems/tools/orchestrator.rs` | `handle_tool_action` 新增 `AskUser` arm + 签名扩展加 `frontend_registry` |
| `src/systems/tools/dispatch.rs` | 调用点同步新签名 |
| `src/systems/tools/confirmation.rs` | 调用点同步新签名 |
| `src/systems/tools/approval.rs` | 调用点同步新签名 |
| `src/systems/routing.rs` | `user_input_routing_system` 新增 `Waiting(AskUser)` 分支 + Query 扩展 |
| `docs/current-state.md` | 工具列表补充 |
| `docs/async-tool-bridge.md` | 声明式 Sync 工具列表补充 |
