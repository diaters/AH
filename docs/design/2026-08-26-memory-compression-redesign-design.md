# 短期记忆压缩重设计：一次全压与结构性收敛

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-08-26 |
| 关联 TODO | `docs/TODO.md` 中优先级两项：`MemoryConfig` token 压缩阈值配置入口；token 压缩触发过迟排查 |
| 相关文档 | `docs/design/2026-08-09-context-compression-blind-spot-fix.md`、`docs/design/multi-turn-memory-design.md`、`logs/bugs/2026-08-15-skill-context-compression-loop.md` |

---

## 1. 背景

### 1.1 待办原文

- __[ ] 为 `MemoryConfig` 的 token 压缩阈值提供配置入口__，避免硬编码 `8000`
  （`compression_threshold_tokens` / `preserve_recent_turns` / `summary_target_tokens`
  当前仅由 `src/domain/memory.rs:35` 的 `Default` 提供，生产路径
  `src/plugins/task_runtime.rs:27` 用 `init_resource` 无配置文件覆盖）
- __[ ] 排查 token 压缩触发过迟问题__：实测 `CompressionTriggered` 时 `current_tokens: 86351`
  远超阈值 `8000`（10 倍以上），`groups_to_compress` 仅压 1/3、`compress_text_len: 260`，
  压缩生效明显滞后

### 1.2 现状

#### 配置链路缺失

- `MemoryConfig` 是独立 Resource（`src/domain/memory.rs:22-40`），三个字段仅由 `Default`
  提供（`8000 / 2 / 1000`）。
- 生产装配 `src/plugins/task_runtime.rs:27` 用 `app.init_resource::<MemoryConfig>()`，
  无任何配置来源覆盖。
- 唯一运行时配置载体是 `HarnessConfig`（`src/systems/runtime_config.rs`，env 驱动），
  其中没有 memory 字段。两者零关联，生产路径的 `compression_threshold_tokens` 恒为 `8000`。

#### 压缩选择逻辑失效

当前 `src/domain/memory.rs` 的 `compressible_entry_count` 按「组数」做保护窗口：

- 保留最后 `preserve_recent_turns`（默认 2）个配对组后，其余组整体压缩；
- 组数不足时返回 `0`，不压缩。

失效机理（日志实证，见 1.3）：

1. __压缩不削减成本__：多次 `SummarizationCompleted` 显示 `removed_entries > 0` 但
   `new_tokens` 几乎不变，甚至不降反升（`47940→47977`）。根因：被压缩组 token 很小，
   `compress_text_len:78`（仅一个 78 字符小片段），而超大内容所在的组位于保护窗口内，
   永不被压缩。
2. __触发严重滞后__：`current_tokens:86351 ≫ 8000` 才首次触发，因为阈值固定 8000，
   一次大工具组即可超出，而保护窗口使其压不掉。
3. __无收敛判定__：压缩后仍远超阈值时无熔断，摘要在每个状态切换后重新触发
   （8-15 发生十几次连续循环，见相关 bug 文档）。

### 1.3 证据摘要

- 日志 `logs/harness_2026-08-25_11-19-45.jsonl`：
  `CompressionTriggered current_tokens:124127 threshold:8000 groups_total:3
  groups_to_compress:1 entries_to_compress:1 compress_text_len:260` →
  `SummarizationCompleted removed_entries:1, new_tokens:124088`（几乎无下降）。
- 另一段：压缩前 `estimated_tokens:47940` → 压缩后 `47977`（反而增加）。

---

## 2. 设计目标

- __配置入口__：`MemoryConfig` 的阈值可经 env 覆盖，不再硬编码。
- __一次配全__：阈值触发时把「全部历史配对组」一次性压成一份有界摘要，不再做「选择哪几组」。
- __结构性保障__：收敛性由「成本单调下降」+「可压缩组集合随帧单调减少」保证，不依赖状态机。
- __保持既有约束__：配对组原子性（含 `tool_calls` 的 Assistant 不拆散）不变；
  `estimate_tokens` 计入 tool input/output 不变；`summary_prefix` 仍为有界前缀。

---

## 3. 核心设计

### 3.1 `MemoryConfig` 并入 `HarnessConfig`

- `HarnessConfig` 增加 `memory: MemoryConfig` 字段（`src/systems/runtime_config.rs`）。
- `from_env()` 读取：
  `HARNESS_MEMORY_COMPRESSION_THRESHOLD_TOKENS`（默认 8000）、
  `HARNESS_MEMORY_SUMMARY_TARGET_TOKENS`（默认 1000）。
- `MemoryConfig` 删除 `preserve_recent_turns` 字段（其语义由 3.2 节取代）。
- `build_harness_app`（`src/app/mod.rs`）装配期 `app.insert_resource(config.memory.clone())`；
  `TaskRuntimePlugin::init_resource::<MemoryConfig>()` 保留为兼容兜底
  （Bevy `insert_resource` 已存在则不覆盖）。
- 同步更新 `docs/configuration.md` 与 `.env.example` 增加 2 个环境变量说明。

### 3.2 可压缩组的选择：去掉「组数保护窗口」

新协议：__阈值触发时，把「全部历史组」整体压成一份有界摘要__。

- 保留最后 1 个「进行中」的组（正在交互的对话/工具组）；
- 其余组（全部历史）一次全压——不存在「选哪几组」的排序，选择集是确定的。
- `preserve_recent_turns` 字段删除；`MemoryConfig` 只保留两个字段：
  `compression_threshold_tokens`、`summary_target_tokens`。

### 3.3 结构函数（`src/domain/memory.rs`）

```rust
/// 返回可压缩组：除最后 1 个进行中组外的全部组。
/// 全部组（1 个组）时返回空。
fn compressible_group_indices(entries: &[MemoryEntry]) -> Vec<Vec<usize>>
```

- 采用既有 `split_into_groups` 切分；`groups.len() <= 1` → 空（无可压）；
- 否则返回 `groups[..len - 1]` 的全部组。

```rust
/// 触发端：构造压缩输入（旧 summary + 全部可压组的拼接）。
/// render_tool_calls 用于将分组内 Assistant 条目的 tool_calls 渲染为文本，
/// 与完成端使用同一份 compressible_group_indices 选择逻辑。
fn build_compress_text(
    entries: &[MemoryEntry],
    summary_prefix: Option<&str>,
    render_tool_calls: impl Fn(&[ToolCall]) -> String,
) -> String

/// 完成端：移除全部可压组并计数，内部自行 recalculate_tokens()。
fn drain_compressible_groups(&mut self) -> usize
```

- `drain_compressible_groups` 内部先调用 `compressible_group_indices` 得到集合，
  `entries.drain(0..k)`（凸前缀，安全），再 `recalculate_tokens()`；
  不再需要 `config` 参数（保护窗口语义已由「仅保留最后 1 组」取代）。

### 3.4 触发端（`src/systems/memory.rs`）

```rust
if short_term.estimated_tokens > config.compression_threshold_tokens {
    let groups = compressible_group_indices(&short_term.entries);
    if groups.is_empty() { continue; }   // 无可压 → 不触发
    let text = build_compress_text(
        &short_term.entries,
        short_term.summary_prefix.as_deref(),
        render_tool_calls_summary,
    );
    // 一次性发起 SummarizationRequestMessage（包含 text + target_tokens）
}
```

- 删除旧的多段选择 + 循环重压逻辑；删除 `compressible_entry_count` 与「组数不足时退化 0」。
- 摘要在「无可压缩组」时自然停止（组数不足 / 全部为进行中组），不循环。

### 3.5 完成端（`src/systems/summarization.rs`）

```rust
memory.summary_prefix = Some(summary.clone());   // 覆盖式：旧 summary 不再保留
let removed = memory.drain_compressible_groups(); // 内部 recalculate_tokens()
```

- `summary_prefix` 改为覆盖式：每轮把「旧摘要 + 全部历史」重新生成一份新摘要替换旧值，
  该字段恒有界（≤ `summary_target_tokens` 对应量级）。
- 摘要长度异常长时不做硬截断（沿用现状，交由 provider 窗口 / dispatch 截断兜底）。

---

## 4. 收敛性论证

- __每次压缩成本严格下降__：压掉全部可压缩组，把其成本替换为一份 ≤ `summary_target_tokens`
  的摘要，`estimated_tokens` 至多降到 `最后 1 组的成本 + summary 成本`，严格小于压前总值。
- __每轮触发减少__：压缩后若帧间无新 User 组进入，可压组为空（只剩最后 1 组）
  → 不可再触发。只有新对话加入产生新的被保护组 → `estimated_tokens` 再次超阈值时才触发，
  不会死循环。
- **最终上界**：`entries = 最后 1 组`，`estimated_tokens ≈ last_group + summary_target`。
  若最后 1 组本身超大，不压缩——语义正确（进行中的对话不强制删除）；
  下一条新消息把它推入「历史组」后才压缩。

---

## 5. 边界场景

| 场景 | 行为 |
|------|------|
| 仅 1 组（进行中）且超阈值 | 无可压 → 不触发，等新消息把它挤出保护窗口 |
| 超大工具组在历史区 | 整体压掉（提出 output 计成本，收益最大） |
| 摘要输出异常长 | 现状兜底：provider 窗口与 dispatch 截断；不做摘要硬断 |
| 连续触发 | 每轮压缩后只剩 1 组 → 无可压 → 停止，不循环 |
| 子 Agent / chat_with_agent | 沿用配对组语义，工具交互不冒入父 STM |

---

## 6. 关键改动点

| # | 改动 | 位置 |
|---|------|------|
| 1 | `HarnessConfig` 增 `memory` 字段 + 2 个 env | `src/systems/runtime_config.rs` |
| 2 | 装配期 `insert_resource(MemoryConfig)` | `src/app/mod.rs` |
| 3 | `MemoryConfig` 删 `preserve_recent_turns` | `src/domain/memory.rs` |
| 4 | `compressible_entry_count` → `compressible_group_indices` | `src/domain/memory.rs` |
| 5 | 新增 `build_compress_text`/`drain_compressible_groups` | `src/domain/memory.rs` |
| 6 | 触发端改用新 API，删除二次重压 | `src/systems/memory.rs` |
| 7 | 完成端用新函数 + 覆盖式前缀 | `src/systems/summarization.rs` |
| 8 | 更新测试引用 | `src/**/*.rs` 测试 |

影响面：`src/domain/memory.rs`、`src/systems/memory.rs`、`src/systems/summarization.rs`、
`src/systems/runtime_config.rs`、`src/app/mod.rs` + 配置文档 + 测试。

---

## 7. 风险与缓解

| 风险 | 缓解 |
|------|------|
| 行为变化：默认阈值保持 8000 不变 | 新行为只在「存在可压缩的非进行中组」时触发，收敛性结构性保证 |
| summary 覆盖式丢弃旧摘要细节 | 语义收敛：旧摘要被新生成的综合摘要取代，属性预期行为 |
| 既有测试（`memory.rs` 的 `compressible_entry_count`/`drain_compressed_groups`）失效 | 预期：按新语义重写（见 §8 验证) |
| `truncate_conversation_by_budget(100_000)` 与压缩阈值是两条预算轨道 | 本次不动，标注为后续项（§9） |

---

## 8. 验证策略

1. 单元测试（`domain/memory.rs`）：
   - `compressible_group_indices`：1 组空 / 多组除最后 1 组 / 含工具组在目标中（不拆散）
   - `drain_compressible_groups`：压后 `entries = 最后 1 组`，`estimated_tokens` 收敛（内部自算）
   - `build_compress_text` 结构与内容正确
2. 系统测试：
   - 触发端：超大数组一次命中且仅发 1 次 SummaryRequest
   - 完成后 `entries` 只剩最后 1 组，`new_tokens <= last_group + summary_target`
   - 仅 1 组超阈值不触发
   - 无「removed > 0 但 new_tokens 不下降」的反例断言
3. 回归：`cargo test --all-features`、`cargo clippy --all-targets --all-features -- -D warnings`、
   `cargo fmt --all --check`（CI 等价）
4. 手工：以 8-15 日志场景复跑，观察 `new_tokens` 单调下降至阈值下

---

## 9. 后续项（不在本次范围）

- `truncate_conversation_by_budget(100_000)` 的预算与 `summary_target` 的关系（硬截断兜底，
  本次不动，后续评估统一）。
- 摘要轮次与 prompt cache 调优（跨轮摘要触发时的缓存行为）。