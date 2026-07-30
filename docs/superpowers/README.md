# Superpowers 文档

本目录由 superpowers 插件自动生成，包含实施计划（`plans/`）和设计规格（`specs/`）。

## 文档状态

### 活跃计划

| 文件 | 主题 | 状态 |
|------|------|------|
| `plans/2026-07-04-sequential-tool-confirmation-plan.md` | 顺序工具确认实施 | 活跃 |
| `plans/2026-07-05-per-user-turn-tool-limit.md` | 单轮工具调用软限制实施 | 活跃 |
| `plans/2026-07-06-timer-local-timezone-schedule-task-plan.md` | 定时任务本地时区调度实施 | 活跃 |
| `plans/2026-07-07-dynamic-scheduled-task-approval-routing.md` | 动态 scheduled task 审批路由与事件任务审批通道检查实施 | 活跃 |
| `plans/2026-07-07-log-level-restructure.md` | 日志级别重构实施 | 活跃 |
| `plans/2026-07-08-im-channel-task-identification.md` | IM 通道任务标识实施 | 活跃 |
| `plans/2026-07-10-data-flow-guide-plan.md` | 数据流转指南实施 | 活跃 |
| `plans/2026-07-10-knowledge-manager-agent.md` | 知识管理 Agent 实施 | 活跃 |
| `plans/2026-07-10-per-agent-multi-model-fallback.md` | 单 Agent 多模型降级实施 | 活跃 |
| `plans/2026-07-10-tui-global-task-panel.md` | TUI 全局任务面板实施 | 活跃 |
| `plans/2026-07-13-agent-profile-llm-generation.md` | Agent Profile LLM 生成实施 | 活跃 |
| `plans/2026-07-18-dispatch-architecture-unification.md` | 调度架构统一实施 | 活跃 |
| `plans/2026-07-18-skill-first-class-and-experience-governance.md` | Skill 一等公民与经验治理改造实施 | 活跃 |

### 活跃规格

| 文件 | 主题 | 状态 |
|------|------|------|
| `specs/2026-07-04-sequential-tool-confirmation-design.md` | 顺序工具确认设计 | 当前有效 |
| `specs/2026-07-05-per-user-turn-tool-limit-design.md` | 单轮工具调用软限制设计 | 当前有效 |
| `specs/2026-07-06-timer-local-timezone-schedule-task-design.md` | 定时任务本地时区调度设计 | 当前有效 |
| `specs/2026-07-07-dynamic-scheduled-task-approval-routing-design.md` | 动态 scheduled task 审批路由与事件任务审批通道检查设计 | 当前有效 |
| `specs/2026-07-07-log-level-restructure-design.md` | 日志级别重构设计 | 当前有效 |
| `specs/2026-07-08-im-channel-task-identification-design.md` | IM 通道任务标识设计 | 当前有效 |
| `specs/2026-07-10-data-flow-guide-design.md` | 数据流转指南设计 | 当前有效 |
| `specs/2026-07-10-knowledge-manager-agent-design.md` | 知识管理 Agent 设计 | 未标注状态 |
| `specs/2026-07-10-knowledge-manager-agent-design-revision.md` | 知识管理 Agent 设计修订 | 草案 |
| `specs/2026-07-10-per-agent-multi-model-fallback-design.md` | 单 Agent 多模型降级设计 | 当前有效 |
| `specs/2026-07-10-tui-global-task-panel-design.md` | TUI 全局任务面板设计 | 当前有效 |
| `specs/2026-07-13-plugin-system-v2-application-model-design.md` | 插件系统 v2 应用模型设计 | 当前有效 |

> 注意：规格状态以文件自身顶部标注为准。标记为 `当前有效` 的规格若对应功能已合并 main（对照 `docs/current-state.md` 的"已实现"章节），应按下方生命周期规则在 7 天内归档到 `docs/archive/superpowers/`。`knowledge-manager-agent-design.md` 缺状态标注，建议补 `当前有效` 或 `草案`。

### 已归档

以下文档对应的功能已合并到 main，于 2026-07-05 归档到 `docs/archive/superpowers/`：

<details>
<summary>已归档计划（26 篇）</summary>

- `2026-06-06-workitem-unified-execution.md` — WorkItem 统一执行
- `2026-06-07-continue-existing-delegate.md` — 委托任务续接
- `2026-06-09-space-module-convergence.md` — Space 模块收敛
- `2026-06-10-memory-convergence-implementation.md` — 记忆系统收敛
- `2026-06-11-experience-candidate-governance.md` — 经验候选治理与可执行记忆实施
- `2026-06-11-memory-persistence-implementation.md` — 长期记忆持久化实施
- `2026-06-14-experience-collection-workitem.md` — 经验收集 WorkItem 化
- `2026-06-14-experience-module-layered-governance.md` — 经验模块两层分层汇聚治理
- `2026-06-14-experience-persistence-fix.md` — 经验落盘链路完整修复
- `2026-06-15-experience-governance-writeback.md` — 经验治理统一写回与任务级孵化实施
- `2026-06-16-experience-governance-writeback-fix.md` — 经验治理写回修复
- `2026-06-16-governance-runtime-defects.md` — 治理运行时缺陷修复
- `2026-06-17-experience-governance-child-candidate-writeback.md` — 子候选写回修复
- `2026-06-17-experience-module-refactor-plan.md` — 经验模块重构
- `2026-06-18-experience-incubation-knowledge-writeback.md` — 孵化知识写回
- `2026-06-19-experience-governance-completion.md` — 经验治理完成
- `2026-06-19-experience-submission-simplification.md` — 经验提交简化
- `2026-06-23-plugin-system.md` — 插件系统实施
- `2026-06-25-reduce-idle-cpu.md` — 降低空闲 CPU
- `2026-06-26-im-channel-adapters.md` — IM 通道适配器实施
- `2026-06-27-auto-channel-reply.md` — IM 出向-自动回执实施
- `2026-06-27-qq-channel.md` — QQ 通道实施
- `2026-06-27-telegram-channel-experience.md` — Telegram 通道体验优化
- `2026-06-29-channel-isolation-fix.md` — 通道隔离修复
- `2026-06-30-chat-with-agent-implementation.md` — chat_with_agent 工具实施
- `2026-07-01-chat-with-agent-tool-result-race-fix.md` — chat_with_agent 竞态修复

</details>

<details>
<summary>已归档规格（24 篇）</summary>

- `2026-06-07-continue-existing-delegate-design.md` — 委托任务续接设计
- `2026-06-09-space-module-convergence-design.md` — Space 模块收敛设计
- `2026-06-10-memory-convergence-design.md` — 记忆系统收敛设计
- `2026-06-11-experience-candidate-governance-design.md` — 经验候选治理设计
- `2026-06-11-memory-persistence-design.md` — 长期记忆持久化设计
- `2026-06-14-experience-collection-workitem-design.md` — 经验收集 WorkItem 化设计
- `2026-06-14-experience-module-layered-governance-design.md` — 两层分层汇聚治理设计
- `2026-06-14-experience-persistence-fix-design.md` — 经验落盘链路修复设计
- `2026-06-15-experience-governance-writeback-design.md` — 经验治理写回设计
- `2026-06-16-experience-governance-writeback-fix-design.md` — 经验治理写回修复设计
- `2026-06-16-governance-runtime-defects-design.md` — 治理运行时缺陷设计
- `2026-06-17-experience-governance-child-candidate-writeback-design.md` — 子候选写回设计
- `2026-06-17-experience-module-refactor-design.md` — 经验模块重构设计
- `2026-06-18-experience-incubation-knowledge-writeback-design.md` — 孵化知识写回设计
- `2026-06-19-experience-governance-completion-design.md` — 经验治理完成设计
- `2026-06-19-experience-submission-simplification-design.md` — 经验提交简化设计
- `2026-06-23-plugin-system-design.md` — 插件系统设计
- `2026-06-25-reduce-idle-cpu-design.md` — 降低空闲 CPU 设计
- `2026-06-27-auto-channel-reply-design.md` — IM 出向-自动回执设计
- `2026-06-27-qq-channel-design.md` — QQ 通道设计
- `2026-06-27-telegram-channel-experience-design.md` — Telegram 通道体验设计
- `2026-06-29-channel-isolation-fix.md` — 通道隔离修复设计
- `2026-06-30-chat-with-agent-design.md` — chat_with_agent 设计
- `2026-07-01-chat-with-agent-tool-result-race-fix-design.md` — chat_with_agent 竞态修复设计

</details>

## 生命周期规则

- 计划执行完毕或规格被代码实现后，应在 **7 天内** 移动到 `docs/archive/superpowers/`
- 归档时在文件顶部添加 `> **状态：已归档**` 标注
- 归档后的文档只增不改，不做内容修订
- 历史计划和规格参见 [docs/archive/superpowers/](../archive/superpowers/)

## 与插件的兼容性

superpowers 插件默认将新文件写入以下路径，无需手动调整：

- 设计规格：`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`
- 实施计划：`docs/superpowers/plans/YYYY-MM-DD-<feature-name>.md`
