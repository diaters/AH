# shell Tool 设计

> 本文档描述 Harness 中通用 `shell` 工具域的设计方案，覆盖阻塞与非阻塞执行、多会话管理、大输出截断、受控交互输入，以及可替换 backend（含 herdr）的集成边界。

---

## 一、设计目标

- 为 LLM 提供统一的 `shell` 工具体系，而不是零散的命令执行接口
- 同时支持阻塞与非阻塞两种执行模式
- 支持长时间运行命令的生命周期管理
- 支持大量输出场景下的窗口化返回和增量读取
- 支持受控交互输入，而不是完整终端字节流暴露
- 保持 Harness 自有工具契约稳定，不让外部 runtime 反向定义领域模型
- 通过 backend 抽象支持原生进程与 herdr 多会话管理

---

## 二、核心结论

| 维度 | 设计决策 |
|------|----------|
| 工具定位 | `shell` 是会话控制型工具域，不只是单次命令执行器 |
| 调用方式 | 同时支持阻塞与非阻塞 |
| 生命周期 | 所有长命令围绕统一 `handle_id` 管理 |
| 输出策略 | 默认只返回最新 N 行，N 由配置控制 |
| 交互输入 | 支持受控 `send_input` / `send_signal`，不支持任意 TTY 字节流 |
| 等待语义 | 仅 `shell.wait` 和 `shell.stop(wait_for_exit=true)` 进入等待态 |
| 后端架构 | 增加 `SessionBackend` 抽象，支持 `NativeProcessBackend` 与 `HerdrSessionBackend` |
| herdr 定位 | 作为可选 backend，不直接暴露 herdr 原生 API 给 LLM |
| 默认权限 | 查询类工具偏 `Allow`，执行/输入/控制类工具偏 `Confirm` |

---

## 三、对外工具集合

### 3.1 工具列表

- `shell.exec`
- `shell.start`
- `shell.status`
- `shell.read_output`
- `shell.send_input`
- `shell.send_signal`
- `shell.wait`
- `shell.stop`

### 3.2 设计原则

- LLM 永远面向 `shell.*`，不直接面向 OS 进程或 herdr pane/workspace
- 所有会话型操作都围绕统一 `handle_id`
- 工具返回值默认使用统一字段集，减少 LLM 的 schema 负担
- 大输出只返回窗口化结果，完整日志不直接进入 STM

---

## 四、工具契约

### 4.1 `shell.exec`

**语义**

- 阻塞执行一次性命令，返回最终结果
- 适合编译、测试、lint、一次性脚本

**参数**

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "要执行的 shell 命令"
    },
    "cwd": {
      "type": "string",
      "description": "执行目录，缺省时由 backend 使用默认工作目录"
    },
    "env": {
      "type": "object",
      "description": "额外环境变量"
    },
    "timeout_secs": {
      "type": "integer",
      "description": "超时时间（秒）"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["command"]
}
```

**返回格式**

```json
{
  "handle_id": "sess_01J...",
  "backend": "native",
  "status": "completed",
  "command": "cargo build",
  "cwd": "/workspace/project",
  "exit_code": 0,
  "timed_out": false,
  "interaction_required": false,
  "started_at": "2026-06-07T12:00:00Z",
  "finished_at": "2026-06-07T12:00:12Z",
  "output": {
    "combined_tail": "...latest output...",
    "combined_truncated": true,
    "returned_lines": 200
  }
}
```

**补充约束**

- `shell.exec` 偏向非交互命令
- 若命令进入明显交互等待，可返回 `interaction_required = true`，提示后续改用会话模式继续操作

### 4.2 `shell.start`

**语义**

- 非阻塞启动一个会话/进程，立即返回句柄
- 适合 HTTP server、watcher、daemon、长时间任务

**参数**

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "要执行的 shell 命令"
    },
    "cwd": {
      "type": "string",
      "description": "执行目录"
    },
    "env": {
      "type": "object",
      "description": "额外环境变量"
    },
    "session_name": {
      "type": "string",
      "description": "可选的会话名称"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["command"]
}
```

### 4.3 `shell.status`

**语义**

- 查询句柄当前状态
- 返回状态摘要和最新输出窗口

**参数**

```json
{
  "type": "object",
  "properties": {
    "handle_id": {
      "type": "string",
      "description": "会话句柄"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["handle_id"]
}
```

### 4.4 `shell.read_output`

**语义**

- 读取最新输出，或基于 `cursor` 增量读取
- 专门用于大量日志和追踪长命令输出

**参数**

```json
{
  "type": "object",
  "properties": {
    "handle_id": {
      "type": "string",
      "description": "会话句柄"
    },
    "cursor": {
      "type": "string",
      "description": "增量读取游标；省略时返回最新窗口"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["handle_id"]
}
```

### 4.5 `shell.send_input`

**语义**

- 向会话发送普通文本输入
- 适合 `y/n`、一行命令、CLI 向导文本输入

**参数**

```json
{
  "type": "object",
  "properties": {
    "handle_id": {
      "type": "string",
      "description": "会话句柄"
    },
    "input": {
      "type": "string",
      "description": "输入文本"
    },
    "append_newline": {
      "type": "boolean",
      "description": "是否自动追加换行",
      "default": true
    },
    "wait_for_output": {
      "type": "boolean",
      "description": "发送后是否等待短时间新输出",
      "default": false
    },
    "wait_timeout_secs": {
      "type": "integer",
      "description": "等待新输出的最长时间（秒）"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["handle_id", "input"]
}
```

### 4.6 `shell.send_signal`

**语义**

- 发送高层控制信号
- 第一版仅暴露高价值控制动作，而不暴露原始控制字符流

**参数**

```json
{
  "type": "object",
  "properties": {
    "handle_id": {
      "type": "string",
      "description": "会话句柄"
    },
    "signal": {
      "type": "string",
      "enum": ["interrupt", "terminate", "kill"],
      "description": "高层控制信号"
    },
    "wait_for_exit": {
      "type": "boolean",
      "description": "发送后是否等待退出",
      "default": false
    },
    "timeout_secs": {
      "type": "integer",
      "description": "等待退出的最长时间（秒）"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["handle_id", "signal"]
}
```

### 4.7 `shell.wait`

**语义**

- 显式等待句柄进入终态或交互态
- 是唯一应让当前 task 进入等待态的查询型工具

**参数**

```json
{
  "type": "object",
  "properties": {
    "handle_id": {
      "type": "string",
      "description": "会话句柄"
    },
    "timeout_secs": {
      "type": "integer",
      "description": "最长等待时间（秒）"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["handle_id"]
}
```

### 4.8 `shell.stop`

**语义**

- 请求停止会话/进程
- 可选等待退出

**参数**

```json
{
  "type": "object",
  "properties": {
    "handle_id": {
      "type": "string",
      "description": "会话句柄"
    },
    "wait_for_exit": {
      "type": "boolean",
      "description": "是否等待会话退出",
      "default": false
    },
    "timeout_secs": {
      "type": "integer",
      "description": "等待退出的最长时间（秒）"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回的最新输出行数"
    }
  },
  "required": ["handle_id"]
}
```

---

## 五、状态模型

### 5.1 对 LLM 暴露的状态

- `starting`
- `running`
- `waiting_for_input`
- `completed`
- `failed_to_start`
- `exited_with_error`
- `stopped`

### 5.2 状态语义

| 状态 | 说明 | LLM 的典型下一步 |
|------|------|------------------|
| `starting` | 已提交启动但未稳定可用 | 查询状态或读取输出 |
| `running` | 会话仍在执行中 | 等待、读输出、停止 |
| `waiting_for_input` | 会话明确等待输入 | 调用 `shell.send_input` |
| `completed` | 正常结束且退出码为 0 | 分析结果 |
| `failed_to_start` | 启动失败，进程未进入稳定运行 | 修正命令或环境 |
| `exited_with_error` | 进程执行结束但业务失败 | 阅读输出并修复 |
| `stopped` | 被外部主动停止 | 决定是否重启 |

### 5.3 关键约束

- 非零退出码不是 `ToolError`，而是业务结果 `exited_with_error`
- `shell.wait` 不仅等待退出，也可在进入 `waiting_for_input` 时提前返回

---

## 六、统一返回模型

### 6.1 顶层字段

- `handle_id`
- `backend`
- `status`
- `command`
- `cwd`
- `exit_code`
- `timed_out`
- `interaction_required`
- `started_at`
- `finished_at`
- `output`

### 6.2 输出字段

```json
{
  "combined_tail": "...latest output...",
  "combined_truncated": true,
  "returned_lines": 200
}
```

### 6.3 设计原则

- 所有工具默认只返回最新 N 行
- 查询型工具优先返回 `combined_tail`
- 若后续确有需要，可扩展 `stdout_tail` / `stderr_tail`，但第一版不强制引入

---

## 七、大输出处理

### 7.1 目标

- 防止 tool 返回值过大污染 prompt
- 避免 STM 被长日志刷爆
- 保留追踪长命令的能力

### 7.2 策略

- 内部保存 ring buffer 或 chunk buffer
- 对 LLM 默认只返回“最新 N 行”
- N 来自配置文件，可按调用参数覆盖，但受系统上限限制
- 增量日志通过 `shell.read_output(cursor=...)` 获取

### 7.3 推荐配置项

- `shell.default_tail_lines`
- `shell.max_tail_lines`
- `shell.default_tail_bytes`
- `shell.max_buffer_bytes_per_session`

### 7.4 STM 约束

- 只记录本次真正返回给 LLM 的窗口化输出
- 不把完整日志直接写入 `ShortTermMemory`

---

## 八、交互式输入

### 8.1 范围

- 第一版支持受控文本输入和高层控制信号
- 第一版不支持完整终端按键仿真
- 第一版不支持任意 TTY 字节流注入

### 8.2 设计原因

- 文本输入足以覆盖大多数交互式 CLI 场景
- 高层信号可以稳定映射到跨平台 backend 行为
- 若直接暴露原始控制字符和字节流，会显著增加跨平台差异、审计风险和 prompt 复杂度

### 8.3 隐私与日志

- `shell.send_input` 的输入内容默认不完整写入 STM
- 日志记录“发送了输入”这一事实即可
- 应支持对输入内容做省略或脱敏

---

## 九、SessionBackend 抽象

### 9.1 目标

- 把工具契约与具体运行时解耦
- 允许后端替换而不影响 LLM 使用方式

### 9.2 推荐 trait 能力

```rust
trait SessionBackend {
    fn exec_blocking(...);
    fn start_session(...);
    fn get_status(...);
    fn read_output(...);
    fn send_input(...);
    fn send_signal(...);
    fn wait_session(...);
    fn stop_session(...);
}
```

### 9.3 backend 实现

- `NativeProcessBackend`
  - 适用于简单、本地、直接命令执行
  - 第一阶段必须实现
- `HerdrSessionBackend`
  - 适用于多会话、持久 pane、后台服务托管
  - 第二阶段作为可选 backend 接入

---

## 十、herdr 集成边界

### 10.1 采用方式

- herdr 作为可选 `SessionBackend`
- Harness 自己定义 `shell.*` 工具语义
- 不直接把 herdr CLI/socket API 暴露给 LLM

### 10.2 原因

- 保持 Harness 领域模型稳定
- 避免后续切换 backend 时 tool contract 失稳
- 允许在无 herdr 环境下继续运行

### 10.3 风险

- 需要单独评估 herdr 许可证与分发边界
- 需要确认 socket/CLI 稳定性是否足够支撑自动化使用
- 仍需保留 native backend 作为兜底实现

---

## 十一、ECS 接入设计

### 11.1 保持现有主链

shell 工具接入时不新增平行执行链，而是复用现有主链：

- `ToolExecutionRequestMessage`
- `tool_dispatch_system`
- `ToolExecutionResultMessage`
- `tool_result_system`

### 11.2 新增领域对象

#### 11.2.1 `SpaceSessionRegistry`

负责管理受控会话句柄、状态快照、输出缓存、游标信息。

#### 11.2.2 `SessionHandle`

建议字段：

- `handle_id`
- `backend`
- `command`
- `cwd`
- `status`
- `started_at`
- `finished_at`
- `exit_code`
- `owner_task_id`
- `owner_agent_id`
- `interaction_state`
- `last_output_cursor`

#### 11.2.3 `WaitingForSessionInfo`

挂在发起 `shell.wait` 或 `shell.stop(wait_for_exit=true)` 的 task entity 上。

建议字段：

- `handle_id`
- `timeout_at`
- `tool_call_id`
- `agent_id`
- `return_tail_lines`

### 11.3 `WaitingReason`

推荐新增：

```rust
WaitingReason::Session { handle_id: Uuid }
```

若暂时不扩展枚举，也可先复用 `WaitingReason::ToolExecution`，再由 `WaitingForSessionInfo` 区分具体等待目标。

### 11.4 建议新增消息

- `SessionStartedMessage`
- `SessionExitedMessage`
- `SessionOutputAppendedMessage`

这些消息用于把 backend 的异步状态变化收敛回 ECS 世界。

---

## 十二、系统交互流

### 12.1 `shell.exec`

1. `tool_dispatch_system` 识别 `shell.exec`
2. 调用 backend `exec_blocking`
3. 生成 `ToolExecutionResultMessage`
4. `tool_result_system` 记录窗口化输出到 STM

### 12.2 `shell.start`

1. 调用 backend `start_session`
2. 注册 `SessionHandle` 到 `SpaceSessionRegistry`
3. 立即返回 `handle_id` 和当前状态

### 12.3 `shell.status`

1. 查询 registry
2. 必要时同步 backend 最新状态
3. 返回状态摘要和最新输出窗口

### 12.4 `shell.read_output`

1. 按 `cursor` 或最新窗口读取输出
2. 返回输出和新的 `next_cursor`
3. 不改变 task 状态

### 12.5 `shell.send_input`

1. 校验会话存在且允许输入
2. 发送文本输入
3. 若 `wait_for_output = true`，短暂等待新输出
4. 返回最新输出窗口

### 12.6 `shell.send_signal`

1. 发送高层控制信号
2. 若 `wait_for_exit = true`，复用等待逻辑
3. 返回当前状态或终态结果

### 12.7 `shell.wait`

1. 若会话已终态，立即返回结果
2. 若会话运行中：
   - task 进入等待态
   - task entity 挂 `WaitingForSessionInfo`
   - 清理本次 tool request entity
3. 后续由等待恢复 system 观察状态变化并补发 `ToolExecutionResultMessage`

### 12.8 `shell.stop`

- `wait_for_exit = false`：立即返回“已请求停止”
- `wait_for_exit = true`：复用 `shell.wait` 的等待语义

---

## 十三、权限与审批

### 13.1 默认建议

| 工具 | 默认权限 |
|------|----------|
| `shell.status` | `Allow` |
| `shell.read_output` | `Allow` |
| `shell.exec` | `Confirm` |
| `shell.start` | `Confirm` |
| `shell.send_input` | `Confirm` |
| `shell.send_signal` | `Confirm` |
| `shell.wait` | `Confirm` |
| `shell.stop` | `Confirm` |

### 13.2 原因

- 查询类工具风险较低，适合自动允许
- 执行、输入、控制类工具直接影响系统状态，默认需要确认更安全

---

## 十四、错误处理

### 14.1 `ToolError` 场景

- 句柄不存在
- 参数非法
- 权限拒绝
- backend 不可用
- backend 协议错误

### 14.2 业务失败场景

- 命令退出码非 0
- 启动成功但运行失败
- 会话运行后主动被中断

这些不应走 `ToolError`，而应走正常 `tool_output` 返回，状态为：

- `exited_with_error`
- `stopped`

---

## 十五、测试策略

### 15.1 单元测试

- 状态映射
- 输出截断
- `cursor` 增量读取
- `wait` 超时语义
- `send_input`/`send_signal` 参数校验

### 15.2 集成测试

- `shell.exec` 正常完成
- `shell.exec` 非零退出码但 tool 调用成功返回
- `shell.start -> status -> read_output -> stop`
- `shell.start -> wait`
- `shell.start -> read_output -> send_input -> wait`
- 大输出只返回最新 N 行
- 同一 contract 在 native/herdr backend 下返回结构一致

---

## 十六、分阶段实施建议

### 第一阶段

- `shell.exec`
- `shell.start`
- `shell.status`
- `shell.read_output`
- `shell.send_input`
- `shell.send_signal`
- `shell.wait`
- `shell.stop`
- `SessionBackend`
- `NativeProcessBackend`

### 第二阶段

- `HerdrSessionBackend`
- 多 session 持久化增强
- 更细粒度状态订阅

### 暂不实施

- 完整终端按键仿真
- 任意 TTY 字节流注入
- 直接暴露 herdr 原生 pane/workspace API 给 LLM

---

## 十七、设计总结

本方案将 `shell` 设计为会话控制型工具域，以 `handle_id` 统一抽象阻塞执行、后台托管、输出读取、交互输入和生命周期控制，并通过 `SessionBackend` 把具体运行时与 Harness 工具契约解耦。

该方案满足以下约束：

- 同时支持阻塞与非阻塞
- 长时间运行命令既可等待结果，也可仅托管生命周期
- 大量输出默认只返回最新窗口
- 交互式命令支持受控输入
- 多会话能力可由 herdr 承担，但不绑死 Harness 领域模型

该方案与当前 Harness 的 `tool_dispatch -> orchestrator -> tool_result` 主链兼容，适合作为后续 implementation plan 的基础。
