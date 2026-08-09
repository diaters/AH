# 设计文档索引

本文档用于说明 `docs/design/` 下各设计文档的当前状态与推荐阅读顺序。

历史设计文档（已被取代）已移入 [docs/archive/design/](../archive/design/)。

## 文档状态总览

| 文件 | 状态 | 作用 | 备注 |
|------|------|------|------|
| `im-channel-adapters.md` | 当前有效 | Telegram/QQ/飞书通道抽象与 ECS 集成 | 已实现 Telegram 与 QQ 入向、出向-主动与出向-自动 |
| `2026-06-06-workitem-boundary-design.md` | 当前有效 | 定义 `Task`、`WorkItem` 与控制状态边界 | 当前架构收敛的核心依据 |
| `2026-06-06-plan-evaluation-reassessment-design.md` | 当前有效 | 定义 `Plan` 收敛与 `Evaluation` 重新定位 | 与 WorkItem 边界文档配套阅读 |
| `2026-05-24-genai-migration-design.md` | 当前有效（决策背景） | 记录 `genai` 替换方案与 provider 设计原因 | 适合理解当前 LLM 接入来源 |
| `2026-05-14-brain-agent-design.md` | 历史背景 | Brain 调度早期设计 | 当前能力仍有效，但表述使用旧阶段语境 |
| `2026-05-17-multi-turn-memory-design.md` | 历史背景 | 多轮对话与记忆管理设计 | 当前记忆链路已结合 summarization 与 WorkItem 演进 |
| `2026-07-13-agent-profile-llm-generation-design.md` | 当前有效 | Agent 元信息 LLM 生成与动态更新 | 孵化时 LLM 生成 profile，经验积累后评估更新 |
| `2026-07-18-dispatch-architecture-unification-design.md` | 当前有效 | 派发架构统一 | 单一 `PendingDispatch` 入口，治理 9 个腐化点 |
| `2026-07-26-ecs-relation-modeling-design.md` | 当前有效 | 实体关系改用 ECS 原生建模（EntityIndex + ChildOf） | 消灭 55+ 线性扫描 |
| `2026-08-09-context-compression-blind-spot-fix.md` | 当前有效 | 上下文压缩盲区修复 | 工具结果入 STM、配对组粒度压缩、结构化发送路径 |

## 推荐阅读顺序

1. [docs/current-state.md](../current-state.md)
2. [2026-06-06-workitem-boundary-design.md](2026-06-06-workitem-boundary-design.md)
3. [2026-06-06-plan-evaluation-reassessment-design.md](2026-06-06-plan-evaluation-reassessment-design.md)
4. 按需阅读相关历史背景文档

## 阅读建议

### 当前架构问题

若你想回答"当前系统到底怎么建模"，优先阅读：

- [docs/current-state.md](../current-state.md)
- [2026-06-06-workitem-boundary-design.md](2026-06-06-workitem-boundary-design.md)
- [2026-06-06-plan-evaluation-reassessment-design.md](2026-06-06-plan-evaluation-reassessment-design.md)

### LLM 接入与 provider

若你想回答"为什么现在使用 `genai`，provider 如何定位"，优先阅读：

- [2026-05-24-genai-migration-design.md](2026-05-24-genai-migration-design.md)
- [docs/configuration.md](../configuration.md)

### IM 通道接入

若你想回答”如何从 IM 触发 Task、Agent 如何主动推送消息到 IM”，优先阅读：

- [im-channel-adapters.md](im-channel-adapters.md)
- [docs/configuration.md](../configuration.md) 的”IM 通道配置”章节
- 历史实施计划与规格已归档至 [docs/archive/superpowers/](../archive/superpowers/)

## 已归档文档

以下文档已移入 [docs/archive/design/](../archive/design/)，保留供查阅历史演进：

| 文件 | 归档原因 |
|------|----------|
| `2026-05-10-core-flow-design.md` | LLM 客户端已从 `async-openai` 迁移到 `genai` |
| `2026-05-16-multi-agent-design.md` | 权限继承已从 tags 子集改为 tools 权限过滤 |
| `2026-05-17-tool-space-design.md` | `AgentExperience` 已删除，Space 已收敛为两个资源 |
| `2026-05-20-llm-summarization-design.md` | 独立 summarization 管道已删除，迁移到 WorkItem 闭环 |
| `modular-refactor-implementation.md` | 历史模块化重构计划，已含废止说明 |

## 维护要求

- 新增设计文档时，必须同步更新本文档中的状态和用途
- 设计文档与 `docs/current-state.md` 出现明显冲突时，应归档到 `docs/archive/design/`
- 当某篇文档成为新的当前依据时，应同步调整本索引和 `docs/current-state.md`
