# 系统管线与按需驱动

> __状态：当前有效__。依据：`src/app/`、`src/plugins/`、`src/systems/demand.rs`。
> 设计和验证范围见 [ECS 按需唤醒](2026-10-29-ecs-wakeup-design.md)。

## 一次更新的阶段

`build_harness_app` 将七个 `HarnessSet` 按以下顺序串联：

| 阶段 | 主要职责 |
| --- | --- |
| Ingress | 更新 Clock，读取前端动作与外部输入 |
| Signal | 重试到期转换、输入信号与定时触发路由 |
| Transform | LLM / 工具结果落地、任务状态转换、收件箱与等待完成 |
| Dispatch | Task / WorkItem 派发、工具权限与审批 |
| Execution | 提交异步请求、经验治理与写回 |
| Output | 前端事件、实际发送回执记账、定时定义持久化 |
| Maintenance | Agent 工厂、效果提交、取消、失联兜底、保留期回收 |

具体系统在阶段内用 `.before/.after` 建立依赖，保留 Bevy 的 deferred commands 提交。
阶段结束后 `Last` 根据有限业务组件的变化和移除登记续跑，并投影最近处理期限。
Clock 更新时间和普通统计变化不会触发续跑。

## 主循环什么时候执行

- 启动至少执行一轮。
- 外部输入与前端动作直接等待其 crossbeam 数据队列，取出的数据先放进 `BufferedIngress`，
  继续由原 ingress 系统转换为领域消息。
- LLM、模型链、异步工具与发送回执经 `WakeSender` 成功入队后发出通知。
  `RuntimeWake` 容量为一，只合并通知，不合并或丢弃业务数据。
- 存在内部续跑或到期工作时执行下一轮，其他时间阻塞等待。
- 连续推进每批最多八轮或约四毫秒后让出线程；系统本身不被中途抢占。
  主要结果接收入口每轮最多取 256 条，剩余数据继续唤醒，外部输入队列也限批处理。
- 只有请求关闭时才计算优雅退出资格，不通过全量 Task 扫描决定休眠资格。

不能在确认无工作之后清空通知，否则可能丢失“检查与阻塞之间”到达的工作。
发送端的底层业务 sender 不对外暴露，调用方必须通过 `WakeSender::send`。

## 时间与等待

`DemandSchedule` 保存调度投影，`RuntimeDeadlines` 使用可替换、可撤销的有序索引。
Task、WorkItem 和 SchedulerState 继续承担领域真相。

- 重试、等待子任务和工具失联截止时间投影为 `Instant`。
  到期时重新校验实体与原期限，等待子任务额外校验 tool call 身份。
- 新等待登记时检查目标是否已经完成，之后通过 child → waiters 反向索引处理终态变化，
  完成与超时收敛到一个系统，避免两条完成路径竞争。
- 工具取消通过 `EntityIndex` 查 owner；claim 只摘除在飞标记，结果仍由 ingest 单点落地。
- 存储失败按 1、2、4、8、16、32 秒间隔重试，保持原有去重和真实回执语义。
- 维护期限为 30 秒，长期记忆治理自身最小间隔一分钟。
- 收件箱合并、自动唤醒额度和输出窗口继续使用其既有 Clock 时间策略，
  截止时间索引负责唤醒；改变这些窗口在墙钟跳变下的业务语义不属于本次调整。
- cron 与一次性任务继续由现有 Tokio timer 调度，原始触发通过输入队列唤醒 ECS。

## TUI 与兼容适配器

终端事件由独立线程读取并唤醒主线程；TUI 状态更新和绘制仍在主线程。
输入框编辑、滚动、resize 与终态展示清理只需要 UI 工作。
有可见变化时绘制最大约每 16 ms 一次，无变化时跳过重绘。
终态任务展示的五秒清理通过 UI deadline 驱动。

当前保留两种显式兼容检查：

- supervisor IPC 只有请求/响应接口，存在 shell 后台任务时按 `background_poll_ms` 检查，默认 500 ms。
  子任务批次完成通过状态变化处理，不单独维持该轮询。
- 未提供动作队列且声明 `requires_polling()` 的旧式 Frontend 每 150 ms 检查。
  内置 TUI 和 IM 前端均不需要该兼容周期。

`HARNESS_ACTIVE_POLL_MS` 和 `HARNESS_IDLE_POLL_MS` 已废弃，兼容读取但不生效。

## 记忆时间语义

长期记忆的 30 天扣分速率为：

```text
rate = 0.5 - importance_discount - min(reuse_count × 0.02, 0.2)
importance_discount = Low: 0 / Medium: 0.05 / High: 0.1 / Critical: 0.2
score = clamp(previous_score + rounding_remainder - rate × elapsed_seconds / 2592000, 0, 1)
```

每次保存 `decay_updated_at` 和舍入余量；重复执行同一时刻不再改变分数。
旧 JSON 缺少检查点时，以当前分数和第一次治理时间为起点，不补扣历史。
访问之前的时间不计入新访问之后的衰退；回拨不把检查点退回过去。
`pin` 和 `Critical` 继续免驱逐；驱逐阈值仍是分数小于 0.1。
