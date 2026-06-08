# shell Tool 精简重构设计

> 本文档定义 Harness 当前 `shell` 工具的精简重构方案。目标是在保留必要交互控制能力的前提下，按照奥卡姆剃刀原则收缩工具面，提升 LLM 调用的稳定性、可理解性与可恢复性。

---

## 一、背景

当前 `shell` 工具集合偏向“后端原语暴露”，主要问题包括：

- 工具数量偏多，LLM 需要理解过多动作边界
- `status`、`read_output`、`wait` 的职责存在重叠
- `cursor`、`wait_for_output`、`wait_for_exit` 等字段提高了心智负担
- 部分参数语义不够闭合，容易让 LLM 形成错误预期
- LLM 在丢失 `session_id` 后缺少稳定的找回入口

这些问题的共同后果是：LLM 会反复试探、重复读取、误用工具，增加 token 成本与失败率。

---

## 二、设计目标

- 只保留两类执行模式：
  - 阻塞执行，默认带超时
  - 异步启动，后续通过读取最新快照观察输出
- 保留必要的交互式控制能力，但不暴露多余的底层原语
- 提供稳定的活动会话列表，帮助 LLM 找回上下文
- 保持返回结构统一，减少 schema 学习成本
- 删除对 LLM 不友好的伪精细能力

---

## 三、非目标

- 不实现严格增量输出读取
- 不暴露底层 TTY 字节流能力
- 不区分复杂信号策略，如 `interrupt` / `terminate` / `kill`
- 不保留独立的 `wait` 工具
- 不在本次重构中引入新的 backend 类型

---

## 四、核心结论

| 维度 | 设计决策 |
|------|----------|
| 执行模式 | 只保留阻塞执行和异步启动两类主路径 |
| 输出读取 | 统一使用“最新快照”语义，不做精确增量 |
| 会话发现 | 提供 `shell_list`，只返回活动会话 |
| 交互控制 | 保留 `shell_input` 和 `shell_stop` |
| 状态查询 | 并入 `shell_read`，不再单独保留 `shell_status` |
| 等待语义 | 删除 `shell_wait` |
| 信号控制 | 不再向 LLM 直接暴露 `shell_send_signal` |
| 参数策略 | 删除未兑现或高心智负担字段 |

---

## 五、对外工具集合

### 5.1 最小工具集

- `shell_exec`
- `shell_start`
- `shell_read`
- `shell_list`
- `shell_input`
- `shell_stop`

### 5.2 主调用路径

阻塞命令：

```text
shell_exec
```

异步命令：

```text
shell_start -> shell_read / shell_list -> shell_input / shell_stop
```

### 5.3 删除的旧工具

- `shell_status`
- `shell_read_output`
- `shell_wait`
- `shell_send_signal`

---

## 六、工具契约

### 6.1 `shell_exec`

**语义**

- 阻塞执行一次性命令
- 默认带超时
- LLM 可显式覆盖超时值
- 返回命令结束后的最终状态与输出快照

**适用场景**

- `cargo test`
- `cargo build`
- `ls`
- 一次性脚本

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
      "description": "额外环境变量，使用字符串键值对，例如 {\"RUST_LOG\":\"debug\",\"PORT\":\"3000\"}"
    },
    "timeout_secs": {
      "type": "integer",
      "description": "超时时间（秒），省略时使用默认值"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回输出快照的最新行数"
    }
  },
  "required": ["command"]
}
```

**返回格式**

```json
{
  "status": "completed",
  "exit_code": 0,
  "timed_out": false,
  "interaction_required": false,
  "output": "...\nlatest output...",
  "returned_lines": 120,
  "truncated": true
}
```

**约束**

- 若命令明显进入交互态，可返回 `interaction_required=true`
- 不要求阻塞执行复用后续会话 ID
- 默认超时必须是配置项，而不是调用端必填项
- 默认超时来源应为 `HarnessConfig.shell_default_exec_timeout_secs`
- `env` 只影响本次命令启动出的进程，不会永久修改系统环境
- `env` 应视为字符串键值对，例如 `{\"RUST_LOG\":\"debug\"}` 等价于命令行前缀 `RUST_LOG=debug`

### 6.2 `shell_start`

**语义**

- 异步启动命令并立即返回
- 用于后续观察、交互、停止的长生命周期任务

**适用场景**

- dev server
- watcher
- REPL
- 长时间运行脚本

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
      "description": "额外环境变量，使用字符串键值对，例如 {\"NODE_ENV\":\"development\",\"PORT\":\"3000\"}"
    },
    "tail_lines": {
      "type": "integer",
      "description": "启动后立即返回的最新输出行数"
    }
  },
  "required": ["command"]
}
```

**返回格式**

```json
{
  "session_id": "sess_01J...",
  "command": "npm run dev",
  "cwd": "/workspace/project",
  "status": "running",
  "started_at": "2026-06-08T12:00:00Z"
}
```

**约束**

- `env` 只作用于该异步会话对应的进程环境
- `env` 推荐统一使用字符串值，避免布尔和数字在不同 shell/程序中的解析差异

### 6.3 `shell_read`

**语义**

- 读取指定会话的最新输出快照
- 同时返回会话状态，替代原 `shell_status`
- 不承诺增量，只返回“当前最新窗口”

**参数**

```json
{
  "type": "object",
  "properties": {
    "session_id": {
      "type": "string",
      "description": "会话 ID"
    },
    "tail_lines": {
      "type": "integer",
      "description": "返回输出快照的最新行数"
    }
  },
  "required": ["session_id"]
}
```

**返回格式**

```json
{
  "session_id": "sess_01J...",
  "status": "running",
  "running": true,
  "exit_code": null,
  "interaction_required": false,
  "output": "...\nlatest output...",
  "returned_lines": 80,
  "truncated": true
}
```

**约束**

- `shell_read` 必须自带状态字段，避免再拆出单独状态查询工具
- 若会话已结束，仍允许读取其最后一次快照，直到该会话被清理

### 6.4 `shell_list`

**语义**

- 列出当前所有活动会话
- 用于帮助 LLM 找回 `session_id`

**参数**

```json
{
  "type": "object",
  "properties": {}
}
```

**返回格式**

```json
[
  {
    "session_id": "sess_01J...",
    "command": "npm run dev",
    "status": "running",
    "cwd": "/workspace/project",
    "started_at": "2026-06-08T12:00:00Z"
  }
]
```

**约束**

- 只返回活动会话
- “活动”定义为仍在运行或仍可交互控制的会话
- 不返回历史结束会话，避免列表污染

### 6.5 `shell_input`

**语义**

- 向会话写入受控输入
- 仅负责输入，不附带等待逻辑

**参数**

```json
{
  "type": "object",
  "properties": {
    "session_id": {
      "type": "string",
      "description": "会话 ID"
    },
    "input": {
      "type": "string",
      "description": "要写入 stdin 的内容"
    },
    "append_newline": {
      "type": "boolean",
      "description": "是否自动追加换行"
    }
  },
  "required": ["session_id", "input"]
}
```

**返回格式**

```json
{
  "session_id": "sess_01J...",
  "status": "running",
  "accepted": true
}
```

**约束**

- 返回字段应最小化为 `session_id`、`status`、`accepted`
- 不返回 `command`、`cwd`、`output`、`started_at` 等与“输入已受理”无关的字段

### 6.6 `shell_stop`

**语义**

- 停止指定会话
- 作为唯一对外终止入口

**参数**

```json
{
  "type": "object",
  "properties": {
    "session_id": {
      "type": "string",
      "description": "会话 ID"
    }
  },
  "required": ["session_id"]
}
```

**返回格式**

```json
{
  "session_id": "sess_01J...",
  "status": "stopped"
}
```

**约束**

- 返回字段应最小化为 `session_id` 和 `status`
- 不再暴露 `wait_for_exit`、`timeout_secs` 或额外的会话快照字段

---

## 七、为什么删除 `shell_wait`

`shell_wait` 的问题不在于“实现困难”，而在于它对 LLM 的主路径价值有限，却引入了额外状态机复杂度。

删除原因如下：

- 对 LLM 来说，“启动后读取最新快照”已经足以覆盖大多数异步观察场景
- `wait` 会把控制流转移到隐藏的等待态，不利于模型维持当前上下文
- `wait` 与 `read`、`stop` 的组合边界容易重叠
- 删除 `wait` 后，异步路径更稳定：
  - 启动
  - 列会话
  - 读快照
  - 输入
  - 停止

因此，本次重构将 `wait` 明确视为应删除的多余能力。

---

## 八、为什么不用增量输出

本次明确采用“最新快照”而不是“增量输出”，理由如下：

- 最新快照语义最简单、最稳定
- 不需要维护额外游标或读取偏移
- 避免伪增量语义误导 LLM
- 即使存在重复输出，LLM 也比面对不严格的游标协议更容易处理

为减少重复输出带来的 token 浪费，应遵循以下约束：

- 默认 `tail_lines` 保持较小值
- 返回 `returned_lines` 与 `truncated`
- 只保留最新窗口，不将完整日志直接注入 STM

---

## 九、内部重构建议

### 9.1 后端接口收敛

建议将 session backend 收敛为以下高层接口：

- `exec`
- `start`
- `read`
- `list_active`
- `input`
- `stop`

这样可以让 backend 直接对齐对外语义，而不是继续围绕低层原语拼装。

### 9.2 统一状态来源

当前会话状态不应存在多个“近似真相源”。重构后应保证：

- 会话状态由单一 registry 或单一 backend 状态源负责
- 上层系统不直接穿透修改 backend 内部 map
- `read` / `list` / `stop` 统一读取同一份状态数据

### 9.3 输出模型收敛

建议将输出模型收敛为快照结构，而不是面向增量协议设计。对外只需表达：

- 当前状态
- 最新输出文本
- 返回行数
- 是否截断

### 9.4 调度链简化

删除 `wait` 后，应尽量避免 shell 工具再依赖额外等待态恢复逻辑。理想状态是：

- `exec` 立即完成并回写结果
- `start` 立即返回会话信息
- `read` / `list` / `input` / `stop` 都是同步完成的短调用

这样可以显著降低 tool calling 编排复杂度。

---

## 十、兼容性与迁移

### 10.1 工具层迁移

旧工具与新工具的映射关系如下：

| 旧工具 | 新策略 |
|--------|--------|
| `shell_status` | 删除，语义并入 `shell_read` |
| `shell_read_output` | 删除，由 `shell_read` 替代 |
| `shell_wait` | 删除，不再提供 |
| `shell_send_signal` | 删除，由 `shell_stop` 替代 |
| `shell_send_input` | 重命名为 `shell_input` |

### 10.2 参数层迁移

以下字段应从对外 schema 中移除：

- `cursor`
- `wait_for_output`
- `wait_timeout_secs`
- `wait_for_exit`
- `timeout_secs`（仅 `shell_stop` / `shell_input` 的等待控制场景）
- `signal`

### 10.3 行为层迁移

- 任何“先查状态，再读输出”的调用路径，都应改为直接调用 `shell_read`
- 任何“想找回会话 ID”的路径，都应改为先调用 `shell_list`
- 任何“等待命令自己结束”的需求，在异步路径中都应改为周期性 `shell_read`

---

## 十一、测试建议

本次重构至少需要覆盖以下行为：

- `shell_exec` 默认超时生效
- `shell_exec` 显式超时覆盖默认值
- `shell_start` 成功返回活动会话
- `shell_list` 只返回活动会话
- `shell_read` 返回最新快照并自带状态
- `shell_input` 可用于交互式命令
- `shell_stop` 可停止活动会话
- 已删除工具不会继续暴露给 LLM

---

## 十二、结论

重构后的 `shell` 工具应收敛为一套更小、更诚实、更面向 LLM 意图的接口：

- `shell_exec`
- `shell_start`
- `shell_read`
- `shell_list`
- `shell_input`
- `shell_stop`

其中核心原则只有三条：

- 阻塞路径默认超时
- 异步路径只读最新快照
- 活动会话必须可列举、可找回

这套设计保留了必要控制能力，同时删除了 `wait`、`status`、增量游标和底层 signal 等高心智负担能力，符合当前阶段的简化目标。
