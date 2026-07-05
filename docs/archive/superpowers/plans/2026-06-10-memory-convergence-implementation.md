> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# Memory Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将项目记忆系统收敛为 `ShortTermMemory`、`LongTermMemory`、`SharedKnowledgeBase` 三类模型，删除 `AgentExperience`，落地长期记忆受控注入、共享知识受控写入与基础衰退治理。

**Architecture:** 保持现有 `ShortTermMemory` 的任务级职责不变，将长期记忆与共享知识从当前通用 `MemoryEntry` 模型中拆分为更诚实的领域条目结构。调度链路通过新的记忆选择器实现 `Core + Relevant` 注入，共享知识保留现有 `knowledge_search` 工具名但改为基于 `SharedKnowledgeBase` 查询，候选升级与衰退治理则通过独立系统完成，避免把所有逻辑堆进现有 dispatch 或 contribution 文件。

**Tech Stack:** Rust, Bevy ECS, serde, chrono, tracing, cargo test, markdownlint

---

## Scope Check

本计划覆盖以下已确认设计要求：

- 删除 `AgentExperience`
- 保留 `ShortTermMemory`
- 将 `LongTermMemory` 收敛为 Agent 私有长期记忆
- 将全局共享知识收敛为 `SharedKnowledgeBase`
- 禁止 `LongTermMemory` 全量无差别注入
- 为共享知识写入增加人工/主控准入边界
- 为长期记忆与共享知识增加基础衰退治理

本计划刻意不引入：

- 向量数据库
- 复杂语义检索
- 自动 LLM 审核共享知识入库
- 新的 UI 交互界面

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/domain/agent.rs` | 删除 `AgentExperience`，收紧 `Agent` 的领域模型 |
| `src/domain/memory.rs` | 定义 `ShortTermMemory` 现有模型，以及新的长期记忆条目、重要度、衰退字段与辅助方法 |
| `src/domain/space.rs` | 定义 `SharedKnowledgeBase`、共享知识条目、候选条目与审核状态 |
| `src/domain/mod.rs` | 调整导出，删除 `AgentExperience`，导出新的记忆与共享知识类型 |
| `src/contracts/memory.rs` | 更新长期记忆与共享知识的契约类型，避免继续使用通用 `MemoryEntry` 作为长期模型 |
| `src/app/mod.rs` | 注册 `SharedKnowledgeBase`，移除旧 `SpaceKnowledge` 初始化 |
| `src/systems/memory.rs` | 保留 STM 压缩，并新增长期记忆衰退治理系统 |
| `src/systems/mod.rs` | 导出新的衰退治理系统，供 plugin 注册 |
| `src/systems/contribution.rs` | 将子 Agent 贡献从“原样吸收”改为“提炼后写回 LTM / 共享知识候选” |
| `src/systems/dispatch/memory_selection.rs` | 新增长期记忆筛选与排序逻辑，隔离 dispatch 中的注入规则 |
| `src/systems/dispatch/task_dispatch.rs` | 改用结构化 `Core + Relevant` 注入 prompt，并更新测试 |
| `src/systems/command.rs` | 将 `/remember` 改为写入 `SharedKnowledgeBase` 的审核通过条目 |
| `src/systems/tools/builtin/knowledge_search.rs` | 改为基于 `SharedKnowledgeBase` 查询共享知识条目 |
| `src/systems/tools/mod.rs` | 调整工具上下文与相关测试，使其依赖 `SharedKnowledgeBase` |
| `tests/multi_agent_flow.rs` | 覆盖删除 `AgentExperience` 后的 Agent 生命周期与记忆贡献链路 |
| `tests/multi_turn_flow.rs` | 覆盖长期记忆写回过滤行为与任务多轮场景 |
| `tests/tool_execution_flow.rs` | 覆盖 `knowledge_search` 与共享知识资源更新 |
| `tests/llm_tool_calling_flow.rs` | 保证工具调用链路在共享知识改造后仍可工作 |
| `tests/shell_tool_flow.rs` | 机械修正 `Agent` 构造以移除 `experience` 字段 |
| `tests/wait_tasks_flow.rs` | 机械修正 `ToolContext` 与共享知识资源字段 |
| `src/plugins/memory.rs` | 将衰退治理系统接入现有 MemoryPlugin |
| `docs/current-state.md` | 更新对外能力描述，反映三类记忆与删除 `AgentExperience` |

---

### Task 1: 删除 AgentExperience 并修正 Agent 构造

**Files:**
- Modify: `src/domain/agent.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/systems/memory.rs`
- Modify: `src/systems/dispatch/agent_selection.rs`
- Modify: `src/systems/dispatch/task_dispatch.rs`
- Modify: `src/systems/tools/mod.rs`
- Modify: `tests/multi_agent_flow.rs`
- Modify: `tests/multi_turn_flow.rs`
- Modify: `tests/tool_execution_flow.rs`
- Modify: `tests/llm_tool_calling_flow.rs`
- Modify: `tests/shell_tool_flow.rs`
- Modify: `tests/wait_tasks_flow.rs`
- Modify: `tests/evaluation_workitem_flow.rs`
- Modify: `tests/multi_turn_routing.rs`

- [ ] **Step 1: 先写一个会因字段移除而编译失败的测试**

在 `src/domain/agent.rs` 的 `#[cfg(test)] mod tests` 中追加：

```rust
#[test]
fn agent_without_experience_still_grants_permissions() {
    let mut overrides = std::collections::HashMap::new();
    overrides.insert("knowledge_search".to_string(), super::ToolPermission::Allow);

    let agent = Agent {
        id: uuid::Uuid::nil(),
        profile: AgentProfile {
            name: "memory-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["memory".to_string()],
            description: "memory agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions {
            default_permission: super::ToolPermission::Confirm,
            overrides,
        },
    };

    assert!(agent.has_permission("knowledge_search"));
}
```

- [ ] **Step 2: 运行测试确认因旧字段存在而失败**

Run: `cargo test -q agent_without_experience_still_grants_permissions -- --nocapture`

Expected: FAIL，报错提示 `Agent` 缺少 `experience` 字段或构造不匹配。

- [ ] **Step 3: 删除 AgentExperience 并批量修正所有构造点**

在 `src/domain/agent.rs` 中删除 `AgentExperience` 定义，并把 `Agent` 改成：

```rust
/// Agent 实体
#[derive(Debug, Clone, Component)]
pub struct Agent {
    pub id: AgentId,
    pub profile: AgentProfile,
    pub capabilities: AgentCapabilities,
    pub kind: AgentKind,
    pub parent_id: Option<AgentId>,
    pub bound_task_id: Option<TaskId>,
    /// Tool 权限配置：启动加载、父 Agent 授权或后续修正
    pub tool_permissions: AgentToolPermissions,
}
```

在 `src/domain/mod.rs` 中把导出改成：

```rust
pub use agent::{Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions};
```

随后在所有测试和系统里的 `Agent { ... }` 构造中删除这一行：

```rust
experience: AgentExperience::default(),
```

- [ ] **Step 4: 运行受影响的单元测试与集成测试**

Run:

```bash
cargo test -q agent_without_experience_still_grants_permissions -- --nocapture
cargo test -q sub_task_prefers_default_on_no_tag_match -- --nocapture
cargo test -q executor_knowledge_search -- --nocapture
cargo test -q task_scoped_agent_lifecycle -- --nocapture
cargo test -q prompt_includes_summary_entries_as_system_notes -- --nocapture
cargo test -q tool_execution_flow -- --nocapture
```

Expected: PASS，且不再有任何 `AgentExperience` 未定义或缺字段的编译错误。

- [ ] **Step 5: 提交**

```bash
git add src/domain/agent.rs src/domain/mod.rs src/systems/memory.rs src/systems/dispatch/agent_selection.rs src/systems/dispatch/task_dispatch.rs src/systems/tools/mod.rs tests/multi_agent_flow.rs tests/multi_turn_flow.rs tests/tool_execution_flow.rs tests/llm_tool_calling_flow.rs tests/shell_tool_flow.rs tests/wait_tasks_flow.rs tests/evaluation_workitem_flow.rs tests/multi_turn_routing.rs
git commit -m "refactor: remove agent experience model"
```

---

### Task 2: 引入结构化 LongTermMemory 与 SharedKnowledgeBase 模型

**Files:**
- Modify: `src/domain/memory.rs`
- Modify: `src/domain/space.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/contracts/memory.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: 先写长期记忆与共享知识的领域测试**

在 `src/domain/memory.rs` 的测试模块中追加：

```rust
#[test]
fn long_term_memory_entry_defaults_to_decay_ready_state() {
    let entry = LongTermMemoryEntry::new(
        LongTermMemoryKind::Strategy,
        "Always prefer truthful shell semantics",
    );

    assert_eq!(entry.reuse_count, 0);
    assert!(!entry.pin);
    assert_eq!(entry.importance, MemoryImportance::Medium);
    assert!(entry.decay_score > 0.0);
}
```

在 `src/domain/space.rs` 的测试模块中追加：

```rust
#[test]
fn shared_knowledge_entry_from_user_is_approved() {
    let entry = SharedKnowledgeEntry::approved_from_user_input(
        "Project docs are written in Chinese",
    );

    assert_eq!(entry.validation_status, KnowledgeValidationStatus::Approved);
    assert_eq!(entry.source, KnowledgeSource::UserCommand);
}
```

- [ ] **Step 2: 运行测试确认新类型尚不存在**

Run:

```bash
cargo test -q long_term_memory_entry_defaults_to_decay_ready_state -- --nocapture
cargo test -q shared_knowledge_entry_from_user_is_approved -- --nocapture
```

Expected: FAIL，提示 `LongTermMemoryEntry`、`SharedKnowledgeEntry`、`KnowledgeValidationStatus` 等类型不存在。

- [ ] **Step 3: 在领域层引入新模型并替换旧共享知识资源**

在 `src/domain/memory.rs` 中新增：

```rust
/// 长期记忆条目类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LongTermMemoryKind {
    Constraint,
    Preference,
    Strategy,
    Fact,
    AntiPattern,
}

/// 长期记忆重要度。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryImportance {
    Low,
    Medium,
    High,
    Critical,
}

/// Agent 长期记忆条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LongTermMemoryEntry {
    pub content: String,
    pub kind: LongTermMemoryKind,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub pin: bool,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub reuse_count: u32,
    pub decay_score: f32,
    pub source: String,
    pub confidence: f32,
}

impl LongTermMemoryEntry {
    /// 创建默认可衰退的长期记忆条目。
    pub fn new(kind: LongTermMemoryKind, content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind,
            scope_tags: Vec::new(),
            importance: MemoryImportance::Medium,
            pin: false,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            decay_score: 1.0,
            source: "manual".to_string(),
            confidence: 0.8,
        }
    }
}
```

并明确给现有 `MemoryEntry` 补一条注释，说明它收敛为 `ShortTermMemory` 专属的对话条目类型，不再用于 `LongTermMemory` 或 `SharedKnowledgeBase`：

```rust
/// 短期记忆条目。
///
/// `MemoryEntry` 仅用于 `ShortTermMemory` 的对话与摘要条目，
/// 不再作为长期记忆或共享知识的底层模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
```

并把 `LongTermMemory` 改成：

```rust
#[derive(Component, Default, Clone)]
pub struct LongTermMemory {
    pub entries: Vec<LongTermMemoryEntry>,
}

impl LongTermMemory {
    /// 添加长期记忆条目。
    pub fn add_entry(&mut self, entry: LongTermMemoryEntry) {
        self.entries.push(entry);
    }

    /// 吸收来自子 Agent 的长期记忆条目。
    pub fn absorb(&mut self, entries: Vec<LongTermMemoryEntry>) {
        self.entries.extend(entries);
    }
}
```

在 `src/domain/space.rs` 中新增：

```rust
use super::memory::{LongTermMemoryKind, MemoryImportance};

/// 共享知识审核状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KnowledgeValidationStatus {
    Candidate,
    Approved,
    Rejected,
    Deprecated,
}

/// 共享知识来源。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KnowledgeSource {
    UserCommand,
    BrainReview,
    Migration,
}

/// 共享知识条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedKnowledgeEntry {
    pub content: String,
    pub kind: LongTermMemoryKind,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_accessed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub reuse_count: u32,
    pub confidence: f32,
    pub validation_status: KnowledgeValidationStatus,
    pub approved_by: Option<String>,
    pub source: KnowledgeSource,
}

#[derive(Resource, Default)]
pub struct SharedKnowledgeBase {
    pub entries: Vec<SharedKnowledgeEntry>,
}
```

把 `src/app/mod.rs` 中的资源注册改成：

```rust
app.insert_resource(SharedKnowledgeBase::default());
```

并在 `src/domain/mod.rs` 中导出新类型。

- [ ] **Step 4: 更新契约层，停止把长期模型继续写成通用 MemoryEntry**

在 `src/contracts/memory.rs` 中把导入改成：

```rust
use crate::domain::{
    AgentId, LongTermMemoryEntry, SharedKnowledgeEntry, ShortTermMemory, TaskId,
};
```

将 `MemoryStore` 改成：

```rust
pub trait MemoryStore: Send + Sync + 'static {
    fn get_entries(&self, agent_id: AgentId) -> Vec<LongTermMemoryEntry>;
    fn add_entry(&mut self, agent_id: AgentId, entry: LongTermMemoryEntry);
    fn clear(&mut self, agent_id: AgentId);
}
```

并在 trait 注释上补一句说明当前状态，避免误导后续实现者：

```rust
/// 记忆存储
///
/// 当前仓库内尚无 `MemoryStore` 的具体实现者；
/// 此处签名调整的目标是保持契约与新领域模型一致。
pub trait MemoryStore: Send + Sync + 'static {
```

将写回决策改成：

```rust
pub enum WritebackDecision {
    UpdateShortTermContext,
    AddLongTermMemory(LongTermMemoryEntry),
    AddSharedKnowledgeCandidate(SharedKnowledgeEntry),
    Drop,
}
```

- [ ] **Step 5: 运行领域层测试并提交**

Run:

```bash
cargo test -q long_term_memory_entry_defaults_to_decay_ready_state -- --nocapture
cargo test -q shared_knowledge_entry_from_user_is_approved -- --nocapture
cargo test -q long_term_memory_default_is_empty -- --nocapture
```

Expected: PASS。

Commit:

```bash
git add src/domain/memory.rs src/domain/space.rs src/domain/mod.rs src/contracts/memory.rs src/app/mod.rs
git commit -m "feat: add structured memory domain models"
```

---

### Task 3: 实现 LongTermMemory 的 Core + Relevant 受控注入

**Files:**
- Create: `src/systems/dispatch/memory_selection.rs`
- Modify: `src/systems/dispatch/mod.rs`
- Modify: `src/systems/dispatch/task_dispatch.rs`

- [ ] **Step 1: 先写记忆筛选与 prompt 注入的失败测试**

在 `src/systems/dispatch/task_dispatch.rs` 的测试模块中追加：

```rust
#[test]
fn prompt_includes_only_core_and_relevant_long_term_memory() {
    let long_term = LongTermMemory {
        entries: vec![
            LongTermMemoryEntry {
                content: "Always keep shell tools truthful".to_string(),
                kind: LongTermMemoryKind::Constraint,
                scope_tags: vec!["shell".to_string()],
                importance: MemoryImportance::Critical,
                pin: true,
                created_at: chrono::Utc::now(),
                last_accessed_at: None,
                reuse_count: 0,
                decay_score: 1.0,
                source: "migration".to_string(),
                confidence: 1.0,
            },
            LongTermMemoryEntry {
                content: "Use bounded timeout handling for shell commands".to_string(),
                kind: LongTermMemoryKind::Strategy,
                scope_tags: vec!["shell".to_string()],
                importance: MemoryImportance::High,
                pin: false,
                created_at: chrono::Utc::now(),
                last_accessed_at: None,
                reuse_count: 0,
                decay_score: 1.0,
                source: "migration".to_string(),
                confidence: 0.9,
            },
            LongTermMemoryEntry {
                content: "Unrelated frontend palette note".to_string(),
                kind: LongTermMemoryKind::Preference,
                scope_tags: vec!["ui".to_string()],
                importance: MemoryImportance::Low,
                pin: false,
                created_at: chrono::Utc::now(),
                last_accessed_at: None,
                reuse_count: 0,
                decay_score: 0.1,
                source: "migration".to_string(),
                confidence: 0.6,
            },
        ],
    };

    let prompt = build_prompt_with_context(
        "please improve shell timeout behavior",
        None,
        Some(&long_term),
    );

    assert!(prompt.contains("[Core agent memory]"));
    assert!(prompt.contains("Always keep shell tools truthful"));
    assert!(prompt.contains("[Relevant agent memory]"));
    assert!(prompt.contains("Use bounded timeout handling for shell commands"));
    assert!(!prompt.contains("Unrelated frontend palette note"));
}
```

- [ ] **Step 2: 运行测试确认当前全量注入实现不满足要求**

Run: `cargo test -q prompt_includes_only_core_and_relevant_long_term_memory -- --nocapture`

Expected: FAIL，旧实现会把无关长期记忆也拼进 prompt，或根本没有 `Core/Relevant` 分段。

- [ ] **Step 3: 把记忆筛选逻辑拆到独立模块**

创建 `src/systems/dispatch/memory_selection.rs`：

```rust
use chrono::Utc;

use crate::domain::{LongTermMemory, LongTermMemoryEntry, MemoryImportance};

/// prompt 注入预算。
#[derive(Debug, Clone, Copy)]
pub struct MemorySelectionBudget {
    pub max_core_entries: usize,
    pub max_relevant_entries: usize,
    pub max_relevant_tokens: u32,
}

/// 选中的长期记忆集合。
#[derive(Debug, Default)]
pub struct SelectedLongTermMemories {
    pub core: Vec<LongTermMemoryEntry>,
    pub relevant: Vec<LongTermMemoryEntry>,
}

/// 根据当前任务选择需要注入 prompt 的长期记忆。
pub fn select_long_term_memories(
    task_content: &str,
    long_term: &LongTermMemory,
    budget: MemorySelectionBudget,
) -> SelectedLongTermMemories {
    let lowered = task_content.to_lowercase();

    let mut core: Vec<_> = long_term
        .entries
        .iter()
        .filter(|entry| entry.pin && entry.confidence >= 0.8 && entry.decay_score > 0.2)
        .cloned()
        .collect();
    core.sort_by_key(|entry| {
        (
            std::cmp::Reverse(entry.importance),
            std::cmp::Reverse((entry.confidence * 100.0) as i32),
        )
    });
    core.truncate(budget.max_core_entries);

    let mut relevant: Vec<_> = long_term
        .entries
        .iter()
        .filter(|entry| !entry.pin)
        .filter(|entry| entry.decay_score > 0.2)
        .filter(|entry| {
            entry.content.to_lowercase().contains(&lowered)
                || entry.scope_tags.iter().any(|tag| lowered.contains(&tag.to_lowercase()))
        })
        .cloned()
        .collect();

    relevant.sort_by_key(|entry| {
        (
            std::cmp::Reverse(entry.importance),
            std::cmp::Reverse(entry.reuse_count),
            std::cmp::Reverse(entry.last_accessed_at.unwrap_or_else(Utc::now)),
        )
    });
    relevant.truncate(budget.max_relevant_entries);

    SelectedLongTermMemories { core, relevant }
}
```

在 `src/systems/dispatch/mod.rs` 中增加：

```rust
mod memory_selection;
```

在 `src/systems/dispatch/task_dispatch.rs` 中改写 prompt 构建逻辑：

```rust
use super::memory_selection::{select_long_term_memories, MemorySelectionBudget};

if let Some(ltm) = long_term && !ltm.entries.is_empty() {
    let selected = select_long_term_memories(
        task_content,
        ltm,
        MemorySelectionBudget {
            max_core_entries: 5,
            max_relevant_entries: 5,
            max_relevant_tokens: 800,
        },
    );

    if !selected.core.is_empty() {
        let core_text = selected
            .core
            .iter()
            .map(|entry| format!("- {}", entry.content))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("[Core agent memory]\n{}", core_text));
    }

    if !selected.relevant.is_empty() {
        let relevant_text = selected
            .relevant
            .iter()
            .map(|entry| format!("- {}", entry.content))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("[Relevant agent memory]\n{}", relevant_text));
    }
}
```

- [ ] **Step 4: 在新选择器模块中补排序测试，确保无关长期记忆不再进入 prompt**

在 `src/systems/dispatch/memory_selection.rs` 的 `#[cfg(test)] mod tests` 中追加：

```rust
#[test]
fn select_long_term_memories_skips_unrelated_and_low_decay_entries() {
    let mut long_term = LongTermMemory::default();
    long_term.entries.push(LongTermMemoryEntry {
        content: "Shell tools should expose honest waiting semantics".to_string(),
        kind: LongTermMemoryKind::Constraint,
        scope_tags: vec!["shell".to_string()],
        importance: MemoryImportance::Critical,
        pin: true,
        created_at: chrono::Utc::now(),
        last_accessed_at: None,
        reuse_count: 0,
        decay_score: 1.0,
        source: "test".to_string(),
        confidence: 1.0,
    });
    long_term.entries.push(LongTermMemoryEntry {
        content: "frontend color tweak".to_string(),
        kind: LongTermMemoryKind::Preference,
        scope_tags: vec!["ui".to_string()],
        importance: MemoryImportance::Low,
        pin: false,
        created_at: chrono::Utc::now(),
        last_accessed_at: None,
        reuse_count: 0,
        decay_score: 0.1,
        source: "test".to_string(),
        confidence: 0.7,
    });

    let selected = select_long_term_memories(
        "fix shell timeout",
        &long_term,
        MemorySelectionBudget {
            max_core_entries: 5,
            max_relevant_entries: 5,
            max_relevant_tokens: 800,
        },
    );

    assert_eq!(selected.core.len(), 1);
    assert!(selected.core[0].content.contains("honest waiting semantics"));
    assert!(selected.relevant.is_empty());
}
```

- [ ] **Step 5: 运行测试并提交**

Run:

```bash
cargo test -q prompt_includes_only_core_and_relevant_long_term_memory -- --nocapture
cargo test -q select_long_term_memories_skips_unrelated_and_low_decay_entries -- --nocapture
cargo test -q prompt_includes_summary_entries_as_system_notes -- --nocapture
```

Expected: PASS。

Commit:

```bash
git add src/systems/dispatch/memory_selection.rs src/systems/dispatch/mod.rs src/systems/dispatch/task_dispatch.rs
git commit -m "feat: add filtered long-term memory injection"
```

---

### Task 4: 将共享知识写入与查询切换到 SharedKnowledgeBase

**Files:**
- Modify: `src/domain/space.rs`
- Modify: `src/systems/command.rs`
- Modify: `src/systems/tools/builtin/knowledge_search.rs`
- Modify: `src/systems/tools/mod.rs`
- Modify: `tests/tool_execution_flow.rs`
- Modify: `tests/llm_tool_calling_flow.rs`
- Modify: `tests/wait_tasks_flow.rs`

- [ ] **Step 1: 先写共享知识入库与查询的失败测试**

在 `src/systems/command.rs` 的测试模块中追加：

```rust
#[test]
fn remember_command_creates_approved_shared_knowledge_entry() {
    let entry = crate::domain::SharedKnowledgeEntry::approved_from_user_input(
        "Docs should stay in Chinese",
    );

    assert_eq!(
        entry.validation_status,
        crate::domain::KnowledgeValidationStatus::Approved
    );
    assert_eq!(entry.approved_by.as_deref(), Some("user:/remember"));
}
```

在 `src/systems/tools/builtin/knowledge_search.rs` 的测试模块里把 `test_knowledge()` 改成返回 `SharedKnowledgeBase`，并新增：

```rust
#[test]
fn knowledge_search_ignores_non_approved_entries() {
    let mut knowledge = SharedKnowledgeBase::default();
    knowledge.entries.push(SharedKnowledgeEntry::candidate(
        "Unreviewed shell note",
        LongTermMemoryKind::Fact,
    ));

    let ctx = ToolContext {
        knowledge: &knowledge,
        default_wait_tasks_timeout_secs: 300,
        shell_default_tail_lines: 50,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 60,
        shell_default_stop_timeout_secs: 5,
        current_task_id: uuid::Uuid::nil(),
        current_agent_id: uuid::Uuid::nil(),
    };

    let executor = KnowledgeSearchTool;
    let result = executor.execute(&serde_json::json!({"query": "shell"}), &ctx).unwrap();
    match result {
        ToolAction::Direct(value) => assert_eq!(value["count"], 0),
        other => panic!("expected Direct action, got {:?}", other),
    }
}
```

- [ ] **Step 2: 运行测试确认 SharedKnowledgeBase 还没有接入命令和工具**

Run:

```bash
cargo test -q remember_command_creates_approved_shared_knowledge_entry -- --nocapture
cargo test -q knowledge_search_ignores_non_approved_entries -- --nocapture
```

Expected: FAIL。

- [ ] **Step 3: 实现用户直写审核通过条目与审批过滤查询**

在 `src/domain/space.rs` 中为共享知识补辅助构造：

```rust
impl SharedKnowledgeEntry {
    /// 创建用户显式确认的共享知识条目。
    pub fn approved_from_user_input(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: LongTermMemoryKind::Fact,
            scope_tags: Vec::new(),
            importance: MemoryImportance::High,
            created_at: chrono::Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            confidence: 1.0,
            validation_status: KnowledgeValidationStatus::Approved,
            approved_by: Some("user:/remember".to_string()),
            source: KnowledgeSource::UserCommand,
        }
    }

    /// 创建待审核候选条目。
    pub fn candidate(content: impl Into<String>, kind: LongTermMemoryKind) -> Self {
        Self {
            content: content.into(),
            kind,
            scope_tags: Vec::new(),
            importance: MemoryImportance::Medium,
            created_at: chrono::Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            confidence: 0.6,
            validation_status: KnowledgeValidationStatus::Candidate,
            approved_by: None,
            source: KnowledgeSource::BrainReview,
        }
    }
}
```

在 `src/systems/command.rs` 中将 `/remember` 写入改成：

```rust
knowledge
    .entries
    .push(SharedKnowledgeEntry::approved_from_user_input(content.clone()));
```

在 `src/systems/tools/builtin/knowledge_search.rs` 中把查询过滤改成：

```rust
let results: Vec<&str> = ctx
    .knowledge
    .entries
    .iter()
    .filter(|entry| entry.validation_status == KnowledgeValidationStatus::Approved)
    .filter(|entry| entry.content.to_lowercase().contains(&query.to_lowercase()))
    .take(limit)
    .map(|entry| entry.content.as_str())
    .collect();
```

- [ ] **Step 4: 修正 ToolContext 和工具测试**

在 `src/domain/space.rs` 中把 `ToolContext` 的字段改为：

```rust
pub struct ToolContext<'a> {
    pub knowledge: &'a SharedKnowledgeBase,
    pub default_wait_tasks_timeout_secs: u64,
    pub shell_default_tail_lines: u32,
    pub shell_max_tail_lines: u32,
    pub shell_default_exec_timeout_secs: u64,
    pub shell_default_stop_timeout_secs: u64,
    pub current_task_id: TaskId,
    pub current_agent_id: AgentId,
}
```

随后同步修改测试数据，避免 approved 过滤让原有断言全部失效：

```rust
fn test_knowledge() -> SharedKnowledgeBase {
    let mut knowledge = SharedKnowledgeBase::default();
    knowledge.entries.push(SharedKnowledgeEntry::approved_from_user_input(
        "The project uses Rust and Bevy framework",
    ));
    knowledge.entries.push(SharedKnowledgeEntry::approved_from_user_input(
        "The system follows ECS architecture",
    ));
    knowledge
}
```

并在 `src/systems/tools/mod.rs`、`tests/tool_execution_flow.rs`、`tests/llm_tool_calling_flow.rs`、`tests/wait_tasks_flow.rs` 中把 `SpaceKnowledge::default()` 构造点统一替换为 `SharedKnowledgeBase::default()`。

- [ ] **Step 5: 运行测试并提交**

Run:

```bash
cargo test -q remember_command_creates_approved_shared_knowledge_entry -- --nocapture
cargo test -q executor_knowledge_search -- --nocapture
cargo test -q knowledge_search_ignores_non_approved_entries -- --nocapture
cargo test -q llm_tool_calls_are_executed -- --nocapture
```

Expected: PASS。

Commit:

```bash
git add src/domain/space.rs src/systems/command.rs src/systems/tools/builtin/knowledge_search.rs src/systems/tools/mod.rs tests/tool_execution_flow.rs tests/llm_tool_calling_flow.rs tests/wait_tasks_flow.rs
git commit -m "feat: route shared knowledge through approved knowledge base"
```

---

### Task 5: 将子 Agent 贡献改为“提炼后写回 + 共享知识候选”

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/domain/space.rs`
- Modify: `src/systems/contribution.rs`
- Modify: `src/contracts/memory.rs`
- Modify: `tests/multi_agent_flow.rs`
- Modify: `tests/multi_turn_flow.rs`

- [ ] **Step 1: 先写当前“原样吸收全部记忆”应当失败的测试**

在 `src/systems/contribution.rs` 的测试模块中追加：

```rust
#[test]
fn memory_contribution_skips_low_value_entries_and_creates_candidates() {
    let summary = TaskSummary {
        task_id: uuid::Uuid::nil(),
        goal: "stabilize shell behavior".to_string(),
        outcome: "done".to_string(),
    };

    let entries = vec![
        LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "shell stop uses timeout"),
        LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "temporary debugging note"),
    ];

    let (accepted, candidates) = extract_memory_writebacks("worker", &summary, &entries);

    assert_eq!(accepted.len(), 1);
    assert!(accepted[0].content.contains("shell stop"));
    assert!(candidates.is_empty());
}
```

- [ ] **Step 2: 运行测试确认提炼函数尚不存在**

Run: `cargo test -q memory_contribution_skips_low_value_entries_and_creates_candidates -- --nocapture`

Expected: FAIL。

- [ ] **Step 3: 引入提炼结果类型与写回函数**

在 `src/domain/contribution.rs` 中新增：

```rust
use super::{AgentId, LongTermMemoryEntry, SharedKnowledgeEntry, TaskId};

/// 记忆写回结果。
#[derive(Debug, Clone, Default)]
pub struct MemoryWritebackBatch {
    pub accepted_long_term_memories: Vec<LongTermMemoryEntry>,
    pub shared_knowledge_candidates: Vec<SharedKnowledgeEntry>,
}

/// 记忆贡献请求消息
#[derive(Debug, Clone, Component)]
pub struct MemoryContributionRequestMessage {
    pub contributor_id: AgentId,
    pub contributor_name: String,
    pub parent_id: AgentId,
    pub memories: Vec<LongTermMemoryEntry>,
    pub task_summary: TaskSummary,
}

/// 记忆吸收消息（内部使用）
#[derive(Debug, Clone, Component)]
pub struct MemoryAbsorptionMessage {
    pub parent_id: AgentId,
    pub absorbed: Vec<LongTermMemoryEntry>,
}
```

在 `src/systems/contribution.rs` 中新增：

```rust
/// 根据子 Agent 贡献提炼长期记忆写回结果。
pub fn extract_memory_writebacks(
    contributor_name: &str,
    task_summary: &TaskSummary,
    memories: &[LongTermMemoryEntry],
) -> (Vec<LongTermMemoryEntry>, Vec<SharedKnowledgeEntry>) {
    let mut accepted = Vec::new();
    let mut candidates = Vec::new();

    for memory in memories {
        if memory.content.trim().is_empty() || memory.decay_score <= 0.2 {
            continue;
        }
        if memory.content.to_lowercase().contains("temporary") {
            continue;
        }

        let mut accepted_entry = memory.clone();
        accepted_entry.source = format!("task:{}:{}", task_summary.task_id, contributor_name);
        accepted.push(accepted_entry.clone());

        if accepted_entry.importance >= MemoryImportance::High && accepted_entry.confidence >= 0.9 {
            candidates.push(SharedKnowledgeEntry::candidate(
                accepted_entry.content.clone(),
                accepted_entry.kind,
            ));
        }
    }

    (accepted, candidates)
}
```

将 `memory_contribution_system` 改成：

```rust
pub(crate) fn memory_contribution_system(
    mut commands: Commands,
    mut knowledge: ResMut<SharedKnowledgeBase>,
    requests: Query<(Entity, &MemoryContributionRequestMessage)>,
) {
    for (entity, request) in &requests {
        let parent_id = request.parent_id;
        let (accepted, candidates) = extract_memory_writebacks(
            &request.contributor_name,
            &request.task_summary,
            &request.memories,
        );

        commands.spawn(MemoryAbsorptionMessage {
            parent_id,
            absorbed: accepted,
        });

        if !candidates.is_empty() {
            knowledge.entries.extend(candidates);
        }

        commands.entity(entity).despawn();
    }
}
```

- [ ] **Step 4: 在集成测试中验证父 Agent 只吸收提炼后的长期记忆**

在 `tests/multi_turn_flow.rs` 中追加：

```rust
#[test]
fn parent_agent_absorbs_filtered_long_term_memory_only() {
    let mut child_memory = LongTermMemory::default();
    child_memory.entries.push(LongTermMemoryEntry::new(
        LongTermMemoryKind::Strategy,
        "Prefer two-phase application for borrow-heavy mutations",
    ));
    child_memory.entries.push(LongTermMemoryEntry::new(
        LongTermMemoryKind::Fact,
        "temporary scratch pad",
    ));

    let summary = harness::TaskSummary {
        task_id: uuid::Uuid::nil(),
        goal: "refactor memory logic".to_string(),
        outcome: "done".to_string(),
    };

    let (accepted, _) = harness::systems::contribution::extract_memory_writebacks(
        "child",
        &summary,
        &child_memory.entries,
    );

    assert_eq!(accepted.len(), 1);
    assert!(accepted[0].content.contains("two-phase application"));
}
```

- [ ] **Step 5: 运行测试并提交**

Run:

```bash
cargo test -q memory_contribution_skips_low_value_entries_and_creates_candidates -- --nocapture
cargo test -q parent_agent_absorbs_filtered_long_term_memory_only -- --nocapture
cargo test -q task_scoped_agent_lifecycle -- --nocapture
```

Expected: PASS。

Commit:

```bash
git add src/domain/contribution.rs src/domain/space.rs src/systems/contribution.rs src/contracts/memory.rs tests/multi_agent_flow.rs tests/multi_turn_flow.rs
git commit -m "feat: extract filtered memory writebacks from child agents"
```

---

### Task 6: 增加长期记忆与共享知识的基础衰退治理，并更新文档

**Files:**
- Modify: `src/systems/memory.rs`
- Modify: `src/systems/mod.rs`
- Modify: `src/plugins/memory.rs`
- Modify: `docs/current-state.md`

- [ ] **Step 1: 先写衰退治理的单元测试**

在 `src/systems/memory.rs` 的测试模块中追加：

```rust
#[test]
fn decay_system_marks_stale_long_term_entries_inactive() {
    let mut memory = LongTermMemory {
        entries: vec![LongTermMemoryEntry {
            content: "stale note".to_string(),
            kind: LongTermMemoryKind::Fact,
            scope_tags: vec![],
            importance: MemoryImportance::Low,
            pin: false,
            created_at: chrono::Utc::now() - chrono::Duration::days(30),
            last_accessed_at: Some(chrono::Utc::now() - chrono::Duration::days(30)),
            reuse_count: 0,
            decay_score: 0.25,
            source: "test".to_string(),
            confidence: 0.7,
        }],
    };

    apply_memory_decay(&mut memory.entries, chrono::Utc::now());

    assert!(memory.entries[0].decay_score < 0.25);
}
```

- [ ] **Step 2: 运行测试确认治理函数尚不存在**

Run: `cargo test -q decay_system_marks_stale_long_term_entries_inactive -- --nocapture`

Expected: FAIL。

- [ ] **Step 3: 实现可解释的衰退函数与系统**

在 `src/systems/memory.rs` 中新增：

```rust
/// 根据最后访问时间、重要度和复用次数更新长期记忆衰退分数。
pub fn apply_memory_decay(
    entries: &mut [LongTermMemoryEntry],
    now: chrono::DateTime<chrono::Utc>,
) {
    for entry in entries {
        let age_days = entry
            now
            .signed_duration_since(entry.last_accessed_at.unwrap_or(entry.created_at))
            .num_days()
            .unsigned_abs() as f32;

        let base_penalty = (age_days / 30.0).min(0.5);
        let importance_bonus = match entry.importance {
            MemoryImportance::Low => 0.0,
            MemoryImportance::Medium => 0.05,
            MemoryImportance::High => 0.1,
            MemoryImportance::Critical => 0.2,
        };
        let reuse_bonus = (entry.reuse_count as f32 * 0.02).min(0.2);

        entry.decay_score = (entry.decay_score - base_penalty + importance_bonus + reuse_bonus)
            .clamp(0.0, 1.0);
    }
}

/// 周期性衰退治理系统。
pub(crate) fn long_term_memory_decay_system(
    mut agents: Query<(&Agent, &mut LongTermMemory)>,
) {
    let now = chrono::Utc::now();
    for (_agent, mut memory) in &mut agents {
        apply_memory_decay(&mut memory.entries, now);
    }
}
```

并在 `src/systems/mod.rs` 中导出：

```rust
pub(crate) use memory::{
    init_agent_memory_system, long_term_memory_decay_system, memory_compression_system,
};
```

在 `src/plugins/memory.rs` 中注册：

```rust
use crate::systems::{
    HarnessSet, init_agent_memory_system, long_term_memory_decay_system,
    memory_absorption_system, memory_compression_system, summarization_dispatch_system,
};

app.add_systems(
    Update,
    (
        memory_compression_system.in_set(HarnessSet::Maintenance),
        init_agent_memory_system.in_set(HarnessSet::Maintenance),
        long_term_memory_decay_system.in_set(HarnessSet::Maintenance),
        memory_absorption_system.in_set(HarnessSet::Maintenance),
        summarization_dispatch_system
            .in_set(HarnessSet::Maintenance)
            .after(crate::systems::agent_factory_system),
    ),
);
```

- [ ] **Step 4: 更新当前能力文档**

在 `docs/current-state.md` 中追加或修改如下内容：

```md
- 记忆系统已收敛为 `ShortTermMemory`、`LongTermMemory`、`SharedKnowledgeBase`
- `AgentExperience` 已删除，不再作为独立运行时概念
- `LongTermMemory` 使用 `Core + Relevant` 方式受控注入 prompt
- 共享知识写入默认仅允许用户显式命令或主控审核链路
- 长期记忆与共享知识已具备基础衰退治理能力
```

- [ ] **Step 5: 运行回归并提交**

Run:

```bash
cargo test -q decay_system_marks_stale_long_term_entries_inactive -- --nocapture
cargo test -q short_term_memory_token_estimation -- --nocapture
cargo test -q multi_turn_flow -- --nocapture
cargo test -q tool_execution_flow -- --nocapture
markdownlint -- docs/current-state.md docs/superpowers/plans/2026-06-10-memory-convergence-implementation.md
```

Expected: PASS。若 `markdownlint` 仍打印帮助信息，先用 `markdownlint docs/current-state.md docs/superpowers/plans/2026-06-10-memory-convergence-implementation.md` 手动确认本地 CLI 用法，再重跑一次。

Commit:

```bash
git add src/systems/memory.rs src/systems/mod.rs src/plugins/memory.rs docs/current-state.md
git commit -m "feat: add memory decay governance"
```

---

## Self-Review Checklist

- [ ] `AgentExperience` 在代码、导出与测试中均已删除
- [ ] `LongTermMemory` 不再使用原始 `MemoryEntry` 作为长期条目主模型
- [ ] `task_dispatch` 不再全量注入长期记忆
- [ ] `knowledge_search` 只查询审核通过的共享知识
- [ ] 子 Agent 贡献链路不再原样吸收所有长期记忆
- [ ] 衰退治理对长期记忆生效
- [ ] `docs/current-state.md` 已同步当前实现状态

## Final Validation

在所有任务完成后，运行完整验证：

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
markdownlint docs/current-state.md docs/superpowers/specs/2026-06-10-memory-convergence-design.md docs/superpowers/plans/2026-06-10-memory-convergence-implementation.md
```
