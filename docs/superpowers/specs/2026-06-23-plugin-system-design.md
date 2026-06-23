# 插件系统设计

> __状态：当前有效__

## 背景

AI Harness 当前的能力（记忆系统、tool 注册表、Brain 调度、shell 工具、SkillLoader、
经验治理、TUI、ECS 主链路）都是核心内置能力，整体框架是一体化结构。为了支持第三方
用户在不修改核心代码的前提下扩展框架能力，需要引入一套插件系统。

参考 VSCode 模型：核心是一个完整可运行的基础平台，插件只做加法，不替换核心能力。

## 设计目标

- 让框架变得轻量化和模块化，核心保留基础闭环，扩展能力由插件贡献
- 核心即使不加载任何插件，也能完成最基本的运行时流程
- 插件作为"完整能力包"：可以同时贡献 tool / skill / agent / slash command，并通过
  hook 在特定流程阶段介入
- 插件运行时动态加载和关闭，实现方式为"重启"，避免热重载复杂的副作用清理
- 对 LLM 语义诚实：hook 的能力边界由框架主动暴露的 Host API 限定，不暴露任意 ECS
  访问或文件 / 网络能力

## 非目标

- 不支持插件间依赖（v1 每个插件独立）
- 不支持插件直接访问其他插件内部
- 不支持插件替换核心内置能力（核心系统不被插件 override）
- 不支持插件运行时热插拔（加载 / 卸载通过重启完成）
- 不暴露任意文件读写或网络 host API（受控能力仅通过已有的 shell tool 间接访问）

## 总体架构

```text
┌────────────────────────────────────────────────────────┐
│                       TUI / Frontend                   │
└────────────────────────────────────────────────────────┘
                          │
┌────────────────────────────────────────────────────────┐
│  Harness Core (完整基础能力，开箱即用)                  │
│  ─────────────────────────────────                     │
│  - Bevy ECS 运行时                                     │
│  - Task / WorkItem / Brain / 调度 / 评估闭环           │
│  - Tool Registry（含 shell 工具）                       │
│  - Memory 系统（ShortTerm / LongTerm / SharedKnowledge)│
│  - Skill Loader（扫描 .harness/skills/）               │
│  - 经验治理 / Agent 配置                               │
│  ─────────────────────────────────                     │
│  - Plugin Loader（受控加载 .harness/plugins/）        │
│  - Hook 派发器（固定 hook 点清单）                      │
│  - Host API 表面（Rhai 可调用的受控函数集）             │
└────────────────────────────────────────────────────────┘
                          │  扫描 + 注册
┌────────────────────────────────────────────────────────┐
│  .harness/plugins/<plugin-id>/                         │
│  ├── manifest.toml                                     │
│  ├── hooks/*.rhai                                      │
│  ├── skills/*/SKILL.md                                 │
│  ├── commands/*.rhai                                   │
│  └── tools/*.rhai                                      │
└────────────────────────────────────────────────────────┘
```

核心提供完整可运行的基础能力，插件在核心之外做加法贡献。

## 插件 Manifest

`manifest.toml` 描述插件贡献了什么、hook 订阅了哪些点。综合示例：

```toml
# .harness/plugins/my-plugin/manifest.toml
id = "my-plugin"                          # 必填，全局唯一，作为命名空间
name = "My Plugin"                         # 可读名
version = "0.1.0"
author = "your-name"
description = "一个示例插件"

# --- hook 订阅 ---
[[hooks]]
event = "on_task_created"                  # 必须来自核心契约清单
script = "hooks/on_task_created.rhai"      # 相对插件根目录

[[hooks]]
event = "on_tool_called"
script = "hooks/on_tool_called.rhai"

# --- 贡献 tool ---
[[tools]]
id = "my_tool"                             # 全局 tool id，会以 "my-plugin:my_tool" 暴露
schema = "tools/my_tool.schema.json"       # JSON schema 描述参数
handler = "tools/my_tool.rhai"             # 实现脚本

# --- 贡献 skill ---
[[skills]]
id = "negotiation"
path = "skills/negotiation/SKILL.md"       # 对齐现有 SkillLoader 规范

# --- 贡献 agent ---
[[agents]]
id = "researcher"                          # 在 agents.toml 之外独立注册
profile = "agents/researcher.toml"          # Agent 配置（沿用现有 Agent 配置结构）

# --- 贡献 slash command ---
[[commands]]
id = "summarize"                            # 调用形式：/summarize
script = "commands/summarize.rhai"
description = "汇总当前 task 进展"
```

### 命名空间

- 插件贡献的 tool / agent 强制以 `<plugin-id>:<local-id>` 前缀作为全局 id
- `agents.toml` 中的内置 Agent 不带前缀；插件贡献的 Agent 永远带前缀，避免冲突
- skill id 同样强制以 `<plugin-id>:<skill-id>` 前缀，与 tool / agent 命名空间保持一致

## 加载流程

1. 启动时 `PluginLoader` 扫描 `.harness/plugins/*/manifest.toml`
2. 解析 manifest，校验 schema：
   - `id` 全局唯一
   - hook `event` 必须在核心契约清单内
   - 引用的脚本 / SKILL.md / schema 文件必须存在
   - hook 脚本语法在启动时静态编译，语法错误在加载阶段暴露
3. 校验通过的插件按 id 注册到 `PluginRegistry`（ECS Resource）
4. 失败的插件跳过，写入 `warn` 日志，其他插件继续加载
5. 核心启动完成后，后续系统按 `PluginRegistry` 实际派发 hook、暴露 tool、注入 skill、
   加载 agent

### 启动时输出

启动时统一打印一次简洁的"有效 / 无效插件清单"，让用户对当前插件状态一目了然：

```text
[plugins] loaded: core-pulse, my-plugin, debugger
[plugins] failed: bad-plugin (hook script parse error: ...)
```

### `/reload-plugins` 语义

`/reload-plugins` 不在本进程内做热重载，而是触发"重启执行流"：

- 退出当前进程（或仅 TUI 模式的内部启动序列重置）
- 再次进入启动流程，按当前磁盘上的 `.harness/plugins/` 重新加载

这避免了热重载需要处理"已有 ECS 实体引用插件贡献的 component"等复杂语义。在语义上等
同于重启。

### `/plugins` 命令

提供 `/plugins` slash command 查看当前已加载插件清单、贡献的 tool / agent / hook / 
command / skill 列表，以及失败插件的错误摘要。

## Hook 点清单（核心契约）

hook 点清单是核心契约的一部分，新增 hook 点算重大设计决策，需要设计评审。

v1 仅暴露一组命名 hook 点，不支持事件总线或自由订阅。

```text
v1 hook 点清单
─────────────────────────────────────────────────────────────

[前 hook 可拒绝 / 改入参]
- on_tool_called           # 工具调用前，参数已解析；可 deny

[后 hook 仅观察 + 受控修改]
- on_task_created
- on_task_completed
- on_task_failed
- on_workitem_started
- on_workitem_completed
- on_workitem_failed
- on_agent_started
- on_agent_stopped
- on_tool_returned          # 可读 result，受控修改
- on_message_dispatched
- on_message_received
- on_llm_response
- on_long_term_memory_write
- on_long_term_memory_evicted
- on_shared_knowledge_write
- on_experience_candidate_submitted
- on_experience_candidate_approved
- on_experience_candidate_rejected
- on_approval_requested
- on_approval_resolved
```

### 前 / 后语义

- "前" hook：在事件之前派发；可拒绝或修改入参（如 `on_tool_called` 可 deny）
- "后" hook：在事件之后派发；只能观察 + 通过 host API 受控修改出参或副作用
- v1 默认采用"后"观察为主，仅在必要处提供"前"

### 派发顺序

- hook 顺序同步派发，按插件加载顺序依次执行
- 前一个 hook 完成才下一个；不并行
- 派发在线程内同步执行；脚本若需要触发异步动作（如 `spawn_agent`），通过 host API
  发指令，不阻塞 hook 返回
- hook 脚本固定超时 1 秒（v1 保守值），超时视为失败，log warn，下次同类事件继续派
  发

### 上下文对象

每个 hook 被派发时，Rhai 脚本接收一个 `ctx` 对象，包含本次事件相关的句柄：

```rhai
// hooks/on_task_created.rhai
let task_id = ctx.task_id;
let task = get_task(task_id);
log_info(`task created: ${task.title}`);

if task.metadata["source"] == "ci_failure" {
    task_set_metadata(task_id, "needs_triage", "true");
}
```

```rhai
// hooks/on_tool_called.rhai
if ctx.tool_id == "core:shell_exec" {
    let cmd = ctx.params["command"];
    if cmd.contains("rm -rf") {
        tool_deny("blocked by plugin policy");
        return;
    }
}
```

## Host API 表面

Host API 表面是 Rhai 脚本能调用的受控函数集，也是"对 LLM 语义诚实"的关键防线。

设计原则：

1. 只暴露框架主动承诺的能力，不让插件通过组合 API 拿到任意 component 句柄
2. 每个 API 都有清晰语义和失败模式，失败返回 `Result`
3. 不直接暴露 `World` / `Entity` 引用；通过句柄（`task_id`、`agent_id`、`workitem_id`）
   访问
4. API 表面演进算核心契约，新增 API 算重大变更，需要设计评审

### v1 Host API 清单

```text
[读 - 实体查询]
get_task(task_id)                 -> Task
get_task_ids()                   -> [TaskId]
get_work_item(workitem_id)       -> WorkItem
get_work_item_ids_for(task_id)   -> [WorkItemId]
get_agent(agent_id)              -> Agent
get_agent_ids()                  -> [AgentId]

[写 - 创建]
create_task(input)               -> TaskId        # 触发 on_task_created
spawn_agent(profile_id, ctx)     -> AgentId       # 派生 Agent
create_work_item(task_id, ...)   -> WorkItemId

[写 - 修改 component]
task_set_tag(task_id, key, val)
task_set_metadata(task_id, k, v)

[工具相关]
tool_deny(reason)                # 只在 on_tool_called 中有效
tool_set_result(result)          # 只在 on_tool_returned 中有效（允许替换结果）

[Skill / 命令相关]
list_skills()                    -> [SkillInfo]
emit_message(channel, payload)   # 发送到消息通道
register_temp_resource(...)       # 在 PluginRegistry 注册临时资源，reload 时清空

[审批相关]
approval_request_id()            # 在 on_approval_requested 中拿 ID
approval_resolve(request_id, decision)

[经验治理相关]
experience_get_candidate(id)    -> ExperienceCandidate
experience_set_pinned(id, pinned)

[日志]
log_warn(msg)
log_info(msg)
log_error(msg)
```

### 插件级 state

- 每个插件实例有自己的 `state: Map`，由同一插件所有 hook 共享
- `reload-plugins`（重启）时清空所有插件 state
- 插件 state 不写入 ECS，仅存在于 `PluginRegistry` 内

### 安全边界

| 策略 | 说明 |
|---|---|
| host API 白名单 | Rhai 只能调用 manifest 注册的函数 |
| 无 World 句柄 | 插件不能拿 World / Entity 直接引用 |
| 插件间隔离 | 不暴露跨插件调用能力 |
| 插件沙箱目录 | 插件只能读自己目录（SKILL.md / scripts）；不能读其他插件目录、不能读 `.harness/` 之外 |
| tool / agent id 命名空间 | 强制 `plugin-id:tool-id` 前缀 |
| 无网络 / 文件系统 host API | v1 不暴露网络、任意文件读写 API；插件要操作文件必须通过贡献的 shell tool 间接访问 |

### API 表面演进规则

- 新增 host API 算核心契约变更，需要设计评审
- 已有 API 的签名变更算破坏性变更，按重大变更流程处理（ADR 或设计文档）

## 错误处理

```text
错误层次                                处理策略
──────────────────────────────────────────────────────────────────
manifest 校验失败                       跳过该插件，warn 日志，启动继续
hook 脚本引用文件缺失                   跳过该插件，warn 日志
hook 脚本语法错误                       启动时静态编译失败，跳过该插件
hook 脚本运行时 panic                   一次失败仅 log warn，下次继续派发
hook 脚本超时 (1s)                      视为失败，log warn，下次继续派发
host API 调用失败                        返回 Result，脚本可处理
host API 调用越权                        返回 Error，记 warn 日志
插件贡献的 tool / command 执行失败       返回错误给调用方，写入工具调用历史
```

核心原则：插件失败永远是"软失败"——不扩散到其他插件、不毁核心进程。日志是主要可
观测手段。

## 测试策略

### 单元测试

- `PluginLoader`: manifest 解析 / 校验
- `PluginRegistry`: 注册 / 查询 / 命名空间前缀
- `HostApi`: 单个 API 的输入校验、错误路径
- Hook 派发器: 顺序派发、超时、panic 隔离

### 集成测试

- 插件加载 → hook 被派发 → host API 副作用可见
- 多插件共存，hook 顺序派发
- 坏插件（manifest 错误 / 脚本 panic）不影响好插件
- `/reload-plugins` 触发重启效果

### 回归测试

- 核心"基础闭环"在空 `.harness/plugins/` 目录下仍可用
- 不加载任何插件时，记忆治理 / shell 工具 / 评估闭环不退化

### Hook 点覆盖

每个 hook 点至少一个集成测试：
- 触发事件
- 验证 hook 脚本被调用
- 验证 host API 副作用可见
- 验证"前 hook"的 deny 能阻止后续流程

### 内置示例插件

仓库内置一个 `test-plugin` 用于测试：

- 贡献一个无副作用的 tool
- 订阅 `on_task_created` hook 写一条 metadata
- 贡献一个 `/test-hello` slash command

该插件不进 `.harness/plugins/` 默认目录，只在测试期间由测试框架读入。

## 实现范围

本 spec 仅覆盖插件系统本身的设计。后续实施计划由 `writing-plans` skill 单独生成。

实施时应同步更新 `docs/current-state.md` 与 `docs/README.md`，新增插件系统的能力状态
条目和阅读入口。