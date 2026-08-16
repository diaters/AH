# 摘要压缩无限循环修复实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 修复摘要压缩无限循环——完成端按配对组粒度 drain 已压缩 entries，并增加在飞摘要保护，使压缩循环必然收敛。

**架构：** 将 `split_into_groups` 与新增的 `compressible_entry_count` 下沉到 `src/domain/memory.rs` 作为触发端与完成端共享的选择逻辑；`ShortTermMemory` 新增 `drain_compressed_groups` 方法供完成端调用；`memory_compression_system` 增加在飞 Summarization WorkItem 检查。

**技术栈：** Rust + Bevy ECS（`run_system_once` 世界级测试模式，参照 `src/systems/experience/approval.rs:383`）

**设计文档：** `docs/design/2026-08-16-summarization-loop-fix.md`

**验证命令：** `cargo test --all-features`（每个任务后运行相关子集）、最终运行 `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`

---

## 文件结构

| 文件 | 职责 |
|------|------|
| `src/domain/memory.rs` | 新增共享纯函数 `split_into_groups`、`compressible_entry_count` 与方法 `ShortTermMemory::drain_compressed_groups`（含单元测试） |
| `src/systems/memory.rs` | 删除本地 `split_into_groups`，改用共享版本；`memory_compression_system` 增加在飞保护（含世界级测试） |
| `src/systems/transform/llm_response.rs` | `handle_summarization_work_item_result` 的 drain 逻辑替换为 `drain_compressed_groups` |
| `tests/context_compression_flow.rs` | 新增循环终止性串联测试 |

---

### 任务 1：共享分组函数下沉到 domain 层

**文件：**
- 修改：`src/domain/memory.rs`（在 `impl ShortTermMemory` 块之前添加两个函数；文件尾部添加测试模块条目）
- 修改：`src/systems/memory.rs:15-48`（删除本地 `split_into_groups`）

- [ ] **步骤 1：编写失败的测试**

在 `src/domain/memory.rs` 文件尾部添加（若已有 `#[cfg(test)] mod tests` 则并入，否则新建）：

```rust
#[cfg(test)]
mod split_into_groups_tests {
    use super::*;

    /// 复现日志 harness_2026-08-15_23-56-36.jsonl 的分组场景：
    /// [User(78字符), Assistant(含大 tool_calls), User, Assistant] → 3 组
    fn log_scenario_stm() -> ShortTermMemory {
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, "帮我看今天的新闻", EntryMetadata::default());
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "playwright-cli browse".to_string(),
            output: "huge news page content".repeat(2000),
            timestamp: chrono::Utc::now(),
        });
        stm.add_entry(EntryRole::Assistant, String::new(), metadata);
        stm.add_entry(EntryRole::User, "总结一下", EntryMetadata::default());
        stm.add_entry(EntryRole::Assistant, "好的", EntryMetadata::default());
        stm
    }

    #[test]
    fn split_into_groups_tool_entry_forms_own_group() {
        let stm = log_scenario_stm();
        let groups = split_into_groups(&stm.entries);
        assert_eq!(groups, vec![vec![0], vec![1], vec![2, 3]]);
    }

    #[test]
    fn compressible_entry_count_protects_recent_groups() {
        let stm = log_scenario_stm();
        let groups = split_into_groups(&stm.entries);

        // preserve=2（默认）：保留工具组与最近对话组，仅组 0 可压缩（日志中的 78 字符）
        assert_eq!(compressible_entry_count(&groups, 2), 1);
        // preserve=1：工具组落入压缩区
        assert_eq!(compressible_entry_count(&groups, 1), 2);
        // 组数 <= preserve：无可压缩
        assert_eq!(compressible_entry_count(&groups, 3), 0);
        assert_eq!(compressible_entry_count(&groups, 4), 0);
        // 空分组
        assert_eq!(compressible_entry_count(&[], 2), 0);
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib split_into_group`
预期：编译错误，`cannot find function split_into_groups in this module`（函数尚未迁移）

- [ ] **步骤 3：迁移函数到 domain 层**

将 `src/systems/memory.rs:15-48` 的 `split_into_groups` 整体（含文档注释）剪切到 `src/domain/memory.rs` 的 `impl ShortTermMemory` 块之前，可见性从 `fn` 改为 `pub(crate) fn`；紧随其后新增：

```rust
/// 计算可压缩的 entry 数量：排除最后 `preserve_recent_turns` 个配对组后，
/// 前置各组包含的 entry 总数。组数不足时返回 0。
///
/// 触发端（`memory_compression_system`）与完成端
/// （`ShortTermMemory::drain_compressed_groups`）共用此选择逻辑，
/// 保证两端粒度对齐。
pub(crate) fn compressible_entry_count(
    groups: &[Vec<usize>],
    preserve_recent_turns: u32,
) -> usize {
    let preserve = preserve_recent_turns as usize;
    if groups.len() <= preserve {
        return 0;
    }
    groups
        .iter()
        .take(groups.len() - preserve)
        .map(|g| g.len())
        .sum()
}
```

同时删除 `src/systems/memory.rs` 中被剪走的定义，并在其 `use crate::{...}` 的 domain 导入中追加 `split_into_groups`（若 `src/domain/mod.rs` 使用非通配导出，需在该文件补 `pub use memory::{split_into_groups, compressible_entry_count};`——先确认现有导出方式）。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib split_into_group`
预期：2 个测试 PASS

运行：`cargo build` 确认 `systems/memory.rs` 引用编译通过

- [ ] **步骤 5：Commit**

```bash
git add src/domain/memory.rs src/systems/memory.rs
git commit -m "refactor: 将 split_into_groups 下沉为 domain 层共享函数并新增 compressible_entry_count"
```

---

### 任务 2：`drain_compressed_groups` 方法与完成端接入

**文件：**
- 修改：`src/domain/memory.rs`（`impl ShortTermMemory` 内新增方法）
- 修改：`src/systems/transform/llm_response.rs:536-544`

- [ ] **步骤 1：编写失败的测试**

在任务 1 新增的 `split_into_group_tests` 模块中追加：

```rust
#[test]
fn drain_compressed_groups_removes_only_leading_groups() {
    let mut stm = log_scenario_stm();
    let tokens_before = stm.estimated_tokens;

    // 摘要完成后 drain（preserve=2 默认配置）：仅移除组 0 的 1 个 entry
    let removed = stm.drain_compressed_groups(2);
    assert_eq!(removed, 1);
    assert_eq!(stm.entries.len(), 3);
    assert!(stm.estimated_tokens < tokens_before);

    // 二次 drain：保护窗口已满，无进展 → 触发端将停止，循环终止
    assert_eq!(stm.drain_compressed_groups(2), 0);
    assert_eq!(stm.entries.len(), 3);
}

#[test]
fn drain_compressed_groups_then_next_turn_exposes_tool_group() {
    // 终止后用户追加一轮对话，工具组落出保护窗口，可被后续压缩
    let mut stm = log_scenario_stm();
    stm.drain_compressed_groups(2);

    stm.add_entry(EntryRole::User, "新的问题", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "新的回答", EntryMetadata::default());

    // 此时分组 [[工具组], [对话组], [新对话组]]：工具组成为可压缩区
    let removed = stm.drain_compressed_groups(2);
    assert_eq!(removed, 1);
    assert!(stm.entries.iter().all(|e| e.metadata.tool_calls.is_empty()));
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib drain_compressed_groups`
预期：编译错误，`no method named drain_compressed_groups`

- [ ] **步骤 3：实现方法**

在 `src/domain/memory.rs` 的 `impl ShortTermMemory` 中（`recalculate_tokens` 方法之后）添加：

```rust
/// 摘要完成后移除已压缩的 entries。
///
/// 与触发端 `memory_compression_system` 使用同一份
/// `split_into_groups` + `compressible_entry_count` 选择逻辑：
/// 移除的是被压缩组的前置 entries，保留最后 `preserve_recent_turns`
/// 个配对组。返回实际移除的 entry 数。
pub fn drain_compressed_groups(&mut self, preserve_recent_turns: u32) -> usize {
    let groups = split_into_groups(&self.entries);
    let count = compressible_entry_count(&groups, preserve_recent_turns);
    if count > 0 {
        self.entries.drain(0..count);
    }
    count
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib drain_compressed_groups`
预期：2 个测试 PASS

- [ ] **步骤 5：完成端接入**

修改 `src/systems/transform/llm_response.rs:536-544`，将：

```rust
                    // 移除已压缩的 entries（保留最近 N 轮）
                    let preserve_count = (config.preserve_recent_turns * 2) as usize;
                    let removed = if memory.entries.len() > preserve_count {
                        let removed = memory.entries.len() - preserve_count;
                        memory.entries.drain(0..removed);
                        removed
                    } else {
                        0
                    };
```

替换为：

```rust
                    // 移除已压缩的 entries：与触发端 memory_compression_system
                    // 共用配对组选择逻辑（见 domain::memory 的
                    // split_into_groups / compressible_entry_count），
                    // 保证压缩循环每轮必有进展、必然收敛
                    let removed = memory.drain_compressed_groups(config.preserve_recent_turns);
```

（`recalculate_tokens` 调用与其后日志保持不变。）

- [ ] **步骤 6：运行测试与编译验证**

运行：`cargo test --lib -- memory:: drain_compressed`
预期：PASS

运行：`cargo test --test summarization_flow`
预期：既有测试全部 PASS（无回归）

- [ ] **步骤 7：Commit**

```bash
git add src/domain/memory.rs src/systems/transform/llm_response.rs
git commit -m "fix: 摘要完成后按配对组粒度 drain 已压缩 entries，修复压缩无限循环"
```

---

### 任务 3：在飞摘要保护

**文件：**
- 修改：`src/systems/memory.rs`（`memory_compression_system` 签名与逻辑；测试模块追加世界级测试）

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/memory.rs` 的 `#[cfg(test)] mod tests` 中追加：

```rust
    #[test]
    fn compression_skips_when_summarization_workitem_inflight() {
        use crate::domain::{WorkItem, WorkItemType, WorkItemStatus};

        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
            preserve_recent_turns: 1,
            summary_target_tokens: 50,
        });

        let mut task = Task::from_user_input(
            "test",
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
        );
        let task_id = task.id;
        let _entity = world.spawn((task, ShortTermMemory::default())).id();
        {
            let mut stm = world.get_mut::<ShortTermMemory>(_entity).unwrap();
            for i in 0..10 {
                stm.add_entry(
                    EntryRole::User,
                    format!("This is message number {} with some content", i),
                    Default::default(),
                );
            }
        }

        // 该 task 已有一个在飞的 Summarization WorkItem
        let mut wi = WorkItem::summarization(
            task_id,
            "compress".to_string(),
            100,
            crate::domain::SummarizationTrigger::TokenThreshold,
        );
        wi.status = WorkItemStatus::Running;
        world.spawn(wi);

        let _ = world.run_system_once(super::memory_compression_system);

        let requests = world
            .query::<&crate::domain::SummarizationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            requests, 0,
            "must not trigger a new summarization while one is inflight"
        );
    }
```

注意：若 `WorkItem::summarization` 工厂签名与上述调用不符（见 `src/domain/work_item.rs:204`），以实际签名为准调整参数；测试语义不变——spawn 一个 `work_type == WorkItemType::Summarization && status 非终态 && task_id 匹配` 的 WorkItem。若工厂不设置 `task_id`/`status`，spawn 后直接对组件字段赋值即可（参照 `tests/experience_collection_workitem_flow.rs:128-129` 的直接赋值模式）。

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib compression_skips_when_summarization_workitem_inflight`
预期：FAIL，`requests` 为 1（当前无在飞保护，会 spawn 请求）

- [ ] **步骤 3：实现在飞保护**

修改 `src/systems/memory.rs` 的 `memory_compression_system`：

签名（L51-55）追加一个 Query 参数：

```rust
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut commands: Commands,
    tasks: Query<(&Task, &ShortTermMemory)>,
    work_items: Query<&WorkItem>,
) {
```

并在循环体内、`if short_term.estimated_tokens > config.compression_threshold_tokens` 判定通过之后（L69 之后、L71 `let groups` 之前）插入：

```rust
            // 在飞保护：该 task 已有未完成的 Summarization WorkItem 时不重复触发，
            // 避免 follow-up 将任务标回 Running 后产生并发摘要
            let has_inflight = work_items.iter().any(|wi| {
                wi.work_type == WorkItemType::Summarization
                    && wi.task_id == task.id
                    && !wi.status.is_terminal()
            });
            if has_inflight {
                continue;
            }
```

同时更新文件头部 `use crate::{...}` 导入，追加 `WorkItem, WorkItemType`。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib compression_skips_when_summarization_workitem_inflight`
预期：PASS

运行：`cargo test --lib memory`
预期：既有 memory 测试全部 PASS

- [ ] **步骤 5：Commit**

```bash
git add src/systems/memory.rs
git commit -m "fix: memory_compression_system 增加在飞摘要保护，消除并发摘要请求"
```

---

### 任务 4：触发端接入共享选择函数

**文件：**
- 修改：`src/systems/memory.rs:69-85`

- [ ] **步骤 1：替换触发端选择逻辑**

将 L70-85：

```rust
            // 替换原有的 preserve_count / compress_count 逻辑
            let groups = split_into_groups(&short_term.entries);
            if groups.len() <= config.preserve_recent_turns as usize {
                continue;
            }

            let preserve_group_count = config.preserve_recent_turns as usize;
            let compress_entry_count = groups
                .iter()
                .take(groups.len() - preserve_group_count)
                .map(|g| g.len())
                .sum();

            if compress_entry_count == 0 {
                continue;
            }
```

替换为：

```rust
            let groups = split_into_groups(&short_term.entries);
            let preserve_group_count = config.preserve_recent_turns as usize;
            let compress_entry_count =
                compressible_entry_count(&groups, config.preserve_recent_turns);
            if compress_entry_count == 0 {
                continue;
            }
```

（L107-117 的 `CompressionTriggered` 日志字段 `groups_total`、`groups_to_compress` 继续使用 `groups.len()` 与 `groups.len() - preserve_group_count`，保持不变。）

- [ ] **步骤 2：运行全部相关测试**

运行：`cargo test --lib -- systems::memory`
预期：PASS

- [ ] **步骤 3：Commit**

```bash
git add src/systems/memory.rs
git commit -m "refactor: 触发端复用 compressible_entry_count 共享选择逻辑"
```

---

### 任务 5：集成串联测试与全量验证

**文件：**
- 修改：`tests/context_compression_flow.rs`（文件尾部追加测试）

- [ ] **步骤 1：添加循环终止性串联测试**

在 `tests/context_compression_flow.rs` 尾部追加（复用该文件既有的 `harness::domain::{...}` 导入风格，补充所需项）：

```rust
// ── 4. 摘要循环终止性（2026-08-16 修复回归）────────────────────

#[test]
fn summarization_loop_terminates_after_group_aligned_drain() {
    use harness::domain::{
        EntryMetadata, EntryRole, ShortTermMemory, ToolCall, compressible_entry_count,
        split_into_groups,
    };

    // 复现日志 harness_2026-08-15_23-56-36.jsonl：
    // 71k token 工具 entry 独立成组，落入 preserve_recent_turns=2 保护窗口
    let mut stm = ShortTermMemory::default();
    stm.add_entry(EntryRole::User, "帮我看今天的新闻", EntryMetadata::default());
    let mut metadata = EntryMetadata::default();
    metadata.tool_calls.push(ToolCall {
        id: Some("call_1".to_string()),
        tool_name: "shell_exec".to_string(),
        input: "playwright-cli browse".to_string(),
        output: "huge news page content".repeat(2000),
        timestamp: chrono::Utc::now(),
    });
    stm.add_entry(EntryRole::Assistant, String::new(), metadata);
    stm.add_entry(EntryRole::User, "总结一下", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "好的", EntryMetadata::default());
    stm.summary_prefix = Some("历史摘要".to_string());

    assert!(stm.estimated_tokens > 8000, "场景应超过默认阈值");

    // 模拟完成端 drain：首轮压缩组 0
    let removed = stm.drain_compressed_groups(2);
    assert_eq!(removed, 1);

    // 触发端视角：剩余组数 <= preserve → 不再触发，循环终止
    let groups = split_into_groups(&stm.entries);
    assert_eq!(compressible_entry_count(&groups, 2), 0);

    // 用户下一轮对话后，工具组落出保护窗口，可被正常压缩
    stm.add_entry(EntryRole::User, "新问题", EntryMetadata::default());
    stm.add_entry(EntryRole::Assistant, "新回答", EntryMetadata::default());
    let groups = split_into_groups(&stm.entries);
    assert_eq!(compressible_entry_count(&groups, 2), 1);
}
```

注意：`split_into_groups` / `compressible_entry_count` 为 `pub(crate)`，集成测试（外部 crate）不可见。需将两者可见性改为 `pub` 并在 `src/domain/mod.rs` / `src/lib.rs` 的既有导出路径中 re-export（参照 `estimate_tokens`、`render_tool_calls_summary` 的导出方式，见 `tests/context_compression_flow.rs:10-13` 已从 `harness::domain` 导入 `estimate_tokens`）。任务 1 步骤 3 中若已确认导出方式，此处同步应用。

- [ ] **步骤 2：运行集成测试**

运行：`cargo test --test context_compression_flow`
预期：全部 PASS（含既有测试与新增测试）

- [ ] **步骤 3：全量验证**

运行：`cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
预期：格式化无 diff、clippy 无警告、全部测试 PASS

- [ ] **步骤 4：Commit**

```bash
git add tests/context_compression_flow.rs src/domain/memory.rs src/domain/mod.rs
git commit -m "test: 摘要循环终止性回归测试（复现 2026-08-15 日志场景）"
```

---

## 自检记录

- **规格覆盖度：** 设计文档变更点 1（共享函数下沉）→ 任务 1；变更点 2（完成端 drain 对齐）→ 任务 2；变更点 3（在飞保护）→ 任务 3；4.2 节"触发端与完成端共用同一份分组逻辑"→ 任务 1/2/4；验证方案 6.1/6.2 → 任务 1-5。设计文档 6.2 的完整 app 级异步链路测试未采用（flaky 风险，参照项目"timeout-based poll"约定），以世界级 `run_system_once` 测试 + 纯函数串联测试覆盖同等断言。
- **占位符扫描：** 无"待定/TODO/类似任务 N"；所有代码步骤含完整代码。
- **类型一致性：** `drain_compressed_groups(preserve_recent_turns: u32) -> usize` 在任务 2 定义、任务 5 使用；`compressible_entry_count(&[Vec<usize>], u32) -> usize` 在任务 1 定义、任务 2/4/5 使用；`WorkItemStatus`/`is_terminal()` 与 `src/domain/work_item.rs:50/158/390` 一致。
