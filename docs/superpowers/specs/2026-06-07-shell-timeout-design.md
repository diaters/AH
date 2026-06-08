# shell 超时语义补齐设计

> 本文档聚焦补齐 Harness `shell_exec` / `shell_wait` 的超时语义与返回行为，使其满足“超时即中断并返回，且尽可能携带已产生输出”的一致性目标。

---

## 一、背景与问题

当前实现存在两类不一致：

- `shell_exec`：超时会 kill 进程并返回 `timed_out=true`，但超时分支返回的 `output` 为空窗口，无法携带被中断前的 tail 输出。
- `shell_wait`：超时仅返回 `timed_out=true`，但不会中断仍在运行的会话进程，语义不符合“超时即中断”的预期。

---

## 二、目标与非目标

### 2.1 目标

- `shell_wait(timeout_secs=...)`：超时后中断会话并返回 `status="stopped"` 且 `timed_out=true`。
- `shell_exec(timeout_secs=...)`：超时后中断并返回，同时尽可能返回“被中断前的 tail 输出”。
- 保持返回 schema 形状稳定，避免引入额外字段或破坏现有工具调用方。
- 保持职责边界清晰：等待态超时策略仍由 waiting system 控制；后端负责提供 stop/wait/status 能力。

### 2.2 非目标

- 不实现精确的 cursor 增量切片（当前仍按窗口化返回）。
- 不在本次引入跨平台信号语义细分（interrupt/terminate/kill 的平台差异维持现状）。
- 不改变 tool 权限/确认策略。

---

## 三、期望语义（对外行为约定）

### 3.1 `shell_exec`

- 正常退出：与当前一致，返回 `completed` / `exited_with_error`，并返回 tail 输出窗口。
- 超时：
  - 中断：杀掉进程，避免继续运行。
  - 返回：
    - `status="stopped"`
    - `timed_out=true`
    - `output.combined_tail` 尽可能包含超时前已产生的输出 tail（允许少量丢失，但不应恒为空）。

### 3.2 `shell_wait`

- 正常退出：与当前一致，返回 `completed` / `exited_with_error`。
- 超时：
  - 中断：对该 `handle_id` 执行 stop/kill。
  - 返回：
    - `status="stopped"`
    - `timed_out=true`
    - `output.combined_tail` 尽可能包含已产生的输出 tail。

---

## 四、设计方案（推荐：方案 A）

原则：尽量复用现有会话机制（start/wait/stop + output reader），最小化对契约与 trait 的改动。

### 4.1 `shell_wait`：在 waiting system 超时分支触发 stop

在 `check_waiting_sessions_system` 中：

- 仍每帧调用后端 `wait_session(try_wait)` 判断是否已退出。
- 若超时：
  - 调用后端 `stop_session(SessionStopRequest)` 杀掉进程。
  - 再查询一次 `get_status`（或复用 `stop_session` 返回 handle），以获得最新 `output` 窗口。
  - 返回 `SessionHandle`（或等价 JSON）给 tool_result，确保：
    - `status="stopped"`
    - `timed_out=true`
    - `output` 来自 sessions 中的最新窗口（由 output reader 持续写入）。

### 4.2 `shell_exec`：阻塞执行改为会话式执行并等待到完成/超时

当前 `exec_blocking` 使用 `wait_with_output` 收集 stdout/stderr，但超时分支没有返回任何输出。为保证“超时前 tail 输出”：

- 将 `exec_blocking` 的实现调整为：
  - 走 `start_session` 启动真实会话（已有 output reader 持续写入 `SessionHandle.output`）。
  - 在本函数内部循环 `wait_session(try_wait)` 直到：
    - 进程退出：返回最终 handle；
    - 达到 deadline：调用 `stop_session`，将 handle 标记为 `timed_out=true` 后返回。
- 这样超时返回时，`output` 将来自 reader 持续更新的 tail 窗口，满足“尽可能携带输出”的目标。

### 4.3 一致性与边界

- `timeout_secs` 仍只在两处起作用：
  - `shell_exec` 的 blocking 等待循环；
  - `WaitingForSessionInfo.timeout_at`（由 `shell_wait` / `shell_stop(wait_for_exit=true)` 写入）。
- 后端 `wait_session` 继续保持“非阻塞 try_wait”语义，避免引入阻塞等待对 ECS 帧的影响。

---

## 五、错误处理与日志

- 后端 stop/wait/status 的错误按现有模式返回 `ToolError::ExecutionFailed(...)` 或在 waiting system 中以 `warn` 记录并继续容错。
- 对 `shell_wait` 的超时 stop 行为增加结构化日志事件（`SessionWaitTimedOutStopIssued` / `SessionWaitTimedOutStopFailed`），便于排查“超时未停止”的问题。

---

## 六、测试矩阵（新增/补齐）

### 6.1 新增测试

- `shell_exec_times_out_returns_stopped_with_tail_output`
  - 执行持续输出且不会自然退出的命令（例如循环打印），设置较小 `timeout_secs`。
  - 断言：
    - `status == "stopped"`
    - `timed_out == true`
    - `output.combined_tail` 非空（或包含预期片段）。

- `shell_wait_times_out_kills_session_and_returns_stopped`
  - `shell_start` 一个长命令（例如 `sleep 5`），随后 `shell_wait(timeout_secs=0)` 或极短超时。
  - 轮询直到返回 `shell_wait` tool result。
  - 断言：
    - `status == "stopped"`
    - `timed_out == true`
  - 再调用一次 `shell_status` 或 `shell_wait(timeout_secs=1)`，应不再是 `running`（用于验证确实被中断）。

### 6.2 回归测试

- 保持现有 `shell_wait_returns_completed_when_process_exits` 不变。
- 保持 `shell_stop_transitions_a_running_session_to_stopped` 不变。

---

## 七、迁移与兼容性

- 兼容性：不引入新的输入字段；不改变既有字段含义，仅使超时行为更“强语义”。
- 风险：`shell_wait` 超时即 stop 会改变少量依赖“只返回超时不停止”的潜在调用方行为；当前 Harness 内部使用场景以“控制型工具”语义为主，可接受。

