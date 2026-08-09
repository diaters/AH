# 上下文压缩盲区修复设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-08-09 |
| 适用阶段 | 上下文压缩机制改造 |
| 相关文档 | `docs/design/multi-turn-memory-design.md`、`docs/current-state.md`、`logs/context-compression-blind-spot.md` |

---

## 1. 背景

### 1.1 问题描述

当前上下文压缩机制只覆盖对话级短期记忆（`ShortTermMemory`），无法管控工具执行级上下文
（`ToolCallingState.conversation`）。对工具密集型长任务，真正会膨胀的上下文层完全处于
压缩机制的盲区，存在窗口溢出的风险。

详细问题分析见 `logs/context-compression-blind-spot.md`。

### 1.2 现状

#### 两套上下文

| 层级 | 数据结构 | 压缩机制 | Token 估算 |
|------|----------|----------|------------|
| 对话级 | `ShortTermMemory.entries` + `summary_prefix` | `memory_compression_system` 阈值触发 | `add_entry` 累加；`recalculate` |
| 工具级 | `ToolCallingState.conversation` | 无（仅 `max_iterations` 迭代上限） | 无 |

#### 工具调用元数据已入 STM

工具结果并非完全不入 STM。`tool_result_system`（`src/systems/tools/result.rs:71`）在
每次工具执行后调用 `stm.record_tool_call(...)`，将 `(id, tool_name, input, output, timestamp)`
写入最后一个 Assistant 条目的 `EntryMetadata.tool_calls: Vec<ToolCall>`
（`src/domain/memory.rs:227-259`）。

但存在两个盲区：

1. __`estimated_tokens` 不计 `EntryMetadata.tool_calls`__：`add_entry` 只对 `content`
   计 token（`memory.rs:269`），`recalculate_tokens` 也只遍历 `entry.content`
   （`memory.rs:293-294`），`metadata.tool_calls` 的 input/output 完全不在 token 估算中。
   这导致工具密集型任务的 `estimated_tokens` 严重失真，压缩机制无法及时触发。
2. __`build_prompt_with_context` 不渲染 `EntryMetadata.tool_calls`__：纯文本路径只渲染
   `entry.content`（`prompt_builder.rs:56-64`），工具调用元数据被丢弃。首次 LLM 请求
   中 LLM 看不到工具交互历史，且首次请求（纯文本）与后续请求（结构化 `conversation`）
   格式不同，prompt cache 无法跨轮次命中。

#### 结构化路径的还原缺口

当 STM 中存在含 `EntryMetadata.tool_calls` 的 Assistant 条目时，从 STM 还原
`ConversationMessage` 序列可以满足缓存最大化目标。当前没有这条还原路径。

### 1.3 实证

会话 `logs/harness_2026-08-08_14-59-16.jsonl`（3446 行）：浏览器工具密集型任务，
STM 全程仅 6 条对话级条目，token 峰值 1027，远未触及 8000 阈值（代码默认值，
见 `src/app/mod.rs:273`）——压缩机制对工具密集型任务完全失效。

根因：工具输出的 token 计入了 `EntryMetadata.tool_calls.output`，但 `estimated_tokens`
不计入，压缩阈值形同虚设。

## 2. 设计目标

- __分层策略__：摘要保质量 + 硬截断兜底
- __修正 token 估算__：让 `estimated_tokens` 反映 `EntryMetadata.tool_calls` 的真实消耗
- __缓存最大化__：从 `EntryMetadata.tool_calls` 还原 `ConversationMessage` 结构化消息，
  避免格式切换导致 prompt cache 失效
- __压缩原子性__：含 `tool_calls` 的 Assistant 条目不可拆散，保证 ID 链安全
- __循环内不压缩__：工具循环中 `conversation` 保持 ID 链完整，不做任何截断或摘要

## 3. 核心设计

### 3.1 修正 `estimated_tokens` 计入 `EntryMetadata.tool_calls`

#### 改动点

1. __`add_entry`__（`src/domain/memory.rs:262`）：除了对 `content` 计 token，
   还需对 `metadata.tool_calls` 的 `input` + `output` 计 token 并累加。

2. __`recalculate_tokens`__（`src/domain/memory.rs:287`）：遍历 entries 时，
   除了 `entry.content`，还需对每个 `entry.metadata.tool_calls` 的 `input` + `output`
   计 token 并累加。

3. __`record_tool_call`__（`src/domain/memory.rs:227`）：当前不更新 `estimated_tokens`。
   改为追加 `tool_call` 后也对新追加的 `input` + `output` 计 token 并累加。

#### 效果

修正后，工具密集型任务的 `estimated_tokens` 会真实反映 `EntryMetadata.tool_calls`
的消耗，压缩阈值可以正常触发。

### 3.2 结构化还原路径

#### 还原时机

当调度层构建 `AgentExecutionRequest` 时，检查 STM 是否存在含非空 `metadata.tool_calls`
的 Assistant 条目。若有，走结构化路径；否则走现有纯文本路径。

__还原只在 `ToolCallingState` 首次创建时生效__——即下一次 User 输入触发的新工具循环
（`llm_response.rs:1264` 的 First iteration 分支）。同一 User 输入内的 follow-up
迭代（`llm_response.rs:1146` 的 `find_calling_state` 复用路径）不重新还原，
直接复用 `state.conversation`，与 2 节"循环内不压缩"一致。

#### WorkItem 派发路径的行为变化

改动点 8 让 `First iteration` 分支优先使用 `request.conversation`，这会导致 WorkItem
派发路径的 `conversation` 第一次真正生效。经分析各 WorkItem 类型：

| WorkItem 类型 | `conversation` 内容 | 影响 |
|---|---|---|
| ExperienceCollection | 从 STM 还原结构化消息（`collection.rs:139-207`） | __正向修正__——创建者希望 `conversation` 生效，但被忽略 |
| SkillUpdate | 空的 `Some(vec![])` | __需防御__——空 Vec 不是 `None`，会走结构化路径但无历史消息，缺少 System 和 User 消息 |
| ProfileGeneration | 空的 `Some(vec![])` | 同上 |

防御措施：`First iteration` 分支的判断条件改为 `request.conversation.as_ref().is_some_and(|c| !c.is_empty())`，
空 Vec 视同 `None`，走纯文本路径。非空 Vec 才走结构化路径。

此行为变化需要在集成测试中覆盖：

- ExperienceCollection WorkItem 的 `conversation` 在 `First iteration` 中生效
- SkillUpdate / ProfileGeneration WorkItem 的空 `conversation` 不影响现有行为

#### 两种请求构建路径

| 路径 | 条件 | 行为 |
|------|------|------|
| 纯文本路径 | STM 无含 `tool_calls` 的条目 | `build_prompt_with_context` → `prompt` 字段 → `conversation: None` |
| 结构化路径 | STM 有含 `tool_calls` 的条目 | 从 STM 还原 `ConversationMessage` 序列 → `conversation: Some(...)` |

#### 路径选择位置

在 `dispatch_system.rs` 构建 `AgentExecutionRequest` 时判断。当前有两处派发入口：

- __Task 直接派发__（`dispatch_system.rs:314`）：当前 `conversation: None`，需要新增
  "从 STM 还原"逻辑。
- __WorkItem 派发__（`dispatch_system.rs:130`）：当前已使用
  `conversation: work_item.input.context.conversation.clone()`，`conversation` 字段
  由 `WorkItemContext` 传入，由 WorkItem 创建者负责构造。__此路径保持现状__，不从
  STM 还原，避免与 WorkItem 创建者的意图冲突。

判断逻辑集中在一个辅助函数中，仅 Task 直接派发路径调用。

#### 还原逻辑

遍历 STM 的 `summary_prefix` + `entries`，将每个条目映射为 `ConversationMessage`：

- `summary_prefix` → 一条 `ConversationMessage::User { content: "[Previous context summary]\n..." }`
- `User` 条目 → `ConversationMessage::User { content }`
- `Assistant` 条目（有 `metadata.tool_calls`）→ 先输出 `ConversationMessage::Assistant`
    `{ content, tool_calls: [...], reasoning_content: None }`，
  再为每个 `ToolCall` 输出 `ConversationMessage::Tool { tool_call_id: call.id, content: call.output }`
- `Assistant` 条目（无 `metadata.tool_calls`）→ `ConversationMessage::Assistant`
    `{ content, tool_calls: [], reasoning_content: None }`
- `Summary` 条目 → 复用纯文本路径的渲染逻辑，输出为 `ConversationMessage::User`
- `Archive` 条目 → 跳过

#### `ToolCall` → `LlmToolCall` 映射

```rust
ToolCall { id, tool_name, input, output, timestamp }
→ LlmToolCall { id: id.unwrap_or_default(), name: tool_name, arguments: input }
```

`output` 不在 `LlmToolCall` 中，而是作为 `ConversationMessage::Tool { tool_call_id, content }`
单独输出。`timestamp` 用于保持调用顺序，不参与 `ConversationMessage` 构建。

#### 轮次还原约束

`record_tool_call` 不区分"同一轮并行调用"和"跨轮串行调用"——所有工具调用都追加到
同一个 Assistant 条目的 `metadata.tool_calls` 下。还原时统一作为一轮输出：

```text
Assistant { tool_calls: [LlmToolCall_1, LlmToolCall_2] }
Tool { tool_call_id: "1", content: output_1 }
Tool { tool_call_id: "2", content: output_2 }
```

这意味着并行调用和串行调用在还原后格式相同（都是单轮多工具）。这是可接受的降级：
LLM 看到的消息格式仍然合法（`tool_call_id` 与 `tool_calls` 中的 `id` 匹配），
缓存命中率不受影响。

### 3.3 压缩原子性

#### 约束

含非空 `metadata.tool_calls` 的 Assistant 条目是一个"工具配对组"的锚点。
压缩时该条目不可与后续条目拆散——如果该条目被摘要或 drain，其 `tool_calls`
引用的 `Tool` 消息在下游还原时将失去父条目，导致 API 报错。

#### 压缩粒度

当前 `memory_compression_system` 按 `preserve_recent_turns * 2` 条 entry 切割。
改为配对组粒度：

1. 将 STM entries 按配对组切分
2. 按 `preserve_recent_turns` 保留最近 N 个配对组
3. 被压缩的配对组整体进入摘要内容
4. 摘要完成后整体 drain

#### 配对组切分算法

```text
遍历 entries，按顺序分配配对组：
  1. 遇到 User：开启新的对话配对组
  2. 遇到 Assistant（无 tool_calls）：归入当前对话配对组；
     若当前无配对组（首条即为 Assistant），单独成组
  3. 遇到 Assistant（有 tool_calls）：开启新的工具配对组
  4. 遇到 Summary / Archive：归入最近的配对组；
     若当前无配对组，单独成组
```

由于 `record_tool_call` 将所有 `ToolCall` 挂在 Assistant 条目的 `metadata.tool_calls`
下而非作为独立 `MemoryEntry`，不存在需要配对的独立 `Tool` 条目。配对组的原子性
体现在"含 `tool_calls` 的 Assistant 条目不可拆散"这一点上。

#### `preserve_recent_turns` 语义迁移

现有语义为"对话轮数"，按 `preserve_recent_turns * 2` 计算保留条目数
（每轮 = User + Assistant）。改为配对组粒度后，语义调整为"保留最近 N 个配对组"。

量化影响：工具密集型场景下，一个含 `tool_calls` 的 Assistant 条目可能挂载 10+ 条
`ToolCall`，保留 2 个配对组对应的 token 量远多于原语义的 4 条 entries。但配对组
粒度保证 ID 链安全，这是必要约束。权衡可接受——配对组整体保留避免了半截配对组
导致的 API 报错，且 3.1 节修正 `estimated_tokens` 后压缩会更早触发，保留量增大的
同时压缩频率也增大，总量仍可控。

#### `compress_text` 构造须包含 `tool_calls`

当前 `compress_text` 构造（`memory.rs:47-52`）只渲染 `entry.content`，不含
`metadata.tool_calls`。含 `tool_calls` 的 Assistant 条目在 `compress_text` 中
只有空 `content`（`"Assistant: \n"`），摘要 WorkItem 无法生成有意义的摘要。

修正：`compress_text` 构造时，含 `metadata.tool_calls` 的 Assistant 条目要渲染
工具调用摘要，渲染格式与 3.5 节 `build_prompt_with_context` 的渲染格式统一：

```text
Assistant: <content>
  [Tool calls: shell_exec("ls") → file1.txt\nfile2.txt; shell_exec("cat x") → content...]
```

否则配对组整体 drain 后，`summary_prefix` 质量劣化，`summary_prefix` 的降级（3.6 节）
将不是"可接受的降级"而是"失真的摘要"。

### 3.4 硬截断兜底

在 `dispatch_system` 从 STM 还原 `ConversationMessage` 序列后做一次裁剪，
follow-up 路径直接复用 `state.conversation`，不重复裁剪（与 2 节"循环内不压缩"一致）。

1. 计算当前 `ConversationMessage` 序列的总 token
2. 超出窗口预算时，从最早的消息开始按配对组整体移除
3. 移除部分直接丢弃，不尝试摘要
4. 直到总量在预算内

__配对组整体移除__：与 3.3 压缩原子性一致，硬截断也以配对组为最小移除单位，
不出现半截配对组导致 ID 链悬空。

这是最后一道防线，确保任何情况下不会因上下文溢出导致 API 调用失败。

### 3.5 `build_prompt_with_context` 适配（防御性兜底）

结构化路径（3.2 节）在 Task 直接派发路径中总是可用的。`build_prompt_with_context`
中渲染 `metadata.tool_calls` 是防御性兜底——当结构化路径因故未生效（如未来新增
派发路径遗漏了结构化路径判断）时，纯文本路径仍能保留工具调用的关键信息。

渲染格式：

```text
Assistant: <content>
  [Tool calls: shell_exec("ls") → file1.txt\nfile2.txt; shell_exec("cat x") → content...]
```

在 `prompt_builder.rs` 的 `match entry.role` 分支中，`EntryRole::Assistant` 且
`metadata.tool_calls` 非空时，在 `content` 后追加工具调用摘要。此渲染格式与
`compress_text` 构造（3.3 节）统一，保证摘要和纯文本路径的信息保真度一致。

### 3.6 边界场景

#### 子 Agent / chat_with_agent

子任务也走 `record_tool_call`，工具调用元数据写入子任务的 STM。子任务完成后，
父 Agent 看到的是子任务的最终文本回复，工具交互不冒泡到父 Agent 的 STM。

#### `summary_prefix` 中的工具交互

STM 压缩后，含 `tool_calls` 的 Assistant 条目被摘要到 `summary_prefix`（纯文本），
`tool_calls` 字段在 drain 后消失。此时 `summary_prefix` 渲染为一条 `User` 消息，
结构化信息丢失——这是可接受的降级，只有未被压缩的条目走结构化路径。

#### `tool_call_id` 跨循环唯一性

LLM 触发的工具调用（`llm_response.rs:1281`）总是带有 `id`，在单次会话内通常足够唯一。
但不同 LLM provider 的 `tool_call_id` 格式不一致（OpenAI 风格 `call_abc123`、
部分模型可能使用递增序号如 `1`、`2`、`3`），使用递增序号的 provider 跨循环碰撞
风险较高。当前不做额外防护——加前缀会改变 ID 格式，破坏缓存命中。若后续实证发现
碰撞，可在还原到 `conversation` 时校验 ID 唯一性。

#### `ToolCall.id` 为 `None` 的场景

`record_tool_call` 的 `id` 参数类型为 `Option<String>`，但 LLM 触发的工具调用
总是 `Some`（`result.rs:71` 中 `result.tool_call_id` 来自
`llm_response.rs:1281` 的 `calls.iter().map(|c| c.id.clone())`，`id` 类型为 `String`）。
`None` 的场景仅出现在非 LLM 触发的工具调用（如 hook 触发），但当前代码中此类调用
不经过 `tool_result_system`。还原时 `None` 映射为空字符串，实际运行中不应出现。

## 4. 关键改动点

| # | 改动 | 位置 | 说明 |
|---|------|------|------|
| 1 | `add_entry` / `recalculate_tokens` 计入 `metadata.tool_calls` 的 token | `src/domain/memory.rs` | 修正 token 估算的根因 |
| 2 | `record_tool_call` 追加后更新 `estimated_tokens` | `src/domain/memory.rs` | 同上 |
| 3 | 结构化还原：从 `tool_calls` 还原 `ConversationMessage` | `src/systems/dispatch/dispatch_system.rs` | 缓存最大化；仅 Task 直接派发路径 |
| 4 | 路径选择：STM 有含 `tool_calls` 的条目时走结构化路径 | `src/systems/dispatch/dispatch_system.rs` | 与改动点 3 配合；仅 Task 直接派发路径 |
| 5 | 配对组粒度 + `compress_text` 渲染 `tool_calls` | `src/systems/memory.rs` | 含 `tool_calls` 的 Assistant 不可拆散；摘要须包含工具调用细节 |
| 6 | 硬截断兜底：按模型窗口预算从最早配对组移除 | `src/systems/dispatch/dispatch_system.rs` | 在结构化还原后做一次裁剪 |
| 7 | `prompt_builder` 渲染 `metadata.tool_calls`（防御性） | `src/systems/dispatch/prompt_builder.rs` | 纯文本降级路径，正常不触发 |
| 8 | `ToolCallingState` 读 `request.conversation` | `src/systems/transform/llm_response.rs` | 还原前提；非空用还原；空 Vec 视同 None |

## 5. 未改动的部分

- __数据模型__——`EntryRole` 不新增变体，`MemoryEntry` 不新增字段，`ConversationMessage` 不改
- __`record_tool_call` / `EntryMetadata.tool_calls`__——保留现有路径，不废止
- __`ToolCallingState` 数据结构__——不改
- __`build_chat_messages`__——读 `conversation` 的逻辑不变
- __`build_prompt_with_context`__——无 `tool_calls` 条目时行为不变
- __`memory_compression_system` 的触发条件和摘要 WorkItem 流程__——只改压缩粒度和 `compress_text` 构造
- __`tool_result_system`__——不改
- __WorkItem 派发路径__——保持使用 `WorkItemContext.conversation`，不从 STM 还原
- __follow-up 迭代路径__——`llm_response.rs:1146` 的 `find_calling_state` 复用路径不改，不重新还原

## 6. 风险与缓解

| 风险 | 缓解 |
|------|------|
| `estimated_tokens` 计入 `tool_calls` 后更快触发压缩 | 期望行为——压缩本该更早触发；配对组粒度 + 渲染 `tool_calls` 保证质量 |
| 还原时同一 Assistant 下的所有 `ToolCall` 被视为单轮 | 格式仍然合法，LLM 行为不受影响；缓存命中率不受影响；若后续需分轮次还原，可在 `ToolCall` 中加 `iteration` 字段 |
| `summary_prefix` 丢失 `tool_calls` 后结构化降级 | `compress_text` 已渲染 `tool_calls`，`summary_prefix` 质量保障；压缩后首次请求受影响，后续重建缓存 |
| `preserve_recent_turns` 语义迁移后保留量增大 | 工具密集型场景下 2 个配对组可能对应 20+ 条 tool_calls 的 token；但配对组粒度保证 ID 链安全是必要约束，且压缩更早触发后总量可控 |
| 硬截断丢弃最早配对组导致 LLM 丢失上下文 | 最后防线，正常情况下压缩管道会先做摘要保质量 |
| 部分模型使用递增序号作为 `tool_call_id`，跨循环碰撞风险 | 当前不做防护；若后续实证发现碰撞，可在还原时校验 ID 唯一性 |
| 改动点 8 致 WorkItem `conversation` 生效 | ExperienceCollection 正向修正；`!c.is_empty()` 防御 SkillUpdate/ProfileGeneration 不受影响 |

## 7. 验证策略

1. __单元测试__：
   - `add_entry` / `recalculate_tokens` / `record_tool_call` 的 token 估算正确性
   - `add_entry` 传入 `metadata.tool_calls` 非空时的 token 计入
   - 配对组切分逻辑（含 `tool_calls` 的 Assistant 条目原子性、连续 Assistant 归组、Summary/Archive 归组边界）
   - 从 `ToolCall` 还原 `ConversationMessage` 的正确性
   - `compress_text` 构造内容正确性（含 `tool_calls` 的 Assistant 条目是否被正确渲染）
   - 纯文本路径中 `metadata.tool_calls` 的渲染
2. __集成测试__：
   - 模拟工具密集型任务，验证 `estimated_tokens` 真实反映工具消耗后压缩能及时触发
   - 压缩触发后配对组完整性
   - 结构化路径与纯文本路径的切换
   - 还原后首次创建 `ToolCallingState` 是否使用 `request.conversation`
   - 硬截断在 `dispatch_system` 还原后做一次裁剪，follow-up 路径不重复裁剪
   - 子 Agent 工具结果不冒泡到父 Agent STM
   - WorkItem 派发路径行为变化：ExperienceCollection 的 conversation 在 First iteration 中生效；
     SkillUpdate / ProfileGeneration 空 conversation 不影响现有行为
3. __实证验证__：用 `logs/harness_2026-08-08_14-59-16.jsonl` 同类任务验证修复效果
