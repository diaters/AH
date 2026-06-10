# 归档文档

本目录存放已被替代或已执行的文档，仅作历史记录参考。

## 目录结构

```text
archive/
├── README.md          ← 本文件
├── design/            ← 已被取代的设计文档
├── superpowers/       ← 已完成的计划和过期规格
│   ├── plans/
│   └── specs/
└── *.md               ← 早期归档文档（历史遗留）
```

## 早期归档文档

| 文件 | 说明 | 替代文档 |
|------|------|----------|
| `harness（废弃）.md` | 早期 Harness Core 设计 | `design/2026-05-10-core-flow-design.md` |
| `2026-05-16-multi-turn-memory-design-v1.md` | 多轮对话设计 v1（按轮数管理） | `design/2026-05-17-multi-turn-memory-design.md` |
| `2026-05-16-multi-agent-plan.md` | Phase 3 多 Agent 支持 | 已完成 |
| `2026-05-16-multi-turn-memory-plan-v1.md` | Phase 4.1 v1 实现计划 | 已完成（后被 v2 替代） |
| `2026-05-17-multi-turn-memory-plan-v2.md` | Phase 4.1 v2 实现计划 | 已完成 |

## design/ — 已被取代的设计文档

| 文件 | 归档原因 |
|------|----------|
| `2026-05-10-core-flow-design.md` | LLM 客户端已从 `async-openai` 迁移到 `genai` |
| `2026-05-16-multi-agent-design.md` | 权限继承已从 tags 子集改为 tools 权限过滤 |
| `2026-05-17-tool-space-design.md` | `AgentExperience` 已删除，Space 已收敛为两个资源 |
| `2026-05-20-llm-summarization-design.md` | 独立 summarization 管道已删除，迁移到 WorkItem 闭环 |
| `modular-refactor-implementation.md` | 历史模块化重构计划，已含废止说明 |

## superpowers/plans/ — 已完成的实施计划

| 文件 | 对应能力 |
|------|----------|
| `2026-05-21-llm-summarization.md` | LLM 摘要能力 |
| `2026-05-23-agent-spawn-subagent.md` | Agent 子任务派生 |
| `2026-05-24-genai-migration.md` | genai 迁移 |
| `2026-05-24-log-persistence.md` | 结构化日志持久化 |
| `2026-05-25-remove-echo-fix-subtask-dispatch.md` | 子任务调度修复 |
| `2026-05-25-subtask-result-passing.md` | 子任务结果传递 |
| `2026-05-26-tui.md` | TUI 主循环 |
| `2026-05-27-tui-task-hierarchy.md` | TUI 任务层级展示 |
| `2026-05-28-wait-tasks-tool.md` | wait_tasks 工具 |
| `2026-06-07-shell-timeout.md` | Shell 超时控制 |
| `2026-06-08-shell-tool-simplification.md` | Shell 工具精简 |

## superpowers/specs/ — 已执行或已过期的设计规格

| 文件 | 对应能力 |
|------|----------|
| `2026-05-23-agent-spawn-subagent-design.md` | Agent 子任务派生设计 |
| `2026-05-23-data-level-debug-logging-design.md` | 结构化日志设计 |
| `2026-05-24-log-persistence-design.md` | 日志持久化设计 |
| `2026-05-25-subtask-result-passing-design.md` | 子任务结果传递设计 |
| `2026-05-26-tui-design.md` | TUI 设计 |
| `2026-05-27-tui-task-hierarchy-design.md` | TUI 任务层级设计 |
| `2026-05-27-wait-tasks-tool-design.md` | wait_tasks 工具设计 |
| `2026-06-07-shell-timeout-design.md` | Shell 超时设计 |
| `2026-06-07-shell-tool-design.md` | Shell 工具原始设计（已被精简设计取代） |
| `2026-06-07-shell-tool-implementation-alignment.md` | Shell 工具实现对齐文档 |
| `2026-06-08-shell-tool-simplification-design.md` | Shell 工具精简设计 |
