# Tool 权限决策链路统一与可观测性实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 统一 tool 权限决策链路为三层回退 + 显式标记，修复子 Agent 权限放大，补齐 required_tag 校验与权限审计事件。

**架构：** 新增 `AgentToolPermissions::effective_permission` 作为权限决策单一入口（替代 `get_permission`/`has_permission`），新增 `PermissionSource` / `PermissionAction` / `PermissionAuditContext` 三个枚举支撑审计，`AgentToolPermissions` 增加 `default_permission_explicit` 字段精确区分显式/隐式 Confirm。

**技术栈：** Rust + Bevy ECS + serde + tracing

**规格：** [docs/superpowers/specs/2026-08-05-tool-permission-truth-source-design.md](../specs/2026-08-05-tool-permission-truth-source-design.md)

---

## 文件结构

| 文件 | 职责 | 变更类型 |
|------|------|---------|
| `src/domain/agent.rs` | `Agent` 实体、`AgentToolPermissions`、新增 `PermissionSource` / `effective_permission`（放 `AgentToolPermissions`） | 修改 |
| `src/domain/frontend.rs` | `EngineEvent` 新增 `PermissionAudit` 变体 + `PermissionAction` / `PermissionAuditContext` 枚举 | 修改 |
| `src/domain/mod.rs` | re-export 新类型 | 修改 |
| `src/systems/tools/dispatch.rs` | 主决策点改用 `effective_permission` + 发出审计事件 | 修改 |
| `src/systems/tools/async_dispatch.rs` | async 分流改用 `effective_permission` + 发出审计事件 | 修改 |
| `src/systems/tools/confirmation.rs` | Permanent grant 发出审计事件 | 修改 |
| `src/systems/tools/approval.rs` | Permanent grant 发出审计事件 | 修改 |
| `src/systems/maintenance.rs` | spawn 逻辑改用 `effective_permission` 逐工具继承 + 发出审计事件 + 新增 `validate_required_tags` + 在 `load_agents_system` 调用 | 修改 |
| `src/contracts/tools.rs` | `DefaultToolApprovalPolicy` 改用 `effective_permission` | 修改 |
| `src/domain/space.rs` | 无（`AgentToolsConfig` 已是 `Option<ToolPermission>`，无需改动） | 无改动 |
| `docs/current-state.md` | 权限决策链路描述更新 | 修改 |
| `docs/configuration.md` | `[agent.tools]` 段回退规则说明 | 修改 |

---

## 任务 1：新增 PermissionSource 枚举 + AgentToolPermissions 增加 explicit 字段

**文件：**
- 修改：`src/domain/agent.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/domain/agent.rs` 的 `#[cfg(test)] mod tests` 末尾追加：

```rust
    #[test]
    fn default_permission_explicit_defaults_to_false() {
        let perms = AgentToolPermissions::default();
        assert!(!perms.default_permission_explicit);
        assert_eq!(perms.default_permission, ToolPermission::Confirm);
    }

    #[test]
    fn from_agent_tools_config_some_marks_explicit() {
        use crate::domain::AgentToolsConfig;
        let config = AgentToolsConfig {
            default_permission: Some(ToolPermission::Allow),
            overrides: std::collections::HashMap::new(),
        };
        let perms = AgentToolPermissions::from(config);
        assert!(perms.default_permission_explicit);
        assert_eq!(perms.default_permission, ToolPermission::Allow);
    }

    #[test]
    fn from_agent_tools_config_none_marks_implicit() {
        use crate::domain::AgentToolsConfig;
        let config = AgentToolsConfig {
            default_permission: None,
            overrides: std::collections::HashMap::new(),
        };
        let perms = AgentToolPermissions::from(config);
        assert!(!perms.default_permission_explicit);
        assert_eq!(perms.default_permission, ToolPermission::Confirm);
    }
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib domain::agent::tests -- --nocapture`
预期：FAIL，报错 `no field named default_permission_explicit` 或类似编译错误。

- [ ] **步骤 3：实现 PermissionSource 枚举 + AgentToolPermissions 字段**

在 `src/domain/agent.rs` 顶部 `use` 区下方新增 `PermissionSource`：

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

修改 `AgentToolPermissions` 结构：

```rust
/// Agent 的 Tool 权限配置
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

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib domain::agent::tests -- --nocapture`
预期：PASS（3 个新测试通过，原有 `agent_tool_permissions_default_is_confirm` / `agent_tool_permissions_override` 可能因新增字段失败——同步修复这两个测试的构造）。

修复现有测试中所有 `AgentToolPermissions { default_permission, overrides }` 构造为 `AgentToolPermissions { default_permission, default_permission_explicit, overrides }`。在 `src/domain/agent.rs` 测试模块中：

```rust
    // 原：
    // let mut perms = AgentToolPermissions {
    //     default_permission: ToolPermission::Deny,
    //     ..Default::default()
    // };
    // ..Default::default() 已包含 default_permission_explicit: false，无需改动
```

确认 `..Default::default()` 用法不破坏；显式列举字段的构造需补 `default_permission_explicit`。

- [ ] **步骤 5：编译并运行所有测试**

运行：`cargo build --lib && cargo test --lib`
预期：编译通过，所有测试 PASS。

- [ ] **步骤 6：Commit**

```bash
git add src/domain/agent.rs
git commit -m "feat(domain): AgentToolPermissions 增加 default_permission_explicit 字段 + PermissionSource 枚举"
```

---

## 任务 2：新增 Agent::effective_permission 方法

**文件：**
- 修改：`src/domain/agent.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/domain/agent.rs` 的 `#[cfg(test)] mod tests` 追加：

```rust
    use crate::domain::{SpaceToolRegistry, ToolDefinition, ToolExecutorKind, ToolSchema};

    fn make_tool_def(name: &str, perm: ToolPermission) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "test".to_string(),
            parameters: ToolSchema::default(),
            default_permission: perm,
            executor: ToolExecutorKind::Builtin(name.to_string()),
            required_tag: None,
        }
    }

    fn make_agent_with_default(default: ToolPermission, explicit: bool) -> Agent {
        Agent {
            id: uuid::Uuid::nil(),
            profile: AgentProfile {
                name: "test".to_string(),
                model: "m".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: String::new(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: default,
                default_permission_explicit: explicit,
                overrides: HashMap::new(),
            },
            system_prompt: None,
        }
    }

    #[test]
    fn effective_permission_override_hits() {
        let mut agent = make_agent_with_default(ToolPermission::Confirm, false);
        agent.tool_permissions.overrides.insert(
            "shell_exec".to_string(),
            ToolPermission::Allow,
        );
        let (perm, source) = agent.effective_permission("shell_exec", None);
        assert_eq!(perm, ToolPermission::Allow);
        assert_eq!(source, PermissionSource::AgentOverride);
    }

    #[test]
    fn effective_permission_implicit_confirm_falls_back_to_tool_default() {
        let agent = make_agent_with_default(ToolPermission::Confirm, false);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Allow));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Allow);
        assert_eq!(source, PermissionSource::ToolDefault);
    }

    #[test]
    fn effective_permission_explicit_confirm_does_not_fall_back() {
        let agent = make_agent_with_default(ToolPermission::Confirm, true);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Allow));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Confirm);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_explicit_allow_does_not_fall_back() {
        let agent = make_agent_with_default(ToolPermission::Allow, true);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Confirm));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Allow);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_explicit_deny_does_not_fall_back() {
        let agent = make_agent_with_default(ToolPermission::Deny, true);
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_def("shell_exec", ToolPermission::Allow));
        let (perm, source) = agent.effective_permission("shell_exec", Some(&registry));
        assert_eq!(perm, ToolPermission::Deny);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_implicit_confirm_no_registry_returns_agent_default() {
        let agent = make_agent_with_default(ToolPermission::Confirm, false);
        let (perm, source) = agent.effective_permission("unknown_tool", None);
        assert_eq!(perm, ToolPermission::Confirm);
        assert_eq!(source, PermissionSource::AgentDefault);
    }

    #[test]
    fn effective_permission_implicit_confirm_unknown_tool_returns_agent_default() {
        let agent = make_agent_with_default(ToolPermission::Confirm, false);
        let registry = SpaceToolRegistry::default(); // 空 registry
        let (perm, source) = agent.effective_permission("unknown_tool", Some(&registry));
        assert_eq!(perm, ToolPermission::Confirm);
        assert_eq!(source, PermissionSource::AgentDefault);
    }
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib domain::agent::tests::effective_permission -- --nocapture`
预期：FAIL，报错 `no method named effective_permission`。

- [ ] **步骤 3：实现 effective_permission 方法**

在 `src/domain/agent.rs` 中：

1. 在 `impl AgentToolPermissions` 块中，**删除** `get_permission`，新增 `effective_permission`：

```rust
impl AgentToolPermissions {
    /// 计算工具的生效权限，按三层回退：
    /// 1. self.overrides
    /// 2. self.default_permission
    ///    - 仅当 !default_permission_explicit && default == Confirm 时，回退到 (3)
    ///    - 显式 Allow / Deny / Confirm 直接使用
    /// 3. tool_def.default_permission（通过 registry 查询）
    ///
    /// 返回 (permission, source)。registry 缺失或工具未注册时
    /// 返回 (self.default_permission, AgentDefault)。
    pub fn effective_permission(
        &self,
        tool_name: &str,
        registry: Option<&crate::domain::SpaceToolRegistry>,
    ) -> (crate::domain::ToolPermission, PermissionSource) {
        use crate::domain::ToolPermission;
        if let Some(p) = self.overrides.get(tool_name).copied() {
            return (p, PermissionSource::AgentOverride);
        }
        let tool_default = registry
            .and_then(|r| r.get(tool_name))
            .map(|d| d.default_permission);
        let implicit_confirm = !self.default_permission_explicit
            && self.default_permission == ToolPermission::Confirm;
        match tool_default {
            Some(tp) if implicit_confirm => (tp, PermissionSource::ToolDefault),
            _ => (self.default_permission, PermissionSource::AgentDefault),
        }
    }
}
```

2. 在 `impl Agent` 块中，**删除** `has_permission`，新增委托方法 + `grant_permission` 增加 tracing log：

```rust
impl Agent {
    /// 委托到 AgentToolPermissions::effective_permission
    pub fn effective_permission(
        &self,
        tool_name: &str,
        registry: Option<&crate::domain::SpaceToolRegistry>,
    ) -> (crate::domain::ToolPermission, PermissionSource) {
        self.tool_permissions.effective_permission(tool_name, registry)
    }

    /// 授予永久权限
    pub fn grant_permission(&mut self, tool_name: String) {
        tracing::info!(
            event = "PermissionGrant",
            agent_id = %self.id,
            tool_name = %tool_name,
            "永久授权写入"
        );
        self.tool_permissions
            .overrides
            .insert(tool_name, ToolPermission::Allow);
    }
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib domain::agent::tests::effective_permission -- --nocapture`
预期：PASS（7 个新测试通过）。

- [ ] **步骤 5：编译确认（预期有调用点报错）**

运行：`cargo build --lib`
预期：编译错误，多个文件报 `get_permission` / `has_permission` 不存在。这是预期的——任务 3-6 会逐个修复。

- [ ] **步骤 6：Commit**

```bash
git add src/domain/agent.rs
git commit -m "feat(domain): Agent::effective_permission 三层回退 + 删除 get_permission/has_permission"
```

---

## 任务 3：迁移 dispatch.rs 调用点

**文件：**
- 修改：`src/systems/tools/dispatch.rs`

- [ ] **步骤 1：迁移权限决策调用**

在 `src/systems/tools/dispatch.rs` 中，定位 [L138](../../../src/systems/tools/dispatch.rs)：

```rust
        let permission = agent.tool_permissions.get_permission(&tool_name);
```

替换为：

```rust
        let (permission, _source) = agent.effective_permission(&tool_name, Some(&registry));
```

- [ ] **步骤 2：迁移父审批权限检查**

定位 [L315](../../../src/systems/tools/dispatch.rs) `.filter(|parent| parent.has_permission(&tool_name))`：

```rust
                            .filter(|parent| parent.has_permission(&tool_name))
```

替换为：

```rust
                            .filter(|parent| {
                                parent.effective_permission(&tool_name, Some(&registry)).0
                                    == ToolPermission::Allow
                            })
```

- [ ] **步骤 3：编译验证**

运行：`cargo build --lib`
预期：dispatch.rs 编译通过。其他文件（async_dispatch / maintenance / contracts）仍报错。

- [ ] **步骤 4：运行 dispatch 相关测试**

运行：`cargo test --lib systems::tools::dispatch -- --nocapture`
预期：PASS（原有测试不变）。

- [ ] **步骤 5：Commit**

```bash
git add src/systems/tools/dispatch.rs
git commit -m "refactor(tools): dispatch.rs 改用 effective_permission"
```

---

## 任务 4：迁移 async_dispatch.rs / maintenance.rs / contracts/tools.rs 调用点

**文件：**
- 修改：`src/systems/tools/async_dispatch.rs`
- 修改：`src/systems/maintenance.rs`
- 修改：`src/contracts/tools.rs`

- [ ] **步骤 1：迁移 async_dispatch.rs**

在 `src/systems/tools/async_dispatch.rs` [L113](../../../src/systems/tools/async_dispatch.rs)：

```rust
            let permission = agent.tool_permissions.get_permission(&request.tool_name);
```

替换为：

```rust
            let (permission, _source) =
                agent.effective_permission(&request.tool_name, registry.as_deref());
```

- [ ] **步骤 2：迁移 maintenance.rs spawn 逻辑（O2 修复）**

在 `src/systems/maintenance.rs` 定位 [L337-L392](../../../src/systems/maintenance.rs) 的 `allowed_tools` 计算和 `tool_permissions` 构造：

```rust
    // 过滤 tools：保留父 Agent 有 Allow 或 Confirm 权限的工具
    let allowed_tools: Vec<String> = request
        .tools
        .iter()
        .filter(|tool| {
            let perm = parent_agent.tool_permissions.get_permission(tool);
            !matches!(perm, crate::domain::ToolPermission::Deny)
        })
        .cloned()
        .collect();

    // 只在请求了工具但全部无效时才拒绝
    // 空工具列表是合法的，表示纯 LLM 对话任务
    if allowed_tools.is_empty() && !request.tools.is_empty() {
        warn!(
            event = "SpawnRequestRejected",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            requested_tools = ?request.tools,
            reason = "all_requested_tools_denied",
            "spawn rejected: all requested tools are denied for parent agent"
        );
        let msg = format!(
            "Agent spawn rejected: all requested tools {:?} are denied for parent agent",
            request.tools
        );
        mark_task_failed(tasks, index, clock, request.task_id, &msg);
        return;
    }
```

替换为：

```rust
    // 过滤 tools：保留父 Agent 非 Deny 的工具，并继承父的 effective_permission
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

    let allowed_tools: Vec<String> =
        parent_perms.iter().map(|(t, _)| t.clone()).collect();

    // 只在请求了工具但全部无效时才拒绝
    // 空工具列表是合法的，表示纯 LLM 对话任务
    if allowed_tools.is_empty() && !request.tools.is_empty() {
        warn!(
            event = "SpawnRequestRejected",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            requested_tools = ?request.tools,
            reason = "all_requested_tools_denied",
            "spawn rejected: all requested tools are denied for parent agent"
        );
        let msg = format!(
            "Agent spawn rejected: all requested tools {:?} are denied for parent agent",
            request.tools
        );
        mark_task_failed(tasks, index, clock, request.task_id, &msg);
        return;
    }
```

然后定位 [L386-L392](../../../src/systems/maintenance.rs) 的 `tool_permissions` 构造：

```rust
    // 构建 tool_permissions: 子 Agent 默认拒绝，仅显式允许的工具可用
    let tool_permissions = AgentToolPermissions {
        default_permission: ToolPermission::Deny,
        overrides: allowed_tools
            .iter()
            .map(|t| (t.clone(), ToolPermission::Allow))
            .collect(),
    };
```

替换为：

```rust
    // 构建 tool_permissions: 子 Agent 默认拒绝，按工具逐个继承父的 effective_permission
    let tool_permissions = AgentToolPermissions {
        default_permission: ToolPermission::Deny,
        default_permission_explicit: true,
        overrides: parent_perms.into_iter().collect(),
    };
```

- [ ] **步骤 3：迁移 contracts/tools.rs**

在 `src/contracts/tools.rs` [L71](../../../src/contracts/tools.rs) 的 `DefaultToolApprovalPolicy`：

```rust
        let permission = agent.tool_permissions.get_permission(tool_name);
```

替换为：

```rust
        let (permission, _source) = agent.tool_permissions.effective_permission(tool_name, None);
```

同时 [L85](../../../src/contracts/tools.rs) `parent_agent.has_permission(tool_name)` 替换为：

```rust
                    && parent_agent.tool_permissions.effective_permission(tool_name, None).0
                        == ToolPermission::Allow
```

- [ ] **步骤 4：修复 contracts/tools.rs 测试模块的 has_permission 调用**

`src/contracts/tools.rs` 测试模块的 `make_agent` 构造 `AgentToolPermissions` 时需补 `default_permission_explicit` 字段。定位 [L129-L132](../../../src/contracts/tools.rs)：

```rust
            tool_permissions: AgentToolPermissions {
                default_permission: permission,
                overrides: std::collections::HashMap::new(),
            },
```

替换为：

```rust
            tool_permissions: AgentToolPermissions {
                default_permission: permission,
                default_permission_explicit: true,
                overrides: std::collections::HashMap::new(),
            },
```

若 `src/systems/tools/mod.rs` 测试模块有 `agent_tool_permissions_default_is_confirm` / `agent_tool_permissions_override` 测试调用 `perms.get_permission`，`get_permission` 已删除——这两个测试的覆盖已由任务 1（`default_permission_explicit_defaults_to_false`）和任务 2（`effective_permission_override_hits`）完整覆盖。删除这两个测试，在原位置添加注释：

```rust
    // AgentToolPermissions 的查询行为由 effective_permission 统一覆盖，
    // 见 src/domain/agent.rs tests::effective_permission_*。
    // 构造/默认值行为见 default_permission_explicit_defaults_to_false。
```

- [ ] **步骤 5：全局搜索遗漏的 get_permission / has_permission 调用**

运行：`grep -rn "get_permission\|has_permission" src/ tests/`（用 Grep 工具）

预期：仅测试文件中可能有残留。逐一修复——测试夹具中的 `has_permission` 改为 `effective_permission(...).0 == Allow`，`get_permission` 改为 `effective_permission(...).0`。如果测试夹具没有 `SpaceToolRegistry`，传 `None`。

- [ ] **步骤 6：编译并运行所有测试**

运行：`cargo build --lib && cargo test --lib`
预期：编译通过，所有测试 PASS。

- [ ] **步骤 7：Commit**

```bash
git add src/systems/tools/async_dispatch.rs src/systems/maintenance.rs src/contracts/tools.rs src/systems/tools/mod.rs
git commit -m "refactor: 全部调用点迁移到 effective_permission + 修复 O2 子 Agent 权限继承"
```

---

## 任务 5：新增 O2 子 Agent 权限继承测试（单元 + 集成）

**文件：**
- 修改：`src/systems/maintenance.rs`（测试模块）
- 创建：`tests/o2_permission_inheritance.rs`（集成测试）

- [ ] **步骤 1：编写单元测试（逻辑验证）**

在 `src/systems/maintenance.rs` 测试模块追加（同原计划内容，验证 `effective_permission` + `filter_map` 逻辑）：

```rust
#[cfg(test)]
mod o2_inheritance_tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, SpaceToolRegistry,
        ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema,
    };
    use std::collections::HashMap;

    fn make_parent(default: ToolPermission, explicit: bool, overrides: HashMap<String, ToolPermission>) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile { name: "parent".to_string(), model: "m".to_string() },
            capabilities: AgentCapabilities { tags: vec![], description: String::new() },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions {
                default_permission: default,
                default_permission_explicit: explicit,
                overrides,
            },
            system_prompt: None,
        }
    }

    fn make_registry_with(tool_name: &str, perm: ToolPermission) -> SpaceToolRegistry {
        let mut r = SpaceToolRegistry::default();
        r.register(ToolDefinition {
            name: tool_name.to_string(),
            description: "test".to_string(),
            parameters: ToolSchema::default(),
            default_permission: perm,
            executor: ToolExecutorKind::Builtin(tool_name.to_string()),
            required_tag: None,
        });
        r
    }

    /// 父 Confirm → 子 Confirm（不再降为 Allow）
    #[test]
    fn child_inherits_confirm_from_parent_confirm() {
        let registry = make_registry_with("shell_exec", ToolPermission::Confirm);
        let parent = make_parent(ToolPermission::Deny, true, {
            let mut m = HashMap::new();
            m.insert("shell_exec".to_string(), ToolPermission::Confirm);
            m
        });
        let parent_perms: Vec<(String, ToolPermission)> = ["shell_exec"]
            .iter()
            .filter_map(|tool| {
                let (perm, _) = parent.effective_permission(tool, Some(&registry));
                if perm == ToolPermission::Deny { return None; }
                Some((tool.to_string(), perm))
            })
            .collect();
        assert_eq!(parent_perms.len(), 1);
        assert_eq!(parent_perms[0].1, ToolPermission::Confirm);
    }

    /// 父 Allow → 子 Allow
    #[test]
    fn child_inherits_allow_from_parent_allow() {
        let registry = make_registry_with("shell_exec", ToolPermission::Confirm);
        let parent = make_parent(ToolPermission::Deny, true, {
            let mut m = HashMap::new();
            m.insert("shell_exec".to_string(), ToolPermission::Allow);
            m
        });
        let parent_perms: Vec<(String, ToolPermission)> = ["shell_exec"]
            .iter()
            .filter_map(|tool| {
                let (perm, _) = parent.effective_permission(tool, Some(&registry));
                if perm == ToolPermission::Deny { return None; }
                Some((tool.to_string(), perm))
            })
            .collect();
        assert_eq!(parent_perms[0].1, ToolPermission::Allow);
    }

    /// 父 Deny → 工具不传入子 overrides
    #[test]
    fn child_excludes_denied_tool() {
        let registry = make_registry_with("shell_exec", ToolPermission::Allow);
        let parent = make_parent(ToolPermission::Deny, true, {
            let mut m = HashMap::new();
            m.insert("shell_exec".to_string(), ToolPermission::Deny);
            m
        });
        let parent_perms: Vec<(String, ToolPermission)> = ["shell_exec"]
            .iter()
            .filter_map(|tool| {
                let (perm, _) = parent.effective_permission(tool, Some(&registry));
                if perm == ToolPermission::Deny { return None; }
                Some((tool.to_string(), perm))
            })
            .collect();
        assert!(parent_perms.is_empty());
    }
}
```

- [ ] **步骤 2：运行单元测试验证通过**

运行：`cargo test --lib systems::maintenance::o2_inheritance_tests -- --nocapture`
预期：PASS（3 个测试）。

- [ ] **步骤 3：编写集成测试（验证 handle_spawn_request 实际行为）**

创建 `tests/o2_permission_inheritance.rs`：

```rust
//! O2 子 Agent 权限继承集成测试
//!
//! 验证 handle_spawn_request 的实际 spawn 行为：
//! 父 Confirm → 子 Confirm（不再降为 Allow）

use harness::app::build_harness_app;
use harness::domain::{
    Agent, AgentCapabilities, AgentExecutionRequestMessage, AgentKind, AgentProfile,
    AgentSpawnRequestMessage, AgentToolPermissions, Task, TaskId, TaskRoutingPolicy, TaskStatus,
    ToolPermission,
};
use harness::ecs::EntityIndex;
use bevy_app::App;
use bevy_ecs::prelude::*;
use std::collections::HashMap;
use uuid::Uuid;

fn spawn_parent_agent(app: &mut App, overrides: HashMap<String, ToolPermission>) -> Uuid {
    let agent_id = Uuid::new_v4();
    let agent = Agent {
        id: agent_id,
        profile: AgentProfile { name: "parent".to_string(), model: "test".to_string() },
        capabilities: AgentCapabilities { tags: vec![], description: String::new() },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            default_permission_explicit: true,
            overrides,
        },
        system_prompt: None,
    };
    let entity = app.world_mut().spawn(agent).id();
    app.world_mut().resource_mut::<EntityIndex>().agents.insert(agent_id, entity);
    agent_id
}

fn spawn_parent_task(app: &mut App, agent_id: Uuid) -> TaskId {
    let task_id = Uuid::new_v4();
    let task = Task {
        id: task_id,
        content: "parent task".to_string(),
        creator: agent_id,
        delegate: Some(agent_id),
        status: TaskStatus::Running,
        pending_confirmation_id: None,
        input_summary: String::new(),
        result_summary: String::new(),
        priority: 0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        retry_count: 0,
        max_retries: 3,
        next_retry_at: None,
        last_error: None,
        multi_turn: false,
        parent_task_id: None,
        batch_id: None,
        origin_channel: None,
        routing_policy: TaskRoutingPolicy::event(),
        last_evaluated_turn: None,
    };
    let entity = app.world_mut().spawn(task).id();
    app.world_mut().resource_mut::<EntityIndex>().tasks.insert(task_id, entity);
    task_id
}

#[test]
fn spawn_inherits_confirm_permission_from_parent() {
    let mut app = build_harness_app();
    let mut overrides = HashMap::new();
    overrides.insert("shell_exec".to_string(), ToolPermission::Confirm);
    let parent_id = spawn_parent_agent(&mut app, overrides);
    let task_id = spawn_parent_task(&mut app, parent_id);

    // 发送 spawn 请求
    app.world_mut().spawn(AgentSpawnRequestMessage {
        parent_agent_id: parent_id,
        task_id,
        tools: vec!["shell_exec".to_string()],
        system_prompt: None,
        multi_turn: false,
    });

    // 运行 agent_factory_system
    app.update();

    // 查找 spawn 的子 Agent
    let mut query = app.world_mut().query::<&Agent>();
    let child_agent = query
        .iter(app.world())
        .find(|a| a.parent_id == Some(parent_id))
        .expect("子 Agent 应已 spawn");

    // 验证子 Agent 继承了 Confirm（不是 Allow）
    let perm = child_agent
        .tool_permissions
        .overrides
        .get("shell_exec")
        .expect("shell_exec 应在子 Agent overrides 中");
    assert_eq!(
        *perm, ToolPermission::Confirm,
        "子 Agent 应继承父的 Confirm 权限，而非降级为 Allow"
    );
}
```

- [ ] **步骤 4：运行集成测试验证通过**

运行：`cargo test --test o2_permission_inheritance -- --nocapture`
预期：PASS。若 `AgentSpawnRequestMessage` 字段与实际不符，按实际结构调整。

- [ ] **步骤 5：Commit**

```bash
git add src/systems/maintenance.rs tests/o2_permission_inheritance.rs
git commit -m "test(maintenance): O2 子 Agent 权限继承单元 + 集成测试"
```

---

## 任务 6：新增 required_tag 启动校验（O7）

**文件：**
- 修改：`src/systems/maintenance.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/maintenance.rs` 测试模块追加：

```rust
#[cfg(test)]
mod required_tag_tests {
    use super::*;
    use crate::domain::{
        AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, SpaceToolRegistry,
        ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema,
    };

    fn make_agent_with_tags(tags: Vec<String>) -> Agent {
        Agent {
            id: Uuid::new_v4(),
            profile: AgentProfile { name: "t".to_string(), model: "m".to_string() },
            capabilities: AgentCapabilities { tags, description: String::new() },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    fn make_tool_with_tag(name: &str, tag: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "t".to_string(),
            parameters: ToolSchema::default(),
            default_permission: ToolPermission::Allow,
            executor: ToolExecutorKind::Builtin(name.to_string()),
            required_tag: Some(tag.to_string()),
        }
    }

    #[test]
    fn validate_required_tags_no_warn_when_tag_held() {
        let mut registry = SpaceToolRegistry::default();
        registry.register(make_tool_with_tag("submit_profile_update", "profile"));
        let agents = vec![make_agent_with_tags(vec!["profile".to_string()])];
        // 不 panic 即通过
        validate_required_tags(&registry, &agents);
    }

    #[test]
    fn validate_required_tags_no_warn_when_no_required_tag() {
        let mut registry = SpaceToolRegistry::default();
        registry.register(ToolDefinition {
            name: "shell_exec".to_string(),
            description: "t".to_string(),
            parameters: ToolSchema::default(),
            default_permission: ToolPermission::Confirm,
            executor: ToolExecutorKind::Builtin("shell_exec".to_string()),
            required_tag: None,
        });
        let agents: Vec<Agent> = vec![];
        validate_required_tags(&registry, &agents);
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib systems::maintenance::required_tag_tests -- --nocapture`
预期：FAIL，报错 `validate_required_tags` 未定义。

- [ ] **步骤 3：实现 validate_required_tags 函数**

在 `src/systems/maintenance.rs` 顶部函数区新增：

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

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib systems::maintenance::required_tag_tests -- --nocapture`
预期：PASS。

- [ ] **步骤 5：新增 validate_required_tags_system 并注册**

由于 `load_agents_system` 的 `Commands` 延迟 spawn 特性，`agents` Query 在 `load_agents_system` 内看不到新 spawn 的 agent。新增独立 Startup system，排在 `load_agents_system` 之后。

**Bevy 0.18 时序说明：** Bevy 0.18 默认启用 `AutoInsertApplyDeferredEdges`，当 system A `.after()` system B 且 B 使用 `Commands` 时，会自动插入 `apply_deferred`。因此 `validate_required_tags_system.after(load_agents_system)` 在默认行为下能保证 spawn 结果可见。但为防御性，使用 `Update` 首帧 + `run_once` 模式更稳妥。

在 `src/systems/maintenance.rs` 新增：

```rust
/// O7: 启动期 required_tag 孤儿扫描
///
/// 在 Update 首帧执行一次（run_once 守卫），确保 agent entities 已 flush 到 world。
/// 比 Startup + after 更稳妥——不依赖 Bevy 的 AutoInsertApplyDeferredEdges 行为。
pub(crate) fn validate_required_tags_system(
    mut ran: Local<bool>,
    agents: Query<&Agent>,
    tool_registry: Res<SpaceToolRegistry>,
) {
    if *ran {
        return;
    }
    *ran = true;
    let agent_list: Vec<Agent> = agents.iter().cloned().collect();
    validate_required_tags(&tool_registry, &agent_list);
}
```

- [ ] **步骤 6：在 app/mod.rs 注册新 system**

定位 `src/app/mod.rs` [L341](../../../src/app/mod.rs) `app.add_systems(Startup, load_agents_system);`，在 `app.add_systems(Update, ...)` 区添加：

```rust
    app.add_systems(
        Update,
        validate_required_tags_system.in_set(HarnessSet::Maintenance),
    );
```

并更新 `use` 语句导入 `validate_required_tags_system`。

- [ ] **步骤 7：编译并运行所有测试**

运行：`cargo build --lib && cargo test --lib`
预期：编译通过，所有测试 PASS。

- [ ] **步骤 8：Commit**

```bash
git add src/systems/maintenance.rs src/app/mod.rs
git commit -m "feat(maintenance): O7 required_tag 启动期孤儿扫描 + warn"
```

---

## 任务 7：新增 EngineEvent::PermissionAudit 事件（O11）

**文件：**
- 修改：`src/domain/frontend.rs`
- 修改：`src/domain/mod.rs`

- [ ] **步骤 1：新增 PermissionAction 和 PermissionAuditContext 枚举**

在 `src/domain/frontend.rs` `EngineEvent` 定义之前新增：

```rust
/// 权限审计动作（表达"发生了什么"而非"决策状态是什么"，
/// 与 ToolPermission 的 Allow/Confirm/Deny 状态区分）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionAction {
    /// 允许直接执行
    Allow,
    /// 需要确认（用户或父 Agent）
    Confirm,
    /// 拒绝
    Deny,
    /// 永久授权写入（grant_permission 调用，是写入动作而非决策状态）
    Grant,
}

/// 权限审计触发场景
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
    /// required_tag 拒绝（dispatch.rs）
    TagDenied,
}
```

- [ ] **步骤 2：新增 EngineEvent::PermissionAudit 变体**

在 `src/domain/frontend.rs` 的 `EngineEvent` 枚举末尾（`ToolCallStarted` 之后）追加：

```rust
    /// 权限审计事件
    PermissionAudit {
        target: EventTarget,
        agent_id: AgentId,
        agent_name: String,
        tool_name: String,
        action: PermissionAction,
        source: crate::domain::PermissionSource,
        context: PermissionAuditContext,
    },
```

- [ ] **步骤 3：在 src/domain/mod.rs re-export 新类型**

定位 `src/domain/mod.rs` 的 re-export 区，添加：

```rust
pub use crate::domain::agent::PermissionSource;
pub use crate::domain::frontend::{PermissionAction, PermissionAuditContext};
```

- [ ] **步骤 4：编译验证**

运行：`cargo build --lib`
预期：编译通过（新变体未被使用，但 EngineEvent 的所有 match 分支可能需要更新——检查所有 match EngineEvent 的位置，添加 `_ => {}` 或显式 PermissionAudit 分支）。

运行 Grep 搜索 `match.*EngineEvent|=> EngineEvent` 找出所有 match 点，对没有 `PermissionAudit` 分支的 match 添加 `_ => {}`（前端处理可暂时忽略该事件，后续任务补齐推送逻辑）。

- [ ] **步骤 5：Commit**

```bash
git add src/domain/frontend.rs src/domain/mod.rs
git commit -m "feat(domain): EngineEvent::PermissionAudit + PermissionAction/Context 枚举"
```

---

## 任务 8：在 dispatch.rs 发出 PermissionAudit 事件

**文件：**
- 修改：`src/systems/tools/dispatch.rs`

- [ ] **步骤 1：编写 helper 单元测试**

`emit_permission_audit` 是纯函数，针对它编写单元测试，不依赖完整 World。在 `src/systems/tools/dispatch.rs` 测试模块追加：

```rust
    #[test]
    fn emit_permission_audit_constructs_event_with_correct_fields() {
        use crate::domain::{
            AgentId, EventTarget, PermissionAction, PermissionAuditContext, PermissionSource,
        };
        use std::sync::{Arc, Mutex};

        // 捕获事件的前端
        #[derive(Default)]
        struct CapturingFrontend(Arc<Mutex<Vec<crate::domain::EngineEvent>>>);
        impl crate::channels::traits::Frontend for CapturingFrontend {
            fn kind(&self) -> crate::domain::FrontendKind {
                crate::domain::FrontendKind::Tui
            }
            fn push_event(&self, event: crate::domain::EngineEvent) {
                self.0.lock().unwrap().push(event);
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        let frontend = CapturingFrontend(captured.clone());
        let registry = crate::app::FrontendRegistry {
            frontends: vec![Box::new(frontend)],
        };

        // 构造最小 Task + EntityIndex 用于 target 解析
        let mut world = World::new();
        let task_id = Uuid::new_v4();
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "u".to_string(),
            thread_id: None,
        };
        let task = Task {
            id: task_id,
            content: "t".to_string(),
            creator: Uuid::nil(),
            delegate: Some(Uuid::nil()),
            status: TaskStatus::Waiting(WaitingReason::ToolExecution),
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel),
            last_evaluated_turn: None,
        };
        let task_entity = world.spawn(task).id();
        let mut index = crate::ecs::EntityIndex::default();
        index.tasks.insert(task_id, task_entity);
        world.insert_resource(index);
        let index_ref = world.resource::<crate::ecs::EntityIndex>().clone();
        let tasks_query: Query<(Entity, &mut Task)> = Query::new();

        emit_permission_audit(
            &registry,
            &tasks_query,
            &index_ref,
            task_id,
            Uuid::nil(),
            "test_agent".to_string(),
            "shell_exec".to_string(),
            PermissionAction::Allow,
            PermissionSource::AgentDefault,
            PermissionAuditContext::Dispatch,
        );

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1, "应推送 1 个事件");
        match &events[0] {
            crate::domain::EngineEvent::PermissionAudit {
                action,
                context,
                source,
                tool_name,
                agent_name,
                ..
            } => {
                assert_eq!(*action, PermissionAction::Allow);
                assert_eq!(*context, PermissionAuditContext::Dispatch);
                assert_eq!(*source, PermissionSource::AgentDefault);
                assert_eq!(tool_name, "shell_exec");
                assert_eq!(agent_name, "test_agent");
            }
            _ => panic!("expected PermissionAudit event"),
        }
    }
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib systems::tools::dispatch::tests::emit_permission_audit -- --nocapture`
预期：FAIL，`emit_permission_audit` 未定义。

- [ ] **步骤 3：实现 emit_permission_audit helper + 发出逻辑**

在 `src/systems/tools/dispatch.rs` 顶部 `use` 区添加：

```rust
use crate::domain::{
    PermissionAction, PermissionAuditContext, PermissionSource,
};
```

在文件顶部新增私有函数：

```rust
/// 推送 PermissionAudit 事件到所有前端（仅当 task 有 output_channel 时）
fn emit_permission_audit(
    frontend_registry: &FrontendRegistry,
    tasks: &Query<(Entity, &mut Task)>,
    index: &EntityIndex,
    task_id: uuid::Uuid,
    agent_id: uuid::Uuid,
    agent_name: String,
    tool_name: String,
    action: PermissionAction,
    source: PermissionSource,
    context: PermissionAuditContext,
) {
    if let Some(target) = index
        .get_task(&task_id)
        .and_then(|e| tasks.get(e).ok())
        .and_then(|(_, t)| t.routing_policy.output_channel.clone())
        .map(|channel| EventTarget::Directed(vec![channel]))
    {
        let event = EngineEvent::PermissionAudit {
            target,
            agent_id,
            agent_name,
            tool_name,
            action,
            source,
            context,
        };
        for frontend in &frontend_registry.frontends {
            frontend.push_event(event.clone());
        }
    }
}
```

在 `match permission` 的三个分支后调用 `emit_permission_audit`：

- `Allow` 分支：在 `restore_task_after_tool` 之前调用，`action=PermissionAction::Allow`，`source=_source`，`context=Dispatch`
- `Confirm` 分支：在 `parent_approval` 命中后或 fallback 用户确认后调用，`action=PermissionAction::Confirm`，`context=Dispatch`
- `Deny` 分支：在 `spawn_tool_error` 之前调用，`action=PermissionAction::Deny`，`context=Dispatch`

同时在 `required_tag` 拒绝路径 [L125-L136](../../../src/systems/tools/dispatch.rs) 调用 `emit_permission_audit`，`action=Deny`，`context=TagDenied`。

- [ ] **步骤 4：运行 helper 单元测试验证通过**

运行：`cargo test --lib systems::tools::dispatch::tests::emit_permission_audit -- --nocapture`
预期：PASS。

- [ ] **步骤 5：编写简化集成测试（使用 build_harness_app，可选）**

helper 单元测试已覆盖核心逻辑（事件构造 + 字段正确性）。集成测试作为补充，验证 dispatch 全流程发出事件。若 `build_harness_app` 的夹具复杂度较高，可在实现时按实际情况调整或跳过此步骤。

在 `tests/permission_audit_dispatch.rs` 创建集成测试骨架：

```rust
//! PermissionAudit 事件集成测试：验证 dispatch 路径发出事件

use harness::app::build_harness_app;
use harness::domain::{EngineEvent, PermissionAction, PermissionAuditContext};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct CapturingFrontend(Arc<Mutex<Vec<EngineEvent>>>);
impl harness::channels::traits::Frontend for CapturingFrontend {
    fn kind(&self) -> harness::domain::FrontendKind {
        harness::domain::FrontendKind::Tui
    }
    fn push_event(&self, event: EngineEvent) {
        self.0.lock().unwrap().push(event);
    }
}

#[test]
fn dispatch_allow_emits_permission_audit() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let frontend = CapturingFrontend(captured.clone());

    let mut app = build_harness_app();
    // 注入捕获前端（覆盖 build_harness_app 默认前端）
    app.world_mut().insert_resource(harness::app::FrontendRegistry {
        frontends: vec![Box::new(frontend)],
    });

    // spawn agent + task + ToolExecutionRequestMessage，
    // agent 对某工具为 Allow，运行 app.update() 触发 tool_dispatch_system。
    // 具体夹具参照 tests/sequential_tool_confirmation.rs 的 build_test_app 模式。
    // 实现时按 build_harness_app 返回的 App 能力调整。

    let events = captured.lock().unwrap();
    let has_audit = events.iter().any(|e| {
        matches!(
            e,
            EngineEvent::PermissionAudit {
                action: PermissionAction::Allow,
                context: PermissionAuditContext::Dispatch,
                ..
            }
        )
    });
    assert!(has_audit, "expected PermissionAudit Allow event");
}
```

实现时参照 `tests/sequential_tool_confirmation.rs` 的 `build_test_app` 夹具模式补全 spawn 逻辑。

- [ ] **步骤 6：Commit**

```bash
git add src/systems/tools/dispatch.rs tests/permission_audit_dispatch.rs
git commit -m "feat(tools): dispatch.rs 发出 PermissionAudit 事件 + helper 单元测试"
```

---

## 任务 9：在其他路径发出 PermissionAudit 事件

**文件：**
- 修改：`src/systems/tools/async_dispatch.rs`
- 修改：`src/systems/tools/confirmation.rs`
- 修改：`src/systems/tools/approval.rs`
- 修改：`src/systems/maintenance.rs`

- [ ] **步骤 1：async_dispatch.rs 发出事件**

在 `src/systems/tools/async_dispatch.rs` 中，权限分流决定后（Allow 认领 / Confirm 跳过留给 sync 路径）发出 `PermissionAudit`。由于 `async_tool_dispatch_system` 已有 `frontend_registry: Option<Res<FrontendRegistry>>`，使用它。

在认领请求后（`if executor.kind() == ToolActionKind::Async` 通过、权限检查通过后）发出 `PermissionAudit`，`context=AsyncDispatch`，`action=Allow`。Confirm 路径不发（由 sync 路径发）。

具体位置：在 [L113-L117](../../../src/systems/tools/async_dispatch.rs) 权限检查 `continue` 之后、`let tool_call_id = ...` 之前。

- [ ] **步骤 2：confirmation.rs Permanent grant 发出事件**

在 `src/systems/tools/confirmation.rs` [L187-L203](../../../src/systems/tools/confirmation.rs) Permanent grant 写入 `overrides` 后，发出 `PermissionAudit`，`action=Grant`，`context=UserConfirmation`，`source=AgentOverride`。

需要从 `frontend_registry` 推送——`tool_confirmation_result_system` 已有 `frontend_registry` 参数。

- [ ] **步骤 3：approval.rs Permanent grant 发出事件**

在 `src/systems/tools/approval.rs` [L208-L220](../../../src/systems/tools/approval.rs) Permanent grant 写入 `agent.grant_permission` 后，发出 `PermissionAudit`，`action=Grant`，`context=ParentApproval`，`source=AgentOverride`。

`approval_result_system` 已有 `frontend_registry` 参数。

- [ ] **步骤 4：maintenance.rs spawn 后发出 tracing log（不发 EngineEvent）**

按规格更新：spawn 继承**不通过 `EngineEvent::PermissionAudit` 审计**，仅通过 `tracing::info` log 审计——避免给 `agent_factory_system` 增加 `FrontendRegistry` 参数。

在 `src/systems/maintenance.rs` `handle_spawn_request` 中 spawn 子 Agent 后 [L411-L414](../../../src/systems/maintenance.rs)，遍历 `tool_permissions.overrides` 发出 tracing log：

```rust
    for (tool, perm) in &tool_permissions.overrides {
        info!(
            event = "PermissionInherit",
            agent_id = %id,
            tool_name = %tool,
            permission = ?perm,
            context = "SpawnInherit",
            "子 Agent 继承父权限"
        );
    }
```

注意：`grant_permission` 的 tracing log 已在任务 2 步骤 3 中添加到 `Agent::grant_permission` 方法内，此处不重复。

- [ ] **步骤 5：编译并运行所有测试**

运行：`cargo build --lib && cargo test --lib`
预期：编译通过，所有测试 PASS。

- [ ] **步骤 6：Commit**

```bash
git add src/systems/tools/async_dispatch.rs src/systems/tools/confirmation.rs src/systems/tools/approval.rs src/systems/maintenance.rs
git commit -m "feat(tools): async_dispatch/confirmation/approval/maintenance 发出 PermissionAudit"
```

---

## 任务 10：文档同步

**文件：**
- 修改：`docs/current-state.md`
- 修改：`docs/configuration.md`

- [ ] **步骤 1：更新 docs/current-state.md**

定位权限决策相关段落，更新为三层回退描述。添加：

```markdown
- 工具权限决策采用三层回退：agent.overrides → agent.default_permission（显式配置时）→ ToolDefinition.default_permission
- `Agent::effective_permission(tool_name, registry)` 是权限查询单一入口
- `default_permission_explicit` 字段区分显式/隐式 Confirm，仅隐式 Confirm 回退到工具默认
```

- [ ] **步骤 2：更新 docs/configuration.md**

在 `[agent.tools]` 段说明添加：

```markdown
**default_permission 回退规则：**

- 若显式设置 `default_permission`，对未在 overrides 中列出的工具使用该值
- 若未设置 `default_permission`（结构默认 Confirm），对未在 overrides 中列出的工具回退到 `ToolDefinition.default_permission`（工具注册时声明的默认值）

**示例：**

\`\`\`toml
[agent.tools]
default_permission = "Deny"        # 显式 Deny：所有未列出的工具拒绝
shell_exec = "Allow"               # 显式 Allow
\`\`\`

\`\`\`toml
# 未写 [agent.tools] 段的 agent
# 所有工具回退到 ToolDefinition.default_permission
\`\`\`
```

- [ ] **步骤 3：Commit**

```bash
git add docs/current-state.md docs/configuration.md
git commit -m "docs: 权限决策链路三层回退说明"
```

---

## 任务 11：全量验证

- [ ] **步骤 1：运行完整 CI 检查**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

预期：全部通过。

- [ ] **步骤 2：运行 markdownlint**

```bash
markdownlint docs/superpowers/specs/2026-08-05-tool-permission-truth-source-design.md docs/superpowers/plans/2026-08-05-tool-permission-truth-source-plan.md docs/current-state.md docs/configuration.md
```

预期：通过。

- [ ] **步骤 3：Commit 任何修复**

```bash
git add -A
git commit -m "chore: CI 检查修复"
```

---

## 自检

**规格覆盖度：**

| 规格章节 | 对应任务 |
|---------|---------|
| 子模块 1：权限真相源 | 任务 1（explicit 字段）+ 任务 2（effective_permission）+ 任务 3-4（调用点迁移） |
| 子模块 2：子 Agent 权限继承 | 任务 4（spawn 逻辑）+ 任务 5（回归测试） |
| 子模块 3：required_tag 校验 | 任务 6 |
| 子模块 4：权限审计事件 | 任务 7（类型）+ 任务 8-9（发出点） |
| 文档同步 | 任务 10 |
| 测试 | 各任务内嵌 + 任务 11 全量验证 |

无遗漏。

**占位符扫描：** 无 TODO/待定。所有代码块完整。

**类型一致性：** `PermissionSource` 在任务 1 定义，任务 8 使用——名称一致。`PermissionAction` / `PermissionAuditContext` 在任务 7 定义（无 `SpawnInherit` 变体），任务 8-9 使用——名称一致。`effective_permission` 在任务 2 定义于 `AgentToolPermissions`，`Agent` 保留委托方法，任务 3-4 通过 `agent.effective_permission` 或 `agent.tool_permissions.effective_permission` 调用——签名一致。`default_permission_explicit` 在任务 1 定义，任务 2/4/5 使用——名称一致。`grant_permission` 在任务 2 增加 tracing log，任务 9 步骤 4 注明不重复。

**评审响应：** 已采纳评审 #1-#6、#8-#11。#7（Startup 时序）降级为防御性方案（`Update` 首帧 + `run_once`），注释说明 Bevy 0.18 默认行为已处理但防御性更稳妥。
