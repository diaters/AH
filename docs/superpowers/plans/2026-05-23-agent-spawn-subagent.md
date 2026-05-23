# Agent 自主创建子 Agent 功能实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 Agent 通过 `spawn_agent` Tool 创建子 Agent，支持权限继承、动态申请和审批路由。

**Architecture:** 扩展现有 Tool 系统和 Agent 工厂系统，新增审批路由逻辑，复用现有消息类型（ApprovalRequestMessage、ToolConfirmationRequestMessage）。

**Tech Stack:** Rust, Bevy ECS, serde_json

---

## 文件结构

| 文件 | 操作 | 职责 |
|------|------|------|
| `src/domain/mod.rs` | 修改 | 数据结构扩展（AgentSpawnRequestMessage、ApprovalResultMessage、GrantMode、ConfirmationSource） |
| `src/domain/mod.rs` | 修改 | Agent impl 新增 has_permission/grant_permission 方法 |
| `src/systems/tool.rs` | 修改 | 注册 spawn_agent Tool，实现 builtin executor，扩展审批路由逻辑 |
| `src/systems/tool.rs` | 修改 | 扩展 ToolConfirmationRequestMessage 新增字段 |
| `src/systems/tool.rs` | 修改 | 实现 approval_result_system |
| `src/systems/maintenance.rs` | 修改 | handle_spawn_request 支持 tools 参数 |
| `tests/tool_execution_flow.rs` | 修改 | 新增 spawn_agent 和审批路由测试 |

---

### Task 1: 数据结构扩展

**Files:**
- Modify: `src/domain/mod.rs:603-610` (AgentSpawnRequestMessage)
- Modify: `src/domain/mod.rs:766-772` (ApprovalResultMessage)
- Create: `src/domain/mod.rs` 新增 GrantMode, ConfirmationSource

- [ ] **Step 1: 修改 AgentSpawnRequestMessage**

将 `src/domain/mod.rs` 中 `AgentSpawnRequestMessage` 定义修改为：

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

- [ ] **Step 2: 新增 GrantMode 枚举**

在 `src/domain/mod.rs` 中 `ConfirmMode` 定义之后添加：

```rust
/// 授权模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantMode {
    /// 单次授权，仅本次执行
    Once,
    /// 永久授权，更新 Agent 权限配置
    Permanent,
}
```

- [ ] **Step 3: 扩展 ApprovalResultMessage**

将 `ApprovalResultMessage` 定义修改为：

```rust
/// 审批结果消息
#[derive(Debug, Clone, Component)]
pub struct ApprovalResultMessage {
    pub request_id: Uuid,
    pub source_task_id: TaskId,
    pub approval_task_id: TaskId,
    pub decision: ApprovalDecision,
    pub reasoning: String,
    /// 授权模式
    pub grant_mode: GrantMode,
}
```

- [ ] **Step 4: 新增 ConfirmationSource 枚举**

在 `GrantMode` 之后添加：

```rust
/// 审批来源
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfirmationSource {
    #[default]
    User,
    ParentAgent,
}
```

- [ ] **Step 5: 扩展 ToolConfirmationRequestMessage**

将 `ToolConfirmationRequestMessage` 定义修改为：

```rust
/// Tool 确认请求消息
#[derive(Debug, Clone, Component)]
pub struct ToolConfirmationRequestMessage {
    pub request_id: Uuid,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub options: Vec<ConfirmationOption>,
    /// 审批来源
    pub source: ConfirmationSource,
    /// 父 Agent ID（当 source == ParentAgent 时）
    pub parent_agent_id: Option<AgentId>,
}
```

- [ ] **Step 6: 运行编译检查**

Run: `cargo check`
Expected: 编译通过或仅有字段未使用的警告

- [ ] **Step 7: Commit**

```bash
git add src/domain/mod.rs
git commit -m "feat(domain): extend data structures for agent spawn and approval routing"
```

---

### Task 2: Agent 权限工具方法

**Files:**
- Modify: `src/domain/mod.rs` Agent impl 块

- [ ] **Step 1: 新增 has_permission 方法**

在 `Agent` 的 impl 块中添加：

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

注意：查找现有的 `impl Agent` 块位置，在其内部添加这两个方法。

- [ ] **Step 2: 编写单元测试**

在 `src/domain/mod.rs` 的 `#[cfg(test)]` 模块中添加：

```rust
#[test]
fn agent_has_permission_returns_true_for_allow() {
    let mut perms = AgentToolPermissions::default();
    perms.overrides.insert("test_tool".to_string(), ToolPermission::Allow);
    
    let agent = Agent {
        id: Uuid::nil(),
        profile: AgentProfile {
            name: "test".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "test".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: perms,
        experience: AgentExperience::default(),
    };
    
    assert!(agent.has_permission("test_tool"));
    assert!(!agent.has_permission("other_tool"));
}

#[test]
fn agent_grant_permission_updates_overrides() {
    let mut agent = Agent {
        id: Uuid::nil(),
        profile: AgentProfile {
            name: "test".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "test".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        experience: AgentExperience::default(),
    };
    
    assert!(!agent.has_permission("new_tool"));
    
    agent.grant_permission("new_tool".to_string());
    
    assert!(agent.has_permission("new_tool"));
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test agent_has_permission agent_grant_permission`
Expected: 2 tests passed

- [ ] **Step 4: Commit**

```bash
git add src/domain/mod.rs
git commit -m "feat(domain): add has_permission and grant_permission methods to Agent"
```

---

### Task 3: 注册 spawn_agent Tool

**Files:**
- Modify: `src/systems/tool.rs:23-55` (register_builtin_tools 函数)

- [ ] **Step 1: 在 register_builtin_tools 中添加 spawn_agent Tool**

在 `register_builtin_tools` 函数中添加：

```rust
pub fn register_builtin_tools(registry: &mut SpaceToolRegistry) {
    use crate::domain::{ToolExecutorKind, ToolSchema};

    // 现有 echo 工具
    registry.register(ToolDefinition {
        name: "echo".to_string(),
        description: "Echo back the input message".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("echo".to_string()),
    });

    // 现有 knowledge_search 工具
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
    });

    // 新增 spawn_agent 工具
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
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
    });
}
```

- [ ] **Step 2: 运行编译检查**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/systems/tool.rs
git commit -m "feat(tool): register spawn_agent tool definition"
```

---

### Task 4: 实现 spawn_agent Builtin Executor

**Files:**
- Modify: `src/systems/tool.rs` execute_builtin_tool 函数

- [ ] **Step 1: 扩展 execute_builtin_tool 函数**

找到 `execute_builtin_tool` 函数，添加 `spawn_agent` 的处理分支：

```rust
fn execute_builtin_tool(
    name: &str,
    input: &serde_json::Value,
    knowledge: &SpaceKnowledge,
) -> Result<serde_json::Value, ToolError> {
    match name {
        "echo" => Ok(input.clone()),
        "knowledge_search" => {
            // 现有实现...
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidInput("missing query".to_string()))?;
            
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as usize;

            let results: Vec<_> = knowledge
                .entries
                .iter()
                .filter(|entry| {
                    entry
                        .content
                        .to_lowercase()
                        .contains(&query.to_lowercase())
                })
                .take(limit)
                .collect();

            Ok(serde_json::json!({
                "results": results.iter().map(|e| &e.content).collect::<Vec<_>>()
            }))
        }
        "spawn_agent" => {
            // spawn_agent 不在这里执行，因为它需要访问 ECS World
            // 这里返回一个标记，由 tool_dispatch_system 特殊处理
            Ok(serde_json::json!({
                "status": "spawn_request_created",
                "message": "Agent spawn request has been submitted"
            }))
        }
        _ => Err(ToolError::NotFound(format!("unknown builtin tool: {}", name))),
    }
}
```

- [ ] **Step 2: 运行编译检查**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/systems/tool.rs
git commit -m "feat(tool): add spawn_agent builtin executor stub"
```

---

### Task 5: 扩展 tool_dispatch_system 审批路由

**Files:**
- Modify: `src/systems/tool.rs:102-220` (tool_dispatch_system 函数)

- [ ] **Step 1: 理解现有逻辑**

阅读 `tool_dispatch_system` 函数，理解当前的权限检查流程：
1. 检查 Tool 是否存在
2. 获取 Agent 权限
3. 根据 `get_permission()` 结果决定：Allow 直接执行，Confirm 生成确认请求，Deny 返回错误

- [ ] **Step 2: 在 Confirm 分支添加审批路由逻辑**

找到处理 `ToolPermission::Confirm` 的分支，修改为：

```rust
use crate::domain::{ConfirmationSource, GrantMode, WaitingReason};

// 在 tool_dispatch_system 函数中，处理 Confirm 权限的部分：

ToolPermission::Confirm => {
    // 检查是否是 spawn_agent 工具（需要特殊处理）
    if tool_name == "spawn_agent" {
        // spawn_agent 需要用户确认，生成确认请求
        let request_id = Uuid::new_v4();
        request.pending_confirmation_id = Some(request_id);
        
        commands.spawn(ToolConfirmationRequestMessage {
            request_id,
            task_id: request.request.task_id,
            agent_id: request.request.agent_id,
            tool_name: tool_name.clone(),
            tool_input: request.tool_input.clone(),
            options: ConfirmationOption::default_options(),
            source: ConfirmationSource::User,
            parent_agent_id: None,
        });
        
        // 标记任务等待确认
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.request.task_id) {
            task.status = TaskStatus::Waiting(WaitingReason::User);
        }
        continue;
    }
    
    // 检查 Agent 是否有父 Agent
    let parent_agent = agents
        .iter()
        .find(|a| a.id == agent.parent_id.unwrap_or(Uuid::nil()));
    
    if let Some(parent) = parent_agent {
        // 父 Agent 有该权限 → 父 Agent 审批
        if parent.has_permission(&tool_name) {
            let request_id = Uuid::new_v4();
            request.pending_confirmation_id = Some(request_id);
            
            commands.spawn(ToolConfirmationRequestMessage {
                request_id,
                task_id: request.request.task_id,
                agent_id: request.request.agent_id,
                tool_name: tool_name.clone(),
                tool_input: request.tool_input.clone(),
                options: ConfirmationOption::default_options(),
                source: ConfirmationSource::ParentAgent,
                parent_agent_id: Some(parent.id),
            });
            
            // 标记任务等待审批
            if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.request.task_id) {
                task.status = TaskStatus::Waiting(WaitingReason::Approval);
            }
            continue;
        }
    }
    
    // 无父 Agent 或父 Agent 无权限 → 用户审批
    let request_id = Uuid::new_v4();
    request.pending_confirmation_id = Some(request_id);
    
    commands.spawn(ToolConfirmationRequestMessage {
        request_id,
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        tool_name: tool_name.clone(),
        tool_input: request.tool_input.clone(),
        options: ConfirmationOption::default_options(),
        source: ConfirmationSource::User,
        parent_agent_id: None,
    });

    // 标记任务等待确认
    if let Some(mut task) = tasks.iter_mut().find(|t| t.id == request.request.task_id) {
        task.status = TaskStatus::Waiting(WaitingReason::User);
    }
}
```

- [ ] **Step 3: 运行编译检查**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 4: Commit**

```bash
git add src/systems/tool.rs
git commit -m "feat(tool): extend tool_dispatch_system with approval routing logic"
```

---

### Task 6: 处理 spawn_agent Tool 确认结果

**Files:**
- Modify: `src/systems/tool.rs:503-620` (tool_confirmation_result_system 函数)

- [ ] **Step 1: 在 tool_confirmation_result_system 中处理 spawn_agent**

在用户确认通过后的执行分支中，添加 spawn_agent 的特殊处理：

```rust
// 在 tool_confirmation_result_system 中，用户确认后的执行部分：

// 检查是否是 spawn_agent 工具
if tool_request.tool_name == "spawn_agent" {
    // 解析参数
    let name = tool_request.tool_input
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("child-agent")
        .to_string();
    
    let model = tool_request.tool_input
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    
    let description = tool_request.tool_input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    
    let tools: Vec<String> = tool_request.tool_input
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    
    // 生成 AgentSpawnRequestMessage
    commands.spawn(AgentSpawnRequestMessage {
        parent_agent_id: tool_request.request.agent_id,
        task_id: tool_request.request.task_id,
        name,
        model,
        description,
        tools,
    });
    
    // 生成成功结果
    let execution_result = AgentExecutionResult {
        task_id: tool_request.request.task_id,
        agent_id: tool_request.request.agent_id,
        request_kind: tool_request.request.request_kind.clone(),
        result: Ok("spawn_agent request submitted".to_string()),
    };
    
    commands.spawn(ToolExecutionResultMessage {
        result: execution_result,
        tool_name: "spawn_agent".to_string(),
        tool_output: Ok(serde_json::json!({
            "status": "spawn_request_created"
        })),
    });
    
    // 恢复 Task 状态
    if let Some(mut task) = tasks
        .iter_mut()
        .find(|t| t.id == tool_request.request.task_id)
    {
        task.status = TaskStatus::Ready;
    }
    
    // 清理请求
    commands.entity(request_entity).despawn();
    commands.entity(entity).despawn();
    continue;
}

// 其他工具的正常执行逻辑...
```

- [ ] **Step 2: 运行编译检查**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/systems/tool.rs
git commit -m "feat(tool): handle spawn_agent confirmation result"
```

---

### Task 7: 修改 handle_spawn_request 支持 tools 参数

**Files:**
- Modify: `src/systems/maintenance.rs:143-200` (handle_spawn_request 函数)

- [ ] **Step 1: 修改 handle_spawn_request 函数签名和实现**

将 `handle_spawn_request` 函数修改为：

```rust
fn handle_spawn_request(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    tasks: &mut Query<&mut Task>,
    clock: &Clock,
    request: &AgentSpawnRequestMessage,
) {
    let Some((_, parent_agent)) = agents
        .iter()
        .find(|(_, a)| a.id == request.parent_agent_id)
    else {
        warn!(
            event = "SpawnRequestFailed",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            reason = "parent_agent_not_found",
            "parent agent not found for spawn request"
        );
        mark_task_failed(
            tasks,
            clock,
            request.task_id,
            "parent agent not found for spawn request",
        );
        return;
    };

    // 过滤 tools：仅保留父 Agent 拥有的权限
    let allowed_tools: Vec<String> = request
        .tools
        .iter()
        .filter(|tool| parent_agent.has_permission(tool))
        .cloned()
        .collect();

    if allowed_tools.is_empty() {
        warn!(
            event = "SpawnRequestRejected",
            parent_id = %request.parent_agent_id,
            task_id = %request.task_id,
            requested_tools = ?request.tools,
            reason = "no_valid_tools",
            "spawn rejected: no valid tools after filtering"
        );
        let msg = format!(
            "Agent spawn rejected: requested tools {:?} not available in parent agent",
            request.tools
        );
        mark_task_failed(tasks, clock, request.task_id, &msg);
        return;
    }

    // 使用父 Agent 的 model（如果未指定）
    let model = request.model.clone().unwrap_or_else(|| parent_agent.profile.model.clone());

    let id = Uuid::new_v4();
    debug!(
        event = "TaskScopedAgentSpawned",
        agent_id = %id,
        agent_name = %request.name,
        agent_model = %model,
        parent_agent_id = %request.parent_agent_id,
        task_id = %request.task_id,
        tools = ?allowed_tools,
        "spawning task-scoped agent"
    );

    // 构建 tool_permissions
    let tool_permissions = AgentToolPermissions {
        default_permission: ToolPermission::Deny,
        overrides: allowed_tools
            .iter()
            .map(|t| (t.clone(), ToolPermission::Allow))
            .collect(),
    };

    commands.spawn(Agent {
        id,
        profile: AgentProfile {
            name: request.name.clone(),
            model,
        },
        capabilities: AgentCapabilities {
            tags: vec![],  // TaskScoped Agent 不参与路由
            description: request.description.clone(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(request.parent_agent_id),
        bound_task_id: Some(request.task_id),
        tool_permissions,
        experience: AgentExperience::default(),
    });

    let execution_request = AgentExecutionRequest {
        task_id: request.task_id,
        agent_id: id,
        request_kind: crate::domain::AgentRequestKind::LlmCompletion,
        prompt: String::new(),
        system_prompt: None,
    };

    commands.spawn(AgentExecutionRequestMessage {
        request: execution_request,
    });
}
```

- [ ] **Step 2: 运行编译检查**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 3: Commit**

```bash
git add src/systems/maintenance.rs
git commit -m "feat(maintenance): support tools parameter in handle_spawn_request"
```

---

### Task 8: 实现 approval_result_system

**Files:**
- Modify: `src/systems/tool.rs` 新增 system

- [ ] **Step 1: 新增 approval_result_system 函数**

在 `src/systems/tool.rs` 中添加新的 system：

```rust
/// 处理父 Agent 审批结果
pub(crate) fn approval_result_system(
    mut commands: Commands,
    mut agents: Query<&mut Agent>,
    mut tasks: Query<&mut Task>,
    registry: Res<SpaceToolRegistry>,
    knowledge: Res<SpaceKnowledge>,
    approval_results: Query<(Entity, &ApprovalResultMessage)>,
    tool_requests: Query<(Entity, &ToolExecutionRequestMessage)>,
) {
    for (entity, result) in &approval_results {
        // 查找对应的 Tool 执行请求
        let Some((request_entity, tool_request)) = tool_requests
            .iter()
            .find(|(_, r)| r.pending_confirmation_id == Some(result.request_id))
        else {
            debug!(
                event = "ApprovalResultNoMatch",
                request_id = %result.request_id,
                "no matching tool request found, may have been processed"
            );
            commands.entity(entity).despawn();
            continue;
        };

        match result.decision {
            ApprovalDecision::Rejected => {
                warn!(
                    event = "ToolApprovalRejected",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    reasoning = %result.reasoning,
                    "tool execution rejected by parent agent"
                );

                let execution_result = AgentExecutionResult {
                    task_id: tool_request.request.task_id,
                    agent_id: tool_request.request.agent_id,
                    request_kind: tool_request.request.request_kind.clone(),
                    result: Err(ExecutionError::UserCancelled(
                        format!("parent agent rejected: {}", result.reasoning)
                    )),
                };

                commands.spawn(ToolExecutionResultMessage {
                    result: execution_result,
                    tool_name: tool_request.tool_name.clone(),
                    tool_output: Err(ToolError::PermissionDenied(
                        format!("parent agent rejected: {}", result.reasoning)
                    )),
                });

                // 恢复 Task 状态
                if let Some(mut task) = tasks
                    .iter_mut()
                    .find(|t| t.id == result.source_task_id)
                {
                    task.status = TaskStatus::Ready;
                }

                commands.entity(request_entity).despawn();
            }
            ApprovalDecision::Approved => {
                debug!(
                    event = "ToolApprovalGranted",
                    tool_name = %tool_request.tool_name,
                    task_id = %tool_request.request.task_id,
                    agent_id = %tool_request.request.agent_id,
                    grant_mode = ?result.grant_mode,
                    "tool execution approved by parent agent"
                );

                // Permanent 模式：更新 Agent 权限
                if result.grant_mode == GrantMode::Permanent {
                    if let Some(mut agent) = agents
                        .iter_mut()
                        .find(|a| a.id == tool_request.request.agent_id)
                    {
                        agent.grant_permission(tool_request.tool_name.clone());
                        debug!(
                            event = "AgentPermissionUpdated",
                            agent_id = %agent.id,
                            tool_name = %tool_request.tool_name,
                            "agent permission updated to Allow permanently"
                        );
                    }
                }

                // 执行 Tool
                if let Some(tool_def) = registry.get(&tool_request.tool_name) {
                    execute_tool(
                        &mut commands,
                        request_entity,
                        tool_request,
                        tool_def,
                        &knowledge,
                    );
                }

                // 恢复 Task 状态
                if let Some(mut task) = tasks
                    .iter_mut()
                    .find(|t| t.id == result.source_task_id)
                {
                    task.status = TaskStatus::Ready;
                }
            }
        }

        commands.entity(entity).despawn();
    }
}
```

- [ ] **Step 2: 在 mod.rs 中导出 system**

在 `src/systems/mod.rs` 中确保导出：

```rust
pub(crate) use tool::{
    // ... 现有导出
    approval_result_system,
};
```

- [ ] **Step 3: 注册 system 到 App**

在 `src/app/mod.rs` 中找到 system 注册位置，添加：

```rust
// 在 PostUpdate 或 Transform set 中
.add_systems(PostUpdate, approval_result_system.in_set(HarnessSet::Transform));
```

- [ ] **Step 4: 运行编译检查**

Run: `cargo check`
Expected: 编译通过

- [ ] **Step 5: Commit**

```bash
git add src/systems/tool.rs src/systems/mod.rs src/app/mod.rs
git commit -m "feat(tool): implement approval_result_system for parent agent approval"
```

---

### Task 9: 集成测试 - spawn_agent Tool

**Files:**
- Modify: `tests/tool_execution_flow.rs`

- [ ] **Step 1: 新增 spawn_agent 测试**

在 `tests/tool_execution_flow.rs` 中添加：

```rust
#[test]
fn spawn_agent_creates_child_agent() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（拥有 spawn_agent 和 echo 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            overrides: HashMap::new(),
        },
    );

    // 注册 spawn_agent 工具
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "spawn_agent".to_string(),
        description: "Create a child agent".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin("spawn_agent".to_string()),
    });
    app.world_mut().insert_resource(registry);

    // 创建任务
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 发起 spawn_agent 请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id: parent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "spawn_agent".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "spawn_agent".to_string(),
        tool_input: serde_json::json!({
            "name": "child-agent",
            "description": "A test child agent",
            "tools": ["echo"]
        }),
        pending_confirmation_id: None,
    });

    // 运行系统
    for _ in 0..10 {
        app.update();
    }

    // 验证：子 Agent 已创建
    let child_agents: Vec<&Agent> = {
        let world = app.world();
        let mut query = world.query::<&Agent>();
        query.iter(world)
            .filter(|a| a.parent_id == Some(parent_id))
            .collect()
    };
    
    assert_eq!(child_agents.len(), 1, "should have created one child agent");
    let child = child_agents[0];
    assert_eq!(child.profile.name, "child-agent");
    assert_eq!(child.kind, AgentKind::TaskScoped);
    assert_eq!(child.bound_task_id, Some(task_id));
    assert!(child.has_permission("echo"));
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test spawn_agent_creates_child_agent`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/tool_execution_flow.rs
git commit -m "test(tool): add integration test for spawn_agent tool"
```

---

### Task 10: 集成测试 - 审批路由

**Files:**
- Modify: `tests/tool_execution_flow.rs`

- [ ] **Step 1: 新增父 Agent 审批路由测试**

```rust
#[test]
fn tool_request_routes_to_parent_agent_approval() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(MockExecutor);
    let (_input_tx, input_rx) = unbounded();
    let (output_tx, _output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    app.update();

    // 创建父 Agent（拥有 test_tool 权限）
    let parent_id = create_test_agent(
        app.world_mut(),
        AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: [("test_tool".to_string(), ToolPermission::Allow)]
                .into_iter()
                .collect(),
        },
    );

    // 创建子 Agent（无 test_tool 权限）
    let child_id = uuid::Uuid::new_v4();
    app.world_mut().spawn(Agent {
        id: child_id,
        profile: AgentProfile {
            name: "child".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec![],
            description: "child agent".to_string(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(parent_id),
        bound_task_id: None,
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Deny,
            overrides: HashMap::new(),
        },
        experience: AgentExperience::default(),
    });

    // 注册 test_tool（需要确认）
    let mut registry = SpaceToolRegistry::default();
    registry.register(ToolDefinition {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("echo".to_string()),
    });
    app.world_mut().insert_resource(registry);

    // 创建任务
    let task_entity = app
        .world_mut()
        .spawn((
            Task::from_user_input_ready("test task", 3),
            ShortTermMemory::default(),
        ))
        .id();
    let task_id = app.world().get::<Task>(task_entity).unwrap().id;

    // 子 Agent 发起 tool 请求
    let request = AgentExecutionRequest {
        task_id,
        agent_id: child_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: "test_tool".to_string(),
        },
        prompt: String::new(),
        system_prompt: None,
    };
    app.world_mut().spawn(ToolExecutionRequestMessage {
        request,
        tool_name: "test_tool".to_string(),
        tool_input: serde_json::json!({}),
        pending_confirmation_id: None,
    });

    // 运行系统
    for _ in 0..5 {
        app.update();
    }

    // 验证：生成了 ToolConfirmationRequestMessage，source 为 ParentAgent
    let confirmation_requests: Vec<&ToolConfirmationRequestMessage> = {
        let world = app.world();
        let mut query = world.query::<&ToolConfirmationRequestMessage>();
        query.iter(world).collect()
    };

    assert_eq!(confirmation_requests.len(), 1);
    assert_eq!(confirmation_requests[0].source, ConfirmationSource::ParentAgent);
    assert_eq!(confirmation_requests[0].parent_agent_id, Some(parent_id));
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test tool_request_routes_to_parent_agent_approval`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/tool_execution_flow.rs
git commit -m "test(tool): add approval routing integration tests"
```

---

### Task 11: 运行完整测试套件

- [ ] **Step 1: 运行所有测试**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 2: 运行 clippy 检查**

Run: `cargo clippy -- -D warnings`
Expected: 无警告

- [ ] **Step 3: 运行格式检查**

Run: `cargo fmt --check`
Expected: 格式正确

---

### Task 12: 更新文档

**Files:**
- Modify: `docs/design/2026-05-17-tool-space-design.md`

- [ ] **Step 1: 更新设计文档**

在设计文档中添加 spawn_agent Tool 的说明，更新 AgentSpawnRequestMessage 结构等。

- [ ] **Step 2: Commit**

```bash
git add docs/design/2026-05-17-tool-space-design.md
git commit -m "docs: update tool-space-design with spawn_agent feature"
```

---

## 实现总结

| 任务 | 文件 | 状态 |
|------|------|------|
| Task 1 | src/domain/mod.rs | 数据结构扩展 |
| Task 2 | src/domain/mod.rs | Agent 权限方法 |
| Task 3 | src/systems/tool.rs | 注册 spawn_agent Tool |
| Task 4 | src/systems/tool.rs | spawn_agent executor |
| Task 5 | src/systems/tool.rs | 审批路由逻辑 |
| Task 6 | src/systems/tool.rs | spawn_agent 确认处理 |
| Task 7 | src/systems/maintenance.rs | handle_spawn_request 修改 |
| Task 8 | src/systems/tool.rs | approval_result_system |
| Task 9 | tests/tool_execution_flow.rs | spawn_agent 测试 |
| Task 10 | tests/tool_execution_flow.rs | 审批路由测试 |
| Task 11 | - | 测试套件验证 |
| Task 12 | docs/ | 文档更新 |
