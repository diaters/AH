# Harness

Harness 是一个基于 Rust + Bevy ECS 的 AI Harness 框架，聚焦于任务驱动执行、
多 Agent 协作、工具调用、记忆治理与评估闭环，并提供基于 TUI 的本地交互入口。

## 能力状态

### 已实现

- TUI 驱动的本地交互与事件循环
- `Task` 作为用户目标载体，`WorkItem` 作为内部执行单元
- Brain 调度、Agent 配置加载与多 Agent 执行链路
- `create_tasks` + `wait_tasks` 驱动的任务分解能力
- Summarization 与 Evaluation 收敛到 `WorkItem` 闭环
- 工具权限、审批 UI、结果回写与工具执行链路
- 精简后的 shell 工具集：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- 结构化日志、CI、单元测试与集成测试

### 待完善

- 父 Agent 审批仍为 MVP 自动通过实现，尚未接入真实 LLM 审查
- 部分历史设计文档仍需持续整理状态标注
- 更多真实场景下的 provider 兼容性与复杂任务策略验证

### 已收敛

- `Plan` 已收敛为任务分解能力，不再作为独立运行时模块存在
- `Evaluation` 保留独立语义层，但执行链路统一走 `WorkItem`
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役

## 核心架构

```text
用户输入
  -> Frontend / Signal
  -> Task
  -> Dispatch / Brain
  -> Agent Execution
  -> Tool Calls / WorkItems
  -> Response / Writeback
  -> TUI Output
```

当前代码主目录：

```text
src/
├── app/         # 应用配置、资源与装配
├── contracts/   # 契约与 trait 定义
├── domain/      # 核心实体、消息、状态模型
├── llm/         # provider 配置与执行器实现
├── plugins/     # 模块装配
├── systems/     # ECS systems
│   ├── dispatch/
│   ├── tools/
│   └── transform/
├── tui/         # TUI 前端
├── lib.rs
└── main.rs
```

## 快速开始

### 1. 准备环境变量

复制示例配置：

```bash
cp .env.example .env.local
```

根据所用 provider 补充对应变量。

### 2. OpenAI 兼容接口示例

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://example.com/v1
cargo run
```

### 3. 标准 provider 示例

支持的 provider 值：

- `openai`
- `anthropic`
- `deepseek`
- `openai-compatible`

其中 `openai-compatible` 需要显式提供 `HARNESS_LLM_API_KEY` 和
`HARNESS_LLM_API_BASE`。标准 provider 使用 `genai` 默认接入方式。

## 常用命令

### 启动程序

```bash
cargo run
```

### 编译检查

```bash
cargo check
```

### 运行测试

```bash
cargo test --all-features
```

### 运行格式与静态检查

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

## 关键配置

常用环境变量包括：

- `HARNESS_LLM_PROVIDER`
- `HARNESS_MODEL`
- `HARNESS_LLM_API_KEY`
- `HARNESS_LLM_API_BASE`
- `HARNESS_BRAIN_ENABLED`
- `HARNESS_MAX_RETRIES`
- `HARNESS_MAX_TOOL_ITERATIONS`
- `HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS`
- `HARNESS_AGENTS_CONFIG`
- `HARNESS_LOG_DIR`

Shell 相关运行参数见 `docs/configuration.md`。

## 重要文档

- 项目规范：[`AGENTS.md`](AGENTS.md)
- 当前状态：[`docs/current-state.md`](docs/current-state.md)
- 配置说明：[`docs/configuration.md`](docs/configuration.md)
- 待办与风险：[`docs/TODO.md`](docs/TODO.md)
- 设计文档索引：[`docs/design/README.md`](docs/design/README.md)
- WorkItem 边界设计：
  [`docs/design/2026-06-06-workitem-boundary-design.md`](docs/design/2026-06-06-workitem-boundary-design.md)
- Plan / Evaluation 重评估：
  [`docs/design/2026-06-06-plan-evaluation-reassessment-design.md`](docs/design/2026-06-06-plan-evaluation-reassessment-design.md)
- Shell 工具精简设计：
  [`docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`](docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md)
