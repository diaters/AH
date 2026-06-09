# 配置说明

本文档说明 Harness 当前通过环境变量加载的运行配置，覆盖 LLM provider、
运行时参数、shell 运行参数与本地开发建议。

## 配置入口

当前配置入口位于：

- `src/main.rs`
- `src/app/mod.rs`
- `src/llm/provider.rs`

加载顺序如下：

1. 主程序启动并读取 `.env.local`
2. `HarnessConfig::from_env()` 解析运行时参数
3. `LlmProviderConfig::from_env()` 解析 provider、模型和连接信息

## LLM 配置

### 核心变量

| 变量名 | 默认值 | 说明 |
|--------|--------|------|
| `HARNESS_LLM_PROVIDER` | `openai` | provider 类型 |
| `HARNESS_MODEL` | `gpt-4.1-mini` | 模型名称 |
| `HARNESS_LLM_API_KEY` | 无 | OpenAI 兼容 provider 的显式 API Key |
| `HARNESS_LLM_API_BASE` | 无 | OpenAI 兼容 provider 的基础地址 |

### provider 取值

当前支持以下 provider：

| 取值 | 含义 |
|------|------|
| `openai` | 标准 OpenAI provider |
| `anthropic` | Anthropic provider |
| `claude` | `anthropic` 的别名 |
| `deepseek` | DeepSeek provider |
| `openai-compatible` | OpenAI 兼容协议 provider |
| `openai_compatible` | `openai-compatible` 的别名 |
| `compatible` | `openai-compatible` 的别名 |

### 校验规则

- `HARNESS_MODEL` 不可为空
- 当 `HARNESS_LLM_PROVIDER=openai-compatible` 时：
  - `HARNESS_LLM_API_KEY` 必填
  - `HARNESS_LLM_API_BASE` 必填
- 标准 provider 由 `genai` 使用默认接入方式处理

### 回退变量

当前仅对以下变量提供回退读取：

| 主变量 | 回退变量 |
|--------|----------|
| `HARNESS_LLM_API_KEY` | `OPENAI_API_KEY` |
| `HARNESS_LLM_API_BASE` | `OPENAI_BASE_URL` |

说明：

- 当前代码未消费 `HARNESS_LLM_ORG_ID`、`HARNESS_LLM_PROJECT_ID`
- 如未来重新启用相关能力，应在代码与文档中一起恢复

## 运行时配置

### 通用运行参数

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_BRAIN_ENABLED` | `false` | 是否启用 Brain Agent 调度 |
| `HARNESS_MAX_RETRIES` | `3` | LLM 请求最大重试次数 |
| `HARNESS_MAX_TOOL_ITERATIONS` | `5` | 单轮工具调用最大迭代次数 |
| `HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS` | `300` | `wait_tasks` 默认超时时间 |
| `HARNESS_AGENTS_CONFIG` | `agents.toml` | Agent 配置文件路径 |
| `HARNESS_LOG_DIR` | `logs` | JSONL 日志输出目录 |

### Shell Runtime

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_SHELL_DEFAULT_TAIL_LINES` | `200` | shell 默认返回的最新输出行数 |
| `HARNESS_SHELL_MAX_TAIL_LINES` | `500` | 单次允许返回的最大输出行数 |
| `HARNESS_SHELL_DEFAULT_EXEC_TIMEOUT_SECS` | `300` | `shell_exec` 默认超时时间 |
| `HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS` | `10` | `shell_stop` 内部停止等待的默认超时时间 |
| `HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM` | `65536` | 每个 stdout 或 stderr stream 的最大缓存字节数 |

## 配置示例

### OpenAI 兼容 provider

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://example.com/v1
```

### 启用 Brain

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://api.deepseek.com/v1
export HARNESS_BRAIN_ENABLED=true
```

### 自定义 Agent 配置和日志目录

```bash
export HARNESS_AGENTS_CONFIG=agents.toml
export HARNESS_LOG_DIR=logs
```

## 已废弃的旧配置语义

以下旧语义已经不再作为当前对外能力存在：

- `shell.wait` 相关配置
- `HARNESS_SHELL_DEFAULT_WAIT_TIMEOUT_SECS`
- 对外暴露的 `shell_status`、`shell_read_output`、`shell_send_signal`

说明：

- 当前 shell 工具集已经收敛为六个意图化工具
- 若文档、注释或计划文稿中仍出现旧字段，应视为历史内容，而不是当前契约

## 本地开发建议

- 复制 `.env.example` 生成 `.env.local`
- 不要提交真实 API Key
- 对外共享示例配置时，只更新 `.env.example`
- 修改配置项时，必须同步更新本文档和 `.env.example`
