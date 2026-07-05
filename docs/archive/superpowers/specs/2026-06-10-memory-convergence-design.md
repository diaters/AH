> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 记忆系统概念收敛设计

## 背景

当前项目中与记忆相关的概念存在重叠：

- `ShortTermMemory` 已经承担任务级上下文与摘要压缩职责
- `LongTermMemory` 已经承担 Agent 长期记忆职责
- `SpaceKnowledge` 已经承担全局共享知识职责
- `AgentExperience` 与 `LongTermMemory` 在语义上高度重叠

现状问题主要有三类：

- 概念边界不清，导致文档、代码和讨论中存在重复术语
- 长期记忆当前采用全量 prompt 注入，容易膨胀并污染上下文
- 共享知识库与 Agent 私有记忆的升级边界不够严格

本设计的目标是把记忆系统收敛为最少且诚实的概念集合，并为后续实现提供明确边界。

## 目标

- 收敛记忆系统概念，只保留必要的三类记忆
- 删除 `AgentExperience`，避免双主数据源
- 明确任务记忆、Agent 私有记忆、全局共享知识的职责边界
- 明确 `LongTermMemory` 的受控注入方式，禁止全量无差别注入
- 明确共享知识库的准入、审核与升级路径
- 为长期记忆和共享知识引入衰退治理，避免记忆冗余

## 非目标

- 本设计不引入向量数据库或复杂语义检索基础设施
- 本设计不追求拟人化或不可解释的“智能遗忘”系统
- 本设计不改变 `ShortTermMemory` 的任务级定位
- 本设计不要求一步完成所有代码重构，可以先完成概念和边界收敛

## 概念模型

最终只保留三类记忆：

| 概念 | 归属 | 作用 | 是否默认长期保留 |
|------|------|------|------------------|
| `ShortTermMemory` | `Task` | 当前任务上下文、对话历史、工具结果、摘要压缩 | 否 |
| `LongTermMemory` | `Agent` | Agent 私有、可复用、稳定的长期经验与事实 | 是 |
| `SharedKnowledgeBase` | `Space` | 全局共享、稳定、公共、经审核的知识 | 是 |

本设计明确废止：

- `AgentExperience`

`AgentExperience` 不再作为独立概念、独立结构或独立存储存在。后续所有“经验沉淀”都直接表述为写入 `LongTermMemory`。

## 各概念职责

### ShortTermMemory

`ShortTermMemory` 绑定 `Task`，用于承载当前任务运行所需的上下文。

允许进入 `ShortTermMemory` 的内容：

- 用户输入
- 模型输出
- 工具调用结果
- 阶段性摘要
- 为当前任务服务的必要系统说明

`ShortTermMemory` 的原则：

- 它是工作内存，不是长期资产仓库
- 它可以保留任务执行过程，但默认不跨任务继承
- 它可以被压缩、摘要、截断
- 它在任务结束后默认丢弃原始流水账

### LongTermMemory

`LongTermMemory` 绑定单个 `Agent`，用于保存该 Agent 在自己负责领域内可复用、稳定、能改善未来决策质量的内容。

适合进入 `LongTermMemory` 的内容：

- 稳定约束
- 固定偏好
- 可靠策略
- 领域事实
- 高价值纠错经验
- 经反复验证的模式总结

不适合进入 `LongTermMemory` 的内容：

- 临时中间结果
- 纯日志和流水账
- 一次性过程记录
- 未验证猜测
- 与该 Agent 领域无关的信息

`LongTermMemory` 的原则：

- 只服务当前 Agent，不直接视为公共知识
- 默认写入应偏保守，宁可少记，也不要积累噪音
- 长期记忆必须参与衰退治理

### SharedKnowledgeBase

`SharedKnowledgeBase` 绑定全局 `Space`，表示面向多个 Agent 的共享知识面。

适合进入 `SharedKnowledgeBase` 的内容：

- 项目规范
- 系统级边界
- 通用配置约定
- 公共事实
- 经过重复验证的通用知识

不适合进入 `SharedKnowledgeBase` 的内容：

- 单个 Agent 的私有偏好
- 只对局部任务成立的策略
- 未审核的经验结论
- 单次任务产物
- 调试痕迹

`SharedKnowledgeBase` 的原则：

- 它不是结果回收站
- 它不是所有任务产物的默认归宿
- 它必须是少量、稳定、公共、可审计的知识集合

## 生命周期与数据流

### 任务开始

- 创建或加载 `ShortTermMemory`
- 读取当前 Agent 的 `LongTermMemory`
- 根据需要查询 `SharedKnowledgeBase`

### 任务执行中

- 当前对话、工具结果、系统说明持续进入 `ShortTermMemory`
- 允许对 `ShortTermMemory` 做摘要压缩以控制上下文窗口
- 不允许把执行过程原样直接写入 `LongTermMemory`

### 任务结束

- 对 `ShortTermMemory` 做一次价值提炼
- 低价值内容直接丢弃
- 私域且可复用的内容写入 `LongTermMemory`
- 具备公共价值的内容只形成共享知识候选，不直接入库

### 共享知识升级

共享知识升级严格遵循以下链路：

```text
ShortTermMemory
  -> 提炼
  -> LongTermMemory
  -> 候选审核
  -> SharedKnowledgeBase
```

以下链路明确禁止：

- `ShortTermMemory -> SharedKnowledgeBase` 直接写入
- 普通 Agent 直接写入 `SharedKnowledgeBase`

## LongTermMemory 注入设计

### 问题定义

当前实现会把 `LongTermMemory` 全量拼接进 prompt。该方式虽然简单，但存在明显问题：

- token 成本会随长期记忆增长持续上升
- 低价值旧记忆会污染当前任务上下文
- 不同任务无法按相关性选择真正有用的记忆

因此，本设计明确禁止长期记忆的全量无差别注入。

### 注入策略

`LongTermMemory` 采用两段式注入：

- `Core Agent Memory`
- `Relevant Agent Memory`

#### Core Agent Memory

`Core Agent Memory` 表示少量稳定、长期有效、对当前 Agent 决策始终重要的条目。

特点：

- 数量严格受限
- 默认注入
- 必须高置信、高重要度
- 不允许无限扩张

典型内容：

- 领域边界
- 关键禁忌
- 固定偏好
- 稳定工作准则

#### Relevant Agent Memory

`Relevant Agent Memory` 表示除核心记忆外，按当前任务动态筛选出的相关记忆。

特点：

- 不默认全量注入
- 必须经过任务相关性筛选
- 受条目数量和 token 预算双重限制

### 推荐 prompt 结构

```text
[Core agent memory]
- ...

[Relevant agent memory]
- ...

[Previous context summary]
...

[Conversation history]
...

[Current request]
...
```

## LongTermMemory 条目模型

### 最小字段

长期记忆条目应从当前通用 `MemoryEntry` 模型中独立出长期治理所需字段。建议至少包含：

| 字段 | 作用 |
|------|------|
| `content` | 记忆正文，必须是提炼后的稳定表述 |
| `kind` | 条目类型，如 `constraint`、`preference`、`strategy`、`fact`、`anti_pattern` |
| `scope_tags` | 领域标签，用于筛选和排序 |
| `importance` | 重要度 |
| `pin` | 是否为核心记忆候选 |
| `created_at` | 创建时间 |
| `last_accessed_at` | 最近一次被命中和注入的时间 |
| `reuse_count` | 复用次数 |
| `decay_score` | 衰退分数 |
| `source` | 来源摘要 |
| `confidence` | 置信度 |

### 设计原则

- `content` 必须是提炼结果，不允许保存原始流水账
- `pin` 只能用于极少数核心记忆
- `confidence` 低的条目不能进入核心记忆
- `decay_score` 过低的条目不能继续参与注入

## LongTermMemory 排序与注入规则

### Core Agent Memory 规则

- 仅从 `pin = true` 的条目中选择
- 设置严格上限，例如 3 到 10 条
- 超限时按 `importance`、`confidence`、`recency` 排序截断

### Relevant Agent Memory 规则

- 排除已进入 `Core Agent Memory` 的条目
- 先按当前任务做相关性过滤
- 再结合以下信号做排序：
  - `task_match_score`
  - `importance`
  - `reuse_count`
  - `last_accessed_at`
  - `confidence`
  - `decay_score`
- 最终受 `top_k` 和 `token_budget` 双重限制

### 排序原则

可采用简单可解释的线性排序思路：

```text
final_score =
  task_match_score
  + importance_weight
  + reuse_bonus
  + recency_bonus
  + confidence_weight
  - decay_penalty
```

本设计强调可解释、可调试、可维护，不追求伪精确的复杂评分系统。

## SharedKnowledgeBase 准入与升级

### 准入原则

进入 `SharedKnowledgeBase` 的内容必须同时满足：

- 跨 Agent 可复用
- 具有稳定性
- 属于公共规则、公共事实、公共约定或公共约束
- 已被验证
- 值得长期保留

写入权限收敛为：

- 用户显式写入
- 主控链路审核通过后写入

普通 Agent 不能直接写入 `SharedKnowledgeBase`。

### 升级关系

`LongTermMemory` 到 `SharedKnowledgeBase` 不是自动同步关系，而是候选升级关系：

```text
LongTermMemory
  -> knowledge candidate
  -> human/brain review
  -> SharedKnowledgeBase
```

共享知识升级分为三个步骤：

- `Extract`：识别潜在共享知识候选
- `Review`：人工或主控判断其公共性与稳定性
- `Promote`：审核通过后正式入库

### 最小字段

共享知识条目建议至少包含：

| 字段 | 作用 |
|------|------|
| `content` | 知识正文 |
| `kind` | 知识类型 |
| `scope_tags` | 适用范围标签 |
| `importance` | 重要度 |
| `created_at` | 创建时间 |
| `last_accessed_at` | 最近访问时间 |
| `reuse_count` | 复用次数 |
| `confidence` | 置信度 |
| `validation_status` | 审核状态，如 `candidate`、`approved`、`rejected`、`deprecated` |
| `approved_by` | 审批来源 |
| `source` | 来源信息 |

## 检索与读取策略

### ShortTermMemory

- 直接作为当前任务上下文使用
- 继续采用摘要压缩和窗口控制

### LongTermMemory

- 禁止全量注入
- 采用 `Core + Relevant` 的受控注入方式
- 每次注入后更新访问时间和复用次数

### SharedKnowledgeBase

- 默认按需查询
- 不建议默认每次都注入 prompt
- 检索结果同样受 `top_k` 和 `token_budget` 限制

默认读取优先级为：

```text
ShortTermMemory
  -> LongTermMemory
  -> SharedKnowledgeBase
```

## 衰退与遗忘机制

### 设计原则

- 不做复杂拟人化遗忘系统
- 采用诚实、可解释、可治理的打分淘汰机制
- 所有长期记忆都必须参与衰退治理

### ShortTermMemory

- 通过摘要、压缩、截断控制大小
- 任务结束后默认不保留流水账

### LongTermMemory

- 记忆条目按 `recency`、`reuse_count`、`importance`、`confidence`、`decay_score` 综合治理
- 被注入和复用后，条目可获得适度回升
- 长期未访问、低价值条目逐步衰退
- 低于阈值后先停止注入，再进入清理或归档

### SharedKnowledgeBase

- 同样参与衰退治理
- 衰退节奏应慢于 `LongTermMemory`
- 更适合先标记为 `deprecated`，后续再清理

## 迁移映射

### 保留并明确语义

- 保留 `ShortTermMemory`，继续作为任务级上下文记忆
- 保留 `LongTermMemory`，明确其语义为 Agent 私有长期记忆

### 重命名或语义收敛

- 将当前 `SpaceKnowledge` 在领域语义上收敛为 `SharedKnowledgeBase`
- 若短期代码暂不改名，文档中必须明确其领域名称为共享知识库

### 删除

- 删除 `AgentExperience`
- 删除所有把 `AgentExperience` 与 `LongTermMemory` 并列描述的文档表述
- 删除任何“长期经验”和“长期记忆”双主数据源设计

## 约束总结

- 只保留三类记忆：`ShortTermMemory`、`LongTermMemory`、`SharedKnowledgeBase`
- 删除 `AgentExperience`
- 长期记忆默认保守写入
- 禁止长期记忆全量注入
- 共享知识必须经过人工或主控审核
- 不保留流水账式长期记忆
- 长期记忆和共享知识都必须参与衰退治理

## 开放实现问题

以下问题留待后续实现计划阶段细化，不影响当前概念收敛：

- `task_match_score` 的最小实现采用关键词匹配、标签匹配还是轻量规则组合
- `pin` 条目的创建来源是否仅允许人工或主控设置
- `LongTermMemory` 与 `SharedKnowledgeBase` 的物理存储是否继续共用底层条目结构
- 共享知识查询是否保留现有 `knowledge_search` 工具名或同步重命名
