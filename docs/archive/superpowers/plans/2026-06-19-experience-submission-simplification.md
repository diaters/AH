> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验治理模块参数与概念简化 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 全链路简化经验治理模块：从提交工具参数到领域模型、治理分流、写回执行，移除冗余概念，对齐 Agent Skills 规范。

**Architecture:** 保留单工具 `submit_experience_candidate`，将 `kind_hint` 简化为 `knowledge`/`skill`，移除无结构 `payload` 和伪精细控制面（risk_level/risk_reason/suggested_confirmation），移除 `LongTermMemoryKind` 枚举，治理分流仅根据 `kind` + `is_default_agent` 决定，写回产出对齐 Agent Skills 规范的 SKILL.md 目录结构。

**Tech Stack:** Rust, Bevy ECS, serde_json, chrono, uuid

## Global Constraints

- 遵循 Conventional Commits
- 提交前完成 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`
- 使用中文撰写项目文档
- 同一变更涉及的代码与文档应尽量放在同一提交中
- 不引入新依赖

---

## File Structure

| 文件 | 职责 | 变更类型 |
|------|------|---------|
| `src/domain/contribution.rs` | 经验候选类型定义 | 重构 |
| `src/domain/memory.rs` | 记忆数据模型 | 修改 |
| `src/domain/space.rs` | 共享资源定义 | 修改 |
| `src/domain/mod.rs` | domain 导出 | 修改 |
| `src/systems/tools/mod.rs` | 工具注册与 Schema | 修改 |
| `src/systems/tools/builtin/submit_experience_candidate.rs` | 提交工具实现 | 重写 |
| `src/systems/tools/orchestrator.rs` | 工具执行协调 | 修改 |
| `src/systems/experience/governance.rs` | 经验治理分流 | 重构 |
| `src/systems/experience/writeback.rs` | 经验写回执行 | 重构 |
| `src/systems/experience/collection.rs` | 经验收集 | 修改 |
| `src/systems/experience/approval.rs` | 经验审批 | 修改 |
| `src/systems/memory.rs` | 记忆压缩/衰退 | 修改测试 |
| `src/systems/dispatch/memory_selection.rs` | 记忆选择 | 修改测试 |
| `src/systems/dispatch/task_dispatch.rs` | 任务派发 | 修改测试 |
| `src/systems/tools/builtin/knowledge_search.rs` | 知识搜索 | 修改测试 |
| `src/systems/tools/builtin/list_experience_candidates.rs` | 候选列表 | 修改测试 |
| `src/infrastructure/memory/service.rs` | LTM 服务 | 修改测试 |
| `src/infrastructure/memory/upgrade_service.rs` | 共享知识升级服务 | 删除 |
| `src/infrastructure/memory/mod.rs` | 基础设施导出 | 修改 |
| `src/infrastructure/assets/service.rs` | 资产服务 | 修改 SkillPackageDraft |
| `src/app/mod.rs` | 应用初始化 | 修改 |
| `src/plugins/memory.rs` | 记忆插件 | 修改 |
| `src/lib.rs` | 库导出 | 修改 |
| `tests/experience_candidate_flow.rs` | 候选流程测试 | 修改 |
| `tests/experience_collection_workitem_flow.rs` | 收集流程测试 | 修改 |
| `tests/experience_layered_governance_flow.rs` | 分层治理测试 | 修改 |
| `tests/incubation_execution_flow.rs` | 孵化执行测试 | 修改 |
| `tests/memory_persistence_flow.rs` | 记忆持久化测试 | 修改 |

---

### Task 1: 领域模型重构 — contribution.rs

**Files:**
- Modify: `src/domain/contribution.rs`

**Interfaces:**
- Produces: `ExperienceKindHint { Knowledge, Skill }`, `ExperienceCandidatePayload { Knowledge { content }, Skill { name, description, instructions, file_refs } }`, `SkillFileRef`, `SkillFileRole`, `ExperienceCandidateSubmission { title, kind, content, skill_description, instructions, file_refs }`, `ExperienceCandidate`（移除 risk_level/risk_reason/suggested_confirmation）
- Removes: `ExperienceKindHint::Executable/SharedKnowledge/Discard`, `ExperienceRiskLevel`, `ExperienceConfirmationPolicy`, `ExperienceCandidatePayload::Executable`, `SharedKnowledgeUpgradeCandidate`, `ExperienceWritebackDestination::SharedKnowledgeUpgrade`

- [ ] **Step 1: 更新 ExperienceKindHint 枚举**

将 `ExperienceKindHint` 从四个变体简化为两个：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceKindHint {
    Knowledge,
    Skill,
}
```

移除 `Executable`、`SharedKnowledge`、`Discard` 变体。

- [ ] **Step 2: 新增 SkillFileRef 和 SkillFileRole**

在 `ExperienceKindHint` 之后添加：

```rust
/// Skill 关联文件角色。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillFileRole {
    Script,
    Reference,
    Asset,
}

/// Skill 关联文件引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillFileRef {
    pub path: String,
    pub role: SkillFileRole,
}
```

- [ ] **Step 3: 删除 ExperienceRiskLevel 和 ExperienceConfirmationPolicy 枚举**

移除整个 `ExperienceRiskLevel` 枚举定义和 `ExperienceConfirmationPolicy` 枚举定义。

- [ ] **Step 4: 更新 ExperienceCandidatePayload**

替换为：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidatePayload {
    Knowledge { content: String },
    Skill {
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
    },
}
```

更新 `ExperienceCandidatePayload::content()` 方法：

```rust
impl ExperienceCandidatePayload {
    pub fn content(&self) -> Option<String> {
        match self {
            ExperienceCandidatePayload::Knowledge { content, .. } => Some(content.clone()),
            ExperienceCandidatePayload::Skill { .. } => None,
        }
    }
}
```

- [ ] **Step 5: 更新 ExperienceCandidate**

移除字段 `risk_level`、`risk_reason`、`suggested_confirmation`。结构体变为：

```rust
pub struct ExperienceCandidate {
    pub candidate_id: uuid::Uuid,
    pub producer_task_id: TaskId,
    pub producer_agent_id: AgentId,
    pub title: String,
    pub kind_hint: ExperienceKindHint,
    pub payload: ExperienceCandidatePayload,
    pub dependency_refs: Vec<String>,
    pub status: ExperienceCandidateStatus,
    pub governing_agent_id: Option<AgentId>,
    pub derived_from_candidate_ids: Vec<uuid::Uuid>,
}
```

更新 `ExperienceCandidate::knowledge()` 工厂方法，移除 `memory_kind` 参数：

```rust
pub fn knowledge(
    candidate_id: uuid::Uuid,
    producer_task_id: TaskId,
    producer_agent_id: AgentId,
    title: String,
    content: String,
) -> Self {
    Self {
        candidate_id,
        producer_task_id,
        producer_agent_id,
        title,
        kind_hint: ExperienceKindHint::Knowledge,
        payload: ExperienceCandidatePayload::Knowledge { content },
        dependency_refs: Vec::new(),
        status: ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        derived_from_candidate_ids: Vec::new(),
    }
}
```

新增 `ExperienceCandidate::skill()` 工厂方法：

```rust
pub fn skill(
    candidate_id: uuid::Uuid,
    producer_task_id: TaskId,
    producer_agent_id: AgentId,
    title: String,
    name: String,
    description: String,
    instructions: String,
    file_refs: Vec<SkillFileRef>,
) -> Self {
    Self {
        candidate_id,
        producer_task_id,
        producer_agent_id,
        title,
        kind_hint: ExperienceKindHint::Skill,
        payload: ExperienceCandidatePayload::Skill {
            name,
            description,
            instructions,
            file_refs,
        },
        dependency_refs: Vec::new(),
        status: ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        derived_from_candidate_ids: Vec::new(),
    }
}
```

更新 `requires_user_confirmation()`：skill 类始终需确认，knowledge 类无需确认：

```rust
pub fn requires_user_confirmation(&self) -> bool {
    matches!(self.kind_hint, ExperienceKindHint::Skill)
}
```

更新 `as_long_term_memory_entry()`：移除 `memory_kind` 参数，`LongTermMemoryEntry::new` 不再接受 kind（见 Task 2）：

```rust
pub fn as_long_term_memory_entry(&self) -> Option<super::LongTermMemoryEntry> {
    match &self.payload {
        ExperienceCandidatePayload::Knowledge { content, .. } => {
            Some(super::LongTermMemoryEntry::new(content.clone()))
        }
        ExperienceCandidatePayload::Skill { .. } => None,
    }
}
```

- [ ] **Step 6: 更新 ExperienceWritebackDestination**

移除 `SharedKnowledgeUpgrade` 变体：

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceWritebackDestination {
    LongTermMemory,
    SkillPackage,
    IncubationProposal,
    Rejected,
}
```

- [ ] **Step 7: 更新 ExperienceGovernanceDecision**

移除 `final_risk_level` 和 `risk_overridden` 字段：

```rust
pub struct ExperienceGovernanceDecision {
    pub candidate_id: uuid::Uuid,
    pub destination: ExperienceWritebackDestination,
    pub requires_user_confirmation: bool,
    pub decision_rationale: String,
    pub source_task_id: TaskId,
}
```

- [ ] **Step 8: 删除 SharedKnowledgeUpgradeCandidate**

移除整个 `SharedKnowledgeUpgradeCandidate` 结构体定义。

- [ ] **Step 9: 更新 ExperienceCandidateSubmission**

替换为：

```rust
pub struct ExperienceCandidateSubmission {
    pub title: String,
    pub kind: ExperienceKindHint,
    pub content: Option<String>,
    pub skill_description: Option<String>,
    pub instructions: Option<String>,
    pub file_refs: Vec<SkillFileRef>,
}
```

更新 `from_json` 方法：

```rust
impl ExperienceCandidateSubmission {
    pub fn from_json(
        _task_id: TaskId,
        _agent_id: AgentId,
        title: &str,
        input: &serde_json::Value,
    ) -> Result<Self, ToolError> {
        let kind_str = input
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("knowledge");
        let kind = match kind_str {
            "skill" => ExperienceKindHint::Skill,
            _ => ExperienceKindHint::Knowledge,
        };

        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);

        let skill_description = input
            .get("skill_description")
            .and_then(|v| v.as_str())
            .map(String::from);

        let instructions = input
            .get("instructions")
            .and_then(|v| v.as_str())
            .map(String::from);

        let file_refs = input
            .get("file_refs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let path = item.get("path")?.as_str()?.to_string();
                        let role_str = item
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let role = match role_str {
                            "script" => SkillFileRole::Script,
                            "reference" => SkillFileRole::Reference,
                            "asset" => SkillFileRole::Asset,
                            _ => {
                                // 根据扩展名推断
                                if path.ends_with(".sh") || path.ends_with(".py") {
                                    SkillFileRole::Script
                                } else if path.ends_with(".md") || path.ends_with(".txt") {
                                    SkillFileRole::Reference
                                } else {
                                    SkillFileRole::Asset
                                }
                            }
                        };
                        Some(SkillFileRef { path, role })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            title: title.to_string(),
            kind,
            content,
            skill_description,
            instructions,
            file_refs,
        })
    }
}
```

- [ ] **Step 10: 更新 IncubationProposal**

`IncubationProposal` 中的 `shared_knowledge_candidate_ids` 字段移除，`merge_candidate` 中 `ExperienceKindHint::SharedKnowledge` 和 `Discard` 分支移除：

```rust
pub struct IncubationProposal {
    pub proposal_id: uuid::Uuid,
    pub source_agent_id: AgentId,
    pub source_task_id: TaskId,
    pub proposed_agent_profile: super::AgentProfile,
    pub knowledge_candidate_ids: Vec<uuid::Uuid>,
    pub skill_candidate_ids: Vec<uuid::Uuid>,  // 从 executable_candidate_ids 重命名
    pub incubation_rationale: String,
    pub status: IncubationProposalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

`merge_candidate` 更新：

```rust
pub fn merge_candidate(&mut self, candidate: &ExperienceCandidate) {
    let ids = match candidate.kind_hint {
        ExperienceKindHint::Knowledge => &mut self.knowledge_candidate_ids,
        ExperienceKindHint::Skill => &mut self.skill_candidate_ids,
    };
    if !ids.contains(&candidate.candidate_id) {
        ids.push(candidate.candidate_id);
    }
    self.updated_at = chrono::Utc::now();
}
```

- [ ] **Step 11: 更新内联测试**

更新 `contribution.rs` 中所有内联测试，适配新类型。主要变更：
- `ExperienceCandidate::knowledge()` 调用移除 `memory_kind` 参数
- 移除 `ExperienceRiskLevel`、`ExperienceConfirmationPolicy` 相关断言
- `ExperienceKindHint::Executable` → `ExperienceKindHint::Skill`
- `ExperienceCandidatePayload::Executable` → `ExperienceCandidatePayload::Skill`
- `candidate_status_machine_has_required_states` 测试保留
- `experience_candidate_tracks_risk_metadata` 测试移除（risk 字段已删除）
- `SharedKnowledgeUpgradeCandidate` 相关测试移除

- [ ] **Step 12: 运行编译检查**

此步骤仅检查 contribution.rs 的变更是否自洽（其他文件尚未更新，预期编译失败）：

Run: `cargo check 2>&1 | head -50`

预期：大量编译错误来自其他文件引用已删除类型，这是正常的。确认 contribution.rs 本身无语法错误。

- [ ] **Step 13: 提交**

```bash
git add src/domain/contribution.rs
git commit -m "refactor: simplify experience candidate domain model

- ExperienceKindHint: Knowledge + Skill (remove Executable/SharedKnowledge/Discard)
- Remove ExperienceRiskLevel, ExperienceConfirmationPolicy enums
- ExperienceCandidatePayload: Knowledge{content} + Skill{name,description,instructions,file_refs}
- Add SkillFileRef, SkillFileRole types
- Remove risk_level/risk_reason/suggested_confirmation from ExperienceCandidate
- Remove SharedKnowledgeUpgradeCandidate
- Simplify ExperienceWritebackDestination (remove SharedKnowledgeUpgrade)
- Simplify ExperienceGovernanceDecision (remove risk fields)
- Rename executable_candidate_ids to skill_candidate_ids in IncubationProposal"
```

---

### Task 2: 领域模型重构 — memory.rs

**Files:**
- Modify: `src/domain/memory.rs`

**Interfaces:**
- Consumes: Task 1 的类型变更
- Produces: `LongTermMemoryEntry`（无 `kind` 字段），移除 `LongTermMemoryKind`

- [ ] **Step 1: 移除 LongTermMemoryKind 枚举**

删除 `LongTermMemoryKind` 枚举定义（Constraint/Preference/Strategy/Fact/AntiPattern）。

- [ ] **Step 2: 更新 LongTermMemoryEntry**

移除 `kind` 字段，更新 `new` 方法：

```rust
pub struct LongTermMemoryEntry {
    pub content: String,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub pin: bool,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub reuse_count: u32,
    pub decay_score: f32,
    pub source: String,
    pub confidence: f32,
    #[serde(default)]
    pub source_candidate_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub source_task_id: Option<super::TaskId>,
    #[serde(default)]
    pub agent_id: Option<super::AgentId>,
}

impl LongTermMemoryEntry {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            scope_tags: Vec::new(),
            importance: MemoryImportance::Medium,
            pin: false,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            decay_score: 1.0,
            source: "manual".to_string(),
            confidence: 0.8,
            source_candidate_id: None,
            source_task_id: None,
            agent_id: None,
        }
    }
}
```

注意：为兼容已持久化的旧数据，需添加 `#[serde(default)]` 使 `kind` 字段在反序列化时可选（旧 JSON 中有 `kind` 字段会被忽略）。由于 `kind` 字段已移除，旧数据中的 `kind` 会被 serde 忽略（Rust 的 serde 默认行为是忽略未知字段）。但为了安全，在 `MemorySnapshot` 中递增 `schema_version`。

- [ ] **Step 3: 更新 MemorySnapshot schema_version**

```rust
pub const CURRENT_SCHEMA_VERSION: u32 = 2;
```

- [ ] **Step 4: 更新 ExecutableMemoryEntry**

`ExecutableMemoryEntry` 保持不变（它是独立于 `LongTermMemoryKind` 的结构体）。

- [ ] **Step 5: 更新内联测试**

- `long_term_memory_entry_defaults_to_decay_ready_state`：移除 `kind` 字段断言
- `long_term_memory_entry_carries_source_traceability`：移除 `kind` 参数
- `add_entry_dedups_by_source_candidate_id`：移除 `kind` 参数
- `memory_snapshot_round_trip_serialization`：移除 `kind` 参数
- `long_term_memory_default_is_empty`：保持不变
- `estimate_tokens_returns_positive`：保持不变
- `executable_memory_entry_keeps_asset_refs_readable`：保持不变

- [ ] **Step 6: 提交**

```bash
git add src/domain/memory.rs
git commit -m "refactor: remove LongTermMemoryKind from domain memory model

- Remove LongTermMemoryKind enum (Constraint/Preference/Strategy/Fact/AntiPattern)
- Remove kind field from LongTermMemoryEntry
- Simplify LongTermMemoryEntry::new to accept content only
- Bump MemorySnapshot schema_version to 2"
```

---

### Task 3: 领域模型重构 — space.rs 和 mod.rs

**Files:**
- Modify: `src/domain/space.rs`
- Modify: `src/domain/mod.rs`

**Interfaces:**
- Consumes: Task 1, Task 2 的类型变更
- Produces: 更新后的 `SharedKnowledgeEntry`（`kind: String`），移除 `SharedKnowledgeUpgradeQueue`

- [ ] **Step 1: 更新 SharedKnowledgeEntry**

将 `kind: LongTermMemoryKind` 改为 `kind: String`：

```rust
pub struct SharedKnowledgeEntry {
    pub content: String,
    pub kind: String,  // 从 LongTermMemoryKind 改为 String
    pub scope_tags: Vec<String>,
    // ... 其余字段不变
}
```

更新 `approved_from_user_input`：

```rust
pub fn approved_from_user_input(content: impl Into<String>) -> Self {
    Self {
        content: content.into(),
        kind: "fact".to_string(),  // 默认值
        // ... 其余不变
    }
}
```

更新 `candidate`：移除 `kind` 参数：

```rust
pub fn candidate(content: impl Into<String>) -> Self {
    Self {
        content: content.into(),
        kind: "fact".to_string(),
        // ... 其余不变
    }
}
```

- [ ] **Step 2: 移除 SharedKnowledgeUpgradeQueue**

删除 `SharedKnowledgeUpgradeQueue` 结构体定义。

- [ ] **Step 3: 更新 domain/mod.rs 导出**

移除已删除类型的导出：
- 移除 `LongTermMemoryKind`
- 移除 `ExperienceRiskLevel`
- 移除 `ExperienceConfirmationPolicy`
- 移除 `SharedKnowledgeUpgradeCandidate`
- 移除 `SharedKnowledgeUpgradeQueue`
- 新增 `SkillFileRef`、`SkillFileRole`

- [ ] **Step 4: 提交**

```bash
git add src/domain/space.rs src/domain/mod.rs
git commit -m "refactor: update SharedKnowledgeEntry kind to String, remove SharedKnowledgeUpgradeQueue

- SharedKnowledgeEntry.kind: LongTermMemoryKind -> String
- SharedKnowledgeEntry::candidate() no longer takes kind parameter
- Remove SharedKnowledgeUpgradeQueue resource
- Update domain exports"
```

---

### Task 4: 工具层重构

**Files:**
- Modify: `src/systems/tools/mod.rs`
- Modify: `src/systems/tools/builtin/submit_experience_candidate.rs`
- Modify: `src/systems/tools/orchestrator.rs`

**Interfaces:**
- Consumes: Task 1 的新 `ExperienceCandidateSubmission` 结构
- Produces: 新的 JSON Schema、新的参数解析逻辑、新的 `submission_to_candidate` 转换

- [ ] **Step 1: 更新工具 JSON Schema**

在 `src/systems/tools/mod.rs` 中，更新 `submit_experience_candidate` 的 ToolDefinition：

```rust
registry.register(ToolDefinition {
    name: "submit_experience_candidate".to_string(),
    description: "提交经验候选。knowledge 类提交可复用知识，skill 类提交可复用技能包（对齐 Agent Skills 规范）。".to_string(),
    parameters: ToolSchema {
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "简明标题，概括此经验的核心要点"
                },
                "kind": {
                    "type": "string",
                    "enum": ["knowledge", "skill"],
                    "description": "经验类型：knowledge=可复用知识，skill=可复用技能包"
                },
                "content": {
                    "type": "string",
                    "description": "knowledge 类的经验正文"
                },
                "skill_description": {
                    "type": "string",
                    "description": "skill 类的简要描述，说明做什么+何时触发"
                },
                "instructions": {
                    "type": "string",
                    "description": "skill 类的分步指令正文"
                },
                "file_refs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "文件路径（绝对路径或相对于项目根目录的相对路径）"
                            },
                            "role": {
                                "type": "string",
                                "enum": ["script", "reference", "asset"],
                                "description": "文件角色，默认根据扩展名自动推断"
                            }
                        },
                        "required": ["path"]
                    },
                    "description": "skill 关联的资源文件列表"
                }
            },
            "required": ["title", "kind"]
        }),
    },
    default_permission: ToolPermission::Allow,
    executor: ToolExecutorKind::Builtin("submit_experience_candidate".to_string()),
    required_tag: None,
});
```

- [ ] **Step 2: 重写 SubmitExperienceCandidateTool::execute**

在 `src/systems/tools/builtin/submit_experience_candidate.rs` 中：

```rust
impl crate::domain::BuiltinTool for SubmitExperienceCandidateTool {
    fn name(&self) -> &str {
        "submit_experience_candidate"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let title = input
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing title".to_string()))?;

        let submission = ExperienceCandidateSubmission::from_json(
            ctx.current_task_id,
            ctx.current_agent_id,
            title,
            input,
        )?;

        // 验证：knowledge 类需要 content，skill 类需要 skill_description + instructions
        match submission.kind {
            ExperienceKindHint::Knowledge => {
                if submission.content.as_deref().unwrap_or("").is_empty() {
                    return Err(ToolError::InvalidInput(
                        "knowledge kind requires non-empty content".to_string(),
                    ));
                }
            }
            ExperienceKindHint::Skill => {
                if submission
                    .skill_description
                    .as_deref()
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(ToolError::InvalidInput(
                        "skill kind requires non-empty skill_description".to_string(),
                    ));
                }
                if submission.instructions.as_deref().unwrap_or("").is_empty() {
                    return Err(ToolError::InvalidInput(
                        "skill kind requires non-empty instructions".to_string(),
                    ));
                }
                // 验证 file_refs 中所有文件必须存在
                let missing: Vec<String> = submission
                    .file_refs
                    .iter()
                    .filter(|f| !std::path::Path::new(&f.path).exists())
                    .map(|f| f.path.clone())
                    .collect();
                if !missing.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "skill file_refs references non-existent files: {}",
                        missing.join(", ")
                    )));
                }
            }
        }

        Ok(ToolAction::SubmitExperienceCandidate(submission))
    }
}
```

更新测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceStore, SharedKnowledgeBase};

    #[test]
    fn submit_knowledge_candidate_returns_submit_action() {
        let knowledge = SharedKnowledgeBase::default();
        let store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = SubmitExperienceCandidateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "title": "shell timeout note",
                    "kind": "knowledge",
                    "content": "shell_stop 默认等待退出"
                }),
                &ctx,
            )
            .unwrap();

        assert!(matches!(action, ToolAction::SubmitExperienceCandidate(_)));
    }

    #[test]
    fn submit_skill_candidate_returns_submit_action() {
        let knowledge = SharedKnowledgeBase::default();
        let store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = SubmitExperienceCandidateTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "title": "smoke test skill",
                    "kind": "skill",
                    "skill_description": "Run smoke tests after shell changes",
                    "instructions": "1. Run shell smoke test\n2. Check output"
                }),
                &ctx,
            )
            .unwrap();

        assert!(matches!(action, ToolAction::SubmitExperienceCandidate(_)));
    }

    #[test]
    fn submit_experience_candidate_rejects_missing_title() {
        let knowledge = SharedKnowledgeBase::default();
        let store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = SubmitExperienceCandidateTool;
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn submit_knowledge_rejects_empty_content() {
        let knowledge = SharedKnowledgeBase::default();
        let store = ExperienceStore::default();
        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
        };

        let tool = SubmitExperienceCandidateTool;
        let result = tool.execute(
            &serde_json::json!({
                "title": "test",
                "kind": "knowledge",
                "content": ""
            }),
            &ctx,
        );
        assert!(result.is_err());
    }
}
```

- [ ] **Step 3: 重写 orchestrator 中的 submission_to_candidate**

在 `src/systems/tools/orchestrator.rs` 中，替换 `submission_to_candidate` 函数：

```rust
fn submission_to_candidate(
    submission: &ExperienceCandidateSubmission,
    agent_id: AgentId,
    task_id: TaskId,
) -> ExperienceCandidate {
    let payload = match &submission.kind {
        ExperienceKindHint::Knowledge => {
            let content = submission.content.clone().unwrap_or_default();
            ExperienceCandidatePayload::Knowledge { content }
        }
        ExperienceKindHint::Skill => {
            let name = submission.title.clone();
            let description = submission.skill_description.clone().unwrap_or_default();
            let instructions = submission.instructions.clone().unwrap_or_default();
            let file_refs = submission.file_refs.clone();
            ExperienceCandidatePayload::Skill {
                name,
                description,
                instructions,
                file_refs,
            }
        }
    };

    ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: task_id,
        producer_agent_id: agent_id,
        title: submission.title.clone(),
        kind_hint: submission.kind.clone(),
        payload,
        dependency_refs: Vec::new(),
        status: crate::domain::ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        derived_from_candidate_ids: Vec::new(),
    }
}
```

- [ ] **Step 4: 提交**

```bash
git add src/systems/tools/mod.rs src/systems/tools/builtin/submit_experience_candidate.rs src/systems/tools/orchestrator.rs
git commit -m "refactor: update submit_experience_candidate tool schema and parsing

- New JSON Schema: kind (knowledge/skill), content, skill_description, instructions, file_refs
- Remove payload/risk_level/risk_reason/suggested_confirmation/dependency_refs
- Add validation: knowledge requires content, skill requires skill_description + instructions
- Rewrite submission_to_candidate for new payload structure"
```

---

### Task 5: 经验系统重构 — governance.rs

**Files:**
- Modify: `src/systems/experience/governance.rs`

**Interfaces:**
- Consumes: Task 1 的 `ExperienceKindHint { Knowledge, Skill }`、`ExperienceGovernanceDecision`（无 risk 字段）
- Produces: 简化后的治理分流逻辑

- [ ] **Step 1: 简化治理分流逻辑**

替换 `experience_governance_system` 中的 match 分支。核心变更：

- 移除 `ExperienceKindHint::Discard` 分支（不再存在）
- 移除 `ExperienceKindHint::SharedKnowledge` 分支
- `ExperienceKindHint::Executable` → `ExperienceKindHint::Skill`
- 移除所有 `risk_level` 条件判断
- `ExperienceGovernanceDecision` 使用 `requires_user_confirmation: bool` 替代 `confirmation_policy`

治理分流逻辑：

```rust
ExperienceKindHint::Knowledge => {
    if is_default {
        ExperienceGovernanceDecision {
            candidate_id: *candidate_id,
            destination: ExperienceWritebackDestination::IncubationProposal,
            requires_user_confirmation: true,
            decision_rationale: "default agent knowledge -> incubation".to_string(),
            source_task_id: request.task_id,
        }
    } else {
        ExperienceGovernanceDecision {
            candidate_id: *candidate_id,
            destination: ExperienceWritebackDestination::LongTermMemory,
            requires_user_confirmation: false,
            decision_rationale: "persistent agent private knowledge".to_string(),
            source_task_id: request.task_id,
        }
    }
}
ExperienceKindHint::Skill => {
    if is_default {
        ExperienceGovernanceDecision {
            candidate_id: *candidate_id,
            destination: ExperienceWritebackDestination::IncubationProposal,
            requires_user_confirmation: true,
            decision_rationale: "default agent skill -> incubation".to_string(),
            source_task_id: request.task_id,
        }
    } else {
        ExperienceGovernanceDecision {
            candidate_id: *candidate_id,
            destination: ExperienceWritebackDestination::SkillPackage,
            requires_user_confirmation: true,
            decision_rationale: "skill requires user confirmation".to_string(),
            source_task_id: request.task_id,
        }
    }
}
```

- [ ] **Step 2: 更新确认分流逻辑**

将 `match decision.confirmation_policy` 改为 `if decision.requires_user_confirmation`：

```rust
if decision.requires_user_confirmation {
    // 需要用户确认
    if let Some(c) = store.candidates.get_mut(candidate_id) {
        c.status = ExperienceCandidateStatus::NeedsUserApproval;
    }
    if decision.destination == ExperienceWritebackDestination::IncubationProposal {
        spawn_incubation_confirmation(
            &mut commands, &mut store, request, agent, candidate_id,
        );
    } else {
        spawn_experience_confirmation(
            &mut commands, &mut store, request, candidate_id, &candidate,
        );
    }
    commands.spawn(decision);
} else {
    // 无需确认，直接进入 WritebackPending
    if let Some(c) = store.candidates.get_mut(candidate_id) {
        c.status = ExperienceCandidateStatus::WritebackPending;
    }
    commands.spawn(ExperienceWritebackRequestMessage {
        decision: decision.clone(),
    });
}
```

- [ ] **Step 3: 更新内联测试**

移除 `is_default_agent` 测试中对旧类型的依赖（如有），确保测试编译通过。

- [ ] **Step 4: 提交**

```bash
git add src/systems/experience/governance.rs
git commit -m "refactor: simplify experience governance branching logic

- Remove SharedKnowledge/Discard branches
- Rename Executable -> Skill
- Remove risk_level conditions
- Use requires_user_confirmation bool instead of ExperienceConfirmationPolicy"
```

---

### Task 6: 经验系统重构 — writeback.rs

**Files:**
- Modify: `src/systems/experience/writeback.rs`

**Interfaces:**
- Consumes: Task 1 的 `ExperienceCandidatePayload::Skill`、`SkillFileRef`、`SkillFileRole`
- Produces: 移除 SharedKnowledgeUpgrade 写回，更新 Skill 写回产出 SKILL.md

- [ ] **Step 1: 移除 SharedKnowledgeUpgrade 相关代码**

- 从函数签名中移除 `upgrade_queue: ResMut<SharedKnowledgeUpgradeQueue>` 和 `upgrade_service: Res<SharedKnowledgeUpgradeService>` 参数
- 移除 `ExperienceWritebackDestination::SharedKnowledgeUpgrade` match 分支
- 删除 `writeback_to_shared_knowledge_upgrade` 函数

- [ ] **Step 2: 更新 Skill 写回逻辑**

替换 `writeback_to_skill_package` 中的 `ExperienceCandidatePayload::Executable` 匹配为 `ExperienceCandidatePayload::Skill`：

```rust
fn writeback_to_skill_package(
    candidate: &crate::domain::ExperienceCandidate,
    agents: &Query<&Agent>,
    asset_service: &crate::infrastructure::assets::AgentAssetService,
) -> Result<(), String> {
    let governing_agent_id = candidate
        .governing_agent_id
        .ok_or_else(|| "no governing_agent_id".to_string())?;
    let agent = agents
        .iter()
        .find(|a| a.id == governing_agent_id)
        .ok_or_else(|| format!("agent {} not found", governing_agent_id))?;

    let crate::domain::ExperienceCandidatePayload::Skill {
        name,
        description,
        instructions,
        file_refs,
    } = &candidate.payload
    else {
        return Err("candidate payload is not skill".to_string());
    };

    let draft = crate::infrastructure::assets::SkillPackageDraft {
        skill_id: format!("{}", candidate.candidate_id),
        title: candidate.title.clone(),
        name: name.clone(),
        description: description.clone(),
        instructions: instructions.clone(),
        file_refs: file_refs.clone(),
        source_task_id: Some(candidate.producer_task_id),
        source_candidate_id: Some(candidate.candidate_id),
    };
    asset_service
        .persist_skill_package(&agent.profile.name, &draft)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 3: 更新 incubation 写回中的知识候选处理**

`writeback_incubation_proposal` 中 `candidate.as_long_term_memory_entry()` 已不需要 `memory_kind`，确认调用点适配。

- [ ] **Step 4: 更新内联测试**

- `description_builds_from_candidate_titles`：`ExperienceCandidate::knowledge()` 移除 `memory_kind` 参数
- `incubation_writeback_persists_knowledge_to_ltm_and_agents_toml`：同上
- 移除 `LongTermMemoryKind` import

- [ ] **Step 5: 提交**

```bash
git add src/systems/experience/writeback.rs
git commit -m "refactor: update experience writeback for simplified model

- Remove SharedKnowledgeUpgrade writeback path
- Update Skill writeback to use new SkillPackageDraft
- Remove upgrade_queue and upgrade_service from system signature"
```

---

### Task 7: 经验系统重构 — collection.rs 和 approval.rs

**Files:**
- Modify: `src/systems/experience/collection.rs`
- Modify: `src/systems/experience/approval.rs`

**Interfaces:**
- Consumes: Task 1 的类型变更

- [ ] **Step 1: 更新 collection.rs**

- `ExperienceCandidate::knowledge()` 调用移除 `memory_kind` 参数
- 确认所有 `LongTermMemoryKind` 引用已移除

- [ ] **Step 2: 更新 approval.rs**

- 移除 `ExperienceConfirmationPolicy` import
- 更新内联测试：`ExperienceKindHint::Executable` → `ExperienceKindHint::Skill`，`ExperienceCandidatePayload::Executable` → `ExperienceCandidatePayload::Skill`，移除 `risk_level`/`risk_reason`/`suggested_confirmation` 字段

approval.rs 内联测试更新：

```rust
#[test]
fn approved_skill_becomes_persisted() {
    let mut store = ExperienceStore::default();
    let request_id = uuid::Uuid::new_v4();
    let candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "test skill".to_string(),
        kind_hint: ExperienceKindHint::Skill,
        payload: ExperienceCandidatePayload::Skill {
            name: "test-skill".to_string(),
            description: "run smoke test".to_string(),
            instructions: "1. Run test".to_string(),
            file_refs: vec![],
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::NeedsUserApproval,
        governing_agent_id: None,
        derived_from_candidate_ids: vec![],
    };
    let candidate_id = candidate.candidate_id;
    store.stage_root_candidate(candidate);
    store.bind_approval_request(request_id, candidate_id);
    store.apply_confirmation_response(request_id, "approve");

    assert_eq!(
        store.candidates.get(&candidate_id).unwrap().status,
        ExperienceCandidateStatus::Approved,
        "approved skill should be marked Approved"
    );
}
```

- [ ] **Step 3: 提交**

```bash
git add src/systems/experience/collection.rs src/systems/experience/approval.rs
git commit -m "refactor: update experience collection and approval for simplified types"
```

---

### Task 8: 基础设施层和插件更新

**Files:**
- Modify: `src/infrastructure/memory/service.rs`
- Delete: `src/infrastructure/memory/upgrade_service.rs`
- Modify: `src/infrastructure/memory/mod.rs`
- Modify: `src/infrastructure/assets/service.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/plugins/memory.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: 更新 infrastructure/memory/service.rs**

移除 `LongTermMemoryKind` import，更新内联测试中 `LongTermMemoryEntry::new()` 调用（移除 kind 参数）：

```rust
// 之前
LongTermMemoryEntry::new(LongTermMemoryKind::Fact, "persisted fact")
// 之后
LongTermMemoryEntry::new("persisted fact")
```

- [ ] **Step 2: 删除 infrastructure/memory/upgrade_service.rs**

删除整个文件。

- [ ] **Step 3: 更新 infrastructure/memory/mod.rs**

移除 `upgrade_service` 模块导出。

- [ ] **Step 4: 更新 infrastructure/assets/service.rs**

更新 `SkillPackageDraft` 结构体，对齐新的 Skill 模型：

```rust
pub struct SkillPackageDraft {
    pub skill_id: String,
    pub title: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub file_refs: Vec<crate::domain::SkillFileRef>,
    pub source_task_id: Option<TaskId>,
    pub source_candidate_id: Option<uuid::Uuid>,
}
```

更新 `persist_skill_package` 方法，生成 Agent Skills 规范的 SKILL.md 目录结构：

```rust
pub fn persist_skill_package(
    &self,
    agent_name: &str,
    draft: &SkillPackageDraft,
) -> Result<String> {
    // skill-name: title 转为小写连字符格式
    let skill_name = draft.name
        .to_lowercase()
        .replace(' ', "-")
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "");

    let skill_dir = self.base_dir.join(agent_name).join("skills").join(&skill_name);
    fs::create_dir_all(&skill_dir)
        .with_context(|| format!("failed to create skill dir {}", skill_dir.display()))?;

    // 生成 SKILL.md
    let skill_md = format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        skill_name,
        draft.description,
        draft.instructions,
    );
    let skill_md_path = skill_dir.join("SKILL.md");
    fs::write(&skill_md_path, &skill_md)
        .with_context(|| format!("failed to write {}", skill_md_path.display()))?;

    // 复制关联文件到对应子目录
    for file_ref in &draft.file_refs {
        let sub_dir = match file_ref.role {
            crate::domain::SkillFileRole::Script => "scripts",
            crate::domain::SkillFileRole::Reference => "references",
            crate::domain::SkillFileRole::Asset => "assets",
        };
        let dest_dir = skill_dir.join(sub_dir);
        fs::create_dir_all(&dest_dir)
            .with_context(|| format!("failed to create {}", dest_dir.display()))?;

        let src_path = std::path::Path::new(&file_ref.path);
        let file_name = src_path.file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid file path: {}", file_ref.path))?;
        let dest_path = dest_dir.join(file_name);

        if src_path.exists() {
            fs::copy(src_path, &dest_path)
                .with_context(|| format!("failed to copy {} to {}", file_ref.path, dest_path.display()))?;
        }
        // 注意：提交阶段已验证文件存在性，此处不应出现文件不存在的情况
    }

    Ok(format!("{}/skills/{}", agent_name, skill_name))
}
```

- [ ] **Step 5: 更新 app/mod.rs**

移除 `SharedKnowledgeUpgradeService` 和 `SharedKnowledgeUpgradeQueue` 的初始化代码：

```rust
// 删除以下代码：
// let upgrade_service = crate::infrastructure::memory::SharedKnowledgeUpgradeService::default_path();
// let upgrade_queue = upgrade_service.load().unwrap_or_default();
// app.insert_resource(upgrade_service);
// app.insert_resource(upgrade_queue);
```

- [ ] **Step 6: 更新 plugins/memory.rs**

确认无 `SharedKnowledgeUpgradeQueue` 注册（当前代码中无，无需修改）。

- [ ] **Step 7: 更新 lib.rs**

移除 `SkillPackageDraft` 的公开导出（如果 `SkillPackageDraft` 仍需公开则保留，但字段已变更）。

- [ ] **Step 8: 提交**

```bash
git add src/infrastructure/memory/service.rs src/infrastructure/memory/mod.rs src/infrastructure/assets/service.rs src/app/mod.rs src/lib.rs
git rm src/infrastructure/memory/upgrade_service.rs
git commit -m "refactor: update infrastructure layer for simplified experience model

- Remove SharedKnowledgeUpgradeService
- Update SkillPackageDraft for new Skill model
- Generate SKILL.md directory structure (Agent Skills spec)
- Remove SharedKnowledgeUpgradeQueue from app init"
```

---

### Task 9: 其他系统适配

**Files:**
- Modify: `src/systems/memory.rs`
- Modify: `src/systems/dispatch/memory_selection.rs`
- Modify: `src/systems/dispatch/task_dispatch.rs`
- Modify: `src/systems/tools/builtin/knowledge_search.rs`
- Modify: `src/systems/tools/builtin/list_experience_candidates.rs`
- Modify: `src/infrastructure/memory/json_file_store.rs`

- [ ] **Step 1: 更新 systems/memory.rs 测试**

移除 `LongTermMemoryKind` import，更新 `LongTermMemoryEntry` 构造（移除 `kind` 字段）。

- [ ] **Step 2: 更新 systems/dispatch/memory_selection.rs 测试**

移除 `LongTermMemoryKind` import，更新 `LongTermMemoryEntry` 构造（移除 `kind` 字段）。

- [ ] **Step 3: 更新 systems/dispatch/task_dispatch.rs 测试**

移除 `LongTermMemoryKind` import，更新 `LongTermMemoryEntry` 构造（移除 `kind` 字段）。

- [ ] **Step 4: 更新 systems/tools/builtin/knowledge_search.rs 测试**

移除 `LongTermMemoryKind` import，更新 `SharedKnowledgeEntry::candidate()` 调用（移除 `kind` 参数）。

- [ ] **Step 5: 更新 systems/tools/builtin/list_experience_candidates.rs 测试**

移除 `LongTermMemoryKind` import，更新 `ExperienceCandidate::knowledge()` 调用（移除 `memory_kind` 参数）。

- [ ] **Step 6: 检查 json_file_store.rs**

确认 `json_file_store.rs` 中无 `LongTermMemoryKind` 引用（如有则移除）。

- [ ] **Step 7: 提交**

```bash
git add src/systems/memory.rs src/systems/dispatch/memory_selection.rs src/systems/dispatch/task_dispatch.rs src/systems/tools/builtin/knowledge_search.rs src/systems/tools/builtin/list_experience_candidates.rs src/infrastructure/memory/json_file_store.rs
git commit -m "refactor: adapt remaining systems to remove LongTermMemoryKind"
```

---

### Task 10: 集成测试更新

**Files:**
- Modify: `tests/experience_candidate_flow.rs`
- Modify: `tests/experience_collection_workitem_flow.rs`
- Modify: `tests/experience_layered_governance_flow.rs`
- Modify: `tests/incubation_execution_flow.rs`
- Modify: `tests/memory_persistence_flow.rs`

- [ ] **Step 1: 更新 experience_candidate_flow.rs**

- `ExperienceKindHint::Executable` → `ExperienceKindHint::Skill`
- `ExperienceCandidatePayload::Executable` → `ExperienceCandidatePayload::Skill`
- `ExperienceCandidate::knowledge()` 移除 `memory_kind` 参数
- 移除 `risk_level`/`risk_reason`/`suggested_confirmation` 字段
- 移除 `ExperienceRiskLevel`/`ExperienceConfirmationPolicy` 引用

- [ ] **Step 2: 更新 experience_collection_workitem_flow.rs**

- `ExperienceCandidate::knowledge()` 移除 `memory_kind` 参数
- 移除 `LongTermMemoryKind` 引用

- [ ] **Step 3: 更新 experience_layered_governance_flow.rs**

- 移除 `SharedKnowledge` 相关断言
- `ExperienceKindHint::Executable` → `ExperienceKindHint::Skill`
- 移除 `ExperienceRiskLevel`/`ExperienceConfirmationPolicy` 引用
- `ExperienceCandidate::knowledge()` 移除 `memory_kind` 参数

- [ ] **Step 4: 更新 incubation_execution_flow.rs**

- `ExperienceCandidate::knowledge()` 移除 `memory_kind` 参数
- 移除 `LongTermMemoryKind` 引用

- [ ] **Step 5: 更新 memory_persistence_flow.rs**

- `LongTermMemoryEntry::new()` 移除 `kind` 参数
- 移除 `LongTermMemoryKind` 引用

- [ ] **Step 6: 运行完整测试**

Run: `cargo test --all-features`

预期：所有测试通过。

- [ ] **Step 7: 提交**

```bash
git add tests/
git commit -m "test: update integration tests for simplified experience model"
```

---

### Task 11: 验证与文档更新

**Files:**
- Modify: `docs/current-state.md`

- [ ] **Step 1: 运行完整验证**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

预期：全部通过。

- [ ] **Step 2: 更新 docs/current-state.md**

更新经验治理模块描述，反映简化后的设计：
- kind_hint 简化为 knowledge/skill
- 移除 LongTermMemoryKind、ExperienceRiskLevel、ExperienceConfirmationPolicy
- Skill 写回产出 SKILL.md 目录结构（对齐 Agent Skills 规范）
- 移除 SharedKnowledgeUpgrade 路径

- [ ] **Step 3: 提交**

```bash
git add docs/current-state.md
git commit -m "docs: update current-state for simplified experience governance model"
```
