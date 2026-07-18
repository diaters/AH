# 派发架构统一设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-07-18 |
| 适用阶段 | 派发模块重构 |
| 相关文档 | `docs/design/2026-06-06-workitem-boundary-design.md`、`docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md`、`docs/current-state.md` <!-- markdownlint-disable-line MD013 --> |

---

## 1. 背景

当前派发架构分散在 4 个 system 中，每个 system 各自决定"由哪个 Agent 执行"，缺乏统一的派发决策点。随着 ADR-004 v6 引入 skill 注入与 Brain LLM 选 Agent+skill 的能力，派发链路的复杂度上升，已有的结构性问题开始显化为多个腐化点。

### 1.1 现状

派发相关 system 共有 4 个，外加 2 个旁路：

| System | 位置 | 职责 |
|---|---|---|
| `brain_dispatch_system` | `src/systems/dispatch/brain_dispatch.rs` | 顶级 Task → Brain Agent；SubTask → 临时 Agent + skill <!-- markdownlint-disable-line MD013 --> |
| `brain_decision_system` | `src/systems/transform/brain_decision.rs` | 解析 Brain LLM 输出，委派给真实执行 Agent |
| `task_dispatch_system` | `src/systems/dispatch/task_dispatch.rs` | 顶级 Task 直接派发（Brain 未启用 / chat 子任务） |
| `workitem_dispatch_system` | `src/systems/dispatch/workitem_dispatch.rs` | WorkItem 按 tag 找 Agent（仅覆盖 3 种类型） |
| `skill_update_workitem_system`（旁路） | `src/systems/experience/skill_update.rs` | 直接 spawn WorkItem + AgentExecutionRequest <!-- markdownlint-disable-line MD013 --> |
| `profile_generation_workitem_system`（旁路） | `src/systems/experience/profile_generation.rs` | 同上，绕过通用派发 |

### 1.2 已识别的腐化点

- 严重 1：`select_agent_with_memory` 与 `select_agent_for_sub_task` 几乎完全重复
- 严重 2：`contracts/dispatch.rs` 的 trait 体系与实际派发实现脱节，多数 trait 是 dead code
- 严重 3：ADR-004 v6 的 `parse_brain_skill_selection` 已实现但未接入生产路径，`select_agent_for_sub_task_with_skill` 仍是 placeholder
- 严重 4：`skill_update_workitem_system` 和 `profile_generation_workitem_system` 绕过通用派发入口直接 spawn
- 严重 5：`task_dispatch` 与 `brain_dispatch` 通过隐式条件分支划分边界，规则不直观
- 中等 6：`SubTaskConfig.child_agent_name` 字段名与实际用途不符——
  该字段用于命名 spawn 出的 Agent（同时作为 `AgentSpawnRequestMessage.name` 和 `description`，
  见 [brain_dispatch.rs:314-316](../../src/systems/dispatch/brain_dispatch.rs)），
  但不参与 agent 选择（`select_agent_for_sub_task_with_skill` 基于 task content 与 agent tags 匹配评分，
  完全不读取此字段）
- 中等 7：多个 placeholder 与 TODO 残留
  （`writeback_to_long_term_memory_for_persistent_agent`、
  `writeback.rs:123-135` 的 `ExperienceWritebackDestination::SkillUpdate` 占位分支等。
  后者注释明确"任务 20 替换后此分支不再被触发"，真正路由在 `governance.rs` 中通过
  `SkillUpdateRequestMessage` spawn）
- 中等 8：`"brain"`、`"default"`、`"summarization"`、`"evaluation"`、`"collect"`、`"skill-updater"` 等 tag 在多处硬编码，无集中定义
- 中等 9：`WorkItem::skill_update()` 构造函数使用 tag `"skill-update"`
  （[work_item.rs:304](../../src/domain/work_item.rs)），
  而 `skill_update_workitem_system` 查找 Agent 时使用 `"skill-updater"`
  （[skill_update.rs:164](../../src/systems/experience/skill_update.rs)）。
  当前因旁路派发不暴露，统一后将冲突

### 1.3 目标

- 建立单一派发入口，所有派发请求通过统一的 `PendingDispatch` Component 流转
- 消除重复实现与旁路派发，将所有派发决策收敛到单一 `dispatch_system`
- 完成 ADR-004 v6 的 Brain LLM 选 Agent+skill 能力接入
- 统一 WorkItem 创建器与派发器的职责边界
- 治理所有上述腐化点（轻微 10 除外）

### 1.4 非目标

- Brain LLM 调用超时机制（由 LLM 调用侧后续设计）
- default Agent 是否重新定义为 TaskScoped（属于 Agent 配置模型改动）
- `sanitize_brain_output` 与 `brain_prompt.rs` 的重复治理（轻微 10，保留本地实现）
- 父 Agent 审批的 LLM 化（独立任务）

---

## 2. 设计决策

### 2.1 统一派发入口形态

#### 决策 1：Component 标记位

派发请求以 `PendingDispatch` Component 形式附加在原 Entity（Task 或 WorkItem）上，由单一 `dispatch_system` 扫描处理。

理由：

- 符合 ECS 数据驱动风格，派发请求是实体的临时状态
- 零额外 Entity 生命周期管理，派发完成只需 `remove::<PendingDispatch>()`
- 上下文天然可访问，无需在 request 里冗余 Task.content / WorkItem.work_type
- 可观测，`PendingDispatch` 在 inspector 中直接可见
- 可重试，派发失败时保留 Component 下一帧再试（当前设计选择直接 Failed，但保留扩展空间）

不采用 Event（transient 特性是隐患）、Resource（违背 ECS 风格）、独立 Entity（额外生命周期管理）等形态。

#### 决策 2：单一 Component + 枚举 kind + hint 结构

```rust
#[derive(Component)]
pub struct PendingDispatch {
    pub kind: DispatchKind,
    pub hint: DispatchHint,
}

pub enum DispatchKind {
    Task,                    // 合并 TopLevelTask + SubTask
    WorkItem(WorkItemType),
}

pub enum DispatchStrategy {
    BrainLlm,          // 走 Brain LLM 选 Agent + skill
    DirectDelegate,    // Brain 决策后或显式指定
}

pub struct DispatchHint {
    pub strategy: DispatchStrategy,
    pub preferred_agent_name: Option<String>,
    pub required_skill_id: Option<SkillId>,
    pub agent_spawn_spec: Option<AgentSpawnSpec>,
}

pub struct AgentSpawnSpec {
    pub name: String,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub parent_agent_id: Option<AgentId>,
}
```

理由：

- 单一 query 入口，真正实现"单一派发入口"
- kind 枚举承袭现有 `WorkItemType`，不重新发明
- hint 携带所有派发提示，可选字段默认走通用策略
- 解决 `child_agent_name` 字段腐化（字段迁移到 `AgentSpawnSpec.name`，语义对齐）
- 解决 tag 硬编码散落（通过 `WorkItemType::required_tag()` 集中映射）
- 不设 `required_tags` 字段：Task 派发走 BrainLlm/DirectDelegate 不需要 tag，
  WorkItem 派发从 `work_type.required_tag()` 获取，字段无存在必要。
  避免两处 tag 不一致的风险

### 2.2 TopLevelTask 与 SubTask 合并

#### 决策 3：合并为 `DispatchKind::Task`

TopLevelTask 和 SubTask 在派发决策层面的差异通过 `DispatchHint` 表达，不需要 kind 枚举区分。

理由：

- 两者真正的派发决策差异只有：是否走 Brain LLM、是否 spawn 临时 Agent、是否注入 skill
- 这些差异全部可通过 hint 表达：`strategy` / `agent_spawn_spec` / `required_skill_id`
- DAG 依赖检查和兄弟结果收集是"派发前置条件"，不是"派发决策"，保留在 `subtask_dispatch_preparation_system` 中
- 为 ADR-004 v6 接入铺路：SubTask 也应该走 Brain LLM 选 Agent+skill，合并后只需把 `strategy` 从 fallback 改为 `BrainLlm`
- 后置经验路径差异由经验 system 根据 `parent_task_id` 和 `agent.kind` 自动分流，派发层不关心

#### 决策 4：派发不预设 Agent kind 约束

派发决策不再过滤 `AgentKind::Persistent`（当前代码 [agent_selection.rs:31-32](../../src/systems/dispatch/agent_selection.rs) 的预设）。
Agent kind 是 Agent 的属性，不是 Task 的属性。

派发动作统一为"先委派，找不到再 spawn"：

- 找到匹配 Agent（不论 kind）→ 委派已有
- 找不到且提供 `agent_spawn_spec` → spawn 新 Agent 再委派
- 找不到且无 `agent_spawn_spec` → Failed

#### 决策 5：skill 注入对所有 Task 适用

只要目标 Agent 是 Persistent 且有 skills，就可以注入一个 skill（或不注入）。`TaskInjectedSkill` Component 不再是 SubTask 专属。

约束：

- 最多注入一个 skill
- 仅 Persistent Agent 可被注入 skill（TaskScoped Agent 无 skills）
- 注入由 `DispatchHint.required_skill_id` 携带

### 2.3 派发策略

#### 决策 6：Task 派发只走 BrainLlm 或 DirectDelegate

移除 TagMatch 作为 Task 派发策略。Tag 匹配仅用于 WorkItem（按 `work_type.required_tag()` 查找）。

理由：

- Tag 匹配准确性太差，不适合作为 Task 派发策略
- Tag 匹配仅适用于 WorkItem 这类"特定 Agent 类型"的强约束场景
- 现有 `select_agent_with_memory` 和 `select_agent_for_sub_task` 的 tag 匹配逻辑可彻底删除（腐化点严重 1 根治）

策略使用规则：

- `strategy = BrainLlm` 时，`preferred_agent_name` 和 `required_skill_id` 都应为 None（Brain 负责选）
- `strategy = DirectDelegate` 时，`preferred_agent_name` 必填，`required_skill_id` 可选
- `agent_spawn_spec` 对所有 Task 适用
- WorkItem 派发不走 strategy 枚举，由 `DispatchKind::WorkItem(work_type)` 直接分流，tag 从 `work_type.required_tag()` 获取

#### 决策 7：Brain LLM 失败直接 Failed

Brain LLM 超时、解析失败、选了不存在的 Agent → 直接把 Task 标 `Failed`，不引入隐式 fallback。

理由：

- 语义诚实，失败立即暴露
- 符合 AGENTS.md "避免伪精细控制面"原则
- 当前 `brain_decision_system` fallback 到第一个非 brain Persistent Agent（[brain_decision.rs:93-108](../../src/systems/transform/brain_decision.rs)）是腐化点，应移除
- 不掩盖 Brain LLM 的 prompt 或配置问题

### 2.4 Brain LLM 异步性处理

#### 决策 8：引入 `AwaitingBrainDecision` 中间状态

派发流程：

1. `dispatch_system` 看到 `PendingDispatch` + `strategy = BrainLlm`
2. 移除 `PendingDispatch`，添加 `AwaitingBrainDecision` Component，spawn Brain LLM 调用（通过 `AgentExecutionRequestMessage`）
3. Brain LLM 完成后，`brain_decision_system` 处理输出，解析出 `{agent_name, skill_name?}`
4. 移除 `AwaitingBrainDecision`，添加 `PendingDispatch` + `strategy = DirectDelegate` +
   `preferred_agent_name` + `required_skill_id` 回到队列
5. 下一帧 `dispatch_system` 按 DirectDelegate 策略派发

```rust
#[derive(Component)]
pub struct AwaitingBrainDecision {
    pub task_id: TaskId,
    pub spawn_spec: Option<AgentSpawnSpec>,
}
```

状态机：

```text
PendingDispatch(BrainLlm) → AwaitingBrainDecision → PendingDispatch(DirectDelegate) → 派发完成
                                                    → Failed（Brain 失败）
```

理由：

- 派发入口真正单一，Brain LLM 只是策略之一
- 状态机清晰可观测
- `parse_brain_skill_selection` 在 `brain_decision_system` 中被调用，产出 `required_skill_id`（腐化点严重 3 根治）
- `spawn_spec` 携带保证状态完整（SubTask 场景的 spawn spec 在 Brain 决策后复用）

#### 决策 8.1：System 排序约束——接受额外一帧延迟

`brain_decision_system` 与 `dispatch_system` 不显式声明排序约束，接受 Brain 决策结果下一帧才被 `dispatch_system` 处理的额外一帧延迟。

理由：

- 实现简单，无需在 SystemSet 排序中显式声明 `.before()` / `.after()`
- 一帧延迟在派发场景下可忽略（Brain LLM 调用本身是跨帧异步，多一帧无关紧要）
- 状态机更清晰：`PendingDispatch(BrainLlm) → AwaitingBrainDecision → [下一帧] → PendingDispatch(DirectDelegate) → 派发完成`

如果未来需要消除这一帧延迟，可在 `plugins/dispatch.rs` 中显式声明 `brain_decision_system.before(dispatch_system)`。

#### 决策 9：超时由 LLM 调用侧负责

派发层不引入显式超时检测。Brain LLM 调用的超时机制由 LLM 调用侧后续设计，错误通过 `AgentExecutionResultMessage` 回流到 `brain_decision_system`，统一走失败路径。

### 2.5 派发请求生成器

#### 决策 10：TopLevelTask 在创建时直接附加 `PendingDispatch`

TopLevelTask 没有前置条件检查，不需要 preparation system。任务入口（用户提交 / 信号触发 / Agent `create_tasks` 工具）在 spawn Task Entity 时直接附加 `PendingDispatch`。

理由：

- 简化优先，避免过度设计
- 多入口可接受，每个入口附加 `PendingDispatch` 是一行代码

#### 决策 11：SubTask 保留独立 `subtask_dispatch_preparation_system`

SubTask 派发前置条件（DAG 依赖检查 + 兄弟任务结果收集 + spawn spec 准备）由独立 system 处理。准备完成后附加 `PendingDispatch`。

职责：

- 扫描带 `SubTaskConfig` 的 Task
- 检查 DAG 依赖
- 收集兄弟任务结果
- 准备 `AgentSpawnSpec`（从 `SubTaskConfig` 转换）
- 准备完成后附加 `PendingDispatch` + `strategy = BrainLlm` + `agent_spawn_spec = Some(...)`

SystemSet：在 `HarnessSet::Dispatch` 之前。

理由：

- 单一职责，派发前置条件检查与派发决策分离
- 可独立测试 DAG 逻辑
- 未来扩展（权限检查、资源配额检查）有归属
- 解决 `task_dispatch` 与 `brain_dispatch` 边界混乱（腐化点严重 5）

### 2.6 WorkItem 派发统一

#### 决策 12：WorkItem 创建器与派发器职责切分

把每个 WorkItem 链路拆成两阶段：

```text
RequestMessage → [WorkItem 创建器] → WorkItem + PendingDispatch → [统一 dispatch_system] → AgentExecutionRequest
                （构造 prompt + 工具）                            （按 work_type 找 Agent + spawn 执行请求）
```

- `skill_update_workitem_system` 剥离直接派发逻辑，仅保留 WorkItem 创建 + 附加 PendingDispatch：
  消费 `SkillUpdateRequestMessage`，构造 prompt + 工具，
  spawn `WorkItem + SkillUpdateContext + PendingDispatch`
- `profile_generation_workitem_system` 剥离直接派发逻辑，仅保留 WorkItem 创建 + 附加 PendingDispatch：
  消费 `ProfileGenerationRequestMessage`，构造 prompt + 工具，
  spawn `WorkItem + PendingDispatch`
- `experience_collection_workitem_system` 同样剥离直接派发逻辑
  （该系统已是 WorkItem 创建器，只需移除直接 spawn `AgentExecutionRequestMessage` 的部分，
  改为附加 `PendingDispatch`）
- `evaluation` / `summarization` WorkItem 创建器保持不变
- `workitem_dispatch_system` 升级为统一派发器（合并到 `dispatch_system`）

理由：

- 真正消除旁路（腐化点严重 4 根治）
- 职责单一，创建器只构造内容，派发器只派发
- 旁路存在的本质原因是"Prompt 构造 + 派发"被绑在一起，剥离 Prompt 构造后派发器不再需要特殊上下文
- ProfileGenerationContext 从 ExperienceStore 迁移到 Entity Component，与 SkillUpdateContext 存储模型一致

#### 决策 13：`WorkItemType::required_tag()` 集中 tag 映射

```rust
impl WorkItemType {
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

理由：

- 解决 tag 硬编码散落（腐化点中等 8）
- 所有 WorkItem 类型通过统一 tag 映射查找 Agent
- 新增 WorkItemType 时只需更新一处

### 2.7 失败处理统一

#### 决策 14：WorkItem 失败处理统一为 fail + hook + Context 分流

派发器失败处理统一为：

1. `work_item.fail()` 标记 WorkItem Failed
2. 派发 `OnWorkItemFailed` hook
3. 移除 `PendingDispatch` Component
4. 失败后的特化逻辑由 Context Component 携带，companion system 在处理 hook 时按 Context 类型分流

```text
[dispatch_system] → WorkItem(failed) + OnWorkItemFailed hook
                          ↓
[workitem_lifecycle_hook_system] → 按 Context Component 分流
                          ├─ SkillUpdateContext: 候选保持 GovernanceResolved
                          ├─ ProfileGenerationContext: handle_profile_designer_missing 逻辑
                          ├─ Evaluation/Summarization: 恢复 Task 状态
                          └─ ExperienceCollection: 不回滚 Task
```

Context Component 统一模型：

```rust
#[derive(Component)]
pub struct SkillUpdateContext { ... }  // 已有

#[derive(Component)]
pub struct ProfileGenerationContext { ... }  // 从 ExperienceStore 迁移
```

Evaluation / Summarization / ExperienceCollection 无需额外 Context，失败处理通过 work_type + task 关联即可。

理由：

- 派发器失败处理完全统一，不看 work_type
- 特化逻辑收敛到 companion system 的 hook 处理
- 解决当前 skill_update 候选悬空问题（候选状态由 Context 携带的 candidate_id 决定）
- 与现有 hook 机制对齐

### 2.8 统一 dispatch_system 结构

#### 决策 15：单一 dispatch_system + 内部 match kind

```rust
fn dispatch_system(
    mut commands: Commands,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,
    mut tasks: Query<(Entity, &mut Task, Option<&PendingDispatch>)>,
    mut work_items: Query<(Entity, &mut WorkItem, Option<&PendingDispatch>)>,
) {
    // 处理 Task 派发
    for (entity, mut task, pending) in tasks.iter_mut() {
        let Some(pending) = pending else { continue };
        match &pending.hint.strategy {
            DispatchStrategy::BrainLlm => {
                // 移除 PendingDispatch，加 AwaitingBrainDecision，spawn Brain 调用
            }
            DispatchStrategy::DirectDelegate => {
                // 按 preferred_agent_name 找 Agent，委派或 spawn
            }
        }
    }

    // 处理 WorkItem 派发
    for (entity, mut work_item, pending) in work_items.iter_mut() {
        let Some(pending) = pending else { continue };
        let DispatchKind::WorkItem(work_type) = &pending.kind else { continue };
        let tag = work_type.required_tag();
        let agent = agents.iter().find(|a| a.capabilities.tags.iter().any(|t| t == tag));
        // assign + start + spawn AgentExecutionRequest，或 fail
    }
}
```

理由：

- 派发入口真正单一，所有派发决策在一个地方可观测、可审计
- 内部 match kind 是合理的复杂度，Bevy ECS 中一个 system 处理多种 Entity 类型是常见模式
- query 参数多不是问题，Bevy ECS 的 system 支持多 query

### 2.9 contracts/dispatch.rs trait 体系处置

#### 决策 16：删除未使用 trait，保留 `BrainSelectionPolicy`

删除：

- `TagMatcher` trait
- `AgentSelector` trait
- `DispatchPolicy` trait
- `TagBasedSelector`
- `DefaultDispatchPolicy`
- `SummarizerSelectionPolicy` trait 及其实现

保留：

- `BrainSelectionPolicy` trait + `FirstBrainPolicy`（实际被使用）
- `AgentCapabilitySummary`（构造 Brain 候选列表用）

理由：

- 符合 AGENTS.md "简化优先 / 避免为了'抽象完整'引入不必要层级"原则
- 当前 trait 体系只有 `BrainSelectionPolicy` 被使用，其余是 dead code
- 统一派发入口后选择策略集中可见，不需要 trait 间接层
- YAGNI 原则，未来如果真的需要多种选择策略再引入抽象

---

## 3. 详细设计

### 3.1 数据结构定义

新增 `src/domain/dispatch.rs`：

```rust
use crate::domain::{AgentId, SkillId, TaskId, WorkItemType};
use bevy_ecs::prelude::Component;

/// 派发请求标记 Component，附加在 Task 或 WorkItem Entity 上
#[derive(Component)]
pub struct PendingDispatch {
    pub kind: DispatchKind,
    pub hint: DispatchHint,
}

/// 派发类型
pub enum DispatchKind {
    /// 合并 TopLevelTask + SubTask
    Task,
    /// WorkItem 派发，按 work_type 分流
    WorkItem(WorkItemType),
}

/// 派发策略
pub enum DispatchStrategy {
    /// 走 Brain LLM 选 Agent + skill（默认）
    BrainLlm,
    /// Brain 决策后或显式指定，直接委派
    DirectDelegate,
}

/// 派发提示
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
pub struct AgentSpawnSpec {
    pub name: String,
    pub model: Option<String>,
    pub allowed_tools: Vec<String>,
    pub parent_agent_id: Option<AgentId>,
}

/// Brain LLM 决策等待状态
#[derive(Component)]
pub struct AwaitingBrainDecision {
    pub task_id: TaskId,
    pub spawn_spec: Option<AgentSpawnSpec>,
}
```

### 3.2 `WorkItemType::required_tag()` 方法

修改 `src/domain/work_item.rs`：

```rust
impl WorkItemType {
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

### 3.3 System 拓扑

```text
[Task 创建入口]                          [WorkItem 创建器]
  - 用户提交                                - experience_collection_workitem_system
  - 信号触发                                - skill_update_workitem_system（退化为创建器）
  - create_tasks 工具                       - profile_generation_workitem_system（退化为创建器）
  - evaluation/summarization 创建
       │                                          │
       ▼                                          ▼
  spawn Task + PendingDispatch            spawn WorkItem + PendingDispatch (+ Context Component)
       │                                          │
       └──────────────┬───────────────────────────┘
                      ▼
        [subtask_dispatch_preparation_system]  ← 仅 SubTask
                      │
                      ▼
              [dispatch_system]  ← 单一派发入口
                ├─ Task + BrainLlm → AwaitingBrainDecision + spawn Brain 调用
                ├─ Task + DirectDelegate → 按 preferred_agent_name 委派或 spawn
                └─ WorkItem(work_type) → 按 required_tag() 委派或 fail
                      │
                      ▼
        [brain_decision_system]  ← 处理 Brain LLM 输出
          ├─ 成功 → PendingDispatch + DirectDelegate
          └─ 失败 → Task 标 Failed
                      │
                      ▼
        [workitem_lifecycle_hook_system]  ← 失败特化逻辑
          ├─ SkillUpdateContext: 候选保持 GovernanceResolved
          ├─ ProfileGenerationContext: handle_profile_designer_missing 逻辑
          ├─ Evaluation/Summarization: 恢复 Task 状态
          └─ ExperienceCollection: 不回滚 Task
```

### 3.4 dispatch_system 实现骨架

__前置假设__：Task 和 WorkItem 是不同 entity，因此 `dispatch_system` 持有两个 `mut Query`
不会触发 Bevy ECS 的 query 冲突。未来如果架构调整使 Task 和 WorkItem 可能共存于同一 entity，
需要重新审视 query 设计。

```rust
pub(crate) fn dispatch_system(
    mut commands: Commands,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,
    clock: Res<Clock>,
    mut tasks: Query<(Entity, &mut Task, Option<&PendingDispatch>)>,
    mut work_items: Query<(Entity, &mut WorkItem, Option<&PendingDispatch>)>,
) {
    // 处理 Task 派发
    for (entity, mut task, pending) in tasks.iter_mut() {
        let Some(pending) = pending else { continue };
        let DispatchKind::Task = &pending.kind else { continue };

        match &pending.hint.strategy {
            DispatchStrategy::BrainLlm => {
                // 移除 PendingDispatch，加 AwaitingBrainDecision，spawn Brain 调用
                commands.entity(entity).remove::<PendingDispatch>();
                commands.entity(entity).insert(AwaitingBrainDecision {
                    task_id: task.id,
                    spawn_spec: pending.hint.agent_spawn_spec.clone(),
                });
                spawn_brain_llm_request(&mut commands, &task, &agents);
            }
            DispatchStrategy::DirectDelegate => {
                let Some(agent_name) = &pending.hint.preferred_agent_name else {
                    // 语义错误，DirectDelegate 必须有 preferred_agent_name
                    mark_task_failed(&mut task, &clock, "DirectDelegateWithoutPreferredAgent");
                    commands.entity(entity).remove::<PendingDispatch>();
                    continue;
                };
                let agent = agents.iter().find(|a| &a.profile.name == agent_name);
                match agent {
                    Some(agent) => {
                        delegate_task_to_agent(&mut commands, &mut task, agent, &pending.hint);
                        commands.entity(entity).remove::<PendingDispatch>();
                    }
                    None => {
                        // 如果有 spawn spec，spawn 新 Agent 后委派
                        if let Some(spec) = &pending.hint.agent_spawn_spec {
                            spawn_agent_and_delegate(&mut commands, &mut task, spec, &pending.hint);
                            commands.entity(entity).remove::<PendingDispatch>();
                        } else {
                            mark_task_failed(&mut task, &clock, "AgentNotFound");
                            commands.entity(entity).remove::<PendingDispatch>();
                        }
                    }
                }
            }
        }
    }

    // 处理 WorkItem 派发
    for (entity, mut work_item, pending) in work_items.iter_mut() {
        let Some(pending) = pending else { continue };
        let DispatchKind::WorkItem(work_type) = &pending.kind else { continue };

        let tag = work_type.required_tag();
        let agent = agents.iter().find(|a| a.capabilities.tags.iter().any(|t| t == tag));

        match agent {
            Some(agent) => {
                work_item.assign(agent.id);
                work_item.start();
                commands.entity(entity).insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted));
                spawn_workitem_execution_request(&mut commands, &work_item, agent);
                commands.entity(entity).remove::<PendingDispatch>();
            }
            None => {
                work_item.fail();
                commands.entity(entity).insert(WorkItemLifecycleHookPending(HookPoint::OnWorkItemFailed));
                commands.entity(entity).remove::<PendingDispatch>();
            }
        }
    }
}
```

#### Brain LLM 调用逻辑迁移策略

`spawn_brain_llm_request` 辅助函数复用现有 `brain_dispatch.rs` 中的 Brain LLM 调用逻辑，提取为独立函数。迁移范围：

- Brain Agent 选择（`FirstBrainPolicy`，[brain_dispatch.rs:174-180](../../src/systems/dispatch/brain_dispatch.rs)）
- Brain LLM prompt 构建（包含 Agent 候选列表、skill 描述等）
- `AgentExecutionRequestMessage` 构造（`request_kind = BrainDecision`）

迁移方式：

1. 将 `brain_dispatch.rs` 中 Brain LLM 调用相关逻辑提取为
   `build_brain_execution_request(task, agents) -> AgentExecutionRequestMessage` 独立函数
2. `dispatch_system` 在 BrainLlm 策略分支中调用该函数
3. 保留 `brain_dispatch.rs` 中的 Brain Agent 选择和 prompt 构建逻辑，仅移除派发决策部分

理由：

- 复用现有逻辑的测试覆盖，减少重写风险
- 保留 Brain LLM 调用的复杂逻辑（prompt 构建、Agent 候选列表、skill 注入）的完整性
- `brain_dispatch.rs` 退化为 Brain LLM 调用工具函数库，不再承担派发决策职责

### 3.5 brain_decision_system 改造

```rust
pub(crate) fn brain_decision_system(
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut Task, &AwaitingBrainDecision)>,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,
    results: Query<&AgentExecutionResultMessage>,
) {
    for (entity, mut task, awaiting) in tasks.iter() {
        let Some(result) = results.iter().find(|r| r.request.task_id == task.id) else { continue };

        match parse_brain_decision_and_skill(&result.output, &skill_registry) {
            Ok((agent_name, skill_id)) => {
                let agent_exists = agents.iter().any(|a| a.profile.name == agent_name);
                if !agent_exists {
                    mark_task_failed(&mut task, "BrainSelectedAgentNotFound");
                    commands.entity(entity).remove::<AwaitingBrainDecision>();
                    continue;
                }
                // 移除 AwaitingBrainDecision，加 PendingDispatch + DirectDelegate
                commands.entity(entity).remove::<AwaitingBrainDecision>();
                commands.entity(entity).insert(PendingDispatch {
                    kind: DispatchKind::Task,
                    hint: DispatchHint {
                        strategy: DispatchStrategy::DirectDelegate,
                        preferred_agent_name: Some(agent_name),
                        required_skill_id: skill_id,
                        agent_spawn_spec: awaiting.spawn_spec.clone(),
                    },
                });
            }
            Err(e) => {
                mark_task_failed(&mut task, &format!("BrainDecisionError: {:?}", e));
                commands.entity(entity).remove::<AwaitingBrainDecision>();
            }
        }
    }
}
```

### 3.6 SubTask preparation system

```rust
pub(crate) fn subtask_dispatch_preparation_system(
    mut commands: Commands,
    tasks: Query<(Entity, &Task, &SubTaskConfig, Option<&PendingDispatch>)>,
    batch_states: Query<&SubTaskBatchState>,
    all_tasks: Query<&Task>,
) {
    for (entity, task, config, pending) in tasks.iter() {
        if pending.is_some() { continue };  // 已有 PendingDispatch，跳过
        if task.status != TaskStatus::Ready { continue };

        // 1. DAG 依赖检查
        if !all_dependencies_done(&config.depends_on, &all_tasks) { continue };

        // 2. 兄弟任务结果收集（用于 prompt 上下文）
        let sibling_results = collect_sibling_results(&config.batch_id, &all_tasks);

        // 3. 准备 AgentSpawnSpec
        let spawn_spec = AgentSpawnSpec {
            name: config.child_agent_name.clone(),
            model: config.child_agent_model.clone(),
            allowed_tools: config.allowed_tools.clone(),
            parent_agent_id: Some(config.parent_agent_id),
        };

        // 4. 附加 PendingDispatch
        commands.entity(entity).insert(PendingDispatch {
            kind: DispatchKind::Task,
            hint: DispatchHint {
                strategy: DispatchStrategy::BrainLlm,
                preferred_agent_name: None,
                required_skill_id: None,
                agent_spawn_spec: Some(spawn_spec),
            },
        });
    }
}
```

### 3.7 WorkItem 创建器改造（以 skill_update 为例）

改造前：

```rust
// skill_update_workitem_system 直接 spawn WorkItem + AgentExecutionRequestMessage
commands.spawn((work_item, SkillUpdateContext {...}));
commands.spawn((AgentExecutionRequestMessage {...}, ...));  // 旁路！
```

改造后：

```rust
// skill_update_workitem_system 退化为 WorkItem 创建器
let prompt = build_skill_update_prompt(&skill_entry, &candidate);
let work_item = WorkItem::skill_update(...);
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
    WorkItemLifecycleHookPending(HookPoint::OnWorkItemStarted),
));
// 不再 spawn AgentExecutionRequestMessage
```

---

## 4. 实施路径

### 4.1 改动范围

__新增__：

- `src/domain/dispatch.rs`：
  `PendingDispatch` / `DispatchKind` / `DispatchStrategy` / `DispatchHint`
  / `AgentSpawnSpec` / `AwaitingBrainDecision`
- `src/systems/dispatch/dispatch_system.rs`：统一派发器
- `src/systems/dispatch/subtask_dispatch_preparation.rs`：SubTask 前置 system
- `WorkItemType::required_tag()` 方法

__修改__：

- `src/systems/dispatch/brain_dispatch.rs`：移除 SubTask 派发逻辑，
  Brain LLM 调用逻辑提取为独立函数 `build_brain_execution_request` 供 dispatch_system 调用
- `src/systems/transform/brain_decision.rs`：接入 `parse_brain_skill_selection`，产出 `PendingDispatch + DirectDelegate`
- `src/systems/experience/skill_update.rs`：`skill_update_workitem_system` 剥离直接派发逻辑，
  仅保留 WorkItem 创建 + 附加 PendingDispatch
- `src/systems/experience/profile_generation.rs`：`profile_generation_workitem_system`
  剥离直接派发逻辑，`ProfileGenerationContext` 迁移到 Entity Component
- `src/systems/dispatch/workitem_lifecycle_hook.rs`：按 Context Component 分流失败处理
- `src/plugins/dispatch.rs`：system 注册更新
- `src/domain/work_item.rs`：
  - 新增 `WorkItemType::required_tag()` 方法
  - 删除 `WorkItem.tags` 字段（派发完全依赖 `work_type.required_tag()`，字段无存在必要）
  - 修改所有 `WorkItem::xxx()` 构造函数，移除 `tags` 参数
  - 检查是否有其他代码路径读取 `WorkItem.tags`，统一清理

__删除__：

- `src/systems/dispatch/task_dispatch.rs`（合并到 dispatch_system）
- `src/systems/dispatch/workitem_dispatch.rs`（合并到 dispatch_system）
- `src/systems/dispatch/agent_selection.rs`（tag 匹配逻辑收敛到 dispatch_system）
- `src/contracts/dispatch.rs` 中未使用的 trait：
  `TagMatcher` / `AgentSelector` / `DispatchPolicy` / `TagBasedSelector`
  / `DefaultDispatchPolicy` / `SummarizerSelectionPolicy`

__保留__：

- `BrainSelectionPolicy` trait + `FirstBrainPolicy`
- `AgentCapabilitySummary`
- Brain LLM 调用机制（`AgentExecutionRequestMessage` + `AgentExecutionResultMessage`）

### 4.2 分阶段实施

实施分 5 个阶段，每个阶段可独立验证：

1. __数据结构定义__：新增 `src/domain/dispatch.rs` 和 `WorkItemType::required_tag()`，纯数据层改动，无行为变化
2. __统一 dispatch_system 建立__：新增 `dispatch_system` 和 `subtask_dispatch_preparation_system`，与现有 system 并存（不删除旧 system）
3. __Task 派发迁移__：TopLevelTask 和 SubTask 派发迁移到新 system，移除 `task_dispatch.rs` 和 `brain_dispatch.rs` 的派发逻辑
4. __WorkItem 派发迁移__：WorkItem 派发迁移到新 system，`skill_update_workitem_system` 和 `profile_generation_workitem_system` 退化为创建器
5. __清理与简化__：删除 `agent_selection.rs` 和 `contracts/dispatch.rs` 未使用 trait，更新文档

### 4.3 腐化点覆盖情况

| 腐化点 | 治理方式 | 阶段 |
|---|---|---|
| 严重 1：派发逻辑重复 | dispatch_system 统一处理，`select_agent_with_memory` / `select_agent_for_sub_task` 删除 | 5 |
| 严重 2：trait 脱节 | `contracts/dispatch.rs` 未使用 trait 删除 | 5 |
| 严重 3：ADR-004 v6 未接入 | `parse_brain_skill_selection` 在 brain_decision_system 中调用 | 3 |
| 严重 4：WorkItem 旁路派发 | 创建器/派发器职责切分 | 4 |
| 严重 5：task_dispatch 与 brain_dispatch 边界混乱 | 统一入口 | 3 |
| 中等 6：`child_agent_name` 字段语义误导 | 字段迁移到 `AgentSpawnSpec.name`（同时用作 spawn Agent 的 name 和 description） | 3 |
| 中等 7：Placeholder 与 TODO 残留 | `select_agent_for_sub_task_with_skill` placeholder 删除 | 5 |
| 中等 8：tag 硬编码散落 | `WorkItemType::required_tag()` 集中映射 | 1 |
| 中等 9：WorkItem tag 与 Agent tag 不一致 | 删除 `WorkItem.tags` 字段，派发完全依赖 `work_type.required_tag()` | 1 |
| 轻微 10：sanitize 逻辑重复 | 不在本次治理范围 | - |

---

## 5. 测试策略

### 5.1 单元测试

- `DispatchHint` 字段约束验证（DirectDelegate 必须有 preferred_agent_name 等）
- `WorkItemType::required_tag()` 映射完整性
- `parse_brain_decision_and_skill` 解析正确性与容错

### 5.2 集成测试

每个实施阶段配套集成测试：

1. __数据结构阶段__：无新行为，验证数据结构可序列化/Clone
2. __dispatch_system 建立阶段__：验证新 system 与旧 system 并存时不会重复派发
3. __Task 派发迁移阶段__：
   - TopLevelTask 通过 BrainLlm 策略派发
   - SubTask 通过 preparation system + BrainLlm 策略派发
   - Brain LLM 失败时 Task 标 Failed
   - DirectDelegate 策略下 spawn_spec 携带和复用
4. __WorkItem 派发迁移阶段__：
   - SkillUpdate WorkItem 通过创建器 + dispatch_system 派发
   - ProfileGeneration WorkItem 同上
   - WorkItem 失败时按 Context Component 分流
5. __清理阶段__：回归测试，确保删除旧代码后所有派发链路正常

### 5.3 CI 验证

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`
- `markdownlint`（本文档与相关文档）

---

## 6. 风险与缓解

### 6.1 风险：Brain LLM 决策链路变长

__问题__：统一后 Brain LLM 决策从"brain_dispatch → brain_decision"两步变为
"dispatch(BrainLlm) → AwaitingBrainDecision → brain_decision → dispatch(DirectDelegate)"四步，
跨帧数增加。

__缓解__：跨帧数从 2 帧增加到 3 帧（dispatch_system → brain_decision_system → dispatch_system），延迟影响可忽略。状态机可观测性提升补偿了链路长度。

### 6.2 风险：迁移过程中派发链路中断

__问题__：分阶段迁移时，旧 system 和新 system 可能同时处理同一 Task/WorkItem，导致重复派发或派发遗漏。

__缓解__：

- 阶段 2 新 system 建立时，与旧 system 通过 `PendingDispatch` Component 互斥——旧 system 不处理带 `PendingDispatch` 的 Entity
- 阶段 3、4 迁移时，逐个 Task/WorkItem 创建入口切换到新流程，未切换的入口仍走旧流程
- 每阶段配套集成测试验证

### 6.3 风险：ProfileGenerationContext 存储位置迁移

__问题__：`ProfileGenerationContext` 从 ExperienceStore 迁移到 Entity Component，需要同步修改所有读取方。

__缓解__：通过全局搜索 `ProfileGenerationContext` 定位所有读取点，统一迁移。迁移后通过编译错误验证完整性。

### 6.4 风险：删除 trait 体系影响扩展性

__问题__：删除 `contracts/dispatch.rs` 的 trait 体系后，未来新增派发策略需要修改 dispatch_system 内部。

__缓解__：符合 YAGNI 原则。当前只有两种策略（BrainLlm / DirectDelegate），dispatch_system 内部 match 足够清晰。未来如果策略数量增长（超过 3 种），再引入 trait 抽象。

---

## 7. 文档同步要求

实施完成后需同步更新：

- `docs/current-state.md`：派发架构章节
- `docs/design/README.md`：新增本文档索引
- `docs/adr/`：本次改动涉及派发架构重大调整，建议新增 ADR-005 记录决策
- `docs/design/2026-06-06-workitem-boundary-design.md`：如有冲突，补充说明或归档

---

## 8. 自审清单

- [x] 与 AGENTS.md 一致（简化优先、规范驱动、用户确认）
- [x] 逻辑自洽（决策之间无冲突）
- [x] 技术路径合理（ECS 数据驱动、单一职责、职责切分）
- [x] 文档中的"当前状态"与实际代码一致（腐化点引用了具体文件和行号）
- [x] 所有腐化点都有对应治理方案（轻微 10 除外，已说明原因）
- [x] 实施路径分阶段，每阶段可独立验证
- [x] 测试策略覆盖每个实施阶段
- [x] 风险识别与缓解方案完整
- [x] 评审反馈已修正（事实描述、required_tags 字段删除、System 排序约束、WorkItem.tags 字段删除、Brain LLM 迁移策略）
