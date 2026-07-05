> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 降低空闲 CPU 占用实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 TUI 主循环在系统空闲时降低轮询频率并跳过无意义重绘，从而减少 CPU 占用。

**Architecture:** 在 `HarnessConfig` 中新增 `active_poll_ms` 与 `idle_poll_ms` 配置；
在 `src/main.rs` 的主循环末尾根据 `app_is_idle()`、`had_input`、`had_engine_events`
动态选择休眠时长，并决定是否调用 `terminal.draw()`。

**Tech Stack:** Rust, Bevy ECS, crossterm, ratatui, crossbeam-channel

## Global Constraints

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS
- 配置从环境变量读取，保持与现有 `HarnessConfig::from_env` 模式一致
- 默认 `active_poll_ms = 16`，`idle_poll_ms = 150`
- 不引入新线程、新通道或新调度层
- 所有代码变更需通过 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`

---

### Task 1: 在 HarnessConfig 中新增轮询配置项

**Files:**
- Modify: `src/app/mod.rs:22-88`（`HarnessConfig` 结构体及 `from_env` / `Default` 实现）
- Test: `src/app/mod.rs` 底部 `#[cfg(test)]` 模块（新建或扩展）

**Interfaces:**
- Consumes: 无
- Produces: `HarnessConfig` 新增字段 `pub active_poll_ms: u64` 和 `pub idle_poll_ms: u64`

- [ ] **Step 1: 编写失败测试**

在 `src/app/mod.rs` 的测试模块中添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_for_poll_intervals() {
        let config = HarnessConfig::default();
        assert_eq!(config.active_poll_ms, 16);
        assert_eq!(config.idle_poll_ms, 150);
    }
}
```

运行：

```bash
cargo test --lib app::tests::config_defaults_for_poll_intervals -- --nocapture
```

Expected: FAIL - 字段 `active_poll_ms` / `idle_poll_ms` 不存在

- [ ] **Step 2: 在 HarnessConfig 中添加字段并解析环境变量**

修改 `src/app/mod.rs` 中的 `HarnessConfig`：

```rust
pub struct HarnessConfig {
    // ... 现有字段保持不变 ...
    /// TUI 主循环在活跃状态下的轮询间隔（毫秒）
    pub active_poll_ms: u64,
    /// TUI 主循环在空闲状态下的轮询间隔（毫秒）
    pub idle_poll_ms: u64,
}
```

在 `from_env` 中追加解析：

```rust
active_poll_ms: std::env::var("HARNESS_ACTIVE_POLL_MS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(16),
idle_poll_ms: std::env::var("HARNESS_IDLE_POLL_MS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(150),
```

在 `Default` 实现中追加：

```rust
active_poll_ms: 16,
idle_poll_ms: 150,
```

- [ ] **Step 3: 运行测试**

```bash
cargo test --lib app::tests::config_defaults_for_poll_intervals -- --nocapture
```

Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add src/app/mod.rs
git commit -m "feat: add active/idle poll interval config"
```

---

### Task 2: 在主循环中应用自适应休眠与条件重绘

**Files:**
- Modify: `src/main.rs:120-260`（主循环体）

**Interfaces:**
- Consumes: `HarnessConfig.active_poll_ms`、`HarnessConfig.idle_poll_ms`、`app_is_idle()`
- Produces: 无新增公开接口

- [ ] **Step 1: 统计 EngineEvent 数量**

在 `src/main.rs` 中，把：

```rust
while let Ok(ev) = event_rx.try_recv() {
    debug!(...);
    app_state.handle_engine_event(ev);
}
```

改为：

```rust
let mut engine_event_count = 0u32;
while let Ok(ev) = event_rx.try_recv() {
    debug!(...);
    app_state.handle_engine_event(ev);
    engine_event_count += 1;
}
```

- [ ] **Step 2: 统计鼠标事件数量**

在主循环的 `Event::Mouse(mouse)` 分支中增加计数：

```rust
Event::Mouse(mouse) => {
    app_state.handle_mouse_event(mouse);
    mouse_count += 1;
}
```

并在循环开头新增：

```rust
let mut mouse_count = 0u32;
```

- [ ] **Step 3: 计算是否空闲、是否重绘、休眠时长**

在调用 `app.update()` 之后、渲染之前，插入：

```rust
let idle = app_is_idle(app.world_mut());
let had_input = key_count > 0 || paste_count > 0 || mouse_count > 0;
let had_engine_events = engine_event_count > 0;
let settings = app.world().resource::<HarnessSettings>();

let sleep_ms = if idle && !had_input && !had_engine_events {
    settings.0.idle_poll_ms
} else {
    settings.0.active_poll_ms
};

let should_render = !idle || had_input || had_engine_events;
```

- [ ] **Step 4: 条件重绘**

把原来的：

```rust
if let Err(e) = terminal.draw(|frame| app_state.render(frame)) {
    warn!(...);
}
```

改为：

```rust
if should_render {
    if let Err(e) = terminal.draw(|frame| app_state.render(frame)) {
        warn!(
            event = "RenderError",
            error = %e,
            "TUI render failed"
        );
    }
}
```

- [ ] **Step 5: 使用动态休眠**

把原来的：

```rust
thread::sleep(Duration::from_millis(16));
```

改为：

```rust
thread::sleep(Duration::from_millis(sleep_ms));
```

- [ ] **Step 6: 编译与静态检查**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: 无错误、无警告

- [ ] **Step 7: 运行测试套件**

```bash
cargo test --all-features
```

Expected: 全部通过

- [ ] **Step 8: 提交**

```bash
git add src/main.rs
git commit -m "feat: adaptive poll interval and conditional render in idle loop"
```

---

### Task 3: 手动验证 CPU 降低效果

**Files:**
- 无需修改文件

**Interfaces:**
- Consumes: 已构建的二进制

- [ ] **Step 1: 构建 release 或 debug 二进制**

```bash
cargo build --release
```

- [ ] **Step 2: 在空闲状态下观察 CPU**

启动 TUI，不输入任何内容，等待 5–10 秒：

```bash
./target/release/harness
```

在另一个终端用 `top` / `htop` / Activity Monitor 观察进程 CPU 占用。

Expected: 空闲 CPU 显著低于修改前（修改前约等于一核的 5–15%，修改后应接近 0–2%）。

- [ ] **Step 3: 验证交互响应**

在 TUI 中按任意键、滚动鼠标、等待 EngineEvent 触发（如发送一条消息），
确认界面在 150ms 内更新。

Expected: 无明显迟钝。

- [ ] **Step 4: 验证环境变量生效**

```bash
HARNESS_IDLE_POLL_MS=500 ./target/release/harness
```

Expected: 空闲 CPU 进一步降低，但交互延迟上限变为 500ms。

---

## Self-Review

**1. Spec coverage:**

- 自适应休眠时长：Task 2 Step 3、Step 5 ✅
- 空闲时跳过无意义重绘：Task 2 Step 4 ✅
- 新增配置项：Task 1 ✅
- 默认值 16ms / 150ms：Task 1 Step 2 ✅
- 测试计划：Task 1 单元测试 + Task 3 手动验证 ✅

**2. Placeholder scan:**

- 无 TBD / TODO
- 无"稍后实现"类描述
- 代码块完整
- 命令与期望输出明确

**3. Type consistency：**

- `HarnessConfig` 中字段类型为 `u64`，与 `Duration::from_millis` 入参一致
- `HarnessSettings` 是 `Resource` 包装 `HarnessConfig`，通过 `settings.0.xxx` 访问
