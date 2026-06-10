# 模块化重构实施计划

> **状态：已归档（2026-06-10）**
>
> 本文档为历史阶段性实施计划，已从 `docs/design/` 移入归档。
> `PlanPolicy`、`PlanArtifactBuilder`、`WorkItemDeriver` 等抽象已从代码中删除。
>
> 当前状态请参考 [docs/current-state.md](../../current-state.md)。

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 已废止 |
| 创建日期 | 2026-05-30 |
| 分支 | `refactor/modular-architecture` |
| 基于文档 | `docs/wiki/07-modular-refactor-plan.md` |

---

## 1. 概述

### 1.1 重构目标

根据架构师提出的优化建议，本次重构旨在：

1. __删除废弃实现__：不保留旧方案兼容层
2. __拆分过大文件__：降低单文件复杂度
3. __模块化架构__：将系统改造为可替换的模块化架构
4. __重新定位 Brain/Plan/Summary__：明确其在框架中的职责与流程

### 1.2 重构原则

- __硬切换策略__：不做旧方案兼容
- __契约先行__：先定义接口，再实现
- __小步提交__：每个 PR 只改一个模块
- __测试保障__：所有测试始终通过

### 1.3 目标架构

```text
src/
├── contracts/           # 契约层：trait 接口、事件、协议
│   ├── mod.rs
│   ├── dispatch.rs
│   ├── planning.rs
│   ├── execution.rs
│   ├── memory.rs
│   ├── tools.rs
│   └── frontend.rs
├── domain/              # 领域层：拆分后的子域
│   ├── mod.rs
│   ├── task.rs
│   ├── agent.rs
│   ├── execution.rs
│   ├── error.rs
│   ├── workflow.rs
│   ├── tool_runtime.rs
│   ├── work_item.rs
│   └── ...
├── systems/             # 系统层：按模块分目录
│   ├── mod.rs
│   ├── intake/
│   ├── dispatch/
│   ├── planning/
│   ├── execution/
│   ├── tools/
│   ├── memory/
│   └── frontend/
└── plugins/             # 插件层：模块装配
    ├── mod.rs
    ├── task_runtime.rs
    ├── dispatch.rs
    ├── planning.rs
    ├── execution.rs
    ├── tools.rs
    ├── memory.rs
    └── frontend.rs
```text

---

## 2. 阶段划分

```mermaid
flowchart LR
    P0[P0: 文件拆分] --> P1[P1: 契约定义]
    P1 --> P2[P2: Plugin 化]
    P2 --> P3[P3: Brain/Plan/Summary 重构]
```text

| 阶段 | 名称 | 预估工作量 | 风险等级 |
|------|------|------------|----------|
| P0 | 文件拆分 | 2-3 人日 | 中 |
| P1 | 契约定义 | 3-4 人日 | 中高 |
| P2 | Plugin 化 | 2-3 人日 | 中 |
| P3 | Brain/Plan/Summary 重构 | 5-7 人日 | 高 |

---

## 3. 阶段一（P0）：文件拆分

### 3.1 目标

- 恢复代码可维护性
- 建立清晰的模块边界
- 为后续重构奠定基础

### 3.2 任务清单

#### 3.2.1 拆分 `domain/mod.rs`

__当前状态__：1227 行，包含约 15 个核心类型

__目标结构__：
```text
src/domain/
├── mod.rs          # 仅保留 pub use 导出
├── task.rs         # Task, TaskStatus, WaitingReason, FailureReason
├── agent.rs        # Agent, AgentProfile, AgentCapabilities, AgentKind, AgentToolPermissions
├── execution.rs    # AgentExecutionRequest, AgentExecutionResult, AgentExecutionOutput, OutputContent
├── error.rs        # ExecutionError, ToolError
├── workflow.rs     # SubTaskDefinition, SubTaskConfig, SubTaskBatchState, BatchTaskState
├── tool_runtime.rs # ToolCallingState, ConversationMessage, LlmToolCall
├── message.rs      # 所有 Message 类型（Signal, UserInputMessage, etc.）
├── command.rs      # UserCommand
├── confirmation.rs # ConfirmationOption, GrantMode, ApprovalDecision
└── brain.rs        # BrainDecisionOutput, BrainDecisionError
```text

__验收标准__：
- [x] 所有类型迁移到对应文件
- [x] `domain/mod.rs` 仅包含 `pub use` 语句
- [x] 所有测试通过
- [x] `cargo clippy` 无警告

#### 3.2.2 拆分 `systems/tool.rs`

__当前状态__：2027 行，包含 4 个 Tool 实现 + 8 个 System

__目标结构__：
```text
src/systems/tools/
├── mod.rs              # 模块导出 + register_builtin_tools
├── dispatch.rs         # tool_dispatch_system
├── result.rs           # tool_result_system
├── approval.rs         # approval_dispatch_system, approval_result_system
├── confirmation.rs     # tool_confirmation_request_system, tool_confirmation_result_system
├── waiting.rs          # check_waiting_tasks_system, on_subtask_completed_check_waiting
├── orchestrator.rs     # handle_tool_action, spawn_* 辅助函数
└── builtin/
    ├── mod.rs
    ├── knowledge_search.rs
    ├── spawn_agent.rs
    ├── create_tasks.rs
    └── wait_tasks.rs
```text

__验收标准__：
- [x] 每个 Tool 独立一个文件
- [x] 每个System按职责分组
- [x] 所有测试通过
- [x] `cargo clippy` 无警告

#### 3.2.3 拆分 `systems/transform.rs`

__当前状态__：1050 行，11 个 System

__目标结构__：
```text
src/systems/transform/
├── mod.rs                  # 模块导出
├── signal_ingest.rs        # signal_ingest_system
├── llm_response.rs         # llm_response_system, tool_calling_orchestrator_system
├── brain_decision.rs       # brain_decision_system
├── task_lifecycle.rs       # task_termination_system, retry_ready_system, finish_task_system
├── subtask.rs              # sub_task_batch_block_system, sub_task_completion_system
└── task_creation.rs        # user_message_to_task_system
```text

__验收标准__：
- [x] 系统按职责分组
- [x] 所有测试通过
- [x] `cargo clippy` 无警告

#### 3.2.4 拆分 `systems/dispatch.rs`

__当前状态__：644 行，混合 Brain 和普通任务分发

__目标结构__：
```text
src/systems/dispatch/
├── mod.rs              # 模块导出
├── task_dispatch.rs    # task_dispatch_system（普通任务）
├── brain_dispatch.rs   # brain_dispatch_system（Brain 决策）
└── agent_selection.rs  # select_agent_with_memory, match_score 等辅助函数
```text

__验收标准__：
- [x] Brain 和普通分发分离
- [x] 所有测试通过
- [x] `cargo clippy` 无警告

### 3.3 执行顺序

```text
1. domain/mod.rs 拆分（最底层，无依赖）
2. systems/tool.rs 拆分（依赖 domain）
3. systems/transform.rs 拆分（依赖 domain + tool）
4. systems/dispatch.rs 拆分（依赖 domain）
```text

### 3.4 迁移映射表

#### `domain/mod.rs` 类型映射

| 类型 | 目标文件 | 行数范围 |
|------|----------|----------|
| `SignalType`, `Signal`, `SignalPayload` | `message.rs` | 44-183 |
| `WaitingReason`, `FailureReason`, `TaskStatus` | `task.rs` | 51-91 |
| `Task` | `task.rs` | 408-632 |
| `Agent`, `AgentProfile`, `AgentCapabilities`, `AgentKind` | `agent.rs` | 222-309 |
| `AgentToolPermissions`, `AgentExperience` | `agent.rs` | 241-281 |
| `AgentExecutionRequest`, `AgentExecutionResult`, `AgentExecutionOutput` | `execution.rs` | 311-406 |
| `ExecutionError`, `ToolError` | `error.rs` | 634-720 |
| `ToolCallingState`, `ConversationMessage`, `LlmToolCall` | `tool_runtime.rs` | 320-378, 927-941 |
| `SubTaskDefinition`, `SubTaskConfig`, `SubTaskBatchState` | `workflow.rs` | 767-873 |
| `ConfirmationOption`, `GrantMode`, `ApprovalDecision` | `confirmation.rs` | 977-1095 |
| `UserCommand` | `command.rs` | 1097-1145 |
| `BrainDecisionOutput`, `BrainDecisionError` | `brain.rs` | 729-743 |
| 所有 `*Message` 类型 | `message.rs` | 各处分散 |

---

## 4. 阶段二（P1）：契约定义

### 4.1 目标

- 定义稳定接口，支撑模块替换
- 明确模块间依赖关系
- 为 Plugin 化奠定基础

### 4.2 契约层结构

```text
src/contracts/
├── mod.rs           # 导出所有契约
├── dispatch.rs      # DispatchPolicy, AgentSelector, AssignmentResult, TagMatcher
├── planning.rs      # PlanPolicy, PlanArtifactBuilder, ReplanPolicy, WorkItemDeriver
├── execution.rs     # ExecutionBackend, ExecutionPolicy
├── memory.rs        # MemoryStore, MemoryCompactor, CompactionPolicy, ContributionPolicy
├── tools.rs         # ToolCatalog, ToolExecutor, ToolApprovalPolicy
└── frontend.rs      # FrontendProjection（扩展现有 Frontend）
```text

### 4.3 任务清单

#### 4.3.1 定义 Dispatch 契约

```rust
// src/contracts/dispatch.rs

use crate::domain::{AgentId, TaskId, WorkItem, WorkItemType};

/// WorkItem 的标签集合
#[derive(Debug, Clone, Default)]
pub struct TagSet {
    pub tags: Vec<String>,
}

/// Agent 的可见能力摘要
#[derive(Debug, Clone)]
pub struct AgentCapabilitySummary {
    pub agent_id: AgentId,
    pub name: String,
    pub tags: Vec<String>,
    pub model: String,
}

/// 派发上下文
#[derive(Debug, Clone)]
pub struct DispatchContext {
    pub task_id: TaskId,
    pub work_type: WorkItemType,
    pub available_agents: Vec<AgentCapabilitySummary>,
}

/// 分配结果
#[derive(Debug, Clone)]
pub struct AssignmentResult {
    pub agent_id: AgentId,
    pub reasoning: String,
}

/// 标签匹配器
pub trait TagMatcher: Send + Sync + 'static {
    fn matches(&self, agent_tags: &[String], required_tags: &TagSet) -> bool;
}

/// 候选 Agent 选择器
pub trait AgentSelector: Send + Sync + 'static {
    fn select_candidates(
        &self,
        work_item: &WorkItem,
        agents: &[AgentCapabilitySummary],
    ) -> Vec<AgentCapabilitySummary>;
}

/// 派发策略
pub trait DispatchPolicy: Send + Sync + 'static {
    fn assign(
        &self,
        work_item: &WorkItem,
        context: &DispatchContext,
    ) -> Option<AssignmentResult>;
}
```text

__验收标准__：
- [ ] trait 定义完成
- [ ] 支持多标签组合匹配
- [ ] 匹配规则采用“Agent tags 全包含 WorkItem tags”
- [ ] 提供 Mock 实现
- [ ] 单元测试通过

#### 4.3.2 定义 Planning 契约

```rust
// src/contracts/planning.rs

use crate::{
    contracts::dispatch::{AgentCapabilitySummary, TagSet},
    domain::{TaskId, WorkItemType},
};

/// 规划结果
pub struct PlanArtifact {
    pub steps: Vec<PlanStep>,
    pub subtasks: Vec<SubtaskSpec>,
    pub dependencies: Vec<(String, String)>, // (from, to)
}

pub struct PlanStep {
    pub name: String,
    pub description: String,
    pub estimated_complexity: Complexity,
}

pub struct SubtaskSpec {
    pub name: String,
    pub content: String,
    pub work_type: WorkItemType,
    pub tags: TagSet,
    pub required_tools: Vec<String>,
    pub depends_on: Vec<String>,
}

pub enum Complexity {
    Low,
    Medium,
    High,
}

/// 判断是否需要规划
pub trait PlanPolicy: Send + Sync + 'static {
    fn should_plan(&self, task_content: &str, context: &PlanContext) -> bool;
}

/// 构建规划结果
pub trait PlanArtifactBuilder: Send + Sync + 'static {
    fn build(&self, raw_output: &str) -> Result<PlanArtifact, PlanError>;
}

/// 判断是否需要重新规划
pub trait ReplanPolicy: Send + Sync + 'static {
    fn should_replan(&self, event: &ReplanEvent) -> bool;
}

/// 将规划结果标准化为后续 WorkItem 草案
pub trait WorkItemDeriver: Send + Sync + 'static {
    fn derive(&self, task_id: TaskId, artifact: &PlanArtifact) -> Vec<PlannedWorkItemSpec>;
}

pub struct PlannedWorkItemSpec {
    pub name: String,
    pub work_type: WorkItemType,
    pub prompt: String,
    pub tags: TagSet,
    pub required_tools: Vec<String>,
    pub depends_on: Vec<String>,
}

pub struct PlanContext {
    pub task_id: TaskId,
    pub stm_entries: usize,
    pub available_agents: Vec<AgentCapabilitySummary>,
}

pub enum ReplanEvent {
    SubtaskFailed { subtask_name: String, error: String },
    SubtaskBlocked { subtask_name: String, reason: String },
    ContextChanged { change: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
}
```text

__验收标准__：
- [ ] trait 定义完成
- [ ] 明确 `PlanArtifact -> WorkItem` 协议
- [ ] 提供 Mock 实现
- [ ] 单元测试通过

#### 4.3.3 定义 Memory 契约

```rust
// src/contracts/memory.rs

use crate::{
    contracts::dispatch::TagSet,
    domain::{AgentId, MemoryEntry, TaskId, WorkItemType},
};

/// 长期记忆存储
pub trait MemoryStore: Send + Sync + 'static {
    fn get_entries(&self, agent_id: AgentId) -> Vec<MemoryEntry>;
    fn add_entry(&mut self, agent_id: AgentId, entry: MemoryEntry);
    fn remove_entry(&mut self, agent_id: AgentId, entry_id: Uuid);
}

/// 记忆治理协调器
pub trait MemoryCompactor: Send + Sync + 'static {
    fn build_summary_request(
        &self,
        context: &MemoryCompactionContext,
    ) -> Option<SummaryWorkRequest>;
}

/// 压缩策略
pub trait CompactionPolicy: Send + Sync + 'static {
    fn should_compact(&self, context: &MemoryCompactionContext) -> bool;
    fn target_tokens(&self, context: &MemoryCompactionContext) -> usize;
    fn preserve_recent_turns(&self) -> usize;
}

/// 经验沉淀策略
pub trait ContributionPolicy: Send + Sync + 'static {
    fn decide_writeback(&self, result: &SummaryResult) -> WritebackDecision;
}

pub struct MemoryCompactionContext {
    pub task_id: TaskId,
    pub owner_agent_id: Option<AgentId>,
    pub content_to_compress: String,
    pub token_count: usize,
    pub trigger: CompressionTrigger,
}

pub struct SummaryWorkRequest {
    pub task_id: TaskId,
    pub work_type: WorkItemType,
    pub prompt: String,
    pub tags: TagSet,
    pub target_tokens: usize,
    pub writeback_target: MemoryWritebackTarget,
}

pub enum CompressionTrigger {
    TokenThreshold,
    TaskComplete,
    UserCommand,
}

pub enum MemoryWritebackTarget {
    ShortTermContext,
    LongTermMemory { agent_id: AgentId },
    SharedKnowledge,
}

pub struct SummaryResult {
    pub task_id: TaskId,
    pub content: String,
}

pub enum WritebackDecision {
    UpdateShortTermContext,
    AddLongTermMemory(MemoryEntry),
    AddSharedKnowledge(MemoryEntry),
    Drop,
}
```text

__验收标准__：
- [ ] trait 定义完成
- [ ] 明确 `MemoryCompactor -> Summary WorkItem` 协议
- [ ] 提供 Mock 实现
- [ ] 单元测试通过

#### 4.3.4 定义 Tool 契约

```rust
// src/contracts/tools.rs

use crate::domain::{AgentId, ToolDefinition, ToolPermission};

/// 工具目录
pub trait ToolCatalog: Send + Sync + 'static {
    fn list_tools(&self) -> Vec<ToolDefinition>;
    fn get_tool(&self, name: &str) -> Option<&ToolDefinition>;
    fn filter_by_permission(&self, agent_id: AgentId, permission: ToolPermission) -> Vec<ToolDefinition>;
}

/// 工具审批策略
pub trait ToolApprovalPolicy: Send + Sync + 'static {
    fn determine_approval_route(&self, tool_name: &str, agent: &Agent) -> ApprovalRoute;
}

pub enum ApprovalRoute {
    AutoAllow,
    UserConfirmation,
    ParentApproval { parent_agent_id: AgentId },
    Deny,
}
```text

__验收标准__：
- [ ] trait 定义完成
- [ ] 提供 Mock 实现
- [ ] 单元测试通过

#### 4.3.5 定义 Execution 契约

```rust
// src/contracts/execution.rs

use crate::domain::{AgentExecutionRequest, AgentExecutionOutput, ExecutionError};

/// 执行后端
pub trait ExecutionBackend: Send + Sync + 'static {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutionFuture;
}

pub type ExecutionFuture = std::pin::Pin<
    Box<std::future::Future<Output = Result<AgentExecutionOutput, ExecutionError>> + Send>
>;

/// 执行策略
pub trait ExecutionPolicy: Send + Sync + 'static {
    fn max_retries(&self) -> u32;
    fn retry_delay(&self, retry_count: u32) -> std::time::Duration;
    fn timeout(&self) -> std::time::Duration;
}
```text

__验收标准__：
- [ ] trait 定义完成
- [ ] 现有 `AgentExecutor` 迁移到此契约
- [ ] 单元测试通过

### 4.4 现有实现适配

| 现有实现 | 适配到契约 |
|----------|------------|
| `brain_dispatch_system` 中的选择逻辑 | 拆到 `DispatchPolicy` + `AgentSelector` |
| `Frontend` trait | 保持不变，扩展 `FrontendProjection` |
| `AgentExecutor` trait | 迁移到 `ExecutionBackend` |
| `BuiltinTool` trait | 保持不变 |
| `SpaceToolRegistry` | 实现 `ToolCatalog` |
| `MemoryConfig` | 实现 `CompactionPolicy` |

---

## 5. 阶段三（P2）：Plugin 化

### 5.1 目标

- 以 Bevy Plugin 为模块装载单位
- 清晰的模块组合方式
- 支持模块替换

### 5.2 目标结构

```text
src/plugins/
├── mod.rs                    # 导出所有 Plugin
├── task_runtime.rs           # TaskRuntimePlugin
├── dispatch.rs               # DispatchPlugin
├── planning.rs               # PlanningPlugin
├── execution.rs              # ExecutionPlugin
├── tools.rs                  # ToolRuntimePlugin
├── memory.rs                 # MemoryPlugin
├── frontend.rs               # FrontendPlugin
└── default_runtime.rs        # DefaultRuntimePluginGroup
```text

### 5.3 任务清单

#### 5.3.1 创建 TaskRuntimePlugin

```rust
// src/plugins/task_runtime.rs

use bevy::prelude::*;

pub struct TaskRuntimePlugin;

impl Plugin for TaskRuntimePlugin {
    fn build(&self, app: &mut App) {
        // 注册 Task 相关 Resource
        app.init_resource::<TaskEvaluationConfig>();
        
        // 注册 Task 相关 System
        app.add_systems(Update, (
            task_termination_system,
            retry_ready_system,
            finish_task_system,
        ).in_set(HarnessSet::Transform));
    }
}
```text

#### 5.3.2 创建 DispatchPlugin

```rust
// src/plugins/dispatch.rs

use bevy::prelude::*;

pub struct DispatchPlugin;

impl Plugin for DispatchPlugin {
    fn build(&self, app: &mut App) {
        // 注册派发策略资源
        app.insert_resource(DispatchServices::default());

        // 注册派发相关 System
        app.add_systems(Update, (
            brain_dispatch_system.in_set(HarnessSet::Dispatch),
            task_dispatch_system.in_set(HarnessSet::Dispatch),
        ));
    }
}
```text

#### 5.3.3 创建 ToolRuntimePlugin

```rust
// src/plugins/tools.rs

use bevy::prelude::*;

pub struct ToolRuntimePlugin;

impl Plugin for ToolRuntimePlugin {
    fn build(&self, app: &mut App) {
        // 注册 Tool 相关 Resource
        app.init_resource::<SpaceToolRegistry>();
        app.init_resource::<BuiltinToolExecutors>();
        
        // 注册内置 Tools
        let mut registry = app.world.resource_mut::<SpaceToolRegistry>();
        let mut executors = app.world.resource_mut::<BuiltinToolExecutors>();
        register_builtin_tools(&mut registry, &mut executors);
        
        // 注册 Tool 相关 System
        app.add_systems(Update, (
            tool_dispatch_system.in_set(HarnessSet::Dispatch),
            tool_result_system.in_set(HarnessSet::Transform),
            // ...
        ));
    }
}
```text

#### 5.3.4 创建 DefaultRuntimePluginGroup

```rust
// src/plugins/default_runtime.rs

use bevy::prelude::*;

pub struct DefaultRuntimePluginGroup;

impl PluginGroup for DefaultRuntimePluginGroup {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(TaskRuntimePlugin)
            .add(DispatchPlugin)
            .add(ToolRuntimePlugin)
            .add(ExecutionPlugin)
            .add(MemoryPlugin)
            .add(FrontendPlugin)
            .add(PlanningPlugin)
    }
}
```text

#### 5.3.5 重构 build_harness_app

```rust
// src/app/mod.rs

pub fn build_harness_app(
    config: HarnessConfig,
    runtime: Arc<Runtime>,
    executor: Arc<dyn AgentExecutor>,
    input_rx: Receiver<crate::domain::ExternalInput>,
    frontends: Vec<Box<dyn Frontend>>,
) -> App {
    let mut app = App::new();
    
    // 基础 Resource
    app.insert_resource(InputReceiver(input_rx));
    app.insert_resource(FrontendRegistry { frontends });
    app.insert_resource(AsyncRuntime(runtime));
    app.insert_resource(ExecutorHandle(executor));
    // ...
    
    // 注册 PluginGroup
    app.add_plugins(DefaultRuntimePluginGroup);
    
    app
}
```text

### 5.4 验收标准

- [ ] 所有 Plugin 创建完成
- [ ] `build_harness_app` 使用 PluginGroup
- [ ] 所有测试通过
- [ ] `cargo clippy` 无警告

---

## 6. 阶段四（P3）：Brain/Plan/Summary 重构

### 6.1 目标

- Brain 改造为纯派发模块
- Plan 改造为独立规划模块
- Summary 改造为记忆治理模块
- 引入 WorkItem 统一工作单元

### 6.2 核心概念

#### 6.2.1 WorkItem

```rust
// src/domain/work_item.rs

use uuid::Uuid;
use crate::{
    contracts::dispatch::TagSet,
    domain::{AgentId, TaskId},
};

/// 统一工作单元
#[derive(Debug, Clone, Component)]
pub struct WorkItem {
    pub id: Uuid,
    pub task_id: TaskId,
    pub work_type: WorkItemType,
    pub input: WorkItemInput,
    pub tags: TagSet,
    pub status: WorkItemStatus,
    pub assigned_agent: Option<AgentId>,
    pub origin: WorkItemOrigin,
    pub writeback_target: WorkItemWritebackTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkItemType {
    Planning,
    Execution,
    Summarization,
    Evaluation,
}

#[derive(Debug, Clone)]
pub struct WorkItemInput {
    pub prompt: String,
    pub context: WorkItemContext,
}

#[derive(Debug, Clone, Default)]
pub struct WorkItemContext {
    pub conversation: Option<Vec<ConversationMessage>>,
    pub tools: Vec<ToolDefinition>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub enum WorkItemOrigin {
    UserTask,
    PlanArtifact,
    MemoryCompaction,
    Evaluation,
}

#[derive(Debug, Clone)]
pub enum WorkItemWritebackTarget {
    TaskResult,
    PlanArtifact,
    ShortTermContext,
    LongTermMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkItemStatus {
    Pending,
    Assigned,
    Running,
    Completed,
    Failed,
}
```text

#### 6.2.2 BrainDispatch 模块

```rust
// src/systems/dispatch/brain_dispatch.rs

#[derive(Resource)]
pub struct DispatchServices {
    pub policy: Arc<dyn DispatchPolicy>,
}

/// Brain 派发系统：为未分配工作项选择 Agent
pub fn brain_dispatch_system(
    mut commands: Commands,
    mut work_items: Query<(Entity, &mut WorkItem)>,
    agents: Query<&Agent>,
    dispatch_services: Res<DispatchServices>,
) {
    for (entity, mut work_item) in &mut work_items {
        if work_item.status != WorkItemStatus::Pending {
            continue;
        }

        // BrainDispatch 自身不被递归派发；其他工作项按多标签规则筛选候选 Agent
        let context = build_dispatch_context(&work_item, &agents);
        let selected = dispatch_services.policy.assign(&work_item, &context);

        if let Some(result) = selected {
            let agent_id = result.agent_id;
            work_item.assigned_agent = Some(agent_id);
            work_item.status = WorkItemStatus::Assigned;

            // 生成后续执行请求或执行消息，继续交给执行模块处理
            commands.spawn(AgentExecutionRequestMessage {
                request: build_request(&work_item, agent_id),
            });
        } else {
            commands.entity(entity).insert(DispatchFailedMarker);
        }
    }
}
```text

__关键约束__：

- `BrainDispatch` 是模块，不是普通规划 Agent。
- `BrainDispatch` 自身固定绑定 `BrainAgent` 用于复杂派发决策。
- 除 `BrainDispatch` 外，`plan`、`summary`、`worker` 等工作项都通过多标签匹配动态选择 Agent。
- 第一版多标签匹配只要求：`Agent.tags` 必须包含 `WorkItem.tags` 的全部元素。

### 6.3 任务清单

#### 6.3.1 引入 WorkItem

- [x] 创建 `src/domain/work_item.rs`
- [x] 定义 `WorkItem`, `WorkItemType`, `WorkItemStatus`
- [x] 定义 `TagSet`, `WorkItemOrigin`, `WorkItemWritebackTarget`
- [x] 创建 `WorkItemCreatedMessage`, `WorkItemCompletedMessage` 消息类型
- [x] 添加 WorkItem 到 ECS 测试

#### 6.3.2 重构 BrainDispatch

- [x] 创建 `src/systems/dispatch/brain_dispatch.rs`
- [x] 创建 `src/contracts/dispatch.rs`
- [x] 实现 `DispatchPolicy` trait
- [x] 实现多标签匹配规则
- [x] 采用”全包含匹配”作为第一版默认规则
- [x] 固化 `BrainDispatch -> BrainAgent` 的固定绑定约束（通过 Tag 查找，选择配置中最前的）
- [ ] 重写 `brain_dispatch_system` 使用 WorkItem（保留 Task 向后兼容）
- [ ] 更新所有调用点

#### 6.3.3 重构 Plan 模块

- [x] 创建 `src/contracts/planning.rs`
- [x] 定义 `PlanArtifact`, `WorkItemDeriver`
- [ ] 将规划结果统一转化为 `Planning WorkItem / Worker WorkItem`
- [ ] 验证 `Task -> PlanArtifact -> WorkItem` 流程闭环

#### 6.3.4 重构 MemoryCompactor

- [x] 创建 `src/contracts/memory.rs`（MemoryCompactor trait 已定义）
- [x] 实现 `MemoryCompactor` trait（DefaultCompactionPolicy）
- [x] 分离压缩触发和压缩执行（memory_compression_system 和 summarization_dispatch_system）
- [ ] 让 `MemoryCompactor` 生成 `Summary WorkItem`
- [x] 让 `ContributionPolicy` 负责摘要结果写回决策
- [x] 更新 `summarization_dispatch_system`（使用 TagBasedSelector 选择 Summarizer）

#### 6.3.5 流程验证

- [ ] 更新集成测试
- [ ] 验证 `Plan -> WorkItem -> BrainDispatch -> Agent` 流程
- [ ] 验证 `Worker -> BrainDispatch -> Agent` 流程
- [ ] 验证 `Summary -> WorkItem -> BrainDispatch -> Agent` 流程
- [ ] 验证多标签匹配与回退策略

### 6.4 迁移路径

```text
当前流程:
Task → Brain Agent → select agent → execute

目标流程:
Task → PlanPolicy
     → Planning WorkItem → BrainDispatch → Planning Agent
     → PlanArtifact → Worker WorkItem → BrainDispatch → Worker Agent
     → MemoryCompactor → Summary WorkItem → BrainDispatch → Summary Agent
```text

### 6.5 验收标准

- [x] WorkItem 概念完整实现
- [ ] Brain 仅负责派发，不做规划（保留现有逻辑向后兼容）
- [x] Plan 独立为 Planning 模块（契约层完成）
- [x] Summary 作为 MemoryCompactor 模块（契约层完成）
- [x] `BrainDispatch` 通过 Tag 查找 BrainAgent，选择配置中最前的
- [x] 普通工作项支持多标签组合匹配
- [x] 所有测试通过
- [ ] 文档更新完成

---

## 7. 风险与缓解

### 7.1 风险列表

| 风险 | 概率 | 影响 | 缓解措施 |
|------|------|------|----------|
| 循环依赖 | 中 | 高 | 引入 contracts 层解耦 |
| 测试失败 | 低 | 高 | 每个 PR 运行完整测试 |
| 性能回退 | 低 | 中 | 保持 ECS 数据驱动模式 |
| 重构范围蔓延 | 中 | 高 | 严格按阶段执行，不新增需求 |

### 7.2 回滚策略

- 每个阶段完成后创建 tag
- 如遇重大问题可回退到上一阶段
- 保留原分支 `feat/space-knowledge-retrieval` 作为参考

---

## 8. 进度追踪

### 8.1 阶段完成标准

| 阶段 | 完成标准 | 状态 |
|------|----------|------|
| P0 | 文件拆分完成，所有测试通过 | ✅ 已完成 |
| P1 | 契约定义完成，Mock 实现可用 | ✅ 已完成 |
| P2 | Plugin 化完成，build_harness_app 使用 PluginGroup | ✅ 已完成 |
| P3 | Brain/Plan/Summary 重构完成，流程验证通过 | 🔄 进行中（契约层完成，WorkItem 集成待完成） |

### 8.2 提交规范

每个阶段使用独立的 PR：

- `refactor(p0): split domain and systems files`
- `refactor(p1): add contracts layer`
- `refactor(p2): convert to plugin architecture`
- `refactor(p3): redesign brain plan and summary modules`

---

## 9. 参考资料

- [架构优化建议](../wiki/07-modular-refactor-plan.md)
- [Bevy Plugin 文档](https://docs.rs/bevy/latest/bevy/app/trait.Plugin.html)
- [Conventional Commits](https://www.conventionalcommits.org/)
