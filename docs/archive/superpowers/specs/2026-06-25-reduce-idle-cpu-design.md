> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 降低空闲 CPU 占用设计

> **状态：当前有效**

## 背景

TUI 主循环当前每轮都以 `event::poll(Duration::ZERO)` 非阻塞读取终端事件，
然后无条件执行 `app.update()` 驱动全部 ECS 系统、无条件重绘 TUI，
最后固定 `thread::sleep(Duration::from_millis(16))`。

这导致即使系统完全空闲、无用户输入、无 EngineEvent，程序仍以约 60 FPS
的频率跑完整套 ECS Update + 渲染，CPU 占用显著高于必要水平。

## 设计目标

- 空闲时显著降低 CPU 占用
- 保持交互延迟在 100–200ms 可接受范围内
- 改动集中、可配置、可快速回退
- 不引入新的线程、通道或调度层

## 非目标

- 不改为完全事件驱动架构（避免合并 crossterm 与 crossbeam 两路事件源）
- 不在 ECS 层大规模加 `run_if` 条件（避免验证各系统对周期性 tick 的依赖）
- 不处理任务执行期间的 CPU 优化（本次只聚焦空闲状态）

## 总体思路

在主循环末尾，根据“系统是否空闲”和“本轮是否产生了新事件”动态决定：

1. 本轮休眠多久
2. 本轮是否值得重绘 TUI

关键判断依据：

- `app_is_idle(world)`：是否没有活跃任务、信号、待处理输入、执行请求等
- `had_input`：本轮是否处理了键盘 / 鼠标 / 粘贴事件
- `had_engine_events`：本轮是否从 `event_rx` 消费了任何 `EngineEvent`

## 具体设计

### 1. 自适应休眠时长

```rust
let idle = app_is_idle(app.world_mut());
let had_input = key_count > 0 || paste_count > 0 || mouse_count > 0;
let had_engine_events = engine_event_count > 0;

let sleep_ms = if idle && !had_input && !had_engine_events {
    settings.idle_poll_ms
} else {
    settings.active_poll_ms
};

thread::sleep(Duration::from_millis(sleep_ms));
```

默认值：

- `active_poll_ms`：16（保持现有 ~60 FPS 流畅度）
- `idle_poll_ms`：150（空闲时约 6–7 FPS，延迟上限 150ms）

### 2. 空闲时跳过无意义重绘

TUI 状态在完全空闲且无新事件时不会变化，因此可以跳过 `terminal.draw`：

```rust
let should_render = !idle || had_input || had_engine_events;
if should_render {
    terminal.draw(|frame| app_state.render(frame))?;
}
```

注意：crossterm resize 事件属于 `Event`，会触发 `had_input` 为真，因此窗口
大小变化时仍会正常重绘。

### 3. 新增配置项

在 `HarnessConfig` 中增加：

- `HARNESS_ACTIVE_POLL_MS`：活跃/默认轮询间隔，默认 16
- `HARNESS_IDLE_POLL_MS`：空闲轮询间隔，默认 150

配置从环境变量读取，允许在不同机器上微调。

## 改动范围

- `src/main.rs`：主循环中计算 `sleep_ms` 和 `should_render`
- `src/app/mod.rs`：在 `HarnessConfig` 中新增两个字段及环境变量解析

## 测试计划

- 启动 TUI 后不输入任何内容，用系统工具观察 CPU 占用是否下降
- 按键盘、鼠标滚轮、触发 EngineEvent 时，界面仍应在 150ms 内响应
- 运行一个实际任务，确认任务执行期间 `active_poll_ms` 生效、无卡顿
- 调整环境变量，确认配置读取生效

## 风险与回退

- 风险 1：某些后台系统（如记忆衰退、retry 唤醒）依赖周期性 tick，拉长空闲
  间隔后可能延迟。缓解：`app_is_idle` 已把这些待处理状态纳入判断，只要存在
  未完成任务或待处理消息，就不会进入长休眠。
- 风险 2：跳过重绘可能导致某些视觉状态不及时更新。缓解：重绘条件包含所有
  输入和 EngineEvent；如发现问题，可单独把“需要重绘”标记暴露给 ECS Output
  阶段。
- 回退：把 `HARNESS_IDLE_POLL_MS` 设为 16 即恢复现有行为。

## 后续可能演进

- 若 150ms 仍不够低，可引入 `event::poll(timeout)` 替代 `poll(Duration::ZERO) +
  sleep`，在空闲时真正阻塞等待事件
- 若 ECS 系统进一步增多，可考虑把“维护类 tick”拆分为低频 FixedUpdate，
  进一步减少空闲时 `Update` 的工作量
