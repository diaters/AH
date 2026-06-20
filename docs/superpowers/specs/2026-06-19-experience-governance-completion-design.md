# 经验治理模块功能补全设计

> **状态：当前有效**

## 背景与目标

经验治理模块已完成参数与概念简化（见 `2026-06-19-experience-submission-simplification-design.md`），但仍有 5 项功能缺口需要补全：

1. **#8** `list_experience_candidates` 输出字段过时（`kind_hint` 未更新，缺少新字段）
2. **#9** `IncubationProposal` 执行链路未处理 `skill_candidate_ids`
3. **#2** 长期记忆无淘汰机制，低价值条目永久累积
4. **#4** Skill Package 写回后无加载机制，Agent 不感知已有 Skill
5. **#6** 非顶层多子候选无合并能力，相似经验重复上送

本次设计目标：**分两个子项目补全上述功能，A 优先（数据层修复），B 在后（功能增强）**。

## 子项目 A：数据层修复

### 一、list_experience_candidates 字段修正

**现状**：输出使用 `kind_hint` 字段名，且缺少 `content`/`skill_description` 等新字段，对 Agent 参考价值有限。

**方案**：

输出字段更新：

```json
{
  "candidate_id": "...",
  "title": "...",
  "kind": "Knowledge",
  "status": "Submitted",
  "summary": "knowledge 类显示前 200 字 content；skill 类显示 skill_description"
}
```

变更要点：

- `kind_hint` → `kind`
- 新增 `summary` 字段：根据 kind 提取关键信息
  - Knowledge → `content` 前 200 字符（截断加 `…`）
  - Skill → `skill_description` 全文
- 不暴露完整载荷（避免 prompt 污染）

**涉及文件**：`src/systems/tools/builtin/list_experience_candidates.rs`

### 二、IncubationProposal Skill 候选处理

**现状**：`writeback_incubation_proposal` 只处理 `knowledge_candidate_ids`（写入新 Agent 的 LTM），`skill_candidate_ids` 被忽略。

**方案**：

在知识候选写入 LTM 之后，增加 Skill 候选处理：

1. 遍历 `skill_candidate_ids`，对每个 Skill 候选构造 `SkillPackageDraft`
2. 调用 `asset_service.persist_skill_package(&profile.name, &draft)` 写入新 Agent 的 Skill 目录
3. 在 `IncubatedAgentRecord` 中新增 `skills: Option<Vec<String>>` 字段，记录 Skill 相对路径列表
4. `agents.toml` 中新 Agent 记录包含 `skills` 字段

`IncubatedAgentRecord` 更新：

```rust
pub struct IncubatedAgentRecord {
    pub name: String,
    pub model: String,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,  // 新增：Skill Package 相对路径
}
```

`writeback_incubation_proposal` 签名需新增 `asset_service: &AgentAssetService` 参数。

**涉及文件**：
- `src/systems/experience/writeback.rs` — 增加 Skill 候选处理
- `src/infrastructure/incubation/agent_registry.rs` — `IncubatedAgentRecord` 新增 `skills` 字段
- `src/domain/contribution.rs` — `AgentConfig/AgentConfigEntry` 新增 `skills` 字段（如果 agents.toml 解析需要）

---

## 子项目 B：功能增强

### 三、长期记忆淘汰（移除 + 文件归档）

**现状**：`decay_score` 只降不删，低价值条目永久累积。`memory_selection` 用 `decay_score > 0.2` 过滤，但条目仍占内存和持久化空间。

**方案**：

淘汰条件：`decay_score < 0.1` 且 `!pin` 且 `importance != Critical`。

淘汰行为：

1. 条目从 `LongTermMemory.entries` 中移除
2. 移除前，将条目追加写入 `<agent-name>/archive.jsonl`（每行一个 JSON 对象）
3. 归档文件路径：与 LTM 持久化目录同级，如 `.harness/memory/agents/<agent-name>/archive.jsonl`
4. 归档为追加写入，无需读-改-写
5. `LongTermMemory` 不新增任何字段，内存中不保留归档数据
6. 不提供恢复接口

`apply_memory_decay` 更新：

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

        // 淘汰条件
        let should_evict = entry.decay_score < 0.1
            && !entry.pin
            && entry.importance != MemoryImportance::Critical;

        if should_evict {
            evicted.push(entry.clone());
            false // 移除
        } else {
            true // 保留
        }
    });
    evicted
}
```

`long_term_memory_decay_system` 更新：

```rust
pub(crate) fn long_term_memory_decay_system(
    mut agents: Query<(&Agent, &mut LongTermMemory)>,
    service: Res<LongTermMemoryService>,
) {
    let now = chrono::Utc::now();
    for (agent, mut memory) in &mut agents {
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

`LongTermMemoryService` 新增 `archive_entries` 方法：

```rust
pub fn archive_entries(&self, agent_name: &str, entries: &[LongTermMemoryEntry]) {
    let archive_path = self.base_dir.join(agent_name).join("archive.jsonl");
    if let Some(parent) = archive_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&archive_path)
        .unwrap();
    for entry in entries {
        let _ = writeln!(file, "{}", serde_json::to_string(entry).unwrap());
    }
}
```

**涉及文件**：
- `src/systems/memory.rs` — 更新 `apply_memory_decay` 和 `long_term_memory_decay_system`
- `src/infrastructure/memory/service.rs` — 新增 `archive_entries` 方法
- 相关测试文件

### 四、Skill Package 加载为知识注入

**现状**：Skill 写回后无加载机制，Agent 启动时不感知已有 Skill。

**方案**：

新增 `SkillLoader` 基础设施服务，负责扫描和解析 SKILL.md：

```rust
/// Skill 加载器：扫描 Agent 的 skills 目录，解析 SKILL.md。
#[derive(Resource, Debug, Clone)]
pub struct SkillLoader {
    base_dir: PathBuf,
}

pub struct LoadedSkill {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

impl SkillLoader {
    pub fn default_path() -> Self {
        Self { base_dir: PathBuf::from(".harness/assets/agents") }
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
}

fn parse_skill_md(path: &std::path::Path) -> Option<LoadedSkill> {
    let content = std::fs::read_to_string(path).ok()?;
    // 解析 YAML frontmatter
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

    Some(LoadedSkill { name, description, instructions })
}
```

Agent 初始化时注入 Skill 为系统提示区段：

在 `init_agent_memory_system` 或 LLM 上下文组装阶段，调用 `SkillLoader::load_skills`，将 Skill 组装为结构化文本块追加到系统提示：

```
## 可用技能

### <skill-name>
<description>

<instructions>
```

注入位置：在 `src/systems/dispatch/task_dispatch.rs` 的上下文组装中，系统提示末尾追加 Skill 区段。

**涉及文件**：
- `src/infrastructure/skills/mod.rs` — 新增 `SkillLoader` 模块
- `src/infrastructure/skills/loader.rs` — `SkillLoader` 实现
- `src/infrastructure/mod.rs` — 导出
- `src/app/mod.rs` — 注册 `SkillLoader` Resource
- `src/systems/dispatch/task_dispatch.rs` — 上下文组装时注入 Skill

### 五、非顶层 LLM 合并子候选

**现状**：非顶层只做汇聚（Aggregated），多个子 Agent 提交相似经验时无去重合并。

**方案**：

在 `ExperienceCollectionCompletionSystem` 中，当非顶层汇聚完成且候选数 > 1 时，触发 LLM 合并。

新增 `ExperienceConsolidationWorkItem`：

- 类似 `ExperienceCollectionWorkItem`，但 prompt 要求 LLM 对多个相似候选做去重合并
- 合并 prompt 输入：所有子候选的 title + content/description
- 合并 prompt 输出：调用 `submit_experience_candidate` 提交精炼后的候选

合并流程：

1. 非顶层汇聚完成后，检查候选数
2. 候选数 ≤ 1：跳过合并，直接上送
3. 候选数 > 1：按 kind 分组（Knowledge 一组，Skill 一组）
4. 每组创建一个 `ExperienceConsolidationWorkItem`
5. WorkItem 的 prompt 包含所有同组候选的完整信息
6. LLM 输出合并后的候选（通过 `submit_experience_candidate` 提交）
7. 合并后原始子候选标记为 `Superseded`（新增状态），不进入上层治理

新增 `ExperienceCandidateStatus::Superseded`：

```rust
pub enum ExperienceCandidateStatus {
    Submitted,
    InInbox,
    Aggregated,
    Superseded,  // 新增：被合并候选替代
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

合并 prompt 模板：

```
你是一个经验整理助手。以下是同一任务下多个 Agent 提交的经验候选，请对它们进行去重和合并。

## 输入候选

{遍历每个候选，输出 title + content/description}

## 要求

1. 去除重复或高度相似的经验
2. 合并互补的经验为更完整的版本
3. 通过调用 submit_experience_candidate 提交合并后的候选
4. 如果所有候选都是重复的，只提交一个最完整的版本
5. 不要提交任何原始候选，只提交合并后的版本
```

`ExperienceCollectionCompletionSystem` 更新：

```rust
// 非顶层：汇聚后判断是否需要合并
if let Some(parent_task_id) = msg.parent_task_id {
    let ids = store.aggregate_inbox_for_task(parent_task_id);
    let candidates: Vec<_> = ids.iter()
        .filter_map(|id| store.candidates.get(id))
        .collect();

    if candidates.len() > 1 {
        // 按 kind 分组，为每组创建合并 WorkItem
        spawn_consolidation_workitems(&mut commands, &store, &candidates, parent_task_id, msg.governing_agent_id);
    }
    // 候选数 ≤ 1 时直接上送，无需合并
}
```

**涉及文件**：
- `src/domain/contribution.rs` — 新增 `Superseded` 状态
- `src/systems/experience/collection.rs` — 汇聚后触发合并
- `src/domain/space.rs` — 新增 `ExperienceConsolidationRequestMessage` 等消息类型
- `src/systems/experience/consolidation.rs` — 新增合并系统
- `src/systems/experience/mod.rs` — 注册新系统

---

## 影响分析

### 子项目 A

| 文件 | 变更类型 |
|------|---------|
| `src/systems/tools/builtin/list_experience_candidates.rs` | 修改 |
| `src/systems/experience/writeback.rs` | 修改 |
| `src/infrastructure/incubation/agent_registry.rs` | 修改 |
| `src/domain/contribution.rs` | 修改（AgentConfigEntry 新增 skills） |

### 子项目 B

| 文件 | 变更类型 |
|------|---------|
| `src/systems/memory.rs` | 修改（淘汰逻辑） |
| `src/infrastructure/memory/service.rs` | 修改（archive_entries） |
| `src/infrastructure/skills/mod.rs` | 新增 |
| `src/infrastructure/skills/loader.rs` | 新增 |
| `src/infrastructure/mod.rs` | 修改 |
| `src/app/mod.rs` | 修改 |
| `src/systems/dispatch/task_dispatch.rs` | 修改 |
| `src/domain/contribution.rs` | 修改（Superseded 状态） |
| `src/domain/space.rs` | 修改（合并消息类型） |
| `src/systems/experience/collection.rs` | 修改 |
| `src/systems/experience/consolidation.rs` | 新增 |
| `src/systems/experience/mod.rs` | 修改 |

### 需要修改的测试

- `src/systems/memory.rs` 内联测试 — 适配 `apply_memory_decay` 返回值变更
- `src/systems/experience/collection.rs` 内联测试 — 适配合并逻辑
- `tests/experience_layered_governance_flow.rs` — 适配 IncubationProposal Skill 处理
- `tests/memory_persistence_flow.rs` — 适配淘汰逻辑

---

## 验证命令

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
