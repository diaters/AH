# WorkItem 边界与迁移设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-06-06 |
| 适用阶段 | MVP 主链路收敛 |
| 相关文档 | `docs/design/2026-06-06-plan-evaluation-reassessment-design.md` |

---

## 1. 背景

当前项目已经定义了 `WorkItem` 统一工作单元模型，当前支持以下工作类型：

- `Execution`
- `Summarization`
- `Evaluation`

`Planning` 类型已在重评估后从代码中删除，不再作为预留项保留。

同时，`WorkItem` 还定义了：

- 输入与上下文
- 标签
- 来源
- 写回目标
- 创建 / 完成事件

但在当前运行时主链路中，`WorkItem` 尚未真正成为统一执行载体。多数系统仍然直接操作 `Task`，或者通过专用消息流完成内部执行过程，例如：

- 普通任务分发直接由 `Task -> AgentExecutionRequest`
- 摘要通过 `SummarizationRequestMessage / SummarizationResultMessage` 专线处理
- 评估通过 `EvaluationRequestMessage / EvaluationResultMessage` 专线处理
- 子任务编排通过 `Task + SubTaskBatchState + WaitingReason` 管理

这使得项目目前处于一种“抽象已存在、主链路未统一”的中间态。

---

## 2. 问题陈述

当前围绕 `WorkItem` 存在四个核心问题：

1. `Task` 与 `WorkItem` 的职责边界不够清晰
2. 哪些运行时动作应该提升为 `WorkItem`，缺少统一标准
3. 哪些专用消息流需要迁移到 `WorkItem`，缺少优先级判断
4. `WorkItem` 改造与 `Plan / Evaluation` 重评估之间的先后顺序尚未明确

如果这些问题不先澄清，后续推进 `Evaluation` 收敛或执行链统一时，容易出现以下风险：

- `Task` 和 `WorkItem` 双重建模但边界重叠
- 为了统一而过度统一，把控制状态也误建模为 `WorkItem`
- `Plan / Evaluation` 文档依赖 `WorkItem`，但 `WorkItem` 本身定义仍模糊

---

## 3. 设计目标

- 明确 `Task`、`WorkItem`、`AgentExecutionRequest`、控制状态之间的边界
- 给出“是否应该使用 `WorkItem`”的判断标准
- 指出当前代码中最适合迁移到 `WorkItem` 的部分
- 为后续 `Evaluation` 与执行链统一提供基础设计
- 明确本设计与 `Plan / Evaluation` 重评估设计的先后顺序

---

## 4. 核心定义

### 4.1 Task

`Task` 是__用户目标的载体__。

它代表用户真实想完成的事情，具有以下特征：

- 面向用户和产品语义
- 拥有完整业务生命周期
- 拥有最终完成 / 失败语义
- 是最终结果归属主体
- 可以包含多轮交互、等待、重试、失败恢复等业务状态

`Task` 回答的问题是：

```text
用户到底想完成什么？
```

### 4.2 WorkItem

`WorkItem` 是__为完成 Task 而派生的内部执行单元__。

它代表系统内部需要被调度、执行、跟踪和写回的工作，具有以下特征：

- 不直接代表用户意图
- 从某个 `Task` 或运行时策略派生
- 需要进入统一执行链路
- 执行结果需要写回 `Task`、上下文或其他内部载体
- 生命周期短于或从属于 `Task`

`WorkItem` 回答的问题是：

```text
为了完成当前 Task，系统现在具体要做哪一步内部工作？
```

### 4.3 AgentExecutionRequest

`AgentExecutionRequest` 是__某个执行单元被实际发送给 Agent 时的瞬时请求__。

它不应被视为业务主实体，也不应承担长期状态。它只负责描述：

- 由谁执行
- 执行什么 prompt
- 使用哪些 tools
- 带哪些上下文

### 4.4 控制状态

以下对象属于__执行过程控制状态__，而不是 `WorkItem`：

- `ToolCallingState`
- `WaitingReason`
- `WaitingForTasksInfo`
- `SubTaskBatchState`
- 轮询、确认、阻塞、重试等中间状态

这些对象回答的问题不是“系统要做什么工作”，而是：

```text
当前工作执行到哪里、为什么暂停、什么时候恢复？
```

因此它们不应被统一为 `WorkItem`。

---

## 5. 核心原则

### 5.1 Task 是主实体，WorkItem 是从实体

`Task` 是用户目标的业务主实体，`WorkItem` 是为了完成该目标而派生的执行从实体。

两者不是平级关系，也不是“递归 Task”关系。

### 5.2 不是所有内部动作都应成为 WorkItem

只有同时满足以下条件的内部动作，才适合提升为 `WorkItem`：

1. 具有清晰输入与输出
2. 需要被调度给执行者
3. 需要跟踪执行状态
4. 结果需要写回某个目标
5. 该动作本身是“工作”，而不是“状态”

### 5.3 WorkItem 只统一执行单元，不统一控制流

`WorkItem` 负责统一“内部工作单元”，但不负责吞并：

- 等待机制
- 工具循环机制
- DAG 批次元数据
- 轮询 / 恢复 / 审批等过程控制逻辑

### 5.4 WorkItem 优先服务于执行链收敛

引入 `WorkItem` 的目标不是重新发明一套 `Task` 体系，而是减少专用消息流，形成可复用的内部执行载体。

---

## 6. 判断标准

### 6.1 应改造成 WorkItem 的动作

以下动作通常适合使用 `WorkItem`：

- 需要调用 LLM 的内部治理工作
- 需要被统一分发的内部执行步骤
- 输入和输出边界稳定的内部处理步骤
- 结果需要写回 `Task`、记忆、评估结论等目标的步骤

### 6.2 不应改造成 WorkItem 的动作

以下动作通常不应使用 `WorkItem`：

- 纯粹的状态切换
- 等待中的阻塞占位
- 工具调用的中间迭代状态
- 仅用于批次编排的元数据
- 仅用于权限确认或恢复的控制消息

---

## 7. 当前系统适配分析

### 7.1 第一优先级：Evaluation

`Evaluation` 最适合优先迁移到 `WorkItem`。

原因如下：

- 已有稳定领域语义
- 已存在 `WorkItemType::Evaluation`
- 当前实现仍是专用请求 / 结果消息流
- 与 `TaskRuntime`、Dispatch、结果写回之间天然适合做统一收敛

目标形态：

```text
TaskRuntime / TriggerPolicy
    -> Evaluation WorkItem
    -> Dispatch
    -> AgentExecutionRequest
    -> EvaluationResult
    -> DecisionApplyPolicy
    -> Task writeback
```

### 7.2 第一优先级：Summarization

`Summarization` 同样适合优先迁移到 `WorkItem`。

原因如下：

- 已存在 `WorkItemType::Summarization`
- 已有 `WorkItem::summarization()` 构造能力
- 当前摘要链路完全是专用消息流
- 它是典型的“内部工作单元”，不是用户任务

目标形态：

```text
Memory policy / Trigger
    -> Summarization WorkItem
    -> Dispatch
    -> AgentExecutionRequest
    -> Summary writeback
    -> ShortTermMemory update
```

### 7.3 第二优先级：Execution

普通执行链路从语义上也适合统一为 `Execution WorkItem`，但不建议放在第一轮。

原因如下：

- 它会触及当前最核心的主链路
- 一旦迁移，`Task` 与 `WorkItem` 的关系必须彻底理顺
- 风险高于 `Evaluation` 和 `Summarization`

因此建议后置处理：

- 先让 `WorkItem` 在治理型内部工作中落地
- 再评估是否将普通任务执行统一到 `Execution WorkItem`

### 7.4 Brain / Planning 已不再预留

`Planning WorkItem` 已从代码中删除，不再作为未来扩展的预留项。

原因如下：

- `Plan` 已在重评估中被收敛为能力，而非独立模块
- 当前 Brain 通过工具驱动编排（`create_tasks` + DAG 调度），不需要 `Planning WorkItem` 抽象
- 保留预留类型会产生误导性，暗示该路线仍在推进

如未来确有需要，可重新引入规划抽象，但不应复用已删除的 `Planning` 变体。

### 7.5 不建议迁移：控制流对象

以下对象不建议改造为 `WorkItem`：

- `ToolCallingState`
- `WaitingForTasksInfo`
- `SubTaskBatchState`
- `WaitingReason`
- 审批、阻塞、轮询、恢复等过程控制消息

原因是这些对象是控制流结构，而不是待执行的内部工作单元。

---

## 8. 目标边界模型

```text
Task
  └─ 用户目标、业务生命周期、最终结果归属

WorkItem
  └─ 为完成 Task 而派生的内部执行单元

AgentExecutionRequest
  └─ 某个 WorkItem 或 Task 执行步骤被真正发送给 Agent 时的瞬时请求

Control State
  └─ Tool loop / wait / batch / approval / retry 等过程状态
```

---

## 9. 改造策略

### 9.1 第一阶段：边界固化

目标：

- 固化 `Task / WorkItem / Control State` 的定义
- 明确哪些专用消息流的目标态是 `WorkItem`
- 不立即重构主链路

产出：

- 本文档
- 对相关设计文档的补充说明

### 9.2 第二阶段：治理型 WorkItem 落地

优先改造：

1. `Evaluation`
2. `Summarization`

实施目标：

- 用 `WorkItem` 替代专用请求消息作为执行载体
- 保留领域结果模型
- 将写回逻辑统一到通用结果处理层

### 9.3 第三阶段：执行链评估

在 `Evaluation` 和 `Summarization` 收敛完成后，再评估是否推进：

- `Execution WorkItem`
- 通用 `WorkItem` 分发器

这一阶段不属于本文档立即要求的范围。

---

## 10. 与 Plan / Evaluation 设计的先后顺序

### 10.1 设计顺序

建议顺序如下：

1. `WorkItem` 边界设计
2. 补充 `Plan / Evaluation` 重评估设计
3. 再进入实施计划

原因如下：

- `WorkItem` 是更底层的执行抽象
- `Evaluation` 目标态已经显式依赖 `WorkItem`
- 如果不先定义 `WorkItem` 边界，`Plan / Evaluation` 文档中的“统一执行链”会缺少稳定基础

### 10.2 实现顺序

建议顺序如下：

1. `Evaluation` 收敛到 `WorkItem`
2. `Summarization` 收敛到 `WorkItem`
3. 再评估是否推进 `Execution WorkItem`

不建议的顺序：

- 不建议先做全链路 `Task -> WorkItem` 重写
- 不建议先改工具循环和等待状态

---

## 11. 对 Plan / Evaluation 文档的补充要求

已有的 `Plan / Evaluation` 重评估设计不需要重写，但需要补充两类信息：

### 11.1 前置依赖说明

需要明确：

- `Evaluation` 的统一执行链设计，依赖于 `WorkItem` 边界的先行明确
- `WorkItem` 只负责统一执行单元，不负责统一全部控制状态

### 11.2 范围限制说明

需要明确：

- `Plan` 去模块化已完成（代码已删除），不存在重新引入 Planner 抽象的风险
- `Evaluation` 收敛到 `WorkItem` 不应被误解为所有内部动作都应转化为 `WorkItem`

---

## 12. 风险与缓解

| 风险 | 说明 | 缓解措施 |
|------|------|----------|
| 双重建模 | `Task` 与 `WorkItem` 语义重叠 | 明确主从关系，禁止把 `WorkItem` 视为用户任务 |
| 过度统一 | 把等待、工具循环等状态也改为 `WorkItem` | 明确控制状态不属于 `WorkItem` |
| 改造范围失控 | 从治理型工作扩展到整个主链路 | 先只落地 `Evaluation` 与 `Summarization` |
| ~~与 Plan 回潮冲突~~ | ~~借 `WorkItem` 名义重新引入 Planner 抽象~~ | ~~已缓解：`Planning` 变体已从代码中删除~~ |

---

## 13. 非目标

- 本文档不要求立即实现完整 `Execution WorkItem` 主链路
- 本文档不要求重新设计工具调用循环
- 本文档不要求重新设计子任务 DAG 元数据结构
- 本文档不恢复独立 `Plan` 模块路线
- 本文档不直接定义实施级文件修改清单

---

## 14. 验收标准

- `Task` 与 `WorkItem` 的边界被明确区分
- `AgentExecutionRequest` 与控制状态的边界被明确区分
- 当前应迁移到 `WorkItem` 的系统被明确列出
- 不应迁移到 `WorkItem` 的系统被明确列出
- `WorkItem` 与 `Plan / Evaluation` 设计之间的先后顺序被明确说明
- 后续实施可基于本文档继续写具体计划

---

## 15. 最终决策

本次设计作出如下正式决策：

1. `Task` 是用户目标的业务主实体
2. `WorkItem` 是为完成 `Task` 而派生的内部执行单元
3. `WorkItem` 只统一可执行的内部工作，不统一控制状态
4. 第一轮应优先将 `Evaluation` 与 `Summarization` 收敛到 `WorkItem`
5. `Plan / Evaluation` 设计应以本设计作为执行边界前提

---

## 16. 实施备注

__实施日期：__ 2026-06-06

__实施范围：__ 第一轮已将 `Evaluation` 与 `Summarization` 的执行载体迁移到 `WorkItem`

__已完成的迁移：__

1. __Evaluation WorkItem 完整闭环__
   - 触发：`evaluation_trigger_system` 创建 `Evaluation WorkItem`
   - 调度：`workitem_dispatch_system` 分发给评估器 Agent
   - 执行：Agent 执行并返回结果
   - 应用：`llm_response_system` 解析结果并更新任务状态

2. __Summarization WorkItem 完整闭环__
   - 触发：`summarization_dispatch_system` 创建 `Summarization WorkItem`
   - 调度：`workitem_dispatch_system` 分发给摘要 Agent
   - 执行：Agent 执行并返回结果
   - 应用：`llm_response_system` 更新 ShortTermMemory

__保留的控制流对象：__

- `SummarizationRequestMessage` 作为触发型控制消息继续存在
- `WaitingReason`、`ToolCallingState` 等控制状态仍保持原有建模

__未纳入实施范围：__

- `Execution WorkItem`（普通任务执行）
- 工具调用循环的重写

__架构简化：__

- 评估结果应用集成在 `llm_response_system` 中，而非独立的 `evaluation_apply_system`
- 摘要结果应用集成在 `llm_response_system` 中，复用现有内存更新逻辑

__验收状态：__

- ✅ 所有单元测试通过（105 个）
- ✅ 所有集成测试通过
- ✅ clippy 静态分析通过
- ✅ 功能验证：Evaluation 和 Summarization 完整流程正常工作
