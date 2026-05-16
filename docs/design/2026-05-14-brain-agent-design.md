# Brain Agent 调度设计

本文档描述 Phase 2 Brain Agent 调度的详细设计，包括数据流、状态转换、接口定义和实施步骤。

---

## 一、设计目标

- Brain 作为全局调度者，通过 LLM 决策任务应该交给哪个 Agent
- 复用现有 `AgentExecutor` + `agent_execution_system` 异步链路
- Brain 不启用时行为与 MVP 完全一致
- Brain 决策失败时具备容错能力

---

## 二、核心数据流

### 完整流程（Brain 启用）

```mermaid
sequenceDiagram
    participant User
    participant InputIngress
    participant SignalIngest
    participant UserMsgToTask
    participant BrainDispatch
    participant AgentExecution
    participant BrainDecision
    participant TaskDispatch
    participant LlmResponse
    participant UserOutput

    User->>InputIngress: 输入事件
    InputIngress->>SignalIngest: 生成 Signal
    SignalIngest->>UserMsgToTask: 转为 Message
    UserMsgToTask->>BrainDispatch: Task 进入 Ready
    BrainDispatch->>AgentExecution: 发起 BrainDecision 请求
    Note over BrainDispatch: Task 进入 Waiting(Brain)
    AgentExecution->>BrainDecision: 回注 Brain 决策结果
    BrainDecision->>TaskDispatch: 解析决策，产出 LlmCompletion 请求
    Note over BrainDecision: Task 进入 Waiting(Agent)
    TaskDispatch->>AgentExecution: 执行 LlmCompletion 请求
    AgentExecution->>LlmResponse: 回注执行结果
    LlmResponse->>UserOutput: Task 完成，生成输出
    UserOutput->>User: 输出结果
```

### MVP 兼容流程（Brain 不启用）

与当前 MVP 流程完全一致，`brain_dispatch_system` 跳过，`task_dispatch_system` 直接处理 Ready Task。

---

## 三、状态转换

### Task 状态扩展

`WaitingReason::Brain` 已在 MVP 中预留，现在正式启用。

```mermaid
stateDiagram-v2
    [*] --> Ready
    Ready --> Waiting_Brain: brain_dispatch (Brain 启用)
    Ready --> Waiting_Agent: task_dispatch (Brain 不启用)
    Waiting_Brain --> Waiting_Agent: brain_decision 成功
    Waiting_Brain --> Failed: brain 决策不可重试失败
    Waiting_Brain --> Waiting_RetryBackoff: brain 决策可重试失败
    Waiting_Agent --> Running: executor accepted
    Running --> Done: success
    Running --> Waiting_RetryBackoff: retryable failure
    Running --> Failed: fatal failure
    Waiting_RetryBackoff --> Ready: retry wakeup signal
```

---

## 四、接口变更

### AgentRequestKind 扩展

```rust
enum AgentRequestKind {
    LlmCompletion,
    BrainDecision,  // 新增
}
```

### AgentExecutionRequest 扩展

```rust
struct AgentExecutionRequest {
    task_id: TaskId,
    agent_id: AgentId,
    request_kind: AgentRequestKind,
    prompt: String,
    system_prompt: Option<String>,  // 新增，Brain 请求使用
}
```

### AgentExecutionResult 扩展

```rust
struct AgentExecutionResult {
    task_id: TaskId,
    agent_id: AgentId,
    request_kind: AgentRequestKind,  // 新增，用于结果分流
    result: Result<String, ExecutionError>,
}
```

### Task 新增方法

```rust
impl Task {
    pub fn mark_waiting_for_brain(&mut self, agent_id: AgentId, now: DateTime<Utc>) {
        self.delegate = Some(agent_id);
        self.status = TaskStatus::Waiting(WaitingReason::Brain);
        self.updated_at = now;
    }
}
```

### 新增类型

```rust
struct BrainDecisionOutput {
    selected_agent_name: String,
    delegate_prompt: String,
    reasoning: String,
}

enum BrainDecisionError {
    ParseFailed(String),
    UnknownAgent(String),
    EmptyResponse,
}
```

---

## 五、System 设计

### brain_dispatch_system

- __阶段__：DispatchSet（在 `task_dispatch_system` 之前）
- __输入__：`Ready` 状态的 Task、`Idle` 状态的 Brain Agent
- __输出__：`AgentExecutionRequestMessage(BrainDecision)`
- __副作用__：Task 进入 `Waiting(Brain)`，Brain Agent 进入 `Busy`
- __跳过条件__：Brain 未启用、Brain Agent 不可用

### brain_decision_system

- __阶段__：TransformSet（在 `ingest_execution_results_system` 之后）
- __输入__：`AgentExecutionResultMessage` 中 `request_kind == BrainDecision` 的结果
- __输出__：`AgentExecutionRequestMessage(LlmCompletion)`
- __副作用__：Task 从 `Waiting(Brain)` 转为 `Waiting(Agent)`，Brain Agent 恢复 `Idle`，选定 Agent 进入 `Busy`
- __容错__：解析失败时 Task 标记为 Failed；选定 Agent 不存在时回退到默认 Agent

### llm_response_system 改造

- 增加过滤：跳过 `request_kind == BrainDecision` 的结果（由 `brain_decision_system` 处理）

### agent_execution_system 改造

- 回注结果携带 `request_kind`
- Brain 请求不调用 `task.mark_running()`

---

## 六、Brain Prompt 设计

### System Prompt

要求 LLM 输出 JSON 格式的结构化决策，包含 `selected_agent_name`、`delegate_prompt`、`reasoning` 三个字段。

### User Prompt

包含 Task 内容和所有可用 Agent（排除 Brain 自身）的能力描述，供 LLM 做调度判断。

### 结果解析

- 支持直接 JSON 文本
- 支持被 markdown code block 包裹的 JSON
- 解析失败时返回 `BrainDecisionError::ParseFailed`

---

## 七、Brain 配置

| 环境变量                  | 必填 | 默认值        | 说明                 |
|-------------------------|------|---------------|----------------------|
| `HARNESS_BRAIN_ENABLED` | 否   | `false`       | 是否启用 Brain 调度  |
| `HARNESS_BRAIN_MODEL`   | 否   | 与主模型相同   | Brain 使用的 LLM 模型 |
| `HARNESS_BRAIN_AGENT_NAME` | 否 | `brain`      | Brain Agent 名称    |

---

## 八、SystemSet 编排变化

```mermaid
flowchart LR
    A[IngressSet] --> B[SignalSet]
    B --> C[TransformSet]
    C --> D[DispatchSet]
    D --> E[ExecutionSet]
    E --> F[OutputSet]
    F --> G[MaintenanceSet]
```

DispatchSet 内部顺序：

```mermaid
flowchart LR
    A[brain_dispatch_system] --> B[task_dispatch_system]
```

TransformSet 内部顺序（新增 brain_decision_system）：

```mermaid
flowchart LR
    A[ingest_execution_results_system] --> B[brain_decision_system]
    A --> C[llm_response_system]
```

---

## 九、容错策略

| 场景                     | 处理方式                                         |
|-------------------------|-------------------------------------------------|
| Brain LLM 调用网络错误  | 走标准重试流程（与 Task 重试机制一致）         |
| Brain 返回非 JSON 格式  | Task 标记 Failed(AgentError)，记录 last_error  |
| Brain 选定不存在的 Agent | 回退到默认 Agent                                |
| Brain 选中自身          | 在 prompt 中排除 Brain，解析后校验              |
| Brain Agent 不存在      | brain_dispatch_system 跳过，task_dispatch_system 接管 |

---

## 十、同帧推进分析

| 帧   | 推进                                         | 跳数 |
|------|---------------------------------------------|------|
| N    | Ready -> Waiting(Brain) + BrainDecision 请求 | 1    |
| N+1  | agent_execution_system spawn 异步任务        | 1    |
| N+K  | ingest + brain_decision -> Waiting(Agent) + LlmCompletion 请求 | 2 |
| N+K+1| agent_execution_system spawn 异步任务        | 1    |
| N+K+M| ingest + llm_response -> Done + UserOutput  | 2    |

每帧最多推进 2 跳，符合约束。
