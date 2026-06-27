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

### TUI 主循环

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_ACTIVE_POLL_MS` | `16` | 有任务或事件时主循环的轮询间隔（毫秒） |
| `HARNESS_IDLE_POLL_MS` | `150` | 完全空闲时主循环的轮询间隔（毫秒） |

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

## 插件系统配置

### 核心变量

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_PLUGINS_DIR` | 无（不加载插件） | 插件根目录路径，其下每个子目录视为一个插件 |

当 `HARNESS_PLUGINS_DIR` 未设置时，插件系统不启用，所有 hook 派发为 noop。

### 插件清单格式

每个插件目录下必须包含 `manifest.toml`，格式如下：

```toml
id = "my-plugin"         # 插件唯一标识（必填）
api_version = 1          # API 版本，当前仅支持 1（必填）

[[hooks]]                # 订阅的 hook 点（可选，可多条）
event = "on_task_created" # hook 点名称
script = "hooks/hook.rhai" # Rhai 脚本相对路径

[[tools]]                # 贡献的工具（可选，可多条）
name = "hello"           # 工具名（注册为 my-plugin:hello）
script = "tools/hello.rhai" # Rhai 脚本相对路径
schema = "tools/hello.schema.json" # JSON Schema 相对路径

[[skills]]               # 贡献的技能（可选，可多条）
id = "my-skill"          # 技能标识（注册为 my-plugin:my-skill）
path = "skills/my-skill.md" # 技能文件相对路径

[[agents]]               # 贡献的 Agent（可选，可多条）
name = "my-agent"        # Agent 名称
model = "gpt-4.1-mini"  # Agent 使用的模型
system_prompt = "You are a helper." # 系统提示
```

### Hook 点列表

| Hook 点 | 类型 | 触发时机 |
|---------|------|----------|
| `on_task_created` | 观察 | Task 创建后 |
| `on_task_completed` | 观察 | Task 完成后 |
| `on_task_failed` | 观察 | Task 失败后 |
| `on_tool_called` | 前置（可拒绝） | 工具执行前 |
| `on_tool_returned` | 观察（可替换结果） | 工具返回后 |
| `on_work_item_started` | 观察 | WorkItem 开始执行 |
| `on_work_item_completed` | 观察 | WorkItem 完成 |
| `on_work_item_failed` | 观察 | WorkItem 失败 |
| `on_agent_started` | 观察 | Agent 启动 |
| `on_agent_stopped` | 观察 | Agent 停止 |
| `on_message_dispatched` | 观察 | 消息派发 |
| `on_message_received` | 观察 | 消息接收 |
| `on_llm_response` | 观察 | LLM 响应返回 |
| `on_long_term_memory_write` | 观察 | 长期记忆写入 |
| `on_long_term_memory_evicted` | 观察 | 长期记忆淘汰 |
| `on_shared_knowledge_write` | 观察 | 共享知识写入 |
| `on_experience_submitted` | 观察 | 经验候选提交 |
| `on_experience_governed` | 观察 | 经验候选治理完成 |
| `on_experience_persisted` | 观察 | 经验候选持久化 |
| `on_approval_requested` | 观察 | 审批请求 |
| `on_approval_resolved` | 观察 | 审批完成 |

### Host API（Rhai 脚本可用函数）

| 函数 | 说明 |
|------|------|
| `log_info(msg)` / `log_warn(msg)` | 日志输出 |
| `tool_deny(reason)` | 拒绝工具调用（仅 `on_tool_called`） |
| `tool_set_result(json)` | 替换工具返回结果（仅 `on_tool_returned`） |
| `create_task(title)` | 创建 Task |
| `set_task_metadata(task_id, key, value)` | 设置 Task 元数据（v1 延迟，不写回） |
| `set_task_tag(task_id, key, value)` | 设置 Task 标签（v1 延迟，不写回） |
| `read_plugin_resource(path)` | 读取插件目录内文件（沙箱路径检查） |
| `query_entities(filter)` | 查询 World 快照中的实体 |
| `submit_experience(...)` | 提交经验候选 |
| `send_message(content)` | 发送消息 |
| `request_approval(prompt)` | 请求审批 |

### 插件命令

| 命令 | 说明 |
|------|------|
| `/plugins` | 列出已加载的插件 |
| `/reload-plugins` | 热重载插件（清除旧贡献，重新扫描磁盘） |
| `/<plugin_id>:<command> [args]` | 调用插件贡献的命令 |

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
