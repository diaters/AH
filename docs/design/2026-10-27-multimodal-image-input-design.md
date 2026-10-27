# 多模态图片输入：让模型真正"看到"图片

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-10-27 |
| 关联 TODO | `docs/TODO.md`「IM 通道后续阶段 / 媒体附件」 |
| 相关文档 | `docs/design/im-channel-adapters.md`、`docs/design/2026-05-24-genai-migration-design.md` |

---

## 1. 背景

### 1.1 现状

- __入站图片已落地为文本标记__：Telegram（`src/channels/telegram.rs:346`）与 QQ
  （`src/channels/qq.rs:466` 起）把用户发来的图片下载到 `telegram_files/`、`qq_files/`，
  然后以 `[IMAGE:<本地路径>]` 文本标记拼进用户消息；QQ 下载失败时标记目标退化为原始 URL。
- __LLM 只能看到标记文字__：`ConversationMessage` 全部为 `String`
  （`src/domain/execution.rs:62`），`src/llm/genai.rs` 的 `build_chat_request` /
  `build_chat_messages` 统一用 `ChatMessage::user(&str)` 发送，图片从未以图片形式进入请求。
- __genai 已具备能力__：`genai 0.6.5` 的 `ContentPart::Binary`（`Binary::from_base64`、
  `Binary::from_url`）可直接构造多模态用户消息，OpenAI / Anthropic / Gemini 等 adapter 均已支持。
  `ToolResponse.content` 仍是纯 `String`，工具结果无法直接携带图片。
- __模型能力差异__：示例配置中的 `deepseek-chat` 等模型不接受图片输入，收到图片会直接报错。

### 1.2 目标

- IM 通道入站图片以真实图片形式交给支持视觉的模型。
- 新增 `view_image` 工具，让 Agent 主动查看本地图片文件（如 shell 生成的截图、下载的图）。
- QQ 移动端逐张发送的纯图片消息，与随后的文字消息合并为一条输入。
- 按模型条目声明视觉能力，不支持视觉的模型（含 fallback 后的模型）行为与现状完全一致。

### 1.3 非目标

- TUI 输入图片（本期不做，用户仍可手打标记，行为同 IM 通道）。
- 音频、视频、PDF 等其他模态。
- 出站附件（已由 `channel_send` + 标记实现，不变）。
- 图片持久化到记忆 / 压缩摘要中保留图片语义（压缩后只剩标记文字，见 §5）。

## 2. 方案总览

核心决策：__领域层继续以 `[IMAGE:...]` 文本标记作为图片的唯一表示，只在 LLM adapter 边界
"晚绑定"为图片 ContentPart。__

```text
IM 通道 ──[IMAGE:path]──┐
                        ├─> STM / prompt / ConversationMessage（全部仍是 String）
view_image ─[IMAGE:path]┘                │
                                         ▼
              execute_with_fallback_logic：按当前模型条目写入 request.vision
                                         │
                                         ▼
          genai::build_chat_request：vision=true 时解析标记 → ContentPart::Binary
                                     vision=false 时原样发送文本（与现状一致）
```

选择理由（对应"统一优先 / 简化优先"）：

- 不改 `ConversationMessage`、STM、记忆压缩、Brain 路由、评估等十余处消费方；
  图片在这些链路里本来就以标记存在，现状已自洽。
- 标记格式复用 `extract_attachments`（`src/domain/attachment.rs`）与出站通道同一套语法，
  不引入第二种图片表示。
- fallback 到非视觉模型时天然降级，无需额外分支。

被否决的备选：把 `ConversationMessage::User.content` 改为 `Vec<ContentPart>`。需要改动
STM 序列化、压缩、prompt 拼接与全部测试构造点，而当前没有任何消费方需要结构化图片，属于过度设计。

## 3. 详细设计

### 3.1 视觉能力声明

`ModelChainEntry`（`src/domain/model_chain.rs:26`）新增字段：

```rust
#[serde(default)]
pub vision: bool,
```

`agents.toml` 示例：

```toml
[[agent.models]]
provider = "openai"
model = "gpt-4.1-mini"
vision = true

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"   # 未声明 vision，默认 false
```

`AgentExecutionRequest`（`src/domain/execution.rs:83`）新增
`#[serde(default)] pub vision: bool`，与 `model_override` 一样由
`execute_with_fallback_logic`（`src/systems/execution.rs:113`）在每次选定模型条目时写入：

```rust
request.model_override = Some(entry.model.clone());
request.vision = entry.vision;
```

因此 fallback 到非视觉模型时，重试请求自动退回纯文本。其他直接构造请求的地方
（`maintenance.rs`、`tool_calling.rs` 等）默认 `false`，行为不变。

### 3.2 adapter 边界的图片解析

新增 `src/llm/image.rs`（`genai.rs` 已 782 行，图片逻辑独立成文件），对外只暴露：

```rust
pub(crate) fn attach_images(task_id: &TaskId, messages: Vec<ChatMessage>) -> Vec<ChatMessage>;
```

在 `build_chat_request` 末尾、`request.vision == true` 时调用。处理规则：

1. __扫描范围__：所有 user 消息（含由 `request.prompt` 生成的当前消息，覆盖
   `build_prompt_with_context` 纯文本拼接路径）和 tool 消息中的 `[IMAGE:target]` 标记。
2. __数量上限__：按出现顺序收集全部图片，只保留最后 `MAX_IMAGES_PER_REQUEST` 张（默认 10，见 §3.4）；
   更早的图片保留原标记文字，等同于现状。
3. __user 消息__：把消息内容改为 `[Text(原文), Binary(img1), Binary(img2)...]`，
   原文保留标记，便于模型把图片与路径对应起来（后续 `channel_send` 回传图片时需要路径）。
4. __tool 消息__：`ToolResponse` 不能带图，在"连续 tool 消息块"之后插入一条合成 user 消息：
   `[Text("[Tool result image] <path>"), Binary(img)]`。插在整块之后而不是每条之后，
   以满足 OpenAI "assistant(tool_calls) 之后必须紧跟全部 tool 消息" 的约束。
5. __图片来源__：
   - 本地路径 / `file://`：读取文件，按扩展名判定 MIME，`Binary::from_base64`。
   - `http(s)://`：`Binary::from_url`，由 provider 拉取（覆盖 QQ 下载失败的退化标记）。
6. __校验与降级__：仅接受 `png / jpeg / gif / webp`；单张超过 `MAX_IMAGE_BYTES`
   （默认 5 MiB，取各主流 provider 的保守下限）、文件不存在或读取失败时，不附图，
   在消息中追加说明文本 `[image unavailable: <target> (<reason>)]`，并记 `warn!(event = "LlmImageSkipped", ...)`。
   任何单张图片失败都不让整个请求失败。

上限以常量实现：`MAX_IMAGES_PER_REQUEST`（`src/llm/image.rs`）与 `MAX_IMAGE_BYTES`、
`image_mime_type`（`src/domain/attachment.rs`，与 `view_image` 共用），本期不暴露配置。

文件读取发生在 executor 的 async 块中，是同步 `std::fs::read`；图片数量与体积已有上限，
沿用 `read_skill_file` 的做法，不额外引入 `spawn_blocking`。

### 3.3 `view_image` 工具

在 `src/systems/tools/builtin/` 新增 `view_image.rs`，按 `ReadSkillFileTool` 模式实现
`BuiltinTool`（`ToolActionKind::Async`），并在 `src/systems/tools/mod.rs` 注册定义：

- 参数：`{ "path": string }`，相对路径基于进程工作目录解析。
- 执行：校验存在、是普通文件、扩展名在白名单内、大小不超过 `MAX_IMAGE_BYTES`
  （与 adapter 共用常量）。
- 返回 `ToolWorkerOutput::Value`（示例见下），工具结果序列化进 STM 后含标记，
  下一轮由 §3.2 规则 4 转换为图片。
- 工具描述明确说明"仅当当前模型支持视觉时才会看到图片内容"。非视觉模型调用该工具
  只会看到 JSON 文本，不会报错。
- 权限：纯读工具，`ToolDefinition.default_permission = Allow`（同 `read_skill_file`），
  agent `[agent.tools]` 可覆盖；读取范围与 `shell_exec` 一致，不新增路径沙箱。

```json
{ "path": "/abs/path/a.png", "bytes": 12345, "image": "[IMAGE:/abs/path/a.png]" }
```

### 3.4 QQ 纯图片消息合并

__问题__：QQ 移动端每张图片单独作为一条消息发出，随后才是文字。当前
`C2C_MESSAGE_CREATE` / `GROUP_AT_MESSAGE_CREATE` 处理（`src/channels/qq.rs:1705` 起）
每条都立即转为 `ChannelInboundMessage`，图片与问题被拆成多个独立 Task 输入。

__规则__：纯图片消息先缓存，等同一发送者的下一条文字消息到达后合并为一条入站消息。

- __纯图片消息判定__：对 `compose_message_content` 的结果用 `extract_attachments` 拆分，
  去掉标记后的文本为空，且附件全部是 `AttachmentKind::Image`。图片 + 文字同条消息
  （如桌面端）不是纯图片消息。
- __文字消息__：去掉标记后文本非空，或带语音转写（语音视作文字）。
  只含文件/视频等非图片附件的消息保持现状直接转发，并顺带合并已缓存的图片。
- __缓存键__：`(recipient, user_openid)`。群聊中不同成员的图片互不串扰。
- __缓存位置__：`QqChannel` 新增 `pending_images: Arc<RwLock<PendingImages>>`
  （`src/channels/qq/pending_images.rs`），内部按键存图片标记字符串，与现有
  `pending_feedback` 同级。纯内存，进程重启丢失。
- __处理位置__：放在白名单检查之后、`pending_feedback` 与审批回复匹配之前。纯图片消息
  入缓存后直接 `continue`，不会被误当成评审反馈或审批回复。
- __合并时机__：只在走到"常规消息"分支时合并。被 `/bind`、评审反馈、审批回复消费的文字
  不触发合并，图片继续等下一条常规消息。
- __合并格式__：`<文字>\n<缓存图片标记...>\n<本条自身附件标记...>`，按到达顺序排列，
  合并后清空该键缓存。
- __等待策略__：不设超时，一直等到下一条文字。每个键最多缓存 20 张，超出时丢弃最早一张，
  记 `warn!(event = "QqPendingImageDropped", ...)`。
- __用户反馈__：缓存期间不回复、不发 typing，避免连发多图刷屏；合并转发时沿用现有 typing。
- 下载仍在 `compose_message_content` 中即时完成，缓存里存的是本地路径标记，
  不受 QQ 临时 URL 过期影响。

合并后的消息可能携带较多图片，因此 §3.2 的 `MAX_IMAGES_PER_REQUEST` 默认值调整为 __10__。

Telegram 已在单条消息中携带图片与 caption，本期不做合并。

### 3.5 日志

按 `docs/logs.md` 规范新增：

- `LlmImagesAttached`（debug）：`task_id`、`attached`、`dropped_by_limit`。
- `LlmImageSkipped`（warn）：`task_id`、`target`、`reason`。

图片 base64 内容不得进入任何日志。

## 4. 测试计划

单元测试（`src/llm/image.rs`、`view_image.rs` 内 `#[cfg(test)]`）：

- `vision=false`：消息原样不变。
- user 消息单图 / 多图转换为 Text + Binary，原文保留。
- 超出 `MAX_IMAGES_PER_REQUEST` 时只保留最后 N 张，前面保持标记文本。
- 连续多条 tool 消息含图：合成 user 消息插在整块 tool 消息之后。
- 文件不存在、超大、非白名单扩展名：降级文本 + 不 panic。
- `http(s)` 目标走 `from_url`。
- `view_image`：正常返回、缺参、不存在、超大、非图片扩展名。
- QQ 合并（`PendingImages` 单元测试；网关监听循环无现成测试设施，只验证合并规则本身）：
  - 单图 + 文字、多图 + 文字合并为一条，图片顺序与到达顺序一致。
  - 图片 + 文字同条消息直接转发，不进入缓存。
  - 群聊两名成员交错发图，各自只合并自己的图片。
  - 缓存中有图片时收到审批回复 / 评审反馈：不合并，图片保留到下一条常规消息。
  - 超过 20 张丢弃最早一张。

集成测试（`tests/`）：

- `ModelChainEntry` 反序列化 `vision` 缺省为 `false`。
- fallback 链 `[vision=true, vision=false]`：首个模型失败降级后，第二次请求 `vision=false`
  （用 mock executor 断言收到的 `AgentExecutionRequest.vision`）。

## 5. 已知限制

- 记忆压缩后历史图片只剩标记文字，摘要器不看图（summarizer 通常不声明 vision）；
  长对话早期的图片语义会丢失，属可接受取舍。
- 本地图片在每次请求时重新读取并 base64，`telegram_files/`、`qq_files/` 中的文件被清理后
  历史图片降级为文本。
- `MAX_IMAGES_PER_REQUEST` 内的历史图片每轮重复计费 token，由上限控制成本。
- QQ 纯图片缓存不设超时：用户发图后隔很久才发的无关文字也会与这些图片合并；
  缓存为纯内存，进程重启后未合并的图片丢失（本地文件仍在 `qq_files/`）。
- Brain 决策请求若其模型声明了 vision，也会收到图片；保持统一规则，不对 Brain 特殊处理。

## 6. 文档同步

实施时同步更新：

- `docs/configuration.md`：`[[agent.models]].vision` 说明。
- `agents.toml.example`：示例条目加 `vision = true`。
- `docs/current-state.md`、`README.md`：已实现能力增加"图片输入（IM 入站 + `view_image`）"。
- `docs/design/im-channel-adapters.md`：入向图片小节补充"支持视觉的模型将收到真实图片"，
  QQ 小节补充纯图片消息合并规则。
- `docs/TODO.md`：媒体附件项更新进度；TUI 图片输入列为后续项。
- `docs/design/README.md`：索引新增本文档。

## 7. 实施步骤

1. `ModelChainEntry.vision` + `AgentExecutionRequest.vision` + fallback 写入，附测试。
2. `src/llm/image.rs` 解析与降级逻辑（TDD），接入 `build_chat_request`。
3. `view_image` 工具实现与注册，附测试。
4. QQ 纯图片消息合并（§3.4），附测试。
5. 文档同步（§6），跑 CI 四项检查。
