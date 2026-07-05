> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验模块两层分层汇聚治理设计

> **状态：当前有效**

## 背景

当前仓库中的经验模块已经具备一部分基础能力：

- 任务结束后可触发经验收集 `WorkItem`
- 经验候选已收敛为 `ExperienceCandidate`
- 已存在 `ExperienceStore`、`ExperienceInbox`、经验治理系统和长期记忆落盘能力
- 可执行经验与共享知识升级入口也已有部分语义雏形

但当前链路仍存在明显缺口：

- 经验收集、经验流转、最终治理与正式落盘尚未打通成一条不间断主链路
- `ExperienceInbox`、顶层治理、共享知识升级入口之间仍有语义与实现脱节
- `Executable` 经验的正式落盘形态尚未统一
- `default Agent` 的治理边界虽然已有方向，但尚未被完整固化为单一规则

本设计的目标不是继续在现有半闭环链路上追加补丁，而是直接把经验模块收敛为一套完整、诚实、可落地的两层治理闭环：

```text
非顶层任务结束
  -> 经验产生
  -> 经验汇聚
  -> 向父层贡献
  -> 顶层治理
  -> LongTermMemory / Skill Package / SharedKnowledge 升级入口 / IncubationProposal
```

## 目标

- 将经验模块收敛为“产生 -> 流转 -> 治理 -> 落盘”的完整主链路
- 首版直接采用两层分层汇聚治理，不再设计旁路或补丁式兼容路径
- 明确非顶层 `TaskScoped Agent` 与顶层 `Persistent Agent` 的职责边界
- 统一 `ExperienceCandidate` 为经验治理的唯一中间态
- 打通四类最终去向：
  - `LongTermMemory`
  - Agent 私有 `Skill Package`
  - `SharedKnowledge` 升级入口
  - `IncubationProposal`
- 对 `default Agent` 实施严格孵化制，但允许公共知识直接进入共享知识升级入口
- 保证首版实现可以相对粗糙，但流程不能中断、候选不能悬空

## 非目标

- 不设计多层无限递归的复杂治理层级，首版只明确两层：
  - 非顶层 `TaskScoped Agent`
  - 顶层 `Persistent Agent`
- 不实现复杂候选去重、候选相似度聚类或自动冲突合并
- 不实现复杂风险评分模型
- 不实现复杂共享知识终审自动化
- 不实现 `Skill Package` 的版本治理、资产回收和复杂仓储治理
- 不实现“关键上下文筛选器”，首版不以复杂上下文裁剪作为成功前提
- 不引入全局共享 skill 仓库
- 不允许 `default Agent` 直接沉淀私有长期身份资产

## 设计原则

- 简化优先：首版优先保证主链路完整，不优先做复杂治理细节
- 语义诚实：所有经验必须先经历候选态，再进入正式资产层
- 两层治理：非顶层负责贡献，顶层负责最终治理与落盘
- 候选唯一中间态：不允许绕过 `ExperienceCandidate` 直接写正式资产
- 可追溯：所有正式落盘对象必须保留最小来源追溯信息
- 可迁移：可执行经验以文件系统中的 `Skill Package` 作为真源
- 隔离执行态与治理态：运行时上下文不等于正式经验资产

## 第一部分：总体架构与对象边界

### 两层治理模型

本设计明确经验治理只有两层：

- 非顶层：`TaskScoped Agent`
- 顶层：`Persistent Agent`

其中：

- 非顶层负责“经验产生、经验汇聚、向上贡献”
- 顶层负责“最终治理、最终分流、正式落盘”

当前项目中，顶层 Agent 来自 `agents.toml` 中定义的持久型 Agent。`default Agent` 也属于持久型 Agent，但治理规则特殊。

### `TaskScoped Agent`

非顶层 `TaskScoped Agent` 负责：

- 执行具体任务
- 在任务结束时生成局部经验候选
- 汇总自己收到的子层候选
- 将“原样保留的子候选 + 新生成的局部/组合候选”继续上送给父层

非顶层不负责：

- 最终治理
- 最终落盘
- 最终审批

### `Persistent Agent`

顶层 `Persistent Agent` 负责：

- 接收来自下层的候选汇聚结果
- 在顶层任务结束时做最终经验收集与总汇聚
- 执行最终治理分流
- 驱动最终写回或生成正式提案

### `default Agent`

`default Agent` 属于 `Persistent Agent`，但不作为默认长期身份资产宿主。

`default Agent` 可以：

- 参与顶层治理
- 判断知识、技能和公共规则是否值得保留
- 将公共规则送入 `SharedKnowledge` 升级入口
- 将私有知识与可执行经验收敛为 `IncubationProposal`

`default Agent` 不可以：

- 直接生成自己的私有 `LongTermMemory`
- 直接生成自己的私有 `Skill Package`

### `ExperienceCandidate`

`ExperienceCandidate` 是经验治理的唯一中间态。

所有经验在进入正式资产层之前，都必须先表现为候选。

候选可以来自：

- 当前层自身任务的局部经验
- 当前层基于多个子候选整理出的组合经验
- 当前层决定原样保留并继续上送的子候选

### `ExperienceInbox`

`ExperienceInbox` 是父任务绑定的层间缓冲层，只承载来自子层的候选输入。

它的职责是：

- 承接下层贡献
- 供当前层在收尾阶段读取与汇聚

它不承担：

- 正式记忆职责
- 执行态上下文注入职责
- 第二套治理状态机职责

### 最终正式去向

顶层治理后，候选只能进入以下正式去向之一：

- `LongTermMemory`
- Agent 私有 `Skill Package`
- `SharedKnowledge` 升级入口
- `IncubationProposal`
- `Rejected`

## 第二部分：完整生命周期与消息流

### 总体生命周期

首版采用下面这条不间断主链路：

```text
任务执行
  -> 非顶层任务结束
  -> 非顶层经验收集
  -> 非顶层经验汇聚
  -> 向父层上送候选
  -> 顶层任务结束
  -> 顶层经验收集与总汇聚
  -> 顶层治理分流
  -> 正式落盘 / 升级入口 / 孵化提案
```

### 任务执行阶段

每个 `TaskScoped Agent` 在执行任务时：

- 正常维护自己的 `ShortTermMemory`
- 正常沿用现有执行主链路
- 不在执行阶段直接写正式经验资产

执行态与治理态在语义上明确分离。

### 非顶层任务结束：生成局部候选

当一个非顶层 `TaskScoped Agent` 结束时，触发一次 `ExperienceCollection`。

首版输入材料不做复杂筛选，直接使用任务终态可获得的完整收敛材料，包括：

- 当前任务目标
- 当前任务结果摘要
- 当前任务的 `ShortTermMemory` 终态快照
- 该任务收到的子层候选摘要

首版经验收集以“完整终态材料可用”为优先目标，不以复杂上下文筛选为前置条件。

经验收集结果是一组 `ExperienceCandidate`，这些候选可以包括：

- 当前层自己的局部候选
- 基于子候选整理出的组合候选
- 决定原样保留并继续上送的子候选

### 非顶层汇聚：写入父层 inbox

非顶层生成的最终候选集合不会直接落盘，而是统一进入父任务的 `ExperienceInbox`。

这一阶段只负责：

- 向上贡献
- 写入父层 inbox
- 等待上层在收尾阶段处理

非顶层不允许：

- 直接推进候选到最终治理
- 直接写正式资产

### 顶层任务结束：总汇聚

顶层 `Persistent Agent` 绑定的任务结束时，也会触发一次 `ExperienceCollection`。

其输入包括：

- 顶层任务自身的终态材料
- 顶层 inbox 中来自下层的候选摘要

顶层要做的是总汇聚，而不是简单透传：

- 保留一部分下层候选
- 丢弃低价值候选
- 基于多个候选重写更高层经验
- 判断候选的正式去向

### 顶层治理：唯一最终分流点

所有进入顶层的候选都必须进入统一的 `ExperienceGovernance`。

顶层治理是首版唯一合法的最终分流点。任何进入顶层治理的候选都必须获得唯一最终去向，不允许长期悬空。

### 推荐消息流

首版推荐使用以下语义消息流：

```text
TaskTerminated
  -> ExperienceCollectionRequested
  -> ExperienceCandidateSubmitted
  -> ExperienceCandidatesAggregated
  -> ExperienceContributionDeliveredToParentInbox
  -> TopLevelExperienceGovernanceRequested
  -> ExperienceGovernanceResolved
  -> FinalWritebackCompleted
```

如果继续沿用现有 `WorkItem` 语义，则可表达为：

```text
非顶层任务结束
  -> ExperienceCollection WorkItem
  -> submit_experience_candidate
  -> ExperienceInbox(parent_task)

顶层任务结束
  -> ExperienceCollection WorkItem
  -> TopLevelGovernance
  -> LongTermMemory / Skill Package / SharedKnowledgeUpgrade / IncubationProposal
```

## 第三部分：首版闭环状态机与终态规则

### `ExperienceCandidate` 状态机

首版候选状态收敛为以下最小集合：

- `Submitted`
- `InInbox`
- `Aggregated`
- `GovernancePending`
- `NeedsUserApproval`
- `Approved`
- `Rejected`
- `Persisted`

### 非顶层状态推进

非顶层只负责贡献，因此状态推进如下：

```text
任务结束
  -> ExperienceCollection
  -> Candidate Submitted
  -> 写入父层 Inbox
  -> Candidate InInbox
  -> 父层汇聚时
     -> 原样保留继续上送 或 被组合吸收
  -> Candidate Aggregated
```

非顶层不允许把候选推进到：

- `GovernancePending`
- `Approved`
- `Persisted`

### 顶层状态推进

顶层治理时，候选状态推进如下：

```text
进入顶层汇聚结果
  -> GovernancePending
  -> 路由判断
     -> Approved -> Persisted
     -> NeedsUserApproval -> Approved/Rejected -> Persisted(如批准)
     -> Rejected
```

### 四类最终去向的终态规则

- `LongTermMemory`
  - 普通持久型 Agent 的低风险私有知识类经验
  - 可自动 `Approved -> Persisted`

- `Skill Package`
  - 可执行经验的正式去向
  - 默认需要用户确认

- `SharedKnowledge` 升级入口
  - 公共规则或公共事实的正式升级入口
  - 首版打通入口持久化，不要求复杂终审自动化

- `IncubationProposal`
  - `default Agent` 的私有知识/私有技能类经验正式去向
  - 必须用户确认

### `default Agent` 的状态规则

当顶层治理者是 `default Agent` 时：

- 知识类候选不能直接进入 `LongTermMemory`
- 可执行候选不能直接生成私有 `Skill Package`
- 这些候选只能进入：
  - `IncubationProposal`
  - 或 `Rejected`

但若候选被判定为公共规则或公共事实：

- 允许直接进入 `SharedKnowledge` 升级入口
- 这不视为 `default Agent` 私有资产沉淀

### `ExperienceInbox` 的最小状态语义

`ExperienceInbox` 不设计复杂状态机，只保留最小生命周期：

- `Pending`
- `Consumed`

候选的核心业务状态全部由 `ExperienceCandidate` 本身承载。

### 首版禁止出现的中断状态

首版禁止以下情况：

- 候选被提交后没有进入父层 inbox
- 候选进入顶层后没有进入治理
- 候选进入治理后没有进入终态
- 用户确认完成后没有触发最终写回
- 写回失败后没有可审计失败记录

首版必须保证：

```text
产生 -> 上送 -> 汇聚 -> 治理 -> 确认(如需要) -> 落盘/拒绝
```

## 第四部分：落盘模型与四个最终去向的存储边界

### 总体原则

候选是运行时治理对象，正式落盘对象必须是稳定、可审计、可读的持久化结构。

### `LongTermMemory`

`LongTermMemory` 是普通持久型 Agent 的私有知识沉淀层，只接收已经治理通过的低风险知识类经验。

首版适合进入 `LongTermMemory` 的内容：

- 稳定事实
- 领域约束
- 固定偏好
- 可复用策略
- 反模式
- 高价值纠错经验

建议补充最小追溯字段：

- `source_candidate_id`
- `source_task_id`
- `agent_id` 或等价归属信息
- `created_at`

### `Executable Skill Package`

可执行经验的正式落盘形态不是结构化条目，而是文件系统中的 Agent 私有 `Skill Package`。

每个 `Skill Package` 以目录作为真源，至少包含：

- `skill.md`
- `scripts/`
- `resources/`

首版推荐目录结构：

```text
agent_assets/
  <agent_name>/
    skills/
      <skill_id>/
        skill.md
        scripts/
        resources/
```

`skill.md` 必须承载：

- 技能名称
- 解决的问题
- 什么时候使用
- 使用步骤
- 依赖脚本或资源说明
- 风险与限制
- 来源任务与来源候选追溯信息

首版 `Skill Package` 只要求支持文本型资源，不要求二进制资产或复杂仓储治理。

运行时可维护轻量索引，但索引不是主数据源。文件目录本身才是可迁移、可读、可复用的正式资产。

### `SharedKnowledge` 升级入口

首版必须打通的是 `SharedKnowledge` 的升级入口持久化，而不是复杂的共享知识终审系统。

因此需要正式持久化一个共享知识升级入口对象，例如：

- `SharedKnowledgeUpgradeCandidate`

建议其至少包含：

- `candidate_id`
- `content`
- `kind`
- `scope_tags`
- `source_candidate_id`
- `source_agent_id`
- `source_task_id`
- `validation_status`
- `created_at`

其语义是：

- 已被顶层治理判定具备公共价值
- 已进入共享知识升级入口
- 但尚不等于已成为最终共享知识正文

### `IncubationProposal`

`IncubationProposal` 是 `default Agent` 的正式治理输出，不是临时对话结构。

它表示：

- 是否基于本次治理结果创建新持久型 Agent

建议至少包含：

- `proposal_id`
- `source_agent_id`
- `source_task_id`
- `proposed_agent_profile`
- `knowledge_candidate_ids`
- `executable_candidate_ids`
- `shared_knowledge_candidate_ids`
- `status`
- `created_at`

用户批准后，才真正执行：

- 创建新持久型 Agent
- 写入其 `LongTermMemory`
- 生成其私有 `Skill Package`
- 处理其共享知识升级候选

### 最小审计要求

首版所有正式落盘对象必须具备最小可追溯性，至少能回溯：

- `source_task_id`
- `source_candidate_id`
- 最终归属

## 第五部分：首版治理规则、自动分流条件与用户确认边界

### 治理输入与输出

顶层治理的输入是顶层汇聚后的候选集合，输出是每个候选的唯一最终去向。

治理输出只允许是：

- `LongTermMemory`
- Agent 私有 `Skill Package`
- `SharedKnowledge` 升级入口
- `IncubationProposal`
- `Rejected`

### 候选主语义

首版候选按主语义收敛为：

- `knowledge`
- `executable`
- `shared_knowledge`
- `discard`

顶层治理可以接受、修正或拒绝候选的 `kind_hint`，但最终必须生成唯一去向。

### 自动落盘规则

#### `knowledge -> LongTermMemory`

满足以下条件时，可自动写入 `LongTermMemory`：

- 顶层治理者是普通持久型 Agent
- 内容是私有、稳定、低风险经验
- 不具备明显公共规则属性
- 不依赖脚本或资源文件

#### `shared_knowledge -> SharedKnowledge 升级入口`

满足以下条件时，可自动进入共享知识升级入口：

- 候选被判定为跨 Agent 可复用的公共规则、公共事实或系统级约束

首版入口对象写入成功即可视为升级入口打通，不要求同步完成共享知识终审系统。

#### `discard -> Rejected`

以下情况可直接拒绝：

- 内容为空
- 低价值噪声
- 只是过程残留
- 与长期资产无关

### 必须用户确认的规则

以下情况一律走用户确认：

- `executable`
- `default Agent` 的私有知识/私有技能类产出
- 高影响共享知识升级

### `default Agent` 的治理规则

当顶层治理者是 `default Agent` 时：

- 私有知识类候选 -> `IncubationProposal`
- 私有可执行候选 -> `IncubationProposal`
- 公共规则/公共事实 -> 允许直接进入 `SharedKnowledge` 升级入口
- 无价值候选 -> `Rejected`

这保证：

- `default Agent` 不自动沉淀私有身份资产
- 公共知识升级链路不被阻断

### `Skill Package` 的生成规则

`executable` 候选经顶层治理和用户确认后，生成 Agent 私有 `Skill Package`。

写盘规则：

- `skill.md` 必须存在
- `scripts/` 与 `resources/` 可为空
- 若候选只有方法说明、没有脚本，也允许只生成 `skill.md`
- 真源是文件目录本身，而不是运行时索引

### 重复与冲突的首版策略

首版采用保守策略：

- `LongTermMemory` 允许一定冗余，后续再治理
- `Skill Package` 若 `skill_id` 冲突，可采用后缀避免阻断
- `SharedKnowledge` 升级入口允许相似候选并行存在

首版宁可保守冗余，也不要因复杂 dedup 导致链路中断。

## 第六部分：首版测试策略、迁移策略与实现优先级

### 首版必须验证的主链路

首版必须验证以下主链路：

- 非顶层局部经验产生
- 非顶层向父层贡献
- 父层汇聚
- 顶层最终治理
- 最终落盘
- 失败可审计

### 测试分层

建议分为三层：

- 领域单元测试
- 系统集成测试
- 端到端闭环测试

### 首版至少保留的 4 条闭环集成测试

#### Case 1：普通持久型 Agent 的知识类闭环

- 子任务产出知识候选
- 父层汇聚
- 顶层治理
- 自动写入 `LongTermMemory`

#### Case 2：普通持久型 Agent 的 executable 闭环

- 子任务产出 executable 候选
- 顶层治理判断需确认
- 用户批准
- 写出 Agent 私有 `Skill Package`

#### Case 3：公共知识升级入口闭环

- 顶层识别某候选属于公共规则
- 进入 `SharedKnowledgeUpgradeCandidate`
- 验证升级入口持久化成功

#### Case 4：`default Agent` 的孵化闭环

- 顶层治理者为 `default Agent`
- 私有知识候选与 executable 候选不直接写私有资产
- 生成 `IncubationProposal`
- 公共规则候选可直接进入 `SharedKnowledge` 升级入口

### 迁移原则

- 不保留旧经验直写链路作为长期兼容路径
- 新链路一旦打通，就收敛为唯一主路径
- 可短期保留兼容代码，但必须明确为过渡态

### 推荐迁移顺序

#### 第一步：统一候选主路径

- 所有经验提交统一收敛为 `ExperienceCandidate`
- 禁止绕过候选直接写正式资产

#### 第二步：打通 inbox 真正语义

- 非顶层候选必须进入父层 `ExperienceInbox`
- 不再只停留在全局 root store

#### 第三步：打通顶层治理触发

- 顶层结束时必须显式触发 `ExperienceGovernance`
- 不允许治理系统存在但无人调用

#### 第四步：接正式落盘层

- `LongTermMemory`
- `Skill Package`
- `SharedKnowledge` 升级入口
- `IncubationProposal`

#### 第五步：删除旧补丁逻辑

- 删除与旧经验直写逻辑冲突的旁路
- 删除语义不诚实的遗留字段和空转状态

### 实现优先级

#### P0：闭环打通

- 非顶层经验收集
- 父层 inbox 写入与消费
- 顶层治理触发
- 四个最终去向全部可达
- 用户确认分支可用
- 最小追溯信息可用

#### P1：落盘质量完善

- `Skill Package` 目录结构稳定
- `skill.md` 模板规范
- 共享知识升级入口元数据完善
- 治理失败记录标准化

#### P2：体验与治理增强

- 基础去重
- 简单冲突合并
- 更好的候选摘要
- 更好的治理可视化
- 更细粒度风险分级

## 约束总结

- 首版直接采用两层分层汇聚治理
- 非顶层 `TaskScoped Agent` 只负责经验产生、汇聚、向上贡献
- 顶层 `Persistent Agent` 负责最终治理与落盘
- `default Agent` 是顶层治理者，但不是默认长期资产宿主
- 知识类经验落 `LongTermMemory`
- 可执行类经验落为 Agent 私有 `Skill Package`
- 公共规则类经验进入 `SharedKnowledge` 升级入口
- `default Agent` 的私有沉淀统一走 `IncubationProposal`
- 首版允许实现粗糙，但不允许流程中断或候选悬空

## 结论

本设计将经验模块从当前的半闭环状态，收敛为一套“首版即可打通”的完整两层汇聚治理体系：

- 非顶层负责向上贡献经验
- 顶层负责统一治理与正式落盘
- `default Agent` 严格执行孵化制
- 可执行经验以 Agent 私有 `Skill Package` 形式持久化
- 公共规则通过 `SharedKnowledge` 升级入口沉淀

该方案既保留了终局架构的清晰边界，也保证首版实现可以在不追求复杂细节的前提下，真正跑通经验的产生、流转、治理与最终落盘全链路。
