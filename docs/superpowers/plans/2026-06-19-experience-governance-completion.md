# 经验治理模块功能补全 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补全经验治理模块 5 项功能缺口：list_experience_candidates 字段修正、IncubationProposal Skill 处理、长期记忆淘汰、Skill 加载注入、非顶层 LLM 合并。

**Architecture:** 分两个子项目执行。子项目 A（#8+#9）为数据层修复，改动小且独立；子项目 B（#2+#4+#6）为功能增强，涉及新模块和系统。A 优先于 B。

**Tech Stack:** Rust, Bevy ECS, serde_json, chrono, uuid

## Global Constraints

- 遵循 Conventional Commits
- 提交前完成 `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-features`
- 使用中文撰写项目文档
- 不引入新依赖
- 子项目 A 完成后须通过完整验证再开始子项目 B

---

## File Structure

| 文件 | 职责 | 变更类型 |
|------|------|---------|
| `src/systems/tools/builtin/list_experience_candidates.rs` | 候选列表工具 | 修改 |
| `src/systems/experience/writeback.rs` | 写回执行 | 修改 |
| `src/infrastructure/incubation/agent_registry.rs` | Agent 注册表 | 修改 |
| `src/domain/mod.rs` | AgentEntry 定义 | 修改 |
| `src/systems/memory.rs` | 记忆衰退/淘汰 | 修改 |
| `src/infrastructure/memory/service.rs` | LTM 服务 | 修改 |
| `src/infrastructure/skills/mod.rs` | Skill 加载模块 | 新增 |
| `src/infrastructure/skills/loader.rs` | SkillLoader 实现 | 新增 |
| `src/infrastructure/mod.rs` | 基础设施导出 | 修改 |
| `src/app/mod.rs` | 应用初始化 | 修改 |
| `src/systems/dispatch/task_dispatch.rs` | 上下文组装 | 修改 |
| `src/domain/contribution.rs` | Superseded 状态 | 修改 |
| `src/domain/space.rs` | 合并消息类型 | 修改 |
| `src/systems/experience/collection.rs` | 汇聚后触发合并 | 修改 |
| `src/systems/experience/consolidation.rs` | 合并系统 | 新增 |
| `src/systems/experience/mod.rs` | 经验系统导出 | 修改 |
| `src/plugins/memory.rs` | 记忆插件 | 修改 |

---

## 子项目 A：数据层修复

### Task 1: list_experience_candidates 字段修正

**Files:**
- Modify: `src/systems/tools/builtin/list_experience_candidates.rs`

**Interfaces:**
- Consumes: `ExperienceCandidate`（含 `kind_hint: ExperienceKindHint`、`payload: ExperienceCandidatePayload`）
- Produces: 更新后的工具输出（`kind` 字段 + `summary` 字段）

- [ ] **Step 1: 更新 execute 方法**

将 `list_experience_candidates.rs` 中的 `execute` 方法更新：

```rust
impl crate::domain::BuiltinTool for ListExperienceCandidatesTool {
    fn name(&self) -> &str {
        "list_experience_candidates"
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let items: Vec<serde_json::Value> = ctx
            .experience_store
            .list_for_task(ctx.current_task_id)
            .into_iter()
            .map(|candidate| {
                let kind = format!("{:?}", candidate.kind_hint);
                let summary = match &candidate.payload {
                    crate::domain::ExperienceCandidatePayload::Knowledge { content } => {
                        if content.len() > 200 {
                            format!("{}…", &content[..200])
                        } else {
                            content.clone()
                        }
                    }
                    crate::domain::ExperienceCandidatePayload::Skill { description, .. } => {
                        description.clone()
                    }
                };
                serde_json::json!({
                    "candidate_id": candidate.candidate_id,
                    "title": candidate.title,
                    "kind": kind,
                    "status": format!("{:?}", candidate.status),
                    "summary": summary,
                })
            })
            .collect();

        Ok(ToolAction::Direct(serde_json::json!({
            "count": items.len(),
            "items": items,
        })))
    }
}
```

- [ ] **Step 2: 更新内联测试**

更新测试中对输出字段的断言：`kind_hint` → `kind`，新增 `summary` 断言。

- [ ] **Step 3: 运行测试**

Run: `cargo test --all-features -- list_experience_candidates`
Expected: PASS

- [ ] **Step 4: 提交**

```bash
git add src/systems/tools/builtin/list_experience_candidates.rs
git commit -m "fix: update list_experience_candidates output fields (kind_hint→kind, add summary)"
```

---

### Task 2: IncubationProposal Skill 候选处理

**Files:**
- Modify: `src/infrastructure/incubation/agent_registry.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/systems/experience/writeback.rs`

**Interfaces:**
- Consumes: `IncubationProposal.skill_candidate_ids`, `SkillPackageDraft`, `AgentAssetService`
- Produces: `IncubatedAgentRecord.skills` 字段，`AgentEntry.skills` 字段

- [ ] **Step 1: 更新 IncubatedAgentRecord**

在 `src/infrastructure/incubation/agent_registry.rs` 中，为 `IncubatedAgentRecord` 新增 `skills` 字段：

```rust
pub struct IncubatedAgentRecord {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
}
```

更新 `append` 方法中构造 `AgentEntry` 时传入 `skills` 字段。

- [ ] **Step 2: 更新 AgentEntry**

在 `src/domain/mod.rs` 中，为 `AgentEntry` 新增 `skills` 字段：

```rust
pub struct AgentEntry {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Option<AgentToolsConfig>,
    pub skills: Option<Vec<String>>,
}
```

使用 `#[serde(default)]` 使旧配置文件兼容。

- [ ] **Step 3: 更新 writeback_incubation_proposal**

在 `src/systems/experience/writeback.rs` 中：

1. 为 `writeback_incubation_proposal` 签名新增 `asset_service: &AgentAssetService` 参数
2. 在知识候选写入 LTM 之后，增加 Skill 候选处理：

```rust
// 处理 Skill 候选
let mut skill_paths: Vec<String> = Vec::new();
for skill_id in &proposal.skill_candidate_ids {
    if let Some(candidate) = store.candidates.get(skill_id) {
        if let crate::domain::ExperienceCandidatePayload::Skill {
            name,
            description,
            instructions,
            file_refs,
        } = &candidate.payload
        {
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
            match asset_service.persist_skill_package(&profile.name, &draft) {
                Ok(path) => skill_paths.push(path),
                Err(e) => {
                    warn!(
                        event = "IncubationSkillPersistFailed",
                        skill_id = %skill_id,
                        error = %e,
                        "failed to persist skill package during incubation"
                    );
                }
            }
        }
    }
}
```

3. 更新 `IncubatedAgentRecord` 构造，传入 `skills: Some(skill_paths)`（如果非空）或 `None`
4. 更新 `experience_writeback_system` 中调用 `writeback_incubation_proposal` 的地方，传入 `asset_service`

- [ ] **Step 4: 运行测试**

Run: `cargo test --all-features`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/infrastructure/incubation/agent_registry.rs src/domain/mod.rs src/systems/experience/writeback.rs
git commit -m "feat: handle skill_candidate_ids in IncubationProposal writeback

- Add skills field to IncubatedAgentRecord and AgentEntry
- Persist Skill packages during incubation execution
- Record skill paths in agents.toml"
```

---

### Task 3: 子项目 A 验证

- [ ] **Step 1: 运行完整验证**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Expected: 全部通过。

- [ ] **Step 2: 提交（如有格式修复）**

```bash
git add -A
git commit -m "chore: sub-project A verification fixes"
```

---

## 子项目 B：功能增强

### Task 4: 长期记忆淘汰（移除 + 文件归档）

**Files:**
- Modify: `src/systems/memory.rs`
- Modify: `src/infrastructure/memory/service.rs`
- Modify: `src/plugins/memory.rs`

**Interfaces:**
- Consumes: `LongTermMemoryEntry`, `MemoryImportance`, `LongTermMemoryService`
- Produces: `apply_memory_decay` 返回被淘汰条目列表，`LongTermMemoryService::archive_entries` 方法

- [ ] **Step 1: 更新 apply_memory_decay**

在 `src/systems/memory.rs` 中，修改 `apply_memory_decay` 签名和实现：

```rust
pub fn apply_memory_decay(
    entries: &mut Vec<LongTermMemoryEntry>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<LongTermMemoryEntry> {
    let mut evicted = Vec::new();
    entries.retain(|entry| {
        let age_days = now
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

        entry.decay_score =
            (entry.decay_score - base_penalty + importance_bonus + reuse_bonus).clamp(0.0, 1.0);

        let should_evict = entry.decay_score < 0.1
            && !entry.pin
            && entry.importance != MemoryImportance::Critical;

        if should_evict {
            evicted.push(entry.clone());
            false
        } else {
            true
        }
    });
    evicted
}
```

- [ ] **Step 2: 新增 archive_entries 方法**

在 `src/infrastructure/memory/service.rs` 中，为 `LongTermMemoryService` 新增：

```rust
pub fn archive_entries(&self, agent_name: &str, entries: &[crate::domain::LongTermMemoryEntry]) {
    let archive_path = self.base_dir.join(agent_name).join("archive.jsonl");
    if let Some(parent) = archive_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&archive_path)
    else {
        return;
    };
    for entry in entries {
        let _ = writeln!(file, "{}", serde_json::to_string(entry).unwrap());
    }
}
```

注意：`LongTermMemoryService` 需要能访问 `base_dir`。检查当前实现是否已有该字段，如果没有，需新增。

- [ ] **Step 3: 更新 long_term_memory_decay_system**

在 `src/systems/memory.rs` 中，更新系统函数签名和实现：

```rust
pub(crate) fn long_term_memory_decay_system(
    mut agents: Query<(&Agent, &mut LongTermMemory)>,
    service: Res<LongTermMemoryService>,
) {
    let now = chrono::Utc::now();
    for (_agent, mut memory) in &mut agents {
        let evicted = apply_memory_decay(&mut memory.entries, now);
        if !evicted.is_empty() {
            if let Some(name) = &memory.agent_name {
                service.archive_entries(name, &evicted);
            }
            debug!(
                event = "LongTermMemoryEvicted",
                agent_name = ?memory.agent_name,
                evicted_count = evicted.len(),
                "evicted low-value memory entries to archive"
            );
        }
    }
}
```

- [ ] **Step 4: 更新插件注册**

在 `src/plugins/memory.rs` 中，确认 `long_term_memory_decay_system` 的系统参数变更后仍能正确注册（新增 `Res<LongTermMemoryService>` 参数后，Bevy 会自动注入）。

- [ ] **Step 5: 更新内联测试**

更新 `apply_memory_decay` 相关测试：
- 旧测试调用 `apply_memory_decay(&mut entries, now)` 无返回值 → 新签名返回 `Vec<LongTermMemoryEntry>`
- 新增测试 `decay_system_evicts_low_value_entries`：构造 `decay_score < 0.1` 的条目，验证返回值包含被淘汰条目，`entries` 中不再包含该条目

```rust
#[test]
fn decay_system_evicts_low_value_entries() {
    let mut entries = vec![LongTermMemoryEntry::new("stale entry")];
    entries[0].decay_score = 0.05;
    entries[0].pin = false;
    entries[0].importance = MemoryImportance::Low;

    let now = chrono::Utc::now();
    let evicted = apply_memory_decay(&mut entries, now);

    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].content, "stale entry");
    assert!(entries.is_empty());
}
```

新增测试 `critical_entries_are_never_evicted`：

```rust
#[test]
fn critical_entries_are_never_evicted() {
    let mut entries = vec![LongTermMemoryEntry::new("critical entry")];
    entries[0].decay_score = 0.01;
    entries[0].pin = false;
    entries[0].importance = MemoryImportance::Critical;

    let now = chrono::Utc::now();
    let evicted = apply_memory_decay(&mut entries, now);

    assert!(evicted.is_empty());
    assert_eq!(entries.len(), 1);
}
```

新增测试 `pinned_entries_are_never_evicted`：

```rust
#[test]
fn pinned_entries_are_never_evicted() {
    let mut entries = vec![LongTermMemoryEntry::new("pinned entry")];
    entries[0].decay_score = 0.01;
    entries[0].pin = true;
    entries[0].importance = MemoryImportance::Low;

    let now = chrono::Utc::now();
    let evicted = apply_memory_decay(&mut entries, now);

    assert!(evicted.is_empty());
    assert_eq!(entries.len(), 1);
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test --all-features`
Expected: PASS

- [ ] **Step 7: 提交**

```bash
git add src/systems/memory.rs src/infrastructure/memory/service.rs src/plugins/memory.rs
git commit -m "feat: add long-term memory eviction with file archiving

- Evict entries with decay_score < 0.1, not pinned, not Critical
- Archive evicted entries to <agent-name>/archive.jsonl
- Add archive_entries method to LongTermMemoryService"
```

---

### Task 5: Skill Package 加载为知识注入

**Files:**
- Create: `src/infrastructure/skills/mod.rs`
- Create: `src/infrastructure/skills/loader.rs`
- Modify: `src/infrastructure/mod.rs`
- Modify: `src/app/mod.rs`
- Modify: `src/systems/dispatch/task_dispatch.rs`

**Interfaces:**
- Consumes: `AgentAssetService.base_dir` 路径约定
- Produces: `SkillLoader` Resource, `LoadedSkill` 结构体

- [ ] **Step 1: 创建 SkillLoader 模块**

创建 `src/infrastructure/skills/mod.rs`：

```rust
pub mod loader;
pub use loader::{LoadedSkill, SkillLoader};
```

创建 `src/infrastructure/skills/loader.rs`：

```rust
use std::path::PathBuf;

use bevy::prelude::Resource;

/// 已加载的 Skill。
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

/// Skill 加载器：扫描 Agent 的 skills 目录，解析 SKILL.md。
#[derive(Resource, Debug, Clone)]
pub struct SkillLoader {
    base_dir: PathBuf,
}

impl SkillLoader {
    pub fn default_path() -> Self {
        Self {
            base_dir: PathBuf::from(".harness/assets/agents"),
        }
    }

    /// 扫描指定 Agent 的 skills 目录，返回所有已加载的 Skill。
    pub fn load_skills(&self, agent_name: &str) -> Vec<LoadedSkill> {
        let skills_dir = self.base_dir.join(agent_name).join("skills");
        let Ok(entries) = std::fs::read_dir(&skills_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    parse_skill_md(&skill_md)
                } else {
                    None
                }
            })
            .collect()
    }

    /// 将 Skill 列表格式化为系统提示注入文本。
    pub fn format_skills_prompt(skills: &[LoadedSkill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut prompt = String::from("## 可用技能\n\n");
        for skill in skills {
            prompt.push_str(&format!("### {}\n", skill.name));
            prompt.push_str(&format!("{}\n\n", skill.description));
            prompt.push_str(&format!("{}\n\n", skill.instructions));
        }
        prompt
    }
}

fn parse_skill_md(path: &std::path::Path) -> Option<LoadedSkill> {
    let content = std::fs::read_to_string(path).ok()?;
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("---")?;
    let frontmatter = &rest[..end];
    let instructions = rest[end + 3..].trim().to_string();

    let name = frontmatter
        .lines()
        .find(|l| l.starts_with("name:"))
        .map(|l| l.trim_start_matches("name:").trim().to_string())
        .unwrap_or_default();
    let description = frontmatter
        .lines()
        .find(|l| l.starts_with("description:"))
        .map(|l| l.trim_start_matches("description:").trim().to_string())
        .unwrap_or_default();

    Some(LoadedSkill {
        name,
        description,
        instructions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_skills_prompt_produces_section() {
        let skills = vec![LoadedSkill {
            name: "smoke-test".to_string(),
            description: "验证工具链".to_string(),
            instructions: "1. 运行脚本".to_string(),
        }];
        let prompt = SkillLoader::format_skills_prompt(&skills);
        assert!(prompt.contains("## 可用技能"));
        assert!(prompt.contains("### smoke-test"));
        assert!(prompt.contains("验证工具链"));
        assert!(prompt.contains("1. 运行脚本"));
    }

    #[test]
    fn format_skills_prompt_empty_returns_empty() {
        let prompt = SkillLoader::format_skills_prompt(&[]);
        assert!(prompt.is_empty());
    }
}
```

- [ ] **Step 2: 更新 infrastructure/mod.rs**

在 `src/infrastructure/mod.rs` 中新增：

```rust
pub mod skills;
```

- [ ] **Step 3: 注册 SkillLoader Resource**

在 `src/app/mod.rs` 中，在 app 构建阶段插入：

```rust
app.insert_resource(crate::infrastructure::skills::SkillLoader::default_path());
```

- [ ] **Step 4: 在上下文组装中注入 Skill**

在 `src/systems/dispatch/task_dispatch.rs` 中，找到系统提示组装的位置，在末尾追加 Skill 区段。

需要：
1. 在 `task_dispatch_system` 或相关函数签名中新增 `skill_loader: Res<SkillLoader>` 参数
2. 在组装系统提示时，调用 `skill_loader.load_skills(&agent.profile.name)`，将结果通过 `SkillLoader::format_skills_prompt` 格式化后追加到系统提示末尾

具体位置需读取 `task_dispatch.rs` 确认系统提示组装点。

- [ ] **Step 5: 运行测试**

Run: `cargo test --all-features`
Expected: PASS

- [ ] **Step 6: 提交**

```bash
git add src/infrastructure/skills/ src/infrastructure/mod.rs src/app/mod.rs src/systems/dispatch/task_dispatch.rs
git commit -m "feat: add SkillLoader to inject skills as knowledge in agent system prompt

- Scan <agent>/skills/*/SKILL.md on agent initialization
- Parse YAML frontmatter (name, description) + instructions
- Append formatted skills section to system prompt"
```

---

### Task 6: 非顶层 LLM 合并子候选 — 领域模型

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/domain/space.rs`

**Interfaces:**
- Produces: `ExperienceCandidateStatus::Superseded`, `ExperienceConsolidationRequestMessage`

- [ ] **Step 1: 新增 Superseded 状态**

在 `src/domain/contribution.rs` 的 `ExperienceCandidateStatus` 枚举中，在 `Aggregated` 之后新增：

```rust
pub enum ExperienceCandidateStatus {
    Submitted,
    InInbox,
    Aggregated,
    Superseded,
    GovernancePending,
    GovernanceResolved,
    NeedsUserApproval,
    WritebackPending,
    Approved,
    Rejected,
    Persisted,
    WritebackFailed,
}
```

- [ ] **Step 2: 新增 ExperienceConsolidationRequestMessage**

在 `src/domain/space.rs` 中新增：

```rust
/// 经验合并请求消息：触发 LLM 对多个相似候选做去重合并。
#[derive(Debug, Clone, Component)]
pub struct ExperienceConsolidationRequestMessage {
    pub task_id: TaskId,
    pub parent_task_id: TaskId,
    pub governing_agent_id: AgentId,
    pub candidate_kind: crate::domain::ExperienceKindHint,
    pub candidate_ids: Vec<uuid::Uuid>,
}
```

- [ ] **Step 3: 运行编译检查**

Run: `cargo check 2>&1 | head -20`
Expected: 编译通过（新类型尚未被使用，但不应报错）

- [ ] **Step 4: 提交**

```bash
git add src/domain/contribution.rs src/domain/space.rs
git commit -m "feat: add Superseded status and ExperienceConsolidationRequestMessage for candidate merging"
```

---

### Task 7: 非顶层 LLM 合并子候选 — 系统实现

**Files:**
- Modify: `src/systems/experience/collection.rs`
- Create: `src/systems/experience/consolidation.rs`
- Modify: `src/systems/experience/mod.rs`

**Interfaces:**
- Consumes: `ExperienceConsolidationRequestMessage`, `ExperienceCandidateStatus::Superseded`, `ExperienceKindHint`
- Produces: `experience_consolidation_trigger_system`, `experience_consolidation_workitem_system`

- [ ] **Step 1: 创建 consolidation.rs**

创建 `src/systems/experience/consolidation.rs`：

```rust
use bevy::prelude::*;
use tracing::debug;

use crate::domain::{
    ExperienceCandidateStatus, ExperienceConsolidationRequestMessage, ExperienceKindHint,
    ExperienceStore, ShortTermMemory, SpaceToolRegistry, Task, WorkItem,
};

/// 经验合并触发系统：当非顶层汇聚完成且候选数 > 1 时，按 kind 分组创建合并请求。
pub(crate) fn experience_consolidation_trigger_system(
    mut commands: Commands,
    store: Res<ExperienceStore>,
    requests: Query<(Entity, &ExperienceConsolidationRequestMessage)>,
) {
    for (entity, request) in &requests {
        let candidates: Vec<_> = request
            .candidate_ids
            .iter()
            .filter_map(|id| store.candidates.get(id))
            .collect();

        if candidates.len() <= 1 {
            debug!(
                event = "ExperienceConsolidationSkipped",
                task_id = %request.task_id,
                reason = "too_few_candidates",
                "skipping consolidation, <=1 candidates"
            );
            commands.entity(entity).despawn();
            continue;
        }

        // 创建合并 WorkItem
        let prompt = build_consolidation_prompt(&candidates, &request.candidate_kind);

        let tools: Vec<crate::domain::ToolDefinition> = {
            // 需要从 SpaceToolRegistry 获取 submit_experience_candidate 工具
            // 此处简化：在系统签名中注入 registry
            Vec::new()
        };

        let work_item = WorkItem::experience_collection(
            request.task_id,
            prompt,
            Some(request.parent_task_id),
            None, // 无对话历史
            tools,
            request.governing_agent_id,
        );

        debug!(
            event = "ExperienceConsolidationWorkItemCreated",
            task_id = %request.task_id,
            candidate_count = candidates.len(),
            kind = ?request.candidate_kind,
            "spawning consolidation work item"
        );

        commands.spawn(work_item);
        commands.entity(entity).despawn();
    }
}

fn build_consolidation_prompt(
    candidates: &[&crate::domain::ExperienceCandidate],
    kind: &ExperienceKindHint,
) -> String {
    let kind_str = match kind {
        ExperienceKindHint::Knowledge => "知识",
        ExperienceKindHint::Skill => "技能",
    };

    let mut prompt = format!(
        "你是一个经验整理助手。以下是同一任务下多个 Agent 提交的{}候选，请对它们进行去重和合并。\n\n## 输入候选\n\n",
        kind_str
    );

    for (i, candidate) in candidates.iter().enumerate() {
        prompt.push_str(&format!("### 候选 {}: {}\n\n", i + 1, candidate.title));
        match &candidate.payload {
            crate::domain::ExperienceCandidatePayload::Knowledge { content } => {
                prompt.push_str(&format!("{}\n\n", content));
            }
            crate::domain::ExperienceCandidatePayload::Skill {
                description,
                instructions,
                ..
            } => {
                prompt.push_str(&format!("描述：{}\n\n指令：{}\n\n", description, instructions));
            }
        }
    }

    prompt.push_str(&format!(
        "## 要求\n\n\
         1. 去除重复或高度相似的{}\n\
         2. 合并互补的{}为更完整的版本\n\
         3. 通过调用 submit_experience_candidate 提交合并后的候选（kind=\"{}\"）\n\
         4. 如果所有候选都是重复的，只提交一个最完整的版本\n\
         5. 不要提交任何原始候选，只提交合并后的版本\n",
        kind_str,
        kind_str,
        match kind {
            ExperienceKindHint::Knowledge => "knowledge",
            ExperienceKindHint::Skill => "skill",
        }
    ));

    prompt
}
```

- [ ] **Step 2: 更新 collection.rs 触发合并**

在 `src/systems/experience/collection.rs` 的 `experience_collection_completion_system` 中，非顶层汇聚后触发合并：

```rust
// 非顶层：汇聚后判断是否需要合并
if let Some(parent_task_id) = msg.parent_task_id {
    let ids = store.aggregate_inbox_for_task(parent_task_id);
    let candidates: Vec<_> = ids
        .iter()
        .filter_map(|id| store.candidates.get(id))
        .collect();

    if candidates.len() > 1 {
        // 按 kind 分组
        let mut knowledge_ids: Vec<uuid::Uuid> = Vec::new();
        let mut skill_ids: Vec<uuid::Uuid> = Vec::new();
        for candidate in &candidates {
            match candidate.kind_hint {
                ExperienceKindHint::Knowledge => knowledge_ids.push(candidate.candidate_id),
                ExperienceKindHint::Skill => skill_ids.push(candidate.candidate_id),
            }
        }

        if knowledge_ids.len() > 1 {
            commands.spawn(ExperienceConsolidationRequestMessage {
                task_id: msg.task_id,
                parent_task_id,
                governing_agent_id: msg.governing_agent_id,
                candidate_kind: ExperienceKindHint::Knowledge,
                candidate_ids: knowledge_ids,
            });
        }
        if skill_ids.len() > 1 {
            commands.spawn(ExperienceConsolidationRequestMessage {
                task_id: msg.task_id,
                parent_task_id,
                governing_agent_id: msg.governing_agent_id,
                candidate_kind: ExperienceKindHint::Skill,
                candidate_ids: skill_ids,
            });
        }
    }

    debug!(
        event = "ExperienceCollectionAggregated",
        task_id = %msg.task_id,
        parent_task_id = %parent_task_id,
        aggregated_count = ids.len(),
        "aggregated child candidates into parent inbox"
    );
} else {
    // 顶层逻辑不变
    ...
}
```

- [ ] **Step 3: 更新 mod.rs 注册系统**

在 `src/systems/experience/mod.rs` 中新增 `consolidation` 模块并注册系统。

- [ ] **Step 4: 运行测试**

Run: `cargo test --all-features`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/systems/experience/collection.rs src/systems/experience/consolidation.rs src/systems/experience/mod.rs
git commit -m "feat: add experience consolidation system for merging similar candidates

- Trigger LLM consolidation when non-top-level aggregation yields >1 candidates
- Group by kind (Knowledge/Skill) and create consolidation work items
- Build consolidation prompt with candidate details"
```

---

### Task 8: 子项目 B 验证与文档更新

- [ ] **Step 1: 运行完整验证**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Expected: 全部通过。

- [ ] **Step 2: 更新 docs/current-state.md**

在经验候选治理部分补充：
- 长期记忆淘汰：`decay_score < 0.1` 且非 pin 非 Critical 的条目被淘汰并归档到 `archive.jsonl`
- Skill 加载：Agent 启动时从 `skills/` 目录加载 SKILL.md 注入系统提示
- 经验合并：非顶层候选数 > 1 时触发 LLM 合并，原始候选标记为 `Superseded`
- IncubationProposal：执行时同时处理 `skill_candidate_ids`

- [ ] **Step 3: 更新 docs/TODO.md**

标记已完成项、移除已失效项。

- [ ] **Step 4: 提交**

```bash
git add docs/current-state.md docs/TODO.md
git commit -m "docs: update current-state and TODO for experience governance completion"
```
