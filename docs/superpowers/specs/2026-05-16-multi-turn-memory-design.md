# Phase 4.1: 多轮对话与双层记忆设计

本文档描述 Phase 4.1 多轮对话能力的详细设计，包括 Task 多轮状态机、双层记忆架构、记忆传承机制和评估器设计。

---

## 一、设计目标

### 核心目标

1. __Task 作为目标载体__ — 一个 Task 代表一个目标，多轮对话围绕目标展开，Task 不在每轮结束时进入终态
2. __双层记忆架构__ — 短期记忆绑定 Task，长期记忆绑定 Agent
3. __统一 Agent 模型__ — 所有 Agent 遵循相同逻辑，差异仅来自位置和特权
4. __评估器机制__ — 判定任务结束或执行偏离
5. __缓存友好__ — 上下文组织支持 LLM Provider 的缓存命中优化

### Phase 4.1 范围

| 包含 | 不包含（Phase 4.2） |
|------|---------------------|
| Task 多轮状态机 | 向量存储集成 |
| 短期/长期记忆实体与存储 | 向量化检索 |
| 混合容量管理策略 | 语义相似度检索 |
| 关键词检索 | |
| 评估器 Agent | |
| 记忆传承机制 | |
| LLM 评估贡献 | |

---

## 二、Agent 统一模型

### 核心原则

__所有 Agent 遵循相同逻辑，差异仅来自位置和特权。__

### 统一能力

所有 Agent 都拥有：

| 能力 | 说明 |
|------|------|
| 长期记忆 | 可接收子 Agent 贡献，可被压缩存入 |
| 短期记忆 | 执行任务时使用（若执行任务） |
| 创建子 Agent | 受 tags 子集约束 |
| 接收子 Agent 贡献 | 子 Agent 销毁时评估吸收 |
| 销毁时贡献父 Agent | 将自身长期记忆贡献给父 Agent |

### 差异来源

差异不由类型硬编码，而是由__位置__和__特权__决定：

| 维度 | 持久性 Agent | 任务型 Agent |
|------|-------------|-------------|
| 有父 Agent | 否（顶级） | 是 |
| 会销毁 | 否 | 是（Task 终态） |
| 执行具体任务 | 可配置 | 可配置 |
| 知识去向 | 自身保留（无父） | 贡献给父 Agent |

### 行为推导

__持久性 Agent：__

- 无父 Agent → 长期记忆不会被贡献出去 → 知识沉淀
- 不销毁 → 持续接收子 Agent 贡献 → 知识富集
- 可配置不执行任务 → 成为组织者/管理者

__任务型 Agent：__

- 有父 Agent → 销毁时贡献知识 → 知识向上流动
- 绑定 Task → 执行具体任务 → 产生知识
- 销毁后记忆传承 → 形成知识树

### 知识流动方向

```text
持久性 Agent (顶级，无父)
    ▲
    │ 最终归宿
    │
任务型 Agent A
    ▲
    │ 贡献
    │
任务型 Agent A1 (A 的子 Agent)
    ▲
    │ 贡献
    │
任务型 Agent A1-1 (A1 的子 Agent)
```text

知识始终向上流动，最终汇聚到无父的顶级 Agent。

---

## 三、Task 状态机

### 设计原则

状态机只描述 Task 的执行过程，不关心"由谁决定执行者"。

### 统一状态流转

__执行状态：__

- __Ready__ — 任务就绪，等待执行
- __Running__ — Agent 正在执行

__等待状态：__

- __Waiting(User)__ — 等待用户输入
- __Waiting(Evaluator)__ — 等待评估器判定
- __Waiting(RetryBackoff)__ — 重试退避

__终态：__

- __Done__ — 任务完成
- __Failed__ — 任务失败

### 状态转换规则

| 当前状态 | 触发条件 | 目标状态 |
|----------|----------|----------|
| Ready | 开始执行 | Running |
| Running | 执行完成，需要用户输入 | Waiting(User) |
| Running | 执行完成，触发评估器 | Waiting(Evaluator) |
| Running | 执行完成，任务完成 | Done |
| Running | 执行失败，可重试 | Waiting(RetryBackoff) |
| Running | 执行失败，不可重试 | Failed |
| Waiting(User) | 用户输入到达 | Ready |
| Waiting(Evaluator) | 评估器：继续 | Ready |
| Waiting(Evaluator) | 评估器：完成 | Done |
| Waiting(Evaluator) | 评估器：失败 | Failed |
| Waiting(RetryBackoff) | 退避时间到 | Ready |

### "由谁执行"的决策时机

决策发生在 __Task 创建时__，而非状态机中：

| Task 来源 | 执行者决策 |
|-----------|-----------|
| 用户输入创建 | Brain 决定（或默认规则） |
| Agent 创建子 Task | 创建时已指定子 Agent |

Brain 只是顶级 Task 的调度机制，不进入 Task 状态机。

### WaitingReason 扩展

```rust
pub enum WaitingReason {
    Agent,        // 等待 Agent 执行（现有）
    User,         // 等待用户输入（新增）
    Evaluator,    // 等待评估器判定（新增）
    RetryBackoff, // 重试退避（现有）
}
```text

> __注意__：移除 `Waiting(Brain)`。Brain 决策发生在 Task 创建前，不进入 Task 状态机。

---

## 四、记忆实体设计

### 设计原则

记忆作为 Component 嵌入 Task/Agent，而非独立 Entity。

__理由：__

- 生命周期强绑定：短期记忆与 Task 共存亡，长期记忆与 Agent 共存亡
- 查询简洁：执行时直接通过 Task/Agent 获取记忆
- 清理简单：Task/Agent despawn 时记忆自动清理

### ShortTermMemory

```rust
#[derive(Component, Default)]
pub struct ShortTermMemory {
    /// 完整对话条目（追加模式，早期不变）
    pub entries: Vec<MemoryEntry>,

    /// 当前轮次
    pub turn_count: u32,

    /// 中期摘要（替换早期对话，但作为前缀保持稳定）
    pub summary_prefix: Option<String>,

    /// 摘要覆盖的轮次范围 [start, end)
    pub summary_range: Option<(u32, u32)>,

    /// 最后一次缓存命中的 token 数（用于监控）
    pub last_cached_tokens: Option<u32>,
}
```text

### LongTermMemory

```rust
#[derive(Component, Default)]
pub struct LongTermMemory {
    pub entries: Vec<MemoryEntry>,
}
```text

### MemoryEntry

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub turn: u32,
    pub role: EntryRole,
    pub content: String,
    pub metadata: EntryMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryRole {
    User,      // 用户输入
    Assistant, // Agent 回复
    Summary,   // 摘要（中期压缩）
    Archive,   // 归档（远期压缩）
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntryMetadata {
    pub tool_calls: Vec<ToolCall>,
    pub resources: Vec<String>,
    pub reasoning: Option<String>,
    pub keywords: Vec<String>,
}
```text

### ToolCall

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_name: String,
    pub input: String,
    pub output: String,
    pub timestamp: DateTime<Utc>,
}
```text

### Task 与记忆的关联

- __Task Entity__ 同时拥有 `Task` 和 `ShortTermMemory` 两个 Component
- __Agent Entity__ 同时拥有 `Agent` 和 `LongTermMemory` 两个 Component
- Task 创建时自动添加 `ShortTermMemory::default()`
- 任务型 Agent 创建时自动添加 `LongTermMemory::default()`
- 持久性 Agent 从配置加载时添加 `LongTermMemory::default()`

### 统一设计体现

| 特性 | 短期记忆 | 长期记忆 |
|------|----------|----------|
| 实体结构 | 相同 | 相同 |
| 绑定对象 | Task | Agent |
| 生命周期 | Task 结束时处理 | Agent 销毁时贡献 |
| 存储方式 | Component | Component |

---

## 五、缓存友好的上下文组织

### LLM 缓存机制

主流 Provider 的缓存策略：

| Provider | 缓存边界 | 命中条件 |
|----------|----------|----------|
| OpenAI | System prompt + 历史消息前缀 | 前 N 个 token 不变 |
| Anthropic | System prompt + 对话开头 | 前缀匹配 |
| DeepSeek | 类似 OpenAI | 前缀匹配 |

__核心原理__：对话越早期的内容越稳定，越适合缓存。

### 上下文结构

```text
┌─────────────────────────────────────────────────────────────┐
│                      Context Window                          │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ [缓存区]                                               │  │
│  │ System Prompt (Agent 配置)                             │  │
│  │ 长期记忆摘要 (跨任务知识)                               │  │
│  │ 任务目标 (Task.content)                                │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ [缓存区 - 近期对话]                                     │  │
│  │ 第 1 轮: 用户输入 + Agent 回复                          │  │
│  │ 第 2 轮: 用户输入 + Agent 回复                          │  │
│  │ ...                                                    │  │
│  │ 第 N 轮: 用户输入 + Agent 回复                          │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ [非缓存区 - 当前轮]                                     │  │
│  │ 第 N+1 轮: 用户输入 (待处理)                            │  │
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```text

### 构建上下文方法

```rust
impl ShortTermMemory {
    /// 构建发送给 LLM 的完整上下文
    pub fn build_context(
        &self,
        agent: &Agent,
        task: &Task,
        long_term: &LongTermMemory,
    ) -> Vec<Message> {
        let mut messages = Vec::new();

        // 1. System Prompt（最稳定）
        messages.push(Message::system(&agent.system_prompt));

        // 2. 长期记忆摘要（相对稳定）
        if !long_term.entries.is_empty() {
            messages.push(Message::system(long_term.to_summary()));
        }

        // 3. 任务目标（稳定）
        messages.push(Message::system(format!("任务目标: {}", task.content)));

        // 4. 中期摘要（替换早期对话，但保持前缀稳定）
        if let Some(summary) = &self.summary_prefix {
            messages.push(Message::system(summary));
        }

        // 5. 近期对话（从摘要范围之后开始）
        let start_turn = self.summary_range.map(|(_, end)| end).unwrap_or(0);
        for entry in &self.entries {
            if entry.turn >= start_turn {
                messages.extend(entry.to_messages());
            }
        }

        messages
    }
}
```text

### 缓存命中优化策略

1. __摘要替换时保持前缀__：
   - 早期对话压缩为摘要后，摘要放在固定位置
   - 新对话追加，不影响已缓存的前缀

2. __分批追加__：
   - 每轮对话完成后追加到 entries
   - 不修改已有 entry

3. __摘要触发时机__：
   - 轮数达到阈值时触发摘要
   - 摘要替换前 N 轮，保留后 M 轮原文

---

## 六、记忆容量管理

### 分层策略

| 层级 | 轮数范围 | 存储方式 |
|------|----------|----------|
| 近期 | 0 ~ N 轮 | 全量保留 |
| 中期 | N ~ M 轮 | 滚动摘要 |
| 远期 | M+ 轮 | 压缩存入长期记忆 |

### 容量配置

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    /// 近期全量保留轮数（默认 5）
    pub recent_turns: u32,
    /// 中期摘要触发阈值（默认 10）
    pub compression_threshold: u32,
    /// 摘要覆盖轮数（默认 5）
    pub summary_window: u32,
}
```text

### 摘要生成流程

1. 轮数达到 `compression_threshold` 时触发
2. 将 `summary_window` 轮的早期对话发送给 LLM 生成摘要
3. 摘要替换早期对话，更新 `summary_prefix` 和 `summary_range`
4. 远期摘要定期压缩存入 Agent 长期记忆

---

## 七、评估器 Agent

### 职责

评估器 Agent 负责判断 Task 是否应该结束或执行是否偏离目标。

### 触发条件（可配置组合）

| 条件 | 说明 |
|------|------|
| Agent 申请 | Agent 在回复中携带标记请求评估 |
| 执行轮数阈值 | 轮数达到配置上限 |
| 用户请求 | 用户输入"结束"等指令 |

### 评估结果

| 结果 | 含义 | 后续动作 |
|------|------|----------|
| Continue | 任务继续执行 | Task → Ready |
| Complete | 任务已完成 | Task → Done |
| Failed | 任务无法完成 | Task → Failed |
| OffTrack | 执行偏离目标 | 根据策略修正或失败 |

### 评估请求结构

```rust
#[derive(Debug, Clone, Component)]
pub struct EvaluationRequestMessage {
    pub task_id: TaskId,
    pub trigger: EvaluationTrigger,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvaluationTrigger {
    AgentRequested,
    TurnLimitReached,
    UserRequested,
}
```text

### 评估结果结构

```rust
#[derive(Debug, Clone, Component)]
pub struct EvaluationResultMessage {
    pub task_id: TaskId,
    pub result: EvaluationResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub decision: EvaluationDecision,
    pub reasoning: String,
    pub suggested_action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EvaluationDecision {
    Continue,
    Complete,
    Failed,
    OffTrack,
}
```text

### Task 评估配置

```rust
#[derive(Debug, Clone)]
pub struct TaskEvaluationConfig {
    pub enabled: bool,
    pub max_turns: Option<u32>,
    pub evaluator_agent: AgentId,
    pub offtrack_policy: OffTrackPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffTrackPolicy {
    AutoCorrect,
    AskUser,
    Fail,
}
```text

### 评估器 Agent 选择

评估器 Agent 通过配置指定名称，在运行时按名称查找：

```rust
fn find_evaluator_agent(agents: &Query<&Agent>, name: &str) -> Option<AgentId> {
    agents.iter()
        .find(|a| a.profile.name == name)
        .map(|a| a.id)
}
```text

评估器可以是持久性 Agent（共享评估经验）或任务型 Agent（隔离评估上下文）。

### Agent 申请评估的标记

Agent 在回复中使用特殊标记请求评估：

```text
[EVALUATE]
```text

`llm_response_system` 检测到此标记后，生成 `EvaluationRequestMessage`，触发为 `AgentRequested`。

### 统一设计体现

评估器本身也是一个 Agent：

- 拥有长期记忆（可学习评估经验）
- 可被配置为任务型或持久性
- 通过 tags 标识评估能力

---

## 八、记忆传承机制

### 触发时机

任务型 Agent 销毁时，将其长期记忆贡献给父 Agent。

### 传承流程

1. Task 到达终态 → 生成 `TaskTerminatedMessage`
2. `AgentTerminationSystem` 检测绑定的任务型 Agent
3. 生成 `MemoryContributionRequestMessage`
4. 父 Agent 通过 LLM 评估贡献内容
5. 选择性吸收到父 Agent 的长期记忆

### 消息结构

```rust
#[derive(Debug, Clone, Component)]
pub struct MemoryContributionRequestMessage {
    pub contributor_id: AgentId,
    pub contributor_name: String,
    pub parent_id: AgentId,
    pub memories: Vec<MemoryEntry>,
    pub task_summary: TaskSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: TaskId,
    pub goal: String,
    pub outcome: String,
}
```text

### LLM 评估

父 Agent 收到贡献请求后，构造 prompt 让 LLM 评估：

```text
你是 Agent 的记忆管理者。以下是子 Agent 完成任务后的记忆贡献，请评估哪些内容值得保留到你的长期记忆中。

任务目标: {goal}
任务结果: {outcome}

贡献的记忆:
{memories}

请返回 JSON 格式:
{
  "absorb": [
    {"content": "...", "reason": "保留原因"}
  ],
  "discard": [
    {"content": "...", "reason": "丢弃原因"}
  ]
}

评估标准:
- 与你职责相关的知识
- 可复用的经验
- 重要的实体信息
- 忽略任务特定的临时细节
```text

### 评估结果

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributionEvaluation {
    pub absorb: Vec<AbsorbedMemory>,
    pub discard: Vec<DiscardedMemory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorbedMemory {
    pub content: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardedMemory {
    pub content: String,
    pub reason: String,
}
```text

### 持久性 Agent 的自然处理

持久性 Agent 无父 Agent，贡献请求不发出，记忆自然保留在自身。这不是特殊逻辑，而是 `parent_id = None` 时的自然结果。

---

## 九、新增消息

### CreateTaskMessage

用于创建新的 Task（用户输入无匹配的等待中 Task 时）：

```rust
#[derive(Debug, Clone, Component)]
pub struct CreateTaskMessage {
    pub content: String,
}
```text

### ContinueTaskMessage

用于追加用户输入到现有 Task：

```rust
#[derive(Debug, Clone, Component)]
pub struct ContinueTaskMessage {
    pub task_id: TaskId,
    pub user_input: String,
}
```text

### 消息流转

```text
UserInputMessage
    │
    ├── 无 Waiting(User) 的 Task → CreateTaskMessage → 新建 Task
    │
    └── 有 Waiting(User) 的 Task → ContinueTaskMessage → 追加到现有 Task
```text

---

## 十、System 设计

### 新增 System

| System | 职责 | 归属 Set |
|--------|------|----------|
| `user_input_routing_system` | 用户输入到达后，判断是新建 Task 还是追加到现有 Task | TransformSet |
| `evaluation_trigger_system` | 检测评估触发条件，生成 EvaluationRequestMessage | DispatchSet |
| `evaluation_system` | 执行评估器 Agent，产出 EvaluationResultMessage | ExecutionSet |
| `evaluation_result_system` | 处理评估结果，更新 Task 状态 | TransformSet |
| `memory_compression_system` | 检测短期记忆容量，执行摘要压缩 | MaintenanceSet |
| `agent_termination_system` | 检测任务型 Agent 销毁，生成贡献请求 | MaintenanceSet |
| `memory_contribution_system` | LLM 评估贡献，吸收到父 Agent 长期记忆 | ExecutionSet |

### 修改 System

| System | 修改内容 |
|--------|----------|
| `task_dispatch_system` | 支持 Task 多轮执行，追加用户输入到现有 Task |
| `llm_response_system` | 识别 Agent 返回的评估申请标记 |
| `agent_factory_system` | 为任务型 Agent 添加长期记忆 Component |
| `task_termination_system` | 触发记忆传承流程 |

### 核心实现示例

__user_input_routing_system：__

```rust
fn user_input_routing_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
) {
    for (entity, input) in &user_inputs {
        let waiting_task = tasks.iter()
            .find(|t| t.status == TaskStatus::Waiting(WaitingReason::User));

        if let Some(task) = waiting_task {
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            commands.spawn(CreateTaskMessage {
                content: input.content.clone(),
            });
        }

        commands.entity(entity).despawn();
    }
}
```text

__evaluation_trigger_system：__

```rust
fn evaluation_trigger_system(
    mut commands: Commands,
    tasks: Query<&Task>,
    memories: Query<&ShortTermMemory>,
    config: Res<TaskEvaluationConfig>,
) {
    if !config.enabled {
        return;
    }

    for task in &tasks {
        if task.status != TaskStatus::Running {
            continue;
        }

        if let Some(memory) = memories.get(task.entity) {
            if let Some(max_turns) = config.max_turns {
                if memory.turn_count >= max_turns {
                    commands.spawn(EvaluationRequestMessage {
                        task_id: task.id,
                        trigger: EvaluationTrigger::TurnLimitReached,
                        agent_id: task.delegate.unwrap(),
                    });
                }
            }
        }
    }
}
```text

---

## 十一、配置

### 新增配置结构

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryConfig {
    /// 近期全量保留轮数
    pub recent_turns: u32,
    /// 中期摘要触发阈值
    pub compression_threshold: u32,
    /// 摘要覆盖轮数
    pub summary_window: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvaluationConfig {
    /// 是否启用评估器
    pub enabled: bool,
    /// 最大轮数阈值
    pub max_turns: Option<u32>,
    /// 评估器 Agent 名称
    pub evaluator_agent: String,
    /// 偏离处理策略
    pub offtrack_policy: OffTrackPolicy,
}
```text

### 配置文件示例

```toml
[memory]
recent_turns = 5
compression_threshold = 10
summary_window = 5

[evaluation]
enabled = true
max_turns = 20
evaluator_agent = "evaluator"
offtrack_policy = "AskUser"
```text

---

## 十二、依赖

Phase 4.1 不引入新依赖。

Phase 4.2 引入向量存储时再添加。

---

## 十三、与现有设计的兼容性

### 移除项

- `Waiting(Brain)` 状态：Brain 决策发生在 Task 创建前，不进入状态机

### 保留项

- 现有 Task 结构（扩展状态）
- 现有 Agent 结构（新增 Component）
- 现有 System 流程（扩展处理逻辑）
- 重试机制
- Brain 决策链路

### 向后兼容

单轮 Task 是多轮 Task 的特例：第一轮执行后直接进入终态。

---

## 十四、后续扩展（Phase 4.2）

| 功能 | 说明 |
|------|------|
| 向量存储集成 | 引入向量数据库 |
| 向量化检索 | 长期记忆的语义相似度检索 |
| 混合检索 | 关键词 + 向量检索结合 |
| 记忆索引优化 | 大规模记忆的高效检索 |
