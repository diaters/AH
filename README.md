# Harness

Harness 是一个基于 Rust + Bevy ECS 的 AI Harness 框架原型，当前聚焦于单轮对话 MVP、任务流转主链路，以及后续 Brain Agent / 多 Agent 扩展所需的基础结构。

## 当前状态

- 已完成 MVP 主链路：输入 -> Signal -> Task -> Dispatch -> Execution -> Output
- 已完成模块化拆分：`app`、`domain`、`systems`、`llm`
- 已支持基于配置的 LLM provider 抽象
- 当前 provider 支持：`openai`、`openai-compatible`

更多背景可参考：

- 设计文档：[2026-05-10-core-flow-design.md](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/docs/design/2026-05-10-core-flow-design.md)
- 配置说明：[configuration.md](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/docs/configuration.md)
- 项目规范：[CLAUDE.md](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/CLAUDE.md)

## 目录结构

```text
src/
├── app/       # 应用装配、资源与运行配置
├── domain/    # 核心实体、状态与执行器 trait
├── llm/       # provider 配置、执行器工厂、OpenAI 接入
├── systems/   # ECS systems，按阶段拆分
├── lib.rs
└── main.rs
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

当前测试分为两类：

- 单元测试：位于 `src/llm/provider.rs`，覆盖 provider 解析与配置校验
- 集成测试：位于 [mvp_flow.rs](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/tests/mvp_flow.rs)，验证单轮对话闭环

## 下一步方向

- BrainDispatchSystem / BrainDecisionSystem
- AgentFactorySystem 的真实能力匹配
- Memory / Tool / Session / Planner 等高级能力
- 多轮上下文管理与真实 provider 联调
