# ADR-007: 任务进度清单定位为纯展示数据，不复活 Plan 模块

## 状态

Accepted（2026-10-28，随 Agent loop 优化设计一并确认实施）

## 背景

`docs/design/2026-06-06-plan-evaluation-reassessment-design.md` 已确认：`Plan` 不再作为
独立运行时模块推进，`Task -> Planning WorkItem -> PlanArtifact -> Worker WorkItem`
闭环与 `PlanPolicy`、`PlanArtifactBuilder` 等抽象均已删除，规划能力收敛为
`create_tasks` + DAG 调度 + `wait_tasks` 的任务分解。

`docs/design/2026-10-28-loop-steering-and-background-notification-design.md` 引入
`update_todos` 工具与 `TaskTodoList` Component，让 Agent 向用户展示自己正在进行到哪一步
（对齐 Claude Code `TodoWrite`、Codex `update_plan`）。它在形态上与"计划"相近，
需要明确它与已废弃 Plan 的边界，避免以"进度清单"为名重新长出规划闭环。

## 决策

1. `TaskTodoList` 是 __Agent 自报的展示数据__，不是执行计划：
   - 不产生 Task、WorkItem 或 `PendingDispatch`；
   - 派发、终态判定、评估、重试、经验收集等系统__不得__读取它作为决策输入；
   - 只有两个读取方：前端输出（TUI / IM 渲染）与上下文注入管道（新一轮回填快照）。
2. 写入只经 `update_todos` → `ToolEffect::UpdateTodos` → `commit_tool_effects_system`，
   不提供其他写入口。
3. 需要并行或委派执行时，仍使用 `create_tasks`。`update_todos` 的工具描述中写明这一分工。
4. 若未来出现"根据清单驱动调度"的需求，必须另立 ADR 重新评估，不得在本 Component 上直接叠加。

## 后果

- 正面：用户获得进度可见性；LLM 在长任务与上下文压缩后保持自我追踪；不新增调度层级，
  符合"简化优先 / 统一优先"。
- 负面：清单内容完全由 LLM 自报，可能与实际执行不一致。这是展示层可以接受的误差，
  不影响正确性。
- 约束：评审 PR 时需检查是否有调度相关系统新增了对 `TaskTodoList` 的查询。
