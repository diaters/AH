> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# ContinueExisting 默认复用 Delegate 与 Brain 输出解析健壮性修复设计

## 背景

当前系统在路由判定 `continue_existing`（继续已有 `Waiting(User)` 任务）后，会在 `continue_task_system` 中将 Task 置为 `Ready`。由于 Dispatch 阶段启用了 Brain 分发，`brain_dispatch_system` 会扫描所有 `Ready/Pending` Task 并触发 `BrainDecision`，导致“继续已有任务”也会再次进入 Brain 决策链路。

在一次实际运行中，第二次用户输入为“我需要你来运行”，路由系统正确识别为 `continue_existing`，但随后触发 Brain 决策并在解析阶段失败（`expected value at line 1 column 1`），进而把 Task 标记为 `Failed(AgentError)`，触发摘要系统。这暴露出两个问题：

- “继续已有任务”的默认语义与用户心智不一致：用户期望继续由上次执行任务的 Agent 接着处理，而非重新分发。
- Brain 输出解析对不可见字符（如 UTF-8 BOM、零宽字符）等噪声过于敏感，导致可恢复的偏差升级为任务失败。

## 目标

- 将 `continue_existing` 的默认执行语义明确为：优先复用 Task 上一次的 `delegate`，由同一 Agent 继续完成多轮任务。
- 在 Brain 功能开启时，避免 `continue_existing` 路径默认触发 Brain 决策。
- 提升 `parse_brain_decision` 的健壮性，使其能容忍常见不可见字符与包裹形式（如 Markdown code block），显著降低 Brain 决策解析失败率。
- 保持现有 SystemSet 的链式调度结构不变，改动聚焦在 dispatch/parse 的判定条件与解析逻辑。

## 非目标

- 不提供显式 reroute（例如 `/reroute`、`/brain`）能力。
- 不改动任务模型为多 delegate 或更复杂的路由策略。
- 不改变 Brain 决策 JSON schema。

## 现状与根因分析

### 继续任务的状态机跃迁

- 路由系统在检测到存在 `Waiting(User)` 的 Task 时，生成 `ContinueTaskMessage`。
- `continue_task_system` 将该 Task：
  - `status` 置为 `Ready`
  - `content` 更新为本次用户输入
  - 将用户输入追加到 STM

### Brain 分发触发条件

- `brain_dispatch_system` 当前对所有 `TaskStatus::Ready | TaskStatus::Pending` 的 Task 执行 Brain 分发，不区分来源（新建或 continue）。
- Dispatch 插件顺序为 Brain 优先于普通 task dispatch，因此 continue 后立即触发 Brain 看起来像是“routing 后又触发了一次 Brain”，实际是 continue 使 Task 回到了可调度态。

### Brain 解析失败的放大效应

当继续路径被引导进入 Brain 后，Brain 输出解析失败会直接把 Task 标记为终态失败，导致用户无法继续该任务，且会触发摘要系统，对交互体验与可恢复性均不利。

## 方案概述

### 核心策略

- 将 Brain 分发的默认适用范围收敛到“尚未绑定执行者”的任务。
- 将普通任务分发增加“优先复用 delegate”的逻辑，使 continue 任务可以直接由上次 Agent 继续执行。
- 对 Brain 输出解析做输入净化与兼容增强，避免不可见字符导致的解析失败。

### 决策规则（无显式 reroute）

- 新建任务：`delegate == None`，仍按原逻辑进入 Brain（若启用 Brain）。
- 继续任务：通常 `delegate != None`，将被优先复用 delegate，绕过 Brain。
- 异常回退：若 `delegate` 不存在或不可用，则按现有选择逻辑回退（Brain 或普通选择器，具体由实现策略决定）。

## 详细设计

### 1. Brain 分发收敛：跳过已绑定 delegate 的 Task

#### 行为定义

当 Brain 开启时，`brain_dispatch_system` 仅对满足以下条件的 Task 触发 Brain 决策：

- `task.status in {Ready, Pending}`
- `task.delegate == None`

这样 continue 任务在大多数情况下不会触发 Brain。

#### 边界情况

- 若 `delegate` 指向的 Agent 不存在（例如配置变更导致 agent 被移除），可以在 brain_dispatch_system 或后续 dispatch 中做“不可用检测”并回退到 Brain 或普通选择器。
- 子任务分发分支维持现状（子任务由 Brain 分发的逻辑不在本设计变更范围内）。

### 2. 普通任务分发增强：优先复用 delegate

#### 行为定义

在 `task_dispatch_system` 中，对 `Ready/Pending` 的 Task 执行分发时：

- 若 `task.delegate` 存在且对应 Agent 可用，则直接选用该 Agent 构建请求并进入 `Waiting(Agent)`。
- 否则走原有 `select_agent_with_memory` 选择逻辑。

#### 说明

- 该行为使“继续已有任务”的默认执行者稳定，符合用户对多轮任务连续性的预期。
- 同时能减少继续输入引发的“换 agent 导致上下文断裂”的问题。

### 3. Brain 决策解析健壮性修复

#### 问题特征

实际日志中 Brain 输出 snippet 形似合法 JSON，但解析报错在 `line 1 column 1`，典型原因是字符串开头存在不可见字符（如 UTF-8 BOM `\u{feff}`）或零宽字符。

#### 行为定义

在 `parse_brain_decision` 进入 `serde_json::from_str` 前，对输入做标准化处理：

- `trim` 两端空白
- 移除开头的 UTF-8 BOM（`U+FEFF`）
- 移除常见零宽字符（如 `U+200B`、`U+200C`、`U+200D`、`U+2060`）
- 保留现有 `extract_json_block`（支持 Markdown code block 包裹）

#### 测试补充

新增单测覆盖：

- JSON 前带 BOM 能成功解析
- JSON 前带零宽字符能成功解析
- Markdown code block + BOM 组合能成功解析

### 4. 可观测性与日志

保持现有事件命名与字段规范，建议补充或强化以下观测点（不改变现有字段约束）：

- 在 dispatch 阶段记录是否“复用 delegate”：
  - `event = "AgentSelected"` 可新增 `selection_reason = "reuse_delegate"`（若现有字段允许扩展）
- 在 Brain dispatch 跳过时记录（仅 debug/trace）：
  - `event = "BrainDispatchSkipped"`，原因例如 `has_delegate = true`

## 兼容性与风险

- 行为变化：继续任务不再默认触发 Brain，可能改变少量依赖“每轮重新路由”的使用习惯，但符合更主流的多轮对话预期。
- 若某任务初次分发后 delegate 不合适，用户后续输入无法通过显式 reroute 改变执行者。本设计接受该取舍，以换取链路简化与稳定性。
- 需要确保 delegate 复用不会影响子任务、WorkItem 等其它分发路径。

## 验收标准

- 当存在 `Waiting(User)` Task 时输入继续消息（如“我需要你来运行”）：
  - Task 继续后不会触发 `BrainDispatch`（在 delegate 存在的情况下）。
  - `task_dispatch_system` 复用 delegate 直接向同一 Agent 发起执行请求。
- Brain 输出即使带 BOM/零宽字符，也不会触发 `BrainDecisionParseFailed`。
- 回归：新建任务在 Brain 启用时仍会触发 Brain 分发并正常选 Agent。

## 测试计划

- 单元测试：
  - `parse_brain_decision` 针对 BOM/零宽字符输入的解析用例。
  - 任务分发逻辑：delegate 复用分支覆盖（可通过构造 Task/Agent 并运行 system 的方式或最小纯函数抽取）。
- 日志回归：
  - 复现日志场景，确认 continue 路径不会再出现 `BrainDispatch`，且不再因 Brain 解析失败终止任务。

