> **状态：当前有效** — 插件系统 v2 改造设计

# 插件系统 v2：应用之于操作系统

## 概述

将插件系统从"宿主扩展"升级为"运行在操作系统上的应用"。插件拥有独立的 ECS 命名空间、
持久化存储和数据驱动的多帧执行能力，通过三个入口（Hook / Command / System）与宿主交互。

## 核心定位

插件之于 Harness，如同应用程序之于操作系统：

- 宿主提供资源调度（ECS 循环）、事件分发（Hook）、用户交互（Command）、安全隔离（数据边界）
- 插件在此之上构建自己的业务逻辑，拥有自己的数据领地，按自身节奏驱动状态机

## 三个入口

| 入口 | 触发方式 | 执行模型 | 数据访问 | 典型用途 |
|---|---|---|---|---|
| Hook | 系统事件被动激活 | 单帧同步，1s 超时 | 只读快照 + 写入自己的 entity | 监听 task 完成、捕获 tool 结果 |
| Command | 用户 `/plugin:cmd` 主动调用 | 即时执行，返回结果给用户 | 自己的 entity + 持久化存储 | 查询状态、手动触发操作 |
| System | 宿主按需分发 | 数据驱动，有匹配实体才调用 | 自己的 entity + 持久化存储 | 多帧状态机处理、批量操作 |

三者互补：Hook 捕获宿主事件数据 → 写入插件自己的 entity → System 逐步处理
→ Tool 暴露能力给其他调用者（Agent / 其他插件）。

## 数据隔离模型

### PluginOwned 命名空间（选项 A）

插件通过 `PluginOwned("plugin-id")` 标记组件在 ECS World 中拥有自己的领地。

```text
ECS World
├── 宿主实体（Task, Agent, WorkItem...）     ← 插件不可直接访问
│
├── PluginOwned("my-plugin") 实体            ← 插件可读写
│   ├── type: "analysis_job"
│   ├── status: "processing"
│   └── data: { ... }
│
└── PluginOwned("other-plugin") 实体         ← 另一个插件的，互不可见
```

### 安全规则

- 插件 spawn 的 entity 自动附加 `PluginOwned(plugin_id)` 标记组件
- 插件的 system / command 脚本只能 `query_plugin_entities()` 查询自己的 entity
- Hook 上下文中可通过 `WorldSnapshot` **只读**访问宿主 entity（Task / Agent / WorkItem）
- 跨插件数据零可见性 —— 不存在任何 `query_other_plugin_entities` Host API
- 插件 entity 的组件以 `PluginComponents(HashMap<String, serde_json::Value>)` 形式存储，
  不暴露宿主 Rust 类型

### 数据流边界

```text
宿主数据（只读）──Hook 快照──> 插件 entity（读写）──System 处理──> 持久化存储
                                                    │
                                                    ▼
                                              Tool 输出（供 Agent/其他调用者）
```

## 轻量级 System 执行模型

### 数据驱动分发（非轮询）

插件的 System 不是每帧调用，而是由原生 Rust System 按需分发：

1. 原生 `plugin_system_dispatcher`（运行在 `HarnessSet::Maintenance` 末尾）
2. 查询所有 `PluginOwned` 实体，按 `plugin_id` 分组
3. 对照每个插件 manifest 中声明的 `watches` 组件类型过滤
4. 有匹配实体 → 调用该插件的 system 脚本，传入匹配实体列表
5. 无匹配 → 跳过，不调用 Rhai 脚本（避免空转）

### Manifest 声明

单个 system 声明：

```toml
[system]
script = "system/process.rhai"
watches = ["pending_job", "retry_needed"]
timeout_ms = 2000  # 可选，默认 2000
```

`watches` 列出插件关心的组件类型。宿主仅当存在带这些组件的 `PluginOwned` 实体时
才分发。一个插件可以声明多个 system，每个关注不同的组件组合：

```toml
[[systems]]
id = "processor"
script = "system/process.rhai"
watches = ["pending_job"]
timeout_ms = 2000

[[systems]]
id = "retrier"
script = "system/retry.rhai"
watches = ["retry_needed"]
timeout_ms = 3000
```

`[system]`（单数）与 `[[systems]]`（复数数组）二选一：若只有一个 system 用单数形式，
多个则用数组形式。manifest 校验层统一归一化为 `Vec<SystemDecl>`。

### 分发伪代码

```rust
/// 运行在 HarnessSet::Maintenance 末尾的插件 system 分发器。
pub fn plugin_system_dispatcher(world: &mut World) {
    let entities_by_plugin: HashMap<String, Vec<EntitySnapshot>> =
        query_plugin_owned_entities(world);

    for plugin in registry.plugins() {
        for system_decl in plugin.manifest.systems() {
            let watched: Vec<&str> = system_decl.watches();
            if watched.is_empty() { continue; }

            let matching = entities_by_plugin
                .get(&plugin.manifest.id)
                .map(|entities| {
                    entities.iter()
                        .filter(|e| e.has_any_component(&watched))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            if matching.is_empty() { continue; }

            dispatch_plugin_system(plugin, system_decl, &matching, world);
        }
    }
}
```

### System 脚本示例

```rhai
// system/process.rhai
let jobs = query_plugin_entities("pending_job");

for job in jobs {
    let data = get_component(job, "payload");
    let result = process(data);

    set_component(job, "status", "completed");
    remove_component(job, "pending_job");

    storage_put(format!("result_{}", job), result);
}
```

### 超时与错误隔离

- 每个插件 system 调用受独立超时保护（建议 2s，可在 manifest 中配置）
- 超时或脚本 panic 仅 `warn` 日志，不影响主循环和其他插件
- 脚本错误不扩散到宿主 entity

## 持久化存储

### 存储位置

插件数据存储在独立数据目录，与代码分离：

```text
.harness/plugin-data/
├── my-plugin/
│   ├── kv_store.json        # KV 持久化数据
│   └── ...                  # 插件自定义文件
├── other-plugin/
│   └── kv_store.json
└── ...
```

### Host API

```rhai
storage_get(key)              // -> value 或 ()
storage_put(key, value)       // 写入持久化 KV 存储
storage_delete(key)           // 删除键
storage_list_keys()           // -> [String]
```

### 存储格式

底层为单个 JSON 文件 `.harness/plugin-data/<id>/kv_store.json`，
结构为 `HashMap<String, serde_json::Value>`。每次 `storage_put` / `storage_delete`
调用后立即落盘（或通过 `PluginDataStore` Resource 批量写入）。

### Reload 生命周期

```text
reload-plugins 触发
    │
    ├── despawn 该插件所有 PluginOwned 实体
    ├── 清空 PluginRegistry 中该插件条目
    ├── 重新扫描磁盘、解析 manifest、编译 AST
    ├── 加载 .harness/plugin-data/<id>/kv_store.json → PluginDataStore
    └── 插件下一帧可被分发
        （entity 需由 hook/command 重新创建，或由插件 init 脚本从 storage 恢复）
```

## 新增 Host API v2 清单

### 实体管理

```rhai
// 创建带 PluginOwned 标记的 entity
spawn_plugin_entity(type, components_map)    // -> entity_id (String)

// 查询自己的 entity，可按组件类型过滤
query_plugin_entities(component_type?)       // -> [EntityInfo]
// EntityInfo = { id: String, components: Map }

// 读写自己 entity 上的组件
get_component(entity_id, key)                // -> value 或 ()
set_component(entity_id, key, value)
remove_component(entity_id, key)

// 销毁自己的 entity
despawn_plugin_entity(entity_id)
```

### 持久化存储

```rhai
storage_get(key)                             // -> value 或 ()
storage_put(key, value)
storage_delete(key)
storage_list_keys()                          // -> [String]
```

### 时间信息

```rhai
get_frame_count()                            // -> i64
get_delta_secs()                             // -> f64
```

### 已有 v1 API（保持不变）

- 日志：`log_info` / `log_warn` / `log_error`
- 实体查询（只读）：`get_task` / `get_task_ids` / `get_agent` / `get_work_item` 等
- 工具控制：`tool_deny` / `tool_set_result`
- 插件资源：`read_plugin_resource`
- 审批：`approval_request_id`
- 经验：`experience_get_candidate`
- 消息：`emit_message`
- 临时资源：`register_temp_resource`
- 技能查询：`list_skills`

## Manifest v2 扩展

### 新增字段

```toml
id = "my-plugin"
api_version = 2              # 升级为 v2

# --- 已有（保持不变） ---
[[hooks]]
event = "on_task_completed"
script = "hooks/on_task_completed.rhai"

[[tools]]
id = "analyze"
schema = "tools/analyze.schema.json"
handler = "tools/analyze.rhai"
description = "Analyze task results"

[[commands]]
id = "status"
display = "/my-plugin:status"
script = "commands/status.rhai"

# --- 新增 ---
[[systems]]
id = "processor"
script = "system/process.rhai"
watches = ["pending_job", "retry_needed"]
timeout_ms = 2000            # 可选，默认 2000

[storage]
enabled = true               # 是否启用持久化存储
```

### API 版本兼容

- `api_version = 2` 与 `api_version = 1` 的 manifest 共存：v1 插件仍可使用，
  但不支持 `[[systems]]` 和 `[storage]` 字段
- Host API 函数签名不随 `api_version` 变化；`api_version` 仅控制 manifest schema
  校验（加载阶段根据版本决定哪些字段合法）
- 核心常量 `API_VERSION` 保持为 `1`，manifest 的 `api_version = 2` 表示启用
  扩展 manifest schema，不要求宿主 `API_VERSION` 变更

## 原生分发 System 编排

### SystemSet 归属

`plugin_system_dispatcher` 运行在 `HarnessSet::Maintenance` 集合末尾，在
`agent_stopped_hook_system` 之后。确保宿主核心流程已完成，插件 system 看到的是
最新状态。

### 与 Hook 系统的交互

```text
帧 N:
  Ingress → Transform → Dispatch → Execution → Maintenance
                                                  │
                                                  ├── agent_started_hook
                                                  ├── agent_stopped_hook
                                                  └── plugin_system_dispatcher ← 新增
                                                        │
                                                        ├── plugin-a system (有匹配实体)
                                                        └── plugin-b system (跳过)

帧 N+1:
  ...（同上）
```

## 插件完整工作流示例

以"任务分析器"插件为例，展示三个入口如何协同：

```rhai
// hooks/on_task_completed.rhai — Hook 入口（被动激活）
// 当 task 完成时，创建分析 job entity
let task_id = ctx.task_id;
let task = get_task(task_id);

if task != () {
    spawn_plugin_entity("analysis_job", {
        "task_id": task_id,
        "content": task.content,
        "pending_job": true,
        "created_at": get_frame_count()
    });
}
```

```rhai
// system/process.rhai — System 入口（数据驱动分发）
// 宿主检测到有 pending_job 组件的 entity，自动调用
let jobs = query_plugin_entities("pending_job");

for job in jobs {
    let task_id = get_component(job, "task_id");
    let content = get_component(job, "content");

    // 分析逻辑...
    let summary = analyze(content);

    // 更新 entity 状态
    set_component(job, "summary", summary);
    set_component(job, "status", "done");
    remove_component(job, "pending_job");

    // 持久化分析结果
    storage_put(format!("analysis_{}", task_id), summary);
}
```

```rhai
// commands/status.rhai — Command 入口（用户主动调用）
// 用户输入 /analyzer:status 时触发
let jobs = query_plugin_entities();
let done = 0;
let pending = 0;

for job in jobs {
    let status = get_component(job, "status");
    if status == "done" { done = done + 1; }
    if status == "pending" { pending = pending + 1; }
}

format!("分析器状态：已完成 {} 个，待处理 {} 个", done, pending)
```

```rhai
// tools/analyze.rhai — Tool（供 Agent 调用）
// Agent 调用 analyzer:get_analysis 时执行
let task_id = args.task_id;
let result = storage_get(format!("analysis_{}", task_id));

if result != () {
    result
} else {
    format!("task {} 尚未完成分析", task_id)
}
```

## 安全边界总结

| 策略 | 说明 |
|---|---|
| Host API 白名单 | Rhai 只能调用注册的函数 |
| PluginOwned 命名空间 | 插件只能访问自己创建的 entity |
| Hook 只读快照 | 宿主数据在 hook 上下文中只读 |
| 跨插件零可见 | 不存在跨插件查询 API |
| 无 World 句柄 | 插件不能拿 World / Entity 直接引用 |
| 沙箱目录 | 文件读写受路径前缀校验约束 |
| System 超时保护 | 独立超时，错误不扩散 |
| 持久化隔离 | 每个插件独立数据目录 |

## 与 v1 的差异

| 维度 | v1 | v2 |
|---|---|---|
| 定位 | 宿主扩展 | 应用之于操作系统 |
| 入口 | Hook + Command | Hook + Command + System |
| 数据访问 | 只读快照 + WorldCommand | 自有 entity + 持久化存储 |
| 执行模型 | 单帧同步 | 数据驱动多帧 |
| 持久化 | 无 | KV 存储 + 独立数据目录 |
| Reload | 清空 Registry | 清空 entity + 恢复存储 |
| API 版本 | `api_version = 1` | `api_version = 2`（向后兼容） |

## 实施优先级建议

### Phase 1：数据隔离基础

- `PluginOwned` 标记组件与 `PluginComponents` HashMap 组件
- Host API：`spawn_plugin_entity` / `query_plugin_entities` / `get_component` /
  `set_component` / `remove_component` / `despawn_plugin_entity`
- Hook 上下文中允许调用上述 API 写入插件自己的 entity

### Phase 2：轻量级 System

- Manifest `[[systems]]` 解析与校验
- `plugin_system_dispatcher` 原生 System 实现
- `watches` 过滤逻辑

### Phase 3：持久化存储

- `PluginDataStore` Resource（per-plugin HashMap）
- Host API：`storage_get` / `storage_put` / `storage_delete` / `storage_list_keys`
- Reload 时恢复存储

### Phase 4：时间信息与 Command 增强

- Host API：`get_frame_count` / `get_delta_secs`
- Command 脚本完善执行与返回值传递

## 边界声明

以下内容不纳入本次设计：

- LLM Host API（`call_llm`）：作为后续独立设计
- 跨插件消息总线：当前设计为零可见性，如需通信应通过宿主 Tool 中转
- 插件热更新（不 reload 的 entity 保留）：当前 reload 统一清空恢复
- 插件级 Agent 声明的 model 配置扩展：保持现有行为
