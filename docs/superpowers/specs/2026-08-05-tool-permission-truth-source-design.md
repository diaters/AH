# Tool 权限决策链路统一与可观测性设计

> **状态：当前有效**

## 背景

当前 tool 权限管理存在 6 项可优化点，集中表现为：权限决策链路存在死代码旁路、子 Agent 派生时权限被静默放大、`required_tag` 拼写错误静默失效、权限决策无结构化审计事件。

本设计将 6 项修复整合为一个连贯改造，分为 4 个独立可测试子模块。

## 范围

| 子模块 | 涉及优化项 | 核心改动 |
|--------|----------|---------|
| 权限真相源 | O1 / O5 / O6 | 新增 `effective_permission` 方法 + `PermissionSource` 枚举；删除 `get_permission` / `has_permission` |
| 子 Agent 权限继承 | O2 | `maintenance.rs` spawn 逻辑改用 `effective_permission` 逐工具继承 |
| `required_tag` 校验 | O7 | 启动期扫描 + warn |
| 权限审计事件 | O11 | 新增 `EngineEvent::PermissionAudit` 变体 |

不在范围内：O3（父审批 MVP 自动通过）、O4（Permanent grant 持久化）、O8（revoke 路径）、O9（批量确认）、O10（Deny 可操作反馈）、O12（confirmed_once 令牌化）—— 见原分析报告，后续单独推进。

## 现状梳理

### 三层权限来源（按优先级递减）

1. `Agent.tool_permissions.overrides: HashMap<tool_name, ToolPermission>` — 运行时 grant 或 agents.toml 显式配置
2. `Agent.tool_permissions.default_permission` — Agent 级默认（`AgentToolPermissions::default()` = Confirm）
3. `ToolDefinition.default_permission` — 工具注册时声明的静态默认

### 决策路径

[dispatch.rs](../../../src/systems/tools/dispatch.rs) `tool_dispatch_system` 按 `agent.tool_permissions.get_permission(tool_name)` 返回值分流：

- `Allow` → 直接执行
- `Confirm` → 查父 Agent 是否有 Allow 权限，有则走父审批；否则 fallback 到 UserConfirmation
- `Deny` → 拒绝

### 腐化点

1. **`ToolDefinition.default_permission` 是死代码**：[dispatch.rs:138](../../../src/systems/tools/dispatch.rs) 调用 `get_permission`，该方法只查 agent 的 overrides 和 default_permission，**不查 registry 中 ToolDefinition 的 default_permission**。工具注册时声明的 `shell_exec=Confirm`、`ask_user=Allow` 实际不生效。
2. **`get_permission` / `has_permission` 旁路**：若新增 `effective_permission` 而保留旧方法，同一 agent + 同一工具会得到不同结果，形成脱节抽象。按 AGENTS.md「代码腐化治理」原则，必须删除。
3. **子 Agent 派生权限放大**：[maintenance.rs:337-392](../../../src/systems/maintenance.rs) 过滤逻辑保留 Allow 和 Confirm，但把保留的工具一律设为 Allow。父 Agent 需要 Confirm 的工具，派生给子 Agent 后变成 Allow，子 Agent 可不经确认调用，绕过审批门。
4. **`required_tag` 无校验**：agents.toml 的 tags 是自由文本，拼写错误会静默失效。
5. **无结构化审计**：仅有 debug/info 级 `AgentPermissionUpdated` 日志，前端无法订阅、无统一查询入口。

## 设计

### 子模块 1：权限真相源（O1 / O5 / O6）

#### 新增类型

在 `src/domain/agent.rs` 新增：

```rust
/// 权限来源标识，用于审计与调试
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionSource {
    /// Agent 的 overrides 显式配置（运行时 grant 或 agents.toml 显式）
    AgentOverride,
    /// Agent 的 default_permission
    AgentDefault,
    /// ToolDefinition.default_permission（最后回退层）
    ToolDefault,
}
```

#### AgentToolPermissions 增加 explicit 标记

`AgentToolPermissions::default()` 的 `Confirm` 是结构默认值，无法区分"agents.toml 显式 Confirm"与"未配置"。为支持精确回退语义，增加 `default_permission_explicit: bool` 字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentToolPermissions {
    /// 未显式配置的 Tool 默认权限
    pub default_permission: ToolPermission,
    /// default_permission 是否由配置显式设置（true=agents.toml 写过 / 运行时改过；
    /// false=结构默认值 Confirm）。仅当 false 且 default==Confirm 时回退到 tool_default。
    #[serde(default)]
    pub default_permission_explicit: bool,
    /// 针对特定 Tool 的覆盖项
    pub overrides: HashMap<String, ToolPermission>,
}

impl Default for AgentToolPermissions {
    fn default() -> Self {
        Self {
            default_permission: ToolPermission::Confirm,
            default_permission_explicit: false,
            overrides: HashMap::new(),
        }
    }
}

impl From<super::AgentToolsConfig> for AgentToolPermissions {
    fn from(config: super::AgentToolsConfig) -> Self {
        let (default_permission, default_permission_explicit) = match config.default_permission {
            Some(p) => (p, true),
            None => (ToolPermission::Confirm, false),
        };
        Self {
            default_permission,
            default_permission_explicit,
            overrides: config.overrides,
        }
    }
}
```

#### 新增方法

```rust
impl Agent {
    /// 计算工具的生效权限，按三层回退：
    /// 1. agent.tool_permissions.overrides
    /// 2. agent.tool_permissions.default_permission
    ///    - 仅当 !default_permission_explicit && default == Confirm 时，回退到 (3)
    ///    - 显式 Allow / Deny / Confirm 直接使用
    /// 3. tool_def.default_permission（通过 registry 查询）
    ///
    /// 返回 (permission, source)。registry 缺失或工具未注册时
    /// 返回 (agent.default_permission, AgentDefault)。
    pub fn effective_permission(
        &self,
        tool_name: &str,
        registry: Option<&SpaceToolRegistry>,
    ) -> (ToolPermission, PermissionSource) {
        if let Some(p) = self.tool_permissions.overrides.get(tool_name).copied() {
            return (p, PermissionSource::AgentOverride);
        }
        let tool_default = registry
            .and_then(|r| r.get(tool_name))
            .map(|d| d.default_permission);
        let implicit_confirm = !self.tool_permissions.default_permission_explicit
            && self.tool_permissions.default_permission == ToolPermission::Confirm;
        match tool_default {
            Some(tp) if implicit_confirm => (tp, PermissionSource::ToolDefault),
            _ => (self.tool_permissions.default_permission, PermissionSource::AgentDefault),
        }
    }
}
```

#### 回退启发式

回退条件：**`!default_permission_explicit && default == Confirm`**——即 agents.toml 未显式配置 `[agent.tools].default_permission`。

覆盖场景：

- agents.toml 中未写 `[agent.tools]` 段的 agent（如 weather-specialist 等孵化 agent）——`default_permission_explicit=false`，回退到工具的 default_permission
- 显式 `default_permission = "Confirm"`（如 default-llm-agent / collector）——`explicit=true`，**不回退**，对所有未配置工具都要确认（保持配置语义）
- 显式 `default_permission = "Deny"`（如 brain/summarizer/evaluator）——`explicit=true`，保持 Deny 不回退
- 显式 `default_permission = "Allow"` ——`explicit=true`，保持 Allow 不回退

`registry: Option<&SpaceToolRegistry>` 为 `Option` 以兼容测试世界（async_dispatch 已有此模式）。

#### 删除腐化方法

- 删除 `Agent::get_permission`
- 删除 `Agent::has_permission`
- 保留 `Agent::grant_permission`（写入路径无腐化）

#### 调用点迁移

| 位置 | 旧调用 | 新调用 |
|------|--------|--------|
| [dispatch.rs:138](../../../src/systems/tools/dispatch.rs) | `agent.tool_permissions.get_permission(&tool_name)` | `agent.effective_permission(&tool_name, Some(&registry))` |
| [dispatch.rs:315](../../../src/systems/tools/dispatch.rs) | `parent.has_permission(&tool_name)` | `parent.effective_permission(&tool_name, Some(&registry)).0 == Allow` |
| [async_dispatch.rs:113](../../../src/systems/tools/async_dispatch.rs) | `agent.tool_permissions.get_permission(&request.tool_name)` | `agent.effective_permission(&request.tool_name, registry.as_deref())` |
| [maintenance.rs:342](../../../src/systems/maintenance.rs) | `parent_agent.tool_permissions.get_permission(tool)` | `parent_agent.effective_permission(tool, Some(&registry))` |
| [contracts/tools.rs:71](../../../src/contracts/tools.rs) | `agent.tool_permissions.get_permission(tool_name)` | `agent.effective_permission(tool_name, None)`（trait 默认实现保留；route 决策改用 effective） |
| [agent.rs:96](../../../src/domain/agent.rs) `has_permission` 测试 | `agent.has_permission("shell_exec")` | `agent.effective_permission("shell_exec", None).0 == Allow` |

`contracts/tools.rs` 的 `DefaultToolApprovalPolicy` 接收 `SpaceToolRegistry` 不可用（trait 签名无 registry 参数），保持 `effective_permission(tool, None)` 调用——此路径下回退到 AgentDefault。若后续需要 tool_default 回退，扩展 trait 签名。

### 子模块 2：子 Agent 权限继承（O2）

[maintenance.rs:337-392](../../../src/systems/maintenance.rs) 修改：

```rust
// 修复后：保留父的 effective_permission，子 Agent default=Deny
let parent_perms: Vec<(String, ToolPermission)> = request
    .tools
    .iter()
    .filter_map(|tool| {
        let (perm, _source) = parent_agent.effective_permission(tool, Some(&registry));
        if perm == ToolPermission::Deny {
            return None;
        }
        Some((tool.clone(), perm))
    })
    .collect();

if parent_perms.is_empty() && !request.tools.is_empty() {
    // spawn rejected: all_requested_tools_denied（保持现状）
}

let tool_permissions = AgentToolPermissions {
    default_permission: ToolPermission::Deny,
    overrides: parent_perms.into_iter().collect(),
};
```

#### 继承规则

| 父 effective_permission | 子 overrides |
|------------------------|--------------|
| Allow | Allow |
| Confirm | Confirm |
| Deny | 不出现在子 overrides |

子 Agent `default_permission = Deny` 不变（仅显式列举工具可用）。

`child_tools` 过滤逻辑 [maintenance.rs:426-430](../../../src/systems/maintenance.rs) 保持 `allowed_tools.contains` 不变——LLM tools 列表会包含 Confirm 工具（与现状一致，LLM 需知道这些工具存在）。

### 子模块 3：required_tag 启动校验（O7）

新增启动期扫描函数：

```rust
/// 启动期扫描 required_tag 孤儿：ToolDefinition 声明了 required_tag
/// 但当前已加载的 agent 中无任何持有该 tag，则 warn。
///
/// 仅 warn 不 fail——task-scoped agent 运行时可能补充 tag。
pub fn validate_required_tags(
    registry: &SpaceToolRegistry,
    agents: &[Agent],
) {
    use std::collections::HashSet;
    let all_tags: HashSet<&str> = agents
        .iter()
        .flat_map(|a| a.capabilities.tags.iter().map(|s| s.as_str()))
        .collect();

    for tool_def in registry.iter() {
        if let Some(required) = &tool_def.required_tag
            && !all_tags.contains(required.as_str())
        {
            warn!(
                event = "RequiredTagOrphan",
                tool_name = %tool_def.name,
                required_tag = %required,
                "no agent currently holds the required_tag; tool will be unusable until \
                 an agent with this tag is loaded (e.g., task-scoped agent at runtime)"
            );
        }
    }
}
```

#### 调用点

放在 `app` 初始化阶段（`register_builtin_tools` / `register_plugin_tools` 之后，agent 加载完成后）。具体位置在实现时定位——预期在 `src/app/` 或 `src/infrastructure/` 的 bootstrap 链路。

不实现编辑距离相近提示（YAGNI）。

### 子模块 4：权限审计事件（O11）

#### 新增事件变体

```rust
pub enum EngineEvent {
    // ... 现有变体
    PermissionAudit {
        target: EventTarget,
        agent_id: AgentId,
        agent_name: String,
        tool_name: String,
        decision: PermissionDecision,
        source: PermissionSource,
        context: PermissionAuditContext,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// 允许直接执行
    Allow,
    /// 需要确认（用户或父 Agent）
    Confirm,
    /// 拒绝
    Deny,
    /// 永久授权写入（grant_permission 调用）
    Granted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionAuditContext {
    /// 工具分发决策（tool_dispatch_system）
    Dispatch,
    /// 异步工具分流决策（async_tool_dispatch_system）
    AsyncDispatch,
    /// 用户确认结果（tool_confirmation_result_system）
    UserConfirmation,
    /// 父 Agent 审批结果（approval_result_system）
    ParentApproval,
    /// 子 Agent 派生（spawn_task_scoped_agent）
    SpawnInherit,
    /// required_tag 拒绝（dispatch.rs）
    TagDenied,
}
```

#### 发出点

| 位置 | context | decision |
|------|---------|----------|
| [dispatch.rs:138](../../../src/systems/tools/dispatch.rs) 决策后 | `Dispatch` | Allow / Confirm / Deny |
| [dispatch.rs:125](../../../src/systems/tools/dispatch.rs) required_tag 拒绝 | `TagDenied` | Deny |
| [async_dispatch.rs:113](../../../src/systems/tools/async_dispatch.rs) 分流决策 | `AsyncDispatch` | Allow / Confirm（Deny 由 sync 路径兜底） |
| [confirmation.rs:187-203](../../../src/systems/tools/confirmation.rs) Permanent grant | `UserConfirmation` | Granted |
| [approval.rs:208-220](../../../src/systems/tools/approval.rs) Permanent grant | `ParentApproval` | Granted |
| [maintenance.rs:411](../../../src/systems/maintenance.rs) spawn 后 | `SpawnInherit` | 每个继承工具的 perm |

#### 前端处理

与现有 `ToolCallStarted` 一致——遍历 `frontend_registry.frontends` 调 `push_event`。`target` 用任务的 `output_channel`（无 channel 时不推送，与 [dispatch.rs:176-199](../../../src/systems/tools/dispatch.rs) 同模式）。

#### 日志保留

现有 `tracing::debug/info` 日志保留（供日志聚合），事件用于前端订阅与 TUI 展示。两路并行，不互相替代。

## 测试

### 子模块 1 测试

- `effective_permission` 三层回退单元测试
  - overrides 命中 → AgentOverride
  - `!explicit && default == Confirm` + tool_default 存在 → ToolDefault
  - `explicit && default == Confirm` + tool_default 存在 → AgentDefault（不回退，保持配置语义）
  - `explicit && default == Allow` → AgentDefault
  - `explicit && default == Deny` → AgentDefault
  - `!explicit && default == Confirm` + registry 为 None → AgentDefault
- `AgentToolPermissions::default()` 的 `default_permission_explicit == false`
- `From<AgentToolsConfig>`：`config.default_permission = Some(X)` → `explicit=true`；`None` → `explicit=false`
- 删除 `get_permission` / `has_permission` 后，原调用点编译通过

### 子模块 2 测试

- 子 Agent 继承父 Allow → 子 Allow
- 子 Agent 继承父 Confirm → 子 Confirm（关键回归测试）
- 父 Deny → 不出现在子 overrides
- 全部 Deny → spawn rejected

### 子模块 3 测试

- 启动期扫描：所有 required_tag 都有 agent 持有 → 无 warn
- 启动期扫描：孤儿 required_tag → warn
- task-scoped agent 运行时补充 tag 不影响启动扫描（不 fail）

### 子模块 4 测试

- `EngineEvent::PermissionAudit` 序列化/反序列化
- dispatch 决策后发出事件（Allow / Confirm / Deny 三场景）
- Permanent grant 发出 `Granted` 事件
- spawn 后发出 `SpawnInherit` 事件

## 兼容性

- `get_permission` / `has_permission` 删除：内部 API，无外部消费者，破坏性可接受
- `AgentToolPermissions` 新增 `default_permission_explicit: bool` 字段，`#[serde(default)]` 保证反序列化向后兼容（旧 agents.toml 不写此字段时默认 false，与"未显式配置"语义一致）
- `effective_permission` 改变 agents.toml 未配置 `[agent.tools].default_permission` 的 agent 行为——这些 agent 此前对未配置工具返回 Confirm，现在回退到 tool_default。这是预期行为修正（让 ToolDefinition.default_permission 生效），需在文档中标注
- 显式 `default_permission = "Confirm"` 的 agent（default-llm-agent / collector）行为不变——`explicit=true` 不回退
- `EngineEvent` 新增变体：前端处理需同步更新，忽略未知变体的前端会跳过（向后兼容）

## 文档同步

- `docs/current-state.md`：权限决策链路更新为三层回退
- `docs/configuration.md`：`[agent.tools]` 段说明 default_permission 回退规则
- `agents.toml`：注释说明未配置 `[agent.tools]` 段时的回退行为

## 风险

1. **O2 行为变更**：子 Agent 此前对父 Confirm 工具直接 Allow，现在改为 Confirm。可能影响现有依赖"子 Agent 静默放行"的流程。需在 PR 描述中明确标注为 breaking change。

2. **`default_permission_explicit` 字段维护成本**：新增字段需要在 `From<AgentToolsConfig>`、运行时修改 `default_permission` 的路径（若存在）同步设置。当前 `grant_permission` 只写 overrides 不动 default，无额外维护点；若未来引入运行时改 default 的能力，需同步更新 explicit 标记。
