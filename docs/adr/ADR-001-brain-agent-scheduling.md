# ADR-001: Brain Agent 调度机制

## 状态

Proposed

## 背景

MVP 阶段 `task_dispatch_system` 使用简单的 `find(Idle)` 策略选择 Agent，无法根据任务内容智能分派。Phase 2 需要引入 Brain Agent 作为全局调度者，通过 LLM 调用决策"谁来执行"。

Brain 自身需要调用 LLM 生成调度决策，存在两种实现路径：

1. __复用现有异步链路__：Brain 作为一个特殊 Agent，复用 `AgentExecutor` + `agent_execution_system`，通过 `AgentRequestKind::BrainDecision` 区分
2. __独立执行器__：新增 `BrainExecutorHandle` 和 `brain_execution_system`，与现有链路并行

## 决策

采用方案 1：Brain 复用现有异步链路。

理由：

- 符合设计文档"Brain 只改变分发给谁，不改变如何执行的标准链路"的约束
- 无需新增 channel pair 和异步 system，减少代码重复
- 通过 `AgentRequestKind` 枚举变体在结果消费阶段分流，天然互斥
- Brain 不启用时行为与 MVP 完全一致，零侵入

## 后果

__正面__：

- 最小化代码变更，复用已有的异步执行、结果回注、重试机制
- `AgentRequestKind` 作为统一分支点，可扩展其他请求类型

__负面__：

- `AgentExecutionResult` 需新增 `request_kind` 字段，现有结构体需调整
- `llm_response_system` 需增加过滤逻辑，跳过 BrainDecision 结果
- Brain 请求与普通请求共享同一条 channel，高并发下可能互相影响（当前规模可忽略）
