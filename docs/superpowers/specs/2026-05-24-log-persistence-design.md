# 日志持久化到本地 — 设计文档

## 概述

为 AI Harness 添加日志持久化功能，将结构化日志以 JSON Lines 格式写入本地文件，兼顾开发调试与生产排查。

## 需求

- 日志同时输出到终端和本地文件
- 文件格式：JSON Lines（每行一个 JSON 对象）
- 文件级别固定 DEBUG，终端级别受 `RUST_LOG` 控制（默认 INFO）
- 按启动分割：每次启动创建新文件，文件名含时间戳
- 默认存放 `./logs/`，支持 `HARNESS_LOG_DIR` 环境变量覆盖
- 非阻塞写入，不影响主循环性能

## 架构

```
tracing events
     │
     ▼
  Subscriber (init_tracing)
   ┌────┴────┐
   ▼         ▼
Layer 1    Layer 2
终端 fmt   文件 fmt (JSON)
INFO       DEBUG
```

`init_tracing()` 创建两个 `fmt::Layer` 共用一个 `Subscriber`：

- **终端层**：纯文本，级别由 `RUST_LOG` 控制（默认 INFO），保持现有行为
- **文件层**：JSON 格式，级别固定 DEBUG，写入本地文件

## 文件路径与命名

- 默认目录：`./logs/`，可通过 `HARNESS_LOG_DIR` 环境变量覆盖
- 文件名格式：`harness_YYYY-MM-DD_HH-MM-SS.jsonl`（启动时间戳，秒级精度）
- 目录不存在时自动创建

## 文件格式

每行一个 JSON 对象（JSON Lines），示例：

```json
{"timestamp":"2026-05-24T14:30:01.234Z","level":"DEBUG","target":"harness::systems::dispatch","span":{},"fields":{"event":"TaskCreated","task_id":"...","content":"..."}}
```

## 日志级别

| 输出目标 | 默认级别 | 配置方式 |
|---------|---------|---------|
| 终端 | INFO | `RUST_LOG` 环境变量 |
| 文件 | DEBUG | 固定 DEBUG，不可配置 |

## 非阻塞写入

使用 `tracing_appender::non_blocking::NonBlocking` 包裹文件 appender，避免日志 IO 阻塞主循环。`WorkerGuard` 在 `main()` 中持有，进程退出时自动 flush。

## 代码变更

### `init_tracing()` 改造

```rust
fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let file_filter = EnvFilter::new("debug");

    let log_dir = std::env::var("HARNESS_LOG_DIR")
        .unwrap_or_else(|_| "logs".to_string());
    let file_appender = tracing_appender::rolling::never(
        &log_dir,
        format!("harness_{}.jsonl", chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")),
    );
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let subscriber = tracing_subscriber::registry()
        .with(env_filter.with_layer(fmt::Layer::new().without_time()))
        .with(file_filter.with_layer(
            fmt::Layer::new()
                .json()
                .with_writer(non_blocking)
                .with_ansi(false),
        ));
    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to set tracing subscriber");

    guard
}
```

返回 `WorkerGuard`，在 `main()` 中持有。

### `main()` 调整

```rust
let _log_guard = init_tracing();
```

## 依赖变更

`Cargo.toml` 新增：

```toml
tracing-appender = "0.2"
```

`tracing-appender` 是 `tracing` 生态官方 crate，MIT/Apache-2.0，纯 Rust 实现，满足项目依赖原则。

## 影响范围

- `src/main.rs`：`init_tracing()` 改造 + `main()` 调整
- `Cargo.toml`：新增 `tracing-appender` 依赖
- 无其他文件变更，不影响现有日志调用方式
