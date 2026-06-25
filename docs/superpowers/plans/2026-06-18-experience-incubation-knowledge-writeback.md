# 经验孵化写回修复实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让经验治理孵化出的 Agent 真正携带知识：审批通过后把 knowledge candidate 内容写入目标 Agent 的长期记忆（LTM），并在 `agents.toml` 中生成非空 `description`；同时清理仓库 `agents.toml` 中的测试污染。

**Architecture：** 复用现有 `LongTermMemoryService` 做知识落盘，复用 `IncubatedAgentRegistry` 做 Agent 元数据落盘；在 `writeback_incubation_proposal` 中先写 LTM、再写 `agents.toml`，任何一步失败都把 proposal 置为 `ExecutionFailed`。`LongTermMemory::add_entry` 增加按 `source_candidate_id` 去重，防止重复审批导致重复条目。

**Tech Stack：** Rust、Bevy ECS、toml、tempfile

---

## 文件结构

| 文件 | 职责 |
|------|------|
| `src/domain/memory.rs` | `LongTermMemory::add_entry` 增加 `source_candidate_id` 去重 |
| `src/systems/experience/writeback.rs` | 新增 `build_incubated_agent_description`；修改 `writeback_incubation_proposal` 写入 LTM 和 description |
| `tests/incubation_execution_flow.rs` | 新增/更新测试，验证 LTM 落盘和 `agents.toml` description |
| `agents.toml` | 删除 `incubated-test` 测试固件 |

---

## Task 1: LTM 按 source_candidate_id 去重

**Files:**
- Modify: `src/domain/memory.rs`
- Test: `src/domain/memory.rs` (现有 `#[cfg(test)]` 模块)

- [ ] **Step 1: 写失败测试**

在 `src/domain/memory.rs` 的测试模块末尾添加：

```rust
#[test]
fn add_entry_dedups_by_source_candidate_id() {
    let mut memory = LongTermMemory::with_name("dedup-agent");
    let candidate_id = uuid::Uuid::new_v4();
    let mut entry = LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "content");
    entry.source_candidate_id = Some(candidate_id);

    memory.add_entry(entry.clone());
    memory.add_entry(entry);

    assert_eq!(memory.entries.len(), 1);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p harness add_entry_dedups_by_source_candidate_id -- --nocapture`

Expected: FAIL，因为当前 `add_entry` 会 push 两次。

- [ ] **Step 3: 实现去重逻辑**

修改 `src/domain/memory.rs` 中 `LongTermMemory::add_entry`：

```rust
pub fn add_entry(&mut self, entry: LongTermMemoryEntry) {
    if let Some(candidate_id) = entry.source_candidate_id {
        if self
            .entries
            .iter()
            .any(|e| e.source_candidate_id == Some(candidate_id))
        {
            return;
        }
    }
    self.entries.push(entry);
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p harness add_entry_dedups_by_source_candidate_id -- --nocapture`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/domain/memory.rs
git commit -m "feat(memory): dedupe LTM entries by source_candidate_id"
```

---

## Task 2: 构建孵化 Agent description

**Files:**
- Modify: `src/systems/experience/writeback.rs`
- Test: `src/systems/experience/writeback.rs` (新增 `#[cfg(test)]`)

- [ ] **Step 1: 写失败测试**

在 `src/systems/experience/writeback.rs` 的测试模块添加：

```rust
#[test]
fn description_builds_from_candidate_titles() {
    let mut store = crate::domain::ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let c1 = crate::domain::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "公式推导".to_string(),
        "content1".to_string(),
        crate::domain::LongTermMemoryKind::Fact,
    );
    let c2 = crate::domain::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "数值验证".to_string(),
        "content2".to_string(),
        crate::domain::LongTermMemoryKind::Fact,
    );
    store.stage_root_candidate(c1.clone());
    store.stage_root_candidate(c2.clone());

    let proposal = crate::domain::IncubationProposal::new(
        task_id,
        agent_id,
        crate::domain::AgentProfile {
            name: "incubated-test".to_string(),
            model: "test".to_string(),
        },
    );

    let description = build_incubated_agent_description(&store, &proposal);
    assert_eq!(description, "基于 2 条经验孵化：公式推导；数值验证");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p harness description_builds_from_candidate_titles -- --nocapture`

Expected: 编译失败，`build_incubated_agent_description` 未定义

- [ ] **Step 3: 实现 helper 函数**

在 `src/systems/experience/writeback.rs` 顶部（`experience_writeback_system` 之前）添加：

```rust
fn build_incubated_agent_description(
    store: &crate::domain::ExperienceStore,
    proposal: &crate::domain::IncubationProposal,
) -> String {
    let titles: Vec<String> = proposal
        .knowledge_candidate_ids
        .iter()
        .filter_map(|id| store.candidates.get(id).map(|c| c.title.clone()))
        .collect();

    match titles.len() {
        0 => String::new(),
        1 => titles[0].clone(),
        n => format!(
            "基于 {} 条经验孵化：{}",
            n,
            titles.iter().take(3).cloned().collect::<Vec<_>>().join("；")
        ),
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p harness description_builds_from_candidate_titles -- --nocapture`

Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/systems/experience/writeback.rs
git commit -m "feat(experience): build incubated agent description from candidate titles"
```

---

## Task 3: 让 writeback_incubation_proposal 持有 LTM 服务

**Files:**
- Modify: `src/systems/experience/writeback.rs`

- [ ] **Step 1: 修改函数签名**

把 `writeback_incubation_proposal` 增加 `service` 参数：

```rust
fn writeback_incubation_proposal(
    task_id: TaskId,
    store: &mut ExperienceStore,
    proposal_store: &crate::infrastructure::incubation::proposal_store::IncubationProposalStore,
    agent_registry: &crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry,
    service: &mut crate::infrastructure::memory::LongTermMemoryService,
    config_path: &str,
) -> Result<(), String> {
```

- [ ] **Step 2: 修改调用点**

在 `experience_writeback_system` 的 `match decision.destination` 中，把 `IncubationProposal` 分支改为：

```rust
ExperienceWritebackDestination::IncubationProposal => {
    writeback_incubation_proposal(
        decision.source_task_id,
        &mut store,
        &proposal_store,
        &agent_registry,
        &mut service,
        &settings.0.agents_config_path,
    )
}
```

- [ ] **Step 3: 编译检查**

Run: `cargo check -p harness`

Expected: 通过（此时函数体还未使用 service，会有 unused 警告，下一步消除）

- [ ] **Step 4: Commit**

```bash
git add src/systems/experience/writeback.rs
git commit -m "refactor(experience): pass LongTermMemoryService into incubation writeback"
```

---

## Task 4: 在 writeback_incubation_proposal 中写入 LTM 和 description

**Files:**
- Modify: `src/systems/experience/writeback.rs`
- Test: `src/systems/experience/writeback.rs` (新增 `#[cfg(test)]`)

- [ ] **Step 1: 在函数体内插入 LTM 写回逻辑**

在 `writeback_incubation_proposal` 中，持久化 proposal 之后、`IncubatedAgentRecord` 创建之前，添加：

```rust
    // 把知识候选写入目标 Agent 的 LTM
    let candidate_entries: Vec<crate::domain::LongTermMemoryEntry> = proposal
        .knowledge_candidate_ids
        .iter()
        .filter_map(|id| store.candidates.get(id))
        .filter_map(|candidate| {
            let mut entry = candidate.as_long_term_memory_entry()?;
            entry.source_candidate_id = Some(candidate.candidate_id);
            entry.source_task_id = Some(candidate.producer_task_id);
            entry.agent_id = Some(candidate.producer_agent_id);
            Some(entry)
        })
        .collect();

    if !candidate_entries.is_empty() {
        let mut memory = crate::domain::LongTermMemory::with_name(profile.name.clone());
        memory.entries = service.load_entries(&profile.name);
        for entry in candidate_entries {
            service
                .add_entry(&mut memory, entry)
                .map_err(|e| e.to_string())?;
        }
    }
```

- [ ] **Step 2: 用 description helper 替换 rationale**

把原来的：

```rust
let rationale = proposal.incubation_rationale.clone();
```

和：

```rust
description: rationale,
```

改为：

```rust
let description = build_incubated_agent_description(store, &proposal);
```

和：

```rust
description: description.clone(),
```

- [ ] **Step 3: 调整 proposal 状态持久化位置**

`LTM` 写回失败时，当前逻辑已经会进入 `Err(e)` 分支并把 proposal 置为 `ExecutionFailed`。确认 `agents.toml` 写回失败时也保持一致。现有逻辑已满足，无需额外改动。

- [ ] **Step 4: 写单元测试验证完整写回链路**

在 `src/systems/experience/writeback.rs` 测试模块添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentProfile, ExperienceCandidate, ExperienceStore, IncubationProposalStatus,
        LongTermMemoryKind,
    };
    use crate::infrastructure::incubation::agent_registry::IncubatedAgentRegistry;
    use crate::infrastructure::incubation::proposal_store::IncubationProposalStore;
    use crate::infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository};
    use tempfile::TempDir;

    fn make_memory_service(dir: &TempDir) -> LongTermMemoryService {
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        LongTermMemoryService::new(MemoryRepository::new(Box::new(store)))
    }

    #[test]
    fn incubation_writeback_persists_knowledge_to_ltm_and_agents_toml() {
        let memory_dir = TempDir::new().unwrap();
        let proposal_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let config_path = config_dir.path().join("agents.toml");

        let mut memory_service = make_memory_service(&memory_dir);
        let proposal_store = IncubationProposalStore::new(proposal_dir.path().join("proposals"));
        let registry = IncubatedAgentRegistry;

        let mut store = ExperienceStore::default();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();

        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "天体表面重力加速度计算流程".to_string(),
            "使用万有引力公式 g = G·M/R²".to_string(),
            LongTermMemoryKind::Fact,
        );
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate.clone());

        let profile = AgentProfile {
            name: "incubated-test-flow".to_string(),
            model: "gpt-4.1-mini".to_string(),
        };
        store.merge_into_proposal(task_id, agent_id, profile.clone(), &candidate);
        store.proposals.get_mut(&task_id).unwrap().status = IncubationProposalStatus::Approved;

        let result = writeback_incubation_proposal(
            task_id,
            &mut store,
            &proposal_store,
            &registry,
            &mut memory_service,
            config_path.to_str().unwrap(),
        );

        assert!(result.is_ok(), "writeback failed: {:?}", result);

        let loaded = memory_service.load_entries(&profile.name);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "使用万有引力公式 g = G·M/R²");
        assert_eq!(loaded[0].source_candidate_id, Some(candidate_id));

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: crate::domain::AgentConfig = toml::from_str(&content).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].name, profile.name);
        assert_eq!(
            config.agent[0].description,
            "天体表面重力加速度计算流程"
        );
    }
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p harness incubation_writeback_persists_knowledge_to_ltm_and_agents_toml -- --nocapture`

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/systems/experience/writeback.rs
git commit -m "feat(experience): persist incubated knowledge to LTM and generate description"
```

---

## Task 5: 清理仓库 agents.toml 并确保测试不污染仓库配置

**Files:**
- Modify: `agents.toml`

- [ ] **Step 1: 删除测试固件**

编辑 `agents.toml`，删除以下条目：

```toml
[[agent]]
name = "incubated-test"
model = "test"
tags = ["incubated"]
description = ""
```

- [ ] **Step 2: 检查是否有测试直接写仓库 agents.toml**

Run:

```bash
grep -rn "IncubatedAgentRegistry" tests/ src/ | grep -v "tempfile\|TempDir"
```

Expected: 无输出（所有 `append` 调用都应使用 `TempDir` 生成的路径）

- [ ] **Step 3: 运行全部测试**

Run: `cargo test --all-features`

Expected: 全部通过，且运行后 `git status` 不显示 `agents.toml` 被修改

- [ ] **Step 4: 确认 agents.toml 不再包含 incubated-test**

Run: `grep -n "incubated-test" agents.toml`

Expected: 无输出

- [ ] **Step 5: Commit**

```bash
git add agents.toml
git commit -m "chore(config): remove incubated-test fixture"
```

---

## Task 6: 运行 CI 检查

- [ ] **Step 1: 格式化检查**

Run: `cargo fmt --all --check`

Expected: 无输出（通过）

- [ ] **Step 2: Clippy 检查**

Run: `cargo clippy --all-targets --all-features -- -D warnings`

Expected: 通过，无 warning

- [ ] **Step 3: 运行全部测试**

Run: `cargo test --all-features`

Expected: 全部通过

- [ ] **Step 4: Markdown 检查**

Run: `markdownlint docs/superpowers/specs/2026-06-18-experience-incubation-knowledge-writeback-design.md docs/superpowers/plans/2026-06-18-experience-incubation-knowledge-writeback.md`

Expected: 无错误

- [ ] **Step 5: Commit 最终调整**

```bash
git add -A
git commit -m "chore: format and lint fixes"
```

---

## 自审检查

**Spec coverage:**
- ✅ 孵化 Agent 携带知识 → Task 4 LTM 写回
- ✅ 提案文件悬空索引问题 → 通过 LTM 持久化候选内容解决
- ✅ agents.toml 测试污染 → Task 5 清理并加强隔离
- ✅ description 生成 → Task 2 + Task 4
- ✅ source_candidate_id 去重 → Task 1

**Placeholder scan:**
- 无 TBD/TODO/"implement later"
- 代码块均为可直接运行的 Rust
- 测试命令和断言完整

**Type consistency:**
- `writeback_incubation_proposal` 新增 `service: &mut LongTermMemoryService` 参数，调用点和测试一致
- `LongTermMemory::add_entry` 签名不变，内部新增去重
- `IncubatedAgentRecord` 字段不变，description 由上层生成后传入

---

## 执行交接

Plan complete and saved to `docs/superpowers/plans/2026-06-18-experience-incubation-knowledge-writeback.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints for review

Which approach?