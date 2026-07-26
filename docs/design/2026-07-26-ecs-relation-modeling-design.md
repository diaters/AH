<!-- markdownlint-disable MD013 -->

# ECS 关系建模改造设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效（v2 — 按 ADR-005 选项 B 收窄范围重做；ADR 当前为 Proposed，待用户再次评审确认） |
| 创建日期 | 2026-07-26 |
| 适用阶段 | ECS 关系建模改造（根因级，喂养经验治理 Resource 化问题） |
| 相关文档 | `docs/adr/ADR-005-ecs-relation-modeling.md`、`docs/design/2026-06-06-workitem-boundary-design.md`、`docs/current-state.md` |

---

## 1. 背景

### 1.1 现状

Harness 已用 Bevy ECS 把 `Task` / `WorkItem` / `Agent` 建模为 Component，但实体之间的"领域关系"
仍以裸 UUID 表达，且代码库内无任何 `TaskId → Entity` 索引、对 Task / Agent 层级无任何 Bevy 原生关系
（`ChildOf` 零命中）。系统只能对全量 `Query<&Task>` 做 `tasks.iter().find(|t| t.id == …)` 线性扫描，
全库共 __16 处__（生产约 13 处，TUI 本地快照 3 处应排除出验收）。

### 1.2 已识别的 ECS 理念违背（仅限本设计范围）

| 违反点 | 位置 | 说明 |
|---|---|---|
| 关系用裸 UUID 替代 ECS 关系 | [task.rs:91](../../src/domain/task.rs#L91) 等 | `parent_task_id` / `delegate` / `creator` 等以 UUID 嵌在 Component |
| 全量线性扫描 | [routing.rs:36](../../src/systems/routing.rs#L36) 等约 13 处生产代码 | O(n) 查找，无法组合查询（原估 "55+" 严重高估） |
| 实体级状态进全局 Resource | [contribution.rs:192-194](../../src/domain/contribution.rs#L192-L194)（`PendingExperienceHooks` 自承，非 `ExperienceStore` 主体） | 经验治理 Resource 化衍生补丁；本设计仅消除其"UUID 无法关联回实体"的根因 |
| 悬空 UUID | 上述所有 UUID 字段 | 指向实体 despawn 后 UUID 残留 |

> __范围边界（评审修正）__：`SessionHandle`（[native.rs:72](../../src/systems/tools/backend/native.rs#L72) 持有，
> shell 进程句柄）与 `ExperienceInbox`（`ExperienceStore.inboxes: HashMap<TaskId, _>`，[contribution.rs:221](../../src/domain/contribution.rs#L221)，
> 已按 TaskId O(1) 索引）__不是 ECS 实体、也不属本设计改造对象__。原设计误将其纳入 `ChildOf` 属过度设计。

### 1.3 目标

- 引入中心 `EntityIndex`（`TaskId` / `AgentId` → `Entity` __两表__），消灭约 13 处生产线性扫描
- 层级关系（仅 `parent_task → child`）改用 Bevy `ChildOf`，使组合查询替代手搓过滤
- 非归属引用保留 UUID、运行期经 index O(1) 解析
- 消除悬空 UUID，索引维护点集中到 spawn / despawn 封装
- 为 `ExperienceStore` 实体级状态回归 Component 扫清障碍（删除 `PendingExperienceHooks` 补丁的根因在"UUID 无法关联回实体"，本设计消除该根因）

### 1.4 非目标

- 不删除外部 UUID 身份（IM 通道 / provider / 存档仍按 UUID 寻址）
- 不引入 Bevy `Related` 多对多关系（当前无该需求）
- 不在此设计中顺带治理 `Task` 的 God-object 字段拆分（独立任务，见后续规划）
- 不在本设计删除 `ExperienceStore`（仅消除其"被迫承载实体级状态"的根因，具体回归在后续候选 3 设计）
- __不__把 `SessionHandle` / `ExperienceInbox` 纳入 ECS 关系建模（基础设施 / 既有 O(1) 字典，维持现状）

---

## 2. 方案

### 2.1 `EntityIndex` Resource

```rust
/// 中心索引：外部 UUID 稳定身份 → ECS Entity 的唯一映射（仅 Task / Agent 两类）
#[derive(Resource, Default)]
pub struct EntityIndex {
    pub tasks:  HashMap<TaskId, Entity>,
    pub agents: HashMap<AgentId, Entity>,
}
```

类型化两表比"统一 `EntityId` 枚举"更安全，避免把 `TaskId` 误当 `AgentId` 查。不设 `sessions` 表。

### 2.2 中心 spawn / despawn 封装（维护点集中）

```rust
/// 阶段 1：所有 task spawn 经此，封装内写 index
pub fn spawn_task(commands: &mut Commands, index: &mut EntityIndex, task: Task) -> Entity {
    let id = task.id; // Task 非 Copy，spawn 会 move，先取出 id（评审 v2 §3.3）
    let entity = commands.spawn(task).id();
    index.tasks.insert(id, entity);
    entity
}

/// 所有 task despawn 经此，封装内清 index（双保险之一）
pub fn despawn_task(commands: &mut Commands, index: &mut EntityIndex, id: TaskId) {
    if let Some(entity) = index.tasks.remove(&id) {
        commands.entity(entity).despawn(); // 非递归：子任务不级联（评审 2.2）
    }
}
```

`spawn_agent` / `despawn_agent` 同理，写入 / 移除 `agents` 表。__不需要 `spawn_session` /
`despawn_session`__（Session 不进 ECS）。

### 2.3 关系划分

| 关联 | 归类 | 运行期访问 |
|---|---|---|
| `parent_task → child` | `ChildOf` | `Query<&Task, With<ChildOf<Parent>>>` |
| `delegate` / `creator` / `bound_task` | 只存 UUID，经 index 解析 | `index.agents.get(&id)` |
| `waiting target_tasks` | `Vec<TaskId>`（经 `index.tasks` 解析） | `target_task_ids` 本就是 `Vec<TaskId>`（[task.rs:145](../../src/domain/task.rs#L145)） |
| `batch_id` | 保留 `Uuid` 分组键，不做实体 | 仅作过滤分组 |
| `task → session` | 维持现状（不进 ECS） | `SessionHandle` 由 `NativeBackend` Map + `owner_task_id` 索引 |
| `task → experience_inbox` | 维持现状（不进 ECS） | `ExperienceStore.inboxes` 按 `TaskId` O(1) 索引 |

### 2.4 边界兜底

- __`RemovedComponents` 兜底监听__（双保险之二）：即便有路径漏掉中心 despawn 封装，
  组件移除时自动摘除映射。__注意该监听在组件移除的下一帧触发__（评审 2.3），与同帧 despawn 封装形成
  "即时 + 延迟"双保险：

  ```rust
  fn cleanup_index_on_task_remove(
      mut index: ResMut<EntityIndex>,
      removed: RemovedComponents<Task>,
  ) {
      for entity in removed.read() {
          index.tasks.retain(|_, &mut e| e != entity);
      }
  }
  ```

- __级联语义（评审 2.2 修正）__：Bevy `ChildOf` 父 despawn 默认级联杀子。本设计__不__接受该默认——
  `parent_task → child` 用非递归 `commands.entity(parent).despawn()`（不调用 `DespawnRecursive`），
  子任务作为独立 Entity 靠 `ChildOf` 关系被查询端独立存活。`task → session` 不在 ECS，无级联问题。
  _悬空 `ChildOf` 验证（评审 v2 §4.1 / 阶段 3 spike）_：非递归 despawn 父后，父的 `RelationshipTarget`
  随父消失，子 entity 上的 `ChildOf(parent)` 可能残留悬空引用。阶段 3 实施前需 spike 确认 Bevy 0.18 是否自动清理；
  若需显式解绑，`despawn_task` 封装内先遍历子任务 `remove::<ChildOf>()` 再 despawn 父。当前判断该悬空为良性，但须 spike 确认。
- __持久化重建__：加载期经 `spawn_*` 封装重建 index（封装已含写 index，自然覆盖）。该机制成熟、非阻塞。

---

## 3. 迁移策略（4 阶段，保持 `main` 可编译可测）

### 阶段 0 — 纯新增（零行为变化）✅ 已落地（2026-07-26）

- 新增 `EntityIndex` Resource 与两表（`src/ecs/entity_index.rs`）
- 新增 `RemovedComponents<Task/Agent>` 兜底监听 system（`cleanup_index_on_task_remove` / `cleanup_index_on_agent_remove`）
- 在 `src/app/mod.rs` 的 `build_runtime` 中注册 `init_resource::<EntityIndex>()` 与两个 system（置于 `HarnessSet::Maintenance`）；不改任何现有调用
- 含 2 个单元测试（见 §5）：`entity_index_resolves_and_cleanup_removes_stale_mapping`、`cleanup_keeps_unrelated_mapping`，均通过；`cargo clippy --lib --all-features` 零警告

### 阶段 1 — 内聚写入 ✅ 已落地（2026-07-26）

- 新增 `spawn_task` / `spawn_agent` 中心封装（封装内写 index，双保险之一）与 `despawn_task` /
  `despawn_agent`（即时清 index，双保险之二），位于 `src/ecs/entity_index.rs`
- 实际收口点：`Task` 走 `user_message_to_task_system` → `spawn_task` 封装
  （[task_creation.rs](../../src/systems/transform/task_creation.rs) 接入 `ResMut<EntityIndex>`）；
  `Agent` 仅 2 处：`load_agents_system` → `load_persistent_agents` →
  `spawn_persistent_agent_from_entry` → `spawn_agent`（[maintenance.rs:226](../../src/systems/maintenance.rs#L226)）；
  `agent_factory_system` → `handle_spawn_request` → `spawn_agent`
  （[maintenance.rs:372](../../src/systems/maintenance.rs#L372)）。
  `index` 参数沿 system → helper 向下传递（`&mut EntityIndex`，system 入口为 `ResMut<EntityIndex>`）；
  内部 helper 无测试直接调用，签名改动安全。
- 目标达成：`EntityIndex` 在运行期始终保持与实体一致；`despawn_*` 封装作为双保险之二，待真实
  Task/Agent despawn 路径出现时收口（见阶段 3 任务终止）。
- `cargo clippy --lib --all-features` 零警告；`cargo test --lib` 668 passed，无回归。

### 阶段 2 — 逐个替换查找（分批、独立 PR）

按模块顺序改造（约 13 处生产代码，TUI 3 处排除）：

1. `routing`
2. `command`
3. `experience/collection`
4. `tools/dispatch` + `tools/orchestrator`
5. `waiting`
6. `chat_round` + `task_lifecycle`

每处 `tasks.iter().find(|t| t.id == …)` 改为：

```rust
// UUID 入口：经 index O(1) 解析
let entity = index.tasks.get(&target_id).copied()?;
let task = tasks.get(entity)?;

// 或层级场景：关系查询替代扫描
fn collect_children(children: Query<&Task, With<ChildOf<ParentEntity>>>) { /* 直接子节点 */ }
```

### 阶段 3 — 关系 ECS 化

- 引入 `ChildOf` 替换 `parent_task_id` 的 UUID 字段并删除该字段
- `waiting` 维持 `target_task_ids: Vec<TaskId>`（[task.rs:145](../../src/domain/task.rs#L145)，本就是，无 `Vec<Entity>` 改动）
- `batch_id` 保留为 `Uuid` 分组键，不在本阶段改动
- __前置 spike__：阶段 3 实施前先验证 Bevy 0.18 非递归 `despawn` 父后子 entity 上 `ChildOf` 悬空行为（见 §2.4），确认是否需要显式解绑再写 `despawn_task` 封装

---

## 4. 验收标准

| 项 | 验收口径 |
|---|---|
| 扫描消除 | 全代码库中生产代码的 `tasks.iter().find(\|t\| t.id == …)` 线性查找降至 0（TUI 3 处本地快照查找除外；仅 `EntityIndex` 内部保留 `HashMap` 访问） |
| 索引一致性 | 所有按 UUID 寻址的入口（IM 消息 / provider 结果 / 工具回调）经 `EntityIndex` 解析；运行期 index 命中率 100%，无 `None` 路径被静默忽略 |
| 级联语义 | `parent_task → child` 父 despawn __不__级联子任务（非递归 despawn，子任务独立存活）；Session 不在 ECS，无级联要求 |
| 兜底生效 | 即使绕过中心 despawn 封装直接 `commands.entity(e).despawn()`，`RemovedComponents` 监听仍能摘除陈旧映射（单测覆盖；注意延迟一帧） |
| 关系查询 | `wait_tasks` 等层级判定改为 `ChildOf` 组合查询，无全量遍历 |
| 补丁清理前提 | `PendingExperienceHooks` 补丁 Resource 在后续经验治理回归设计中可安全删除（本设计消除其根因） |

---

## 5. 测试清单

### 单元测试

1. `EntityIndex` 写入：spawn 后 `tasks.get(id)` 返回正确 Entity
2. `EntityIndex` 移除：despawn 后 `tasks.get(id)` 返回 `None`
3. `RemovedComponents` 兜底：直接 despawn 实体后，监听（下一帧）自动摘除映射
4. `spawn_*` 封装：外部调用签名不变，index 同步更新
5. `ChildOf` 查询：`collect_children` 仅返回目标父的子节点，无全量扫描
6. 级联差异化：despawn 父任务 → 子任务__保留__（非递归 despawn 生效）
7. 非归属引用解析：`delegate` / `creator` 经 `index.agents.get` 正确解析为 Entity
8. `Vec<TaskId>` 等待：despawn 目标后 `wait_tasks` 经 index 解析不悬空

### 集成测试

1. IM 通道消息按 `TaskId` 经 index 定位实体并路由，无线性扫描
2. `wait_tasks` 等全部子任务完成：用 `ChildOf` + 关系查询一步到位
3. 持久化加载后 index 从 spawn 重建，旧 UUID 正确映射新 Entity
4. 绕过封装直接 despawn，index 仍由监听恢复一致（兜底端到端）

---

## 6. 开放问题（留给实施阶段细化）

1. 核对加载链路是否全部经 `spawn_*` 封装（spawn 时已写 index，重建机制成熟、非阻塞，原"重建时机"不确定性已消解）
2. `ChildOf` 在 Bevy 0.18 已稳定，具体 API（`add_child` vs `ChildOf` Component 构造）以实施时为准，非阻塞
3. spawn 收口是否引入 `EntityIndex` 作为 `SystemParam` 封装以减少签名膨胀（参考 ADR-004 D10 的 `SystemParam` 思路）
4. `ExperienceStore` 实体级状态回归 Component 的具体设计（候选 3，本设计为前置根因改造；`inboxes` 维持 `HashMap<TaskId, _>` 不在本设计改动）
