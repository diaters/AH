# 会话空闲结束与审批绑定发起人设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效（已评审并实施） |
| 创建日期 | 2026-10-28 |
| 实施记录 | 2026-10-28 用户确认推荐方案后实施；实现细节与当前行为以 `docs/current-state.md` 为准 |
| 来源 | `2026-10-28-harness-hardening-design.md` §9 遗留的两个次要问题 |
| 相关文档 | `2026-10-28-harness-hardening-design.md`、`2026-05-17-multi-turn-memory-design.md`、`im-channel-adapters.md` |

---

## 1. 背景

加固设计 §9 记录了审查中发现、当时未处理的三个次要问题。其中 `shell_start` 快照逐行重建属于纯性能问题，
已按需生成快照修复（不改变对外行为）。本文档处理另外两项：它们分别改变多轮会话的生命周期和审批的授权边界，
按项目规范先评审再实施。

## 2. 多轮会话空闲自动结束

### 2.1 现状

- 多轮会话在每轮结束后进入 `Waiting(User)`，直到用户发送 `/finish` 才结束。每个通道同一时刻最多一个
  这样的会话，所以会话数量的上限是对话方数量，本身不会无限增长。
- 终态任务回收（`terminal_task_retention_system`）按任务树整体判断：树上只要有一个未终态任务就整树保留。
  多轮会话的根任务一直未终态，它名下所有已完成的子任务、WorkItem 和消息历史都会一直留在 World 中。
- 结果：一个开了几周、反复让 Agent 分解任务的 IM 会话，内存占用随轮次线性增长，且永不释放。

### 2.2 推荐方案

- 新增配置 `HARNESS_CONVERSATION_IDLE_SECS`，默认 `86400`（24 小时），`0` 表示关闭。
- 在终态回收模块中新增 `conversation_idle_system`，复用相同的 30 秒扫描间隔：顶层任务
  （`parent_task_id` 为空）处于 `Waiting(User)` 或 `Waiting(AskUser)`、且 `now - updated_at`
  超过阈值时，生成 `FinishTaskMessage`。这与用户发送 `/finish` 走同一条结束路径，照常触发总结与评估。
- 向该任务的来源通道推送一条提示："会话已空闲超过 N 小时，已自动结束；发送新消息将开启新会话"，
  并记录 `ConversationIdleFinished` 事件。
- 会话结束后，整棵任务树进入终态，由现有终态回收在保留期过后清理，不另写回收逻辑。
- 所有通道（含 TUI）统一适用，不按通道区分。

### 2.3 影响

- 用户隔了超过阈值再回来时，上下文已经结束，需要重新开启会话。长期记忆（总结写回）不受影响。
- `Waiting(Approval)` 不在范围内：等待审批的任务由审批流程自身负责，不按空闲结束。

### 2.4 备选方案

- 只回收"未终态根任务下的已终态子树"，不结束会话。用户无感知，但消息历史仍无限增长，只解决一半问题；
  且子任务结果可能仍被父任务上下文引用，回收时机难以判断。

## 3. 审批绑定发起人

### 3.1 现状

- `ChannelId` 以聊天为粒度：群聊中 `user_id` 是群 ID（如 QQ 的 `group:<openid>`），不区分发言人。
- 通道层拿得到发言人（`ChannelInboundMessage.sender_id`，QQ 的 `group_member_openid`、Telegram 的
  `from.id`），但 `to_external_input` 转换时丢弃了它，领域层不知道"是谁发的"。
- 文本审批回复按通道匹配任务；按钮审批（`ExternalInput::Confirmation`）只按 `request_id` 匹配。
  两条路径都意味着：白名单内的任何群成员都能批准其他成员任务的 Confirm 工具。
- 运行期授权（`RuntimeGrants`）按 `(agent, tool, approval_channel)` 划分。成员 A 在群里选择
  "本会话允许"后，同群成员 B 的任务也会直接复用这份授权。

### 3.2 推荐方案

会话保持群级共享，审批绑定到发起人。群聊本来就是多人共用一个上下文，这一点不改；改的是"谁能放行
危险操作"。

- __传递发言人__：`ExternalInput::TextWithChannel` 与 `ExternalInput::Confirmation` 新增
  `sender: Option<String>`（平台用户 ID），由 `to_external_input` 填入；TUI、webhook、定时器为 `None`。
- __记录发起人__：由用户输入创建的顶层任务挂 `TaskInitiator` 组件，取创建它的那条输入的 `sender`；
  子任务不另存，需要时沿父链取根任务的发起人；webhook 事件任务和定时任务没有发起人。
- __审批判定__：通道配置新增 `approvers: Vec<String>`（`[qq]`、`[telegram]` 各一份，默认为空）。
  一条审批回复（文本或按钮）被接受，当且仅当满足以下任一条件：
  - 回复者等于任务的 `initiator`；
  - 回复者在该通道的 `approvers` 中；
  - 任务没有 `initiator`（事件任务、定时任务），且 `approvers` 为空：保持现有行为，白名单内任何人可审批。
- __拒绝反馈__：不满足时不改变任务状态，向通道回复"仅任务发起人或审批人可以审批此操作"，记录
  `ApprovalSenderRejected` 事件。
- __授权范围__：`RuntimeGrants` 的键扩展为 `(agent, tool, GrantScope)`，`GrantScope` 为
  `(origin_channel, initiator)`。A 的"本会话允许"只对 A 发起的任务生效。
- __私聊与 TUI 不受影响__：私聊中发言人就是对话方本身，`initiator` 总能匹配；TUI 没有 `sender`，
  所有任务 `initiator` 为 `None` 且没有 `approvers`，沿用现有行为。

### 3.3 不在本次范围

- `/finish`、`/stop`、`/cancel` 等控制命令仍按群级生效。它们影响的是共享会话的可用性，而不是授权；
  若要绑定发起人，需另行讨论群聊会话的"所有者"语义。
- 群成员之间的会话隔离（每个成员独立上下文）。这是产品形态变化，不在加固范围内。

### 3.4 影响

- 群里只有发起人（或配置的审批人）能放行自己任务的工具调用；其他成员点击按钮或回复数字会收到拒绝提示。
- 改动面：通道层（QQ、Telegram 的入向转换）、`ExternalInput` 两个变体与沿途消息的 `sender`、
  `TaskInitiator` 组件、审批校验（`approval_sender_gate_system`，文本与按钮两条路径汇合后统一校验）、
  `RuntimeGrants` 键、通道配置与文档。
- 已知限制：群聊会话上下文仍按群共享。成员 B 在群里的新消息会续上 A 发起、仍在等待用户的会话，
  这轮执行沿用 A 的发起人身份，因此会复用 A 已给出的"本会话允许"授权；新的审批仍只有 A 或审批人能放行。
  彻底消除需要群成员会话隔离（见 §3.3）。

### 3.5 实施说明

- 设计稿中"`Task` 新增 `initiator` 字段"改为 `TaskInitiator` 组件：只有根任务需要它，
  组件避免了波及全部 `Task` 构造点，语义相同。

### 3.6 备选方案

- 只记录审批者身份到日志，不做拦截。改动小，便于事后追溯，但不能阻止越权审批。
- 群聊中禁用 Confirm 工具（一律拒绝）。最安全，但群聊里的 Agent 基本失去 shell 能力。

## 4. 评审要点

| 节 | 需要确认的决策 |
|----|----------------|
| §2 | 新增 `HARNESS_CONVERSATION_IDLE_SECS`，默认 24 小时，空闲后按 `/finish` 路径自动结束 |
| §2 | `Waiting(AskUser)` 一并适用；所有通道（含 TUI）统一适用 |
| §3 | 审批绑定发起人，通道配置新增 `approvers` 作为代审名单 |
| §3 | 运行期授权按发起人进一步划分 |
| §3 | 控制命令（`/finish` 等）暂不绑定发起人 |
