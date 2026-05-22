# Harness

Harness 是一个基于 Rust + Bevy ECS 的 AI Harness 框架原型，当前聚焦于单轮对话 MVP、任务流转主链路，以及后续 Brain Agent / 多 Agent 扩展所需的基础结构。

## 当前状态

- 已完成 MVP 主链路：输入 -> Signal -> Task -> Dispatch -> Execution -> Output
- 已完成模块化拆分：`app`、`domain`、`systems`、`llm`
- 已支持基于配置的 LLM provider 抽象
- 当前 provider 支持：`openai`、`openai-compatible`
- __Phase 4.3 已完成__：LLM 生成记忆摘要（替代简单拼接）
  - 支持三种触发条件：Token 阈值、`/summarize` 指令、任务完成
  - 新增 Summarizer Agent，支持经验积累与演化
- __测试体系完善__：88 个测试覆盖主链路、错误处理、边界条件、记忆摘要

更多背景可参考：

- 设计文档：[2026-05-10-core-flow-design.md](docs/design/2026-05-10-core-flow-design.md)
- 摘要设计：[2026-05-20-llm-summarization-design.md](docs/design/2026-05-20-llm-summarization-design.md)
- 配置说明：[configuration.md](docs/configuration.md)
- 项目规范：[CLAUDE.md](CLAUDE.md)

## 目录结构

```text
src/
├── app/              # 应用装配、资源与运行配置
├── domain/           # 核心实体、状态与执行器 trait
│   ├── mod.rs        # Task、Agent、Message 类型定义
│   └── error.rs      # ExecutionError 错误类型
├── llm/              # provider 配置、执行器工厂、OpenAI 接入
│   ├── provider.rs   # LLM provider 配置
│   ├── executor.rs   # Agent 执行器实现
│   └── summarization_prompt.rs  # 摘要生成 prompt
├── systems/          # ECS systems，按阶段拆分
│   ├── dispatch.rs   # 任务分派、Agent 选择
│   ├── transform.rs  # 结果处理、状态流转
│   ├── memory.rs     # 记忆压缩管理
│   ├── summarization.rs  # 摘要请求/结果处理
│   ├── command.rs    # 用户指令解析
│   └── execution.rs  # 异步执行管理
├── lib.rs
└── main.rs

agents.toml         # Agent 配置（含 summarizer）
```

## 快速开始

### 1. 准备环境变量

复制示例配置：

```bash
cp .env.example .env.local
```

然后按你的 provider 填写环境变量。

### 2. OpenAI 示例

```bash
export HARNESS_LLM_PROVIDER=openai
export HARNESS_MODEL=gpt-4.1-mini
export HARNESS_LLM_API_KEY=sk-xxxx
cargo run
```

### 3. OpenAI 兼容接口示例

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://example.com/v1
cargo run
```

## 常用命令

### 编译检查

```bash
cargo check
```

### 运行测试

```bash
cargo test
```

### 启动程序

```bash
cargo run
```

## LLM 配置

支持以下核心环境变量：

- `HARNESS_LLM_PROVIDER`
- `HARNESS_MODEL`
- `HARNESS_LLM_API_KEY`
- `HARNESS_LLM_API_BASE`
- `HARNESS_LLM_ORG_ID`
- `HARNESS_LLM_PROJECT_ID`

详细规则、回退变量和约束见 [configuration.md](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/docs/configuration.md)。

## 测试说明

当前测试共 __88 个__，分为三类：

| 测试文件 | 数量 | 覆盖范围 |
|---------|------|---------|
| `tests/mvp_flow.rs` | 6 | 核心主链路：任务创建、Agent 执行、结果输出、多轮对话 |
| `tests/error_handling_flow.rs` | 7 | 错误处理与边界条件：重试机制、非重试错误、空输入、大输入(100KB)、并发任务(5个)、等待状态、失败消息 |
| `tests/summarization_flow.rs` | 4 | LLM 摘要功能：任务完成触发、多轮不触发、终态保持、端到端流程 |
| `src/llm/provider.rs` (单元测试) | 71 | Provider 配置解析、环境变量回退、验证逻辑 |

运行全部测试：

```bash
cargo test --all-features
```

## 下一步方向

- [x] LLM 记忆摘要（Phase 4.3 已完成）
- [ ] BrainDispatchSystem / BrainDecisionSystem 完善
- [ ] AgentFactorySystem 的真实能力匹配
- [ ] Tool / Session / Planner 等高级能力
- [ ] 真实 OpenAI provider 联调验证
- [ ] 配置文件热加载
- [ ] 分布式/多实例支持
