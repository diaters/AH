# Shell Session Task Ownership Cleanup Plan

## Summary

修复当前 shell session 生命周期与 Task 生命周期脱节的问题：

- shell session 必须严格关联到创建它的 `Task`
- 不同 `Task` 之间不得访问彼此创建的 shell session
- 当 `Task` 进入终态时，系统要立即关闭该 `Task` 关联的所有活动 shell session

本计划基于当前仓库真实实现，采用最小收敛方案：复用现有 `owner_task_id` 字段，不新增新的全局 session registry，不改变六工具外部契约，只在 runtime 层补齐“归属校验 + 终态清理 + 回归测试”。

## Current State Analysis

### 1. 已有的 session 归属信息

- `SessionHandle` 已保存 `owner_task_id` / `owner_agent_id`
- 文件：`src/domain/session.rs`

这说明“session 与 Task 关联”的领域建模已经存在，但当前没有被执法。

### 2. 当前存在的跨 Task 访问缺口

- `shell_read` 只根据 `session_id` 读取 session，没有检查当前 `Task`
- `shell_input` 只根据 `session_id` 写 stdin，没有检查当前 `Task`
- `shell_stop` 只根据 `session_id` 停止 session，没有检查当前 `Task`
- `shell_list` 返回所有活动 session，仅按状态过滤，不按 `owner_task_id` 过滤

相关文件：

- `src/systems/tools/orchestrator.rs`
- `src/systems/tools/backend/native.rs`

结论：当前不同 `Task` 确实能够访问其他 `Task` 创建的 shell session。

### 3. 当前存在的终态清理缺口

- `task_termination_system()` 当前只清理 `ToolCallingState`、发送 `TaskTerminatedMessage`、触发摘要
- 不会清理 shell session

相关文件：

- `src/systems/transform/task_lifecycle.rs`

结论：当前 `Task` 完成或失败后，关联 shell session 会继续存活。

### 4. 当前 backend 的能力基础

- `NativeProcessBackend` 已能：
  - 读取单个 session
  - 列举活动 session
  - 写入 stdin
  - 停止 session
- `stop_session()` 已包含真实进程 kill 与 `processes/stdins` 资源清理

相关文件：

- `src/contracts/sessions.rs`
- `src/systems/tools/backend/native.rs`

结论：不需要发明新清理机制，只需在现有 backend 能力上补一层“按 Task 查找 + 批量 stop + 访问鉴权”。

## Proposed Changes

### A. 收紧 SessionBackend 契约到 Task 作用域

#### 文件

- `src/contracts/sessions.rs`

#### 变更内容

在现有六动作接口基础上，为 task 归属控制增加两类能力：

1. 查询“某个 Task 拥有哪些活动 session”
2. 对 session 访问执行“按 Task 校验”

推荐的最小契约调整：

- 保留现有接口：
  - `exec_blocking`
  - `start_session`
  - `read_session`
  - `list_active_sessions`
  - `input_session`
  - `stop_session`
- 新增接口：
  - `list_task_sessions(task_id: TaskId) -> Result<Vec<SessionSummary>, String>`
  - `assert_task_owns_session(task_id: TaskId, handle_id: SessionHandleId) -> Result<(), String>`
  - `stop_task_sessions(task_id: TaskId) -> Result<Vec<SessionHandleId>, String>`

#### 为什么这样改

- 避免把“Task 归属判断”散落在多个 system 中重复实现
- 让 ownership 规则有单一 runtime 真相源
- 支持后续在 `task_lifecycle` 里直接按 `task_id` 做批量 stop

#### 如何实现

- 这些能力先只由 `NativeProcessBackend` 实现
- 内部直接复用已有 `sessions` map 中的 `owner_task_id`
- 不改变对外 tool schema

### B. 在 NativeProcessBackend 中实现 Task 归属校验与批量清理

#### 文件

- `src/systems/tools/backend/native.rs`

#### 变更内容

新增内部 helper：

- `fn session_belongs_to_task(&self, handle_id: SessionHandleId, task_id: TaskId) -> Result<SessionHandle, String>`
- `fn active_session_ids_for_task(&self, task_id: TaskId) -> Result<Vec<SessionHandleId>, String>`

实现新增 trait 方法：

- `list_task_sessions(task_id)`
- `assert_task_owns_session(task_id, handle_id)`
- `stop_task_sessions(task_id)`

#### 为什么这样改

- 归属信息当前已经存在于 backend 的 `sessions` 存储里，最适合在 backend 层做权威判断
- 可以让 `shell_list` 与 `task_termination_system` 共用同一套 owner 过滤逻辑

#### 如何实现

- `list_task_sessions(task_id)`：
  - 先遍历 `sessions`
  - 只保留 `owner_task_id == task_id`
  - 只返回活动状态：
    - `Starting`
    - `Running`
    - `WaitingForInput`
- `assert_task_owns_session(task_id, handle_id)`：
  - 查找 session
  - 若不存在，返回 `session not found`
  - 若存在但 `owner_task_id != task_id`，返回明确错误，例如：
    - `session {id} does not belong to task {task_id}`
- `stop_task_sessions(task_id)`：
  - 收集该 task 的所有活动 session id
  - 逐个调用现有 `stop_session()`
  - 若某个 session 在 stop 前已自然退出，不把它当成致命错误；记录并继续

### C. 在 orchestrator 中强制 shell 工具按当前 Task 鉴权

#### 文件

- `src/systems/tools/orchestrator.rs`

#### 变更内容

对以下动作增加 `request.request.task_id` 级别的 owner 校验：

- `ToolAction::ReadSession`
- `ToolAction::InputSession`
- `ToolAction::StopSession`

同时把 `shell_list` 改为只列当前 `Task` 的活动 session，而不是全局活动 session。

#### 为什么这样改

- `orchestrator` 已掌握当前 tool call 的 `task_id`
- 这是把 Task 身份传递到 backend ownership 校验的最直接位置

#### 如何实现

- `shell_read`：
  - 先调用 `backend.assert_task_owns_session(request.request.task_id, read_request.handle_id)`
  - 再调用 `backend.read_session(read_request)`
- `shell_input`：
  - 先校验所有权
  - 再调用 `backend.input_session(input_request)`
- `shell_stop`：
  - 先校验所有权
  - 再调用 `backend.stop_session(handle_id)`
- `shell_list`：
  - 改为调用 `backend.list_task_sessions(request.request.task_id)`

错误处理统一走现有 `spawn_tool_error()`，不新增新的消息类型。

### D. 在 Task 终态 system 中批量关闭关联 session

#### 文件

- `src/systems/transform/task_lifecycle.rs`
- 如需要注入 backend 资源，检查并同步：
  - `src/plugins/tools.rs`
  - `src/systems/mod.rs`

#### 变更内容

在 `task_termination_system()` 中补充 shell session 清理步骤：

1. 当 `task.status.is_terminal()` 时
2. 在清理 `ToolCallingState` 之后、发送 `TaskTerminatedMessage` 之前或之后立即执行均可，但顺序要固定
3. 调用 backend 的 `stop_task_sessions(task.id)`
4. 对 stop 结果打结构化日志

#### 为什么这样改

- `task_termination_system()` 是当前唯一稳定识别 Task 进入终态的地方
- 这里统一回收 session，语义最集中，也最不容易漏掉 `Done` / `Failed` / 其他终态

#### 如何实现

- 给 `task_termination_system()` 增加对 `Res<NativeProcessBackend>` 的访问
- 对每个终态 Task 执行：
  - `backend.stop_task_sessions(task.id)`
- 日志建议包含：
  - `event = "TaskShellSessionsStopped"`
  - `task_id`
  - `stopped_sessions`
  - `task_status`

### E. 补齐回归测试矩阵

#### 文件

- `tests/shell_tool_flow.rs`
- 如当前已有适合的 task lifecycle 测试文件，也可补到对应文件，但优先放在现有 shell 集成测试里

#### 变更内容

至少新增以下测试：

1. `shell_list_only_returns_sessions_for_current_task`
   - Task A 创建 session
   - Task B 调用 `shell_list`
   - 断言看不到 Task A 的 session

2. `shell_read_rejects_session_owned_by_another_task`
   - Task A 创建 session
   - Task B 调 `shell_read`
   - 断言收到 tool error

3. `shell_input_rejects_session_owned_by_another_task`
   - Task A 创建 session
   - Task B 调 `shell_input`
   - 断言失败

4. `shell_stop_rejects_session_owned_by_another_task`
   - Task A 创建 session
   - Task B 调 `shell_stop`
   - 断言失败

5. `task_termination_stops_owned_shell_sessions`
   - 创建一个 Task
   - 用该 Task 启动长运行 session
   - 将 Task 标记为终态
   - 驱动 app update
   - 断言 session 已停止，且不再出现在该 Task 的 `shell_list` 中

6. `failed_task_also_stops_owned_shell_sessions`
   - 与上一个测试相同，但终态为失败

#### 为什么这样改

- 这组测试正好覆盖用户指出的两个缺陷：
  - Task 结束后 session 仍存活
  - 不同 Task 可访问他人 session

### F. 文档同步

#### 文件

- `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`

#### 变更内容

补两条已经决策完成但文档未充分体现的约束：

- `shell_list` 只返回“当前 Task 拥有的活动会话”
- `shell_read` / `shell_input` / `shell_stop` 仅允许操作当前 Task 创建的 session
- Task 进入终态时，关联 session 立即 stop

#### 为什么这样改

- 避免后续实现与文档继续漂移
- 让“Task 作用域 session”成为正式公开约束，而不是仅存在于实现里

## Assumptions & Decisions

### 已锁定决策

- shell session 权限边界：`仅同一 Task`
- Task 终态清理策略：`立即 stop`

### 计划中的实现假设

- `TaskStatus::is_terminal()` 已覆盖所有需要清理 session 的终态
- 终态清理时允许粗暴复用现有 `stop_session()`，不新增“优雅退出”分支
- 同一 Task 下即使有多个 Agent，也允许共享该 Task 创建的 session，因为边界是 Task 而不是 Agent
- 本次不引入新的 session 清理队列或后台回收线程

### 不在本计划内

- 按 Agent 进一步细化 session 权限
- 保留已结束 session 历史列表
- 引入 grace period 或优雅停止超时
- 改造 `shell_exec` 阻塞命令的会话可见性策略

## Implementation Steps

1. 修改 `src/contracts/sessions.rs`
   - 为 backend 增加按 Task 列举、校验、批量停止接口

2. 修改 `src/systems/tools/backend/native.rs`
   - 基于 `owner_task_id` 实现 ownership helper
   - 实现 `list_task_sessions`
   - 实现 `assert_task_owns_session`
   - 实现 `stop_task_sessions`

3. 修改 `src/systems/tools/orchestrator.rs`
   - `shell_read` / `shell_input` / `shell_stop` 前置 ownership 校验
   - `shell_list` 改为 task-scoped list

4. 修改 `src/systems/transform/task_lifecycle.rs`
   - 注入 backend 资源
   - Task 终态时立即 stop 该 Task 所有活动 shell session

5. 如编译依赖需要，修改：
   - `src/plugins/tools.rs`
   - `src/systems/mod.rs`

6. 修改 `tests/shell_tool_flow.rs`
   - 新增跨 Task 访问拒绝测试
   - 新增 Task 终态自动清理测试

7. 修改设计文档
   - `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`

## Verification Steps

优先跑精确测试，再跑完整 shell 回归：

1. 运行新增跨 Task 访问测试

```bash
cargo test shell_read_rejects_session_owned_by_another_task --test shell_tool_flow -v
cargo test shell_input_rejects_session_owned_by_another_task --test shell_tool_flow -v
cargo test shell_stop_rejects_session_owned_by_another_task --test shell_tool_flow -v
```

2. 运行 Task 终态回收测试

```bash
cargo test task_termination_stops_owned_shell_sessions --test shell_tool_flow -v
cargo test failed_task_also_stops_owned_shell_sessions --test shell_tool_flow -v
```

3. 运行 shell 集成测试全集

```bash
cargo test --test shell_tool_flow -v
```

4. 运行格式化与静态检查

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

## Success Criteria

当以下条件全部满足时，视为本次修复完成：

- `shell_list` 不再暴露其他 Task 的 session
- `shell_read` / `shell_input` / `shell_stop` 无法访问其他 Task 创建的 session
- Task `Done` / `Failed` 等终态后，其关联活动 session 会立即被关闭
- shell 回归测试与静态检查全部通过
