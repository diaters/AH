# 插件开发指南

本文档说明如何为 AI Harness 开发第三方插件，覆盖插件结构、清单格式、Hook 脚本、
工具/技能/Agent/命令贡献、Host API 参考以及调试方法。

## 快速开始

一个最简插件只需要两个文件：

```text
my-plugin/
├── manifest.toml
└── hooks/
    └── on_task_created.rhai
```text

`manifest.toml`：

```toml
id = "my-plugin"
api_version = 1

[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"
```text

`hooks/on_task_created.rhai`：

```rhai
log_info("my-plugin: a new task was created");
```text

将 `my-plugin/` 目录放到 `$HARNESS_PLUGINS_DIR` 下（默认不加载插件，
需通过环境变量启用），启动 Harness 即自动加载。

## 插件目录结构

```text
my-plugin/
├── manifest.toml          # 必填：插件清单
├── hooks/                 # Hook 脚本（按需）
│   └── on_*.rhai
├── tools/                 # 工具定义（按需）
│   ├── my_tool.rhai       # 工具处理脚本
│   └── my_tool.schema.json # 输入 JSON Schema
├── skills/                # 技能文件（按需）
│   └── my-skill.md
├── agents/                # Agent 配置（按需）
│   └── my-agent.toml
├── commands/              # 命令脚本（按需）
│   └── my_cmd.rhai
└── data/                  # 插件私有数据（按需，供 read_plugin_resource 读取）
    └── config.txt
```text

所有路径均为相对于插件根目录的相对路径，不允许绝对路径。

## 清单格式

`manifest.toml` 是插件的唯一入口，格式如下：

```toml
# ── 必填字段 ──
id = "my-plugin"           # 唯一标识，不允许包含冒号 ':'
api_version = 1            # API 版本，当前仅支持 1

# ── 可选元数据 ──
name = "My Plugin"         # 显示名称
version = "0.1.0"          # 语义版本号
author = "Author Name"
description = "What this plugin does"

# ── Hook 订阅 ──
[[hooks]]
event = "on_task_created"  # hook 点名称
script = "hooks/on_task_created.rhai"  # 脚本相对路径

# ── 工具贡献 ──
[[tools]]
id = "search"              # 工具标识，不允许包含冒号
description = "Search documentation"  # 工具描述（必填，供 LLM 理解用途）
schema = "tools/search.schema.json"   # JSON Schema 相对路径
handler = "tools/search.rhai"         # 处理脚本相对路径
default_permission = "Allow"          # 可选："Allow" / "Confirm" / "Deny"

# ── 技能贡献 ──
[[skills]]
id = "my-skill"            # 技能标识，不允许包含冒号
path = "skills/my-skill.md" # 技能文件相对路径

# ── Agent 贡献 ──
[[agents]]
id = "my-agent"            # Agent 标识，不允许包含冒号
profile = "agents/my-agent.toml"  # Agent 配置相对路径

# ── 命令贡献 ──
[[commands]]
id = "search"              # 命令标识，不允许包含冒号
display = "/my-search"     # 显示名，必须以 '/' 开头
script = "commands/search.rhai"  # 脚本相对路径
description = "Search documentation from plugin"  # 可选描述
```text

### 命名空间规则

插件贡献的工具、技能、Agent 会自动加上命名空间前缀 `plugin_id:`：

- 工具 `search`（plugin_id=`my-plugin`）→ 注册为 `my-plugin:search`
- 技能 `my-skill` → 注册为 `my-plugin:my-skill`
- Agent `my-agent` → 注册为 `my-plugin:my-agent`
- 命令 `/my-search` → 通过 `/my-plugin:search [args]` 调用

因此 `id` 字段不允许包含冒号，避免与命名空间分隔符冲突。

## Hook 点

### 完整列表

| Hook 点 | 类型 | 触发时机 |
|---------|------|----------|
| `on_tool_called` | 前置（可拒绝） | 工具执行前 |
| `on_tool_returned` | 观察（可替换结果） | 工具返回后 |
| `on_task_created` | 观察 | Task 创建后 |
| `on_task_completed` | 观察 | Task 完成后 |
| `on_task_failed` | 观察 | Task 失败后 |
| `on_workitem_started` | 观察 | WorkItem 开始执行 |
| `on_workitem_completed` | 观察 | WorkItem 完成 |
| `on_workitem_failed` | 观察 | WorkItem 失败 |
| `on_agent_started` | 观察 | Agent 启动 |
| `on_agent_stopped` | 观察 | Agent 停止 |
| `on_message_dispatched` | 观察 | 消息派发 |
| `on_message_received` | 观察 | 消息接收 |
| `on_llm_response` | 观察 | LLM 响应返回 |
| `on_long_term_memory_write` | 观察 | 长期记忆写入 |
| `on_long_term_memory_evicted` | 观察 | 长期记忆淘汰 |
| `on_shared_knowledge_write` | 观察 | 共享知识写入 |
| `on_experience_candidate_submitted` | 观察 | 经验候选提交 |
| `on_experience_candidate_approved` | 观察 | 经验候选审批通过 |
| `on_experience_candidate_rejected` | 观察 | 经验候选审批拒绝 |
| `on_approval_requested` | 观察 | 审批请求创建 |
| `on_approval_resolved` | 观察 | 审批完成 |

### 前置 Hook：`on_tool_called`

这是唯一的前置 hook，可以在工具执行**之前**拒绝调用：

```rhai
// 拒绝特定工具的调用
if tool_name == "dangerous_tool" {
    tool_deny("此工具已被插件策略禁止");
}
```text

被拒绝的工具调用不会执行，LLM 收到 `PermissionDenied` 错误。

### 观察 Hook：`on_tool_returned`

可以替换工具返回的结果：

```rhai
// 对特定工具的返回值追加标记
if tool_name == "my-plugin:echo" {
    // 替换返回结果
    tool_set_result(`[observed] ${original_result}`);
}
```text

## Rhai 脚本

插件脚本使用 [Rhai](https://rhai.rs/) 语言编写。Rhai 是 Rust 嵌入式脚本语言，
语法类似 JavaScript/Rust 子集。

### 执行模型

- 每个脚本在独立线程中执行
- 单次执行超时 1 秒
- 引擎最大调用深度 32 层
- 不加载标准库（无文件系统/网络访问）
- 所有与 Harness 的交互通过 Host API 函数完成

### 脚本模板

```rhai
// 读取 World 快照
let tasks = get_task_ids();
log_info(`当前有 ${tasks.len()} 个任务`);

// 查询具体实体
if tasks.len() > 0 {
    let t = get_task(tasks[0]);
    log_info(`第一个任务: ${t.content} [${t.status}]`);
}

// 读取插件私有文件
let config = read_plugin_resource("data/config.txt");
log_info(`配置: ${config}`);
```text

### 字符串与变量

Rhai 支持双引号字符串和模板字面量：

```rhai
let name = "world";
let msg = `hello, ${name}`;   // 模板字面量
let msg2 = "hello, " + name;  // 字符串拼接
```text

### 控制流

```rhai
// 条件
if tasks.len() > 0 {
    log_info("有任务");
} else {
    log_info("无任务");
}

// 循环
let i = 0;
while i < tasks.len() {
    let t = get_task(tasks[i]);
    log_info(`任务 ${i}: ${t.status}`);
    i = i + 1;
}
```text

### 注意事项

- Rhai 没有浮点数字面量，整数除法：`7 / 2` = `3`
- `Vec` 的索引访问通过 `vec[idx]`，从 0 开始
- Map 字段访问通过 `map.key`
- 沙箱环境无 `print()`，使用 `log_info()` / `log_warn()` 输出日志

## Host API 参考

以下函数在所有 Hook 脚本、工具处理脚本、命令脚本中可用。

### 日志

```rhai
log_info(message)   // INFO 级别日志
log_warn(message)   // WARN 级别日志
log_error(message)  // ERROR 级别日志
```text

### 实体查询

```rhai
// 获取所有 Task ID 列表
let ids = get_task_ids();           // -> Vec<String>

// 查询单个 Task 详情
let task = get_task(task_id);       // -> Map { id, content, status } 或 ()

// 获取所有 Agent ID 列表
let agent_ids = get_agent_ids();    // -> Vec<String>

// 查询单个 Agent 详情
let agent = get_agent(agent_id);    // -> Map { id, name } 或 ()

// 获取指定 Task 下的 WorkItem ID
let wi_ids = get_work_item_ids_for(task_id);  // -> Vec<String>

// 查询单个 WorkItem 详情
let wi = get_work_item(work_item_id);  // -> Map { id, task_id, status } 或 ()
```text

### 实体写入

```rhai
// 创建新 Task，返回占位 ID
let new_id = create_task(title);    // -> String

// 设置 Task 元数据（v1 延迟实现，记录日志但不写回）
task_set_metadata(task_id, key, value);

// 设置 Task 标签（v1 延迟实现，记录日志但不写回）
task_set_tag(task_id, key, value);
```text

### 工具控制

```rhai
// 拒绝工具调用（仅在 on_tool_called 中有效）
tool_deny(reason);                  // reason: 拒绝原因字符串

// 替换工具返回结果（仅在 on_tool_returned 中有效）
tool_set_result(value);             // value: 任意 Rhai 值，转为 JSON
```text

### 插件资源

```rhai
// 读取插件目录内的文件（沙箱限制，不允许读取插件目录外的文件）
let content = read_plugin_resource("data/config.txt");  // -> String
```text

### 审批

```rhai
// 获取当前审批请求的 ID（在 on_approval_requested / on_approval_resolved 中有效）
let req_id = approval_request_id(); // -> String（空字符串表示无请求）

// 解决审批请求
approval_resolve(request_id, decision);  // decision: "approved" / "denied"
```text

### 经验

```rhai
// 查询经验候选详情
let candidate = experience_get_candidate(candidate_id);  // -> Map 或 ()

// 固定/取消固定经验候选
experience_set_pinned(candidate_id, true_or_false);
```text

### 技能查询

```rhai
// 列出所有已注册技能
let skills = list_skills();         // -> Vec<Map>（每个 Map 含 id, title, description）
```text

### 消息

```rhai
// 向频道发送跨插件消息（v1 仅记录日志，尚未实现路由）
emit_message(channel, payload);
```text

### 临时资源

```rhai
// 在当前 hook 执行期间暂存数据（跨脚本调用不持久化）
register_temp_resource(key, value);
```text

## 贡献工具

工具是 LLM Agent 可调用的函数。插件工具通过 `[[tools]]` 声明。

### 工具 Schema

每个工具需要一个 JSON Schema 文件定义输入参数：

`tools/search.schema.json`：

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "query": {
      "type": "string",
      "description": "Search query text"
    },
    "limit": {
      "type": "integer",
      "description": "Maximum results to return",
      "default": 5
    }
  },
  "required": ["query"]
}
```text

Schema 必须是合法的 JSON Schema，否则工具会被跳过并记录警告。

### 工具处理脚本

工具脚本通过 `args` 变量访问输入参数，最后一个表达式的值作为返回结果：

`tools/search.rhai`：

```rhai
let query = args.query;
let limit = args.limit;

// 使用 Host API 查询任务
let tasks = get_task_ids();
let results = `搜索 "${query}"，共 ${tasks.len()} 个任务`;

results
```text

### 工具权限

- `Allow`：LLM 可直接调用，无需用户确认
- `Confirm`：需要用户确认后执行（默认值）
- `Deny`：禁止调用

### LLM 侧名称映射

插件工具在内部使用 `plugin_id:tool_id` 格式（如 `my-plugin:search`），
但发送给 LLM 时自动替换为 `plugin_id__tool_id`（冒号→双下划线），
以兼容 OpenAI API 的 `^[a-zA-Z0-9_-]+$` 命名规则。LLM 返回的工具调用名
会自动还原为冒号格式。插件开发者无需处理此映射。

## 贡献技能

技能是注入到 Agent 系统提示中的知识文件。格式为 Markdown：

`skills/my-skill.md`：

```markdown
# My Skill

This skill provides guidance on using the search tool.

## When to Use

When you need to search for information, use the `my-plugin:search` tool.

## Usage

Provide a query string and optional limit parameter.
```text

技能文件会被 `SkillLoader` 在 Agent 启动时加载，内容注入系统提示。
注册名为 `plugin_id:skill_id`（如 `my-plugin:my-skill`）。

## 贡献 Agent

插件可以贡献新的 Agent 配置。Agent 配置文件为 TOML 格式，结构与 `agents.toml` 中的
单个 Agent 条目相同（不含 `[[agent]]` 包裹）：

`agents/my-agent.toml`：

```toml
name = "my-agent"
model = "gpt-4.1-mini"
tags = ["plugin", "custom"]
description = "My custom agent from plugin"

[tools]
default_permission = "Deny"
my-plugin:search = "Allow"
knowledge_search = "Allow"
```text

注册名为 `plugin_id:agent_id`（如 `my-plugin:my-agent`）。

### Agent 工具权限

`[tools]` 中的工具名使用命名空间格式。对插件自身贡献的工具使用 `my-plugin:search`，
对内置工具使用原始名称（如 `knowledge_search`、`shell_exec`）。

## 贡献命令

命令是用户可通过 `/plugin_id:command` 触发的脚本。

`commands/search.rhai`：

```rhai
// /my-plugin:search 命令处理脚本
let tasks = get_task_ids();
`[my-plugin] 当前有 ${tasks.len()} 个任务`
```text

命令返回的字符串会显示在 TUI 中。

### 命令调用格式

用户输入 `/my-plugin:search some query` 时：

- `plugin_id` = `my-plugin`
- `command` = `search`
- `args` = `some query`（不含前导空格的剩余文本）

### 内置命令

| 命令 | 说明 |
|------|------|
| `/plugins` | 列出所有已加载插件 |
| `/reload-plugins` | 热重载插件（清除旧贡献，重新扫描磁盘） |

## 安装与加载

### 环境变量

```bash
# 指定插件根目录（其下每个子目录视为一个插件）
export HARNESS_PLUGINS_DIR=/path/to/plugins
```text

未设置时插件系统不启用，所有 hook 派发为 noop。

### 目录布局

```text
$HARNESS_PLUGINS_DIR/
├── my-plugin/
│   └── manifest.toml
├── another-plugin/
│   └── manifest.toml
└── ...
```text

### 加载顺序

插件按 `id` 字母序加载。同一 hook 点有多个插件订阅时，按此顺序逐个执行。

### 热重载

执行 `/reload-plugins` 命令：

1. 收集当前插件的 ID 和贡献
2. 清除 `PluginRegistry` 和所有命名空间化贡献（工具、技能、Agent）
3. 重新扫描 `$HARNESS_PLUGINS_DIR`
4. 注册新插件贡献

注意：正在执行的 hook 脚本不受影响（已编译的 AST 在内存中）。

## 调试

### 查看加载状态

执行 `/plugins` 查看：

- 已加载的插件 ID、名称、版本
- 各类贡献数量（hooks / tools / skills / agents / commands）
- 加载失败的插件及原因

### 日志

插件脚本通过 `log_info()` / `log_warn()` / `log_error()` 输出的日志
在 Harness 的结构化日志中以 `PluginLog` 事件出现：

```json
{
  "level": "INFO",
  "fields": {
    "event": "PluginLog",
    "message": "my-plugin: a new task was created"
  }
}
```text

Hook 执行错误以 `HookScriptError` 记录，包含插件 ID、hook 点和错误信息。

### 常见错误

| 错误 | 原因 | 解决方式 |
|------|------|----------|
| `Function not found: len` | Rhai 沙箱引擎未注册该类型的方法 | 检查是否对 Host API 返回值调用了未注册的方法 |
| `HookScriptError` + `Function not found` | 脚本调用了不存在的 Host API 函数 | 检查函数名拼写，参考 Host API 参考章节 |
| `HookTimeout` | 脚本执行超过 1 秒 | 减少循环次数或简化逻辑 |
| `PluginToolSchemaInvalid` | JSON Schema 不合法 | 使用 `jsonschema` 验证器检查 schema 文件 |
| `no executor for 'plugin:tool'` | 工具注册时 executor key 不匹配 | 确认工具 id 不含冒号，命名空间由框架自动添加 |
| `400 Bad Request` + `function.name` | 工具名包含非法字符 | 框架已自动处理冒号映射，确认 id 仅含字母数字和下划线 |

### 测试流程

1. 设置 `HARNESS_PLUGINS_DIR` 指向插件目录
2. 启动 Harness，观察启动日志中的 `PluginsLoadedSummary` 和 `PluginToolRegistered` 事件
3. 执行 `/plugins` 确认贡献已注册
4. 触发 hook 点（如创建任务），在日志中搜索 `PluginLog` 确认脚本执行
5. 调用插件工具或命令验证功能
6. 执行 `/reload-plugins` 测试热重载

## 限制与注意事项

- 单脚本执行超时 1 秒，超时后结果被忽略
- `tool_deny` 仅在 `on_tool_called` 中有效
- `tool_set_result` 仅在 `on_tool_returned` 中有效
- `create_task` 创建的 Task 使用 `plugin` 用户 ID 标记来源
- `task_set_metadata` / `task_set_tag` 在 v1 中仅记录日志，不实际写回
- `emit_message` 在 v1 中仅记录日志，消息不会路由到其他插件
- `SpawnAgent` / `CreateWorkItem` / `SetApprovalDecision` / `ExperienceSetPinned` 等
  WorldCommand 在 v1 中尚未实现回放
- 超时线程不会强制终止，可能在后台继续运行（v1 接受此潜在泄漏）
- 单个插件的 `[[commands]]` 中 `display` 不允许重复

## 完整示例

参见仓库中的 `plugins/harness-demo/` 目录，该示例插件覆盖了所有功能：

- 21 个 hook 点全部订阅
- 2 个插件工具（`echo`、`query_tasks`）
- 1 个技能
- 1 个 Agent
- 2 个命令（`/demo-status`、`/demo-echo`）
- 演示 `tool_deny`、`tool_set_result`、`create_task`、`read_plugin_resource`、
  `emit_message`、`experience_get_candidate`、`approval_request_id` 等高级 API
