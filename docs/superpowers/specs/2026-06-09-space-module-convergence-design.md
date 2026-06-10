# Space 模块收敛设计

## 1. 背景

当前项目中的 `Space` 已经不是一个独立的运行时子系统，而是一组全局共享 `Resource` 的集合。
其中真正进入主链路、并被持续消费的能力主要集中在两个方向：

- `SpaceKnowledge`：为 `/remember` 和 `knowledge_search` 提供共享知识容器
- `SpaceToolRegistry`：为调度链路和工具执行链路提供全局工具定义注册

与此同时，`Space` 中还残留若干未充分落地或与当前实现不一致的部分：

- `SpacePreferences`、`SpaceAgentRegistry`、`SpaceRuntimeContext` 仅定义和初始化，未进入主链路
- `SpaceSessionRegistry` 与 `NativeProcessBackend` 同时保存 session 状态，形成双真源
- shell session 模型中仍有 `cursor`、`wait`、`signal` 等旧语义残留，与当前“最新快照”设计不一致
- `shell_exec` / `shell_start` 暴露了 `env` 语义，但运行时没有真正传递到子进程

这些问题会持续增加模块认知负担，弱化系统边界，并使文档与实现发生漂移。

## 2. 目标

本次收敛的目标是：

- 让 `Space` 只保留已被主链路证明有价值的全局共享资源
- 消除 shell session 状态的双真源问题
- 让 shell session 模型彻底收敛到“最新快照”语义
- 补齐已暴露但未兑现的 `env` 契约
- 同步更新测试和文档，使“当前状态”和实际实现保持一致

本次设计不追求扩展新能力，不引入新的抽象层，也不在本轮构建持久化知识系统。

## 3. 非目标

以下事项不在本轮范围内：

- 不重命名 `Space` 概念本身
- 不重构 LLM 工具面，不改变六个 shell 工具的对外意图
- 不新增知识持久化、向量检索或复杂排序能力
- 不引入新的 session backend 抽象层级
- 不扩展审批、父 Agent 审核或 provider 兼容性能力

## 4. 当前问题

### 4.1 `Space` 概念过宽

当前 `Space` 试图同时承载知识、偏好、Agent 配置镜像、运行时上下文和 shell session 状态，
但真正稳定落地的只有知识和工具注册表。这导致：

- `Space` 的概念边界与代码现实不一致
- 未使用资源长期占位，增加理解和维护成本
- 新功能容易继续向 `Space` 堆积，形成新的杂项容器

### 4.2 shell session 双真源

`SpaceSessionRegistry` 和 `NativeProcessBackend` 同时持有 session 相关状态：

- `SpaceSessionRegistry` 持有 `sessions` 和 `runtimes`
- `NativeProcessBackend` 持有 `sessions`、`processes`、`stdins`

这种设计会带来两个问题：

- 系统难以回答“某个 session 的真实状态到底在哪里”
- 后续修改 session 生命周期、清理逻辑或调试行为时，容易产生同步遗漏

### 4.3 session 模型仍有旧协议残影

当前文档已经明确 shell 输出采用“最新快照”语义，但 session 领域模型里仍残留：

- `cursor`
- `next_cursor`
- `SessionOutputRequest`
- `SessionOutputResponse`
- `SessionWaitRequest`
- `SessionCommand::Signal`

这些结构会继续暗示系统支持“增量游标 / wait / signal 协议”，与当前工具面不一致。

### 4.4 `env` 契约未兑现

`SessionStartRequest` 已定义 `env: HashMap<String, String>`，但当前：

- `shell_exec` / `shell_start` 构造请求时传入空 map
- `NativeProcessBackend` 启动进程时没有把 `env` 注入 `Command`

这属于接口已声明但行为未兑现的状态。

## 5. 收敛原则

本次收敛遵循以下原则：

- 只保留真实进入主链路的 `Space` 资源
- 一个运行态事实只允许一个真源
- 对 LLM 暴露的工具协议必须语义诚实
- 不为历史兼容保留没有当前价值的抽象
- 改动优先服务于当前代码可维护性，而不是为未来预留层级

## 6. 方案概览

本次采用“中等收敛”方案：

- 保留 `SpaceKnowledge`
- 保留 `SpaceToolRegistry`
- 删除 `SpacePreferences`
- 删除 `SpaceAgentRegistry`
- 删除 `SpaceRuntimeContext`
- 删除 `SpaceSessionRegistry`
- 将 shell session 真源统一到 `NativeProcessBackend`
- 删除 session 模型中的 `cursor/wait/signal` 旧残留
- 补齐 `shell_exec` / `shell_start` 的 `env` 契约
- 更新测试与文档

## 7. 模块边界调整

### 7.1 `Space` 的新边界

收敛后，`Space` 的定位改为：

> 全局共享 `Resource` 的命名空间，且仅保留已经进入主链路的稳定共享资源。

保留的 `Space` 资源：

- `SpaceKnowledge`
- `SpaceToolRegistry`

删除的 `Space` 资源：

- `SpacePreferences`
- `SpaceAgentRegistry`
- `SpaceRuntimeContext`
- `SpaceSessionRegistry`

### 7.2 shell session 的归属

收敛后，shell session 仍然是全局运行态资源，但不再被建模为 `Space` 资源。
它的归属调整为：

- `Space`：负责共享知识和工具定义
- `NativeProcessBackend`：负责 session 生命周期、进程状态、输出缓冲、owner 校验和清理

## 8. session 真源设计

### 8.1 唯一真源

`NativeProcessBackend` 成为 shell session 的唯一真源。

它统一持有：

- session 句柄
- 子进程句柄
- stdin 句柄
- 输出缓冲和交互状态

### 8.2 数据流

收敛后的 shell 数据流如下：

1. 内置 shell 工具把输入解析为 `ToolAction`
2. `orchestrator` 根据 `ToolAction` 直接调用 `NativeProcessBackend`
3. backend 完成创建、读取、输入、停止、列举和 owner 校验
4. `tool_result_system` 按原有机制把结果写回任务上下文
5. `task_termination_system` 在任务终态时直接要求 backend 停止该任务拥有的活动 session

### 8.3 plugin 调整

`ToolRuntimePlugin` 收敛后只注入：

- `SpaceToolRegistry`
- `BuiltinToolExecutors`
- `NativeProcessBackend`

不再注入 `SpaceSessionRegistry`。

## 9. session 模型收敛

### 9.1 对外语义

shell session 对外统一采用“最新快照”语义。

稳定对外结构保留以下信息：

- `status`
- `exit_code`
- `timed_out`
- `interaction_required`
- `output`
- `returned_lines`
- `truncated`

### 9.2 删除旧残留

本轮删除以下旧结构或旧字段：

- `SessionOutputWindow.cursor`
- `SessionOutputWindow.next_cursor`
- `SessionOutputRequest`
- `SessionOutputResponse`
- `SessionWaitRequest`
- `SessionCommand`

若存在仅服务 backend 内部的状态结构，则收缩到 backend 私有实现层，不再作为 `Space` 相关公开领域模型保留。

### 9.3 `shell_list` 约束保持不变

`shell_list` 继续只返回活动 session，不返回历史结束 session。
这样可以维持现有“帮助 LLM 找回上下文，但不污染上下文窗口”的行为边界。

## 10. `env` 契约补齐

### 10.1 输入语义

`shell_exec` 和 `shell_start` 新增或补齐可选 `env` 参数：

- 类型为对象
- key 必须为字符串
- value 必须为字符串

当 `env` 不满足字符串键值对约束时，工具返回 `InvalidInput`。

### 10.2 请求与后端

- `SessionStartRequest.env` 保留
- 工具执行器将输入中的 `env` 解析并写入 `SessionStartRequest`
- `NativeProcessBackend` 在构造 `Command` 时，把 `env` 注入进程环境

### 10.3 行为约束

- `env` 只影响当前命令对应的子进程
- 不写回全局环境
- 不跨 session 继承

## 11. `SpaceKnowledge` 最小治理

本轮不改变 `SpaceKnowledge` 的基本结构，也不引入持久化。

本轮仅明确其边界：

- `SpaceKnowledge` 只承载用户显式写入的共享知识
- 当前实现为进程内存态，重启后丢失
- `knowledge_search` 继续使用当前简单匹配语义

本轮可补充少量注释或文档说明，但不扩展到新的知识治理系统。

## 12. 兼容性影响

### 12.1 对外工具面

本次不改变以下工具名和主要用途：

- `shell_exec`
- `shell_start`
- `shell_read`
- `shell_list`
- `shell_input`
- `shell_stop`

因此，对 LLM 和回归测试而言，主要外部行为应保持稳定。

### 12.2 内部影响

受影响的主要位置包括：

- `src/domain/space.rs`
- `src/domain/session.rs`
- `src/domain/mod.rs`
- `src/plugins/tools.rs`
- `src/systems/tools/backend/native.rs`
- `src/systems/tools/builtin/shell/*.rs`
- `src/systems/tools/orchestrator.rs`
- `src/systems/transform/task_lifecycle.rs`
- 相关测试与文档

## 13. 实施步骤

建议按以下顺序实施：

1. 删除 `SpacePreferences`、`SpaceAgentRegistry`、`SpaceRuntimeContext`
2. 删除 `SpaceSessionRegistry` 及其注入和引用
3. 收敛 `Session` 领域模型，清理 `cursor/wait/signal` 残留
4. 将 backend 内部仍需保留的运行态结构收缩为私有实现
5. 为 `shell_exec` / `shell_start` 增加 `env` 解析
6. 在 `NativeProcessBackend` 中注入 `env`
7. 更新 shell 相关测试
8. 更新 `docs/current-state.md` 与相关历史文档状态说明

## 14. 测试方案

本轮测试聚焦高价值边界，而不是保留低价值兼容测试。

保留或新增的重点测试：

- `shell_exec` 能透传 `env`
- `shell_start` 能透传 `env`
- `shell_list` 只返回活动 session
- 跨 `Task` 访问 session 仍被拒绝
- 任务终态仍会清理所属 session
- shell 快照输出语义保持稳定

不再为已废弃的 `cursor/wait/signal` 协议保留测试。

## 15. 风险与缓解

### 15.1 风险：内部调用遗漏

删除未使用类型和资源时，可能遗漏某些间接引用。

缓解方式：

- 先通过全局搜索确认引用点
- 分阶段修改并运行测试
- 完成后执行 `cargo fmt`、`cargo clippy`、`cargo test`

### 15.2 风险：session 结构收敛过度

若 backend 仍需要某些运行态结构，直接删除可能影响内部实现。

缓解方式：

- 区分“对外公开的领域结构”和“backend 私有实现结构”
- 对仍有运行时价值的结构采取“内聚迁移”，而非机械删除

### 15.3 风险：文档与实现再次漂移

如果只改代码不改文档，后续会再次出现理解偏差。

缓解方式：

- 将 `docs/current-state.md` 作为当前状态的主要真相源之一同步更新
- 对历史设计文档补充状态说明，避免误读

## 16. 验收标准

完成后应满足以下标准：

- `Space` 只保留 `SpaceKnowledge` 与 `SpaceToolRegistry`
- shell session 状态真源只有 `NativeProcessBackend`
- 代码中不再保留 `cursor/wait/signal` 旧协议残留
- `shell_exec` / `shell_start` 的 `env` 能被真实传递到子进程
- 现有 shell 六工具主行为不变
- 相关测试通过
- 当前状态文档与实现一致

## 17. 结论

本次重构属于一次“减法式收敛”：

- 删除空壳资源
- 统一 session 真源
- 清理历史协议残留
- 补齐已声明契约

其目标不是扩展系统，而是让 `Space` 与 shell 运行时回到清晰、诚实、可维护的边界。
