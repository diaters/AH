# 摘要压缩无限循环修复设计

> __状态：当前有效__

| 属性 | 值 |
|------|-----|
| 创建日期 | 2026-08-16 |
| 问题等级 | P0（持续消耗 LLM 调用并向通道刷系统通知） |
| 证据日志 | `logs/harness_2026-08-15_23-56-36.jsonl`（L540-L747） |
| 相关文档 | `2026-08-09-context-compression-blind-spot-fix.md`、`docs/logs.md` |

## 1. 背景与现象

用户通过 QQ 发起新闻查询任务（browser-operator 使用 playwright 工具产出约 71k tokens 的
工具调用记录），随后执行 `/skill` 命令。此后系统陷入无限摘要循环：

```text
CompressionTriggered (72k > 8000, entries_to_compress=1, compress_text_len=78)
  → Summarization WorkItem（summarizer LLM 调用）
  → SummarizationCompleted (removed_entries=0, new_tokens≈72k 不变)
  → 任务恢复 Waiting(User)
  → 下一帧再次 CompressionTriggered
  → ……无限循环，每轮向 QQ 推送一条"📝 摘要完成"系统通知
```

在 16:00:02 至 16:01:09 的约 67 秒内触发了 9 轮摘要请求，且存在多轮摘要
WorkItem 并发在飞的情况（如 L644-L646 与 L658 两次派发时间交叠）。

## 2. 根因分析

四个代码点共同作用形成循环（行号以当前代码为准）：

### 2.1 触发端：大 token 工具组被保护窗口锁定

- `src/systems/memory.rs:32`：带 `tool_calls` 的 Assistant entry 独立成组
  （配对组，保证 tool_call ID 链不可拆散）。
- `src/systems/memory.rs:76-81`：保护窗口按组数保留最后
  `preserve_recent_turns`（默认 2）个组。

日志中 STM 的 4 个 entry 分组为 `[[User(78字符)], [Assistant(71k 工具组)],
[User+Assistant(最近对话)]`，共 3 组。保留最后 2 组后，71k 的工具组落在保护窗口内
永远不会被压缩，每次只选中 78 字符的组 0——与日志字段
`groups_total=3, groups_to_compress=1, entries_to_compress=1, compress_text_len=78`
完全吻合。

### 2.2 完成端：移除粒度与触发端不对齐，导致 removed=0

- `src/systems/transform/llm_response.rs:537-544`：摘要完成后按
  `preserve_recent_turns * 2`（entry 数，=4）drain，而当前 `entries.len() = 4`，
  `4 > 4` 不成立，走 `removed = 0` 分支。

即使数值巧合满足，按 entry 数 drain 也无法对应触发端按组选中的压缩集合，
两端粒度错位。entries 一条不移除、`summary_prefix` 被同量级摘要替换，
`recalculate_tokens()`（`src/domain/memory.rs:401-423`）重算后 tokens 几乎不变。

### 2.3 触发缺口：Waiting(User) 不在跳过列表

- `src/systems/memory.rs:58-66`：跳过条件只含任务终态与
  `Waiting(Summarization)`。
- `src/systems/transform/llm_response.rs:579`：摘要完成后任务恢复
  `Waiting(User)`，下一帧（系统注册于 `HarnessSet::Maintenance`，每帧执行，
  见 `src/plugins/memory.rs:35`）触发条件 `72k > 8000` 仍成立，立即再次触发。

### 2.4 并发在飞：工具 follow-up 恢复 Running 绕过等待保护

日志 L578 显示工具循环 follow-up 会把任务从 `Waiting(Summarization)` 标回
`Running`（`mark_running`），此时上一轮摘要 WorkItem 尚未完成，
`memory_compression_system` 因任务不再处于 `Waiting(Summarization)` 而再次触发，
产生并发在飞的多轮摘要 LLM 调用（纯浪费）。

## 3. 设计目标

- 摘要每轮必有进展：完成端移除的 entries 与触发端选中的压缩集合同粒度对齐，
  循环必然收敛终止。
- 消除并发在飞的重复摘要请求。
- 不改变既有语义：`preserve_recent_turns` 保护窗口、配对组原子性、
  阈值触发条件均保持不变。
- 不扩大修复面：保护窗口内大工具组继续保持受保护（随对话轮转自然落出窗口后被压缩），
  这是 `2026-08-09-context-compression-blind-spot-fix.md` 既定的语义，本设计不重构窗口策略。

## 4. 方案

### 4.1 变更点 1：`split_into_groups` 下沉为共享函数

将 `split_into_groups` 从 `src/systems/memory.rs:22-48` 移至
`src/domain/memory.rs` 并声明为 `pub(crate)`（或作为 `ShortTermMemory` 的关联函数），
触发端与完成端共用同一份分组逻辑，保证粒度对齐不会再次漂移
（统一优先：一处定义，两处消费）。

### 4.2 变更点 2：完成端按组重算并 drain 已压缩 entries

修改 `handle_summarization_work_item_result`
（`src/systems/transform/llm_response.rs:536-544`），替换现有
`preserve_recent_turns * 2` entry 数 drain 逻辑：

```rust
// 与触发端 memory_compression_system 完全同构的选择逻辑
let groups = split_into_groups(&memory.entries);
let preserve_group_count = config.preserve_recent_turns as usize;
let compress_entry_count = groups
    .iter()
    .take(groups.len().saturating_sub(preserve_group_count))
    .map(|g| g.len())
    .sum::<usize>();

let removed = if compress_entry_count > 0 {
    memory.entries.drain(0..compress_entry_count);
    compress_entry_count
} else {
    0
};
memory.recalculate_tokens();
```

要点：

- 完成时基于__当前__ entries 重算分组，而非回放触发时的快照。摘要执行期间若有新
  entry 追加（如用户输入），重算天然兼容，不会误删新近条目。
- 每轮完成至少 drain 1 个 entry（触发端保证 `compress_entry_count > 0` 才发起请求），
  entries 单调减少，最终 `groups.len() <= preserve_recent_turns` 或
  `tokens <= threshold`，循环必然终止。
- 根因 2.3 的触发缺口（`Waiting(User)` 可触发压缩）__有意保留__：空闲任务超阈值时
  触发压缩是正常语义，问题不在"能触发"而在"触发了却不收敛"。终止性由
  "每轮必有进展"保证，无需收紧触发条件。
- 日志事件 `SummarizationCompleted` 新增 `drained_groups` 字段（可选），保留
  `removed_entries` 语义不变。

### 4.3 变更点 3：摘要请求在飞保护

`memory_compression_system`（`src/systems/memory.rs:51`）新增 Query 参数检索
WorkItem：若该 task 已存在未终态的 `WorkItemType::Summarization` WorkItem，
跳过触发。消除 2.4 描述的并发在飞浪费。

### 4.4 边界情况

- __摘要失败 / 非文本输出__：现有代码已恢复任务状态避免卡死
  （`llm_response.rs:590-613`），entries 不 drain、tokens 不变，配合变更点 3
  不会产生重复请求；任务继续可交互。本设计不改动该路径。
- __多轮在飞摘要的重复完成__（修复过渡期或极端竞态下）：每轮完成端独立重算，
  第二轮完成时 `compress_entry_count` 通常已为 0，drain 空集，仅覆盖
  `summary_prefix`，无副作用。
- __修复后 71k 工具组的归宿__：修复后首轮 drain 掉组 0，剩余 2 组
  `<= preserve_recent_turns`，触发停止，STM 暂保持 ~72k。用户下一轮对话后
  该工具组落出保护窗口，被正常压缩为摘要。这是保护窗口的既定语义
  （保最近交互的 ID 链完整），不是缺陷。

## 5. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/domain/memory.rs` | 修改 | 新增 `pub(crate) fn split_into_groups`（自 systems 迁入） |
| `src/systems/memory.rs` | 修改 | 删除本地 `split_into_groups` 改用共享版本；新增在飞 WorkItem 保护 |
| `src/systems/transform/llm_response.rs` | 修改 | 完成端 drain 逻辑对齐组粒度 |

## 6. 验证方案

### 6.1 单元测试

- `split_into_groups` 迁移后行为不变（纯文本组、工具组独立、Summary/Archive 归组）。
- drain 对齐：构造 `[[User], [Assistant+tool_calls(大)], [User, Assistant]]` 的
  STM，执行完成端逻辑，断言 drain 恰好移除组 0 的 entry、`removed=1`、
  `recalculate_tokens` 后 tokens 相应下降。
- 在飞保护：task 存在未终态 Summarization WorkItem 时不再 spawn
  `SummarizationRequestMessage`。

### 6.2 集成测试

复现日志场景（沿用项目约定的轮询等待模式，避免固定 sleep）：

- 构造 token 超阈值且含大工具组的 STM → 断言仅产生一轮摘要请求，完成后
  `entries` 减少、无后续 `CompressionTriggered`。
- 继续追加一轮用户对话 → 断言大工具组随后被压缩，tokens 降至阈值以下。

## 7. 风险与边界

- `handle_summarization_work_item_result` 中"全局单 STM"假设
  （`llm_response.rs:530-532` 注释自认）为既有债务，本设计不扩大修复面，
  仅在既有假设内对齐粒度。
- 本修复不解决"保护窗口内大工具组持续占用上下文"问题，该方向由
  `2026-08-09-context-compression-blind-spot-fix.md` 的后续迭代承载
  （如 token 预算窗口），属独立设计议题。
