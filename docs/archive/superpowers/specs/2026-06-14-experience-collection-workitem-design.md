> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验收集 WorkItem 化设计

> **状态：当前有效**

## 背景

当前经验收集链路由 `experience_collection_dispatch_system` 在原任务结束后发起一轮 follow-up LLM 请求，
并继续绑定原 TaskScoped Agent 执行。

该实现已经暴露出明确问题：

- 经验收集依赖原 TaskScoped Agent 的生命周期，原 Agent 被清理后会导致后续工具权限校验失败
- 经验收集不是独立执行单元，而是挂在原执行链路尾部的补丁流程，语义不清晰
- 当前链路没有复用 `WorkItem`、统一派发和统一结果回收机制，和 `Summarization`、`Evaluation` 的治理型工作流不一致
- 经验收集输入复用了过多原 Agent 执行上下文，包含原 `system_prompt`、工具定义和长期记忆注入，既浪费 token，又会干扰经验提炼

项目已经明确偏向收敛式设计，避免为了短期兼容保留额外补丁结构。因此本次重构不做折中方案，
直接将经验收集升级为正式的 `WorkItem` 类型，并由持久 `collect` Agent 承担执行。

## 目标

- 将经验收集彻底收敛进 `WorkItem` 统一执行体系
- 让经验收集执行者与原 TaskScoped Agent 生命周期彻底解耦
- 复用现有通用设计：持久 Agent、tag 路由、`WorkItem` 派发、统一结果回收、通用工具权限模型
- 将经验收集输入收敛为净化后的只读快照，减少 token 消耗并避免无效干扰
- 保持经验候选写入 `ExperienceStore` / `ExperienceInbox` 的现有治理主链路不变

## 非目标

- 不继续兼容“原 TaskScoped Agent follow-up”旧路径
- 不为 `collect` Agent 引入独立配置协议或专用权限模型
- 不在首版引入新的复杂状态，如 `Skipped`
- 不在本次设计中改造经验治理规则本身
- 不在本次设计中引入新的 Skill 子系统或额外的收敛框架

## 核心决策

### 经验收集成为正式 `WorkItem`

- 新增 `WorkItemType::ExperienceCollection`
- 新增 `WorkItemOrigin::ExperienceCollection`
- 为经验收集新增专用构造器 `WorkItem::experience_collection(...)`
- 经验收集结果通过 `work_item_id` 回收到 `llm_response_system` 的专用处理分支

该设计与现有 `Summarization` 和 `Evaluation` 保持一致，不再通过一次额外的 follow-up 请求绕过统一执行框架。

### 执行者改为持久 `collect` Agent

- 在 `agents.toml` 中新增持久 Agent，例如 `collector`
- 通过 tag `collect` 参与 `workitem_dispatch_system` 的统一路由
- 经验收集不再 spawn task-scoped collector，也不再复用原 task-scoped worker

这样经验收集的“谁来执行”与“哪个任务终止后触发”被明确拆开：

- 原任务负责提供终态材料
- `collect` Agent 负责提炼和提交经验候选

### 权限与工具集复用现有通用机制

`collect` Agent 不引入专门的工具白名单实现，而是复用 `agents.toml` 和 `AgentToolPermissions`：

```toml
[[agent]]
name = "collector"
model = "gpt-4.1-mini"
tags = ["collect", "experience"]
description = "经验收敛专家，负责从任务终态材料中提炼经验候选"

[agent.tools]
default_permission = "Deny"
submit_experience_candidate = "Allow"
```

同时，`WorkItem::experience_collection(...)` 只向模型注入 `submit_experience_candidate` 的工具定义。

这带来两层一致约束：

- 模型可见工具集合已经是最小集
- 运行时权限仍由通用权限系统兜底校验

### 上下文不机械复用，而是显式净化

经验收集的输入不是原请求的完整上下文快照，而是一次面向“经验提炼”的收敛材料重建。

保留：

- 用户原始目标或任务目标
- 任务最终 `result_summary`
- 与结果直接相关的关键对话片段
- 与经验提炼相关的关键工具结果、失败原因和约束
- 必要的溯源信息，如 `task_id`、`parent_task_id`

移除：

- 原 Agent 的 `system_prompt`
- 原请求中暴露给模型的完整工具定义
- 原 Agent 注入的长期记忆文本
- 与经验提炼无关的中间探索噪声、格式化回声和冗余推理包袱

重建：

- 经验收集专用 `system_prompt`
- 精简后的 `conversation`
- 最小工具集

## 方案概览

重构后的主链路如下：

```text
TaskTerminatedMessage
  -> ExperienceCollectionRequestMessage
  -> ExperienceCollection WorkItem
  -> workitem_dispatch_system 选择 collect Agent
  -> AgentExecutionRequest(work_item_id)
  -> submit_experience_candidate
  -> ExperienceStore / ExperienceInbox
  -> llm_response_system 回收结果并结束 WorkItem
```

原 TaskScoped Agent 在任务终止后可以按正常维护逻辑清理，不再为经验收集保活。

## 触发与创建

### 触发入口

`agent_termination_system` 继续作为经验收集触发入口，但职责收缩为：

- 识别哪些终止任务需要进行经验收集
- 生成 `ExperienceCollectionRequestMessage`

它不再直接生成 `AgentExecutionRequest`。

### 请求消息

`ExperienceCollectionRequestMessage` 语义改为“请求创建经验收集 WorkItem”，不再表示“让原 Agent 继续执行 follow-up”。

建议保留字段：

- `task_id`
- `parent_task_id`
- `parent_agent_id`

建议删除字段：

- `agent_id`

删除 `agent_id` 的原因是执行者已经不再是原 TaskScoped Agent，保留该字段只会制造语义误导。

### WorkItem 构建

新增 `WorkItem::experience_collection(...)`，统一负责：

- 构造经验收集 prompt
- 注入经验收集专用 `system_prompt`
- 注入净化后的 `conversation`
- 注入最小工具集
- 设置 `collect` 标签
- 设置 `WorkItemOrigin::ExperienceCollection`
- 设置经验治理相关的写回目标

这样经验收集相关组装逻辑不会散落在多个 system 中。

## WorkItem 派发

`workitem_dispatch_system` 需要扩展 `ExperienceCollection` 分支。

选择规则：

- `agent.kind == Persistent`
- `agent.capabilities.tags` 包含 `collect`

找不到 `collect` Agent 时：

- 对应 WorkItem 直接标记为 `Failed`
- 不回退到旧 follow-up 路径
- 不临时 spawn task-scoped collector

这里坚持失败语义诚实：系统没有可用的经验收集执行者时，应明确失败，而不是偷偷切回旧实现。

### 请求类型

首版经验收集请求继续使用 `AgentRequestKind::LlmCompletion`。

原因：

- `WorkItemType` 已经足够表达工作语义
- 结果回收依赖 `work_item_id`
- 避免为本次重构扩大 `AgentRequestKind` 枚举范围

如果后续确实需要更强的审计语义，再单独评估是否新增 `AgentRequestKind::ExperienceCollection`。

## 结果回收

`llm_response_system` 需要新增 `handle_experience_collection_work_item_result(...)`。

该处理分支职责如下：

- 根据 `work_item_id` 定位 `ExperienceCollection` WorkItem
- 判断是否完成了合法的经验候选提交
- 将 WorkItem 标记为 `Completed` 或 `Failed`
- 清理 WorkItem 实体和结果消息实体

该分支不再承担以下旧职责：

- 保活原 TaskScoped Agent
- 修复旧 follow-up 链路的权限问题
- 兼容原 Agent 已销毁后的补丁行为

## 成功与失败判定

### 成功判定

首版采用严格成功判定：

- 至少发生一次合法的 `submit_experience_candidate`
- 候选成功进入 `ExperienceStore` / `ExperienceInbox`

仅有普通文本总结但没有提交候选，不算成功。

### 失败判定

以下场景直接视为 `Failed`：

- 没有可用的 `collect` Agent
- LLM 请求失败
- 返回非预期输出，且没有候选提交
- 工具执行失败导致候选未入库

首版不引入 `Skipped` 等额外状态，保持状态机极简。

## 上下文净化策略

经验收集使用“净化后的收敛快照”，而不是原任务上下文的机械复刻。

### 输入材料来源

- `Task.content`
- `Task.result_summary`
- `ShortTermMemory` 中与结果相关的关键对话
- 必要的工具输出事实
- 与经验提炼有关的失败信息和约束

### 过滤规则

- 不带入原 `system_prompt`
- 不带入原始工具定义列表
- 不带入原 Agent 的 LTM 注入结果
- 不带入与经验提炼无关的冗余历史消息

### 产出形式

建议由现有的 `build_experience_collection_conversation(...)` 升级为“经验收集快照构建器”，
明确其职责是：

- 过滤无效上下文
- 重建经验收集对话材料
- 生成面向 `collect` Agent 的收敛输入

## 需要删除或退役的旧结构

### 删除 `ExperienceCollectionTracker`

`ExperienceCollectionTracker` 仅服务于旧链路中的“阻止原 Agent 被提前清理”语义。

在经验收集 WorkItem 化之后：

- 原 Agent 不再承担经验收集执行职责
- 经验收集执行者改为持久 `collect` Agent

因此该 tracker 不再有实际职责，应直接删除，避免继续给维护者造成认知负担。

### 退役旧 follow-up 派发语义

以下旧逻辑应删除或改写：

- `experience_collection_dispatch_system` 中直接生成 `AgentExecutionRequest`
- `ExperienceCollectionRequestMessage.agent_id`
- 基于 tracker 阻止 task-scoped agent cleanup 的逻辑
- 将经验收集视为“原 Agent 最后一轮 follow-up”的实现假设

## 模块边界调整

### `src/domain/work_item.rs`

- 新增 `WorkItemType::ExperienceCollection`
- 新增 `WorkItemOrigin::ExperienceCollection`
- 根据现有设计新增合适的写回目标
- 新增 `WorkItem::experience_collection(...)`

### `src/systems/contribution.rs`

- 保留任务终止触发入口
- 将经验收集请求转换为 WorkItem
- 负责净化快照构建
- 保留经验治理与候选写入的既有链路

### `src/systems/dispatch/workitem_dispatch.rs`

- 新增 `ExperienceCollection` 的持久 Agent 路由分支

### `src/systems/transform/llm_response.rs`

- 新增 `handle_experience_collection_work_item_result(...)`
- 统一回收经验收集 WorkItem 结果

### `src/systems/maintenance.rs`

- 删除对 `ExperienceCollectionTracker` 的依赖
- 恢复 task-scoped agent 的正常终止清理职责

### `agents.toml`

- 新增持久 `collector` Agent 配置

## 测试策略

建议至少补充以下测试：

- `WorkItem::experience_collection(...)` 构造测试
- `workitem_dispatch_system` 能正确为经验收集选择 `collect` Agent
- 缺少 `collect` Agent 时 WorkItem 标记为 `Failed`
- 经验收集成功调用 `submit_experience_candidate` 后，WorkItem 进入 `Completed`
- 候选正确写入 `ExperienceInbox`
- 上下文净化测试，确认原 `system_prompt`、工具定义和 LTM 注入内容被剔除
- 回归测试，验证原任务 Agent 被清理后，经验收集仍然可以独立成功完成

## 迁移原则

- 本次重构不保留旧路径回退
- 先让经验收集在架构层面与 `Summarization`、`Evaluation` 对齐
- 再在实现阶段逐步删掉旧补丁结构，避免“新旧双轨”长期共存

## 预期收益

- 消除原 TaskScoped Agent 被清理后导致的权限校验失败
- 让经验收集成为语义清晰、生命周期独立的治理型工作流
- 降低经验收集的 token 开销和上下文噪声
- 最大化复用现有通用模块，避免为 `collect` 再发明一套专门机制
- 为后续治理型 WorkItem 扩展保持统一演进方向
