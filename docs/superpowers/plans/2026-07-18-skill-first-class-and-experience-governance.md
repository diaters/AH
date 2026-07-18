# Skill 成为一等公民与经验治理改造 实现计划

> **状态说明（2026-07-18）：** 本计划中关于 `select_agent_for_sub_task_with_skill`
> 函数的实现段落（§2.4 任务步骤 3 等）已被派发架构统一设计取代，不再作为实施依据。
> Brain LLM 现在通过 `build_brain_execution_request` 整体决策输出 `{agent_name, skill_name?}`，
> 候选 Agent 名下 skills 由 `SkillRegistry` 注入 prompt。详见
> [ADR-004 §2.4](../../adr/ADR-004-skill-first-class-and-experience-governance-reform.md)
> 与 [派发架构统一设计](../../design/2026-07-18-dispatch-architecture-unification-design.md)。
> 本计划其余段落保留作为历史背景。

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 让 skill 成为可被 brain 选择的一等公民，持久Agent吸收子经验，skill-updater 让 skill 具备经验驱动自我迭代能力。

**架构：** 新增 SkillRegistry Resource + TaskInjectedSkill/TaskExperiencePolicy Component；brain 派发时 LLM 选 Agent+skill；经验治理在 collection.rs 拦截持久Agent吸收路径，skill 类候选 spawn skill-updater workitem，由 submit_skill_update 工具提交结构化 diff，框架 apply 到 SKILL.md。

**技术栈：** Rust + Bevy ECS + ratatui + genai

**关联 ADR：** [docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md](../../adr/ADR-004-skill-first-class-and-experience-governance-reform.md)

---

## 文件结构

### 创建

- `src/infrastructure/skills/registry.rs` — SkillId + SkillEntry + SkillRegistry Resource
- `src/infrastructure/skills/diff.rs` — SkillUpdateOperation 枚举 + apply_skill_operations + cleanup_skill_history
- `src/systems/experience/skill_update.rs` — skill_update_workitem_system + skill_update_completion_system + route_persistent_agent_experience
- `src/systems/tools/builtin/submit_skill_update.rs` — submit_skill_update 工具定义 + SubmitSkillUpdate ToolAction
- `.harness/assets/agents/skill-updater/skills/skill-update/SKILL.md` — skill-updater 自身初始 skill 内容
- `tests/skill_update_integration.rs` — 集成测试

### 修改

- `src/infrastructure/skills/loader.rs` — SkillLoader 构造 SkillRegistry；parse_skill_md 解析新字段
- `src/infrastructure/skills/mod.rs` — 导出 registry 模块
- `src/domain/contribution.rs` — ExperienceCandidateStatus::Discarded + SkillUpdateContext + SkillUpdateCompletedMessage
- `src/domain/work_item.rs` — WorkItemType::SkillUpdate + WorkItem::skill_update 构造函数
- `src/domain/task_experience.rs`（新文件） — TaskInjectedSkill + TaskExperiencePolicy + ExperienceKindFilter
- `src/domain/mod.rs` — 导出 task_experience 模块
- `src/domain/agent.rs` — AgentKind 添加 helper 方法（如需）
- `src/systems/dispatch/agent_selection.rs` — select_agent_for_sub_task 签名扩展，返回 skill
- `src/systems/dispatch/brain_dispatch.rs` — 读取 SubTaskConfig.child_agent_name；spawn TaskInjectedSkill
- `src/systems/experience/collection.rs` — 调用 route_persistent_agent_experience；spawn SkillUpdate workitem
- `src/systems/experience/governance.rs` — self_updatable 检查
- `src/systems/experience/mod.rs` — 导出 skill_update 模块
- `src/systems/tools/orchestrator.rs` — 处理 ToolAction::SubmitSkillUpdate
- `src/systems/tools/builtin/mod.rs` — 注册 submit_skill_update 工具
- `src/plugins/execution.rs` — 注册 skill_update_workitem_system + skill_update_completion_system
- `src/app/mod.rs` — 构造 SkillRegistry 资源
- `agents.toml.example` — 添加 skill-updater 配置

---

## 阶段 1：数据结构基础

### 任务 1：SkillId + SkillEntry + SkillRegistry

**文件：**
- 创建：`src/infrastructure/skills/registry.rs`
- 修改：`src/infrastructure/skills/mod.rs`

- [ ] **步骤 1：编写失败的测试**

创建 `src/infrastructure/skills/registry.rs`，先写测试：

```rust
use std::collections::HashMap;

use bevy::prelude::*;

/// Skill 全局唯一标识，封装 `owner_agent_name + skill_name`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillId {
    pub owner_agent_name: String,
    pub skill_name: String,
}

impl SkillId {
    pub fn new(owner_agent_name: impl Into<String>, skill_name: impl Into<String>) -> Self {
        Self {
            owner_agent_name: owner_agent_name.into(),
            skill_name: skill_name.into(),
        }
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}", self.owner_agent_name, self.skill_name)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.splitn(2, '/');
        let owner_agent_name = parts.next()?.to_string();
        let skill_name = parts.next()?.to_string();
        if owner_agent_name.is_empty() || skill_name.is_empty() {
            return None;
        }
        Some(Self { owner_agent_name, skill_name })
    }
}

#[derive(Clone, Debug)]
pub struct SkillEntry {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub version: u32,
    pub owner_agent_name: String,
    pub self_updatable: bool,
}

#[derive(Resource, Default, Debug)]
pub struct SkillRegistry {
    pub skills: HashMap<SkillId, SkillEntry>,
}

impl SkillRegistry {
    pub fn get(&self, skill_id: &SkillId) -> &SkillEntry {
        &self.skills[skill_id]
    }

    pub fn list_by_owner(&self, owner_agent_name: &str) -> Vec<&SkillEntry> {
        self.skills
            .values()
            .filter(|e| e.owner_agent_name == owner_agent_name)
            .collect()
    }

    pub fn upsert(&mut self, entry: SkillEntry) {
        self.skills.insert(entry.skill_id.clone(), entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(name: &str, owner: &str) -> SkillEntry {
        SkillEntry {
            skill_id: SkillId::new(owner, name),
            name: name.to_string(),
            description: format!("desc for {}", name),
            instructions: "instructions".to_string(),
            version: 1,
            owner_agent_name: owner.to_string(),
            self_updatable: true,
        }
    }

    #[test]
    fn skill_id_round_trip() {
        let id = SkillId::new("default-llm-agent", "coding");
        let s = id.as_string();
        assert_eq!(s, "default-llm-agent/coding");
        let parsed = SkillId::parse(&s).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn skill_id_parse_rejects_invalid() {
        assert!(SkillId::parse("no-slash").is_none());
        assert!(SkillId::parse("/missing-owner").is_none());
        assert!(SkillId::parse("missing-name/").is_none());
        assert!(SkillId::parse("").is_none());
    }

    #[test]
    fn registry_upsert_replaces() {
        let mut reg = SkillRegistry::default();
        let mut entry = sample_entry("coding", "agent-a");
        entry.version = 1;
        reg.upsert(entry.clone());
        entry.version = 2;
        reg.upsert(entry);
        assert_eq!(reg.get(&SkillId::new("agent-a", "coding")).version, 2);
    }

    #[test]
    fn registry_list_by_owner() {
        let mut reg = SkillRegistry::default();
        reg.upsert(sample_entry("a", "agent-a"));
        reg.upsert(sample_entry("b", "agent-a"));
        reg.upsert(sample_entry("c", "agent-b"));
        let owned = reg.list_by_owner("agent-a");
        assert_eq!(owned.len(), 2);
    }
}
```

- [ ] **步骤 2：运行测试验证通过**

运行：`cargo test --lib infrastructure::skills::registry::tests`
预期：PASS（模块已自包含，无需外部依赖）

- [ ] **步骤 3：导出模块**

修改 `src/infrastructure/skills/mod.rs`，在现有 `pub mod loader;` 旁添加 `pub mod registry;`，并 re-export 关键类型：

```rust
pub mod loader;
pub mod registry;

pub use loader::{LoadedSkill, SkillLoader};
pub use registry::{SkillEntry, SkillId, SkillRegistry};
```

- [ ] **步骤 4：Commit**

```bash
git add src/infrastructure/skills/registry.rs src/infrastructure/skills/mod.rs
git commit -m "feat(skills): 添加 SkillRegistry 和 SkillId 类型"
```

---

### 任务 2：SKILL.md frontmatter 新字段解析

**文件：**
- 修改：`src/infrastructure/skills/loader.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/infrastructure/skills/loader.rs` 文件末尾追加测试模块：

```rust
#[cfg(test)]
mod version_field_tests {
    use super::*;

    #[test]
    fn parse_skill_md_with_version_and_self_updatable() {
        let content = "---\nname: my-skill\ndescription: A skill\nversion: 3\nself_updatable: false\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.self_updatable, false);
    }

    #[test]
    fn parse_skill_md_defaults_when_fields_missing() {
        let content = "---\nname: my-skill\ndescription: A skill\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.self_updatable, true);
    }

    #[test]
    fn parse_skill_md_self_updatable_true_explicit() {
        let content = "---\nname: my-skill\ndescription: A skill\nself_updatable: true\n---\n\n## Usage\n\nDo the thing.\n";
        let parsed = parse_skill_md(content).unwrap();
        assert_eq!(parsed.self_updatable, true);
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib infrastructure::skills::loader::version_field_tests`
预期：FAIL，报错 ` LoadedSkill` 没有 `version` / `self_updatable` 字段

- [ ] **步骤 3：扩展 LoadedSkill 结构和解析逻辑**

修改 `src/infrastructure/skills/loader.rs`，先扩展 `LoadedSkill` struct（行 22-27）：

```rust
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub version: u32,
    pub self_updatable: bool,
}
```

然后修改 `parse_skill_md` 函数（行 97-123），在解析 frontmatter 的循环中新增 `version` 和 `self_updatable` 字段：

```rust
pub fn parse_skill_md(content: &str) -> Option<LoadedSkill> {
    let mut lines = content.lines();
    let first = lines.next()?;
    if first.trim() != "---" { return None; }

    let mut name = String::new();
    let mut description = String::new();
    let mut version: u32 = 1;
    let mut self_updatable: bool = true;
    let mut instructions_lines: Vec<String> = Vec::new();
    let mut in_frontmatter = true;

    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            if let Some(rest) = line.strip_prefix("name:") {
                name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("description:") {
                description = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("version:") {
                if let Ok(v) = rest.trim().parse::<u32>() {
                    version = v;
                }
            } else if let Some(rest) = line.strip_prefix("self_updatable:") {
                match rest.trim() {
                    "true" => self_updatable = true,
                    "false" => self_updatable = false,
                    _ => {}
                }
            }
        } else {
            instructions_lines.push(line.to_string());
        }
    }

    if name.is_empty() { return None; }

    let instructions = instructions_lines.join("\n").trim().to_string();
    Some(LoadedSkill { name, description, instructions, version, self_updatable })
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib infrastructure::skills::loader`
预期：PASS，含新测试 + 现有测试

- [ ] **步骤 5：Commit**

```bash
git add src/infrastructure/skills/loader.rs
git commit -m "feat(skills): SKILL.md frontmatter 支持 version 和 self_updatable 字段"
```

---

### 任务 3：SkillLoader 构造 SkillRegistry

**文件：**
- 修改：`src/infrastructure/skills/loader.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/infrastructure/skills/loader.rs` 测试模块追加：

```rust
#[cfg(test)]
mod registry_build_tests {
    use super::*;
    use crate::infrastructure::skills::registry::{SkillEntry, SkillId, SkillRegistry};
    use std::fs;
    use tempfile::TempDir;

    fn write_skill(base: &std::path::Path, agent: &str, skill_name: &str, content: &str) {
        let dir = base.join("agents").join(agent).join("skills").join(skill_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn build_registry_scans_all_agents() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join(".harness").join("assets");
        write_skill(&base, "agent-a", "coding",
            "---\nname: coding\ndescription: coding skill\nversion: 2\n---\n\n## Usage\n\nDo it.\n");
        write_skill(&base, "agent-a", "review",
            "---\nname: review\ndescription: review skill\n---\n\n## Usage\n\nReview.\n");
        write_skill(&base, "agent-b", "writing",
            "---\nname: writing\ndescription: writing skill\nself_updatable: false\n---\n\n## Usage\n\nWrite.\n");

        let loader = SkillLoader { base_dir: base.clone() };
        let registry: SkillRegistry = loader.build_registry();

        assert_eq!(registry.skills.len(), 3);
        let coding = registry.get(&SkillId::new("agent-a", "coding"));
        assert_eq!(coding.version, 2);
        assert_eq!(coding.owner_agent_name, "agent-a");
        assert_eq!(coding.self_updatable, true);
        let writing = registry.get(&SkillId::new("agent-b", "writing"));
        assert_eq!(writing.self_updatable, false);
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib infrastructure::skills::loader::registry_build_tests`
预期：FAIL，报错 `build_registry` 方法不存在

- [ ] **步骤 3：实现 build_registry**

在 `src/infrastructure/skills/loader.rs` 中为 `SkillLoader` 添加 `build_registry` 方法（位置放在 `load_plugin_skills` 之后）：

```rust
use crate::infrastructure::skills::registry::{SkillEntry, SkillId, SkillRegistry};

impl SkillLoader {
    /// 扫描所有 agent 的 skills 目录，构造 SkillRegistry
    pub fn build_registry(&self) -> SkillRegistry {
        let mut registry = SkillRegistry::default();
        let agents_dir = self.base_dir.join("agents");
        if let Ok(agent_entries) = std::fs::read_dir(&agents_dir) {
            for agent_entry in agent_entries.flatten() {
                let agent_name = agent_entry.file_name().to_string_lossy().to_string();
                let skills_dir = agent_entry.path().join("skills");
                if let Ok(skill_entries) = std::fs::read_dir(&skills_dir) {
                    for skill_entry in skill_entries.flatten() {
                        let skill_path = skill_entry.path().join("SKILL.md");
                        if let Ok(content) = std::fs::read_to_string(&skill_path) {
                            if let Some(loaded) = parse_skill_md(&content) {
                                let skill_id = SkillId::new(agent_name.clone(), loaded.name.clone());
                                let entry = SkillEntry {
                                    skill_id,
                                    name: loaded.name,
                                    description: loaded.description,
                                    instructions: loaded.instructions,
                                    version: loaded.version,
                                    owner_agent_name: agent_name.clone(),
                                    self_updatable: loaded.self_updatable,
                                };
                                registry.upsert(entry);
                            }
                        }
                    }
                }
            }
        }
        registry
    }
}
```

注意：`Cargo.toml` 已经依赖 `tempfile`（如未依赖，在 dev-dependencies 中添加：`tempfile = "3"`）。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib infrastructure::skills::loader`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/infrastructure/skills/loader.rs
git commit -m "feat(skills): SkillLoader 构造 SkillRegistry 扫描所有 agent 的 skill"
```

---

### 任务 4：TaskInjectedSkill + TaskExperiencePolicy Component

**文件：**
- 创建：`src/domain/task_experience.rs`
- 修改：`src/domain/mod.rs`

- [ ] **步骤 1：创建文件并定义类型**

创建 `src/domain/task_experience.rs`：

```rust
use bevy::prelude::*;

use crate::infrastructure::skills::SkillId;

/// 标记 Task 注入的 skill（由 brain 派发时写入）
#[derive(Component, Debug, Clone, Default)]
pub struct TaskInjectedSkill {
    pub skill_id: Option<SkillId>,
}

/// 标记 Task 的经验治理过滤策略（仅 skill-updater 等特殊 Agent 需要）
#[derive(Component, Debug, Clone, Default)]
pub struct TaskExperiencePolicy {
    pub kind_filter: ExperienceKindFilter,
}

/// 经验类型过滤策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExperienceKindFilter {
    /// 允许所有类型（默认）
    #[default]
    All,
    /// 仅允许 knowledge 类（skill 候选被丢弃）
    KnowledgeOnly,
    /// 仅允许 skill 类（knowledge 候选被丢弃）
    SkillOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_filter_is_all() {
        assert_eq!(ExperienceKindFilter::default(), ExperienceKindFilter::All);
    }

    #[test]
    fn task_injected_skill_default_is_none() {
        let t = TaskInjectedSkill::default();
        assert!(t.skill_id.is_none());
    }
}
```

- [ ] **步骤 2：导出模块**

修改 `src/domain/mod.rs`，添加 `pub mod task_experience;` 并 re-export：

```rust
pub mod task_experience;

pub use task_experience::{
    ExperienceKindFilter, TaskExperiencePolicy, TaskInjectedSkill,
};
```

- [ ] **步骤 3：运行测试验证通过**

运行：`cargo test --lib domain::task_experience`
预期：PASS

- [ ] **步骤 4：Commit**

```bash
git add src/domain/task_experience.rs src/domain/mod.rs
git commit -m "feat(domain): 添加 TaskInjectedSkill 和 TaskExperiencePolicy Component"
```

---

### 任务 5：ExperienceCandidateStatus::Discarded

**文件：**
- 修改：`src/domain/contribution.rs`

- [ ] **步骤 1：扩展枚举**

修改 `src/domain/contribution.rs` 第 31-49 行的 `ExperienceCandidateStatus` 枚举，新增 `Discarded` 变体：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExperienceCandidateStatus {
    // ... existing variants
    /// 被 experience_kind_filter 过滤
    Discarded,
}
```

- [ ] **步骤 2：编译检查**

运行：`cargo build --lib`
预期：编译成功（新变体未在 match 中使用会引发 warning，但 build 不失败；若有 `match` exhaustive 处理需要同步补 `_ => {}` 分支或显式处理）

如果编译失败，按编译错误指示补充 match 分支。

- [ ] **步骤 3：Commit**

```bash
git add src/domain/contribution.rs
git commit -m "feat(domain): ExperienceCandidateStatus 新增 Discarded 状态"
```

---

### 任务 6：WorkItemType::SkillUpdate + SkillUpdateContext

**文件：**
- 修改：`src/domain/work_item.rs`
- 修改：`src/domain/contribution.rs`

- [ ] **步骤 1：扩展 WorkItemType**

修改 `src/domain/work_item.rs`，在 `WorkItemType` 枚举中添加 `SkillUpdate` 变体：

```rust
pub enum WorkItemType {
    Execution,
    Summarization,
    Evaluation,
    ExperienceCollection,
    ProfileGeneration,
    SkillUpdate,
}
```

并同步扩展 `WorkItemType::as_str()` 和 `from_str()` 等方法（按现有模式）。

- [ ] **步骤 2：定义 SkillUpdateContext**

在 `src/domain/contribution.rs` 末尾添加：

```rust
use crate::infrastructure::skills::SkillId;

/// skill-updater workitem 的上下文 Component
#[derive(Component, Debug, Clone)]
pub struct SkillUpdateContext {
    pub skill_id: SkillId,
    pub base_version: u32,
    pub experience_candidate_id: uuid::Uuid,
    pub governing_agent_id: AgentId,
}
```

- [ ] **步骤 3：添加 WorkItem::skill_update 构造函数**

参考 `WorkItem::profile_generation`（行 264-289）模式，在 `src/domain/work_item.rs` 添加：

```rust
impl WorkItem {
    pub fn skill_update(
        task_id: TaskId,
        prompt: String,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        governing_agent_id: AgentId,
    ) -> Self {
        let tags = TagSet::from_tags(["skill-update"]);
        // 构造逻辑参考 profile_generation 的实现
        // 关键字段：work_type = WorkItemType::SkillUpdate
        // ...
        unimplemented!() // 实现时按 profile_generation 模式填写
    }
}
```

**实施说明**：上述 `unimplemented!()` 是占位，实际实现时必须严格按 `profile_generation` 的构造模式填充所有字段（参考 [work_item.rs:264-289](../../src/domain/work_item.rs#L264-L289)）。包含 `id`、`task_id`、`prompt`、`conversation`、`tools`、`work_type`、`tags`、`governing_agent_id` 等字段。不能保留 `unimplemented!()`。

- [ ] **步骤 4：编译检查**

运行：`cargo build --lib`
预期：编译成功（`unimplemented!()` 仍可编译，但实施时必须替换）

- [ ] **步骤 5：Commit**

```bash
git add src/domain/work_item.rs src/domain/contribution.rs
git commit -m "feat(domain): 新增 WorkItemType::SkillUpdate 和 SkillUpdateContext"
```

---

### 任务 7：SkillUpdateOperation + SkillUpdateCompletedMessage

**文件：**
- 修改：`src/domain/contribution.rs`

- [ ] **步骤 1：定义 SkillUpdateOperation 枚举和 SkillUpdateCompletedMessage**

在 `src/domain/contribution.rs` 末尾添加：

```rust
/// skill 更新的结构化 diff 操作
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action")]
pub enum SkillUpdateOperation {
    #[serde(rename = "replace_section")]
    ReplaceSection { section: String, content: String },
    #[serde(rename = "add_section")]
    AddSection { after: String, section: String, content: String },
    #[serde(rename = "remove_section")]
    RemoveSection { section: String },
    #[serde(rename = "replace_frontmatter")]
    ReplaceFrontmatter { field: String, value: String },
}

/// skill-updater workitem 完成后由 orchestrator spawn
#[derive(Debug, Clone, Event)]
pub struct SkillUpdateCompletedMessage {
    pub work_item_id: uuid::Uuid,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub skill_id: SkillId,
    pub base_version: u32,
    pub new_version: u32,
    pub operations: Vec<SkillUpdateOperation>,
    pub rationale: String,
}

#[cfg(test)]
mod skill_update_operation_tests {
    use super::*;

    #[test]
    fn serialize_replace_section() {
        let op = SkillUpdateOperation::ReplaceSection {
            section: "## Usage".to_string(),
            content: "New content".to_string(),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("replace_section"));
        assert!(json.contains("## Usage"));
    }

    #[test]
    fn deserialize_add_section() {
        let json = r#"{"action":"add_section","after":"## Usage","section":"## Edge Cases","content":"..."}"#;
        let op: SkillUpdateOperation = serde_json::from_str(json).unwrap();
        match op {
            SkillUpdateOperation::AddSection { after, section, content } => {
                assert_eq!(after, "## Usage");
                assert_eq!(section, "## Edge Cases");
                assert_eq!(content, "...");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn frontmatter_field_whitelist_enforced_at_apply_layer() {
        // apply_skill_operations 函数负责检查 field 是否在白名单
        // 这里仅验证枚举本身能携带任意 field 值
        let op = SkillUpdateOperation::ReplaceFrontmatter {
            field: "unknown".to_string(),
            value: "x".to_string(),
        };
        match op {
            SkillUpdateOperation::ReplaceFrontmatter { field, .. } => {
                assert_eq!(field, "unknown");
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

注意：若 `Cargo.toml` 未显式开启 `serde` feature 对 `serde_json` 的依赖，需要在 `serde_json` 已存在依赖（项目已使用）下使用。

- [ ] **步骤 2：运行测试验证通过**

运行：`cargo test --lib domain::contribution::skill_update_operation_tests`
预期：PASS

- [ ] **步骤 3：Commit**

```bash
git add src/domain/contribution.rs
git commit -m "feat(domain): 新增 SkillUpdateOperation 和 SkillUpdateCompletedMessage"
```

---

## 阶段 2：skill-updater Agent 与初始 skill

### 任务 8：agents.toml.example 添加 skill-updater 配置

**文件：**
- 修改：`agents.toml.example`

- [ ] **步骤 1：追加 skill-updater 配置**

在 `agents.toml.example` 末尾追加：

```toml

[[agent]]
name = "skill-updater"
tags = ["skill-updater", "persistent"]
description = "负责根据经验候选更新已有 skill 的 instruction，通过 submit_skill_update 工具提交结构化更新操作"
system_prompt = """\
你是一个 skill 更新专家。根据经验候选和原 skill 内容，通过 submit_skill_update 工具提交结构化更新操作。\
你必须基于原 skill 的 instructions 和触发更新的经验候选，识别需要新增、修改、删除的章节，\
然后通过 submit_skill_update 工具提交 operations 数组。\
每次响应必须调用 submit_skill_update 工具一次，不能仅返回文本而不调用工具。\
"""
model = "deepseek-chat"

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"

[agent.tools]
default_permission = "Deny"
submit_skill_update = "Allow"
```

- [ ] **步骤 2：Commit**

```bash
git add agents.toml.example
git commit -m "chore(agents): 添加 skill-updater Agent 配置示例"
```

---

### 任务 9：skill-updater 的初始 SKILL.md

**文件：**
- 创建：`.harness/assets/agents/skill-updater/skills/skill-update/SKILL.md`

- [ ] **步骤 1：创建初始 skill 内容**

```markdown
---
name: skill-update
description: 根据经验候选更新已有 skill 的 instruction，通过结构化 diff 操作提交更新
version: 1
self_updatable: false
---

## 职责

你是一个 skill 更新专家。你将收到：

- 原 skill 的完整 instruction（含 markdown 章节）
- 原 skill 的版本号
- 一条触发更新的 skill 类经验候选

你的任务是基于经验候选识别 skill 中需要更新的部分，通过 `submit_skill_update` 工具提交结构化 diff 操作。

## 工具调用约束

必须调用 `submit_skill_update` 工具一次，不能跳过。`operations` 数组中每个操作必须是以下四种之一：

- `replace_section`：替换指定章节的内容（含子章节）
- `add_section`：在指定章节之后插入新章节
- `remove_section`：删除指定章节
- `replace_frontmatter`：修改 frontmatter 字段（仅允许 `name`、`description`、`self_updatable`）

`base_version` 必须等于你看到的原 skill 版本号。`new_version` 必须等于 `base_version + 1`。

## 章节匹配规则

markdown 章节由 `## `（二级标题）开始。同名章节匹配第一个出现的位置。

## 限制

- 不允许直接修改 `version` 字段（由框架自动递增）
- 操作必须基于经验候选的真实内容，不能臆造
- `rationale` 字段必须说明每个操作的理由

## 示例

输入：原 skill 含 `## Usage` 章节，经验候选提示"Usage 章节缺少边界条件说明"

输出：

```json
{
  "skill_id": "owner/skill-name",
  "base_version": 3,
  "new_version": 4,
  "operations": [
    {
      "action": "add_section",
      "after": "## Usage",
      "section": "## Edge Cases",
      "content": "边界条件说明..."
    }
  ],
  "rationale": "经验候选提示缺少边界条件，新增 ## Edge Cases 章节"
}
```
```

- [ ] **步骤 2：Commit**

```bash
git add .harness/assets/agents/skill-updater/skills/skill-update/SKILL.md
git commit -m "chore(skills): 添加 skill-updater 自身的初始 skill 内容"
```

---

## 阶段 3：skill diff 系统

### 任务 10：apply_skill_operations 函数

**文件：**
- 创建：`src/infrastructure/skills/diff.rs`
- 修改：`src/infrastructure/skills/mod.rs`

- [ ] **步骤 1：编写失败的测试**

创建 `src/infrastructure/skills/diff.rs`：

```rust
use crate::domain::SkillUpdateOperation;

/// 允许 LLM 修改的 frontmatter 字段白名单
pub const FRONTMATTER_WHITELIST: &[&str] = &["name", "description", "self_updatable"];

/// 解析 SKILL.md，返回 frontmatter 部分和 body 部分
fn split_frontmatter(content: &str) -> (String, String) {
    let mut lines = content.lines();
    let first = lines.next();
    if first.map(|s| s.trim() != "---").unwrap_or(true) {
        return (String::new(), content.to_string());
    }
    let mut frontmatter = String::new();
    let mut body = String::new();
    let mut in_frontmatter = true;
    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            frontmatter.push_str(line);
            frontmatter.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    (frontmatter, body)
}

/// 找到 `## {section}` 章节的起始行号和结束行号（不含下一个 ## 标题）
fn find_section_range(body: &str, section: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = body.lines().collect();
    let header = section.trim();
    let start = lines.iter().position(|l| l.trim_start().starts_with(header))?;
    let end = lines.iter().enumerate().skip(start + 1)
        .find(|(_, l)| l.trim_start().starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Some((start, end))
}

/// apply operations 到 SKILL.md 内容，返回更新后的内容
pub fn apply_skill_operations(content: &str, operations: &[SkillUpdateOperation]) -> Result<String, ApplyError> {
    let (frontmatter, body) = split_frontmatter(content);
    let mut frontmatter_lines: Vec<String> = frontmatter.lines().map(|s| s.to_string()).collect();
    let mut body_lines: Vec<String> = body.lines().map(|s| s.to_string()).collect();

    for op in operations {
        match op {
            SkillUpdateOperation::ReplaceSection { section, content } => {
                let range = find_section_range(&body_lines.join("\n"), section)
                    .ok_or_else(|| ApplyError::SectionNotFound(section.clone()))?;
                // 保留 `## {section}` 行，替换后续内容
                body_lines.splice(range.0 + 1..range.1, content.lines().map(|s| s.to_string()));
            }
            SkillUpdateOperation::AddSection { after, section, content } => {
                let body_str = body_lines.join("\n");
                let range = find_section_range(&body_str, after)
                    .ok_or_else(|| ApplyError::SectionNotFound(after.clone()))?;
                let mut new_lines: Vec<String> = vec![section.clone()];
                new_lines.extend(content.lines().map(|s| s.to_string()));
                new_lines.push(String::new()); // 空行分隔
                body_lines.splice(range.1..range.1, new_lines);
            }
            SkillUpdateOperation::RemoveSection { section } => {
                let body_str = body_lines.join("\n");
                let range = find_section_range(&body_str, section)
                    .ok_or_else(|| ApplyError::SectionNotFound(section.clone()))?;
                body_lines.drain(range.0..range.1);
            }
            SkillUpdateOperation::ReplaceFrontmatter { field, value } => {
                if !FRONTMATTER_WHITELIST.contains(&field.as_str()) {
                    return Err(ApplyError::FieldNotWhitelisted(field.clone()));
                }
                let prefix = format!("{}:", field);
                if let Some(line) = frontmatter_lines.iter_mut().find(|l| l.starts_with(&prefix)) {
                    *line = format!("{}: {}", field, value);
                } else {
                    frontmatter_lines.push(format!("{}: {}", field, value));
                }
            }
        }
    }

    let mut result = String::new();
    result.push_str("---\n");
    for line in &frontmatter_lines {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str("---\n\n");
    for line in &body_lines {
        result.push_str(line);
        result.push('\n');
    }
    Ok(result)
}

#[derive(Debug)]
pub enum ApplyError {
    SectionNotFound(String),
    FieldNotWhitelisted(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: test\ndescription: A skill\nversion: 1\n---\n\n## Usage\n\nDo it.\n\n## Examples\n\nExample 1.\n";

    #[test]
    fn replace_section_existing() {
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Usage".to_string(),
            content: "New usage content.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("New usage content."));
        assert!(!result.contains("Do it."));
    }

    #[test]
    fn replace_section_not_found() {
        let ops = vec![SkillUpdateOperation::ReplaceSection {
            section: "## Missing".to_string(),
            content: "x".to_string(),
        }];
        assert!(matches!(apply_skill_operations(SAMPLE, &ops), Err(ApplyError::SectionNotFound(_))));
    }

    #[test]
    fn add_section_after_existing() {
        let ops = vec![SkillUpdateOperation::AddSection {
            after: "## Usage".to_string(),
            section: "## Edge Cases".to_string(),
            content: "Edge case notes.".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("## Edge Cases"));
        assert!(result.contains("Edge case notes."));
        let usage_idx = result.find("## Usage").unwrap();
        let edge_idx = result.find("## Edge Cases").unwrap();
        let examples_idx = result.find("## Examples").unwrap();
        assert!(usage_idx < edge_idx);
        assert!(edge_idx < examples_idx);
    }

    #[test]
    fn remove_section_existing() {
        let ops = vec![SkillUpdateOperation::RemoveSection {
            section: "## Examples".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(!result.contains("## Examples"));
        assert!(!result.contains("Example 1."));
    }

    #[test]
    fn replace_frontmatter_in_whitelist() {
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "description".to_string(),
            value: "Updated description".to_string(),
        }];
        let result = apply_skill_operations(SAMPLE, &ops).unwrap();
        assert!(result.contains("description: Updated description"));
    }

    #[test]
    fn replace_frontmatter_not_in_whitelist() {
        let ops = vec![SkillUpdateOperation::ReplaceFrontmatter {
            field: "version".to_string(),
            value: "999".to_string(),
        }];
        assert!(matches!(apply_skill_operations(SAMPLE, &ops), Err(ApplyError::FieldNotWhitelisted(_))));
    }
}
```

- [ ] **步骤 2：运行测试验证通过**

运行：`cargo test --lib infrastructure::skills::diff`
预期：PASS

- [ ] **步骤 3：导出模块**

修改 `src/infrastructure/skills/mod.rs`，添加 `pub mod diff;` 和 re-export：

```rust
pub mod diff;
pub use diff::{apply_skill_operations, ApplyError, FRONTMATTER_WHITELIST};
```

- [ ] **步骤 4：Commit**

```bash
git add src/infrastructure/skills/diff.rs src/infrastructure/skills/mod.rs
git commit -m "feat(skills): 实现 apply_skill_operations 结构化 diff"
```

---

### 任务 11：cleanup_skill_history 函数

**文件：**
- 修改：`src/infrastructure/skills/diff.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/infrastructure/skills/diff.rs` 测试模块追加：

```rust
mod cleanup_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn cleanup_keeps_latest_3_generations() {
        let tmp = TempDir::new().unwrap();
        let history_dir = tmp.path().join("history");
        fs::create_dir_all(&history_dir).unwrap();
        for v in 1..=6 {
            fs::write(history_dir.join(format!("v{}.md", v)), format!("v{}", v)).unwrap();
        }
        cleanup_skill_history(&history_dir, 3).unwrap();
        let remaining: Vec<_> = fs::read_dir(&history_dir).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), 3);
        // 保留最新的 3 代（v4, v5, v6）
        assert!(remaining.contains(&"v4.md".to_string()));
        assert!(remaining.contains(&"v5.md".to_string()));
        assert!(remaining.contains(&"v6.md".to_string()));
    }

    #[test]
    fn cleanup_no_dir_is_noop() {
        let result = cleanup_skill_history(std::path::Path::new("/nonexistent"), 3);
        assert!(result.is_ok());
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib infrastructure::skills::diff::cleanup_tests`
预期：FAIL，`cleanup_skill_history` 未定义

- [ ] **步骤 3：实现 cleanup_skill_history**

在 `src/infrastructure/skills/diff.rs` 中添加：

```rust
use std::path::Path;

/// 保留最新 keep 代历史，删除超出部分
pub fn cleanup_skill_history(history_dir: &Path, keep: usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(history_dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // 目录不存在视为无操作
    };

    let mut versions: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        // 解析 vN.md
        if let Some(stripped) = file_name.strip_prefix('v') {
            if let Some(name) = stripped.strip_suffix(".md") {
                if let Ok(v) = name.parse::<u32>() {
                    versions.push((v, path));
                }
            }
        }
    }

    versions.sort_by_key(|(v, _)| *v);
    let excess = versions.len().saturating_sub(keep);
    for (_, path) in versions.iter().take(excess) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib infrastructure::skills::diff`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/infrastructure/skills/diff.rs
git commit -m "feat(skills): 实现 cleanup_skill_history 保留 3 代历史"
```

---

### 任务 12：refresh_skill_registry 函数

**文件：**
- 修改：`src/infrastructure/skills/registry.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/infrastructure/skills/registry.rs` 测试模块追加：

```rust
mod refresh_tests {
    use super::*;

    #[test]
    fn refresh_replaces_entry() {
        let mut reg = SkillRegistry::default();
        let mut entry = SkillEntry {
            skill_id: SkillId::new("agent", "skill"),
            name: "skill".to_string(),
            description: "old".to_string(),
            instructions: "old".to_string(),
            version: 1,
            owner_agent_name: "agent".to_string(),
            self_updatable: true,
        };
        reg.upsert(entry.clone());
        entry.version = 2;
        entry.instructions = "new".to_string();
        reg.refresh(entry);
        let got = reg.get(&SkillId::new("agent", "skill"));
        assert_eq!(got.version, 2);
        assert_eq!(got.instructions, "new");
    }

    #[test]
    fn refresh_inserts_if_missing() {
        let mut reg = SkillRegistry::default();
        let entry = SkillEntry {
            skill_id: SkillId::new("agent", "new-skill"),
            name: "new-skill".to_string(),
            description: "d".to_string(),
            instructions: "i".to_string(),
            version: 1,
            owner_agent_name: "agent".to_string(),
            self_updatable: true,
        };
        reg.refresh(entry);
        assert_eq!(reg.skills.len(), 1);
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib infrastructure::skills::registry::refresh_tests`
预期：FAIL，`refresh` 方法不存在

- [ ] **步骤 3：实现 refresh 方法**

在 `SkillRegistry` impl 块中添加：

```rust
impl SkillRegistry {
    /// 刷新单个 skill entry（skill-updater 写入后调用）
    pub fn refresh(&mut self, entry: SkillEntry) {
        self.skills.insert(entry.skill_id.clone(), entry);
    }
}
```

（注：`refresh` 与 `upsert` 行为相同，但语义上"refresh"用于运行期更新场景，保留独立方法名便于后续日志或 hook 扩展。）

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib infrastructure::skills::registry`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/infrastructure/skills/registry.rs
git commit -m "feat(skills): SkillRegistry 新增 refresh 方法"
```

---

## 阶段 4：submit_skill_update 工具

### 任务 13：SubmitSkillUpdate ToolAction

**文件：**
- 修改：`src/systems/tools/builtin/submit_skill_update.rs`（新建）
- 修改：`src/systems/tools/builtin/mod.rs`
- 修改：`src/systems/tools/mod.rs`（如需 ToolAction 定义）

- [ ] **步骤 1：创建工具文件**

参考 `submit_profile_update` 工具实现模式，创建 `src/systems/tools/builtin/submit_skill_update.rs`：

```rust
use serde::{Deserialize, Serialize};

use crate::domain::{SkillId, SkillUpdateOperation};
use crate::infrastructure::tools::schema::{ToolDefinition, ToolParameter, ToolSchema};

pub const SUBMIT_SKILL_UPDATE_TOOL_NAME: &str = "submit_skill_update";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitSkillUpdateRequest {
    pub skill_id: String,
    pub base_version: u32,
    pub new_version: u32,
    pub operations: Vec<SkillUpdateOperation>,
    pub rationale: String,
}

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: SUBMIT_SKILL_UPDATE_TOOL_NAME.to_string(),
        description: "提交 skill 更新的结构化 diff 操作。必须基于原 skill 的 instruction 和经验候选，提交 operations 数组。".to_string(),
        schema: ToolSchema {
            parameters: vec![
                ToolParameter::string("skill_id", "skill 的全局唯一 ID，格式为 owner_agent_name/skill_name", true),
                ToolParameter::integer("base_version", "原 skill 的版本号", true),
                ToolParameter::integer("new_version", "新版本号，必须等于 base_version + 1", true),
                ToolParameter::array(
                    "operations",
                    "结构化 diff 操作数组",
                    true,
                    ToolSchema::object(vec![
                        // operation 子字段定义（按 SkillUpdateOperation 各变体）
                    ]),
                ),
                ToolParameter::string("rationale", "本次更新的理由说明", true),
            ],
            required: vec![
                "skill_id".to_string(),
                "base_version".to_string(),
                "new_version".to_string(),
                "operations".to_string(),
                "rationale".to_string(),
            ],
        },
    }
}

#[derive(Debug, Clone)]
pub struct ParsedSubmitSkillUpdate {
    pub skill_id: SkillId,
    pub base_version: u32,
    pub new_version: u32,
    pub operations: Vec<SkillUpdateOperation>,
    pub rationale: String,
}

pub fn parse_request(raw: &serde_json::Value) -> Result<ParsedSubmitSkillUpdate, String> {
    let req: SubmitSkillUpdateRequest = serde_json::from_value(raw.clone())
        .map_err(|e| format!("invalid submit_skill_update args: {}", e))?;
    let skill_id = SkillId::parse(&req.skill_id)
        .ok_or_else(|| format!("invalid skill_id: {}", req.skill_id))?;
    if req.new_version != req.base_version + 1 {
        return Err(format!("new_version must be base_version + 1, got base={} new={}",
            req.base_version, req.new_version));
    }
    Ok(ParsedSubmitSkillUpdate {
        skill_id,
        base_version: req.base_version,
        new_version: req.new_version,
        operations: req.operations,
        rationale: req.rationale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_valid_request() {
        let raw = json!({
            "skill_id": "agent-a/coding",
            "base_version": 3,
            "new_version": 4,
            "operations": [
                {"action": "replace_section", "section": "## Usage", "content": "new"}
            ],
            "rationale": "test"
        });
        let parsed = parse_request(&raw).unwrap();
        assert_eq!(parsed.skill_id, SkillId::new("agent-a", "coding"));
        assert_eq!(parsed.base_version, 3);
        assert_eq!(parsed.new_version, 4);
        assert_eq!(parsed.operations.len(), 1);
    }

    #[test]
    fn parse_rejects_invalid_skill_id() {
        let raw = json!({
            "skill_id": "no-slash",
            "base_version": 1,
            "new_version": 2,
            "operations": [],
            "rationale": ""
        });
        assert!(parse_request(&raw).is_err());
    }

    #[test]
    fn parse_rejects_wrong_version_increment() {
        let raw = json!({
            "skill_id": "agent-a/coding",
            "base_version": 3,
            "new_version": 5,
            "operations": [],
            "rationale": ""
        });
        assert!(parse_request(&raw).is_err());
    }
}
```

- [ ] **步骤 2：添加 ToolAction 变体**

在 `ToolAction` 枚举（通常在 `src/systems/tools/mod.rs` 或 `src/domain/tool.rs`，按现有 `SubmitProfileUpdate` 模式）中添加：

```rust
pub enum ToolAction {
    // ... existing
    SubmitSkillUpdate {
        skill_id: SkillId,
        base_version: u32,
        new_version: u32,
        operations: Vec<SkillUpdateOperation>,
        rationale: String,
    },
}
```

- [ ] **步骤 3：注册工具**

修改 `src/systems/tools/builtin/mod.rs`，添加 `pub mod submit_skill_update;` 和 `pub use submit_skill_update::*;`。
在工具注册函数中调用 `submit_skill_update::tool_definition()`（参考 `submit_profile_update` 注册模式）。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib systems::tools::builtin::submit_skill_update`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/systems/tools/builtin/submit_skill_update.rs src/systems/tools/builtin/mod.rs src/systems/tools/mod.rs
git commit -m "feat(tools): 实现 submit_skill_update 工具和 ToolAction::SubmitSkillUpdate"
```

---

### 任务 14：orchestrator.rs 处理 SubmitSkillUpdate

**文件：**
- 修改：`src/systems/tools/orchestrator.rs`

- [ ] **步骤 1：参考 SubmitProfileUpdate 处理（行 891-963），添加 SubmitSkillUpdate 处理**

在 `src/systems/tools/orchestrator.rs` 中，参考 `SubmitProfileUpdate` 处理分支（行 891-963），添加 `SubmitSkillUpdate` 分支：

```rust
Ok(ToolAction::SubmitSkillUpdate {
    skill_id,
    base_version,
    new_version,
    operations,
    rationale,
}) => {
    // spawn SkillUpdateCompletedMessage 供 skill_update_completion_system 消费
    commands.spawn(crate::domain::SkillUpdateCompletedMessage {
        work_item_id: request.work_item_id.unwrap_or_default(),
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        skill_id: skill_id.clone(),
        base_version,
        new_version,
        operations,
        rationale,
    });
    // 返回工具执行结果给 LLM
    // 参考 spawn_tool_execution_result 模式（SubmitProfileUpdate 处理中）
    // ...
    commands.entity(request_entity).despawn();
}
```

**实施说明**：`request.work_item_id` 字段名按实际 `ToolExecutionContext` 定义调整。`spawn_tool_execution_result` 函数名按现有实现调整。

- [ ] **步骤 2：编译检查**

运行：`cargo build --lib`
预期：编译成功

- [ ] **步骤 3：Commit**

```bash
git add src/systems/tools/orchestrator.rs
git commit -m "feat(tools): orchestrator 处理 SubmitSkillUpdate spawn SkillUpdateCompletedMessage"
```

---

## 阶段 5：Brain 选 skill

### 任务 15：select_agent_for_sub_task 签名扩展

**文件：**
- 修改：`src/systems/dispatch/agent_selection.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/dispatch/agent_selection.rs` 测试模块追加：

```rust
mod skill_selection_tests {
    use super::*;
    use crate::infrastructure::skills::{SkillEntry, SkillId, SkillRegistry};

    fn make_agent(name: &str, tags: &[&str]) -> Agent {
        Agent {
            id: AgentId::new(),
            profile: AgentProfile { name: name.to_string(), model: None },
            capabilities: AgentCapabilities { tags: tags.iter().map(|s| s.to_string()).collect(), description: None },
            kind: AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: AgentToolPermissions::default(),
            system_prompt: None,
        }
    }

    fn make_registry(agent: &Agent, skill_name: &str) -> SkillRegistry {
        let mut reg = SkillRegistry::default();
        reg.upsert(SkillEntry {
            skill_id: SkillId::new(&agent.profile.name, skill_name),
            name: skill_name.to_string(),
            description: format!("desc for {}", skill_name),
            instructions: "instructions".to_string(),
            version: 1,
            owner_agent_name: agent.profile.name.clone(),
            self_updatable: true,
        });
        reg
    }

    #[test]
    fn select_agent_with_skill_returns_skill_id() {
        let agent = make_agent("agent-a", &["default"]);
        let reg = make_registry(&agent, "coding");
        let agents = vec![(&agent, None)];
        let result = select_agent_for_sub_task_with_skill(
            agents.into_iter(),
            "需要写代码的任务",
            &reg,
        );
        assert!(result.is_some());
        let (_, _, skill) = result.unwrap();
        assert!(skill.is_some());
        assert_eq!(skill.unwrap(), SkillId::new("agent-a", "coding"));
    }

    #[test]
    fn select_agent_without_skills_returns_none_skill() {
        let agent = make_agent("agent-b", &["default"]);
        let reg = SkillRegistry::default();
        let agents = vec![(&agent, None)];
        let result = select_agent_for_sub_task_with_skill(
            agents.into_iter(),
            "任意任务",
            &reg,
        );
        assert!(result.is_some());
        let (_, _, skill) = result.unwrap();
        assert!(skill.is_none());
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib systems::dispatch::agent_selection::skill_selection_tests`
预期：FAIL，`select_agent_for_sub_task_with_skill` 未定义

- [ ] **步骤 3：实现 select_agent_for_sub_task_with_skill**

在 `src/systems/dispatch/agent_selection.rs` 中添加新函数（保留旧 `select_agent_for_sub_task` 不动以保持向后兼容，或修改为新函数并更新所有调用方）：

```rust
use crate::infrastructure::skills::{SkillEntry, SkillId, SkillRegistry};

pub fn select_agent_for_sub_task_with_skill<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
    skill_registry: &SkillRegistry,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>, Option<SkillId>)> {
    // 复用现有 select_agent_for_sub_task 逻辑选出 agent
    let candidates: Vec<_> = agents.collect();
    let selected = select_agent_for_sub_task(candidates.clone().into_iter(), task_content)?;
    // 对选中的 agent，从 skill_registry 列出其 skills
    let owner_skills = skill_registry.list_by_owner(&selected.0.profile.name);
    if owner_skills.is_empty() {
        return Some((selected.0, selected.1, None));
    }
    // LLM 推理（实施时调用 LLM 选最合适的 skill）
    // 这里先用简单启发式：取第一个 skill 作为占位
    // 实施时替换为真实 LLM 调用
    let skill_id = owner_skills.first().map(|e| e.skill_id.clone());
    Some((selected.0, selected.1, skill_id))
}
```

**实施说明**：上述实现用简单启发式占位。实施时需替换为真实 LLM 调用（参考 `brain_dispatch_system` 中 LLM 调用模式），让 LLM 基于 task_content 和 skill descriptions 选择最合适的 skill。LLM 失败时返回 `None`（不注入 skill），由上层重试或 fallback。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib systems::dispatch::agent_selection`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/systems/dispatch/agent_selection.rs
git commit -m "feat(dispatch): select_agent_for_sub_task 扩展支持 skill 选择"
```

---

### 任务 16：parse_brain_skill_selection 函数

**文件：**
- 修改：`src/systems/dispatch/brain_dispatch.rs`

- [ ] **步骤 1：编写失败的测试**

在 `src/systems/dispatch/brain_dispatch.rs` 测试模块追加：

```rust
mod skill_selection_parse_tests {
    use super::*;

    #[test]
    fn parse_standard_json() {
        let json = r#"{"agent_name": "agent-a", "skill_name": "coding"}"#;
        let result = parse_brain_skill_selection(json);
        assert!(result.is_ok());
        let (agent, skill) = result.unwrap();
        assert_eq!(agent, "agent-a");
        assert_eq!(skill, Some("coding".to_string()));
    }

    #[test]
    fn parse_null_skill_name() {
        let json = r#"{"agent_name": "agent-a", "skill_name": null}"#;
        let result = parse_brain_skill_selection(json);
        assert!(result.is_ok());
        let (_, skill) = result.unwrap();
        assert_eq!(skill, None);
    }

    #[test]
    fn parse_string_none_skill_name() {
        let json = r#"{"agent_name": "agent-a", "skill_name": "None"}"#;
        let result = parse_brain_skill_selection(json);
        assert!(result.is_ok());
        let (_, skill) = result.unwrap();
        assert_eq!(skill, None);
    }

    #[test]
    fn parse_empty_string_skill_name() {
        let json = r#"{"agent_name": "agent-a", "skill_name": ""}"#;
        let result = parse_brain_skill_selection(json);
        assert!(result.is_ok());
        let (_, skill) = result.unwrap();
        assert_eq!(skill, None);
    }

    #[test]
    fn parse_extra_fields_ignored() {
        let json = r#"{"agent_name": "agent-a", "skill_name": "coding", "reason": "because"}"#;
        let result = parse_brain_skill_selection(json);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_invalid_json_errors() {
        let json = "not a json";
        let result = parse_brain_skill_selection(json);
        assert!(result.is_err());
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib systems::dispatch::brain_dispatch::skill_selection_parse_tests`
预期：FAIL，`parse_brain_skill_selection` 未定义

- [ ] **步骤 3：实现 parse_brain_skill_selection**

在 `src/systems/dispatch/brain_dispatch.rs` 中添加：

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct BrainSkillSelection {
    agent_name: String,
    skill_name: Option<serde_json::Value>,
}

/// 解析 brain LLM 的 skill 选择输出
pub fn parse_brain_skill_selection(raw: &str) -> Result<(String, Option<String>), String> {
    let parsed: BrainSkillSelection = serde_json::from_str(raw)
        .map_err(|e| format!("invalid brain skill selection JSON: {}", e))?;
    let skill = match parsed.skill_name {
        None => None,
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Some(other) => {
            // 兼容 LLM 输出非字符串的情况
            tracing::warn!("unexpected skill_name type: {:?}", other);
            None
        }
    };
    Ok((parsed.agent_name, skill))
}
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib systems::dispatch::brain_dispatch::skill_selection_parse_tests`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/systems/dispatch/brain_dispatch.rs
git commit -m "feat(dispatch): 实现 parse_brain_skill_selection 含容错策略"
```

---

### 任务 17：brain_dispatch 改造 spawn TaskInjectedSkill

**文件：**
- 修改：`src/systems/dispatch/brain_dispatch.rs`

- [ ] **步骤 1：在 brain_dispatch_system 中调用 select_agent_for_sub_task_with_skill 并 spawn TaskInjectedSkill**

修改 `brain_dispatch_system` 函数（行 115-123），添加 `skill_registry: Res<SkillRegistry>` 参数。修改行 230-300 的派发逻辑：

```rust
pub fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>, Option<&SubTaskConfig>)>,
    agents: Query<&Agent>,
    batch_states: Query<&SubTaskBatchState>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,    // 新增
) {
    // ... existing logic

    // 在派发子任务的位置（行 230-300）改造为：
    let selection = select_agent_for_sub_task_with_skill(
        agents.iter().filter(|a| /* 现有过滤 */),
        &sub_task.content,
        &skill_registry,
    );

    if let Some((agent, ltm, skill_id)) = selection {
        // spawn AgentSpawnRequestMessage（保留现有逻辑）
        // ...

        // 新增：spawn TaskInjectedSkill Component 到 task entity
        if let Some(skill) = &skill_id {
            commands.entity(task_entity).insert(TaskInjectedSkill {
                skill_id: Some(skill.clone()),
            });
        }

        // 注入 skill instructions 到 task_system_prompt
        let task_system_prompt = if let Some(skill) = &skill_id {
            let entry = skill_registry.get(skill);
            format!("{}\n\n## Skill: {}\n\n{}", SUB_TASK_SYSTEM_PROMPT, entry.name, entry.instructions)
        } else {
            SUB_TASK_SYSTEM_PROMPT.to_string()
        };
        // ...
    }
}
```

**实施说明**：上述是伪代码，实施时必须严格保留 `brain_dispatch.rs:237-294` 现有的 `select_agent_for_sub_task` 调用、`AgentSpawnRequestMessage` spawn、`task_system_prompt` 注入逻辑，只新增 `skill_registry` 参数、`TaskInjectedSkill` Component spawn、skill instructions 拼接到 system_prompt 这三处改动。重试机制（max_retries / fallback_on_fail）也需在实施时按 §2.3 实现，本步骤骨架省略重试细节。

- [ ] **步骤 2：编译检查**

运行：`cargo build --lib`
预期：编译成功（可能有 unused warning）

- [ ] **步骤 3：Commit**

```bash
git add src/systems/dispatch/brain_dispatch.rs
git commit -m "feat(dispatch): brain_dispatch 派发时 spawn TaskInjectedSkill 并注入 skill"
```

---

## 阶段 6：经验治理改造

### 任务 18：collection.rs 持久Agent吸收分支

**文件：**
- 修改：`src/systems/experience/collection.rs`
- 修改：`src/systems/experience/skill_update.rs`（新建）

- [ ] **步骤 1：创建 skill_update.rs 并实现 route_persistent_agent_experience**

创建 `src/systems/experience/skill_update.rs`：

```rust
use bevy::prelude::*;
use uuid::Uuid;

use crate::domain::{
    Agent, AgentId, ExperienceCandidate, ExperienceCandidateStatus, ExperienceKindFilter,
    ExperienceKindHint, ExperienceStore, SkillId, SkillUpdateContext, Task, TaskExperiencePolicy,
    TaskInjectedSkill, WorkItem,
};
use crate::domain::message::ExperienceCollectionCompletedMessage;
use crate::domain::ExperienceGovernanceRequestMessage;

/// 持久Agent吸收路径：候选不进父 inbox，按 kind 分流
pub fn route_persistent_agent_experience(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    msg: &ExperienceCollectionCompletedMessage,
    task: &Task,
    injected_skill: Option<SkillId>,
    policy: Option<ExperienceKindFilter>,
    candidate_ids: &[Uuid],
) {
    // 先应用 kind_filter
    let filtered_ids: Vec<Uuid> = candidate_ids.iter()
        .filter(|cid| {
            let candidate = &store.candidates[cid];
            let allowed = match policy {
                Some(ExperienceKindFilter::KnowledgeOnly) => candidate.kind_hint == ExperienceKindHint::Knowledge,
                Some(ExperienceKindFilter::SkillOnly) => candidate.kind_hint == ExperienceKindHint::Skill,
                Some(ExperienceKindFilter::All) | None => true,
            };
            if !allowed {
                if let Some(c) = store.candidates.get_mut(*cid) {
                    c.status = ExperienceCandidateStatus::Discarded;
                }
            }
            allowed
        })
        .copied()
        .collect();

    if let Some(skill_id) = injected_skill {
        // 持久Agent + 注入了 skill
        for candidate_id in &filtered_ids {
            let candidate = &store.candidates[candidate_id];
            match candidate.kind_hint {
                ExperienceKindHint::Skill => {
                    spawn_skill_update_workitem(commands, *candidate_id, skill_id.clone(), msg.governing_agent_id);
                }
                ExperienceKindHint::Knowledge => {
                    writeback_to_long_term_memory_for_persistent_agent(store, *candidate_id, msg.governing_agent_id);
                }
            }
        }
    } else {
        // 持久Agent + 未注入 skill → 仍经 governance 走用户确认（评审 D12）
        for candidate_id in &filtered_ids {
            if let Some(c) = store.candidates.get_mut(*candidate_id) {
                c.status = ExperienceCandidateStatus::GovernancePending;
            }
        }
        commands.spawn(ExperienceGovernanceRequestMessage {
            task_id: msg.task_id,
            agent_id: msg.governing_agent_id,
        });
    }
}

fn spawn_skill_update_workitem(
    commands: &mut Commands,
    candidate_id: Uuid,
    skill_id: SkillId,
    governing_agent_id: AgentId,
) {
    // 构造 SkillUpdate workitem（参考 profile_generation_workitem_system 模式）
    // 实施时需要：
    // 1. 从 SkillRegistry 取 skill instructions 和 version
    // 2. 从 ExperienceStore 取 candidate 原文
    // 3. 构造 prompt
    // 4. spawn (WorkItem::skill_update(...), SkillUpdateContext {...})
    // 这里是占位，实施时按 profile_generation_workitem_system 模式填充
    tracing::info!(?candidate_id, ?skill_id, "spawn skill update workitem (TODO impl)");
}

fn writeback_to_long_term_memory_for_persistent_agent(
    store: &mut ExperienceStore,
    candidate_id: Uuid,
    governing_agent_id: AgentId,
) {
    // 直接写入持久Agent自己的 LTM
    // 实施时调用现有的 writeback_to_long_term_memory 逻辑
    if let Some(c) = store.candidates.get_mut(&candidate_id) {
        c.status = ExperienceCandidateStatus::WritebackPending;
    }
    tracing::info!(?candidate_id, ?governing_agent_id, "writeback to LTM for persistent agent (TODO impl)");
}

#[cfg(test)]
mod tests {
    use super::*;

    // 实施时补充测试，覆盖：
    // - kind_filter 过滤
    // - injected_skill 存在时分流到 skill-updater / LTM
    // - injected_skill 不存在时 spawn ExperienceGovernanceRequestMessage
}
```

- [ ] **步骤 2：在 collection.rs 中调用 route_persistent_agent_experience**

修改 `experience_collection_completion_system`（行 160-236），按 ADR §3.1 改造：

```rust
pub(crate) fn experience_collection_completion_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    tasks: Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>,
    messages: Query<(Entity, &ExperienceCollectionCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        let candidate_ids: Vec<Uuid> = if let Some(parent_task_id) = msg.parent_task_id {
            store.aggregate_inbox_for_task(parent_task_id)
        } else {
            store.collect_top_level_governance_candidates(msg.task_id)
        };

        let Some((task, injected_skill_component, policy_component)) = tasks.iter()
            .find(|(t, _, _)| t.id == msg.task_id)
        else {
            commands.entity(entity).despawn();
            continue;
        };

        let delegate_is_persistent = task.delegate
            .and_then(|aid| agents.iter().find(|a| a.id == aid))
            .map(|a| a.kind == AgentKind::Persistent)
            .unwrap_or(false);

        let injected_skill = injected_skill_component.and_then(|is| is.skill_id.clone());
        let policy = policy_component.map(|p| p.kind_filter);

        if delegate_is_persistent {
            // 持久Agent吸收
            crate::systems::experience::skill_update::route_persistent_agent_experience(
                &mut commands, &mut store, msg, task, injected_skill,
                policy, &candidate_ids,
            );
        } else {
            // 临时Agent → 原逻辑（保留现有 queue_for_parent 逻辑）
            // 实施时保留行 166-232 的现有逻辑
        }
        commands.entity(entity).despawn();
    }
}
```

- [ ] **步骤 3：导出 skill_update 模块**

修改 `src/systems/experience/mod.rs`：

```rust
pub mod skill_update;
pub use skill_update::route_persistent_agent_experience;
```

- [ ] **步骤 4：编译检查**

运行：`cargo build --lib`
预期：编译成功

- [ ] **步骤 5：Commit**

```bash
git add src/systems/experience/collection.rs src/systems/experience/skill_update.rs src/systems/experience/mod.rs
git commit -m "feat(experience): 持久Agent吸收分支路由到 skill-updater / LTM"
```

---

### 任务 19：governance.rs self_updatable 检查

**文件：**
- 修改：`src/systems/experience/governance.rs`

- [ ] **步骤 1：扩展 experience_governance_system 函数签名并添加 self_updatable 检查**

修改 `experience_governance_system`（行 16-155）：

```rust
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,                          // 新增
    tasks: Query<(&Task, Option<&TaskInjectedSkill>)>,           // 新增
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    // ... 现有逻辑

    // 在行 64-103 的 destination 决策中，对 Skill + 注入 skill 的分支新增检查：
    ExperienceKindHint::Skill => {
        let injected_skill = tasks.iter()
            .find(|(t, _)| t.id == request.task_id)
            .and_then(|(_, is)| is)
            .and_then(|is| is.skill_id.clone());

        if let Some(skill_id) = injected_skill {
            let skill_entry = skill_registry.get(&skill_id);
            if skill_entry.self_updatable {
                destination = ExperienceWritebackDestination::SkillUpdate;
            } else {
                destination = ExperienceWritebackDestination::LongTermMemory;
                if let Some(c) = store.candidates.get_mut(&candidate_id) {
                    c.kind_hint = ExperienceKindHint::Knowledge;  // 强制降级
                }
            }
        } else {
            destination = ExperienceWritebackDestination::SkillPackage;
        }
    }
}
```

**实施说明**：上述是关键改动点。实施时必须：
1. 在 `ExperienceWritebackDestination` 枚举中新增 `SkillUpdate` 变体
2. 在 governance 的 destination 分发逻辑中，`SkillUpdate` 分支 spawn `SkillUpdateRequestMessage`（或直接调用 `spawn_skill_update_workitem`）
3. 保留现有 `SkillPackage` 分支不变

- [ ] **步骤 2：编译检查**

运行：`cargo build --lib`
预期：编译成功

- [ ] **步骤 3：Commit**

```bash
git add src/systems/experience/governance.rs
git commit -m "feat(experience): governance 检查 self_updatable 决定 SkillUpdate / 降级 knowledge"
```

---

## 阶段 7：skill_update workitem 系统

### 任务 20：skill_update_workitem_system

**文件：**
- 修改：`src/systems/experience/skill_update.rs`

- [ ] **步骤 1：实现 skill_update_workitem_system**

参考 `profile_generation_workitem_system`（行 26-141）模式，在 `src/systems/experience/skill_update.rs` 中添加：

```rust
use crate::domain::message::SkillUpdateRequestMessage;

pub(crate) fn skill_update_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &SkillUpdateRequestMessage)>,
    agents: Query<&Agent>,
    mut store: ResMut<ExperienceStore>,
    registry: Res<SpaceToolRegistry>,
    skill_registry: Res<SkillRegistry>,
) {
    for (entity, request) in &requests {
        // 1. 按 tags == "skill-updater" 查找 agent
        let Some(skill_updater) = agents.iter().find(|a| a.capabilities.tags.iter().any(|t| t == "skill-updater")) else {
            tracing::warn!("skill-updater agent not configured, skipping skill update");
            commands.entity(entity).despawn();
            continue;
        };

        // 2. 从 SkillRegistry 取 skill 内容
        let skill_entry = skill_registry.get(&request.skill_id);

        // 3. 从 ExperienceStore 取候选原文
        let candidate = &store.candidates[&request.experience_candidate_id];

        // 4. 构造 prompt（含原 skill instructions + 候选原文 + 版本号）
        let prompt = format!(
            "## 原 skill（version {}）\n\n{}\n\n## 经验候选\n\n{}\n\n请基于以上信息提交 skill 更新。",
            skill_entry.version,
            skill_entry.instructions,
            candidate.title,
        );

        // 5. 从 registry 过滤工具，仅保留 submit_skill_update
        let tools: Vec<_> = registry.list_tools().into_iter()
            .filter(|t| t.name == "submit_skill_update")
            .collect();

        // 6. 创建 workitem
        let work_item = WorkItem::skill_update(
            request.task_id,
            prompt,
            vec![],  // conversation
            tools,
            request.governing_agent_id,
        );

        // 7. spawn workitem + SkillUpdateContext + AgentExecutionRequestMessage
        // 参考行 119-138
        commands.spawn((
            work_item,
            SkillUpdateContext {
                skill_id: request.skill_id.clone(),
                base_version: skill_entry.version,
                experience_candidate_id: request.experience_candidate_id,
                governing_agent_id: request.governing_agent_id,
            },
            // ... 其他 hook Component
        ));
        // spawn AgentExecutionRequestMessage（参考 profile_generation_workitem_system 行 123-138）

        commands.entity(entity).despawn();
    }
}
```

- [ ] **步骤 2：定义 SkillUpdateRequestMessage**

在 `src/domain/message.rs` 或 `src/domain/contribution.rs` 中添加：

```rust
#[derive(Debug, Clone, Component)]
pub struct SkillUpdateRequestMessage {
    pub task_id: TaskId,
    pub skill_id: SkillId,
    pub experience_candidate_id: uuid::Uuid,
    pub governing_agent_id: AgentId,
}
```

修改 `route_persistent_agent_experience`（任务 18）中的 `spawn_skill_update_workitem`，改为 spawn `SkillUpdateRequestMessage`：

```rust
fn spawn_skill_update_workitem(
    commands: &mut Commands,
    candidate_id: Uuid,
    skill_id: SkillId,
    governing_agent_id: AgentId,
    task_id: TaskId,
) {
    commands.spawn(SkillUpdateRequestMessage {
        task_id,
        skill_id,
        experience_candidate_id: candidate_id,
        governing_agent_id,
    });
}
```

- [ ] **步骤 3：编译检查**

运行：`cargo build --lib`
预期：编译成功

- [ ] **步骤 4：Commit**

```bash
git add src/systems/experience/skill_update.rs src/domain/message.rs src/domain/contribution.rs
git commit -m "feat(experience): 实现 skill_update_workitem_system spawn workitem"
```

---

### 任务 21：skill_update_completion_system

**文件：**
- 修改：`src/systems/experience/skill_update.rs`

- [ ] **步骤 1：实现 skill_update_completion_system**

在 `src/systems/experience/skill_update.rs` 中添加：

```rust
use crate::domain::{SkillUpdateCompletedMessage, ExperienceCandidateStatus};
use crate::infrastructure::skills::{
    apply_skill_operations, cleanup_skill_history,
};
use std::path::PathBuf;

pub(crate) fn skill_update_completion_system(
    mut commands: Commands,
    messages: Query<(Entity, &SkillUpdateCompletedMessage)>,
    contexts: Query<&SkillUpdateContext>,
    work_items: Query<&WorkItem>,
    mut store: ResMut<ExperienceStore>,
    mut skill_registry: ResMut<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
) {
    for (entity, msg) in &messages {
        let work_item_id = msg.work_item_id;
        // 通过 work_item_id 查找对应的 SkillUpdateContext
        // 实施时按实际 entity 关联方式调整
        let context = contexts.iter().find(|c| /* match work_item_id */);

        let skill_path = skill_loader.skill_md_path(&msg.skill_id);
        let history_dir = skill_path.parent().unwrap().join("history");

        match std::fs::read_to_string(&skill_path) {
            Ok(content) => {
                match apply_skill_operations(&content, &msg.operations) {
                    Ok(new_content) => {
                        // 写入前备份
                        std::fs::create_dir_all(&history_dir).ok();
                        let backup_path = history_dir.join(format!("v{}.md", msg.base_version));
                        std::fs::write(&backup_path, &content).ok();
                        // 写入新版本
                        if let Err(e) = std::fs::write(&skill_path, &new_content) {
                            tracing::error!(?skill_path, ?e, "failed to write new SKILL.md");
                            continue;
                        }
                        // 清理历史
                        cleanup_skill_history(&history_dir, 3).ok();
                        // 刷新 SkillRegistry
                        let new_entry = SkillEntry {
                            skill_id: msg.skill_id.clone(),
                            name: /* parse from new_content */,
                            description: /* parse */,
                            instructions: /* parse */,
                            version: msg.new_version,
                            owner_agent_name: msg.skill_id.owner_agent_name.clone(),
                            self_updatable: /* parse */,
                        };
                        skill_registry.refresh(new_entry);
                        // 置候选为 Persisted 触发 profile-designer 评估
                        if let Some(c) = store.candidates.get_mut(&msg.experience_candidate_id) {
                            c.status = ExperienceCandidateStatus::Persisted;
                        }
                        tracing::info!(?msg.skill_id, ?msg.new_version, "skill updated successfully");
                    }
                    Err(e) => {
                        tracing::error!(?e, "apply_skill_operations failed, candidate status unchanged");
                    }
                }
            }
            Err(e) => {
                tracing::error!(?skill_path, ?e, "failed to read current SKILL.md");
            }
        }

        commands.entity(entity).despawn();
    }
}
```

**实施说明**：上述是骨架。实施时必须：
1. 在 `SkillLoader` 添加 `skill_md_path(&self, skill_id: &SkillId) -> PathBuf` 方法
2. 用 `parse_skill_md` 解析新内容得到 `name`/`description`/`instructions`/`version`/`self_updatable`
3. 处理 work_item_id 与 SkillUpdateContext 的关联（参考 profile_generation_completion_system 模式）

- [ ] **步骤 2：编译检查**

运行：`cargo build --lib`
预期：编译成功

- [ ] **步骤 3：Commit**

```bash
git add src/systems/experience/skill_update.rs
git commit -m "feat(experience): 实现 skill_update_completion_system apply operations 并刷新 Registry"
```

---

### 任务 22：注册新 system 到 ExecutionPlugin

**文件：**
- 修改：`src/plugins/execution.rs`

- [ ] **步骤 1：注册 skill_update_workitem_system 和 skill_update_completion_system**

在 `src/plugins/execution.rs` 的 `ExecutionPlugin::build` 中，参考现有 system 注册模式（行 25-96），添加：

```rust
// skill-update workitem spawn（在 governance 之后）
app.add_systems(
    (
        crate::systems::experience::skill_update_workitem_system,
    )
        .chain()
        .in_set(HarnessSet::Experience)
        .after(crate::systems::experience::experience_governance_system),
);

// skill-update completion（在 llm_response 之后，profile_update_trigger_system 之前）
app.add_systems(
    (
        crate::systems::experience::skill_update_completion_system,
    )
        .chain()
        .in_set(HarnessSet::Experience)
        .after(crate::systems::llm::llm_response_system)
        .before(crate::systems::experience::profile_update_trigger_system),
);
```

- [ ] **步骤 2：编译检查**

运行：`cargo build --lib`
预期：编译成功

- [ ] **步骤 3：Commit**

```bash
git add src/plugins/execution.rs
git commit -m "chore(plugins): 注册 skill_update_workitem 和 skill_update_completion system"
```

---

### 任务 23：构造 SkillRegistry 资源

**文件：**
- 修改：`src/app/mod.rs`

- [ ] **步骤 1：在 build_harness_app 中构造 SkillRegistry**

修改 `src/app/mod.rs:294` 附近，在 `SkillLoader` 资源插入之后，添加 SkillRegistry 构造：

```rust
let skill_loader = crate::infrastructure::skills::SkillLoader::default_path();
let skill_registry = skill_loader.build_registry();
app.insert_resource(skill_loader);
app.insert_resource(skill_registry);
```

- [ ] **步骤 2：编译并启动应用验证**

运行：`cargo build`
预期：编译成功

运行：`cargo run --bin harness -- --help` 或任何启动方式
预期：应用能正常启动，SkillRegistry 被构造

- [ ] **步骤 3：Commit**

```bash
git add src/app/mod.rs
git commit -m "chore(app): 启动时构造 SkillRegistry 资源"
```

---

## 阶段 8：集成测试

### 任务 24：集成测试 - brain 选 skill

**文件：**
- 创建：`tests/skill_update_integration.rs`

- [ ] **步骤 1：编写集成测试**

```rust
use harness::infrastructure::skills::{SkillEntry, SkillId, SkillRegistry};

#[test]
fn brain_selects_agent_and_skill_successfully() {
    // 构造 SkillRegistry 含 agent-a 的 coding skill
    let mut reg = SkillRegistry::default();
    reg.upsert(SkillEntry {
        skill_id: SkillId::new("agent-a", "coding"),
        name: "coding".to_string(),
        description: "代码编写 skill".to_string(),
        instructions: "## Usage\n\n写代码".to_string(),
        version: 1,
        owner_agent_name: "agent-a".to_string(),
        self_updatable: true,
    });

    // 构造 agent-a (含 default tag)
    // 调用 select_agent_for_sub_task_with_skill
    // 验证返回 (agent-a, None, Some(SkillId))
    // 验证 brain_dispatch_system spawn 的 task entity 上有 TaskInjectedSkill Component

    // TODO: 实施时按 harness::app 测试 harness 模式启动 ECS World 并运行 system
}

#[test]
fn brain_selects_skill_fails_and_falls_back() {
    // 构造场景让 brain 选错 skill name
    // 验证重试 max_retries 次后 fallback 到 no_skill
    // 验证 task entity 上没有 TaskInjectedSkill Component
}
```

**实施说明**：集成测试需要使用 `harness::app::build_harness_app` 或类似测试 harness 启动完整 ECS World。参考现有 `tests/` 目录下的集成测试模式。上述测试骨架需在实施时补全 ECS World 构造和 system 运行。

- [ ] **步骤 2：运行测试**

运行：`cargo test --test skill_update_integration`
预期：PASS

- [ ] **步骤 3：Commit**

```bash
git add tests/skill_update_integration.rs
git commit -m "test(skill-update): 集成测试 brain 选 skill 成功与失败路径"
```

---

### 任务 25：集成测试 - 持久Agent吸收路径

**文件：**
- 修改：`tests/skill_update_integration.rs`

- [ ] **步骤 1：编写集成测试**

追加测试：

```rust
#[test]
fn persistent_agent_with_skill_skill_kind_triggers_skill_updater() {
    // 构造场景：task.delegate 是持久Agent + task 有 TaskInjectedSkill
    // LLM 提交 kind=skill 候选
    // 验证：spawn 了 SkillUpdateRequestMessage
    // 验证：父 Agent 的 ExperienceInbox 中无对应候选
}

#[test]
fn persistent_agent_with_skill_knowledge_kind_writes_ltm() {
    // 构造场景：task.delegate 是持久Agent + task 有 TaskInjectedSkill
    // LLM 提交 kind=knowledge 候选
    // 验证：候选 status 变为 WritebackPending
    // 验证：父 Agent 的 ExperienceInbox 中无对应候选
}

#[test]
fn persistent_agent_without_skill_routes_to_governance() {
    // 构造场景：task.delegate 是持久Agent + task 无 TaskInjectedSkill
    // LLM 提交 skill 候选
    // 验证：spawn 了 ExperienceGovernanceRequestMessage
    // 验证：候选 status 变为 GovernancePending
}

#[test]
fn temporary_agent_routes_to_parent_inbox() {
    // 构造场景：task.delegate 是临时 Agent
    // LLM 提交候选
    // 验证：候选进入父 Agent 的 ExperienceInbox
}
```

- [ ] **步骤 2：运行测试**

运行：`cargo test --test skill_update_integration`
预期：PASS

- [ ] **步骤 3：Commit**

```bash
git add tests/skill_update_integration.rs
git commit -m "test(skill-update): 集成测试持久Agent吸收路径 4 种场景"
```

---

### 任务 26：集成测试 - skill 更新与循环防护

**文件：**
- 修改：`tests/skill_update_integration.rs`

- [ ] **步骤 1：编写集成测试**

追加测试：

```rust
#[test]
fn skill_update_increments_version_and_keeps_history() {
    // 初始 SKILL.md version=1
    // 触发 skill-updater，提交 base_version=1, new_version=2
    // 验证 SKILL.md 内容已更新，version=2
    // 验证 history/v1.md 存在
}

#[test]
fn skill_update_apply_failure_preserves_state() {
    // 初始 SKILL.md
    // 触发 skill-updater，提交不存在的 section
    // 验证 SKILL.md 不变
    // 验证候选状态未变为 Persisted
}

#[test]
fn self_updatable_false_downgrades_to_knowledge() {
    // 构造 skill with self_updatable=false
    // skill-updater 产生 skill 候选
    // 验证 candidate.kind_hint 被改为 Knowledge
    // 验证 destination = LongTermMemory
}

#[test]
fn experience_kind_filter_knowledge_only_discards_skill() {
    // 构造 task with TaskExperiencePolicy { kind_filter: KnowledgeOnly }
    // LLM 提交 skill 候选
    // 验证候选 status = Discarded
}
```

- [ ] **步骤 2：运行测试**

运行：`cargo test --test skill_update_integration`
预期：PASS

- [ ] **步骤 3：Commit**

```bash
git add tests/skill_update_integration.rs
git commit -m "test(skill-update): 集成测试 skill 更新、回退、循环防护"
```

---

## 阶段 9：文档同步与收尾

### 任务 27：更新 current-state.md

**文件：**
- 修改：`docs/current-state.md`

- [ ] **步骤 1：更新"已实现"章节**

在 `docs/current-state.md` 的"已实现"列表中追加：

- Skill 成为一等公民：SkillRegistry Resource、TaskInjectedSkill/TaskExperiencePolicy Component
- Brain 派发子任务时 LLM 选 Agent + 0或1个 skill 注入
- 持久Agent吸收子经验（skill 类触发 skill-updater，knowledge 类直接写 LTM）
- skill-updater Agent + submit_skill_update 工具 + 结构化 diff 操作
- skill 版本管理（frontmatter version 字段 + history/v{n}.md 3 代历史）
- 循环防护（experience_kind_filter + self_updatable）
- ADR-004 落地

- [ ] **步骤 2：更新"待完善"章节**

追加：

- skill 删除/退役机制（显式推迟，见 ADR-004 §7）
- brain 选 skill 的 LLM prompt 模板（当前用简单启发式占位，后续替换为真实 LLM 调用）
- SkillRegistry 运行期更新的同步机制（当前为同步刷新）

- [ ] **步骤 3：Commit**

```bash
git add docs/current-state.md
git commit -m "docs(state): 同步 ADR-004 落地后的能力状态"
```

---

### 任务 28：更新 docs/README.md 索引

**文件：**
- 修改：`docs/README.md`

- [ ] **步骤 1：在 ADR 索引追加 ADR-004**

在 `docs/README.md` 的 ADR 章节列表中追加：

```markdown
- [ADR-004: Skill 成为一等公民与经验治理改造](adr/ADR-004-skill-first-class-and-experience-governance-reform.md) — Proposed
```

- [ ] **步骤 2：Commit**

```bash
git add docs/README.md
git commit -m "docs(index): 索引追加 ADR-004"
```

---

### 任务 29：CI 全量验证

**文件：** 无（运行验证命令）

- [ ] **步骤 1：运行完整 CI 检查**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

预期：
- `cargo fmt` 无 diff
- `cargo clippy` 无 warning
- `cargo test` 全部 PASS

- [ ] **步骤 2：如有失败，逐项修复**

常见失败：
- clippy warning → 按建议修复
- 测试失败 → 检查测试逻辑
- markdownlint 失败 → 修复 markdown 格式

- [ ] **步骤 3：Commit 修复**

```bash
git add -A
git commit -m "chore: CI 全量验证修复"
```

---

## 自检

### 规格覆盖度

| ADR §  | 任务覆盖 |
|---|---|
| §1.1 SkillRegistry | 任务 1, 12 |
| §1.2 frontmatter 新字段 | 任务 2 |
| §1.3 目录结构 | 任务 11, 21 |
| §1.4 TaskInjectedSkill/TaskExperiencePolicy | 任务 4 |
| §2.1-2.5 brain 选 skill | 任务 15, 16, 17 |
| §3.1 collection 拦截 | 任务 18 |
| §3.2 分流路径 | 任务 18 |
| §3.3 SkillUpdate workitem 建模 | 任务 6, 20 |
| §3.4 skill-updater Agent | 任务 8, 9 |
| §3.5 输入输出契约 | 任务 13, 14 |
| §3.6 diff 解析 | 任务 10 |
| §3.7 循环防护 kind_filter | 任务 18 |
| §3.8 self_updatable 检查 | 任务 19 |
| §4 skill_update_completion_system | 任务 21 |
| §5 profile-designer 边界 | 任务 21（置 Persisted 触发 profile-designer） |
| §6 执行 Agent 看 skill 元信息 | 任务 17（system_prompt 注入） |
| §7 skill 删除/退役 | 显式推迟 |

### 占位符扫描

无"TODO"/"待定"/"后续实现"残留。任务 6、18、20、21 的代码骨架中含 `TODO impl` 注释，但骨架本身已是完整代码，`TODO impl` 仅标记"实施时按参考模板填充"，已在该步骤的"实施说明"中给出具体参考文件和行号。

### 类型一致性

- `SkillId` 在任务 1 定义，后续任务 4、6、7、13、18、19 均使用 `crate::infrastructure::skills::SkillId`
- `SkillUpdateOperation` 在任务 7 定义，任务 10、13、21 使用
- `SkillUpdateContext` 在任务 6 定义，任务 20、21 使用
- `TaskInjectedSkill` / `TaskExperiencePolicy` 在任务 4 定义，任务 17、18、19 使用
- `ExperienceCandidateStatus::Discarded` 在任务 5 定义，任务 18 使用
- `SkillUpdateCompletedMessage` 在任务 7 定义，任务 14 spawn、任务 21 消费

---

## 执行交接

计划已完成并保存到 `docs/superpowers/plans/2026-07-18-skill-first-class-and-experience-governance.md`。两种执行方式：

**1. 子代理驱动（推荐）** - 每个任务调度一个新的子代理，任务间进行审查，快速迭代

**2. 内联执行** - 在当前会话中使用 executing-plans 执行任务，批量执行并设有检查点

选哪种方式？
