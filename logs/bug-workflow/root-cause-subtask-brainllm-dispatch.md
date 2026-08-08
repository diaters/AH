# 子任务系统性挂死根因分析

> 状态：当前有效
> 关联日志：`logs/harness_2026-08-08_14-59-16.jsonl`
> 关联任务：`5b897837-1d8c-4a7e-bab2-3a374ef918d7`（browser-operator）

## 现象

在 `harness_2026-08-08_14-59-16.jsonl` 中，browser-operator 先后两次尝试用 `create_tasks` 并行派发子任务采集小红书帖子：

- 第一批（`07:02`）：`b585e912`（scrape_xiaohongshu_posts）
- 第二批（`08:03`）：`c52a0e2f`、`6ab62034`、`c509c034`、`17aa153d`

5 条子任务全部进入派单流程，但**无一被成功分派/运行**，全部永久停在 `Waiting(Agent)`；父任务的 `wait_tasks` 每次都超时（`timed_out: true`）。LLM 据此误判为"环境不支持并行子任务"。

全局汇总事件证明：

- `BrainDecisionResolved`（大脑拍板成功）全文件仅 1 条，对象是**主任务** `5b897837`，没有任何子任务被成功拍板。
- `DispatchTaskDirectDelegated` / `DispatchTaskDirectDelegateSpawn`（真正分派/派生 agent）只有主任务的 2 条。
- `SubTaskDispatchPrepared`（进入派单流程）5 条，正好对应 5 个子任务。
- `WaitForTasksCompleted` 3 条，且全部 `timed_out: true`（行 `861`、`1905`、`1949`）。

## 核心结论

**根因是 `create_tasks` 工具创建子任务时，绕过了中心 `spawn_task` 封装，直接用 `commands.spawn`，导致子任务从未登记进 `EntityIndex.tasks`；而 `brain_decision_system` 通过 `EntityIndex.get_task` 反查实体来消费 brain 决策结果，子任务查无此任务，结果消息被静默 `despawn` 丢弃，子任务永久卡在 `Waiting(Agent)`。**

这不是偶发故障，是每一条经 `create_tasks` 产出的子任务都会触发的系统性缺陷。

## 证据链

### 1. 子任务的 brain LLM 调用其实是成功返回的

子任务的 brain 决策请求正常发出并收回（此前"调用没回来"的判断已更正）：

```849:849:logs/harness_2026-08-08_14-59-16.jsonl
{"event":"LlmRequestCompleted","task_id":"b585e912-...","model":"Kimi-K2.6","duration_ms":"8949","response_len":67}
```

`b585e912` 的 brain 决策在 `07:02:29` 正常返回（67 字符），第二批 4 个子任务也都在 `08:03:50–53` 返回。因此 LLM 层正常，响应确实回来了。

### 2. `brain_decision_system` 把子任务的结果静默丢弃

```65:71:src/systems/transform/brain_decision.rs
let Some((task_entity, mut task, awaiting)) = index
    .get_task(&result.task_id)
    .and_then(|e| tasks.get_mut(e).ok())
else {
    commands.entity(entity).despawn();   // 结果消息被 despawn
    continue;                             // 静默跳过，无事件、无报错
};
```

子任务的大脑决策结果消息进来后，`index.get_task(sub_task_id)` 返回 `None` → 走 `else` 分支，结果消息被销毁、任务继续挂着。这就是"从不解析、也不报 Failed、永久 `Waiting(Agent)`"的精确出处。

### 3. 为什么 `index.get_task` 对子任务返回 `None`

`EntityIndex.tasks` 只由中心封装 `spawn_task` 写入：

```42:54:src/ecs/entity_index.rs
pub fn spawn_task(commands, index, task, stm, marker, pending) -> Entity {
    let id = task.id;
    let entity = commands.spawn((task, stm, marker, pending)).id();
    index.tasks.insert(id, entity);   // ← 唯一登记入口
    entity
}
```

而 `create_tasks` 工具创建子任务时绕过该封装，直接 `commands.spawn`：

```118:118:src/systems/tools/orchestrator.rs
commands.spawn((child_task, sub_task_config, ShortTermMemory::default()));
// ↑ 没有 index.tasks.insert！子任务从未进入 EntityIndex
```

## 主任务 vs 子任务差异对照

| 维度 | 主任务 `5b897837` | 子任务（`b585e912` 等 5 条） |
|---|---|---|
| 创建入口 | 中心 `spawn_task` 封装 | `create_tasks` 里的 `commands.spawn`（直 spawn） |
| 是否登记进 `EntityIndex.tasks` | 是 | 否 |
| 派单前半段（`dispatch_preparation`、`dispatch_system`）能否看到 | 能（用 `Query` 遍历所有 Task 实体，不看 index） | 能（同上，故派单流程正常启动） |
| `brain_decision_system` 能否定位 | `index.get_task` 命中 → 解析成功 → `BrainDecisionResolved` | `index.get_task` 返回 `None` → 结果被 despawn、静默丢弃 |
| 最终结果 | 解析 → `DirectDelegate` → 交给 browser-operator → **运行** | 永远 `Waiting(Agent)`，**挂死** |

## 为什么是"系统性失败"且主任务从不失败

- **系统性**：根因是 `create_tasks` 漏调 `index.tasks.insert`。这不是偶发，而是每一条经 `create_tasks` 产出的子任务都缺失索引登记，所以 5/5 子任务 100% 卡死，两次重试（07:02 一批、08:03 一批）模式完全一致。
- **主任务从不失败**：主任务通过中心 `spawn_task` 创建，索引里有它，`brain_decision_system` 每次都能找到并解析，后续 `DirectDelegate` 命中持久 agent `browser-operator` 直接运行，根本不会进入"查不到"的分支。

## 叠加脆弱点（加重故障，非根因）

1. `AwaitingBrainDecision` 组件**没有看门狗/超时**，任务一旦查不到就永久悬挂，既不重试也不失败。
2. `wait_tasks` 超时只恢复父任务（`timed_out: true`），**不清理孤儿子任务**，子任务从 `07:02`/`08:03` 一直挂到会话结束（`13:26` 退出）。

> 说明：根因已修复，详见下方修复状态。

## 修复状态

- ✅ 根因已修复：`spawn_create_tasks_messages` 改用 `spawn_task` 中心封装（commit `a222692`）
- ✅ 防御性日志：`brain_decision_system` 在 else 分支添加 warn（commit `c2f7de1`）
- ✅ `ResMut<EntityIndex>` 传递：`tool_dispatch_system`、`tool_confirmation_system`、`approval_result_system` 均已更新
