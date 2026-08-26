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

## 测试环境变量

真实 LLM 测试（冒烟 `tests/real_llm_smoke.rs`、场景 `tests/real_llm_scenarios.rs`）
采用 `#[ignore]` + 环境变量双重门控，永不进入 CI。变量仅测试进程读取，
不影响运行时：

| 变量名 | 默认值 | 说明 |
|--------|--------|------|
| `HARNESS_TEST_REAL_LLM` | 未设置 | 显式开关，设置任意值即启用真实 API 测试 |
| `HARNESS_LLM_API_KEY` | 无 | API Key（标准 provider 由测试桥接到原生环境变量） |
| `HARNESS_TEST_PROVIDER` | `openai` | 冒烟测试的 provider，一次只测一个 |
| `HARNESS_LLM_PROVIDER` | `default` | 场景测试使用的 provider |
| `HARNESS_MODEL` | `gpt-4.1-mini` | 测试使用的模型 |

场景测试（Layer 2）附加变量：

| 变量名 | 默认值 | 说明 |
|--------|--------|------|
| `HARNESS_TEST_JUDGE_PROVIDER` | 未设置 | Judge 独立 provider（`openai` / `anthropic` / `deepseek` / `openai-compatible`） |
| `HARNESS_TEST_JUDGE_MODEL` | `gpt-4.1-mini` | Judge 模型（应与被测模型不同源） |
| `HARNESS_TEST_JUDGE_API_KEY` | 无 | Judge API Key |
| `HARNESS_TEST_JUDGE_API_BASE` | 无 | Judge 自定义端点（仅 `openai-compatible` 必需） |
| `HARNESS_TEST_SCENARIO_GAP_SECS` | `2` | 场景间隔节流秒数（防限流） |
| `HARNESS_TEST_SCENARIO_POLL_MS` | `50` | 场景轮询间隔毫秒数 |

执行方式：

```bash
# 冒烟测试
HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=sk-xxxx \
  cargo test --test real_llm_smoke -- --ignored --nocapture

# 场景测试
HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=sk-xxxx \
  cargo test --test real_llm_scenarios -- --ignored --nocapture
```

未设置环境变量时测试自动 skip 且不产生失败；框架自检（mock executor）随 CI
常规运行。详见 `tests/scenarios/README.md` 与
`docs/design/2026-08-16-real-llm-scenario-testing-design.md`。

## 运行时配置

### 通用运行参数

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_BRAIN_ENABLED` | `false` | 是否启用 Brain Agent 调度 |
| `HARNESS_MAX_RETRIES` | `3` | LLM 请求最大重试次数 |
| `HARNESS_MAX_TOOL_ITERATIONS` | `5` | 单次用户输入后，LLM 工具调用最大迭代次数 |
| `HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS` | `300` | `wait_tasks` 默认超时时间 |
| `HARNESS_TOOL_INFLIGHT_TIMEOUT_SECS` | `300` | 异步工具调用全局失联超时（秒），sweeper 推导 `max_duration` 的全局缺省 |
| `HARNESS_AGENTS_CONFIG` | `agents.toml` | Agent 配置文件路径 |
| `HARNESS_LOG_DIR` | `logs` | JSONL 日志输出目录 |

### TUI 主循环

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_ACTIVE_POLL_MS` | `16` | 有任务或事件时主循环的轮询间隔（毫秒） |
| `HARNESS_IDLE_POLL_MS` | `150` | 完全空闲时主循环的轮询间隔（毫秒） |

### 记忆压缩

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_MEMORY_COMPRESSION_THRESHOLD_TOKENS` | `8000` | 短期记忆 token 压缩触发阈值 |
| `HARNESS_MEMORY_SUMMARY_TARGET_TOKENS` | `1000` | LLM 摘要目标 token 数 |

### Shell Runtime

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_SHELL_DEFAULT_TAIL_LINES` | `200` | shell 默认返回的最新输出行数 |
| `HARNESS_SHELL_MAX_TAIL_LINES` | `500` | 单次允许返回的最大输出行数 |
| `HARNESS_SHELL_DEFAULT_EXEC_TIMEOUT_SECS` | `300` | `shell_exec` 默认超时时间 |
| `HARNESS_SHELL_DEFAULT_STOP_TIMEOUT_SECS` | `10` | `shell_stop` 内部停止等待的默认超时时间 |
| `HARNESS_SHELL_MAX_BUFFER_BYTES_PER_STREAM` | `65536` | 每个 stdout 或 stderr stream 的最大缓存字节数 |

## Agent 配置

Agent 配置通过 `HARNESS_AGENTS_CONFIG` 指定的 TOML 文件加载（默认 `agents.toml`），完整格式参见 `agents.toml.example`。

### system_prompt 字段

| 属性 | 值 |
|------|-----|
| 类型 | `Option<String>` |
| 默认值 | `None` |
| 作用 | Agent 级 system prompt，加载时注入 Agent 组件，WorkItem 执行时作为 `system_prompt` 传递给 LLM |
| 向后兼容 | 留空时由 WorkItem 自身的 `system_prompt` 决定，保持向后兼容 |
| 当前使用方 | 仅 `profile-designer` 使用此字段 |

### `[agent.tools]` 段

`[agent.tools]` 段配置 Agent 的工具权限，支持 `default_permission` 字段与各工具的 `Allow` / `Confirm` / `Deny` 覆盖项。

__default_permission 回退规则：__

- 若显式设置 `default_permission`，对未在 overrides 中列出的工具使用该值
- 若未设置 `default_permission`（结构默认 Confirm），对未在 overrides 中列出的工具回退到 `ToolDefinition.default_permission`（工具注册时声明的默认值）

__示例：__

```toml
[agent.tools]
default_permission = "Deny"        # 显式 Deny：所有未列出的工具拒绝
shell_exec = "Allow"               # 显式 Allow
```

```toml
# 未写 [agent.tools] 段的 agent
# 所有工具回退到 ToolDefinition.default_permission
```

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

## Telegram 配置

Telegram 通道配置位于 `HARNESS_CHANNELS_CONFIG` 指向的 TOML 文件（通常为 `channels.toml`）的 `[telegram]` 段。

### 配置项

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `bot_token` | string | 无 | Bot Token，也可通过 `TELEGRAM_BOT_TOKEN` 环境变量提供 |
| `allowed_users` | string array | `[]` | 允许访问的 username、数字 user_id 或 `"*"` |
| `pairing_enabled` | bool | `false` | 是否启用 `/bind` 配对 |
| `pairing_code` | string / null | `null` | 配对码；为空字符串或 null 时，即使启用配对也会拒绝 `/bind` |

### 配对说明

- 仅当 `allowed_users` 为空、`pairing_enabled = true` 且 `pairing_code` 非空时，`/bind <code>` 才会授权当前用户。
- 配对成功后，用户会被加入本次运行的运行时白名单；若配置文件可写，还会追加到 `allowed_users` 并回写。
- 当 `pairing_code` 为空字符串或 null 时，任何 `/bind` 请求都会收到 `配对码错误。`。

## 插件系统配置

### 核心变量

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_PLUGINS_DIR` | `.harness/plugins` | 插件根目录路径，其下每个子目录视为一个插件 |

当 `HARNESS_PLUGINS_DIR` 未设置时，使用默认目录 `.harness/plugins`（相对当前工作目录）；
该目录不存在时不加载任何插件，所有 hook 派发为 noop。

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

## IM 通道配置

### 核心变量

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_CHANNELS_CONFIG` | 无（不启用通道） | IM 通道配置文件路径（TOML 格式） |

当 `HARNESS_CHANNELS_CONFIG` 未设置时，IM 通道不启用，`ChannelManager` 以空实例运行。

### 配置文件格式

配置文件为 TOML 格式，当前支持 Telegram 和 QQ 通道：

```toml
[telegram]
bot_token = "123456:ABC-DEF"
allowed_users = ["alice", "123456789"]
pairing_enabled = false
pairing_code = ""

[qq]
app_id = "1234567890"
app_secret = "your_app_secret"
allowed_users = ["user1"]
pairing_enabled = false
pairing_code = ""
```

### Telegram 通道字段

| 字段 | 必填 | 说明 |
|------|------|------|
| `bot_token` | 是 | Telegram Bot API Token |
| `allowed_users` | 否 | 允许的用户列表，支持用户名（大小写不敏感）、数字 user_id，或 `"*"` 表示允许所有人；空列表拒绝所有用户 |
| `pairing_enabled` | 否 | 是否启用 `/bind` 配对 |
| `pairing_code` | 否 | 配对码；为空字符串或 null 时，即使启用配对也会拒绝 `/bind` |

### 配对说明

- 仅当 `allowed_users` 为空、`pairing_enabled = true` 且 `pairing_code` 非空时，`/bind <code>` 才会授权当前用户。
- 配对成功后，用户会被加入本次运行的运行时白名单；若 `HARNESS_CHANNELS_CONFIG` 指向的 TOML 文件可写，还会追加到 `allowed_users` 并回写。
- 当 `pairing_code` 为空字符串或 null 时，任何 `/bind` 请求都会收到 `配对码错误。`。

### 通道行为

- 入向：长轮询 `getUpdates`，白名单过滤，匹配的用户消息触发 Task 创建
- 出向：`channel_send` 工具主动推送，支持 `[IMAGE:path]`、`[DOCUMENT:path]`、
  `[VIDEO:path]`、`[AUDIO:path]`、`[VOICE:path]` 附件标记，超过 4096 字符自动分块发送
- 监听异常自动重启：指数退避（1s → 60s）

### QQ 通道字段

| 字段 | 必填 | 说明 |
|------|------|------|
| `app_id` | 是 | QQ Bot 应用 ID（appId），也可通过 `QQ_APP_ID` 环境变量提供 |
| `app_secret` | 是 | QQ Bot 应用密钥（appSecret），也可通过 `QQ_APP_SECRET` 环境变量提供 |
| `allowed_users` | 否 | 允许的用户 openid 列表，或 `"*"` 表示允许所有人；空列表拒绝所有用户 |
| `pairing_enabled` | 否 | 是否启用 `/bind` 配对 |
| `pairing_code` | 否 | 配对码；为空字符串或 null 时，即使启用配对也会拒绝 `/bind` |

### QQ 配对说明

- 仅当 `allowed_users` 为空、`pairing_enabled = true` 且 `pairing_code` 非空时，`/bind <code>` 才会授权当前用户。
- 配对成功后，用户会被加入本次运行的运行时白名单；若配置文件可写，还会追加到 `allowed_users` 并回写。
- 当 `pairing_code` 为空字符串或 null 时，任何 `/bind` 请求都会收到 `配对码错误。`。

### QQ 通道行为

- 入向：WebSocket Gateway 接收事件（OAuth2 app token），白名单过滤，匹配的用户消息触发 Task 创建
- 出向：`channel_send` 工具主动推送，支持 `msg_type=2` markdown 文本与 `msg_type=7` 富媒体
- 附件标记：支持 `[IMAGE:path]`、`[DOCUMENT:path]`、`[VIDEO:path]`、`[AUDIO:path]`、`[VOICE:path]`
- 审批交互：QQ 不支持 Inline Keyboard，审批选项以编号列表呈现，用户回复编号即可完成审批
- ChannelId 编码：`user:<openid>` 表示私聊，`group:<openid>` 表示群聊
- 监听异常自动重启：指数退避（1s → 60s）

## 信号触发系统配置

### 核心变量

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `HARNESS_TRIGGERS_CONFIG` | 无（不启用触发） | 信号触发配置文件路径（TOML 格式） |

当 `HARNESS_TRIGGERS_CONFIG` 未设置时，信号触发系统不启用，但 `schedule_task` 工具仍可动态安排任务。

### 配置文件格式

配置文件为 TOML 格式，示例见 `triggers.toml.example`：

```toml
[webhook]
enabled = true
listen_addr = "127.0.0.1:8080"
auth_token = "shared-secret-token"

[[webhook.routes]]
kind = "github.issue_opened"
approval_channel = { frontend = "telegram", user_id = "reviewer" }
approval_context = "GitHub issue opened"
prompt_template = "请分析新 issue:\n{{body_json.title}}"

[timer]
enabled = true

[[timer.routes]]
kind = "daily_summary"
cron = "0 9 * * 1-5"
approval_channel = { frontend = "telegram", user_id = "reviewer" }
approval_context = "daily summary"
prompt_template = "执行每日摘要"
```

### Webhook 配置字段

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `webhook.enabled` | bool | 否 | 是否启用 webhook 服务器，默认 `false` |
| `webhook.listen_addr` | string | 是（启用时） | 监听地址，格式 `host:port` |
| `webhook.auth_token` | string | 否 | 共享认证 token；设置后请求需携带 `Authorization: Bearer <token>` |
| `webhook.routes[].kind` | string | 是 | 触发 kind 标识，用于匹配 `SignalTriggerRegistry` 路由 |
| `webhook.routes[].approval_channel` | table | 否 | 审批通道，包含 `frontend` 和 `user_id` 字段 |
| `webhook.routes[].approval_context` | string | 否 | 审批上下文描述 |
| `webhook.routes[].prompt_template` | string | 是 | 任务提示模板，支持 `{{body_json.field}}` 插值 |

### Timer 配置字段

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `timer.enabled` | bool | 否 | 是否启用 timer 调度器，默认 `false` |
| `timer.routes[].kind` | string | 是 | 触发 kind 标识 |
| `timer.routes[].cron` | string | 是 | cron 表达式（5 字段：分 时 日 月 周） |
| `timer.routes[].approval_channel` | table | 否 | 审批通道 |
| `timer.routes[].approval_context` | string | 否 | 审批上下文描述 |
| `timer.routes[].prompt_template` | string | 是 | 任务提示模板 |

### Prompt 模板插值

模板中使用 `{{body_json.field}}` 语法从 webhook 请求的 JSON body 中提取字段值。
支持点分路径访问嵌套字段，如 `{{body_json.repository.full_name}}`。

### 热重载

运行时执行 `/reload-triggers` 命令可重新加载 `triggers.toml`，无需重启应用。
重载会刷新 `SignalTriggerRegistry`、重启 webhook 服务器与 timer 调度器。
重载仅替换 `static_routes`，`schedule_task` 工具动态添加的任务（`dynamic_tasks`）原样保留。

### schedule_task 工具

内置工具 `schedule_task` 允许 Agent 动态安排未来 AI 任务：

- `content`: 任务提示词
- `schedule`: `"once:2026-07-07T09:00:00"` 或 `"cron:0 9 * * 1-5"`
- `output_channel`: 可选， `"tui" | "telegram" | "qq" | "feishu" | "web"`
- `target`: 可选，指定通道目标 user_id

未指定 `output_channel` 时继承当前任务的 `origin_channel`。
显式指定 `output_channel` 时必须同时提供 `target`。
`once:` 表达式接受 RFC 3339 带偏移时间或无偏移的本地时间，时间不能过去。
`cron:` 表达式按系统本地时区解释（如 `0 9 * * 1-5` 表示本地工作日 9:00）。
动态任务仅存内存，进程重启后丢失。

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
