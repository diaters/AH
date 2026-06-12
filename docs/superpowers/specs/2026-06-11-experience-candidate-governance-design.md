# 经验候选治理与可执行记忆极简设计

## 背景

当前实现中，子 Agent 结束后会直接将筛选后的 `LongTermMemoryEntry` 回写给父 Agent。该链路虽然简单，但已经暴露出几个问题：

- “经验提炼”和“长期吸收”被压成同一步，父 Agent 缺少独立治理空间
- 子任务形成的局部经验、父任务形成的组合经验没有清晰层次
- 如果未来要支持带脚本、模板、资源文件的可复用能力，现有纯文本长期记忆模型承载力不足
- `default Agent` 不应自动积累长期身份资产，但当前链路缺少孵化前的缓冲层

同时，项目已经明确不再引入 `AgentExperience` 这样的并行大概念，并倾向于将“Agent 级 Skill”收敛为长期资产的一种特殊形态，而不是再造一个平行子系统。

因此，本设计将经验沉淀链路收敛为一个极简闭环：

```text
任务结束
  -> 经验收敛对话
  -> ExperienceCandidate
  -> 父级 ExperienceInbox
  -> 顶层治理
  -> Knowledge Memory / Executable Memory / 放弃 / 孵化
```

## 目标

- 将经验沉淀从“直接吸收长期记忆”重构为“候选生成 -> 分级治理 -> 选择性落盘”
- 允许子任务产生局部经验，父任务产生组合经验，并支持逐层上传
- 将“Agent 级 Skill”收敛为长期资产中的 `Executable Memory`
- 支持脚本、模板、资源文件等资产型经验，但不把资产正文塞进长期记忆本体
- 保持 `ExperienceCandidate`、`LongTermMemory`、`Executable Memory` 和资产引用对人类可读
- 为 `default Agent` 引入“孵化前不落盘”的强约束

## 非目标

- 本设计不引入独立的通用 Skill 执行框架
- 本设计不引入向量检索、候选相似度去重或复杂语义合并
- 本设计不实现复杂资产版本治理、垃圾回收或去重
- 本设计不让 `ExperienceCandidate` 直接进入 `ShortTermMemory`
- 本设计不改变现有 `SharedKnowledgeBase` 的审核模型
- 本设计不要求首版支持任意递归的可执行能力编排

## 设计原则

- 概念收敛优先：不新增独立 Skill 子系统，`Executable Memory` 仍属于长期资产体系
- 治理与上下文分离：候选是待治理对象，不是任务上下文条目
- 分层生成：子任务产生局部候选，父任务可产生组合候选
- 人类可读：所有需确认、审计、孵化的对象都必须可直接查看
- 资产外置：脚本、模板、资源文件进入独立资产仓，长期记忆仅保存引用
- 默认保守：知识类可自动沉淀，可执行类和带资产依赖的候选必须经过用户确认

## 方案概览

本设计新增三个极简核心对象：

- `ExperienceCandidate`：任务结束后由收敛对话提交的经验候选
- `ExperienceInbox`：父任务绑定的候选收件箱，只承载待治理候选引用
- `ExecutableMemoryEntry`：落盘后的可执行经验条目，语义上属于长期资产

核心约束如下：

- 子任务结束后，子 Agent 不直接回写 `LongTermMemoryEntry`
- 子任务结束后会复用原任务上下文，额外发起一轮“经验收敛对话”
- 这轮对话只负责调用 `submit_experience_candidate`
- 候选进入父任务的 `ExperienceInbox`，不注入 `ShortTermMemory`
- 父任务结束后，也可以发起一轮同样的经验收敛对话，用于生成组合候选
- 顶层持久型 Agent 负责最终治理与落盘
- 如果顶层为 `default Agent`，则只生成孵化候选包，等待用户确认

## 概念模型

### `ExperienceCandidate`

`ExperienceCandidate` 是一次任务结束后产出的治理候选，不是正式长期资产。

首版只保留以下字段：

| 字段 | 说明 |
|------|------|
| `candidate_id` | 候选唯一标识 |
| `producer_task_id` | 候选来源任务 |
| `producer_agent_id` | 候选来源 Agent |
| `title` | 人类可读的候选标题 |
| `kind_hint` | 候选建议去向：`knowledge`、`executable`、`shared_knowledge`、`discard` |
| `payload` | 候选核心内容，按类型区分 |
| `dependency_refs` | 对其他候选或长期资产的依赖引用 |
| `status` | `Submitted`、`Queued`、`NeedsUserApproval`、`Approved`、`Rejected` |

其中 `payload` 使用最小变体：

- `knowledge`
  - `content`
  - `memory_kind`
- `executable`
  - `intent`
  - `when_to_use`
  - `asset_refs`

首版不单独持久化 `confidence`、`risk_level`、`source_trace` 等字段。风险通过简单规则推导：

- `kind_hint = executable` 或存在 `asset_refs` 时，视为需要用户确认
- 纯知识类候选默认可自动落盘

### `ExperienceInbox`

`ExperienceInbox` 是父任务绑定的治理缓冲层，只保存待处理候选引用，不承担上下文注入职责。

首版字段：

| 字段 | 说明 |
|------|------|
| `owner_task_id` | 当前收件箱绑定的任务 |
| `owner_agent_id` | 当前负责治理的 Agent |
| `candidate_ids` | 进入该收件箱的候选列表 |

### `ExecutableMemoryEntry`

`ExecutableMemoryEntry` 表示经过治理和确认后的可执行经验条目。它不是独立 Skill 系统，而是长期资产中的可执行变体。

首版字段：

| 字段 | 说明 |
|------|------|
| `memory_id` | 可执行记忆唯一标识 |
| `title` | 可读名称 |
| `intent` | 解决什么问题 |
| `when_to_use` | 适用场景 |
| `asset_refs` | 关联的资产引用 |
| `dependency_refs` | 依赖的其他可执行记忆或候选 |

`ExecutableMemoryEntry` 必须对人类可读，且其引用的资产若为文本内容，也必须可直接查看。

## 生成与上传流程

### 子任务结束

子任务结束后，不再直接把该 Agent 的长期记忆条目批量交给父 Agent。

改为：

1. 基于该子任务已有上下文、任务摘要、工具调用记录和任务结果
2. 发起一轮额外的经验收敛对话
3. 在该轮对话中向 Agent 提供 `submit_experience_candidate` 的使用说明
4. 由 Agent 自行提交一个或多个 `ExperienceCandidate`
5. 运行时将这些候选投递到父任务的 `ExperienceInbox`

这样做有两个目的：

- 复用原有任务上下文，提高缓存命中率并降低 token 消耗
- 将“经验产出”显式化为结构化工具调用，而不是隐式筛选已有记忆

### 父任务结束

父任务结束时，如果父 Agent 仍是任务型 Agent，也执行一次同样的经验收敛对话。

父级候选有三种来源：

- 直接沿用子任务上送的候选
- 基于多个子候选形成新的组合候选
- 基于父任务自身执行过程形成新的局部候选

因此，父级候选允许依赖子级候选，但首版只保留显式引用关系，不提供复杂自动编排能力。

## 治理与上下文隔离

`ExperienceCandidate` 明确不进入 `ShortTermMemory`。

原因如下：

- 候选是待治理对象，不是已经可信的任务工作记忆
- 未审候选若进入上下文，会污染父 Agent 的主推理链路
- 候选的职责是“待归档/待晋升/待丢弃”，而不是“立即辅助当前任务继续推理”

因此，本设计要求：

- 候选只进入 `ExperienceInbox`
- 父任务执行中默认不消费 `ExperienceInbox`
- 父任务只在任务收尾阶段显式读取候选摘要并做最终整理

## 顶层治理与落盘规则

### 普通持久型 Agent

当候选上传到顶层持久型 Agent 所属任务结束时，进入最终治理：

- `knowledge` 候选自动落盘为现有 `LongTermMemoryEntry`
- `executable` 候选或带 `asset_refs` 的候选进入用户确认
- 用户确认后，落盘为 `ExecutableMemoryEntry`
- 用户拒绝后，该候选标记为 `Rejected`

### `default Agent`

`default Agent` 视为无长期身份资产的默认母体，不应自动积累记忆。

因此，当顶层为 `default Agent` 时：

- 不直接落盘任何候选
- 只形成一份“孵化候选包”
- 用户确认后，创建新的持久型 Agent
- 将该候选包中的 `knowledge` 与 `executable` 资产一起写入新 Agent
- 用户拒绝后，本次任务产生的候选全部不落盘

该规则的目的，是防止 `default Agent` 退化为无限膨胀的总控记忆池。

## 资产仓边界

脚本、模板、资源文件等不直接存入长期记忆条目。

本设计要求新增独立的 `Agent Asset Store`，用于保存：

- 文本脚本
- 提示模板
- 配置片段
- 其他可复用资源文件

首版仅要求资产仓具备：

- 写入资产
- 通过 `asset_id` 读取资产
- 被 `ExecutableMemoryEntry` 引用

首版不要求：

- 复杂版本治理
- 去重
- 回收清理

## 模块边界调整

### 领域层

- `src/domain/contribution.rs`
  - 从旧的“记忆贡献请求/吸收消息”模型转向 `ExperienceCandidate`、`ExperienceInbox`、顶层治理消息
- `src/domain/memory.rs`
  - 保留现有 `LongTermMemoryEntry`
  - 新增最小 `ExecutableMemoryEntry`

### 系统层

- `src/systems/contribution.rs`
  - 不再核心依赖“从子 Agent 长期记忆筛选并直接吸收”
  - 改为负责经验收集触发、候选投递和顶层治理

### 工具层

- 新增 `submit_experience_candidate`
  - 仅供经验收敛对话使用
- 新增 `list_experience_candidates`
  - 仅供父任务收尾阶段读取候选摘要使用

## ECS 消息流

首版建议引入以下最小消息：

| 消息 | 作用 |
|------|------|
| `ExperienceCollectionRequest` | 任务结束后触发经验收敛对话 |
| `ExperienceCandidateSubmitted` | 表示候选已成功提交 |
| `ExperienceGovernanceRequest` | 顶层任务结束后触发最终治理 |
| `IncubationProposal` | `default Agent` 任务结束后生成的孵化提案 |

推荐流程：

```text
子任务结束
  -> ExperienceCollectionRequest
  -> 经验收敛对话
  -> submit_experience_candidate
  -> ExperienceCandidateSubmitted
  -> 父任务 ExperienceInbox

父任务结束
  -> ExperienceCollectionRequest
  -> 父级候选生成
  -> 若仍有父级则继续上传
  -> 若到顶层则 ExperienceGovernanceRequest
```

## 人类可读性要求

以下对象必须保持人类可直接读取：

- `ExperienceCandidate`
- `LongTermMemoryEntry`
- `ExecutableMemoryEntry`
- 资产引用元数据

如果资产本体是文本脚本、模板或配置，则资产正文也必须可直接读取。

这样可以支持：

- 用户确认 `Executable Memory` 是否应落盘
- 用户确认 `default Agent` 是否应孵化为新持久型 Agent
- 人类审查 Agent 沉淀了哪些经验和可执行能力
- 后续排查错误经验、危险脚本和失效依赖

## 测试策略

首版仅覆盖关键主链路：

- 子任务结束后可以触发经验收敛请求
- `submit_experience_candidate` 可以生成候选并写入父级 `ExperienceInbox`
- 父任务结束时可以读取 inbox 并生成组合候选
- 普通持久型 Agent 的知识类候选可以自动落盘
- 可执行类候选会进入用户确认，而不是自动落盘
- `default Agent` 顶层结束时只生成孵化提案，不直接写长期资产

## 风险与缓解

### 额外对话轮带来的 token 成本

风险：

- 每个任务结束后新增一轮经验收敛对话，会增加模型调用成本

缓解：

- 复用原任务上下文，提升缓存命中率
- 首版只在任务终止点触发，不在执行中频繁生成候选

### 候选过多导致父级治理负担上升

风险：

- 大量子任务可能带来过多候选，增加父任务收尾复杂度

缓解：

- 首版先保持保守生成策略
- 候选不进入主上下文，仅在收尾阶段处理

### `Executable Memory` 过早膨胀为通用脚本系统

风险：

- 可执行经验可能被误用为复杂工作流引擎

缓解：

- 首版只表达“用途 + 场景 + 资产引用 + 依赖”
- 不实现独立 Skill 执行协议和复杂编排能力

## 结论

本设计将经验沉淀从“直接写回长期记忆”重构为“候选生成、分层上传、顶层治理、选择性落盘”的极简闭环。

它保持了以下收敛方向：

- 不新增独立 Skill 系统
- `Executable Memory` 仍属于长期资产
- 候选与任务上下文分离
- 资产外置但保持可读
- `default Agent` 不自动积累长期身份资产

在此基础上，系统可以先稳定落地经验候选治理主链路，再根据真实使用情况决定是否追加更复杂的检索、治理或执行能力。
