# 经验落盘链路完整修复设计

> **状态：当前有效**

## 背景

当前经验模块已经具备以下能力：

- 子任务结束后可触发 `ExperienceCollection` work item
- `collector` 可以通过 `submit_experience_candidate` 生成 `ExperienceCandidate`
- 子层候选可以进入父任务 `ExperienceInbox`
- 顶层治理系统已经支持将候选分流到 `LongTermMemory`、`Skill Package`、`SharedKnowledge` 升级入口和 `IncubationProposal`

但在实际运行日志中，经验落盘链路仍存在三个关键问题：

- 顶层持久型任务结束后不会触发经验收集，导致链路停在“子候选已汇聚到父层 inbox”阶段
- 顶层治理者的身份可能被错误识别为执行 `ExperienceCollection` 的 `collector`，而不是原任务的实际治理者
- 审批结果当前通过扫描所有 `NeedsUserApproval` 候选应用，未按 `request_id` 精确命中，存在误批准或误拒绝风险

这些问题的共同根因不是“落盘 API 不可用”，而是经验链路的触发事实源、治理者身份来源和审批上下文绑定均不够语义诚实。

本设计的目标是做一次完整但克制的修复：不改变现有四类正式去向的语义，不新增新的治理子系统，只修正经验链路的触发、身份路由和审批精确匹配。

## 目标

- 修复顶层持久型任务在终态后无法进入经验治理与落盘的问题
- 修复经验治理者被错误识别为 `collector` 的问题
- 修复经验审批结果未按 `request_id` 精确匹配的问题
- 将经验收集触发统一收敛到 `Task` 终态这一单一事实源
- 保持现有四类正式去向语义不变：
  - `LongTermMemory`
  - Agent 私有 `Skill Package`
  - `SharedKnowledge` 升级入口
  - `IncubationProposal`
- 为顶层任务、子任务、`/finish` 手工结束路径补齐可回归测试

## 非目标

- 不重做 `ExperienceStore` 的整体建模
- 不引入新的全局“经验治理协调器”或额外状态机
- 不改变 `default Agent` 的治理边界
- 不调整 `collector` 的职责范围，仍然只负责提炼候选
- 不实现复杂候选去重、自动冲突合并或候选优先级排序
- 不修改正式写回目标的底层存储格式

## 设计原则

- 任务终态优先：经验收集的触发应依赖 `Task` 是否进入终态，而不是依赖某类 Agent 是否存在
- 身份显式传递：治理者身份必须在消息链路中显式传递，而不是从 work item 执行者反推
- 角色语义诚实：`collector` 是提炼执行者，不是经验宿主，也不是最终治理者
- 审批精确命中：审批请求与候选之间必须建立稳定绑定，禁止模糊批量更新
- 最小结构变更：尽量复用现有系统和消息，只修正当前断链点和错误绑定点

## 问题归因

### 顶层任务未进入经验收集

当前 `agent_termination_system` 只会在检测到 `TaskScoped Agent` 终止时，才生成 `ExperienceCollectionRequestMessage`。

这导致：

- 子任务结束时通常能进入经验收集
- 顶层任务由持久型 Agent 执行时，不会触发经验收集
- `/finish` 等手工结束路径虽然会产生 `TaskTerminatedMessage`，但若没有对应 `TaskScoped Agent`，也不会进入经验链路

最终结果是：

- 子候选已经成功进入父任务 `ExperienceInbox`
- 但父任务自己不会进行顶层经验收集
- 因而也不会触发 `TopLevelExperienceGovernanceRequested`
- 后续治理和落盘逻辑都不会运行

### 治理者身份被错误路由

当前 `ExperienceCollectionCompletedMessage.agent_id` 实际取自 `ExperienceCollection` work item 的 `assigned_agent`。

而这个 `assigned_agent` 是 `collector`，不是原任务的执行者或治理者。

这会带来两个错误风险：

- 非默认顶层任务的知识候选可能错误落到 `collector` 的长期记忆
- `default Agent` 的候选可能绕过默认治理规则，或以错误身份参与治理

### 审批结果未精确绑定

当前 `ExperienceStore.apply_confirmation_response()` 会遍历所有 `NeedsUserApproval` 候选并统一应用结果。

这会导致：

- 同时存在多个待审批候选时，一个审批响应可能影响多个候选
- 后续若支持更高并发审批，风险会进一步放大

## 总体方案

本次修复采用“任务终态统一驱动”的方案，核心做法如下：

- 将经验收集触发统一改为由 `TaskTerminatedMessage` 驱动
- 使用 `Task.delegate` 作为原任务治理者来源
- 在经验收集请求与完成消息中显式携带 `governing_agent_id`
- 在发起审批时为每个请求建立 `request_id -> candidate_id` 精确绑定
- 在确认响应时只更新目标候选及其关联对象

整体链路如下：

```text
Task 进入终态
  -> 读取 Task.delegate
  -> 生成 ExperienceCollectionRequestMessage(governing_agent_id)
  -> collector 执行 ExperienceCollection
  -> 提交 ExperienceCandidate
  -> ExperienceCollectionCompletedMessage(governing_agent_id)
  -> 子任务：聚合进父任务 inbox
  -> 顶层任务：推进 root candidates 到治理
  -> 由 governing_agent_id 执行最终治理
  -> 自动落盘 / 发起审批 / 审批后精确写回
```

## 数据结构改动

### `ExperienceCollectionRequestMessage`

新增字段：

- `governing_agent_id: AgentId`

保留字段：

- `task_id`
- `parent_task_id`

语义调整后，该消息表示：

- 某个已终态任务需要进行经验收集
- 此次经验收集完成后，应由 `governing_agent_id` 负责后续治理与正式写回

### `ExperienceCollectionCompletedMessage`

新增字段：

- `governing_agent_id: AgentId`

移除当前“使用 work item 执行者作为治理者”的隐式语义。

### `ExperienceStore`

新增内部索引：

- `approval_bindings: HashMap<Uuid, Uuid>`

含义为：

- key: `request_id`
- value: `candidate_id`

新增方法：

- `bind_approval_request(request_id, candidate_id)`
- `candidate_id_for_request(request_id) -> Option<Uuid>`
- `apply_confirmation_response_precise(request_id, selected_option) -> Option<Uuid>`

`apply_confirmation_response_precise()` 的返回值用于告诉调用方本次实际更新了哪个候选；若没有绑定则返回 `None`。

## 系统设计

### 一、经验收集触发系统

现有 `agent_termination_system` 改为基于任务终态触发，而不是扫描 `TaskScoped Agent`。

新逻辑：

- 监听 `TaskTerminatedMessage`
- 读取对应 `Task`
- 若任务不存在，跳过并记录日志
- 若 `Task.delegate` 为空，跳过并记录 `ExperienceCollectionSkipped`
- 若 `Task.delegate` 存在，则生成 `ExperienceCollectionRequestMessage`

这样可以统一覆盖：

- 子任务正常结束
- 顶层任务正常结束
- `/finish` 手工结束

### 二、经验收集 work item 创建

`experience_collection_workitem_system` 保持现有职责：

- 构造经验收集 prompt
- 附加 `submit_experience_candidate` 工具
- 创建 `ExperienceCollection` work item

唯一新增要求是：

- 将 `governing_agent_id` 从请求链路保留下去，供完成消息使用

此处不改变 `collector` 作为执行者的事实。

### 三、经验收集完成处理

`ExperienceCollectionCompletedMessage` 到来后：

- 若 `parent_task_id.is_some()`：
  - 调用 `aggregate_inbox_for_task(parent_task_id)`
  - 只做子层汇聚，不触发正式治理
- 若 `parent_task_id.is_none()`：
  - 调用 `promote_root_candidates_to_governance(task_id)`
  - 若存在候选，则生成 `ExperienceGovernanceRequestMessage`
  - 该治理请求中的 agent 必须使用 `governing_agent_id`

这样可以保证顶层治理始终由原任务治理者执行，而不是由 `collector` 或其他执行者代替。

### 四、顶层治理系统

`experience_governance_system` 的主逻辑保持不变，仍按 `ExperienceKindHint` 执行四类分流：

- `Knowledge`
- `Executable`
- `SharedKnowledge`
- `Discard`

但治理者来源改为显式的 `governing_agent_id`，因此：

- 非默认持久型 Agent 的知识候选仍可自动进入 `LongTermMemory`
- 非默认持久型 Agent 的 `Executable` 候选仍需用户确认后生成 `Skill Package`
- `SharedKnowledge` 候选仍进入升级入口
- `default Agent` 仍只能输出 `IncubationProposal` 或 `Rejected`

### 五、审批绑定与确认回写

在 `spawn_experience_confirmation()` 中：

- 生成 `request_id`
- 写入 `ToolConfirmationRequestMessage`
- 同时调用 `store.bind_approval_request(request_id, candidate_id)`

在 `experience_approval_result_system` 中：

- 调用 `apply_confirmation_response_precise(request_id, selected_option)`
- 若未命中绑定，记录 `ExperienceApprovalBindingNotFound` 并跳过
- 若命中目标候选，则只对该候选执行后续写回逻辑

这里禁止任何“回退到全量扫描待审批候选”的模糊策略。

## 关键行为约束

### `Task.delegate` 是治理者唯一来源

本次修复要求将 `Task.delegate` 视为原任务治理者的唯一事实源。

原因：

- `delegate` 代表任务当前由谁执行
- 它在任务生命周期内稳定存在
- 它比 work item 的 `assigned_agent` 更接近经验归属语义

本次设计不再从 `collector` 或其他辅助执行者反推治理者身份。

### `collector` 只负责提炼

`collector` 的职责不变，但语义收敛如下：

- 负责读取任务终态材料
- 负责提炼候选
- 负责调用 `submit_experience_candidate`

`collector` 不负责：

- 决定正式经验宿主
- 决定顶层治理身份
- 决定最终落盘目标

### 绑定缺失时宁可不写回

审批绑定缺失时：

- 不做模糊匹配
- 不批量应用结果
- 不尝试猜测候选

宁可保留候选在待确认或未处理状态，也不允许误写正式资产。

## 错误处理

### 任务不存在

若 `TaskTerminatedMessage` 对应任务不存在：

- 跳过经验收集
- 记录明确日志

### 任务无 delegate

若任务没有 `delegate`：

- 不生成 `ExperienceCollectionRequestMessage`
- 记录 `ExperienceCollectionSkipped`
- 原因应明确为 `missing_delegate`

### 治理 agent 不存在

若顶层治理阶段无法根据 `governing_agent_id` 找到 agent：

- 跳过治理
- 记录 `ExperienceGovernanceAgentNotFound`
- 不进行降级猜测

### 审批绑定不存在

若收到审批结果但找不到 `request_id` 绑定：

- 记录 `ExperienceApprovalBindingNotFound`
- 不更新任何候选
- 不触发后续正式写回

### 正式写回失败

正式写回失败时保持现有行为：

- 记录 `ExperienceWritebackFailed`
- 不将候选状态置为 `Persisted`

## 测试方案

### 单元测试

- `ExperienceStore` 可以为 `request_id` 绑定唯一 `candidate_id`
- `apply_confirmation_response_precise()` 只更新目标候选
- 未绑定 `request_id` 时不会影响任何待审批候选

### 流程测试

- 顶层持久型任务终态后可以生成 `ExperienceCollectionRequestMessage`
- 经验收集完成后，顶层任务会生成 `TopLevelExperienceGovernanceRequested`
- 顶层治理使用 `governing_agent_id`，而不是 `collector`
- 子任务经验仍能成功聚合到父任务 `ExperienceInbox`

### 回归测试

- 非默认持久型 Agent 的知识候选仍可自动写入 `LongTermMemory`
- 非默认持久型 Agent 的 `Executable` 候选审批后仍生成 `Skill Package`
- `SharedKnowledge` 候选仍只更新对应 `source_candidate_id`
- `default Agent` 的知识和可执行候选仍只产生 `IncubationProposal`
- `/finish` 结束顶层任务时也能进入完整经验链路

### 日志验证

新增或强化以下日志事件：

- `ExperienceCollectionRequested`
- `ExperienceCollectionSkipped`
- `TopLevelExperienceGovernanceRequested`
- `ExperienceApprovalBound`
- `ExperienceApprovalBindingNotFound`

## 实施顺序

建议按以下顺序落地：

1. 改造消息结构，补上 `governing_agent_id`
2. 重写经验收集触发入口为任务终态驱动
3. 修正经验收集完成到顶层治理的路由
4. 增加审批绑定索引并切换到精确匹配
5. 补齐单元测试和整链路集成测试

## 预期结果

完成修复后，经验链路将收敛为一条语义一致的主链路：

- 所有任务终态都会以统一方式决定是否触发经验收集
- 顶层治理总是由原任务治理者执行
- `collector` 不再错误承担治理身份
- 审批结果只影响目标候选
- 经验不会再停留在“已汇聚但未治理”的悬空状态

该修复能够解决当前日志中暴露的顶层经验断链问题，同时为后续扩展更复杂的经验治理能力保留清晰、稳定的结构边界。
