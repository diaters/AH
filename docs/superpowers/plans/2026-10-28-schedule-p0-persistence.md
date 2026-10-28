# P0 实施计划：定时任务持久化、补跑、重叠与修改

## 依据

- 设计：`docs/design/2026-10-28-unattended-scheduled-monitoring-design.md` §3、§8、§14 P0
- 前置：PR #108（同一时刻多个 cron 全部触发）已合入

## 范围

P0 只做定义合并、持久化、启动恢复、补跑、重叠规则与 `update_scheduled_task`。
运行账本、通知策略、探针、授权属于 P1–P3，本计划不涉及；`ScheduleHealth` 只先落 `last_fired_at`，
其余字段随 P1 引入。

## 任务

### T1：`ScheduleDefinition` 取代双账本

- `domain/schedule.rs` 新增 `ScheduleExpr { raw, spec }`（按 `raw` 序列化，反序列化时重新解析）、
  `ScheduleHealth { last_fired_at }`、`ScheduleDefinition { id, name, content, schedule, output_channel,
  enabled, created_at, health }`，`kind()` 派生为 `scheduled:<id>`。
- `SchedulerState.dynamic_tasks` 改为 `Vec<ScheduleDefinition>`；删除 `DynamicScheduledTask`、
  `ScheduledTaskInfo`、`ScheduledTaskRegistry` 及其快照类型，`update_scheduler_state*` 闭包只接收
  `&mut SchedulerState`。
- `OwnedToolContext.registry` 删除，`SchedulerStateSnapshot` 直接持有定义列表。
- `trigger_task_routing_system` 从 `SchedulerState` 查找定义。
- 验证：既有测试迁移后全部通过；`list_scheduled_tasks` 不再有双账本漂移分支。

### T2：`ScheduleStore` 持久化

- `infrastructure/schedules/store.rs`：`ScheduleStore::{new, default_path, load, save}`，
  路径 `.harness/schedules/schedules.json`，临时文件 + rename 原子写；损坏文件改名为 `.corrupt-<ts>` 后按空处理。
- `schedule_persist_system`（`Maintenance`）：`SchedulerState` 变化且存在 `ScheduleStore` Resource 时写盘，
  写失败记 `warn`。测试默认不插入 Store，不落盘。
- `main.rs` 启动时 `load` 并放入初始 `SchedulerState`。
- 测试先行：序列化往返（含 cron / once）、原子写、损坏文件恢复、change detection 触发写盘。

### T3：启动补跑

- 纯函数 `catch_up_decision(def, now, window) -> CatchUp::{None, Fire { missed_at }, Expired}`：
  cron 取 `last_fired_at`（缺省 `created_at`）之后、`now` 之前最近一次；once 取 `at`；超过 7 天为 `Expired`。
- `schedule_catch_up_startup_system`（`Startup`）：`Fire` 生成带 `ScheduleFire::CatchUp` 的
  `TriggerTaskMessage`，并向 `output_channel` 推送补跑说明；`Expired` 的 once 删除、cron 仅记日志。
- 补跑运行的任务内容前缀一行说明原定时间（P1 再迁入运行上下文块）。
- 测试先行：纯函数五种情况；startup 系统产出消息与删除过期 once。

### T4：触发记账与重叠规则

- 新增 `ScheduledRun { schedule_id }` Component，随 `CreateTaskMessage` 实体带入，由
  `user_message_to_task_system` 拷到 Task 上。
- `trigger_task_routing_system`：定义 `enabled == false` 跳过；存在未终态且 `ScheduledRun` 同 id 的 Task 时跳过
  （`ScheduleSkipped` 日志，reason=overlap）；否则创建任务并更新 `health.last_fired_at`。
- 测试先行：暂停跳过、重叠跳过、终态后可再次触发、`last_fired_at` 更新。

### T5：`update_scheduled_task` 与名称

- `schedule_task` 新增可选 `name`（缺省取 id 前 8 位），重名在 commit 时拒绝。
- 新工具 `update_scheduled_task`：`kind` 或 `name` 定位；可改 `content`、`schedule`、`enabled`；
  `run_now` 生成带 `ScheduleFire::Manual` 的触发。经 `ToolEffect::UpdateSchedule` 落账。默认权限 Allow
  （与 `schedule_task` 一致）。
- `delete_scheduled_task` 同时接受 `name`；`list_scheduled_tasks` 输出增加 `name`、`enabled`、`last_fired_at`。
- `timer_scheduler::build_all_schedules` 过滤 `enabled == false`。
- 测试先行：按 name / kind 更新、改调度后下一次触发时间变化、暂停后 timer 不再调度、`run_now`、重名拒绝。

### T6：文档与验证

- 更新 `docs/current-state.md`（去掉"仅存内存"、补充持久化 / 补跑 / 重叠 / 修改能力）、`README.md`、
  `docs/async-tool-bridge.md` 中的双账本描述、设计索引实施状态。
- `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo test --all-features`、`markdownlint`。
