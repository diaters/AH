# 经验治理统一写回与任务级孵化设计

> **状态：当前有效**

## 背景

当前经验模块已经具备以下基础能力：

- 任务终态可触发 `ExperienceCollection`
- `collector` 可通过 `submit_experience_candidate` 提交 `ExperienceCandidate`
- 顶层治理已支持 `Knowledge`、`Executable`、`SharedKnowledge`、`Discard` 四类分流
- `LongTermMemory`、`Skill Package`、`SharedKnowledge` 升级入口已有基础持久化能力

但当前从顶层治理到正式落盘仍有几个结构性问题：

- 顶层治理输入不统一：顶层自身候选与子层汇聚候选没有收敛为同一治理输入集
- 正式写回逻辑分散在多个 system 中，治理判断与落盘执行耦合
- 审批只解决“是否放行”，没有形成稳定的“放行后统一写回”主链路
- 风险判断缺少语义来源，首版不应再引入新的硬编码规则系统
- `IncubationProposal` 当前按候选逐个生成，而不是按顶层任务汇总生成
- `default Agent` 的“治理输出”和“真正孵化成新 Agent”之间缺少完整闭环

本设计的目标，是在现有两层汇聚治理模型上继续收敛出一条语义完整的主链路：

```text
任务终态
  -> 经验收集
  -> 候选产生
  -> 顶层输入收束
  -> 顶层治理决议
  -> 用户确认（如需要）
  -> 统一写回执行
  -> 正式资产 / 升级入口 / 任务级孵化提案 / 新 Agent 创建
```

## 目标

- 将顶层治理后的所有正式去向统一收敛到一条写回主链路
- 覆盖所有正式情况：
  - `LongTermMemory`
  - Agent 私有 `Skill Package`
  - `SharedKnowledge` 升级入口
  - `IncubationProposal`
  - `Rejected`
- 将顶层自身候选和子层汇聚候选收敛为同一个治理输入集
- 将风险分级前移到候选产生阶段，由 LLM 给出并随候选提交
- 将 `IncubationProposal` 收敛为顶层任务级对象，而不是候选级对象
- 在提案批准后，首版真正执行到“创建新持久型 Agent”
- 保证失败可审计、状态真实、候选不悬空

## 非目标

- 不引入复杂的风险打分规则引擎
- 不实现复杂候选去重、相似度聚类或自动冲突合并
- 不在首版实现自动重试队列或批处理写回
- 不让新孵化的 Agent 在创建后自动加入当前运行时调度
- 不重做底层文件存储格式

## 设计原则

- 候选唯一中间态：所有正式资产都必须先经过 `ExperienceCandidate`
- 风险语义前移：风险由候选产生时的 LLM 直接判断并附带
- 顶层唯一分流：所有最终去向只在顶层治理阶段决定
- 决议与执行分离：治理负责“决定去向”，写回层负责“真正落盘”
- 任务级孵化：一个顶层任务最多形成一个活跃 `IncubationProposal`
- 状态诚实：审批通过不等于落盘成功，治理完成不等于写回成功
- 失败可审计：任何失败都必须留下明确状态与错误上下文

## 总体架构

首版推荐将顶层经验治理收敛为三个阶段：

### 一、候选产生

- `collector` 负责从任务终态材料中提炼经验候选
- 候选必须至少包含：
  - `kind_hint`
  - `payload`
  - `dependency_refs`
  - `risk_level`
  - `risk_reason`
  - `suggested_confirmation`

这里的风险判断不是系统推导结果，而是经验提炼阶段的 LLM 初判结果。

### 二、治理决议

- 顶层治理对每个候选给出唯一最终去向
- 顶层治理默认采用候选自带的风险判断
- 若治理者认为初判不合理，可以覆盖，但必须记录覆盖理由
- 治理输出不直接写盘，而是先产生统一的治理决议对象

### 三、统一写回

- 自动落盘和审批后落盘都进入统一写回执行层
- 写回层只负责按治理决议写正式目标
- 写回成功后候选才可进入 `Persisted`
- 写回失败必须进入显式失败状态

## 候选模型扩展

### 风险字段

`ExperienceCandidate` 首版应补充以下字段：

- `risk_level: low | medium | high`
- `risk_reason: String`
- `suggested_confirmation: none | user`

含义如下：

- `risk_level`：候选产生阶段的风险初判
- `risk_reason`：为什么这样判断
- `suggested_confirmation`：候选产生阶段建议是否需要用户确认

### 治理覆盖规则

顶层治理默认沿用候选中的风险判断，但允许覆盖。

覆盖时必须额外记录：

- `risk_overridden: bool`
- `governance_rationale: String`

首版不做复杂多轮争论，保持“默认采用、允许显式修正”的简单模式。

## 顶层治理输入统一

### 问题

当前顶层治理输入被拆成两路：

- 顶层任务自身产出的 `root_candidates`
- 子层汇聚到父任务 `ExperienceInbox` 并被标记为 `Aggregated` 的候选

这会导致“已汇聚到顶层 inbox，但未进入最终治理”的断链。

### 新规则

顶层任务结束时，系统必须显式构造一个统一的 `TopLevelGovernanceInput`，它由以下两部分组成：

- `self_candidates`
- `aggregated_candidates`

顶层治理只消费这一份统一输入，不再分别直接读取两个存储区域做条件分支。

### 合并原则

- 对于子层原样上送的候选，沿用原 `candidate_id`
- 对于顶层基于多个候选重写出的新候选，允许创建新的 `ExperienceCandidate`
- 若创建新候选，必须记录：
  - `derived_from_candidate_ids`

这样可以区分：

- 子层原样保留的候选
- 顶层重新提炼或组合出来的更高层候选

## 治理矩阵

### 普通持久型 Agent

- `knowledge`
  - 低风险私有经验：自动进入 `LongTermMemory`
  - 高风险私有经验：用户确认后进入 `LongTermMemory`
- `executable`
  - 用户确认后进入 Agent 私有 `Skill Package`
- `shared_knowledge`
  - 一般公共价值：自动进入 `SharedKnowledgeUpgrade`
  - 高影响公共规则：用户确认后进入 `SharedKnowledgeUpgrade`
- `discard`
  - 直接 `Rejected`

### `default Agent`

- `knowledge`
  - 不允许直接进入自身 `LongTermMemory`
  - 进入任务级 `IncubationProposal`
- `executable`
  - 不允许直接进入自身 `Skill Package`
  - 进入任务级 `IncubationProposal`
- `shared_knowledge`
  - 允许进入 `SharedKnowledgeUpgrade`
- `discard`
  - 直接 `Rejected`

## 统一写回执行层

### 设计目标

顶层治理的输出不直接写盘，而是统一进入写回执行层。

治理输出至少应包含：

- `candidate_id`
- `governing_agent_id`
- `destination`
- `confirmation_policy`
- `risk_level`
- `decision_rationale`

### 写回执行职责

写回层负责：

- 根据 `destination` 调用对应持久化服务
- 推进候选状态
- 写审计日志
- 在失败时生成稳定的失败状态

写回层不负责：

- 再次判断候选去向
- 重新评估风险
- 隐式替换治理决议

## 状态机调整

首版建议将候选状态收敛为：

- `Submitted`
- `InInbox`
- `Aggregated`
- `GovernancePending`
- `GovernanceResolved`
- `NeedsUserApproval`
- `WritebackPending`
- `Persisted`
- `Rejected`
- `WritebackFailed`

其中关键约束如下：

- `GovernanceResolved`：顶层已经做出最终去向决议
- `NeedsUserApproval`：已决议，但等待用户确认
- `WritebackPending`：已被放行，等待正式写回
- `Persisted`：正式目标写入成功
- `WritebackFailed`：决议已确定，但正式写回失败

## 审批位置与行为

审批只承担“放行”职责，不直接承担文件写入职责。

推荐主链路如下：

```text
顶层治理决议
  -> 若无需确认：进入 WritebackPending
  -> 若需要确认：进入 NeedsUserApproval
  -> 用户批准后：进入 WritebackPending
  -> 统一写回执行
  -> Persisted / WritebackFailed
```

### 审批策略

首版采用风险分级策略：

- 低风险私有知识：允许自动写回
- `executable`：必须确认
- `default Agent` 私有沉淀：必须确认
- 高影响共享知识：必须确认

这里的“低风险 / 高影响”判断默认来自候选的 LLM 风险初判，顶层治理可显式覆盖。

## 四类正式去向的执行边界

### `LongTermMemory`

- 目标：普通持久型 Agent 的私有知识沉淀
- 写回方式：通过 `LongTermMemoryService`
- 成功条件：目标 Agent 的 `LongTermMemory` 修改并成功持久化
- 成功后：候选进入 `Persisted`

### `Skill Package`

- 目标：普通持久型 Agent 的私有可执行经验
- 写回方式：通过 `AgentAssetService` 写入稳定目录
- 最小要求：
  - 生成 `skill.md`
  - `scripts/` 和 `resources/` 目录存在，可为空
- 成功后：候选进入 `Persisted`

### `SharedKnowledgeUpgrade`

- 目标：共享知识升级入口，而不是最终共享知识正文
- 写回方式：持久化 `SharedKnowledgeUpgradeCandidate`
- 成功后：候选进入 `Persisted`

### `IncubationProposal`

- 目标：`default Agent` 的任务级正式治理输出
- 首版要求：proposal 自身必须先被正式持久化
- 提案批准后，还要继续进入“创建新 Agent”的执行阶段

## 任务级孵化提案

### 核心规则

`IncubationProposal` 必须是顶层任务级对象，而不是候选级对象。

一条顶层任务在同一轮治理中：

- 最多只能有一个活跃 proposal

### 创建规则

当顶层治理发现某候选的最终去向是 `IncubationProposal` 时：

- 若当前 `source_task_id` 已存在活跃 proposal，则 merge 当前候选
- 若不存在，则创建新的 proposal

merge 规则：

- `knowledge` 候选加入 `knowledge_candidate_ids`
- `executable` 候选加入 `executable_candidate_ids`
- `shared_knowledge` 候选加入 `shared_knowledge_candidate_ids`
- 同一 `candidate_id` 不允许重复加入

### 最小字段

任务级 proposal 至少包含：

- `proposal_id`
- `source_task_id`
- `source_agent_id`
- `proposed_agent_profile`
- `knowledge_candidate_ids`
- `executable_candidate_ids`
- `shared_knowledge_candidate_ids`
- `incubation_rationale`
- `status`
- `created_at`
- `updated_at`

### 状态

建议 proposal 状态至少包含：

- `Proposed`
- `Approved`
- `Executing`
- `Executed`
- `ExecutionFailed`
- `Rejected`

## 提案批准后的新 Agent 创建

### 首版目标

首版不止停留在“提案已保存”，而是执行到“真正创建新持久型 Agent”。

### 执行步骤

- 用户批准任务级 proposal
- proposal 进入 `Approved`
- 系统进入孵化执行阶段
- 创建新的持久型 Agent
- 将批准范围内的知识写入该 Agent 的 `LongTermMemory`
- 将批准范围内的可执行经验写为该 Agent 的 `Skill Package`
- 共享知识候选仍进入 `SharedKnowledgeUpgrade`，不作为新 Agent 私有资产
- 成功后 proposal 进入 `Executed`

### 新 Agent 的最小生成内容

- `name`
- `model`
- `tags`
- `description`
- `tool_permissions`
- 初始 `LongTermMemory`
- 初始 `Skill Package`

### 激活策略

首版创建新 Agent 后：

- 必须保证可持久化、可恢复
- 默认不自动加入当前运行时调度池

这样可以把“Agent 创建成功”和“立即参与当前调度”两个问题拆开，降低首版复杂度。

## 失败处理与审计

### 候选级失败原则

- 不伪装成功
- 不让候选悬空
- 不丢失失败上下文

### 正式写回失败

以下写回失败都必须进入 `WritebackFailed`：

- `LongTermMemory` 写入失败
- `Skill Package` 写入失败
- `SharedKnowledgeUpgrade` 持久化失败
- `IncubationProposal` 持久化失败

### 审批相关失败

- 用户拒绝：进入 `Rejected`
- 审批消息失配：保持原状态或停留在 `NeedsUserApproval`，并记录日志
- 审批通过但写回失败：进入 `WritebackFailed`

### proposal 执行失败

若 proposal 已批准，但新 Agent 创建或其初始资产写入失败：

- proposal 进入 `ExecutionFailed`
- 不得伪装为 `Executed`
- 必须保留错误信息与失败阶段

### 最小失败上下文

首版建议统一保留以下错误上下文：

- `candidate_id` 或 `proposal_id`
- `destination`
- `error_stage`
- `error_message`
- `occurred_at`
- `governing_agent_id`

### 最小审计事件

建议记录以下关键事件：

- `ExperienceCandidateSubmitted`
- `ExperienceCandidateAggregated`
- `TopLevelExperienceGovernanceRequested`
- `ExperienceGovernanceResolved`
- `ExperienceApprovalRequested`
- `ExperienceApprovalResolved`
- `ExperienceWritebackStarted`
- `ExperienceWritebackSucceeded`
- `ExperienceWritebackFailed`
- `IncubationProposalMerged`
- `IncubationExecutionStarted`
- `IncubationExecutionSucceeded`
- `IncubationExecutionFailed`

## 测试策略

### 必测主链路

- 顶层自身候选与子层汇聚候选同时进入顶层治理
- 普通持久型 Agent 的低风险知识自动落到 `LongTermMemory`
- 普通持久型 Agent 的 `executable` 经确认后落到 `Skill Package`
- `SharedKnowledge` 候选进入升级入口
- `default Agent` 的多个私有候选只汇总成一个任务级 `IncubationProposal`
- proposal 批准后真正创建新 Agent，并写入其初始知识与技能
- 写回失败时进入 `WritebackFailed`
- proposal 执行失败时进入 `ExecutionFailed`

### 重点回归

- `/finish` 路径不会重复触发顶层经验收集
- 审批响应不会被错误地路由到普通工具确认链
- 一个顶层任务不会生成多份活跃 `IncubationProposal`

## 结论

本设计在现有两层汇聚治理模型之上，进一步把顶层治理到正式落盘收敛为一条统一主链：

- 候选在产生时附带 LLM 风险初判
- 顶层治理默认采用该风险判断并给出唯一最终去向
- 所有正式去向统一进入写回执行层
- 子层汇聚候选与顶层自身候选统一进入顶层治理输入
- `IncubationProposal` 收敛为任务级对象
- proposal 批准后首版真正执行到新持久型 Agent 的创建

这样可以同时覆盖 `LongTermMemory`、`Skill Package`、`SharedKnowledgeUpgrade` 和任务级孵化四类正式闭环，并保证失败可审计、状态真实、链路不悬空。
