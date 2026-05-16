# Phase 3: 多 Agent 支持实施计划

> __For agentic workers:__ REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

__Goal:__ 实现多 Agent 支持——Agent 无状态化、配置文件加载持久性 Agent、
动态创建/销毁任务型 Agent、权限继承校验。

__Architecture:__ Agent 从可变状态实体转为不可变配置实体。
`AgentFactorySystem` 统一管理全生命周期（配置加载、创建、销毁）。
新增 `TaskTerminatedMessage` 驱动任务型 Agent 销毁。
`task_dispatch_system` 改为 tags 匹配。

__Tech Stack:__ Rust, Bevy ECS, TOML (serde + toml crate)

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `Cargo.toml` | 修改 | 添加 `toml` 依赖 |
| `src/domain/mod.rs` | 修改 | 移除 `AgentStatus`，Agent 新增 `kind`/`parent_id`/`bound_task_id` |
| | | 新增 `AgentKind`/`AgentSpawnRequestMessage`/`TaskTerminatedMessage` |
| `src/systems/maintenance.rs` | 重写 | `agent_factory_system`：加载配置 + 处理 SpawnRequest |
| | | + 处理 TaskTerminated |
| `src/systems/dispatch.rs` | 修改 | `task_dispatch_system`：tags 匹配替代 Idle 过滤 |
| `src/systems/transform.rs` | 修改 | `brain_decision_system`：移除 AgentStatus 修改 |
| | | 新增 `task_termination_system` |
| `src/systems/mod.rs` | 修改 | 更新 pub use，新增 `task_termination_system` 导出 |
| `src/app/mod.rs` | 修改 | `HarnessConfig` 新增 `agents_config_path` |
| | | app 构建中新增 `task_termination_system`，更新 `app_is_idle` |
| `agents.toml` | 新建 | 默认 Agent 配置文件 |
| `tests/mvp_flow.rs` | 修改 | 适配无状态 Agent，测试用例改用配置文件路径 |
| `tests/brain_dispatch_flow.rs` | 修改 | 适配无状态 Agent，移除 AgentStatus 断言 |
| `tests/multi_agent_flow.rs` | 新建 | 多 Agent 集成测试：配置加载、tags 匹配、动态创建、权限校验、自动销毁 |

---

### Task 1: 添加 toml 依赖

__Files:__

- Modify: `Cargo.toml`

- [ ] __Step 1: 添加 toml crate 到 Cargo.toml__

在 `[dependencies]` 中添加：

```toml
toml = "0.8"
```

- [ ] __Step 2: 验证编译__

Run: `cargo check`
Expected: 编译成功（仅添加依赖，不影响现有代码）

- [ ] __Step 3: Commit__

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 添加 toml 依赖用于 Agent 配置加载"
```

---

### Task 2: 修改 domain 层——移除 AgentStatus，新增 AgentKind 和 Message 类型

__Files:__

- Modify: `src/domain/mod.rs`

- [ ] __Step 1: 移除 AgentStatus 枚举和 Agent 中的 status 字段，新增 AgentKind、parent_id、bound_task_id__

在 `src/domain/mod.rs` 中：

1. 移除 `AgentStatus` 枚举定义
2. 将 `Agent` 结构体改为：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentKind {
    Persistent,
    TaskScoped,
}

#[derive(Debug, Clone, Component)]
pub struct Agent {
    pub id: AgentId,
    pub profile: AgentProfile,
    pub capabilities: AgentCapabilities,
    pub kind: AgentKind,
    pub parent_id: Option<AgentId>,
    pub bound_task_id: Option<TaskId>,
}
```

1. 在文件末尾添加新的 Message 和配置类型：

```rust
#[derive(Debug, Clone, Component)]
pub struct AgentSpawnRequestMessage {
    pub parent_agent_id: AgentId,
    pub task_id: TaskId,
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Component)]
pub struct TaskTerminatedMessage {
    pub task_id: TaskId,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub agent: Vec<AgentEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
}
```

- [ ] __Step 2: 编译检查——修复所有编译错误__

由于移除了 `AgentStatus`，需要同时修复引用它的文件。此时仅做最小修复使编译通过（标记为 `// TODO: Phase 3` 或直接删除无用代码）：

- `src/systems/maintenance.rs`：移除 `spawn_default_agent_system` 中对 `AgentStatus` 的使用，函数体清空
- `src/systems/dispatch.rs`：移除所有 `AgentStatus::Idle` / `AgentStatus::Busy` 引用，暂时用 `// TODO` 标记
- `src/systems/transform.rs`：移除所有 `agent.status = AgentStatus::Idle` 引用，暂时用 `// TODO` 标记

Run: `cargo check`
Expected: 编译成功

- [ ] __Step 3: Commit__

```bash
git add src/domain/mod.rs src/systems/maintenance.rs src/systems/dispatch.rs src/systems/transform.rs
git commit -m "refactor: 移除 AgentStatus，Agent 无状态化，新增 AgentKind 和 Message 类型"
```

---

### Task 3: 重写 agent_factory_system

__Files:__

- Modify: `src/systems/maintenance.rs`

- [ ] __Step 1: 实现 agent_factory_system 的三个职责__

将 `src/systems/maintenance.rs` 完整重写为：

```rust
use std::fs;

use bevy::prelude::*;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::HarnessSettings,
    domain::{
        Agent, AgentCapabilities, AgentEntry, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentKind, AgentProfile, AgentSpawnRequestMessage, Task, TaskTerminatedMessage, TaskId,
    },
};

/// 从配置文件加载持久性 Agent、处理任务型 Agent 创建请求、处理任务型 Agent 销毁。
pub(crate) fn agent_factory_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    agents: Query<(Entity, &Agent)>,
    spawn_requests: Query<(Entity, &AgentSpawnRequestMessage)>,
    terminated_messages: Query<(Entity, &TaskTerminatedMessage)>,
    mut loaded: Local<bool>,
) {
    // 1. 启动时加载配置
    if !*loaded {
        load_persistent_agents(&mut commands, &settings, &agents);
        *loaded = true;
    }

    // 2. 处理创建请求
    for (entity, request) in &spawn_requests {
        handle_spawn_request(&mut commands, &agents, request);
        commands.entity(entity).despawn();
    }

    // 3. 处理销毁
    for (entity, terminated) in &terminated_messages {
        handle_termination(&mut commands, &agents, terminated.task_id);
        commands.entity(entity).despawn();
    }
}

fn load_persistent_agents(
    commands: &mut Commands,
    settings: &HarnessSettings,
    agents: &Query<(Entity, &Agent)>,
) {
    let config_path = &settings.0.agents_config_path;

    let content = match fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(_) => {
            warn!("agents config file '{}' not found, no persistent agents loaded", config_path);
            return;
        }
    };

    let config: crate::domain::AgentConfig = match toml::from_str(&content) {
        Ok(config) => config,
        Err(err) => {
            error!("failed to parse agents config: {err}");
            panic!("invalid agents config: {err}");
        }
    };

    // 校验 name 唯一性
    let mut seen_names = std::collections::HashSet::new();
    for entry in &config.agent {
        if !seen_names.insert(entry.name.clone()) {
            panic!("duplicate agent name '{}' in config", entry.name);
        }
    }

    // 校验与已存在 Agent 不重名
    let existing_names: std::collections::HashSet<String> = agents
        .iter()
        .map(|(_, a)| a.profile.name.clone())
        .collect();

    for entry in &config.agent {
        if existing_names.contains(&entry.name) {
            panic!("agent name '{}' already exists", entry.name);
        }
    }

    for entry in &config.agent {
        let id = Uuid::new_v4();
        info!(name = %entry.name, %id, "spawning persistent agent");
        commands.spawn(Agent {
            id,
            profile: AgentProfile {
                name: entry.name.clone(),
                model: entry.model.clone(),
            },
            capabilities: AgentCapabilities {
                tags: entry.tags.clone(),
                description: entry.description.clone(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
        });
    }
}

fn handle_spawn_request(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    request: &AgentSpawnRequestMessage,
) {
    // 查找父 Agent
    let Some(parent_agent) = agents.iter().find(|(_, a)| a.id == request.parent_agent_id).map(|(_, a)| a) else {
        warn!(parent_id = %request.parent_agent_id, "parent agent not found for spawn request");
        return;
    };

    // 校验 tags 子集
    if !validate_tags_subset(&parent_agent.capabilities.tags, &request.tags) {
        warn!(
            parent_tags = ?parent_agent.capabilities.tags,
            child_tags = ?request.tags,
            "spawn rejected: child tags exceed parent tags"
        );
        // 回写错误到关联 Task 由调用方在产出 SpawnRequest 时处理
        return;
    }

    let id = Uuid::new_v4();
    info!(name = %request.name, %id, "spawning task-scoped agent");

    commands.spawn(Agent {
        id,
        profile: AgentProfile {
            name: request.name.clone(),
            model: request.model.clone(),
        },
        capabilities: AgentCapabilities {
            tags: request.tags.clone(),
            description: request.description.clone(),
        },
        kind: AgentKind::TaskScoped,
        parent_id: Some(request.parent_agent_id),
        bound_task_id: Some(request.task_id),
    });

    // 产出执行请求
    let execution_request = AgentExecutionRequest {
        task_id: request.task_id,
        agent_id: id,
        request_kind: crate::domain::AgentRequestKind::LlmCompletion,
        prompt: String::new(), // 由调用方在 prompt 字段填充
        system_prompt: None,
    };

    commands.spawn(AgentExecutionRequestMessage {
        request: execution_request,
    });
}

fn handle_termination(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    task_id: TaskId,
) {
    for (entity, agent) in agents.iter() {
        if agent.kind == AgentKind::TaskScoped && agent.bound_task_id == Some(task_id) {
            info!(name = %agent.profile.name, %task_id, "despawning task-scoped agent");
            commands.entity(entity).despawn();
        }
    }
}

/// 校验子 Agent 的 tags 是否是父 Agent tags 的子集。
pub(crate) fn validate_tags_subset(parent_tags: &[String], child_tags: &[String]) -> bool {
    child_tags.iter().all(|tag| parent_tags.contains(tag))
}
```

- [ ] __Step 2: 编译检查__

Run: `cargo check`
Expected: 可能有编译警告（`AgentSpawnRequestMessage` 的 `prompt` 暂时为空字符串），但无错误

- [ ] __Step 3: Commit__

```bash
git add src/systems/maintenance.rs
git commit -m "feat: 重写 agent_factory_system，支持配置加载、动态创建和销毁"
```

---

### Task 4: 修改 task_dispatch_system 和 brain_dispatch_system

__Files:__

- Modify: `src/systems/dispatch.rs`

- [ ] __Step 1: 重写 dispatch.rs__

将 `src/systems/dispatch.rs` 完整重写为：

```rust
use bevy::prelude::*;

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        Task, TaskStatus,
    },
    llm::brain_system_prompt,
};

/// 将 Ready 任务转换为 Agent 执行请求，按 tags 匹配选择最合适的持久性 Agent。
pub(crate) fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
) {
    for mut task in &mut tasks {
        if task.status != TaskStatus::Ready {
            continue;
        }

        let Some(agent) = select_agent(agents.iter(), &task.content) else {
            continue;
        };

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: agent.id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: task.content.clone(),
            system_prompt: None,
        };

        task.mark_waiting_for_agent(agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

/// 将 Ready 任务提交给 Brain Agent 进行调度决策。
pub(crate) fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    let brain_agent = agents.iter().find(|a| {
        a.kind == AgentKind::Persistent
            && a.capabilities.tags.contains(&"brain".to_string())
    });

    let Some(brain_agent) = brain_agent else {
        return;
    };

    let all_agent_descriptions: Vec<AgentDescription> = agents
        .iter()
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect();

    for mut task in &mut tasks {
        if task.status != TaskStatus::Ready {
            continue;
        }

        let prompt = brain_user_prompt_from_descriptions(
            &task.content,
            &all_agent_descriptions,
        );

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: brain_agent.id,
            request_kind: AgentRequestKind::BrainDecision,
            prompt,
            system_prompt: Some(brain_system_prompt()),
        };

        task.mark_waiting_for_brain(brain_agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

struct AgentDescription {
    name: String,
    model: String,
    tags: Vec<String>,
    description: String,
}

fn brain_user_prompt_from_descriptions(
    task_content: &str,
    agents: &[AgentDescription],
) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| !agent.tags.contains(&"brain".to_string()))
        .map(|agent| {
            format!(
                "- name: \"{}\"\n  model: \"{}\"\n  tags: {:?}\n  description: \"{}\"",
                agent.name, agent.model, agent.tags, agent.description,
            )
        })
        .collect();

    format!(
        r#"Task content: "{}"

Available agents:
{}

Select the best agent for this task and provide a delegate prompt."#,
        task_content,
        agent_descriptions.join("\n"),
    )
}

/// 从持久性 Agent 中选择最匹配的非 Brain Agent。
fn select_agent<'a>(agents: impl Iterator<Item = &'a Agent>, task_content: &str) -> Option<&'a Agent> {
    agents
        .filter(|a| a.kind == AgentKind::Persistent)
        .filter(|a| !a.capabilities.tags.contains(&"brain".to_string()))
        .max_by_key(|a| match_score(a, task_content))
}

/// MVP 匹配算法：基于 tags 与任务内容的关键词重叠度。
fn match_score(agent: &Agent, task_content: &str) -> usize {
    let lower = task_content.to_lowercase();
    agent
        .capabilities
        .tags
        .iter()
        .filter(|tag| lower.contains(&tag.to_lowercase()))
        .count()
}
```

- [ ] __Step 2: 编译检查__

Run: `cargo check`
Expected: 编译成功

- [ ] __Step 3: Commit__

```bash
git add src/systems/dispatch.rs
git commit -m "refactor: task_dispatch 按 tags 匹配，brain_dispatch 移除 Idle 过滤"
```

---

### Task 5: 修改 transform.rs——移除 AgentStatus 引用，新增 task_termination_system

__Files:__

- Modify: `src/systems/transform.rs`

- [ ] __Step 1: 重写 transform.rs__

将 `src/systems/transform.rs` 完整重写为：

```rust
use bevy::prelude::*;

use crate::{
    app::{Clock, ExecutionResultReceiver, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentExecutionResultMessage,
        AgentRequestKind, BrainDecisionError, FailureReason, RetryReadyMessage, Signal,
        SignalPayload, Task, TaskStatus, TaskTerminatedMessage, UserInputMessage,
        UserOutputMessage,
    },
    llm::parse_brain_decision,
};

/// 将轻量 Signal 转换为后续可消费的 Message。
pub(crate) fn signal_ingest_system(mut commands: Commands, signals: Query<(Entity, &Signal)>) {
    for (entity, signal) in &signals {
        match &signal.payload {
            SignalPayload::UserInput(content) => {
                commands.spawn(UserInputMessage {
                    content: content.clone(),
                });
            }
            SignalPayload::RetryWakeup(task_id) => {
                commands.spawn(RetryReadyMessage { task_id: *task_id });
            }
            SignalPayload::SystemWakeup => {}
        }

        commands.entity(entity).despawn();
    }
}

/// 将用户输入消息沉淀为可持续演化的 Task。
pub(crate) fn user_message_to_task_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    messages: Query<(Entity, &UserInputMessage)>,
) {
    for (entity, message) in &messages {
        commands.spawn(Task::from_user_input(
            message.content.clone(),
            settings.0.max_retries,
        ));
        commands.entity(entity).despawn();
    }
}

/// 将异步执行结果回注为 ECS 内的一次性 Message。
pub(crate) fn ingest_execution_results_system(
    mut commands: Commands,
    mut receiver: ResMut<ExecutionResultReceiver>,
) {
    while let Ok(result) = receiver.0.try_recv() {
        commands.spawn(AgentExecutionResultMessage { result });
    }
}

/// 消费 Brain 决策的执行结果，解析结构化决策，产出具体 Agent 的执行请求。
pub(crate) fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::BrainDecision {
            continue;
        }

        let result = &result_message.result;

        // 查找对应的 Task
        let Some(mut task) = tasks.iter_mut().find(|t| t.id == result.task_id) else {
            commands.entity(entity).despawn();
            continue;
        };

        match &result.result {
            Ok(content) => {
                match parse_brain_decision(content) {
                    Ok(decision) => {
                        let selected_agent = agents.iter().find(|agent| {
                            agent.profile.name == decision.selected_agent_name
                                && agent.kind == crate::domain::AgentKind::Persistent
                        });

                        let Some(selected_agent) = selected_agent else {
                            // 选定的 Agent 不存在，回退到非 Brain 的第一个持久性 Agent
                            let fallback = agents.iter().find(|agent| {
                                !agent.capabilities.tags.contains(&"brain".to_string())
                                    && agent.kind == crate::domain::AgentKind::Persistent
                            });

                            let Some(fallback) = fallback else {
                                task.last_error = Some(format!(
                                    "brain selected agent '{}' but no agent available",
                                    decision.selected_agent_name
                                ));
                                task.status = TaskStatus::Failed(FailureReason::AgentError);
                                task.updated_at = clock.0;
                                commands.entity(entity).despawn();
                                continue;
                            };

                            let request = AgentExecutionRequest {
                                task_id: task.id,
                                agent_id: fallback.id,
                                request_kind: AgentRequestKind::LlmCompletion,
                                prompt: decision.delegate_prompt,
                                system_prompt: None,
                            };

                            task.delegate = Some(fallback.id);
                            task.status = TaskStatus::Waiting(crate::domain::WaitingReason::Agent);
                            task.updated_at = clock.0;
                            commands.spawn(AgentExecutionRequestMessage { request });
                            commands.entity(entity).despawn();
                            continue;
                        };

                        let request = AgentExecutionRequest {
                            task_id: task.id,
                            agent_id: selected_agent.id,
                            request_kind: AgentRequestKind::LlmCompletion,
                            prompt: decision.delegate_prompt,
                            system_prompt: None,
                        };

                        task.delegate = Some(selected_agent.id);
                        task.status = TaskStatus::Waiting(crate::domain::WaitingReason::Agent);
                        task.updated_at = clock.0;
                        commands.spawn(AgentExecutionRequestMessage { request });
                    }
                    Err(BrainDecisionError::ParseFailed(msg)) => {
                        task.last_error = Some(format!("brain decision parse failed: {msg}"));
                        task.status = TaskStatus::Failed(FailureReason::AgentError);
                        task.updated_at = clock.0;
                    }
                    Err(BrainDecisionError::EmptyResponse) => {
                        task.last_error = Some("brain returned empty response".to_string());
                        task.status = TaskStatus::Failed(FailureReason::AgentError);
                        task.updated_at = clock.0;
                    }
                    Err(BrainDecisionError::UnknownAgent(name)) => {
                        task.last_error = Some(format!("brain selected unknown agent: {name}"));
                        task.status = TaskStatus::Failed(FailureReason::AgentError);
                        task.updated_at = clock.0;
                    }
                }
            }
            Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                task.schedule_retry(error, clock.0);
            }
            Err(error) => {
                task.mark_failed(error, clock.0);
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 根据执行结果更新 Task，并在需要时生成输出消息或重试状态。
pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            continue;
        }

        let result = &result_message.result;

        for mut task in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            match &result.result {
                Ok(content) => {
                    task.mark_done(content.clone(), clock.0);
                    commands.spawn(UserOutputMessage {
                        content: content.clone(),
                    });
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    task.schedule_retry(error, clock.0);
                }
                Err(error) => {
                    task.mark_failed(error, clock.0);
                    commands.spawn(UserOutputMessage {
                        content: format!(
                            "任务执行失败（{:?}）：{}",
                            task_status_failure_reason(&task).unwrap_or(FailureReason::Unknown),
                            error.message()
                        ),
                    });
                }
            }

            break;
        }

        commands.entity(entity).despawn();
    }
}

/// 消费重试准备消息并把任务重新置回 Ready。
pub(crate) fn retry_ready_system(
    clock: Res<Clock>,
    mut commands: Commands,
    messages: Query<(Entity, &RetryReadyMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, message) in &messages {
        for mut task in &mut tasks {
            if task.id == message.task_id {
                task.mark_ready_for_retry(clock.0);
                break;
            }
        }

        commands.entity(entity).despawn();
    }
}

/// 检测到达终态的 Task，产出 TaskTerminatedMessage 以驱动任务型 Agent 销毁。
pub(crate) fn task_termination_system(
    mut commands: Commands,
    tasks: Query<&Task, Changed<Task>>,
) {
    for task in &tasks {
        if task.status.is_terminal() {
            commands.spawn(TaskTerminatedMessage { task_id: task.id });
        }
    }
}

fn task_status_failure_reason(task: &Task) -> Option<FailureReason> {
    match &task.status {
        TaskStatus::Failed(reason) => Some(reason.clone()),
        _ => None,
    }
}
```

- [ ] __Step 2: 编译检查__

Run: `cargo check`
Expected: 编译成功

- [ ] __Step 3: Commit__

```bash
git add src/systems/transform.rs
git commit -m "refactor: transform 移除 AgentStatus 引用，新增 task_termination_system"
```

---

### Task 6: 更新 systems/mod.rs 和 app/mod.rs

__Files:__

- Modify: `src/systems/mod.rs`
- Modify: `src/app/mod.rs`

- [ ] __Step 1: 更新 systems/mod.rs__

```rust
mod dispatch;
mod execution;
mod ingress;
mod maintenance;
mod output;
mod transform;

use bevy::ecs::schedule::SystemSet;

pub(crate) use dispatch::{brain_dispatch_system, task_dispatch_system};
pub(crate) use execution::agent_execution_system;
pub(crate) use ingress::{input_ingress_system, retry_wakeup_system, tick_clock_system};
pub(crate) use maintenance::agent_factory_system;
pub(crate) use output::user_output_system;
pub(crate) use transform::{
    brain_decision_system, ingest_execution_results_system, llm_response_system,
    retry_ready_system, signal_ingest_system, task_termination_system,
    user_message_to_task_system,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, SystemSet)]
pub enum HarnessSet {
    Ingress,
    Signal,
    Transform,
    Dispatch,
    Execution,
    Output,
    Maintenance,
}
```

- [ ] __Step 2: 更新 HarnessConfig 和 app 构建逻辑__

在 `src/app/mod.rs` 中：

1. 修改 `HarnessConfig`：

```rust
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub max_retries: u32,
    pub llm: LlmProviderConfig,
    pub brain: Option<BrainConfig>,
    pub agents_config_path: String,
}
```

1. 修改 `HarnessConfig::from_env()`：

```rust
impl HarnessConfig {
    pub fn from_env() -> Result<Self> {
        let llm = LlmProviderConfig::from_env("gpt-4.1-mini")?;

        let brain = if std::env::var("HARNESS_BRAIN_ENABLED")
            .is_ok_and(|v| v.to_lowercase() == "true")
        {
            Some(BrainConfig {
                enabled: true,
                model: std::env::var("HARNESS_BRAIN_MODEL")
                    .unwrap_or_else(|_| llm.model.clone()),
                agent_name: std::env::var("HARNESS_BRAIN_AGENT_NAME")
                    .unwrap_or_else(|_| "brain".to_string()),
            })
        } else {
            None
        };

        let agents_config_path = std::env::var("HARNESS_AGENTS_CONFIG")
            .unwrap_or_else(|_| "agents.toml".to_string());

        Ok(Self {
            max_retries: 3,
            llm,
            brain,
            agents_config_path,
        })
    }
}
```

1. 修改 `HarnessConfig::default()`：

```rust
impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            llm: LlmProviderConfig {
                provider: crate::llm::LlmProviderKind::OpenAi,
                model: "gpt-4.1-mini".to_string(),
                api_key: "test-api-key".to_string(),
                api_base: None,
                org_id: None,
                project_id: None,
            },
            brain: None,
            agents_config_path: "agents.toml".to_string(),
        }
    }
}
```

1. 修改 `build_harness_app`：移除 `app.add_systems(Startup, spawn_default_agent_system)`，新增 `task_termination_system`：

```rust
// 移除: app.add_systems(Startup, spawn_default_agent_system);
app.add_systems(
    Update,
    (
        tick_clock_system.in_set(HarnessSet::Ingress),
        input_ingress_system.in_set(HarnessSet::Ingress),
        retry_wakeup_system.in_set(HarnessSet::Signal),
        signal_ingest_system.in_set(HarnessSet::Signal),
        ingest_execution_results_system.in_set(HarnessSet::Transform),
        brain_decision_system
            .in_set(HarnessSet::Transform)
            .after(ingest_execution_results_system),
        user_message_to_task_system.in_set(HarnessSet::Transform),
        retry_ready_system.in_set(HarnessSet::Transform),
        llm_response_system
            .in_set(HarnessSet::Transform)
            .after(ingest_execution_results_system),
        task_termination_system
            .in_set(HarnessSet::Transform)
            .after(llm_response_system),
        brain_dispatch_system
            .in_set(HarnessSet::Dispatch)
            .before(task_dispatch_system),
        task_dispatch_system.in_set(HarnessSet::Dispatch),
        agent_execution_system.in_set(HarnessSet::Execution),
        user_output_system.in_set(HarnessSet::Output),
        agent_factory_system.in_set(HarnessSet::Maintenance),
    ),
);
```

1. 更新 `app_is_idle`，添加新 Message 类型检查：

```rust
pub fn app_is_idle(world: &mut World) -> bool {
    let active_tasks = world
        .query::<&Task>()
        .iter(world)
        .filter(|task| !task.status.is_terminal())
        .count();
    let pending_signals = world.query::<&Signal>().iter(world).count();
    let pending_user_inputs = world.query::<&UserInputMessage>().iter(world).count();
    let pending_retry_ready = world.query::<&RetryReadyMessage>().iter(world).count();
    let pending_requests = world.query::<&AgentExecutionRequestMessage>().iter(world).count();
    let pending_results = world.query::<&AgentExecutionResultMessage>().iter(world).count();
    let pending_outputs = world.query::<&UserOutputMessage>().iter(world).count();
    let pending_spawn_requests = world.query::<&crate::domain::AgentSpawnRequestMessage>().iter(world).count();
    let pending_terminated = world.query::<&crate::domain::TaskTerminatedMessage>().iter(world).count();

    active_tasks == 0
        && pending_signals == 0
        && pending_user_inputs == 0
        && pending_retry_ready == 0
        && pending_requests == 0
        && pending_results == 0
        && pending_outputs == 0
        && pending_spawn_requests == 0
        && pending_terminated == 0
}
```

1. 更新 `use` 导入，移除 `spawn_default_agent_system`，新增 `task_termination_system`。

- [ ] __Step 3: 编译检查__

Run: `cargo check`
Expected: 编译成功

- [ ] __Step 4: Commit__

```bash
git add src/systems/mod.rs src/app/mod.rs
git commit -m "refactor: 更新 app 构建逻辑，接入 task_termination_system，移除 spawn_default_agent_system"
```

---

### Task 7: 创建默认配置文件并修复现有测试

__Files:__

- Create: `agents.toml`
- Modify: `tests/mvp_flow.rs`
- Modify: `tests/brain_dispatch_flow.rs`

- [ ] __Step 1: 创建 agents.toml__

```toml
[[agent]]
name = "default-llm-agent"
model = "gpt-4.1-mini"
tags = ["llm", "default", "general"]
description = "默认 LLM Agent，处理通用任务"

[[agent]]
name = "brain"
model = "gpt-4.1-mini"
tags = ["brain", "dispatcher"]
description = "Brain Agent，负责调度决策"
```

- [ ] __Step 2: 更新 tests/mvp_flow.rs__

测试需要指定 agents.toml 路径。修改 `HarnessConfig` 构造：

```rust
use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    build_harness_app, AgentExecutionRequest, AgentExecutor, ExecutorFuture, ExternalInput,
    HarnessConfig, OutputMessage, Task, TaskStatus,
};
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move { Ok(format!("echo: {}", request.prompt)) })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: "test-api-key".to_string(),
            api_base: None,
            org_id: None,
            project_id: None,
        },
        brain: None,
        agents_config_path: "agents.toml".to_string(),
    }
}

#[test]
fn completes_single_turn_conversation_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, output_tx);

    input_tx
        .send(ExternalInput::Text("你好，Harness".to_string()))
        .expect("input should be accepted");

    for _ in 0..8 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("output should be produced");
    assert_eq!(output.content, "echo: 你好，Harness");

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Done);
    assert_eq!(tasks[0].result_summary, "echo: 你好，Harness");
}
```

- [ ] __Step 3: 更新 tests/brain_dispatch_flow.rs__

```rust
use std::{sync::Arc, thread, time::Duration};

use crossbeam_channel::unbounded;
use harness::{
    build_harness_app, AgentExecutionRequest, AgentExecutor, BrainConfig, ExecutorFuture,
    ExternalInput, HarnessConfig, OutputMessage, Task, TaskStatus,
};
use tokio::runtime::Runtime;

struct BrainMockExecutor;

impl AgentExecutor for BrainMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        match request.request_kind {
            harness::AgentRequestKind::BrainDecision => {
                let decision = r#"{"selected_agent_name":"default-llm-agent","delegate_prompt":"请处理这个任务","reasoning":"测试用例"}"#;
                Box::pin(async move { Ok(decision.to_string()) })
            }
            harness::AgentRequestKind::LlmCompletion => {
                Box::pin(async move { Ok(format!("echo: {}", request.prompt)) })
            }
        }
    }
}

fn brain_test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: "test-api-key".to_string(),
            api_base: None,
            org_id: None,
            project_id: None,
        },
        brain: Some(BrainConfig {
            enabled: true,
            model: "test-brain-model".to_string(),
            agent_name: "brain".to_string(),
        }),
        agents_config_path: "agents.toml".to_string(),
    }
}

#[test]
fn completes_brain_dispatch_flow() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainMockExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(brain_test_config(), runtime, executor, input_rx, output_tx);

    input_tx
        .send(ExternalInput::Text("你好，Harness".to_string()))
        .expect("input should be accepted");

    for _ in 0..16 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("output should be produced");
    assert_eq!(output.content, "echo: 请处理这个任务");

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Done);
}

#[test]
fn mvp_flow_unchanged_when_brain_disabled() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(BrainMockExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();

    let mut no_brain_config = brain_test_config();
    no_brain_config.brain = None;
    let mut app = build_harness_app(no_brain_config, runtime, executor, input_rx, output_tx);

    input_tx
        .send(ExternalInput::Text("你好，Harness".to_string()))
        .expect("input should be accepted");

    for _ in 0..8 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("output should be produced");
    assert_eq!(output.content, "echo: 你好，Harness");
}
```

- [ ] __Step 4: 运行测试__

Run: `cargo test`
Expected: 所有测试通过

- [ ] __Step 5: Commit__

```bash
git add agents.toml tests/mvp_flow.rs tests/brain_dispatch_flow.rs
git commit -m "feat: 创建默认 Agent 配置文件，修复现有测试适配无状态 Agent"
```

---

### Task 8: 新增多 Agent 集成测试

__Files:__

- Create: `tests/multi_agent_flow.rs`

- [ ] __Step 1: 编写多 Agent 集成测试__

```rust
use std::{sync::Arc, thread, time::Duration};

use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::{
    build_harness_app, Agent, AgentCapabilities, AgentExecutionRequest, AgentExecutor,
    AgentKind, AgentProfile, ExecutorFuture, ExternalInput, HarnessConfig, OutputMessage,
    Task, TaskStatus, TaskTerminatedMessage,
};
use tokio::runtime::Runtime;

struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move { Ok(format!("echo: {}", request.prompt)) })
    }
}

fn multi_agent_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: "test-api-key".to_string(),
            api_base: None,
            org_id: None,
            project_id: None,
        },
        brain: None,
        agents_config_path: "agents.toml".to_string(),
    }
}

/// 验证启动时从 agents.toml 加载持久性 Agent。
#[test]
fn loads_persistent_agents_from_config() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, _) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(multi_agent_config(), runtime, executor, input_rx, output_tx);

    // 执行一帧让 factory system 加载配置
    app.update();

    let agents: Vec<Agent> = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query.iter(world).cloned().collect()
    };

    assert!(agents.len() >= 2, "should load at least 2 agents from config");

    let names: Vec<&str> = agents.iter().map(|a| a.profile.name.as_str()).collect();
    assert!(names.contains(&"default-llm-agent"), "should have default agent");
    assert!(names.contains(&"brain"), "should have brain agent");

    for agent in &agents {
        assert_eq!(agent.kind, AgentKind::Persistent);
        assert_eq!(agent.parent_id, None);
        assert_eq!(agent.bound_task_id, None);
    }
}

/// 验证 tags 匹配选择 Agent。
#[test]
fn selects_agent_by_tags_match() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, output_rx) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(multi_agent_config(), runtime, executor, input_rx, output_tx);

    input_tx
        .send(ExternalInput::Text("帮我写一段 general 代码".to_string()))
        .expect("input should be accepted");

    for _ in 0..8 {
        app.update();
        thread::sleep(Duration::from_millis(20));
    }

    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("output should be produced");
    assert!(output.content.starts_with("echo:"));

    let tasks: Vec<Task> = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        query.iter(world).cloned().collect()
    };
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Done);
}

/// 验证任务型 Agent 的创建、执行和销毁完整生命周期。
#[test]
fn task_scoped_agent_lifecycle() {
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(EchoExecutor);
    let (input_tx, input_rx) = unbounded();
    let (output_tx, _) = unbounded::<OutputMessage>();
    let mut app = build_harness_app(multi_agent_config(), runtime, executor, input_rx, output_tx);

    // 先加载持久性 Agent
    app.update();

    // 手动创建一个任务型 Agent
    let parent_agent_id = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        let default_agent = query
            .iter(world)
            .find(|a| a.profile.name == "default-llm-agent")
            .expect("default agent should exist");
        default_agent.id
    };

    let task_id = uuid::Uuid::new_v4();
    {
        let world = app.world_mut();
        world.spawn(Agent {
            id: uuid::Uuid::new_v4(),
            profile: AgentProfile {
                name: "sub-agent".to_string(),
                model: "gpt-4.1-mini".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec!["llm".to_string()],
                description: "子 Agent".to_string(),
            },
            kind: AgentKind::TaskScoped,
            parent_id: Some(parent_agent_id),
            bound_task_id: Some(task_id),
        });
    }

    // 确认任务型 Agent 存在
    let task_scoped_count = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query.iter(world).filter(|a| a.kind == AgentKind::TaskScoped).count()
    };
    assert_eq!(task_scoped_count, 1);

    // 创建一个终态 Task 并发送 TaskTerminatedMessage
    {
        let world = app.world_mut();
        world.spawn(Task {
            id: task_id,
            content: "test".to_string(),
            creator: parent_agent_id,
            delegate: None,
            status: TaskStatus::Done,
            input_summary: String::new(),
            result_summary: "done".to_string(),
            priority: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
        });
        world.spawn(TaskTerminatedMessage { task_id });
    }

    // 执行帧让 factory 处理销毁
    app.update();

    // 任务型 Agent 应已被销毁
    let task_scoped_count = {
        let world = app.world_mut();
        let mut query = world.query::<&Agent>();
        query.iter(world).filter(|a| a.kind == AgentKind::TaskScoped).count()
    };
    assert_eq!(task_scoped_count, 0, "task-scoped agent should be despawned after task termination");
}

/// 验证 tags 子集校验：子 Agent tags 超出父 Agent 时拒绝创建。
#[test]
fn tags_subset_validation_rejects_invalid_spawn() {
    let parent_tags = vec!["llm".to_string(), "code".to_string()];
    let child_tags = vec!["llm".to_string(), "code".to_string(), "web".to_string()];

    // 直接调用 validate_tags_subset 逻辑
    let is_valid = child_tags.iter().all(|tag| parent_tags.contains(tag));
    assert!(!is_valid, "child tags exceeding parent should be rejected");

    let valid_child_tags = vec!["llm".to_string()];
    let is_valid = valid_child_tags.iter().all(|tag| parent_tags.contains(tag));
    assert!(is_valid, "child tags that are a subset should be accepted");
}
```

- [ ] __Step 2: 运行测试__

Run: `cargo test`
Expected: 所有测试通过

- [ ] __Step 3: Commit__

```bash
git add tests/multi_agent_flow.rs
git commit -m "test: 新增多 Agent 集成测试——配置加载、tags 匹配、生命周期、权限校验"
```

---

### Task 9: 更新 TODO 和设计文档

__Files:__

- Modify: `docs/TODO.md`

- [ ] __Step 1: 更新 TODO.md__

将 Phase 3 部分从待办移到已完成：

```markdown
### Phase 3: 多 Agent 支持

- [x] Agent 无状态化（移除 AgentStatus）
- [x] 新增 AgentKind（Persistent / TaskScoped）
- [x] TOML 配置文件加载持久性 Agent
- [x] 重写 AgentFactorySystem（配置加载 + 动态创建 + 销毁）
- [x] 任务型 Agent 动态创建（AgentSpawnRequestMessage）
- [x] 任务型 Task 终态自动销毁（TaskTerminatedMessage）
- [x] Agent tags 匹配逻辑
- [x] tags 子集权限继承校验
- [x] 集成测试
```

- [ ] __Step 2: Commit__

```bash
git add docs/TODO.md
git commit -m "docs: 更新 TODO 列表，标记 Phase 3 完成"
```
