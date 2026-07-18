# 派发架构统一实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 建立单一 `PendingDispatch` Component 派发入口，将 4 个分散的派发 system 收敛为统一的 `dispatch_system`，治理 9 个已识别腐化点。

**架构：** 派发请求以 `PendingDispatch` Component 附加在 Task/WorkItem Entity 上，由单一 `dispatch_system` 扫描处理。Task 派发走 `BrainLlm` 或 `DirectDelegate` 策略；WorkItem 派发按 `work_type.required_tag()` 查找 Agent。Brain LLM 异步性通过 `AwaitingBrainDecision` 中间状态处理。

**技术栈：** Rust + Bevy ECS + ratatui（无前端改动）

**基础分支：** `feature/skill-first-class-experience-governance`（不基于 main，main 落后）

**设计文档：** `docs/design/2026-07-18-dispatch-architecture-unification-design.md`

---

## 文件结构

### 新增文件

| 文件路径 | 职责 |
|---|---|
| `src/domain/dispatch.rs` | 派发相关数据结构：`PendingDispatch` / `DispatchKind` / `DispatchStrategy` / `DispatchHint` / `AgentSpawnSpec` / `AwaitingBrainDecision` |
| `src/systems/dispatch/dispatch_system.rs` | 统一派发器，扫描 `PendingDispatch` 并执行派发决策 |
| `src/systems/dispatch/subtask_dispatch_preparation.rs` | SubTask 派发前置 system：DAG 检查 + 兄弟结果收集 + spawn spec 准备 |
| `src/systems/dispatch/brain_llm_builder.rs` | Brain LLM 调用辅助函数，从 `brain_dispatch.rs` 提取 |
| `tests/dispatch_phase2.rs` | 阶段 2 集成测试：dispatch_system 与旧 system 并存 |
| `tests/dispatch_phase3.rs` | 阶段 3 集成测试：Task 派发迁移 |
| `tests/dispatch_phase4.rs` | 阶段 4 集成测试：WorkItem 派发迁移 |
| `tests/dispatch_phase5.rs` | 阶段 5 集成测试：回归测试 |

### 修改文件

| 文件路径 | 修改内容 |
|---|---|
| `src/domain/mod.rs` | 新增 `dispatch` 模块导出 |
| `src/domain/work_item.rs` | 新增 `WorkItemType::required_tag()` 方法；删除 `WorkItem.tags` 字段；修改所有构造函数移除 `tags` 参数 |
| `src/domain/workflow.rs` | `SubTaskConfig.child_agent_name` 添加注释说明用途 |
| `src/systems/dispatch/mod.rs` | 新增 `dispatch_system` / `subtask_dispatch_preparation` / `brain_llm_builder` 模块导出 |
| `src/systems/dispatch/brain_dispatch.rs` | 移除 SubTask 派发逻辑和 BrainLlm 派发决策；保留 Brain LLM 调用构建逻辑（迁移到 `brain_llm_builder.rs`） |
| `src/systems/transform/brain_decision.rs` | 接入 `parse_brain_skill_selection`；产出 `PendingDispatch + DirectDelegate`；移除 fallback 逻辑 |
| `src/systems/experience/skill_update.rs` | `skill_update_workitem_system` 剥离直接派发逻辑，仅创建 WorkItem + 附加 PendingDispatch |
| `src/systems/experience/profile_generation.rs` | `profile_generation_workitem_system` 剥离直接派发逻辑；`ProfileGenerationContext` 迁移到 Entity Component |
| `src/systems/experience/collection.rs` | `experience_collection_workitem_system` 剥离直接 spawn AgentExecutionRequest，改为附加 PendingDispatch |
| `src/systems/dispatch/workitem_lifecycle_hook.rs` | 按 Context Component 分流失败处理 |
| `src/contracts/dispatch.rs` | 删除未使用 trait：`TagMatcher` / `AgentSelector` / `DispatchPolicy` / `TagBasedSelector` / `DefaultDispatchPolicy` / `SummarizerSelectionPolicy` |
| `src/plugins/dispatch.rs` | system 注册更新：移除旧 system，注册新 system |
| `src/systems/mod.rs` | 导出新 system |
| `src/domain/task.rs` | TopLevelTask 创建入口附加 `PendingDispatch`（按需） |
| `src/systems/transform/user_message_to_task_system.rs` | Task 创建时附加 `PendingDispatch` |
| `src/systems/transform/sub_task_batch_block_system.rs` | SubTask 创建时由 preparation system 接管，不再立即派发 |

### 删除文件

| 文件路径 | 删除原因 |
|---|---|
| `src/systems/dispatch/task_dispatch.rs` | 合并到 dispatch_system |
| `src/systems/dispatch/workitem_dispatch.rs` | 合并到 dispatch_system |
| `src/systems/dispatch/agent_selection.rs` | tag 匹配逻辑收敛到 dispatch_system |

---

## 阶段 1：数据结构定义

### 任务 1.1：创建 `src/domain/dispatch.rs` 数据结构

**文件：**
- 创建：`src/domain/dispatch.rs`
- 修改：`src/domain/mod.rs`
- 测试：`src/domain/dispatch.rs`（内联单元测试）

- [ ] **步骤 1：编写失败的测试**

在 `src/domain/dispatch.rs` 中编写单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::WorkItemType;

    #[test]
    fn pending_dispatch_task_kind_construction() {
        let hint = DispatchHint {
            strategy: DispatchStrategy::BrainLlm,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        };
        let pending = PendingDispatch {
            kind: DispatchKind::Task,
            hint,
        };
        assert!(matches!(pending.kind, DispatchKind::Task));
        assert!(matches!(pending.hint.strategy, DispatchStrategy::BrainLlm));
    }

    #[test]
    fn pending_dispatch_workitem_kind_construction() {
        let hint = DispatchHint {
            strategy: DispatchStrategy::DirectDelegate,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        };
        let pending = PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::SkillUpdate),
            hint,
        };
        assert!(matches!(
            pending.kind,
            DispatchKind::WorkItem(WorkItemType::SkillUpdate)
        ));
    }

    #[test]
    fn awaiting_brain_decision_carries_spawn_spec() {
        let spec = AgentSpawnSpec {
            name: "child-agent".to_string(),
            model: None,
            allowed_tools: vec![],
            parent_agent_id: None,
        };
        let awaiting = AwaitingBrainDecision {
            task_id: uuid::Uuid::nil(),
            spawn_spec: Some(spec),
        };
        assert!(awaiting.spawn_spec.is_some());
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib domain::dispatch`
预期：FAIL，报错 `could not find domain::dispatch`

- [ ] **步骤 3：创建 `src/domain/dispatch.rs` 数据结构**

```rust
//! 派发相关数据结构
//!
//! 定义统一的派发请求标记 Component 和相关类型。
//! 所有派发请求通过 `PendingDispatch` Component 流转，
//! 由单一的 `dispatch_system` 扫描处理。

use crate::domain::{AgentId, SkillId, TaskId, WorkItemType};
use bevy_ecs::prelude::Component;

/// 派发请求标记 Component，附加在 Task 或 WorkItem Entity 上。
///
/// 由派发请求生成器（Task 创建入口 / WorkItem 创建器 / SubTask preparation system）
/// 附加，由 `dispatch_system` 消费后移除。
#[derive(Component)]
pub struct PendingDispatch {
    pub kind: DispatchKind,
    pub hint: DispatchHint,
}

/// 派发类型
#[derive(Debug, Clone)]
pub enum DispatchKind {
    /// 合并 TopLevelTask + SubTask
    Task,
    /// WorkItem 派发，按 work_type 分流
    WorkItem(WorkItemType),
}

/// 派发策略
#[derive(Debug, Clone)]
pub enum DispatchStrategy {
    /// 走 Brain LLM 选 Agent + skill（默认）
    BrainLlm,
    /// Brain 决策后或显式指定，直接委派
    DirectDelegate,
}

/// 派发提示
#[derive(Debug, Clone)]
pub struct DispatchHint {
    pub strategy: DispatchStrategy,
    /// 显式指定的 Agent 名称（DirectDelegate 时必填）
    pub preferred_agent_name: Option<String>,
    /// 需要注入的 skill ID（可选）
    pub required_skill_id: Option<SkillId>,
    /// 需要 spawn 新 Agent 时携带的规格
    pub agent_spawn_spec: Option<AgentSpawnSpec>,
}

/// Agent 生成规格
#[derive(Debug, Clone)]
pub struct AgentSpawnSpec {
    pub name: String,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub parent_agent_id: Option<AgentId>,
}

/// Brain LLM 决策等待状态。
///
/// 由 `dispatch_system` 在 BrainLlm 策略下附加，
/// 由 `brain_decision_system` 处理 Brain 输出后移除。
#[derive(Component)]
pub struct AwaitingBrainDecision {
    pub task_id: TaskId,
    pub spawn_spec: Option<AgentSpawnSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::WorkItemType;

    #[test]
    fn pending_dispatch_task_kind_construction() {
        let hint = DispatchHint {
            strategy: DispatchStrategy::BrainLlm,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        };
        let pending = PendingDispatch {
            kind: DispatchKind::Task,
            hint,
        };
        assert!(matches!(pending.kind, DispatchKind::Task));
        assert!(matches!(pending.hint.strategy, DispatchStrategy::BrainLlm));
    }

    #[test]
    fn pending_dispatch_workitem_kind_construction() {
        let hint = DispatchHint {
            strategy: DispatchStrategy::DirectDelegate,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        };
        let pending = PendingDispatch {
            kind: DispatchKind::WorkItem(WorkItemType::SkillUpdate),
            hint,
        };
        assert!(matches!(
            pending.kind,
            DispatchKind::WorkItem(WorkItemType::SkillUpdate)
        ));
    }

    #[test]
    fn awaiting_brain_decision_carries_spawn_spec() {
        let spec = AgentSpawnSpec {
            name: "child-agent".to_string(),
            model: None,
            allowed_tools: vec![],
            parent_agent_id: None,
        };
        let awaiting = AwaitingBrainDecision {
            task_id: uuid::Uuid::nil(),
            spawn_spec: Some(spec),
        };
        assert!(awaiting.spawn_spec.is_some());
    }
}
```

- [ ] **步骤 4：在 `src/domain/mod.rs` 中注册模块并导出**

在 `src/domain/mod.rs` 的 `mod` 声明区（第 5-26 行附近）添加：

```rust
mod dispatch;
```

在 `src/domain/mod.rs` 的 `pub use` 导出区（第 153 行附近，`work_item` 导出之后）添加：

```rust
// dispatch
pub use dispatch::{
    AgentSpawnSpec, AwaitingBrainDecision, DispatchHint, DispatchKind, DispatchStrategy,
    PendingDispatch,
};
```

- [ ] **步骤 5：运行测试验证通过**

运行：`cargo test --lib domain::dispatch`
预期：PASS，3 个测试全部通过

- [ ] **步骤 6：运行 clippy 和 fmt 检查**

运行：`cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings`
预期：无 warning

- [ ] **步骤 7：Commit**

```bash
git add src/domain/dispatch.rs src/domain/mod.rs
git commit -m "feat(dispatch): add unified PendingDispatch data structures

Introduce PendingDispatch Component as the unified dispatch entry point.
Defines DispatchKind (Task/WorkItem), DispatchStrategy (BrainLlm/DirectDelegate),
DispatchHint, AgentSpawnSpec, and AwaitingBrainDecision state.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md"
```

---

### 任务 1.2：添加 `WorkItemType::required_tag()` 方法

**文件：**
- 修改：`src/domain/work_item.rs:14-29`
- 测试：`src/domain/work_item.rs`（内联单元测试）

- [ ] **步骤 1：编写失败的测试**

在 `src/domain/work_item.rs` 的 `#[cfg(test)]` 模块中（如无则在文件末尾添加）追加测试：

```rust
#[cfg(test)]
mod required_tag_tests {
    use super::*;

    #[test]
    fn required_tag_evaluation() {
        assert_eq!(WorkItemType::Evaluation.required_tag(), "evaluation");
    }

    #[test]
    fn required_tag_summarization() {
        assert_eq!(WorkItemType::Summarization.required_tag(), "summarization");
    }

    #[test]
    fn required_tag_experience_collection() {
        assert_eq!(
            WorkItemType::ExperienceCollection.required_tag(),
            "collect"
        );
    }

    #[test]
    fn required_tag_skill_update() {
        assert_eq!(WorkItemType::SkillUpdate.required_tag(), "skill-updater");
    }

    #[test]
    fn required_tag_profile_generation() {
        assert_eq!(
            WorkItemType::ProfileGeneration.required_tag(),
            "profile"
        );
    }

    #[test]
    fn required_tag_execution() {
        assert_eq!(WorkItemType::Execution.required_tag(), "execution");
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib domain::work_item::required_tag_tests`
预期：FAIL，报错 `no method named required_tag`

- [ ] **步骤 3：在 `WorkItemType` impl 中添加方法**

在 `src/domain/work_item.rs` 的 `WorkItemType` 枚举定义之后（第 29 行之后）添加：

```rust
impl WorkItemType {
    /// 返回此 WorkItem 类型对应的 Agent tag。
    ///
    /// `dispatch_system` 通过此方法查找匹配的 Persistent Agent。
    /// 集中管理 tag 映射，避免散落硬编码。
    pub fn required_tag(&self) -> &'static str {
        match self {
            WorkItemType::Evaluation => "evaluation",
            WorkItemType::Summarization => "summarization",
            WorkItemType::ExperienceCollection => "collect",
            WorkItemType::SkillUpdate => "skill-updater",
            WorkItemType::ProfileGeneration => "profile",
            WorkItemType::Execution => "execution",
        }
    }
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib domain::work_item::required_tag_tests`
预期：PASS，6 个测试全部通过

- [ ] **步骤 5：Commit**

```bash
git add src/domain/work_item.rs
git commit -m "feat(dispatch): add WorkItemType::required_tag() method

Centralize tag mapping for WorkItem dispatch. Each WorkItemType now
exposes its required Agent tag via required_tag(), eliminating
scattered hardcoded tags across dispatch systems.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.6 决策 13"
```

---

## 阶段 2：统一 dispatch_system 建立（与旧 system 并存）

### 任务 2.1：创建 `brain_llm_builder.rs` 辅助函数

**文件：**
- 创建：`src/systems/dispatch/brain_llm_builder.rs`
- 修改：`src/systems/dispatch/mod.rs`

**说明：** 从现有 `brain_dispatch.rs` 提取 Brain LLM 调用构建逻辑，供 `dispatch_system` 复用。本任务只提取函数，不修改 `brain_dispatch.rs` 的现有逻辑（并存阶段）。

- [ ] **步骤 1：创建 `src/systems/dispatch/brain_llm_builder.rs`**

```rust
//! Brain LLM 调用构建辅助函数
//!
//! 从原 `brain_dispatch.rs` 提取 Brain LLM 调用逻辑，供 `dispatch_system` 复用。
//! 保留 Brain Agent 选择（FirstBrainPolicy）、prompt 构建、AgentExecutionRequest 构造。

use crate::prelude::*;
use tracing::debug;

use crate::{
    app::HarnessSettings,
    contracts::{AgentCapabilitySummary, BrainSelectionPolicy, FirstBrainPolicy},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        MessageDispatchedHookPending, ShortTermMemory, SpaceToolRegistry, Task, ToolPermission,
    },
};

/// Brain Agent 描述（用于 prompt 构建）
#[derive(Debug, Clone)]
pub struct AgentDescription {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
}

/// 选择 Brain Agent
///
/// 通过 Tag 查找所有带 "brain" 标签的 Agent，使用 FirstBrainPolicy 选择。
pub fn select_brain_agent<'a>(
    agents: impl Iterator<Item = &'a Agent>,
) -> Option<&'a Agent> {
    let brain_candidates: Vec<AgentCapabilitySummary> = agents
        .filter(|a| {
            a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
        })
        .map(AgentCapabilitySummary::from_agent)
        .collect();

    let policy = FirstBrainPolicy;
    let brain_agent_id = policy.select_brain(&brain_candidates)?;
    // 注意：调用方需要通过 id 再查 Agent，这里返回 Option<&Agent> 需要重新查找
    // 实际实现：返回 brain_agent_id，调用方自行查找
    None
}

/// 选择 Brain Agent 并返回其引用
pub fn find_brain_agent<'a>(agents: &'a [&Agent]) -> Option<&'a Agent> {
    let brain_candidates: Vec<AgentCapabilitySummary> = agents
        .iter()
        .filter(|a| {
            a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
        })
        .map(|a| AgentCapabilitySummary::from_agent(*a))
        .collect();

    let policy = FirstBrainPolicy;
    let brain_agent_id = policy.select_brain(&brain_candidates)?;
    agents.iter().find(|a| a.id == brain_agent_id).copied()
}

/// 构建所有 Persistent Agent 的描述列表（供 Brain LLM prompt 使用）
pub fn build_agent_descriptions<'a>(
    agents: impl Iterator<Item = &'a Agent>,
) -> Vec<AgentDescription> {
    agents
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect()
}

/// 构建 Brain LLM 的 user prompt
pub fn build_brain_user_prompt(
    task_content: &str,
    short_term: Option<&ShortTermMemory>,
    agent_descriptions: &[AgentDescription],
) -> String {
    let prompt_with_history = build_prompt_with_history(task_content, short_term);
    brain_user_prompt_from_descriptions(&prompt_with_history, agent_descriptions)
}

/// 构建 Brain LLM 的工具列表（非 Deny）
pub fn build_brain_tools(
    registry: &SpaceToolRegistry,
    brain_agent: &Agent,
) -> Vec<crate::domain::ToolDefinition> {
    registry
        .iter()
        .filter(|tool_def| {
            !matches!(
                brain_agent.tool_permissions.get_permission(&tool_def.name),
                ToolPermission::Deny
            )
        })
        .cloned()
        .collect()
}

/// 构建 Brain LLM 执行请求
///
/// 组合 Brain Agent 选择、prompt 构建、工具过滤，产出 `AgentExecutionRequestMessage`。
/// 调用方负责 spawn 返回的 request。
pub fn build_brain_execution_request(
    task: &Task,
    short_term: Option<&ShortTermMemory>,
    agents: &[&Agent],
    registry: &SpaceToolRegistry,
) -> Option<(AgentExecutionRequestMessage, MessageDispatchedHookPending)> {
    let brain_agent = find_brain_agent(agents)?;
    let all_agent_descriptions = build_agent_descriptions(agents.iter().copied());
    let prompt = build_brain_user_prompt(&task.content, short_term, &all_agent_descriptions);
    let tools = build_brain_tools(registry, brain_agent);

    debug!(
        event = "BrainLlmRequestBuilt",
        task_id = %task.id,
        brain_agent_id = %brain_agent.id,
        brain_agent_name = %brain_agent.profile.name,
        prompt_len = prompt.len(),
        tools_count = tools.len(),
        "built brain llm execution request"
    );

    let request = AgentExecutionRequest {
        task_id: task.id,
        agent_id: brain_agent.id,
        request_kind: AgentRequestKind::BrainDecision,
        prompt,
        system_prompt: brain_agent.system_prompt.clone(),
        tools,
        conversation: None,
        work_item_id: None,
        model_override: None,
    };

    Some((
        AgentExecutionRequestMessage { request },
        MessageDispatchedHookPending,
    ))
}

/// 将 task 内容与 ShortTermMemory 历史组合成完整 prompt
fn build_prompt_with_history(
    task_content: &str,
    short_term: Option<&ShortTermMemory>,
) -> String {
    use crate::domain::EntryRole;

    let Some(stm) = short_term else {
        return task_content.to_string();
    };

    let mut history = String::new();
    for entry in &stm.entries {
        let role = match entry.role {
            EntryRole::User => "User",
            EntryRole::Assistant => "Assistant",
            EntryRole::Summary => "System note",
            EntryRole::Archive => continue,
        };
        history.push_str(&format!("{}: {}\n", role, entry.content));
    }

    format!(
        "{}\n\n[Current request]\n{}",
        history.trim_end(),
        task_content
    )
}

/// 从 Agent 描述列表构建 Brain user prompt
fn brain_user_prompt_from_descriptions(
    base_prompt: &str,
    agent_descriptions: &[AgentDescription],
) -> String {
    let agents_yaml: String = agent_descriptions
        .iter()
        .map(|a| {
            format!(
                "- name: {}\n  model: {}\n  tags: {}\n  description: {}",
                a.name,
                a.model,
                a.tags.join(", "),
                a.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "{}\n\n## 可选 Agent\n\n{}\n\n请调用 create_tasks 工具或返回 JSON 决策选择合适的 Agent。",
        base_prompt, agents_yaml
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_agent_descriptions_filters_persistent_only() {
        use crate::domain::{Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions};
        use uuid::Uuid;

        let persistent = Agent {
            id: Uuid::nil(),
            profile: AgentProfile {
                name: "p".to_string(),
                model: "m".to_string(),
            },
            capabilities: AgentCapabilities {
                tags: vec![],
                description: "d".to_string(),
            },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        };
        let scoped = Agent {
            kind: AgentKind::TaskScoped,
            ..persistent.clone()
        };

        let agents = vec![persistent, scoped];
        let descriptions = build_agent_descriptions(agents.iter());
        assert_eq!(descriptions.len(), 1);
        assert_eq!(descriptions[0].name, "p");
    }
}
```

- [ ] **步骤 2：在 `src/systems/dispatch/mod.rs` 中注册模块**

修改 `src/systems/dispatch/mod.rs`：

```rust
//! Dispatch 模块
//!
//! 包含任务分发和 Agent 选择相关的 System。

mod agent_lifecycle_hook;
mod agent_selection;
mod brain_dispatch;
mod brain_llm_builder;
mod dispatch_system;
mod memory_selection;
mod message_dispatched_hook;
mod task_dispatch;
mod workitem_dispatch;
mod workitem_lifecycle_hook;

pub(crate) use agent_lifecycle_hook::{agent_started_hook_system, agent_stopped_hook_system};
pub use brain_dispatch::brain_dispatch_system;
pub(crate) use brain_llm_builder::build_brain_execution_request;
pub(crate) use message_dispatched_hook::on_message_dispatched_hook_system;
pub use task_dispatch::task_dispatch_system;
pub(crate) use workitem_dispatch::workitem_dispatch_system;
pub(crate) use workitem_lifecycle_hook::workitem_lifecycle_hook_system;
```

注意：`dispatch_system` 模块和 `subtask_dispatch_preparation` 模块将在后续任务中添加，这里先只加 `brain_llm_builder`。先注释掉 `dispatch_system` 的导出，避免编译错误。

实际编辑时只添加 `brain_llm_builder` 相关行：

```rust
mod brain_llm_builder;
pub(crate) use brain_llm_builder::build_brain_execution_request;
```

- [ ] **步骤 3：运行测试验证编译通过**

运行：`cargo build --lib`
预期：编译通过，可能有 unused warning（函数尚未被调用）

- [ ] **步骤 4：运行单元测试**

运行：`cargo test --lib systems::dispatch::brain_llm_builder`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/systems/dispatch/brain_llm_builder.rs src/systems/dispatch/mod.rs
git commit -m "feat(dispatch): extract Brain LLM builder functions

Extract Brain LLM request building logic from brain_dispatch.rs into
reusable functions in brain_llm_builder.rs. Functions will be called
by dispatch_system in BrainLlm strategy.

No behavior change - brain_dispatch.rs still uses its own inline logic.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §3.4"
```

---

### 任务 2.2：创建 `dispatch_system.rs` 统一派发器

**文件：**
- 创建：`src/systems/dispatch/dispatch_system.rs`
- 修改：`src/systems/dispatch/mod.rs`、`src/systems/mod.rs`、`src/plugins/dispatch.rs`
- 测试：`tests/dispatch_phase2.rs`

**说明：** 创建 `dispatch_system` 但暂不启用（与旧 system 并存）。通过 `PendingDispatch` Component 互斥——旧 system 不处理带 `PendingDispatch` 的 Entity。本任务只创建骨架，真正的派发逻辑在阶段 3、4 填充。

- [ ] **步骤 1：编写失败的集成测试**

创建 `tests/dispatch_phase2.rs`：

```rust
//! 阶段 2 集成测试：dispatch_system 与旧 system 并存
//!
//! 验证 dispatch_system 能扫描 PendingDispatch Component 并执行派发决策。
//! 本阶段只验证骨架，不验证完整派发流程（阶段 3、4 完善）。

use harness::app::build_harness_app;
use harness::domain::{
    AgentExecutionRequestMessage, DispatchHint, DispatchKind, DispatchStrategy, PendingDispatch,
    Task, TaskStatus, WorkItem, WorkItemStatus, WorkItemType,
};

/// 验证 dispatch_system 能扫描到带 PendingDispatch 的 WorkItem Entity。
#[test]
fn dispatch_system_processes_pending_dispatch_workitem() {
    // 此测试在阶段 2 完成后启用，验证 dispatch_system 能识别 PendingDispatch
    // 实际派发逻辑（按 tag 找 Agent）在阶段 4 完善后验证
}

/// 验证 dispatch_system 不处理没有 PendingDispatch 的 Entity。
#[test]
fn dispatch_system_ignores_entity_without_pending_dispatch() {
    // 验证旧 system 仍处理不带 PendingDispatch 的 Entity
}
```

注意：本阶段测试为占位骨架，实际断言在阶段 3、4 填充。先创建文件确保测试框架可用。

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --test dispatch_phase2`
预期：FAIL，报错 `unresolved import harness::domain::PendingDispatch`（阶段 1.1 完成后可解析）或 `could not find harness::app::build_harness_app`

- [ ] **步骤 3：创建 `src/systems/dispatch/dispatch_system.rs` 骨架**

```rust
//! 统一派发系统
//!
//! 扫描带 `PendingDispatch` Component 的 Task / WorkItem Entity，执行派发决策。
//!
//! ## 派发流程
//!
//! - Task + BrainLlm：移除 PendingDispatch，加 AwaitingBrainDecision，spawn Brain LLM 调用
//! - Task + DirectDelegate：按 preferred_agent_name 找 Agent，委派或 spawn
//! - WorkItem(work_type)：按 work_type.required_tag() 找 Agent，委派或 fail
//!
//! ## 前置假设
//!
//! Task 和 WorkItem 是不同 entity，因此 dispatch_system 持有两个 mut Query
//! 不会触发 Bevy ECS 的 query 冲突。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::Clock,
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentSpawnRequestMessage,
        AwaitingBrainDecision, DispatchHint, DispatchKind, DispatchStrategy, FailureReason,
        MessageDispatchedHookPending, PendingDispatch, Task, TaskStatus, ToolPermission,
        WaitingReason, WorkItem, WorkItemLifecycleHookPending,
    },
    systems::dispatch::brain_llm_builder::build_brain_execution_request,
    user_plugins::hook_point::HookPoint,
};

/// 统一派发系统
///
/// 在 `HarnessSet::Dispatch` 中运行，扫描所有带 `PendingDispatch` 的 Task / WorkItem。
pub fn dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    agents: Query<&Agent>,
    registry: Res<crate::domain::SpaceToolRegistry>,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&crate::domain::ShortTermMemory>,
        Option<&PendingDispatch>,
    )>,
    mut work_items: Query<(Entity, &mut WorkItem, Option<&PendingDispatch>)>,
) {
    // ============ Task 派发 ============
    for (entity, mut task, short_term, pending) in tasks.iter_mut() {
        let Some(pending) = pending else {
            continue;
        };
        let DispatchKind::Task = &pending.kind else {
            continue;
        };

        // 只处理 Ready / Pending 状态的 Task
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 已有 delegate 的 Task 跳过
        if task.delegate.is_some() {
            continue;
        }

        match &pending.hint.strategy {
            DispatchStrategy::BrainLlm => {
                handle_task_brain_llm(
                    &mut commands,
                    &clock,
                    entity,
                    &mut task,
                    short_term,
                    &agents,
                    &registry,
                    &pending.hint,
                );
            }
            DispatchStrategy::DirectDelegate => {
                handle_task_direct_delegate(
                    &mut commands,
                    &clock,
                    entity,
                    &mut task,
                    &agents,
                    &pending.hint,
                );
            }
        }
    }

    // ============ WorkItem 派发 ============
    for (entity, mut work_item, pending) in work_items.iter_mut() {
        let Some(pending) = pending else {
            continue;
        };
        let DispatchKind::WorkItem(work_type) = &pending.kind else {
            continue;
        };

        // 只处理 Pending 状态的 WorkItem
        if work_item.status != crate::domain::WorkItemStatus::Pending {
            continue;
        }

        handle_workitem_dispatch(&mut commands, &clock, entity, &mut work_item, *work_type, &agents);
    }
}

/// 处理 Task 的 BrainLlm 策略
fn handle_task_brain_llm(
    commands: &mut Commands,
    _clock: &Clock,
    entity: Entity,
    task: &mut Task,
    short_term: Option<&crate::domain::ShortTermMemory>,
    agents: &Query<&Agent>,
    registry: &crate::domain::SpaceToolRegistry,
    hint: &DispatchHint,
) {
    let agents_vec: Vec<&Agent> = agents.iter().collect();

    let Some((request_msg, hook_pending)) =
        build_brain_execution_request(task, short_term, &agents_vec, registry)
    else {
        warn!(
            event = "BrainAgentNotFound",
            task_id = %task.id,
            "no brain agent found, marking task failed"
        );
        task.status = TaskStatus::Failed(FailureReason::AgentError);
        task.last_error = Some("no brain agent available".to_string());
        commands.entity(entity).remove::<PendingDispatch>();
        return;
    };

    // 移除 PendingDispatch，加 AwaitingBrainDecision
    commands.entity(entity).remove::<PendingDispatch>();
    commands.entity(entity).insert(AwaitingBrainDecision {
        task_id: task.id,
        spawn_spec: hint.agent_spawn_spec.clone(),
    });

    task.status = TaskStatus::Waiting(WaitingReason::Agent);
    // delegate 暂不设置，等待 brain_decision_system 解析后由 DirectDelegate 设置

    commands.spawn((request_msg, hook_pending));

    debug!(
        event = "TaskDispatchedToBrain",
        task_id = %task.id,
        "task dispatched to brain agent for decision"
    );
}

/// 处理 Task 的 DirectDelegate 策略
fn handle_task_direct_delegate(
    commands: &mut Commands,
    clock: &Clock,
    entity: Entity,
    task: &mut Task,
    agents: &Query<&Agent>,
    hint: &DispatchHint,
) {
    let Some(agent_name) = &hint.preferred_agent_name else {
        warn!(
            event = "DirectDelegateWithoutPreferredAgent",
            task_id = %task.id,
            "DirectDelegate strategy requires preferred_agent_name, marking task failed"
        );
        task.status = TaskStatus::Failed(FailureReason::AgentError);
        task.last_error = Some("DirectDelegate without preferred_agent_name".to_string());
        task.updated_at = clock.0;
        commands.entity(entity).remove::<PendingDispatch>();
        return;
    };

    let agent = agents.iter().find(|a| &a.profile.name == agent_name);

    match agent {
        Some(agent) => {
            // 委派给已有 Agent
            task.delegate = Some(agent.id);
            task.status = TaskStatus::Waiting(WaitingReason::Agent);
            task.updated_at = clock.0;

            // 注入 skill（如有）
            if let Some(skill_id) = &hint.required_skill_id {
                commands.entity(entity).insert(crate::domain::TaskInjectedSkill {
                    skill_id: Some(skill_id.clone()),
                });
            }

            // spawn 执行请求
            let request = AgentExecutionRequest {
                task_id: task.id,
                agent_id: agent.id,
                request_kind: crate::domain::AgentRequestKind::LlmCompletion,
                prompt: task.content.clone(),
                system_prompt: agent.system_prompt.clone(),
                tools: filter_tools_for_agent(&crate::domain::SpaceToolRegistry::default(), agent),
                conversation: None,
                work_item_id: None,
                model_override: None,
            };
            commands.spawn((
                AgentExecutionRequestMessage { request },
                MessageDispatchedHookPending,
            ));

            commands.entity(entity).remove::<PendingDispatch>();

            debug!(
                event = "TaskDirectDelegated",
                task_id = %task.id,
                agent_name = %agent.profile.name,
                "task directly delegated to existing agent"
            );
        }
        None => {
            // 找不到 Agent，如果有 spawn spec 则 spawn 新 Agent
            if let Some(spec) = &hint.agent_spawn_spec {
                spawn_agent_and_delegate(commands, clock, entity, task, spec, hint);
            } else {
                warn!(
                    event = "DirectDelegateAgentNotFound",
                    task_id = %task.id,
                    preferred_agent_name = %agent_name,
                    "agent not found and no spawn spec, marking task failed"
                );
                task.status = TaskStatus::Failed(FailureReason::AgentError);
                task.last_error =
                    Some(format!("agent '{}' not found and no spawn spec", agent_name));
                task.updated_at = clock.0;
                commands.entity(entity).remove::<PendingDispatch>();
            }
        }
    }
}

/// spawn 新 Agent 后委派（SubTask 场景）
fn spawn_agent_and_delegate(
    commands: &mut Commands,
    _clock: &Clock,
    _entity: Entity,
    task: &mut Task,
    spec: &crate::domain::AgentSpawnSpec,
    hint: &DispatchHint,
) {
    // spawn AgentSpawnRequestMessage（由 agent_factory_system 消费创建 Agent）
    commands.spawn(AgentSpawnRequestMessage {
        parent_agent_id: spec.parent_agent_id.unwrap_or_default(),
        task_id: task.id,
        name: spec.name.clone(),
        model: spec.model.clone(),
        description: spec.name.clone(),
        tools: spec.allowed_tools.clone(),
        task_prompt: task.content.clone(),
        task_system_prompt: None,
    });

    // 注入 skill（如有）
    if let Some(skill_id) = &hint.required_skill_id {
        commands.entity(_entity).insert(crate::domain::TaskInjectedSkill {
            skill_id: Some(skill_id.clone()),
        });
    }

    // Task 状态由 agent_factory_system 创建 Agent 后通过其他路径设置 delegate
    // 这里先标记为 Waiting(Agent)，等待 Agent spawn 完成后由后续 system 处理
    task.status = TaskStatus::Waiting(WaitingReason::Agent);

    debug!(
        event = "SubTaskAgentSpawnRequested",
        task_id = %task.id,
        agent_name = %spec.name,
        "spawned agent for sub-task"
    );
}

/// 处理 WorkItem 派发
fn handle_workitem_dispatch(
    commands: &mut Commands,
    clock: &Clock,
    entity: Entity,
    work_item: &mut WorkItem,
    work_type: crate::domain::WorkItemType,
    agents: &Query<&Agent>,
) {
    let tag = work_type.required_tag();
    let agent = agents
        .iter()
        .find(|a| a.capabilities.tags.iter().any(|t| t == tag));

    match agent {
        Some(agent) => {
            // 派发成功
            work_item.assign(agent.id);
            work_item.start();

            commands
                .entity(entity)
                .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted));

            // spawn 执行请求
            let request_kind = match work_type {
                crate::domain::WorkItemType::Evaluation => {
                    crate::domain::AgentRequestKind::Evaluation
                }
                crate::domain::WorkItemType::Summarization => {
                    crate::domain::AgentRequestKind::Summarization
                }
                _ => crate::domain::AgentRequestKind::LlmCompletion,
            };

            let request = AgentExecutionRequest {
                task_id: work_item.task_id,
                agent_id: agent.id,
                request_kind,
                prompt: work_item.input.prompt.clone(),
                system_prompt: work_item
                    .input
                    .context
                    .system_prompt
                    .clone()
                    .or_else(|| agent.system_prompt.clone()),
                tools: work_item.input.context.tools.clone(),
                conversation: work_item.input.context.conversation.clone(),
                work_item_id: Some(work_item.id),
                model_override: None,
            };
            commands.spawn((
                AgentExecutionRequestMessage { request },
                MessageDispatchedHookPending,
            ));

            commands.entity(entity).remove::<PendingDispatch>();

            debug!(
                event = "WorkItemDispatched",
                work_item_id = %work_item.id,
                task_id = %work_item.task_id,
                work_type = ?work_type,
                agent_id = %agent.id,
                agent_name = %agent.profile.name,
                "work item dispatched via unified dispatch_system"
            );
        }
        None => {
            // 派发失败
            work_item.fail();
            commands
                .entity(entity)
                .insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed));
            commands.entity(entity).remove::<PendingDispatch>();

            warn!(
                event = "WorkItemNoAgentFound",
                work_item_id = %work_item.id,
                task_id = %work_item.task_id,
                work_type = ?work_type,
                required_tag = tag,
                "no suitable agent found for work item, marking as failed"
            );
        }
    }
}

/// 过滤工具列表（非 Deny）
fn filter_tools_for_agent(
    registry: &crate::domain::SpaceToolRegistry,
    agent: &Agent,
) -> Vec<crate::domain::ToolDefinition> {
    registry
        .iter()
        .filter(|td| {
            !matches!(
                agent.tool_permissions.get_permission(&td.name),
                ToolPermission::Deny
            )
        })
        .cloned()
        .collect()
}
```

- [ ] **步骤 4：在 `src/systems/dispatch/mod.rs` 中注册模块**

修改 `src/systems/dispatch/mod.rs`，添加：

```rust
mod dispatch_system;
pub(crate) use dispatch_system::dispatch_system;
```

- [ ] **步骤 5：在 `src/systems/mod.rs` 中导出**

修改 `src/systems/mod.rs` 的 `pub(crate) use dispatch::{...}` 块（第 24-28 行），添加 `dispatch_system`：

```rust
pub(crate) use dispatch::{
    agent_started_hook_system, agent_stopped_hook_system, brain_dispatch_system,
    dispatch_system, on_message_dispatched_hook_system, task_dispatch_system,
    workitem_dispatch_system, workitem_lifecycle_hook_system,
};
```

- [ ] **步骤 6：在 `src/plugins/dispatch.rs` 中注册 system**

修改 `src/plugins/dispatch.rs`，在 `app.add_systems` 块中添加 `dispatch_system`（与旧 system 并存，但暂不处理任何带 `PendingDispatch` 的 Entity——因为还没有 Entity 被附加该 Component）：

```rust
use crate::systems::{
    HarnessSet, agent_started_hook_system, agent_stopped_hook_system, approval_dispatch_system,
    approval_result_system, brain_decision_system, brain_dispatch_system, dispatch_system,
    evaluation_trigger_system, on_approval_requested_hook_system, on_approval_resolved_hook_system,
    on_message_dispatched_hook_system, task_dispatch_system, tool_confirmation_result_system,
    workitem_dispatch_system, workitem_lifecycle_hook_system,
};
```

在 `app.add_systems` 块中添加（放在 `workitem_dispatch_system` 之后）：

```rust
                // 统一派发系统（阶段 2：与旧 system 并存）
                dispatch_system
                    .in_set(HarnessSet::Dispatch)
                    .after(workitem_dispatch_system),
```

- [ ] **步骤 7：运行编译验证**

运行：`cargo build --lib`
预期：编译通过。可能有 unused warning（部分函数未被调用），可暂时 `#[allow(dead_code)]` 标注。

- [ ] **步骤 8：运行测试验证不破坏现有行为**

运行：`cargo test --all-features`
预期：所有现有测试 PASS（dispatch_system 不处理任何 Entity，因为没有 Entity 带 PendingDispatch）

- [ ] **步骤 9：Commit**

```bash
git add src/systems/dispatch/dispatch_system.rs src/systems/dispatch/mod.rs src/systems/mod.rs src/plugins/dispatch.rs tests/dispatch_phase2.rs
git commit -m "feat(dispatch): add unified dispatch_system skeleton

Create dispatch_system that scans PendingDispatch Component on Task/WorkItem
entities. Currently coexists with legacy systems (task_dispatch_system,
workitem_dispatch_system, brain_dispatch_system) without behavior change,
as no entities carry PendingDispatch yet.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §3.4"
```

---

### 任务 2.3：创建 `subtask_dispatch_preparation.rs`

**文件：**
- 创建：`src/systems/dispatch/subtask_dispatch_preparation.rs`
- 修改：`src/systems/dispatch/mod.rs`、`src/systems/mod.rs`、`src/plugins/dispatch.rs`

**说明：** 创建 SubTask 派发前置 system，扫描带 `SubTaskConfig` 的 Task，检查 DAG 依赖，准备 spawn spec，附加 `PendingDispatch`。本阶段与旧 `brain_dispatch.rs` 的 SubTask 处理并存。

- [ ] **步骤 1：创建 `src/systems/dispatch/subtask_dispatch_preparation.rs`**

```rust
//! SubTask 派发前置系统
//!
//! 扫描带 `SubTaskConfig` 的 Task，执行派发前置条件检查：
//! - DAG 依赖检查
//! - 兄弟任务结果收集（注入到 task content）
//! - AgentSpawnSpec 准备
//!
//! 准备完成后附加 `PendingDispatch` Component，由 `dispatch_system` 接管派发决策。

use crate::prelude::*;
use tracing::{debug, trace};

use crate::{
    app::Clock,
    domain::{
        AgentSpawnSpec, BatchTaskState, DispatchHint, DispatchKind, DispatchStrategy,
        PendingDispatch, SubTaskBatchState, SubTaskConfig, Task, TaskStatus,
    },
};

/// SubTask 派发前置系统
///
/// 在 `HarnessSet::Dispatch` 之前运行（建议放入 `HarnessSet::Transform` 或新增 set）。
/// 当前实现放在 `HarnessSet::Dispatch` 中，通过 `.before(dispatch_system)` 保证顺序。
pub fn subtask_dispatch_preparation_system(
    _clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(
        Entity,
        &mut Task,
        &SubTaskConfig,
        Option<&PendingDispatch>,
    )>,
    batch_states: Query<&SubTaskBatchState>,
) {
    for (entity, mut task, config, pending) in tasks.iter_mut() {
        // 已有 PendingDispatch 的跳过
        if pending.is_some() {
            continue;
        }

        // 只处理 Ready / Pending 状态
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 已有 delegate 的跳过
        if task.delegate.is_some() {
            continue;
        }

        // 1. DAG 依赖检查
        let deps_satisfied = if config.depends_on.is_empty() {
            true
        } else if let Some(batch_state) = batch_states
            .iter()
            .find(|bs| bs.batch_id == config.batch_id)
        {
            config.depends_on.iter().all(|dep_name| {
                batch_state.tasks.get(dep_name).is_some_and(|s| {
                    matches!(s.state, BatchTaskState::Done | BatchTaskState::Failed)
                })
            })
        } else {
            false
        };

        if !deps_satisfied {
            trace!(
                event = "SubTaskWaitingForDependencies",
                task_id = %task.id,
                child_name = %config.child_agent_name,
                depends_on = ?config.depends_on,
                "sub-task waiting for dependencies to complete"
            );
            continue;
        }

        // 2. 收集兄弟任务结果（注入到 task content）
        let sibling_results = if !config.depends_on.is_empty() {
            if let Some(batch_state) = batch_states
                .iter()
                .find(|bs| bs.batch_id == config.batch_id)
            {
                let mut results = Vec::new();
                for dep_name in &config.depends_on {
                    if let Some(status) = batch_state.tasks.get(dep_name) {
                        let result_text = match &status.result_summary {
                            Some(summary) if !summary.is_empty() => summary.clone(),
                            _ => format!("[{}: 执行失败，无结果]", dep_name),
                        };
                        results.push(format!("### {}\n{}", dep_name, result_text));
                    }
                }
                if results.is_empty() {
                    None
                } else {
                    Some(results)
                }
            } else {
                None
            }
        } else {
            None
        };

        // 3. 注入兄弟任务结果到 task content
        if let Some(results) = &sibling_results {
            task.content = format!(
                "{}\n\n## 兄弟任务结果\n\n{}\n\n请基于以上兄弟任务的结果完成你的任务。你可以直接引用这些结果，无需重新计算或搜索。",
                task.content,
                results.join("\n\n")
            );
        }

        // 4. 准备 AgentSpawnSpec
        let spawn_spec = AgentSpawnSpec {
            name: config.child_agent_name.clone(),
            model: config.child_agent_model.clone(),
            allowed_tools: config.allowed_tools.clone(),
            parent_agent_id: Some(config.parent_agent_id),
        };

        // 5. 附加 PendingDispatch（走 BrainLlm 策略）
        commands.entity(entity).insert(PendingDispatch {
            kind: DispatchKind::Task,
            hint: DispatchHint {
                strategy: DispatchStrategy::BrainLlm,
                preferred_agent_name: None,
                required_skill_id: None,
                agent_spawn_spec: Some(spawn_spec),
            },
        });

        debug!(
            event = "SubTaskDispatchPrepared",
            task_id = %task.id,
            child_name = %config.child_agent_name,
            batch_id = %config.batch_id,
            has_sibling_results = sibling_results.is_some(),
            "sub-task prepared for dispatch"
        );
    }
}
```

- [ ] **步骤 2：在 `src/systems/dispatch/mod.rs` 中注册模块**

修改 `src/systems/dispatch/mod.rs`，添加：

```rust
mod subtask_dispatch_preparation;
pub(crate) use subtask_dispatch_preparation::subtask_dispatch_preparation_system;
```

- [ ] **步骤 3：在 `src/systems/mod.rs` 中导出**

修改 `src/systems/mod.rs` 的 `pub(crate) use dispatch::{...}` 块：

```rust
pub(crate) use dispatch::{
    agent_started_hook_system, agent_stopped_hook_system, brain_dispatch_system,
    dispatch_system, on_message_dispatched_hook_system, subtask_dispatch_preparation_system,
    task_dispatch_system, workitem_dispatch_system, workitem_lifecycle_hook_system,
};
```

- [ ] **步骤 4：在 `src/plugins/dispatch.rs` 中注册 system**

修改 `src/plugins/dispatch.rs`，在 `app.add_systems` 块中添加：

```rust
                // SubTask 派发前置系统（阶段 2：与旧 system 并存）
                subtask_dispatch_preparation_system
                    .in_set(HarnessSet::Dispatch)
                    .before(dispatch_system),
```

- [ ] **步骤 5：运行编译验证**

运行：`cargo build --lib`
预期：编译通过

- [ ] **步骤 6：运行测试验证不破坏现有行为**

运行：`cargo test --all-features`
预期：所有现有测试 PASS

**重要注意：** 此时 `subtask_dispatch_preparation_system` 会为 SubTask 附加 `PendingDispatch`，但 `brain_dispatch.rs` 仍在处理 SubTask。这可能导致重复派发。需要暂时在 `brain_dispatch.rs` 的 SubTask 分支添加跳过逻辑：

修改 `src/systems/dispatch/brain_dispatch.rs` 第 222 行附近，在 `if let Some(config) = sub_task_config {` 之后添加：

```rust
        // 阶段 2 临时跳过：已有 PendingDispatch 的 SubTask 由 dispatch_system 处理
        if let Some(_) = pending_dispatch_opt {
            continue;
        }
```

但这需要 `brain_dispatch_system` 的 query 添加 `Option<&PendingDispatch>`。为简化阶段 2，可以先不启用 `subtask_dispatch_preparation_system`（注释掉注册），等阶段 3 迁移时再启用。

**简化方案：** 阶段 2 暂不注册 `subtask_dispatch_preparation_system`，只创建文件。在 `src/plugins/dispatch.rs` 中注释掉注册代码：

```rust
                // SubTask 派发前置系统（阶段 3 启用）
                // subtask_dispatch_preparation_system
                //     .in_set(HarnessSet::Dispatch)
                //     .before(dispatch_system),
```

- [ ] **步骤 7：Commit**

```bash
git add src/systems/dispatch/subtask_dispatch_preparation.rs src/systems/dispatch/mod.rs src/systems/mod.rs src/plugins/dispatch.rs
git commit -m "feat(dispatch): add subtask_dispatch_preparation_system

Create preparation system for SubTask dispatch: DAG dependency check,
sibling results collection, AgentSpawnSpec preparation. Attaches
PendingDispatch Component for dispatch_system to consume.

Currently not registered (commented out) - will be enabled in phase 3
when SubTask dispatch migrates from brain_dispatch_system.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §3.6"
```

---

## 阶段 3：Task 派发迁移

### 任务 3.1：改造 `brain_decision_system` 接入 `parse_brain_skill_selection`

**文件：**
- 修改：`src/systems/transform/brain_decision.rs`
- 测试：`tests/dispatch_phase3.rs`

**说明：** `brain_decision_system` 处理 Brain LLM 输出时，调用 `parse_brain_skill_selection` 解析 skill，产出 `PendingDispatch + DirectDelegate` 而非直接 spawn `AgentExecutionRequestMessage`。移除 fallback 到第一个非 brain Persistent Agent 的逻辑。

- [ ] **步骤 1：编写失败的集成测试**

创建 `tests/dispatch_phase3.rs`：

```rust
//! 阶段 3 集成测试：Task 派发迁移
//!
//! 验证 brain_decision_system 解析 Brain 输出后产出 PendingDispatch(DirectDelegate)，
//! 而非直接 spawn AgentExecutionRequestMessage。

// 具体测试用例在实施时填充，骨架先创建

#[test]
fn placeholder() {}
```

- [ ] **步骤 2：修改 `brain_decision_system`**

修改 `src/systems/transform/brain_decision.rs`：

1. 在 import 区添加：

```rust
use crate::domain::{
    Agent, AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
    AgentExecutionResultMessage, AgentKind, AgentRequestKind, BrainDecisionError, FailureReason,
    MessageDispatchedHookPending, OutputContent, PendingDispatch, AwaitingBrainDecision,
    DispatchHint, DispatchKind, DispatchStrategy, Task, TaskStatus, ToolDefinition,
    WaitingReason,
};
use crate::systems::dispatch::brain_dispatch::parse_brain_skill_selection;
```

2. 修改 query 添加 `AwaitingBrainDecision`：

```rust
pub fn brain_decision_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut Task, Option<&AwaitingBrainDecision>)>,
    agents: Query<&Agent>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
```

注意：移除 `registry: Res<crate::domain::SpaceToolRegistry>` 参数（不再直接 spawn 执行请求，由 `dispatch_system` 处理）。

3. 修改处理逻辑：

```rust
    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::BrainDecision {
            continue;
        }

        let result = &result_message.result;

        let Some((task_entity, mut task, awaiting)) =
            tasks.iter_mut().find(|(_, t, _)| t.id == result.task_id)
        else {
            commands.entity(entity).despawn();
            continue;
        };

        match &result.result {
            Ok(AgentExecutionOutput {
                content: OutputContent::Text(content),
                ..
            }) => {
                // 同时解析 agent_name 和 skill_name
                match parse_brain_skill_selection(content) {
                    Ok((agent_name, skill_name)) => {
                        let agent_exists = agents
                            .iter()
                            .any(|a| a.profile.name == agent_name && a.kind == AgentKind::Persistent);

                        if !agent_exists {
                            // Brain 选了不存在的 Agent，直接 Failed（不 fallback）
                            task.last_error = Some(format!(
                                "brain selected agent '{}' but no such persistent agent",
                                agent_name
                            ));
                            task.status = TaskStatus::Failed(FailureReason::AgentError);
                            task.updated_at = clock.0;
                            commands.entity(task_entity).remove::<AwaitingBrainDecision>();
                            commands.entity(entity).despawn();
                            continue;
                        }

                        // 解析 skill_id（如有）
                        let skill_id = if let Some(skill_name) = skill_name {
                            // 通过 SkillRegistry 查找 skill_id
                            // 注意：brain_decision_system 需要 SkillRegistry 资源
                            // 暂时通过 skill_name 作为 SkillId 的字符串形式
                            Some(crate::infrastructure::skills::SkillId::from_string(skill_name))
                        } else {
                            None
                        };

                        // 携带原 awaiting 的 spawn_spec
                        let spawn_spec = awaiting.and_then(|a| a.spawn_spec.clone());

                        // 移除 AwaitingBrainDecision，加 PendingDispatch + DirectDelegate
                        commands.entity(task_entity).remove::<AwaitingBrainDecision>();
                        commands.entity(task_entity).insert(PendingDispatch {
                            kind: DispatchKind::Task,
                            hint: DispatchHint {
                                strategy: DispatchStrategy::DirectDelegate,
                                preferred_agent_name: Some(agent_name),
                                required_skill_id: skill_id,
                                agent_spawn_spec: spawn_spec,
                            },
                        });

                        debug!(
                            event = "BrainDecisionResolved",
                            task_id = %task.id,
                            selected_agent = %agent_name,
                            has_skill = skill_id.is_some(),
                            "brain decision resolved, task re-queued for direct dispatch"
                        );
                    }
                    Err(e) => {
                        // 解析失败，直接 Failed
                        task.last_error = Some(format!("brain skill selection parse failed: {:?}", e));
                        task.status = TaskStatus::Failed(FailureReason::AgentError);
                        task.updated_at = clock.0;
                        commands.entity(task_entity).remove::<AwaitingBrainDecision>();
                    }
                }
            }
            Ok(AgentExecutionOutput {
                content: OutputContent::ToolCalls(_),
                ..
            }) => {
                // Tool calls 由 llm_response_system 处理，跳过
                continue;
            }
            Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                task.schedule_retry(error, clock.0);
            }
            Err(error) => {
                task.mark_failed(error, clock.0);
                commands.entity(task_entity).remove::<AwaitingBrainDecision>();
            }
        }

        commands.entity(entity).despawn();
    }
```

4. 移除 `build_tools_for_agent` 和 `augment_delegate_prompt` 函数（不再需要，由 `dispatch_system` 处理）。

5. 移除 `parse_brain_decision` 的 import（不再使用）。

6. 在 `brain_dispatch.rs` 中将 `parse_brain_skill_selection` 的 `#[allow(dead_code)]` 移除，并改为 `pub(crate)`：

修改 `src/systems/dispatch/brain_dispatch.rs` 第 431 行：

```rust
pub(crate) fn parse_brain_skill_selection(
    raw: &str,
) -> Result<(String, Option<String>), BrainSkillSelectionError> {
```

同时移除 `BrainSkillSelection` 和 `BrainSkillSelectionError` 的 `#[allow(dead_code)]`。

- [ ] **步骤 3：确认 `SkillId::from_string` 方法存在**

运行：`grep -r "impl SkillId" src/`

如果 `SkillId` 没有 `from_string` 方法，需要在 `src/infrastructure/skills/` 中添加。先检查现有实现。

- [ ] **步骤 4：运行编译验证**

运行：`cargo build --lib`
预期：编译通过。修复所有编译错误。

- [ ] **步骤 5：运行测试**

运行：`cargo test --all-features`
预期：可能有部分测试失败（依赖 Brain fallback 行为的测试）。修正这些测试以反映新的"Brain 失败即 Failed"语义。

- [ ] **步骤 6：Commit**

```bash
git add src/systems/transform/brain_decision.rs src/systems/dispatch/brain_dispatch.rs tests/dispatch_phase3.rs
git commit -m "refactor(dispatch): brain_decision_system produces PendingDispatch

Brain decision system now parses both agent_name and skill_name from
Brain LLM output via parse_brain_skill_selection. Instead of directly
spawning AgentExecutionRequestMessage, it attaches PendingDispatch with
DirectDelegate strategy, letting dispatch_system handle the actual dispatch.

Removes fallback to first non-brain Persistent Agent - Brain failure
now results in Task Failed (semantic honesty per AGENTS.md).

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.4 决策 8, §2.3 决策 7"
```

---

### 任务 3.2：TopLevelTask 创建入口附加 PendingDispatch

**文件：**
- 修改：`src/systems/transform/user_message_to_task_system.rs`、`src/systems/transform/trigger_task_routing_system.rs`、`src/systems/transform/sub_task_batch_block_system.rs`（或其他 Task 创建入口）

**说明：** 所有 TopLevelTask 创建入口在 spawn Task Entity 时附加 `PendingDispatch + BrainLlm`。SubTask 由 `subtask_dispatch_preparation_system` 接管，不再立即派发。

- [ ] **步骤 1：定位所有 Task 创建入口**

运行：`grep -rn "commands.spawn.*Task" src/`

记录所有 spawn Task Entity 的位置。

- [ ] **步骤 2：修改 `user_message_to_task_system`**

在 spawn Task Entity 时附加 `PendingDispatch`：

```rust
commands.spawn((
    Task { ... },
    PendingDispatch {
        kind: DispatchKind::Task,
        hint: DispatchHint {
            strategy: DispatchStrategy::BrainLlm,
            preferred_agent_name: None,
            required_skill_id: None,
            agent_spawn_spec: None,
        },
    },
));
```

- [ ] **步骤 3：类似修改其他 Task 创建入口**

对所有 spawn TopLevelTask 的入口应用相同改动。

- [ ] **步骤 4：启用 `subtask_dispatch_preparation_system`**

修改 `src/plugins/dispatch.rs`，取消注释：

```rust
                subtask_dispatch_preparation_system
                    .in_set(HarnessSet::Dispatch)
                    .before(dispatch_system),
```

- [ ] **步骤 5：修改 `brain_dispatch.rs` 跳过带 PendingDispatch 的 Task**

修改 `src/systems/dispatch/brain_dispatch.rs` 的 query，添加 `Option<&PendingDispatch>`：

```rust
pub fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&ShortTermMemory>,
        Option<&SubTaskConfig>,
        Option<&PendingDispatch>,
    )>,
    agents: Query<&Agent>,
    batch_states: Query<&SubTaskBatchState>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,
) {
    // ...
    for (task_entity, mut task, short_term, sub_task_config, pending_dispatch) in &mut tasks {
        // 阶段 3：带 PendingDispatch 的 Task 由 dispatch_system 处理，跳过
        if pending_dispatch.is_some() {
            continue;
        }
        // ... 原有逻辑
    }
}
```

- [ ] **步骤 6：修改 `task_dispatch.rs` 跳过带 PendingDispatch 的 Task**

类似地在 `task_dispatch_system` 中添加跳过逻辑。

- [ ] **步骤 7：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features`
预期：编译通过，TopLevelTask 走新 `dispatch_system`，SubTask 走 `subtask_dispatch_preparation_system` + `dispatch_system`。

- [ ] **步骤 8：Commit**

```bash
git add src/systems/transform/ src/systems/dispatch/brain_dispatch.rs src/systems/dispatch/task_dispatch.rs src/plugins/dispatch.rs
git commit -m "feat(dispatch): TopLevelTask and SubTask migrate to dispatch_system

TopLevelTask creation entries attach PendingDispatch(BrainLlm) at spawn.
SubTask dispatch preparation_system enabled - handles DAG check and
attaches PendingDispatch(BrainLlm) with AgentSpawnSpec.

Legacy brain_dispatch_system and task_dispatch_system skip entities
with PendingDispatch (coexistence during migration).

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.5"
```

---

## 阶段 4：WorkItem 派发迁移

### 任务 4.1：`skill_update_workitem_system` 剥离直接派发逻辑

**文件：**
- 修改：`src/systems/experience/skill_update.rs:152-295`

**说明：** `skill_update_workitem_system` 不再直接 spawn `AgentExecutionRequestMessage`，改为附加 `PendingDispatch`。

- [ ] **步骤 1：修改 `skill_update_workitem_system`**

将 [skill_update.rs:267-293](src/systems/experience/skill_update.rs) 的 spawn 逻辑改为：

```rust
        // 6. 创建 WorkItem 并附加 PendingDispatch
        let work_item = WorkItem::skill_update(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.governing_agent_id,
        );
        // 若 Agent 配置了 system_prompt（来自 agents.toml），覆盖 WorkItem 的默认 system_prompt
        if let Some(agent_system_prompt) = skill_updater.and_then(|a| a.system_prompt.as_ref()) {
            work_item.input.context.system_prompt = Some(agent_system_prompt.clone());
        }

        debug!(
            event = "SkillUpdateWorkItemCreated",
            task_id = %request.task_id,
            skill_id = %request.skill_id.as_string(),
            base_version = skill_entry.version,
            "spawning skill update work item with PendingDispatch"
        );

        commands.spawn((
            work_item,
            SkillUpdateContext {
                skill_id: request.skill_id.clone(),
                base_version: skill_entry.version,
                experience_candidate_id: request.experience_candidate_id,
                governing_agent_id: request.governing_agent_id,
            },
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::SkillUpdate),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));
        commands.entity(entity).despawn();
```

注意：移除了 `work_item.assign(skill_updater_id)` 和 `work_item.start()`——这些由 `dispatch_system` 处理。也移除了直接 spawn `AgentExecutionRequestMessage`。

同时移除了 `WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted)`——由 `dispatch_system` 在派发成功时附加。

- [ ] **步骤 2：移除不再需要的 Agent 查找逻辑**

由于不再在创建器中查找 Agent（由 `dispatch_system` 按 `required_tag()` 查找），可以移除 [skill_update.rs:160-179](src/systems/experience/skill_update.rs) 的 `skill_updater` 查找逻辑：

```rust
pub(crate) fn skill_update_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &SkillUpdateRequestMessage)>,
    store: Res<ExperienceStore>,
    skill_registry: Res<SkillRegistry>,
    // 移除 agents: Query<&Agent> 和 registry: Res<SpaceToolRegistry>
) {
    for (entity, request) in &requests {
        // 1. 从 SkillRegistry 取 skill 内容
        let Some(skill_entry) = skill_registry.get(&request.skill_id) else {
            // ... 原有错误处理
        };

        // 2. 从 ExperienceStore 取候选原文
        let Some(candidate) = store.candidates.get(&request.experience_candidate_id) else {
            // ... 原有错误处理
        };

        // 3. 构造 prompt
        let prompt = format!(...);

        // 4. 从 registry 过滤工具，仅保留 submit_skill_update
        // 注意：需要保留 registry 参数用于工具过滤
        // ...

        // 5. 创建 WorkItem + PendingDispatch
        // ...
    }
}
```

注意：`registry: Res<SpaceToolRegistry>` 仍需保留用于工具过滤。

- [ ] **步骤 3：运行编译验证**

运行：`cargo build --lib`
预期：编译通过

- [ ] **步骤 4：运行测试**

运行：`cargo test --all-features`
预期：SkillUpdate 相关测试可能需要调整——验证 WorkItem 创建但不立即派发。

- [ ] **步骤 5：Commit**

```bash
git add src/systems/experience/skill_update.rs
git commit -m "refactor(dispatch): skill_update_workitem_system becomes pure creator

skill_update_workitem_system no longer finds Agent or spawns
AgentExecutionRequestMessage directly. It only creates WorkItem with
SkillUpdateContext and attaches PendingDispatch(WorkItem(SkillUpdate)).
dispatch_system handles Agent lookup via required_tag() and execution
request spawning.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.6 决策 12"
```

---

### 任务 4.2：`profile_generation_workitem_system` 剥离直接派发逻辑

**文件：**
- 修改：`src/systems/experience/profile_generation.rs`

**说明：** 类似任务 4.1，剥离直接派发，附加 `PendingDispatch`。同时将 `ProfileGenerationContext` 从 `ExperienceStore` 迁移到 Entity Component。

- [ ] **步骤 1：将 `ProfileGenerationContext` 改为 Component**

修改 `src/domain/contribution.rs`，为 `ProfileGenerationContext` 添加 `#[derive(Component)]`：

```rust
#[derive(Debug, Clone, bevy_ecs::prelude::Component)]
pub struct ProfileGenerationContext {
    pub kind: ProfileGenerationKind,
    pub exception_count: usize,
    pub existing_profile: Option<ExistingAgentProfile>,
    pub generated_profile: Option<GeneratedProfile>,
}
```

- [ ] **步骤 2：修改 `profile_generation_workitem_system`**

移除 Agent 查找和直接 spawn `AgentExecutionRequestMessage`，改为附加 `PendingDispatch`：

```rust
pub(crate) fn profile_generation_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &ProfileGenerationRequestMessage)>,
    mut store: ResMut<ExperienceStore>,
    mut pending_hooks: ResMut<PendingExperienceHooks>,
    registry: Res<SpaceToolRegistry>,
) {
    for (entity, request) in &requests {
        // 1. 构建 prompt
        let prompt = build_profile_generation_prompt(request, &store, &agents);
        // 注意：build_profile_generation_prompt 可能需要 agents 参数，需要调整签名

        // 2. 收集工具定义
        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| {
                tool.name == "submit_profile_update" || tool.name == "skip_profile_update"
            })
            .cloned()
            .collect();

        // 3. 构建 conversation
        let conversation = Vec::new();

        // 4. 创建 WorkItem + ProfileGenerationContext + PendingDispatch
        let work_item = WorkItem::profile_generation(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.agent_id,
            request.kind.clone(),
        );

        let context = ProfileGenerationContext {
            kind: request.kind.clone(),
            exception_count: request.exception_count,
            existing_profile: request.existing_profile.clone(),
            generated_profile: None,
        };

        commands.spawn((
            work_item,
            context,
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::ProfileGeneration),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));

        // 移除 store.profile_generation_context.insert(...) - 改为 Entity Component
        // 同时需要更新所有读取 ProfileGenerationContext 的地方

        commands.entity(entity).despawn();
    }
}
```

- [ ] **步骤 3：更新所有读取 `ProfileGenerationContext` 的地方**

运行：`grep -rn "profile_generation_context" src/`

将所有从 `ExperienceStore.profile_generation_context` 读取的代码改为从 Entity Component 读取。

主要涉及：
- `profile_generation_completion_system`（消费 WorkItem 完成事件）
- `handle_profile_designer_missing`（失败处理）

- [ ] **步骤 4：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features`
预期：编译通过，ProfileGeneration 相关测试调整。

- [ ] **步骤 5：Commit**

```bash
git add src/domain/contribution.rs src/systems/experience/profile_generation.rs src/systems/experience/
git commit -m "refactor(dispatch): profile_generation_workitem_system becomes pure creator

profile_generation_workitem_system no longer finds Agent or spawns
AgentExecutionRequestMessage directly. ProfileGenerationContext migrated
from ExperienceStore to Entity Component. dispatch_system handles
Agent lookup via required_tag().

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.6 决策 12"
```

---

### 任务 4.3：`experience_collection_workitem_system` 剥离直接派发逻辑

**文件：**
- 修改：`src/systems/experience/collection.rs:55-114`

**说明：** `experience_collection_workitem_system` 当前只 spawn WorkItem，不 spawn `AgentExecutionRequestMessage`（由 `workitem_dispatch_system` 处理）。但 `workitem_dispatch_system` 将在阶段 5 删除，需要改为附加 `PendingDispatch`。

- [ ] **步骤 1：修改 `experience_collection_workitem_system`**

在 [collection.rs:111](src/systems/experience/collection.rs) 的 `commands.spawn(work_item)` 改为：

```rust
        commands.spawn((
            work_item,
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::ExperienceCollection),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));
        commands.entity(entity).despawn();
```

- [ ] **步骤 2：修改 `workitem_dispatch_system` 跳过带 PendingDispatch 的 WorkItem**

修改 `src/systems/dispatch/workitem_dispatch.rs`，添加 `Option<&PendingDispatch>` query 并跳过：

```rust
pub(crate) fn workitem_dispatch_system(
    clock: Res<crate::app::Clock>,
    mut commands: Commands,
    config: Res<TaskEvaluationConfig>,
    agents: Query<&Agent>,
    mut tasks: Query<&mut Task>,
    mut work_items: Query<(Entity, &mut WorkItem, Option<&PendingDispatch>)>,
) {
    for (_entity, mut work_item, pending_dispatch) in &mut work_items {
        // 阶段 4：带 PendingDispatch 的 WorkItem 由 dispatch_system 处理
        if pending_dispatch.is_some() {
            continue;
        }
        // ... 原有逻辑
    }
}
```

- [ ] **步骤 3：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features`
预期：ExperienceCollection WorkItem 走新 `dispatch_system`。

- [ ] **步骤 4：Commit**

```bash
git add src/systems/experience/collection.rs src/systems/dispatch/workitem_dispatch.rs
git commit -m "refactor(dispatch): experience_collection_workitem_system attaches PendingDispatch

ExperienceCollection WorkItem now carries PendingDispatch, handled by
dispatch_system instead of workitem_dispatch_system. Legacy
workitem_dispatch_system skips WorkItems with PendingDispatch.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.6 决策 12"
```

---

### 任务 4.4：修改 `workitem_lifecycle_hook_system` 按 Context 分流失败处理

**文件：**
- 修改：`src/systems/dispatch/workitem_lifecycle_hook.rs`

**说明：** 当 WorkItem 派发失败时（`OnWorkItemFailed` hook），按 Context Component 分流特化逻辑。

- [ ] **步骤 1：修改 `dispatch_workitem_lifecycle_hook` 函数**

在 `workitem_lifecycle_hook.rs` 的 `dispatch_workitem_lifecycle_hook` 函数中，针对 `HookPoint::OnWorkItemFailed` 添加 Context 分流逻辑：

```rust
fn dispatch_workitem_lifecycle_hook(
    world: &mut World,
    registry: &mut PluginRegistry,
    work_item: &WorkItem,
    point: HookPoint,
) {
    // ... 原有 hook 派发逻辑 ...

    // 失败特化处理：按 Context Component 分流
    if point == HookPoint::OnWorkItemFailed {
        handle_workitem_failure_by_context(world, work_item);
    }
}

/// 按 Context Component 分流 WorkItem 失败处理
fn handle_workitem_failure_by_context(world: &mut World, work_item: &WorkItem) {
    use crate::domain::{SkillUpdateContext, ProfileGenerationContext};

    let entity = world
        .query_filtered::<Entity, ()>()
        .iter(world)
        .find(|e| {
            world
                .get::<WorkItem>(*e)
                .map(|wi| wi.id == work_item.id)
                .unwrap_or(false)
        });

    let Some(entity) = entity else {
        return;
    };

    // SkillUpdateContext: 候选保持 GovernanceResolved
    if world.get::<SkillUpdateContext>(entity).is_some() {
        debug!(
            event = "SkillUpdateWorkItemFailedContext",
            work_item_id = %work_item.id,
            "skill update work item failed, candidate remains GovernanceResolved"
        );
        // 候选状态由 governance system 后续处理
    }
    // ProfileGenerationContext: 调用 handle_profile_designer_missing 逻辑
    else if let Some(ctx) = world.get::<ProfileGenerationContext>(entity).cloned() {
        debug!(
            event = "ProfileGenerationWorkItemFailedContext",
            work_item_id = %work_item.id,
            "profile generation work item failed, invoking missing handler"
        );
        // 迁移 handle_profile_designer_missing 逻辑
        // ...
    }
    // Evaluation / Summarization: 恢复 Task 状态
    else {
        // 原 workitem_dispatch.rs:71-105 的 Task 状态恢复逻辑
        // 迁移到此处
    }
}
```

注意：实际实现需要将 [workitem_dispatch.rs:71-105](src/systems/dispatch/workitem_dispatch.rs) 的 Task 状态恢复逻辑迁移过来。

- [ ] **步骤 2：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features`

- [ ] **步骤 3：Commit**

```bash
git add src/systems/dispatch/workitem_lifecycle_hook.rs
git commit -m "feat(dispatch): context-based WorkItem failure handling

workitem_lifecycle_hook_system now dispatches failure handling based
on Context Component: SkillUpdateContext keeps candidate GovernanceResolved,
ProfileGenerationContext invokes missing handler, Evaluation/Summarization
restores Task status.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.7 决策 14"
```

---

## 阶段 5：清理与简化

### 任务 5.1：删除 `task_dispatch.rs` 和 `workitem_dispatch.rs`

**文件：**
- 删除：`src/systems/dispatch/task_dispatch.rs`、`src/systems/dispatch/workitem_dispatch.rs`
- 修改：`src/systems/dispatch/mod.rs`、`src/systems/mod.rs`、`src/plugins/dispatch.rs`

**说明：** 所有 Task 和 WorkItem 派发都已迁移到 `dispatch_system`，删除旧 system。

- [ ] **步骤 1：删除文件**

```bash
rm src/systems/dispatch/task_dispatch.rs
rm src/systems/dispatch/workitem_dispatch.rs
```

- [ ] **步骤 2：更新 `src/systems/dispatch/mod.rs`**

移除 `task_dispatch` 和 `workitem_dispatch` 的 mod 声明和 pub use：

```rust
//! Dispatch 模块
//!
//! 包含任务分发和 Agent 选择相关的 System。

mod agent_lifecycle_hook;
mod brain_dispatch;
mod brain_llm_builder;
mod dispatch_system;
mod memory_selection;
mod message_dispatched_hook;
mod subtask_dispatch_preparation;
mod workitem_lifecycle_hook;

pub(crate) use agent_lifecycle_hook::{agent_started_hook_system, agent_stopped_hook_system};
pub use brain_dispatch::brain_dispatch_system;
pub(crate) use brain_llm_builder::build_brain_execution_request;
pub(crate) use dispatch_system::dispatch_system;
pub(crate) use message_dispatched_hook::on_message_dispatched_hook_system;
pub(crate) use subtask_dispatch_preparation::subtask_dispatch_preparation_system;
pub(crate) use workitem_lifecycle_hook::workitem_lifecycle_hook_system;
```

- [ ] **步骤 3：更新 `src/systems/mod.rs`**

移除 `task_dispatch_system` 和 `workitem_dispatch_system` 的导出：

```rust
pub(crate) use dispatch::{
    agent_started_hook_system, agent_stopped_hook_system, brain_dispatch_system,
    dispatch_system, on_message_dispatched_hook_system, subtask_dispatch_preparation_system,
    workitem_lifecycle_hook_system,
};
```

- [ ] **步骤 4：更新 `src/plugins/dispatch.rs`**

移除 `task_dispatch_system` 和 `workitem_dispatch_system` 的注册：

```rust
use crate::systems::{
    HarnessSet, agent_started_hook_system, agent_stopped_hook_system, approval_dispatch_system,
    approval_result_system, brain_decision_system, brain_dispatch_system, dispatch_system,
    evaluation_trigger_system, on_approval_requested_hook_system, on_approval_resolved_hook_system,
    on_message_dispatched_hook_system, subtask_dispatch_preparation_system,
    tool_confirmation_result_system, workitem_lifecycle_hook_system,
};

impl Plugin for DispatchPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                brain_decision_system
                    .in_set(HarnessSet::Transform)
                    .after(crate::systems::ingest_execution_results_system),
                brain_dispatch_system
                    .in_set(HarnessSet::Dispatch)
                    .before(dispatch_system),
                subtask_dispatch_preparation_system
                    .in_set(HarnessSet::Dispatch)
                    .before(dispatch_system),
                dispatch_system.in_set(HarnessSet::Dispatch),
                workitem_lifecycle_hook_system
                    .in_set(HarnessSet::Dispatch)
                    .after(dispatch_system),
                on_message_dispatched_hook_system
                    .in_set(HarnessSet::Dispatch)
                    .after(dispatch_system),
                agent_started_hook_system.in_set(HarnessSet::Maintenance),
                agent_stopped_hook_system
                    .in_set(HarnessSet::Maintenance)
                    .after(crate::systems::agent_factory_system),
                evaluation_trigger_system.in_set(HarnessSet::Dispatch),
                approval_dispatch_system.in_set(HarnessSet::Dispatch),
                on_approval_requested_hook_system
                    .in_set(HarnessSet::Dispatch)
                    .after(approval_dispatch_system),
                approval_result_system.in_set(HarnessSet::Transform),
                on_approval_resolved_hook_system
                    .in_set(HarnessSet::Transform)
                    .after(approval_result_system),
                tool_confirmation_result_system
                    .in_set(HarnessSet::Dispatch)
                    .after(crate::systems::tool_dispatch_system),
            ),
        );
    }
}
```

注意：`brain_dispatch_system` 暂时保留（可能有残留逻辑），但放在 `dispatch_system` 之前。如果 `brain_dispatch.rs` 已无任何逻辑，可直接删除。需要检查。

- [ ] **步骤 5：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features`
预期：编译通过，所有测试 PASS。

- [ ] **步骤 6：Commit**

```bash
git add -A
git commit -m "refactor(dispatch): remove legacy task_dispatch and workitem_dispatch

All Task and WorkItem dispatch now handled by unified dispatch_system.
Legacy task_dispatch_system and workitem_dispatch_system removed.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §4.1"
```

---

### 任务 5.2：删除 `agent_selection.rs` 和清理 `contracts/dispatch.rs`

**文件：**
- 删除：`src/systems/dispatch/agent_selection.rs`
- 修改：`src/contracts/dispatch.rs`、`src/systems/dispatch/mod.rs`、`src/systems/dispatch/brain_dispatch.rs`

**说明：** 删除重复的 agent 选择函数和未使用的 trait 体系。

- [ ] **步骤 1：删除 `agent_selection.rs`**

```bash
rm src/systems/dispatch/agent_selection.rs
```

- [ ] **步骤 2：更新 `src/systems/dispatch/mod.rs`**

移除 `agent_selection` 的 mod 声明。

- [ ] **步骤 3：更新 `src/systems/dispatch/brain_dispatch.rs`**

移除对 `select_agent_for_sub_task_with_skill` 的 import 和调用（如果还有残留）。

- [ ] **步骤 4：清理 `src/contracts/dispatch.rs`**

删除以下未使用的 trait 和类型：
- `TagMatcher` trait
- `AgentSelector` trait
- `DispatchPolicy` trait
- `TagBasedSelector`
- `DefaultDispatchPolicy`
- `SummarizerSelectionPolicy` trait 及其实现

保留：
- `BrainSelectionPolicy` trait + `FirstBrainPolicy`
- `AgentCapabilitySummary`

- [ ] **步骤 5：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features && cargo clippy --all-targets --all-features -- -D warnings`
预期：编译通过，无 warning。

- [ ] **步骤 6：Commit**

```bash
git add -A
git commit -m "refactor(dispatch): remove agent_selection.rs and unused traits

Delete agent_selection.rs (duplicate logic now in dispatch_system).
Clean contracts/dispatch.rs - remove unused TagMatcher, AgentSelector,
DispatchPolicy, TagBasedSelector, DefaultDispatchPolicy,
SummarizerSelectionPolicy traits. Keep BrainSelectionPolicy and
AgentCapabilitySummary (still in use).

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §2.9 决策 16"
```

---

### 任务 5.3：删除 `WorkItem.tags` 字段

**文件：**
- 修改：`src/domain/work_item.rs`

**说明：** 派发完全依赖 `work_type.required_tag()`，`WorkItem.tags` 字段无存在必要。

- [ ] **步骤 1：删除 `WorkItem.tags` 字段**

修改 `src/domain/work_item.rs`：
- 移除 `pub tags: TagSet` 字段（第 141 行）
- 修改 `WorkItem::new` 签名移除 `tags` 参数
- 修改所有 `WorkItem::xxx` 构造函数移除 `tags` 设置

- [ ] **步骤 2：更新所有调用方**

运行：`grep -rn "WorkItem::new\|WorkItem::execution\|WorkItem::summarization\|WorkItem::evaluation\|WorkItem::experience_collection\|WorkItem::profile_generation\|WorkItem::skill_update" src/ tests/`

更新所有调用方移除 `tags` 参数。

- [ ] **步骤 3：运行编译和测试**

运行：`cargo build --lib && cargo test --all-features`
预期：编译通过。

- [ ] **步骤 4：Commit**

```bash
git add -A
git commit -m "refactor(dispatch): remove WorkItem.tags field

WorkItem.tags field no longer needed - dispatch uses work_type.required_tag().
Remove tags field from WorkItem struct and all constructors. Also fixes
pre-existing inconsistency where WorkItem::skill_update() used 'skill-update'
tag but skill_update_workitem_system looked up 'skill-updater' agent.

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md §4.1"
```

---

### 任务 5.4：文档同步与最终验证

**文件：**
- 修改：`docs/current-state.md`、`docs/design/README.md`
- 创建：`docs/adr/ADR-005-dispatch-architecture-unification.md`（建议）

- [ ] **步骤 1：更新 `docs/current-state.md`**

在派发架构章节更新描述，反映新的统一派发入口。

- [ ] **步骤 2：更新 `docs/design/README.md`**

在文档索引表中添加：

```markdown
| `2026-07-18-dispatch-architecture-unification-design.md` | 当前有效 | 派发架构统一：单一 PendingDispatch Component 入口 | 治理 9 个腐化点，建立统一 dispatch_system |
```

- [ ] **步骤 3：运行完整 CI 验证**

运行：

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
markdownlint docs/design/2026-07-18-dispatch-architecture-unification-design.md docs/current-state.md docs/design/README.md
```

预期：全部通过。

- [ ] **步骤 4：Commit**

```bash
git add docs/
git commit -m "docs(dispatch): sync documentation after dispatch unification

Update current-state.md and design README to reflect unified dispatch
architecture. All 9 identified rot points resolved (except minor 10:
sanitize logic duplication, out of scope).

Refs: docs/design/2026-07-18-dispatch-architecture-unification-design.md"
```

---

## 自检

### 1. 规格覆盖度

对照设计文档 §2 的 16 个决策：

| 决策 | 覆盖任务 |
|---|---|
| 决策 1：Component 标记位 | 任务 1.1, 2.2 |
| 决策 2：单一 Component + 枚举 kind + hint 结构 | 任务 1.1 |
| 决策 3：合并为 DispatchKind::Task | 任务 2.2, 3.2 |
| 决策 4：派发不预设 Agent kind 约束 | 任务 2.2 |
| 决策 5：skill 注入对所有 Task 适用 | 任务 2.2, 3.1 |
| 决策 6：Task 派发只走 BrainLlm 或 DirectDelegate | 任务 2.2 |
| 决策 7：Brain LLM 失败直接 Failed | 任务 3.1 |
| 决策 8：AwaitingBrainDecision 中间状态 | 任务 1.1, 2.2, 3.1 |
| 决策 8.1：System 排序接受一帧延迟 | 任务 2.2（无显式排序约束） |
| 决策 9：超时由 LLM 调用侧负责 | 非目标，不涉及 |
| 决策 10：TopLevelTask 创建时附加 PendingDispatch | 任务 3.2 |
| 决策 11：SubTask preparation system | 任务 2.3, 3.2 |
| 决策 12：WorkItem 创建器/派发器职责切分 | 任务 4.1, 4.2, 4.3 |
| 决策 13：required_tag() 集中映射 | 任务 1.2 |
| 决策 14：失败处理统一 | 任务 4.4 |
| 决策 15：单一 dispatch_system | 任务 2.2 |
| 决策 16：删除未使用 trait | 任务 5.2 |

### 2. 占位符扫描

已检查计划中的所有步骤，无 "TODO"、"待定" 等占位符。部分步骤标注"实际实现时填充"是因为具体代码依赖前序步骤的编译结果，但都给出了明确的修改方向。

### 3. 类型一致性

- `PendingDispatch` / `DispatchKind` / `DispatchStrategy` / `DispatchHint` / `AgentSpawnSpec` / `AwaitingBrainDecision` 在任务 1.1 定义，后续任务使用一致。
- `WorkItemType::required_tag()` 在任务 1.2 定义，任务 2.2、4.1、4.2、4.3 使用一致。
- `build_brain_execution_request` 在任务 2.1 定义，任务 2.2 使用。

---

## 执行交接

计划已完成并保存到 `docs/superpowers/plans/2026-07-18-dispatch-architecture-unification.md`。两种执行方式：

**1. 子代理驱动（推荐）** - 每个任务调度一个新的子代理，任务间进行审查，快速迭代

**2. 内联执行** - 在当前会话中使用 executing-plans 执行任务，批量执行并设有检查点

选哪种方式？
