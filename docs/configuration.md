# 配置说明

本文档说明 Harness 当前的运行配置，覆盖 LLM provider 和 Brain Agent 相关环境变量。

## 配置来源

当前主程序通过环境变量加载运行配置，入口位于 `src/main.rs` 和 `src/app/mod.rs`。

配置加载顺序如下：

- 主程序调用 `HarnessConfig::from_env()`
- `HarnessConfig` 内部委托 `LlmProviderConfig::from_env()`
- `LlmProviderConfig` 负责解析 provider、模型与鉴权信息

## 环境变量

### 通用变量

| 变量名                  | 必填     | 说明                           |
|-------------------------|----------|--------------------------------|
| `HARNESS_LLM_PROVIDER`  | 否       | provider 类型，默认 `openai`   |
| `HARNESS_MODEL`         | 否       | 模型名称，默认 `gpt-4.1-mini`  |
| `HARNESS_LLM_API_KEY`   | 条件必填 | 首选 API Key                   |
| `HARNESS_LLM_API_BASE`  | 条件必填 | OpenAI 兼容接口的基础地址      |
| `HARNESS_LLM_ORG_ID`    | 否       | 可选组织 ID                    |
| `HARNESS_LLM_PROJECT_ID`| 否       | 可选项目 ID                    |

### Brain Agent 变量

| 变量名                    | 必填 | 默认值                | 说明                        |
|---------------------------|------|-----------------------|-----------------------------|
| `HARNESS_BRAIN_ENABLED`  | 否   | `false`              | 是否启用 Brain Agent 调度     |
| `HARNESS_MAX_RETRIES`     | 否   | `3`                   | LLM 请求最大重试次数           |
| `HARNESS_MAX_TOOL_ITERATIONS` | 否 | `5`                 | 工具调用循环最大迭代次数       |
| `HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS` | 否 | `300` | wait_tasks 工具默认超时时间（秒） |

Brain 启用后，系统会在启动时创建 Brain Agent 实体，用户输入会先经过 Brain 决策再分派给具体 Agent 执行。Brain 不启用时行为与 MVP 完全一致。

### 兼容回退变量

为了兼容 OpenAI 默认环境变量命名，当前也支持以下回退读取：

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_ORG_ID`
- `OPENAI_PROJECT_ID`

优先级规则：

- API Key：优先读 `HARNESS_LLM_API_KEY`，否则回退 `OPENAI_API_KEY`
- API Base：优先读 `HARNESS_LLM_API_BASE`，否则回退 `OPENAI_BASE_URL`
- Org ID：优先读 `HARNESS_LLM_ORG_ID`，否则回退 `OPENAI_ORG_ID`
- Project ID：优先读 `HARNESS_LLM_PROJECT_ID`，否则回退 `OPENAI_PROJECT_ID`

## Provider 取值

当前支持以下 provider：

| 取值               | 含义                        |
|--------------------|-----------------------------|
| `openai`           | 标准 OpenAI 接口            |
| `openai-compatible`| OpenAI 兼容协议接口         |
| `compatible`       | `openai-compatible` 的别名  |

## 配置约束

当前代码会执行以下校验，见 `src/llm/mod.rs`：

- `HARNESS_MODEL` 不能为空
- API Key 不能为空
- 当 `HARNESS_LLM_PROVIDER=openai-compatible` 时，必须提供 `HARNESS_LLM_API_BASE`

## 配置示例

### OpenAI

```bash
export HARNESS_LLM_PROVIDER=openai
export HARNESS_MODEL=gpt-4.1-mini
export HARNESS_LLM_API_KEY=sk-xxxx
```

### OpenAI Compatible

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://example.com/v1
```

### Brain Agent 启用

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-v4-flash
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://api.deepseek.com/v1
export HARNESS_BRAIN_ENABLED=true
```

## 本地开发建议

- 复制 `.env.example` 生成本地私有环境文件
- 不要提交真实 API Key 到仓库
- 共享示例配置时，仅更新 `.env.example`
- 若接入新的兼容 provider，优先保持 OpenAI 协议兼容，避免过早为单家厂商定制分支逻辑

## Shell Runtime

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_SHELL_DEFAULT_TAIL_LINES` | `200` | shell 返回给 LLM 的默认最新输出行数 |
| `HARNESS_SHELL_MAX_TAIL_LINES` | `500` | 单次 shell 返回允许的最大输出行数 |
| `HARNESS_SHELL_DEFAULT_WAIT_TIMEOUT_SECS` | `300` | `shell.wait` 默认超时时间 |
| `HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS` | `10` | `shell.stop(wait_for_exit=true)` 默认超时时间 |
| `HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM` | `65536` | 每个 stdout/stderr stream 的最大缓存字节数 |
