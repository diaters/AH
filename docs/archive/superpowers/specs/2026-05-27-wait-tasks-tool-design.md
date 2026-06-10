> **状态：已归档（2026-06-10）** — 本规格描述的功能已实现。
> 相关能力已记录在 [docs/current-state.md](../../current-state.md)。

# wait_tasks Tool 设计

## 概述

`wait_tasks` 是一个内置工具，允许 Agent 阻塞等待自己创建的子任务完成，并获取其结果。

## 需求

| 项目 | 决定 |
|------|------|
| Tool 名称 | `wait_tasks` |
| 阻塞方式 | 非阻塞挂起，Task 标记为 `Waiting(ToolExecution)` |
| 多任务处理 | 部分返回：已完成返回结果，未完成/失败返回状态 |
| 权限范围 | 仅限自己创建的子任务 |
| 超时参数 | 可选，单位秒，默认 5 分钟 |
| 失败处理 | 返回失败信息和原因 |

## 设计

### Tool 参数

```json
{
    "type": "object",
    "properties": {
        "task_ids": {
            "type": "array",
            "items": { "type": "string" },
            "description": "List of task IDs to wait for"
        },
        "timeout_secs": {
            "type": "integer",
            "description": "Timeout in seconds (default: 300)"
        }
    },
    "required": ["task_ids"]
}
```

### 返回格式

```json
{
    "results": [
        {
            "task_id": "uuid-1",
            "status": "Done",
            "result": "任务执行结果",
            "error": null
        },
        {
            "task_id": "uuid-2",
            "status": "Running",
            "result": null,
            "error": null
        },
        {
            "task_id": "uuid-3",
            "status": "Failed",
            "result": null,
            "error": "执行失败原因"
        }
    ],
    "timed_out": true
}
```

## 实现

### 1. 数据结构

#### 1.1 新增 `ToolAction` 变体

```rust
// src/domain/mod.rs
pub enum ToolAction {
    Direct(serde_json::Value),
    SpawnAgent { name, model, description, tools },
    CreateBatch(Vec<SubTaskDefinition>),
    // 新增
    WaitForTasks {
        task_ids: Vec<TaskId>,
        timeout_secs: u64,
    },
}
```

#### 1.2 新增组件：等待状态信息

```rust
// src/domain/mod.rs
/// Task 等待其他任务完成的状态信息
/// 此组件添加到发起等待的 Task Entity 上
#[derive(Component, Debug, Clone)]
pub struct WaitingForTasksInfo {
    /// 等待的目标任务 ID 列表
    pub target_task_ids: Vec<TaskId>,
    /// 超时时刻
    pub timeout_at: DateTime<Utc>,
    /// Tool call ID（用于返回结果给 LLM）
    pub tool_call_id: String,
    /// 发起等待的 Agent ID
    pub agent_id: AgentId,
}
```

#### 1.3 扩展 ToolContext

```rust
// src/domain/space.rs
/// 内置 Tool 执行上下文
pub struct ToolContext<'a> {
    pub knowledge: &'a SpaceKnowledge,
    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
}
```

#### 1.4 配置项

```rust
// src/app/mod.rs
pub struct HarnessConfig {
    // 现有配置...

    /// wait_tasks 工具的默认超时时间（秒）
    pub default_wait_tasks_timeout_secs: u64,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            // ...
            default_wait_tasks_timeout_secs: 300, // 5 分钟
        }
    }
}
```

### 2. Tool 实现

```rust
// src/systems/tool.rs

struct WaitTasksTool;

impl BuiltinTool for WaitTasksTool {
    fn name(&self) -> &str {
        "wait_tasks"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let task_ids = parse_task_ids(input)?;
        let timeout_secs = parse_timeout(input, ctx.default_timeout_secs);

        Ok(ToolAction::WaitForTasks {
            task_ids,
            timeout_secs,
        })
    }
}

fn parse_task_ids(input: &serde_json::Value) -> Result<Vec<TaskId>, ToolError> {
    let ids_value = input
        .get("task_ids")
        .ok_or_else(|| ToolError::InvalidInput("missing 'task_ids' parameter".to_string()))?;

    let ids_array = ids_value
        .as_array()
        .ok_or_else(|| ToolError::InvalidInput("'task_ids' must be an array".to_string()))?;

    let mut task_ids = Vec::new();
    for id_str in ids_array.iter().filter_map(|v| v.as_str()) {
        let id = Uuid::parse_str(id_str)
            .map_err(|_| ToolError::InvalidInput(format!("invalid task id: {}", id_str)))?;
        task_ids.push(id);
    }

    if task_ids.is_empty() {
        return Err(ToolError::InvalidInput("'task_ids' cannot be empty".to_string()));
    }

    Ok(task_ids)
}

fn parse_timeout(input: &serde_json::Value, default: u64) -> u64 {
    input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(default)
}
```

### 3. 权限验证

验证目标任务是否为当前 Agent 任务创建的子任务：

```rust
// src/systems/tool.rs

fn validate_task_ownership(
    current_task_id: TaskId,
    target_task_ids: &[TaskId],
    tasks: &Query<&Task>,
) -> Result<(), ToolError> {
    let current_task = tasks
        .iter()
        .find(|t| t.id == current_task_id)
        .ok_or_else(|| ToolError::NotFound(format!("current task {}", current_task_id)))?;

    for target_id in target_task_ids {
        let target = tasks
            .iter()
            .find(|t| t.id == *target_id)
            .ok_or_else(|| ToolError::NotFound(format!("task {}", target_id)))?;

        // 目标任务必须是当前任务的子任务（parent_task_id 匹配）
        if target.parent_task_id != Some(current_task_id) {
            return Err(ToolError::PermissionDenied(format!(
                "task {} is not a child of current task",
                target_id
            )));
        }
    }

    Ok(())
}
```

### 4. System 处理流程

#### 4.1 发起等待

在 `handle_tool_action` 中处理 `WaitForTasks` 动作：

- 验证任务归属
- 将当前 Task 标记为 `Waiting(ToolExecution)`
- 在当前 Task Entity 上添加 `WaitingForTasksInfo` 组件（而不是新建 Entity）

```rust
fn spawn_wait_for_tasks(
    commands: &mut Commands,
    task_entity: Entity,
    task_id: TaskId,
    agent_id: AgentId,
    tool_call_id: String,
    task_ids: Vec<TaskId>,
    timeout_secs: u64,
) {
    // 在 Task Entity 上添加等待信息组件
    commands.entity(task_entity).insert(WaitingForTasksInfo {
        target_task_ids: task_ids,
        timeout_at: Utc::now() + chrono::Duration::seconds(timeout_secs as i64),
        tool_call_id,
        agent_id,
    });
}
```

#### 4.2 轮询检查

```rust
/// 轮询检查等待中的任务
pub(crate) fn check_waiting_tasks_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    all_tasks: Query<&Task>,
) {
    for (entity, task, info) in &waiting_tasks {
        let timed_out = clock.0 >= info.timeout_at;

        let all_terminal = info.target_task_ids.iter().all(|id| {
            all_tasks
                .iter()
                .any(|t| t.id == *id && t.status.is_terminal())
        });

        if timed_out || all_terminal {
            let results = collect_task_results(&info.target_task_ids, &all_tasks);
            spawn_wait_result_message(
                &mut commands,
                entity,
                task.id,
                info,
                results,
                timed_out,
            );
            // 移除等待信息组件，Task 将恢复执行
            commands.entity(entity).remove::<WaitingForTasksInfo>();
        }
    }
}
```

#### 4.3 事件驱动优化

```rust
/// 任务完成时立即检查是否有任务在等待
pub(crate) fn on_task_completed_check_waiting(
    mut events: EventReader<TaskStatusChanged>,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    all_tasks: Query<&Task>,
    mut commands: Commands,
) {
    for event in events.read() {
        if !event.new_status.is_terminal() {
            continue;
        }

        for (entity, task, info) in &waiting_tasks {
            if info.target_task_ids.contains(&event.task_id) {
                let all_terminal = info.target_task_ids.iter().all(|id| {
                    all_tasks
                        .iter()
                        .any(|t| t.id == *id && t.status.is_terminal())
                });

                if all_terminal {
                    let results = collect_task_results(&info.target_task_ids, &all_tasks);
                    spawn_wait_result_message(
                        &mut commands,
                        entity,
                        task.id,
                        info,
                        results,
                        false,
                    );
                    commands.entity(entity).remove::<WaitingForTasksInfo>();
                }
            }
        }
    }
}
```

### 5. 结果收集

```rust
fn collect_task_results(
    task_ids: &[TaskId],
    tasks: &Query<&Task>,
) -> Vec<TaskWaitResult> {
    task_ids
        .iter()
        .map(|id| {
            let task = tasks.iter().find(|t| t.id == *id);
            TaskWaitResult {
                task_id: id.to_string(),
                status: task.map(|t| t.status.clone()).unwrap_or(TaskStatus::Pending),
                result: task.and_then(|t| {
                    if t.status == TaskStatus::Done {
                        Some(t.result_summary.clone())
                    } else {
                        None
                    }
                }),
                error: task.and_then(|t| {
                    if matches!(t.status, TaskStatus::Failed(_)) {
                        t.last_error.clone()
                    } else {
                        None
                    }
                }),
            }
        })
        .collect()
}

#[derive(Serialize)]
struct TaskWaitResult {
    task_id: String,
    status: TaskStatus,
    result: Option<String>,
    error: Option<String>,
}
```

### 6. 结果返回流程

当等待结束时，生成 `ToolExecutionResultMessage`，触发后续的响应处理流程：

```rust
fn spawn_wait_result_message(
    commands: &mut Commands,
    task_entity: Entity,
    task_id: TaskId,
    info: &WaitingForTasksInfo,
    results: Vec<TaskWaitResult>,
    timed_out: bool,
) {
    let output = serde_json::json!({
        "results": results,
        "timed_out": timed_out,
    });

    // 生成工具执行结果消息
    commands.spawn(ToolExecutionResultMessage {
        result: AgentExecutionResult {
            task_id,
            agent_id: info.agent_id,
            request_kind: AgentRequestKind::LlmCompletion,
            result: Ok(AgentExecutionOutput {
                content: OutputContent::Text("wait_tasks completed".to_string()),
                reasoning_content: None,
            }),
            prompt: String::new(),
            system_prompt: None,
            tools: vec![],
            reasoning_content: None,
        },
        tool_name: "wait_tasks".to_string(),
        tool_output: Ok(output),
        tool_call_id: info.tool_call_id.clone(),
        processed: false,
    });
}
```

### 7. System 调度

轮询和事件驱动两个 System 需要协调工作：

- `check_waiting_tasks_system`：负责超时检查，作为兜底机制
- `on_task_completed_check_waiting`：负责实时响应任务完成事件

两个 System 都会移除 `WaitingForTasksInfo` 组件，因此不会重复处理。

## 文件变更清单

| 文件 | 变更 |
|------|------|
| `src/domain/mod.rs` | 新增 `ToolAction::WaitForTasks`，`WaitingForTasksInfo` 组件 |
| `src/domain/space.rs` | 扩展 `ToolContext`，新增 `default_wait_tasks_timeout_secs` 字段 |
| `src/app/mod.rs` | 新增 `default_wait_tasks_timeout_secs` 配置项 |
| `src/systems/tool.rs` | 新增 `WaitTasksTool`，`check_waiting_tasks_system`，`on_task_completed_check_waiting`，修改 `handle_tool_action` |
| `src/lib.rs` | 注册新的 System |
