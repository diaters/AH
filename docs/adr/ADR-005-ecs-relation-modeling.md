<!-- markdownlint-disable MD013 -->

# ADR-005: 实体关系改用 ECS 原生建模（EntityIndex + ChildOf）

## 状态

Accepted（v2 — 经 `improve-codebase-architecture` 技能 grilling 四轮决策确认（v1 为 Accepted）；
后因 `logs/2026-07-26-ecs-relation-modeling-review.md` 评审发现 "`SessionHandle` / `ExperienceInbox`
非 ECS 实体"等真实缺陷，回退为 Proposed，按__选项 B（剔除 Session / Inbox 范围、EntityIndex 降为两表）__
收窄范围重做（v2），并经 `logs/2026-07-26-ecs-relation-modeling-review-v2.md` 复核通过，于 2026-07-26 回到 Accepted。）

> v2 修订已处理评审 v2：必须修复 1 项（设计 §2.2 `spawn_task` 示例所有权编译错误）、建议修改 2 项
> （`waiting.rs` 路径改为 `src/systems/tools/waiting.rs`、级联处补悬空 `ChildOf` spike 提示）。

## 生效范围

本决策自 2026-07-26 提出，关联设计文档：

- `docs/design/2026-07-26-ecs-relation-modeling-design.md`（本决策的实施设计，v2 同步收窄）
- `docs/design/2026-06-06-workitem-boundary-design.md`（Task / WorkItem 边界）
- `docs/current-state.md`
- 评审记录：`logs/2026-07-26-ecs-relation-modeling-review.md`

## 背景

当前 Harness 在 Bevy ECS 中已把 `Task` / `WorkItem` / `Agent` 建模为 Component，但__实体之间的
"领域关系"仍用裸 UUID 表达__，这违背了 ECS 的组合与关系查询理念。注意：`SessionHandle` 与
`ExperienceInbox` 经评审核实__不是 ECS 实体__、也不属于本设计要改造的"领域关系"，见第 5 点边界澄清。

具体问题（仅限本设计范围 Task / Agent 关系）：

1. __关系以裸 UUID 嵌入 Component__：`parent_task_id: Option<TaskId>`（[task.rs:91](../../src/domain/task.rs#L91)）、
   `creator: AgentId`（[task.rs:74](../../src/domain/task.rs#L74)）、`delegate: Option<AgentId>`（[task.rs:75](../../src/domain/task.rs#L75)）、
   `bound_task_id: Option<TaskId>`（[agent.rs:85](../../src/domain/agent.rs#L85)）、
   `target_task_ids: Vec<TaskId>`（[task.rs:145](../../src/domain/task.rs#L145)）。
   这些 UUID 是外部稳定身份（IM 通道、provider 协议、存档按 UUID 寻址），但被同时当作"实体间关系"使用。

2. __无反向索引、无 ECS 关系__：代码库中没有任何 `TaskId → Entity` 索引 Resource
   （`ExperienceStore.inboxes` 内部虽有 `HashMap<TaskId, _>`，但那是经验收件箱、按 TaskId O(1) 索引，不属本设计问题），
   Bevy 原生关系（`ChildOf` / `Related`）对 Task / Agent 层级零命中。拿到一个 UUID，无从知道它对应哪个 ECS Entity。

3. __线性扫描（实际规模经全量扫描核实）__：因无法从 UUID 解析 Entity，系统在全量 `Query<&Task>` /
   `Query<&Agent>` 上做 `tasks.iter().find(|t| t.id == …)` / `agents.iter().find(|a| a.id == …)`。
   __全库核实共 50 处__（2026-07-27 阶段 2 启动前重新扫描）：

   - __36 处 UUID 寻址__（含 `iter().find` / `iter().any` / `iter_mut().find`，散布于 routing / experience /
     tools / waiting / transform/llm_response / subtask / brain_decision / summarization / maintenance /
     contracts/tools / frontend_output / brain_llm_builder）— 本设计改造对象
   - __4 处 UUID+条件复合__（闭包同时含 id 与 status 等字段，可拆为 UUID 解析 + 调用方断言两步）— 本设计改造对象
     （[llm_response.rs:1454](../../src/systems/transform/llm_response.rs#L1454)、
     [dispatch.rs:226](../../src/systems/tools/dispatch.rs#L226)、
     [waiting.rs:27-30](../../src/systems/tools/waiting.rs#L27-L30) / [53-56](../../src/systems/tools/waiting.rs#L53-L56)）
   - __5 处复合查询__（不通过 id 查找、按 status / origin_channel / kind / tags 等非 UUID 字段筛选）— __非本设计改造对象__：
     [routing.rs:36](../../src/systems/routing.rs#L36)、[command.rs:38/95/116](../../src/systems/command.rs#L38)
     （按 channel+status 找活跃任务）、[orchestrator.rs:762](../../src/systems/tools/orchestrator.rs#L762)
     （按 kind+tags 找 Persistent agent）。这些是真正的领域查询、ADR §3 设计意图不覆盖，
     后续是否引入次级索引（如 `ChannelActiveTaskIndex` / `PersistentAgentByTagIndex`）单独立项评审
   - __5 处 TUI 本地快照__（[tui/app.rs:665/891/1132/1167](../../src/tui/app.rs#L665)、
     [tui/status.rs:122](../../src/tui/status.rs#L122)）— 排除出验收

   原 v1 估算"约 13 处"严重低估（实际 40 处生产需改造）；原 v0 估算"55+"亦不准确。
   每次查找都是 O(n)。

4. __实体级状态被迫进全局 Resource（根因部分在本设计）__：`ExperienceStore` 是 `#[derive(Resource)]` 全局枢纽，
   其 `PendingExperienceHooks`（[contribution.rs:192-194](../../src/domain/contribution.rs#L192-L194)，注意该自承注释挂在
   `PendingExperienceHooks` 而非 `ExperienceStore` 主体）源于经验治理的 Resource 化——本设计仅消除其
   "因 UUID 关系无法关联回实体"的根因，不在此改造 `ExperienceStore` 内部结构；`inboxes` 已是 O(1) 字典，无需 ECS 化。

5. __悬空 UUID 风险__：`delegate` / `parent_task_id` 指向的实体若被 despawn，UUID 仍然残留，形成静默悬空引用。

### 边界澄清（评审修正，不在本设计范围）

- __`SessionHandle` 是 shell 子进程句柄，不是领域实体__：它无 `Component` derive，存于 `NativeBackend.sessions:
  Arc<Mutex<HashMap<SessionHandleId, SessionHandle>>>`（[native.rs:72](../../src/systems/tools/backend/native.rs#L72)），
  并配套 `processes` / `stdins` / `runtimes` 持有活的 OS 进程资源。`SessionHandle.owner_task_id`
  （[session.rs:50](../../src/domain/session.rs#L50)）是反向指针，由 `handle_id` 经 `NativeBackend` Map 直接索引。
  它属执行基础设施，强行 Entity 化会触碰进程生命周期，违反「简化优先」。故 __Session 不进 `EntityIndex`、不做 `ChildOf`__。
- __`ExperienceInbox` 已按 `TaskId` O(1) 索引__：`ExperienceStore.inboxes: HashMap<TaskId, ExperienceInbox>`
  （[contribution.rs:221](../../src/domain/contribution.rs#L221)）。本就不需要 index 或 `ChildOf`，原设计给它套 `ChildOf` 属过度设计。

## 决策

### 1. 关系表示（决策 1）

- __保留 `TaskId` / `AgentId` / `SessionHandleId` 作身份 Component__：UUID 是外部与持久化的稳定身份，不能删除。
- __新增中心 `EntityIndex` Resource__：内部__两__张类型化表 `HashMap<TaskId, Entity>` /
  `HashMap<AgentId, Entity>`，专门解决"手里只有 UUID、还不知 Entity"的场合（如 IM 通道消息只带 TaskId）。
  __不设 `sessions` 表__——Session 是 shell 进程句柄，由 `NativeBackend` 自管。
- __层级关系改用 Bevy `ChildOf`__：仅用于 `parent_task → child`（子任务确为 `Task` Component，是合法 ECS 实体），
  让 `Query<&Task, With<ChildOf<ParentEntity>>>` 这类组合查询替代全量扫描。

备选（被否决）：

- (B) 仅在 Component 里加 `Entity` 字段、不引入 index：凡外部消息只带 UUID 的场合仍无法定位 Entity，只解决一半。
- (C) 用 `Entity` 整体替换 UUID：破坏外部协议与持久化，且 Session / Inbox 本就不宜进 ECS，更不可取。

### 2. 关系划分（决策 2）

| 关联 | 归类 | 理由 |
|---|---|---|
| `parent_task → child` | `ChildOf` | 树形层级；子任务是异步执行单元，父 despawn __不__级联杀子（见边界兜底机制） |
| `delegate`(Agent) / `creator`(Agent) / `bound_task`(Agent) | 只存 UUID，经 `index.agents` 解析 | 非归属引用 |
| `waiting target_tasks` | `Vec<TaskId>`（经 `index.tasks` 解析） | 等待非归属、不级联；`target_task_ids` 本就是 `Vec<TaskId>`（[task.rs:145](../../src/domain/task.rs#L145)），不再改 `Vec<Entity>` |
| `batch_id` | 保留 `Uuid` 分组键，不做实体 | 临时分组，永不级联销毁 |
| `task → session` | __维持现状（不进 ECS）__ | `SessionHandle` 是 shell 进程句柄，存 `NativeBackend` Map，由 `owner_task_id` 反向指；基础设施，非领域关系 |
| `task → experience_inbox` | __维持现状（不进 ECS）__ | `ExperienceStore.inboxes` 已按 `TaskId` O(1) 索引，无需 index / `ChildOf` |

`ChildOf` 关系下，`parent_task_id` UUID 字段可以删掉——父实体自身携带 `TaskId` 身份，要 UUID 时查父实体即得。
其余 UUID 关系（delegate / creator / bound_task / target_task_ids / batch_id / session 反向指针 / inbox）保留。

### 3. 非归属引用的存储与 index 范围（决策 3）

- __(a) 只存 UUID，运行期经 index 解析，不缓存 `Entity` 句柄__：`Entity` 句柄在 despawn 后即作废，
  (b) 的"额外缓存 `delegate_entity: Entity`"必然面临 Agent 重生后的陈旧失效；index 是 O(1)，
  每次解析成本可忽略。统一经 index 解析把"UUID↔Entity 的唯一映射"集中在 index 一处（locality）。
- __(b) 备选（否决）__：额外存 `Entity` 字段缓存热路径，引入第二真相源，Agent 重生后悬空。
- __(c) 备选（否决）__：用 `Entity` 替换 UUID 字段，破坏外部序列化。
- __index 范围__：仅 Task / Agent 两类实体的 `id → Entity` 映射
  （通道消息、provider、工具结果只按这两种 id 寻址；Session 由 `NativeBackend` 自管）。

### 4. 迁移策略与边界兜底（决策 4）

__分阶段、保持 `main` 可编译可测__：

- __阶段 0（纯新增）__：引入 `EntityIndex` Resource + __两表__ + `RemovedComponents<Task/Agent>` 兜底监听。不改任何现有调用。
- __阶段 1（内聚写入）__：收口 spawn。实际收口点远少于原估的 "90+"：
  - `Task` 实体创建统一走 `user_message_to_task_system` 的 `commands.spawn((task, stm, …))`（[task_creation.rs:88](../../src/systems/transform/task_creation.rs#L88)）；
  - `Agent` 仅 2 处（[maintenance.rs:226](../../src/systems/maintenance.rs#L226) / [372](../../src/systems/maintenance.rs#L372)）。
  在 `spawn_task` / `spawn_agent` 封装内统一写 index，外部调用签名不变，__不需逐个改几十处__；不需要 `spawn_session` 封装（session 不进 ECS）。
- __阶段 2（逐个替换查找）__：按模块分批把 __40 处__生产 UUID 寻址点改为 `index.get` 或 `ChildOf` 关系查询
  （36 处纯 UUID 寻址 + 4 处 UUID+条件复合，后者拆为 UUID 解析 + 调用方断言两步）。__5 处复合查询明确移出本设计范围__
  （见第 3 点）。批次切分（保持 ADR §3 原始 6 批次口径）：

  1. __routing__（1 处）：[routing.rs:138](../../src/systems/routing.rs#L138) — `continue_task_system` 按 `msg.task_id` 查 task
     （注：原列 [routing.rs:36](../../src/systems/routing.rs#L36) 经核实为 channel+status 复合查询，移出范围）✅ 已落地（PR-1，2026-07-27）
  2. __experience__（6 处）：[collection.rs:19/63/226/234](../../src/systems/experience/collection.rs#L19) +
     [profile_update.rs:64](../../src/systems/experience/profile_update.rs#L64) + [governance.rs:27](../../src/systems/experience/governance.rs#L27)
     （原列 [command.rs:38/95/116](../../src/systems/command.rs#L38) 全部为复合查询，移出范围；command 批次语义由 experience 接续）✅ 已落地（PR-2，2026-07-27）
  3. __tools/dispatch+orchestrator+approval+async_dispatch__（9 处，含 1 处 UUID+条件复合）：
     [dispatch.rs:85/198/226/255](../../src/systems/tools/dispatch.rs#L85) +
     [orchestrator.rs:27/267/1352](../../src/systems/tools/orchestrator.rs#L27) +
     [approval.rs:65](../../src/systems/tools/approval.rs#L65) + [async_dispatch.rs:101](../../src/systems/tools/async_dispatch.rs#L101)
     （注：[orchestrator.rs:762](../../src/systems/tools/orchestrator.rs#L762) 经核实为 kind+tags 复合查询，移出范围）✅ 已落地（PR-3，2026-07-27）
  4. __waiting__（2 处，UUID+条件复合，需轻度重构）：[waiting.rs:27-30](../../src/systems/tools/waiting.rs#L27-L30) / [53-56](../../src/systems/tools/waiting.rs#L53-L56)
     ✅ 已落地（PR-4，2026-07-27）
  5. __transform 系列__（14 处，含 1 处 UUID+条件复合）：
     [chat_round.rs:23/54](../../src/systems/transform/chat_round.rs#L23) +
     [task_lifecycle.rs:239/277](../../src/systems/transform/task_lifecycle.rs#L239) +
     [llm_response.rs:475/568/601/636/1454/1548/1624](../../src/systems/transform/llm_response.rs#L475) +
     [subtask.rs:26/114](../../src/systems/transform/subtask.rs#L26) + [brain_decision.rs:63](../../src/systems/transform/brain_decision.rs#L63)
     ✅ 已落地（PR-5，2026-07-27）
  6. __散点__（10 处，含 ADR v2 漏列 4 处：`summarization.rs` 1 处 + `maintenance.rs` 3 处 + `frontend_output.rs` 2 处
     中的 1 处 + `brain_llm_builder.rs` BrainLlm 分支 1 处）：
     [contracts/tools.rs:76/78](../../src/contracts/tools.rs#L76) +
     [frontend_output.rs:169](../../src/systems/frontend_output.rs#L169) + [maintenance.rs:309/407/480](../../src/systems/maintenance.rs#L309) +
     [brain_llm_builder.rs:35](../../src/systems/dispatch/brain_llm_builder.rs#L35) + [summarization.rs:30](../../src/systems/summarization.rs#L30)
     ✅ 已落地（PR-6，2026-07-27）

  每批带测试、独立 PR。__5 处 TUI 本地快照查找排除出验收__（[tui/app.rs:665/891/1132/1167](../../src/tui/app.rs#L665)、[tui/status.rs:122](../../src/tui/status.rs#L122)）。
- __阶段 3（关系 ECS 化）__：引入 `ChildOf` 替换 `parent_task_id` 的 UUID 字段并删除该字段；
  `waiting` 维持 `Vec<TaskId>`（[task.rs:145](../../src/domain/task.rs#L145)，本就是，无 `Vec<Entity>` 改动）；`batch_id` 保留。

__边界兜底（grilling 自我质疑结论 + 评审修正）__：

- __index 陈旧双保险__：所有 despawn 强制经中心 `despawn_task` / `despawn_agent` 封装
  （封装内同步清 index）；额外挂 `RemovedComponents<Task/Agent>` 监听，即使有路径漏掉封装，组件移除时自动摘映射。
  _注意_：`RemovedComponents` 在组件移除的__下一帧__触发，与同帧 despawn 封装形成"即时 + 延迟"双保险（评审 2.3）。
- __级联语义（评审 2.2 修正）__：Bevy `ChildOf` 父 despawn __默认级联杀子__。子任务__不__级联——
  父 task despawn 用__非递归__ `commands.entity(parent).despawn()`（不调用 `DespawnRecursive`），
  子任务作为独立 Entity 靠 `ChildOf` 关系被查询端独立存活。Session 不在 ECS，无级联问题。
  _悬空 `ChildOf` 验证（评审 v2 §4.1 / 阶段 3 spike）_：非递归 despawn 父后，父的 `RelationshipTarget`
  随父消失，子 entity 上的 `ChildOf(parent)` 可能残留指向已 despawn 父的悬空引用。阶段 3 实施前需做 spike 确认
  Bevy 0.18 是否自动清理；若需显式解绑，`despawn_task` 封装内先 `Query<&Task, With<ChildOf<Parent>>>` 遍历子任务
  `remove::<ChildOf>()` 再 despawn 父。当前判断：子任务不级联、运行期不从不存父 entity 反向查子，悬空 `ChildOf` 为良性，但须 spike 确认。
- __持久化重建__：存档按 UUID 序列化，重启后 Entity 是新值；加载期经 `spawn_*` 封装 spawn 时顺带写 index，自然重建
  （spawn 封装已含写 index）。该机制成熟、非阻塞；原开放问题 1 降级为"核对加载链路是否全部经封装"的验证项（见第 6 点）。

## 后果

### 正面

- __leverage__：约 40 处 O(n) 线性查找 + 散落 UUID 收敛为 1 个查询接口（index 或关系查询）。
- __locality__：层级关系逻辑集中，不再重写过滤；index 维护点集中在 spawn / despawn 封装。
- __引用完整性__：父 despawn 经封装清 index + `RemovedComponents` 兜底，关系不再悬空；子任务不级联、独立存活。
- __测试更直接__：断言对象从"手搓的 find 函数"变为"关系是否建立、index 是否命中"，更贴近真实运行时。
- __范围诚实__：Session / Inbox 维持既有高效结构，避免为 ECS 化而 ECS 化的过度设计（符合「简化优先」）。

### 负面

- 新增 `EntityIndex` Resource 及两表维护成本（集中在 spawn / despawn）。
- 阶段 1 收口 spawn 点（实际很少：Task 1 处元组形式 + Agent 2 处）。
- 阶段 2 涉及约 40 处生产调用点改造，需分批、带测试、独立 PR 控制风险。
- `ChildOf` 非级联需显式机制（非递归 despawn），增加少量正确性约束。

## 关联文件

- [src/domain/task.rs](../../src/domain/task.rs) — `parent_task_id` / `batch_id` / `delegate` / `creator` / `target_task_ids`
- [src/domain/agent.rs](../../src/domain/agent.rs) — `bound_task_id`
- [src/domain/session.rs](../../src/domain/session.rs) — `SessionHandle` 为 shell 进程句柄，__不进本设计__
- [src/domain/contribution.rs](../../src/domain/contribution.rs) — `ExperienceStore.inboxes` 已按 `TaskId` O(1) 索引，__不进本设计__
- [src/systems/tools/backend/native.rs](../../src/systems/tools/backend/native.rs) — `NativeBackend` 持有 `SessionHandle` Map（基础设施边界）
- [src/systems/routing.rs](../../src/systems/routing.rs) — 线性查找示例
- [src/systems/tools/waiting.rs](../../src/systems/tools/waiting.rs) — `wait_tasks` 终态判定
