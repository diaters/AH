> **状态：已归档（2026-06-10）** — 本规格描述的功能已实现。
> 相关能力已记录在 [docs/current-state.md](../../current-state.md)。

# Agent 自主创建子 Agent 功能设计

> 本文档描述 Agent 通过 Tool 主动创建子 Agent 的功能设计。

---

## 一、设计目标

- Agent 可以通过 `spawn_agent` Tool 创建子 Agent
- 子 Agent 与任务绑定，任务完成后自动销毁
- 子 Agent 拥有初始权限，运行时可动态申请额外权限
- 审批路由：父 Agent 有权限则父 Agent 审批，否则用户审批
- 支持单次授权和永久授权两种模式

---

## 二、整体架构

```
┌─────────────────────────────────────────────────────────────────┐
│                         父 Agent 执行                           │
└─────────────────────────────────────────────────────────────────┘
                                │
                    调用 spawn_agent Tool
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                    spawn_agent Tool 执行                        │
│  参数: name, model(可选), description, tools                    │
│                                                                 │
│  校验: tools 中每个权限必须父 Agent has_permission() == Allow   │
└─────────────────────────────────────────────────────────────────┘
                                │
                    生成 AgentSpawnRequestMessage
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                   agent_factory_system                          │
│  - 创建 TaskScoped 子 Agent                                     │
│  - 设置 tool_permissions (初始 tools 设置为 Allow)              │
│  - 设置 bound_task_id, parent_id                                │
└─────────────────────────────────────────────────────────────────┘
                                │
                    子 Agent 开始执行
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                   子 Agent 调用 Tool                            │
│                   tool_dispatch_system 权限检查                 │
└─────────────────────────────────────────────────────────────────┘
                                │
              ┌─────────────────┼─────────────────┐
              ▼                 ▼                 ▼
         Allow            Confirm            Deny
      直接执行          审批路由          返回拒绝
                                │
              ┌─────────────────┴─────────────────┐
              ▼                                   ▼
    ┌─────────────────────┐             ┌─────────────────────┐
    │ parent.has_permission│             │ !parent.has_permission│
    │     == Allow        │             │                      │
    └─────────────────────┘             └─────────────────────┘
              │                                   │
              ▼                                   ▼
    ┌─────────────────────┐             ┌─────────────────────┐
    │ ApprovalRequestMessage│            │ToolConfirmationRequest│
    │   (父 Agent 审批)    │             │Message (用户审批)    │
    └─────────────────────┘             └─────────────────────┘
              │                                   │
              └─────────────────┬─────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                   审批结果处理                                   │
│  - Approved + Permanent: 更新 Agent 权限                        │
│  - Approved + Once: 仅本次执行                                   │
│  - Rejected: 返回 PermissionDenied                              │
└─────────────────────────────────────────────────────────────────┘
```

---

## 三、数据结构

### 3.1 AgentSpawnRequestMessage（修订）

```rust
#[derive(Debug, Clone, Component)]
pub struct AgentSpawnRequestMessage {
    pub parent_agent_id: AgentId,
    pub task_id: TaskId,
    pub name: String,
    /// 可选，None 时继承父 Agent 的 model
    pub model: Option<String>,
    pub description: String,
    /// 初始 Tool 权限列表（每个 Tool 设为 Allow）
    pub tools: Vec<String>,
}
```

### 3.2 ApprovalResultMessage（扩展）

```rust
#[derive(Debug, Clone, Component)]
pub struct ApprovalResultMessage {
    pub request_id: Uuid,
    pub source_task_id: TaskId,
    pub approval_task_id: TaskId,
    pub decision: ApprovalDecision,
    pub reasoning: String,
    /// 新增：授权模式
    pub grant_mode: GrantMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantMode {
    /// 单次授权，仅本次执行
    Once,
    /// 永久授权，更新 Agent 权限配置
    Permanent,
}
```

### 3.3 ToolConfirmationRequestMessage（扩展）

```rust
#[derive(Debug, Clone, Component)]
pub struct ToolConfirmationRequestMessage {
    // ... 现有字段
    /// 新增：审批来源
    pub source: ConfirmationSource,
    /// 新增：父 Agent ID（当 source == ParentAgent 时）
    pub parent_agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationSource {
    User,
    ParentAgent,
}
```

### 3.4 Agent 工具方法（新增）

```rust
impl Agent {
    /// 判断是否拥有某 Tool 的 Allow 权限
    pub fn has_permission(&self, tool_name: &str) -> bool {
        self.tool_permissions.get_permission(tool_name) == ToolPermission::Allow
    }

    /// 授予永久权限
    pub fn grant_permission(&mut self, tool_name: String) {
        self.tool_permissions.overrides.insert(tool_name, ToolPermission::Allow);
    }
}
```

---

## 四、Tool 定义

### 4.1 spawn_agent Tool

```rust
ToolDefinition {
    name: "spawn_agent",
    description: "Create a child agent with specified tools and capabilities.
                  The child agent will be bound to the current task and
                  automatically terminated when the task completes.",
    parameters: ToolSchema {
        schema: json!({
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
        })
    },
    default_permission: ToolPermission::Confirm,
    executor: ToolExecutorKind::Builtin("spawn_agent"),
}
```

**权限说明**：
- `Allow`: 直接执行创建
- `Confirm`: 需要用户确认
- `Deny`: 禁止创建

---

## 五、权限判断与审批路由

### 5.1 权限判断标准

所有 Agent 统一使用 `has_permission()` 方法：

```rust
fn has_permission(&self, tool_name: &str) -> bool {
    self.tool_permissions.get_permission(tool_name) == ToolPermission::Allow
}
```

**注意**：仅 `Allow` 算拥有权限，`Confirm` 和 `Deny` 都不算。

### 5.2 审批路由逻辑

```
子 Agent 调用 Tool
    ↓
tool_dispatch_system 检查权限
    ↓
get_permission(tool) == ?
    ├─ Allow → 直接执行
    ├─ Deny  → 返回 PermissionDenied
    └─ Confirm → 判断审批路由
                    ↓
         parent_agent.has_permission(tool)?
             ├─ true  → ApprovalRequestMessage（父 Agent 审批）
             └─ false → ToolConfirmationRequestMessage（用户审批）
```

---

## 六、授权模式

| 模式 | 行为 |
|------|------|
| **Once** | 仅本次允许执行，不更新权限配置，下次使用仍需审批 |
| **Permanent** | 更新子 Agent `tool_permissions.overrides`，后续使用无需审批 |

复用现有 `ConfirmMode` 和 `ConfirmationOption`：

```rust
pub enum ConfirmMode {
    Once,
    Permanent,
}

impl ConfirmationOption {
    pub fn allow_once() -> Self { /* "allow_once" */ }
    pub fn allow_always() -> Self { /* "allow_always" */ }
    pub fn deny() -> Self { /* "deny" */ }
}
```

---

## 七、System 设计

### 7.1 修改/新增 System

| System | 状态 | 职责 |
|--------|------|------|
| `tool_dispatch_system` | 修改 | 扩展审批路由逻辑（区分父 Agent / 用户审批） |
| `approval_dispatch_system` | 修改 | 实现真正的审批任务创建（当前是 auto-reject） |
| `approval_result_system` | 新增 | 处理父 Agent 审批结果，更新权限，恢复任务 |
| `agent_factory_system` | 修改 | 支持 `tools` 参数，设置子 Agent 初始权限 |
| `tool_confirmation_result_system` | 保持 | 处理用户审批结果（已支持 Once/Permanent） |

### 7.2 System 调度顺序

```
Dispatch Set:
    brain_dispatch_system
        → task_dispatch_system
        → tool_dispatch_system
        → approval_dispatch_system

Transform Set:
    ingest_execution_results_system
        → llm_response_system
        → tool_result_system
        → approval_result_system
        → tool_confirmation_result_system
```

---

## 八、子 Agent 生命周期

### 8.1 创建流程

```
父 Agent 调用 spawn_agent Tool
    ↓
tool_dispatch_system 检查权限
    ↓ 权限检查通过
执行 spawn_agent builtin executor
    ↓ 校验 tools 为父 Agent 权限子集
生成 AgentSpawnRequestMessage
    ↓
agent_factory_system 处理
    ↓
创建子 Agent:
    - kind: TaskScoped
    - parent_id: Some(parent_agent_id)
    - bound_task_id: Some(task_id)
    - tool_permissions.overrides: tools.map(|t| (t, Allow))
    ↓
生成 AgentExecutionRequestMessage
    ↓
子 Agent 开始执行
```

### 8.2 销毁流程

```
任务完成/失败
    ↓
生成 TaskTerminatedMessage
    ↓
agent_factory_system.handle_termination
    ↓
查找 bound_task_id == task_id 的子 Agent
    ↓
despawn 子 Agent Entity
```

---

## 九、错误处理

### 9.1 spawn_agent Tool 错误

| 错误场景 | 处理方式 |
|----------|----------|
| 父 Agent 不存在 | `ToolError::ExecutionFailed("parent agent not found")` |
| tools 中有父 Agent 不拥有的权限 | 过滤掉，仅保留父 Agent 拥有的权限；若全部被过滤，返回错误 |
| 创建失败 | `ToolError::ExecutionFailed(...)` |

### 9.2 权限申请错误

| 错误场景 | 处理方式 |
|----------|----------|
| 子 Agent 无父 Agent | 直接走用户审批流程 |
| 审批被拒绝 | `ToolError::PermissionDenied(...)` |
| 审批超时 | `ToolError::Timeout(...)` |

### 9.3 错误传播

所有错误通过 `ToolExecutionResultMessage.tool_output: Err(ToolError)` 返回给 Agent，由 Agent 决定后续行为。

---

## 十、测试策略

### 10.1 单元测试

| 测试项 | 位置 |
|--------|------|
| `Agent.has_permission()` 方法 | `src/domain/mod.rs` |
| 权限路由逻辑（父 Agent vs 用户） | `src/systems/tool.rs` |
| Agent 创建与销毁 | `src/systems/maintenance.rs` |

### 10.2 集成测试

| 测试场景 | 验证点 |
|----------|--------|
| 父 Agent 创建子 Agent | 子 Agent 正确创建、权限正确设置 |
| 子 Agent 调用已授权 Tool | 直接执行成功 |
| 子 Agent 调用未授权 Tool（父 Agent 有权限） | 路由到父 Agent 审批 |
| 子 Agent 调用未授权 Tool（父 Agent 无权限） | 路由到用户审批 |
| 审批通过（Permanent） | 权限更新，后续调用无需审批 |
| 审批通过（Once） | 本次执行，后续仍需审批 |
| 审批拒绝 | 返回 PermissionDenied |
| 任务完成，子 Agent 销毁 | Agent Entity 正确 despawn |

---

## 十一、设计总结

| 维度 | 设计决策 |
|------|----------|
| **创建方式** | `spawn_agent` Tool，参数：name, model(可选), description, tools |
| **生命周期** | 任务绑定，任务完成/失败后自动销毁 |
| **权限初始化** | 创建时指定 tools 列表（父 Agent 权限子集） |
| **权限扩展** | 子 Agent 调用未授权 Tool 时自动触发审批 |
| **审批路由** | 父 Agent `has_permission() == Allow` → 父 Agent 审批；否则 → 用户审批 |
| **授权模式** | Once（单次）/ Permanent（永久，更新权限配置） |
| **消息复用** | 复用 `ApprovalRequestMessage`、`ToolConfirmationRequestMessage` |
