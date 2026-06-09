# 设计文档索引

本文档用于说明 `docs/design/` 下各设计文档的当前状态、推荐阅读顺序与使用边界，
避免将历史阶段文档误读为当前实现说明。

## 使用规则

- 当前实现优先参考 `docs/current-state.md`
- 设计边界优先参考状态为“当前有效”的文档
- 状态为“历史背景”的文档用于理解设计演进，不直接作为当前实现依据
- 状态为“过渡计划”或“已废止”的文档仅用于回顾重构路径

## 推荐阅读顺序

1. `docs/current-state.md`
2. `docs/design/2026-06-06-workitem-boundary-design.md`
3. `docs/design/2026-06-06-plan-evaluation-reassessment-design.md`
4. 按需阅读相关历史背景文档

## 文档状态总览

| 文件 | 状态 | 作用 | 备注 |
|------|------|------|------|
| `2026-06-06-workitem-boundary-design.md` | 当前有效 | 定义 `Task`、`WorkItem` 与控制状态边界 | 当前架构收敛的核心依据 |
| `2026-06-06-plan-evaluation-reassessment-design.md` | 当前有效 | 定义 `Plan` 收敛与 `Evaluation` 重新定位 | 与 WorkItem 边界文档配套阅读 |
| `2026-05-24-genai-migration-design.md` | 当前有效（决策背景） | 记录 `genai` 替换方案与 provider 设计原因 | 适合理解当前 LLM 接入来源 |
| `2026-05-10-core-flow-design.md` | 历史背景 | 早期主链路深化设计 | 当前实现已演进，不直接等同 |
| `2026-05-14-brain-agent-design.md` | 历史背景 | Brain 调度早期设计 | 当前能力仍有效，但表述使用旧阶段语境 |
| `2026-05-16-multi-agent-design.md` | 历史背景 | 多 Agent 与 Agent 演化设计 | 部分前提已由 ADR 和后续实现修订 |
| `2026-05-17-multi-turn-memory-design.md` | 历史背景 | 多轮对话与记忆管理设计 | 当前记忆链路已结合 summarization 与 WorkItem 演进 |
| `2026-05-17-tool-space-design.md` | 历史背景 | Tool、Space、审批与权限设计 | 当前 shell 工具面和部分实现细节已更新 |
| `2026-05-20-llm-summarization-design.md` | 历史背景 | 摘要能力早期设计 | 当前执行链路已收敛到 `WorkItem` |
| `modular-refactor-implementation.md` | 过渡计划 | 历史模块化重构实施计划 | 已含废止说明，不作为当前实施依据 |

## 阅读建议

### 当前架构问题

若你想回答“当前系统到底怎么建模”，优先阅读：

- `docs/current-state.md`
- `2026-06-06-workitem-boundary-design.md`
- `2026-06-06-plan-evaluation-reassessment-design.md`

### LLM 接入与 provider

若你想回答“为什么现在使用 `genai`，provider 如何定位”，优先阅读：

- `2026-05-24-genai-migration-design.md`
- `docs/configuration.md`

### 工具、权限与审批

若你想回答“工具系统为什么这样设计”，优先阅读：

- `2026-05-17-tool-space-design.md`
- `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`
- `docs/TODO.md`

### 历史演进背景

若你想理解系统为什么会形成现在的结构，可按时间顺序阅读：

- `2026-05-10-core-flow-design.md`
- `2026-05-14-brain-agent-design.md`
- `2026-05-16-multi-agent-design.md`
- `2026-05-17-multi-turn-memory-design.md`
- `2026-05-17-tool-space-design.md`
- `2026-05-20-llm-summarization-design.md`

## 维护要求

- 新增设计文档时，必须同步更新本文档中的状态和用途
- 若历史文档与当前实现出现明显冲突，应在文档顶部补充状态说明
- 当某篇文档成为新的当前依据时，应同步调整本索引和 `docs/current-state.md`
