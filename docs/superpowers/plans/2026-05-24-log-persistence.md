# 日志持久化到本地 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 AI Harness 添加 JSON Lines 格式的本地文件日志，与终端日志并行输出。

**Architecture:** 在 `init_tracing()` 中创建双层 `fmt::Layer`——终端层（纯文本，INFO，受 RUST_LOG 控制）和文件层（JSON，DEBUG，非阻塞写入）。文件按启动分割，文件名含时间戳。使用 `tracing-appender` 官方 crate。

**Tech Stack:** Rust, tracing, tracing-subscriber, tracing-appender, chrono

---

## File Structure

| Action | File | Responsibility |
|--------|------|---------------|
| Modify | `Cargo.toml` | 新增 `tracing-appender` 依赖 |
| Modify | `src/main.rs` | 改造 `init_tracing()` + `main()` 持有 guard |

---

### Task 1: 添加 tracing-appender 依赖

**Files:**
- Modify: `Cargo.toml:19-20`（tracing 相关依赖旁）

- [ ] **Step 1: 在 Cargo.toml 的 tracing 依赖组中添加 tracing-appender**

在 `tracing-subscriber` 行后添加：

```toml
tracing-appender = "0.2"
```

- [ ] **Step 2: 验证依赖可编译**

Run: `cargo check 2>&1 | tail -5`
Expected: 编译成功（无 tracing-appender 相关错误）

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add tracing-appender dependency for log file persistence"
```

---

### Task 2: 改造 init_tracing() 支持双层输出

**Files:**
- Modify: `src/main.rs:22-26`（`init_tracing` 函数）

- [ ] **Step 1: 重写 init_tracing 函数**

将 `src/main.rs` 中的 `init_tracing` 替换为：

```rust
fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let file_filter = EnvFilter::new("debug");

    let log_dir =
        std::env::var("HARNESS_LOG_DIR").unwrap_or_else(|_| "logs".to_string());
    let file_appender = tracing_appender::rolling::never(
        &log_dir,
        format!(
            "harness_{}.jsonl",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        ),
    );
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let subscriber = tracing_subscriber::registry()
        .with(
            fmt::Layer::new()
                .without_time()
                .with_filter(env_filter),
        )
        .with(
            fmt::Layer::new()
                .json()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(file_filter),
        );

    subscriber
        .init();

    guard
}
```

- [ ] **Step 2: 更新 main() 中的 init_tracing 调用**

将 `src/main.rs:138` 的：

```rust
init_tracing();
```

替换为：

```rust
let _log_guard = init_tracing();
```

- [ ] **Step 3: 更新 use 声明**

确认 `src/main.rs` 顶部的 use 声明仍然正确。当前的 `use tracing_subscriber::{EnvFilter, fmt};` 保持不变，新增的 `layer::SubscriberExt` 和 `util::SubscriberInitExt` 在函数体内导入（避免与现有 use 冲突）。

- [ ] **Step 4: 验证编译通过**

Run: `cargo check 2>&1 | tail -5`
Expected: 编译成功，无错误

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: add file logging with JSON Lines format and dual-layer output"
```

---

### Task 3: 运行 clippy 和格式检查

**Files:**
- 无文件变更

- [ ] **Step 1: 运行 cargo fmt**

Run: `cargo fmt --check`
Expected: 无格式问题（若有则运行 `cargo fmt` 后重新 commit）

- [ ] **Step 2: 运行 cargo clippy**

Run: `cargo clippy -- -D warnings 2>&1 | tail -20`
Expected: 无警告或错误

- [ ] **Step 3: 如有 fmt/clippy 问题则修复并 amend commit**

若 step 1 或 2 发现问题，修复后：

```bash
git add -u
git commit -m "fix: resolve clippy and fmt issues"
```

---

### Task 4: 手动集成验证

**Files:**
- 无文件变更

- [ ] **Step 1: 运行应用并验证日志文件生成**

Run: `cargo run 2>&1 &` ，然后在终端输入任意文本触发日志，等待几秒后 Ctrl+C 退出。

- [ ] **Step 2: 检查 logs/ 目录**

Run: `ls -la logs/`
Expected: 存在 `harness_YYYY-MM-DD_HH-MM-SS.jsonl` 文件

- [ ] **Step 3: 验证文件内容为有效 JSON Lines**

Run: `head -3 logs/harness_*.jsonl`
Expected: 每行为完整 JSON 对象，包含 `timestamp`、`level`、`fields` 等字段

- [ ] **Step 4: 验证终端输出不变**

确认终端输出仍为纯文本格式（非 JSON），且无时间戳（`.without_time()` 行为保留）。

- [ ] **Step 5: 验证环境变量覆盖**

Run: `HARNESS_LOG_DIR=/tmp/harness-test cargo run 2>&1 &` ，触发日志后退出。

Run: `ls /tmp/harness-test/`
Expected: 存在 `harness_*.jsonl` 文件

- [ ] **Step 6: 清理测试文件**

```bash
rm -rf /tmp/harness-test
```

---

### Task 5: 运行全量测试

**Files:**
- 无文件变更

- [ ] **Step 1: 运行 cargo test**

Run: `cargo test 2>&1 | tail -20`
Expected: 所有测试通过

- [ ] **Step 2: 清理测试产生的日志文件（如有）**

```bash
rm -rf logs/
```
