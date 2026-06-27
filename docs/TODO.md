# 待办事项

本文档记录项目当前仍需推进的任务、已知限制与近期关注方向。

## 当前结论

- 项目已从“阶段编号驱动”转为“能力状态驱动”表述
- `Task` 是用户目标主实体，`WorkItem` 是内部执行单元
- `Plan` 已收敛为任务分解能力，不再作为独立模块存在
- `Evaluation` 与 `Summarization` 已并入 `WorkItem` 执行闭环
- shell 工具已收敛为六个意图化工具

## 已完成的关键能力

- [x] TUI 运行时与事件主循环
- [x] Task 主链路与多轮对话基本状态管理
- [x] Brain 调度与多 Agent 配置加载
- [x] 任务分解能力：`create_tasks` + DAG 调度 + `wait_tasks`
- [x] Tool 执行链路、权限控制与审批 UI
- [x] Short-term memory 与 summarization 能力
- [x] Evaluation 语义层与 `WorkItem` 执行收敛
- [x] shell 工具精简重构与回归测试
- [x] CI、格式检查、clippy、自动化测试

## 当前待办

### 高优先级

- [ ] 将 `approval_dispatch_system` 的 MVP 自动通过逻辑替换为真实父 Agent LLM 审查
- [ ] 为审批链路补齐更明确的策略语义：
  `Approved`、`Rejected`、`GrantMode::Once`、`GrantMode::Permanent`
- [x] 持续清理仍引用旧 shell 语义的历史注释、文档和过程文稿
- [x] 为长期记忆实现持久化存储（`MemoryStore` + `MemoryRepository` + `LongTermMemoryService` 写穿模型）
- [x] 实现跨会话记忆加载，Agent 启动时可从持久层恢复 `LongTermMemory`
- [ ] `knowledge_search` 工具抽象为 trait，支持多种搜索策略（关键词、向量等），
  当前仅为单一实现，不利于扩展替换
- [ ] 新增「知识库管理员」Agent 角色，负责响应其他 Agent 的搜索知识库请求，
  将知识查询职责从调用方解耦到专职 Agent
- [x] 增加 Agent 级别的 Skill 功能：可执行经验经治理后落盘为 Agent 私有 Skill Package
- [x] 重新设计经验贡献系统架构
- [x] 实现经验模块两层分层汇聚治理（非顶层贡献 / 顶层治理 / 四类去向）
- [x] 为 `SharedKnowledgeUpgradeQueue` 增加文件持久化
- [x] 为 `IncubationProposal` 增加用户审批后的持久型 Agent 创建执行链路
- [ ] 清理旧经验直写链路：`memory_contribution_system` / `memory_absorption_system`（当前为过渡态保留）
- [ ] 实现顶层治理对候选 `kind_hint` 的修正能力
- [x] 实现非顶层基于多个子候选整理组合候选的主动重写

### 中优先级

- [ ] 梳理并补充当前架构索引，明确哪些设计文档仍是有效真相源
- [ ] 增加更多真实 provider 场景验证，明确 `openai`、`anthropic`、
  `deepseek` 与 `openai-compatible` 的运行约束
- [ ] 继续强化复杂任务场景下的调度、评估与恢复策略验证
- [ ] 为共享知识候选条目增加自动 LLM 审核链路，当前仅用户确认和 `IncubationProposal` 流程，
  `Candidate` 状态条目缺少 LLM 自动审核
- [x] 为长期记忆增加清理/淘汰机制（已实现：按 `decay_score` 阈值移除并归档到 `archive.jsonl`）

### 低优先级

- [ ] 评估配置热加载是否值得引入
- [ ] 评估分布式或多实例支持是否进入近期路线
- [ ] 评估长期记忆是否需要引入向量检索或语义匹配能力，
  当前仅依赖轻量关键词匹配，相关性精度有限

## 已知限制

### 审批链路

- 当前审批请求会进入统一审批流程和 TUI 展示
- 但父 Agent 决策仍是 MVP 自动通过，不代表最终架构目标

### 文档状态

- `docs/design/` 下仍有一部分历史阶段文档保留旧语境
- 这些文档可作为设计演进背景，不应直接视为当前实现说明

### Provider 说明

- 标准 provider 已接入统一执行器
- 但跨 provider 的真实行为差异仍需要更多运行验证和文档沉淀

### 长期记忆系统

- 领域模型、Core + Relevant 注入、衰退治理、贡献吸收链路已在进程内闭环
- 长期记忆已实现 JSON 文件持久化，Agent 启动时加载、吸收后立即落盘
- 经验候选治理已实现两层分层汇聚：非顶层向上贡献 → 顶层统一治理 → 四类最终去向落盘
- 共享知识升级入口已实现文件持久化（`.harness/memory/shared_knowledge/upgrades.json`）
- 共享知识候选条目缺少自动 LLM 审核，当前通过用户确认和孵化提案流程
- 衰退分数低于阈值且非 `pin` 非 `Critical` 的条目会被移除并归档到 `<agent-name>/archive.jsonl`
- 相关性匹配仅使用轻量关键词，未引入向量或语义检索

## 近期建议顺序

1. 先完成真实审批链路替换
2. 再补齐 provider 兼容性验证与文档说明
3. 最后评估是否推进更远期能力，如热加载或多实例支持

## 参考文档

- 当前状态：`docs/current-state.md`
- 项目规范：`AGENTS.md`
- WorkItem 边界：
  `docs/design/2026-06-06-workitem-boundary-design.md`
- Plan / Evaluation 重评估：
  `docs/design/2026-06-06-plan-evaluation-reassessment-design.md`
- shell 工具精简设计（已归档）：
  `docs/archive/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`
