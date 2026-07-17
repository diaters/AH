# ADR-004: Skill 成为一等公民与经验治理改造

## 状态

Proposed（v4 — 已根据三轮评审报告修正：v1 `logs/2026-07-18-adr-004-skill-first-class-review.md`、v2 `logs/2026-07-18-adr-004-skill-first-class-review-v2.md`、v3 用户反馈 F15）

## 生效范围

本决策自 2026-07-17 起提出，关联设计文档：

- `docs/design/2026-07-13-agent-profile-llm-generation-design.md`（profile-designer 现有职责）
- `docs/design/2026-06-06-workitem-boundary-design.md`（WorkItem 边界）
- `docs/current-state.md`

## 背景

当前 skill 在代码库中不是一等公民：

- `LoadedSkill` 仅在 prompt 拼装时临时构造（[loader.rs:22-27](../../src/infrastructure/skills/loader.rs#L22-L27)），没有版本号、没有注册表
- skill 通过 `agent.profile.name` 字符串间接关联（[task_dispatch.rs:215](../../src/systems/dispatch/task_dispatch.rs#L215)），无法被 brain 显式选择
- brain 子任务派发路径（[brain_dispatch.rs:237-294](../../src/systems/dispatch/brain_dispatch.rs#L237-L294)）完全不加载 skill
- 经验汇聚当前为两级（子→父 inbox、父终态合并 root+inbox），所有候选都透传到父 Agent
- `writeback_to_skill_package` 直接落盘 SKILL.md，没有版本管理、没有 diff 机制

本决策需要在三个维度同时改造：

1. **Skill 数据模型升级**：让 skill 成为可被 brain 选择、带版本、可注册的一等公民
2. **Brain 派发改造**：brain 在派发子任务时，为 Agent 选择 0 或 1 个 skill 注入
3. **经验治理改造**：持久Agent吸收子经验（含 knowledge 和 skill 类），skill 类经验触发 skill-updater workitem，避免子经验层层透传到顶层

## 决策

### 1. Skill 数据模型一等公民化

#### 1.1 新增 `SkillRegistry` Resource

```rust
#[derive(Resource, Default)]
pub struct SkillRegistry {
    pub skills: HashMap<SkillId, SkillEntry>,
}

#[derive(Clone)]
pub struct SkillEntry {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub instructions: String,        // brain 看不到，仅执行 Agent 可见
    pub version: u32,
    pub owner_agent_name: String,    // 归属持久Agent
    pub self_updatable: bool,        // skill-updater 自己的 skill 标 false，用于循环防护
}
```

- `SkillId` 为新增类型，封装 `owner_agent_name + skill_name` 以保证全局唯一
- `SkillRegistry` 由 `SkillLoader` 在启动时扫描 `.harness/assets/agents/<agent>/skills/*/SKILL.md` 构造，运行期由 skill-updater 写入后刷新

#### 1.2 SKILL.md frontmatter 扩展

新增字段：

```yaml
---
name: <skill_name>
description: <纯自然文本，需突出使用场景>
version: <u32，从 1 起递增，缺省视为 1>
self_updatable: <bool，默认 true，缺省视为 true>
---
```

- [parse_skill_md](../../src/infrastructure/skills/loader.rs#L97-L123) 解析时按行前缀匹配新增字段，缺省值在解析层兜底
- `version` 和 `self_updatable` 均为可选字段，向后兼容

#### 1.3 Skill Package 目录结构

```text
<agent>/skills/<skill_name>/
├── SKILL.md              # 当前版本
└── history/
    ├── v1.md             # 历史版本
    ├── v2.md
    └── v3.md             # 最多保留 3 代
```

- skill-updater 写入新版本前，扫描 `history/` 目录，清理超过 3 代的旧文件
- 写入前先把当前 `SKILL.md` 复制到 `history/v{base_version}.md`

#### 1.4 `Task` 字段扩展 — 拆为独立 Component

评审 D8 指出 `Task` 当前已有 21 个字段（[task.rs:71-100](../../src/domain/task.rs#L71-L100)），作为 Bevy Component 已偏大。新增字段不再塞进 `Task`，改为独立 Component：

```rust
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
```

- 移除 `ExperienceKindFilter::None` 变体（评审 D1），`Option::None` 等价于 `TaskExperiencePolicy` 缺失，等价于 `All`
- `TaskExperiencePolicy` 默认 `All`，仅 skill-updater 等特殊 Agent 的 task 显式标 `KnowledgeOnly`
- `TaskInjectedSkill` 序列化到磁盘（与 Task 同生命周期，作为独立 Component 持久化）
- `TaskExperiencePolicy` 不持久化（运行期临时标记，重启后从 Agent 标识推导）

### 2. Brain 派发改造

#### 2.1 brain 选 Agent+skill 的契约

改造 [brain_dispatch.rs:237-294](../../src/systems/dispatch/brain_dispatch.rs#L237-L294)：

1. brain 的输入：候选 Agent 列表，每个 Agent 暴露其名下所有 skill 的 `name + description + owner_agent_name`（不含 `instructions`）
2. brain 的 LLM 输出：JSON `{agent_name, skill_name?}`，`skill_name` 为 `null` 表示不注入
3. 框架校验：`skill_name` 必须属于该 agent 名下的 skill 集合，否则视为失败

#### 2.2 LLM 输出容错策略

评审 D5 指出 LLM 输出可能不规范。解析层需处理以下情况：

- **非标准 JSON**：解析失败计入重试次数
- **`skill_name` 为字符串 `"None"` / `""`**：等价于 `null`，不注入 skill
- **额外字段**：忽略，不视为错误
- **`agent_name` 不存在或不在候选列表**：计入重试次数

解析策略在 `parse_brain_skill_selection` 函数实现，单元测试覆盖所有边界情况。

#### 2.3 重试策略

新增配置项（`HarnessConfig`）：

```toml
[brain.skill_selection]
max_retries = 3
fallback_on_fail = "no_skill"   # 可选值：no_skill / fail_task
```

- `max_retries`：brain 选错 skill 的重试上限，默认 3
- `fallback_on_fail`：达到重试上限后的策略
  - `no_skill`：放弃注入 skill，仍派发 Agent（与 Q6 决策一致）
  - `fail_task`：任务失败

**token 成本评估**（评审 D5）：最坏情况下同一决策点调用 LLM `max_retries + 1` 次。结合 brain 本身 dispatch 的一次 LLM 调用，单次子任务派发最坏 LLM 调用次数 = `max_retries + 2`。这是可接受的成本，因为子任务派发不是高频路径。

#### 2.4 `select_agent_for_sub_task` 函数签名变更

评审 D5 指出当前签名（[agent_selection.rs:96-162](../../src/systems/dispatch/agent_selection.rs#L96-L162)）只返回 Agent，改造后需要同时返回 skill 选择结果。新签名：

```rust
pub fn select_agent_for_sub_task<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
    skill_registry: &SkillRegistry,    // 新增
) -> Option<(&'a Agent, Option<&'a LongTermMemory>, Option<SkillId>)>
```

- 返回值第三项 `Option<SkillId>` 为 LLM 推理结果
- 调用方负责校验 `SkillId` 属于所选 Agent，校验失败触发重试

#### 2.5 子任务派发支持父Agent指定 Agent

复用 [task_dispatch.rs:146-161](../../src/systems/dispatch/task_dispatch.rs#L146-L161) 的 `task.delegate` 机制，扩展到子任务派发：

- 修改 [brain_dispatch.rs:237-294](../../src/systems/dispatch/brain_dispatch.rs#L237-L294) 读取 `SubTaskConfig.child_agent_name`
- 若父Agent在子任务 `content` 中显式说明"由 AgentX 执行"，brain 的 LLM 推理 prompt 中包含该 hint（软约束）
- `select_agent_for_sub_task` 内部 LLM 调用的 system prompt 中包含该 hint

### 3. 经验治理改造

#### 3.1 持久Agent吸收子经验

改造 [collection.rs:165-215](../../src/systems/experience/collection.rs#L165-L215) 的 `experience_collection_completion_system`。当前函数签名不包含 `agents: Query<&Agent>`，需要扩展。

**联合查询模式**（评审 F14 修正）：使用 `Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>` 联合查询，避免占位符。

**SystemParam 封装**（评审 D10 建议）：参数膨胀到 7 个时，可考虑将 `agents`、`injected_skills`、`task_experience_policies` 封装为 `#[derive(SystemParam)]` 的 `TaskExperienceQuery`，降低签名复杂度。不阻塞当前设计，作为实施阶段优化。

```rust
pub(crate) fn experience_collection_completion_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    // 联合查询 Task 及其可能挂载的 skill/policy Component
    tasks: Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>,
    messages: Query<(Entity, &ExperienceCollectionCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        // 从 ExperienceStore 获取候选 ID（评审 F12 修正：msg 不含 candidate_ids）
        let candidate_ids: Vec<Uuid> = if let Some(parent_task_id) = msg.parent_task_id {
            store.aggregate_inbox_for_task(parent_task_id)
        } else {
            store.collect_top_level_governance_candidates(msg.task_id)
        };

        // 联合查询直接拿到 Task + 可选的 injected_skill / policy
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
            // 持久Agent吸收：不进父 inbox
            route_persistent_agent_experience(
                &mut commands, &mut store, msg, task, injected_skill,
                policy, &candidate_ids
            );
        } else {
            // 临时Agent → 原逻辑：queue_for_parent（原逻辑不变，使用 candidate_ids）
            // ... 原有 queue_for_parent 逻辑不变
        }
        commands.entity(entity).despawn();
    }
}
```

**`governing_agent_id` 取值明确**（评审 D2）：持久Agent吸收路径下，`governing_agent_id` 始终为 `task.delegate`（即持久Agent自身）。无论是 knowledge 类写 LTM 还是 skill 类走 skill-updater，写回路径都以持久Agent自己为归属。

#### 3.2 持久Agent吸收的分流路径

候选 ID 由调用方从 `ExperienceStore` 获取后传入（评审 F12 修正：不依赖 `msg.candidate_ids`）：

```rust
fn route_persistent_agent_experience(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    msg: &ExperienceCollectionCompletedMessage,
    task: &Task,
    injected_skill: Option<SkillId>,
    policy: Option<ExperienceKindFilter>,
    candidate_ids: &[Uuid],
) {
    // 先应用 kind_filter（评审 D7：filter 在收集完成后检查）
    let filtered_ids: Vec<Uuid> = candidate_ids.iter()
        .filter(|cid| {
            let candidate = &store.candidates[cid];
            let allowed = match policy {
                Some(ExperienceKindFilter::KnowledgeOnly) => candidate.kind_hint == ExperienceKindHint::Knowledge,
                Some(ExperienceKindFilter::SkillOnly) => candidate.kind_hint == ExperienceKindHint::Skill,
                Some(ExperienceKindFilter::All) | None => true,
            };
            if !allowed {
                store.candidates.get_mut(*cid).unwrap().status = ExperienceCandidateStatus::Discarded;
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
                    // skill 类候选 → 创建 SkillUpdate workitem（详见 3.3）
                    spawn_skill_update_workitem(commands, *candidate_id, skill_id, msg.governing_agent_id);
                }
                ExperienceKindHint::Knowledge => {
                    // knowledge 类候选 → 直接写持久Agent自己 LTM（无需用户确认）
                    writeback_to_long_term_memory_for_persistent_agent(
                        store, *candidate_id, msg.governing_agent_id
                    );
                }
            }
        }
    } else {
        // 持久Agent + 未注入 skill → 经 governance 走原写回路径（评审 D12：不绕过用户确认）
        // 不在此处直接写回，而是标记候选为 GovernancePending，
        // 由 experience_governance_system 按 destination 分流（含用户确认环节）
        for candidate_id in &filtered_ids {
            store.candidates.get_mut(*candidate_id).unwrap().status = ExperienceCandidateStatus::GovernancePending;
        }
        // spawn ExperienceGovernanceRequestMessage 触发 governance
        // （评审 F15：该消息只携带 task_id 和 agent_id，候选已置为 GovernancePending，
        // governance_candidates_for_task 会自动发现，无需 candidate_ids 字段）
        commands.spawn(ExperienceGovernanceRequestMessage {
            task_id: msg.task_id,
            agent_id: msg.governing_agent_id,
        });
    }
}
```

**用户确认策略明确**（评审 D12）：

- **持久Agent + 注入skill + skill 经验**：不经过 governance 的用户确认环节，直接走 skill-updater workitem。理由：skill-updater 本身是经验驱动的自我迭代，skill 更新属于框架内部治理，且 skill 有版本快照支持回退
- **持久Agent + 注入skill + knowledge 经验**：直接写 LTM，无需用户确认。理由：knowledge 写入持久Agent自己的 LTM，影响范围局部
- **持久Agent + 未注入skill**：**仍经 governance 走用户确认**。理由：这条路径会形成新 skill（`writeback_to_skill_package`）或新 Agent profile（孵化路径），属于跨 Agent 影响的操作，维持现有策略不变

这条策略变更需要在 ADR 中显式记录：从 v2 的"持久Agent吸收路径全部绕过 governance"改为 v3 的"仅注入skill 的吸收路径绕过 governance，未注入skill 仍走 governance"。

#### 3.3 SkillUpdate WorkItem 建模（评审 F1 修正）

`WorkItem` 是 struct，类型通过 `work_type: WorkItemType` 字段区分（[work_item.rs:126-150](../../src/domain/work_item.rs#L126-L150)）。正确做法：

1. 在 `WorkItemType` 中新增 `SkillUpdate` 变体：

```rust
pub enum WorkItemType {
    Execution,
    Summarization,
    Evaluation,
    ExperienceCollection,
    ProfileGeneration,
    SkillUpdate,    // 新增
}
```

2. `skill_id`、`base_version`、`experience_candidate_id` 等字段通过独立 Component 附加：

```rust
#[derive(Component, Debug, Clone)]
pub struct SkillUpdateContext {
    pub skill_id: SkillId,
    pub base_version: u32,
    pub experience_candidate_id: Uuid,
    pub governing_agent_id: AgentId,
}
```

3. `WorkItem::skill_update` 构造函数（类似 [profile_generation](../../src/domain/work_item.rs#L264-L289)）：

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
        // ... 构造逻辑类似 profile_generation
    }
}
```

`SkillUpdateContext` 在 workitem spawn 时同时挂载到同一 entity。

#### 3.4 skill-updater Agent 配置

新增持久Agent `skill-updater`，在 `agents.toml` 中预配置：

```toml
[[agents]]
name = "skill-updater"
tags = ["skill-updater", "persistent"]
description = "负责根据经验候选更新已有 skill 的 instruction"
model = "..."
system_prompt = """
你是一个 skill 更新专家。根据经验候选和原 skill 内容，
通过 submit_skill_update 工具提交结构化更新操作。
"""
tools = ["submit_skill_update"]
```

**brain 不可见性**：`skill-updater` 的 task 由经验治理系统直接 spawn，不经过 brain 选 Agent 路径。`skill-updater` 的 tags 含 `skill-updater`，使其不进入 `select_agent_for_sub_task` 候选（因为 [agent_selection.rs:100-103](../../src/systems/dispatch/agent_selection.rs#L100-L103) 过滤 `Persistent` 且 tags 不含 `"brain"`，需要同时扩展过滤 `skill-updater` 等"内部角色"tag）。

**引导方案**（评审 D3）：

- skill-updater 自身的初始 skill 内容由人工编写一份 SKILL.md，预置在 `.harness/assets/agents/skill-updater/skills/skill-update/SKILL.md`
- 该 skill 的 frontmatter 标 `self_updatable: false`，避免 skill-updater 自己更新自己
- skill-updater 的 knowledge 类经验仍写入自己的 LTM，由自己的 LTM 维护
- skill-updater 自身的 profile（name/tags/description）在 `agents.toml` 中声明，不通过 profile-designer 生成

#### 3.5 skill-updater 的输入输出契约

**输入**（workitem payload + SkillUpdateContext）：

- 原 skill 的完整 instruction（从 SkillRegistry 取）
- 原 skill 的 version（从 SkillRegistry 取）
- 触发更新的那条 skill 经验原文（从 ExperienceStore 取）

**输出**（通过新工具 `submit_skill_update`）：

```json
{
  "skill_id": "<owner_agent_name>/<skill_name>",
  "base_version": 3,
  "new_version": 4,
  "operations": [
    {"action": "replace_section", "section": "## Usage", "content": "..."},
    {"action": "add_section", "after": "## Usage", "section": "## Edge Cases", "content": "..."},
    {"action": "remove_section", "section": "## Legacy"},
    {"action": "replace_frontmatter", "field": "description", "value": "..."}
  ],
  "rationale": "为什么这么改"
}
```

#### 3.6 结构化 diff 操作的 markdown 解析策略（评审 D4）

**章节定义**：markdown 章节由 `## `（二级标题）开始，到下一个 `## ` 或文件末尾结束。`### ` 及更深层级属于父章节内容的一部分。

**操作语义**：

- `replace_section(section, content)`：替换从 `## {section}` 到下一个 `## ` 之间的所有内容（含子章节）
- `add_section(after, section, content)`：在 `## {after}` 章节完整内容之后（即下一个 `## ` 之前）插入新章节
- `remove_section(section)`：删除从 `## {section}` 到下一个 `## ` 之间的所有内容
- `replace_frontmatter(field, value)`：修改 frontmatter 中指定字段的值

**已知局限**（不阻塞实施，但需在测试中覆盖）：

1. **标题重名**：同层级同名章节，匹配第一个。apply 函数记录 warning 日志
2. **frontmatter 字段白名单**：仅允许修改 `name`、`description`、`self_updatable` 三个字段（`version` 由框架自动递增，不允许 LLM 直接改）
3. **解析策略**：基于行级正则匹配 `^## ` 和 `^[a-z_]+:`，不引入完整 markdown 解析器（依赖原则：优先纯 Rust，避免新依赖）

#### 3.7 循环防护（评审 D7 修正）

**修正**：`experience_kind_filter` 检查点从 `task_terminated_experience_trigger_system`（[collection.rs:12](../../src/systems/experience/collection.rs#L12)）移到 `experience_collection_completion_system`（[collection.rs:165-215](../../src/systems/experience/collection.rs#L165-L215)），具体实现在 §3.2 的 `route_persistent_agent_experience` 函数内。

**理由**（评审 D7）：`task_terminated_experience_trigger_system` 只 spawn `ExperienceCollectionRequestMessage`，根本不接触候选，kind 此时未知（由 LLM 在 `submit_experience_candidate` 调用时才确定）。filter 必须在收集完成后、进入汇聚/治理前检查。

**实现**（评审 F12 修正：候选 ID 从 `ExperienceStore` 获取，不依赖 `msg.candidate_ids`）：已在 §3.2 的 `route_persistent_agent_experience` 函数中展示。核心逻辑：

```rust
let filtered_ids: Vec<Uuid> = candidate_ids.iter()
    .filter(|cid| {
        let candidate = &store.candidates[cid];
        let allowed = match policy {
            Some(ExperienceKindFilter::KnowledgeOnly) => candidate.kind_hint == ExperienceKindHint::Knowledge,
            Some(ExperienceKindFilter::SkillOnly) => candidate.kind_hint == ExperienceKindHint::Skill,
            Some(ExperienceKindFilter::All) | None => true,
        };
        if !allowed {
            store.candidates.get_mut(*cid).unwrap().status = ExperienceCandidateStatus::Discarded;
        }
        allowed
    })
    .copied()
    .collect();
```

**新增候选状态**（`ExperienceCandidateStatus`）：

```rust
pub enum ExperienceCandidateStatus {
    // ... existing
    Discarded,    // 新增：被 kind_filter 过滤
}
```

#### 3.8 `self_updatable` 检查

在 [governance.rs:64-103](../../src/systems/experience/governance.rs#L64-L103) 的治理决策中，针对 skill 类候选增加检查。

**评审 F13 修正**：`injected_skill` 已拆为独立 Component `TaskInjectedSkill`，治理系统应通过 `Query<&TaskInjectedSkill>` 查询，不能用 `task.injected_skill`。`experience_governance_system` 的函数签名需要扩展：

```rust
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    skill_registry: Res<SkillRegistry>,                          // 新增
    tasks: Query<(&Task, Option<&TaskInjectedSkill>)>,           // 新增
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    // ...
    ExperienceKindHint::Skill => {
        // 通过联合查询获取 injected_skill（而非 task.injected_skill）
        let injected_skill = tasks.iter()
            .find(|(t, _)| t.id == request.task_id)
            .and_then(|(_, is)| is)
            .and_then(|is| is.skill_id.clone());

        if let Some(skill_id) = injected_skill {
            let skill_entry = skill_registry.get(&skill_id);
            if skill_entry.self_updatable {
                // 正常路径：触发 skill-updater
                destination = SkillUpdate;
            } else {
                // skill-updater 自身的 skill，降级为 knowledge 写入 LTM
                destination = LongTermMemory;
                candidate.kind_hint = ExperienceKindHint::Knowledge;  // 强制降级
            }
        } else {
            // 持久Agent未注入 skill：形成新 skill
            destination = SkillPackage;
        }
    }
}
```

**注意**：注入skill 路径的候选在 §3.2 已被 `route_persistent_agent_experience` 直接走 skill-updater，不会进入 governance。因此 §3.8 的 `SkillUpdate` destination 分支实际上只处理"候选已通过 governance 路径流入但发现 `injected_skill` 存在"的边界情况——但按 §3.2 的设计，这种情况不会发生。为防御性编程，governance 仍检查 `injected_skill`，若存在则 redirect 到 skill-updater 路径。

### 4. skill_update_completion_system 职责（评审 D6 补全、D11 补充）

新增系统 `skill_update_completion_system`，完整职责：

1. 接收 `SkillUpdateCompletedMessage`
2. 从 `SkillUpdateContext` 读取 `experience_candidate_id`
3. apply skill operations 到 SKILL.md（调用 `apply_skill_operations`）
4. apply 成功：
   - 把当前 SKILL.md 复制到 `history/v{base_version}.md`
   - 调用 `cleanup_skill_history` 保留 3 代
   - 刷新 SkillRegistry 中对应 `SkillEntry`（`version` 递增，`instructions` 更新）
   - **将候选状态置为 `Persisted`**（触发 [profile_update.rs:23-111](../../src/systems/experience/profile_update.rs#L23-L111) 的 profile-designer 评估）
5. apply 失败：
   - 候选状态保持原状（不置 `Persisted`）
   - 记录 error 日志
   - 触发重试或失败处理（详见 4.1）

**`SkillUpdateCompletedMessage` 的 spawn 时机**（评审 D11 补充）：

复用现有 WorkItem 完成流程，不单独 spawn。具体路径：

1. skill-updater Agent 调用 `submit_skill_update` 工具
2. orchestrator.rs 处理 `ToolAction::SubmitSkillUpdate`，spawn `SkillUpdateCompletedMessage`（类似 `ProfileGenerationCompletedMessage` 在 [orchestrator.rs:908-917](../../src/systems/tools/orchestrator.rs#L908-L917) 的 spawn 模式）
3. `skill_update_completion_system` 接收 `SkillUpdateCompletedMessage` 执行上述 5 步职责

**不通过** `WorkItemCompletedMessage` 触发，因为 `WorkItemCompletedMessage` 是通用的，不携带 skill 更新所需的具体 payload（operations、rationale 等）。`SkillUpdateCompletedMessage` 需要携带：

```rust
#[derive(Debug, Clone, Event)]
pub struct SkillUpdateCompletedMessage {
    pub work_item_id: Uuid,
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub skill_id: SkillId,
    pub base_version: u32,
    pub new_version: u32,
    pub operations: Vec<SkillUpdateOperation>,
    pub rationale: String,
}
```

`SkillUpdateOperation` 为枚举，对应 §3.5 的四种操作。

#### 4.1 apply 失败处理

- 章节不存在：跳过该 operation，记录 warning，继续后续 operations
- frontmatter 字段不在白名单：跳过该 operation，记录 warning
- 文件 IO 错误：整个 apply 失败回滚，候选状态不变

#### 4.2 与 profile-designer 的链路闭合

skill-updater 写完 SKILL.md 后，候选状态置 `Persisted`，[profile_update_trigger_system](../../src/systems/experience/profile_update.rs#L23-L111) 检测到 `Persisted` 后会 spawn `ProfileGenerationRequestMessage { kind: Update }`，profile-designer 评估 agent profile 是否需要更新。两者不冲突。

### 5. profile-designer 与 skill-updater 的边界

| 场景 | 路径 | 涉及组件 |
|---|---|---|
| default Agent 产生 Skill/Knowledge 经验 | IncubationProposal → profile-designer → 审批 → writeback_incubation_proposal | profile-designer + writeback |
| 持久Agent + 注入skill 产生 skill 经验 | **新路径**：collection 拦截 → skill-updater workitem → submit_skill_update → 写新版本 SKILL.md → profile-designer 评估 profile | skill-updater + writeback + profile-designer |
| 持久Agent + 注入skill 产生 knowledge 经验 | LongTermMemory 路径，写持久Agent自己 LTM → profile-designer 评估 profile | writeback + profile-designer |
| 持久Agent + 未注入skill 产生 skill 经验 | 现有 SkillPackage 路径，形成新 skill → profile-designer 评估 profile | writeback_to_skill_package + profile-designer |
| 持久Agent + 未注入skill 产生 knowledge 经验 | LongTermMemory 路径 → profile-designer 评估 profile | writeback + profile-designer |
| 临时Agent 产生经验 | 现有路径：进入父任务 inbox | queue_for_parent |

**职责切分**：

- profile-designer：管孵化期 profile 生成和持久Agent profile 评估更新（不写 SKILL.md）
- skill-updater：管成熟期 skill 迭代（只更新已有 skill，不形成新 skill）
- 两者写入路径不冲突：skill-updater 写 `<skill>/SKILL.md`，profile-designer 写 `agents.toml`

**触发 profile-designer 的条件不变**：[profile_update.rs:23-111](../../src/systems/experience/profile_update.rs#L23-L111) 检测 `Persisted` 状态。skill-updater 写完 SKILL.md 后，candidate 状态也置 `Persisted`，profile-designer 仍会被触发评估。

### 6. 执行 Agent 能看到 skill 元信息

改造 `submit_experience_candidate` 工具的 prompt（[collection.rs:75-83](../../src/systems/experience/collection.rs#L75-L83)），显式告诉 LLM：

- 当前 task 注入的 skill 是什么（name + description + instructions）
- 如果经验用于改进当前 skill，请使用 `kind=skill`
- 如果经验是事实性知识，请使用 `kind=knowledge`

### 7. skill 删除/退役机制（评审 D9 — 显式推迟）

本 ADR **不引入** skill 删除/退役机制。理由：

- skill 保留成本极低（单文件 + 3 代历史快照）
- 删除策略需要更多运行期数据（如"长期未被 brain 选择"的统计），不在本次改造范围
- 作为已知约束记录，后续单独立项

## 后果

### 正面

- skill 成为可被 brain 显式选择的一等公民，brain 的 Agent+skill 联合选择能力落地
- 持久Agent吸收子经验，避免子经验无序透传到顶层，解决顶层经验膨胀问题
- skill-updater 让 skill 具备经验驱动的自我迭代能力，形成经验治理闭环
- skill 版本管理 + 历史快照，支持回退和审计
- 结构化 diff 操作，避免 LLM 全量重写的 token 成本和不可控性
- Task 字段扩展拆为独立 Component，遵循 ECS 组合原则

### 负面

- 数据结构变更：`WorkItemType` 新增 `SkillUpdate` 变体、新增 `SkillUpdateContext` Component、SKILL.md frontmatter 新增 2 字段
- 新增 `SkillRegistry` Resource，启动期扫描成本略增
- 新增 `skill-updater` Agent，配置和 prompt 维护成本
- brain 派发路径改造，引入 LLM 推理失败重试机制
- 经验治理拦截逻辑复杂化，需要严格的测试覆盖
- `experience_collection_completion_system` 函数签名扩展，新增 `agents`、`injected_skills`、`task_experience_policies` 三个 Query 参数

## 数据结构变更清单

### 新增

- `SkillId`（newtype，封装 `owner_agent_name + skill_name`）
- `SkillRegistry`（Resource）
- `SkillEntry`
- `WorkItemType::SkillUpdate` 变体
- `SkillUpdateContext`（Component）
- `TaskInjectedSkill`（Component）
- `TaskExperiencePolicy`（Component）
- `ExperienceKindFilter` 枚举（无 `None` 变体，`All` 为默认）
- `ExperienceCandidateStatus::Discarded` 变体
- `submit_skill_update` 工具
- `skill-updater` Agent 配置
- `SkillUpdateCompletedMessage`

### 修改

- `LoadedSkill`：新增 `version: u32`、`self_updatable: bool`
- `SkillLoader`：解析 frontmatter 新字段，构造 `SkillRegistry`
- SKILL.md frontmatter：新增 `version`、`self_updatable`
- `SubTaskConfig`：`child_agent_name` 字段在 brain_dispatch 中被读取
- `HarnessConfig`：新增 `[brain.skill_selection]` 配置段
- `select_agent_for_sub_task`：函数签名扩展，新增 `skill_registry` 参数，返回值新增 `Option<SkillId>`
- `experience_collection_completion_system`：新增 `agents` Query 参数，`tasks` 改为联合查询 `Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>`（评审 F14 + D10）
- `experience_governance_system`：新增 `skill_registry: Res<SkillRegistry>` 和 `tasks: Query<(&Task, Option<&TaskInjectedSkill>)>` 参数（评审 F13）

## system 改动清单

### 改造

- [brain_dispatch.rs:237-294](../../src/systems/dispatch/brain_dispatch.rs#L237-L294)：LLM 选 Agent+skill，写入 `TaskInjectedSkill`
- [collection.rs:12-52](../../src/systems/experience/collection.rs#L12-L52)：无改动（filter 检查点不在此处，评审 D7 修正）
- [collection.rs:165-215](../../src/systems/experience/collection.rs#L165-L215)：持久Agent吸收分支 + `experience_kind_filter` 检查
- [collection.rs:75-83](../../src/systems/experience/collection.rs#L75-L83)：prompt 暴露 skill 元信息
- [loader.rs:97-123](../../src/infrastructure/skills/loader.rs#L97-L123)：解析新 frontmatter 字段
- [task_dispatch.rs:215-223](../../src/systems/dispatch/task_dispatch.rs#L215-L223)：从 SkillRegistry 取 skill 注入 prompt
- [governance.rs:64-103](../../src/systems/experience/governance.rs#L64-L103)：检查 `self_updatable`，false 则降级 knowledge
- [agent_selection.rs:100-103](../../src/systems/dispatch/agent_selection.rs#L100-L103)：扩展过滤 `skill-updater` 等"内部角色"tag

### 新增

- `skill_update_workitem_system`（类似 `profile_generation_workitem_system`）
- `skill_update_completion_system`（apply operations、刷新 Registry、置候选状态为 `Persisted`）
- `submit_skill_update` 工具的执行处理（在 orchestrator.rs）
- `apply_skill_operations` 函数（apply diff 到 SKILL.md）
- `refresh_skill_registry` 函数（skill-updater 写入后刷新 Registry）
- `cleanup_skill_history` 函数（保留 3 代）
- `parse_brain_skill_selection` 函数（解析 brain LLM 输出，含容错）

## 测试用例清单

### 单元测试

1. `SkillRegistry` 启动期加载：扫描多个 Agent 目录，构造完整 Registry
2. `SkillRegistry` 运行期刷新：skill-updater 写入后，Registry 中对应 entry 更新
3. SKILL.md frontmatter 解析：`version`、`self_updatable` 字段正确解析，缺省值正确（version=1、self_updatable=true）
4. `apply_skill_operations` 各 action 正确执行：
   - `replace_section`：章节存在 / 不存在两种情况
   - `add_section`：`after` 章节存在 / 不存在
   - `remove_section`：章节存在 / 不存在
   - `replace_frontmatter`：字段在白名单 / 不在白名单
5. `apply_skill_operations` 章节匹配：同层级同名章节匹配第一个，记录 warning
6. `cleanup_skill_history`：保留 3 代，超过的删除
7. `experience_kind_filter` 过滤：`KnowledgeOnly` 下 skill 候选被标记 `Discarded`
8. `self_updatable=false` 的 skill 候选被降级为 knowledge
9. `parse_brain_skill_selection` 容错：标准 JSON、`skill_name: "None"`、`skill_name: ""`、额外字段、非标准 JSON
10. `ExperienceKindFilter` 默认值为 `All`

### 集成测试

1. **brain 选 skill 成功路径**：task 适合某 skill，brain 选 Agent+skill，skill 注入 prompt，任务执行
2. **brain 选 skill 失败重试**：brain 选错 skill 名字，重试，达到上限 fallback
3. **持久Agent + 注入skill + skill 经验**：完整路径——collection 拦截 → skill-updater workitem → submit_skill_update → SKILL.md 更新 → 候选置 `Persisted` → profile-designer 评估
4. **持久Agent + 注入skill + knowledge 经验**：knowledge 写入持久Agent LTM，不进父 inbox
5. **持久Agent + 未注入skill + skill 经验**：走原 writeback_to_skill_package 路径
6. **临时Agent + 经验**：走原 queue_for_parent 路径
7. **skill-updater 自指循环防护**：skill-updater 产生 skill 候选，`self_updatable=false` 降级为 knowledge
8. **skill-updater kind filter 防护**：skill-updater 的 task 标 `KnowledgeOnly`，skill 候选被标记 `Discarded`
9. **skill 版本递增**：连续两次 skill 更新，version 正确递增，history 保留 3 代
10. **skill 回退保护**：apply 失败，SKILL.md 不变，history 不写入，候选状态不变
11. **持久Agent + 注入skill 路径不进父 inbox**：验证父 Agent 的 `ExperienceInbox` 中无对应候选

## 开放问题（留给实施阶段细化）

1. `SkillRegistry` 运行期更新的同步机制：skill-updater 写入后是立即同步还是通过事件异步刷新
2. `skill-updater` workitem 的 governing_agent_id：建议为触发它的持久Agent
3. brain LLM 选 skill 的 prompt 模板：需要单独设计，不在本 ADR 范围内
4. skill-updater 自身 skill 的初始内容：需要在 `agents.toml` 中预配置
5. skill 删除/退役机制：显式推迟，作为已知约束（§7）

## 评审修正记录

本 v3 版本基于两轮评审报告修正以下问题：

### 第一轮（`logs/2026-07-18-adr-004-skill-first-class-review.md`）

| 评审编号 | 修正内容 |
|---|---|
| F1 | `WorkItem::SkillUpdate` enum 变体改为 `WorkItemType::SkillUpdate` + 独立 `SkillUpdateContext` Component |
| F2 | 伪代码改为 ECS 迭代器模式 `agents.iter().find(...)` |
| D1 | 移除 `ExperienceKindFilter::None` 变体，`Option::None` 等价于 `All` |
| D2 | 明确持久Agent吸收路径下 `governing_agent_id` 始终为 `task.delegate` |
| D3 | 补充 skill-updater 引导方案：初始 skill 内容、`self_updatable: false`、brain 不可见性 tag |
| D4 | 明确章节定义、操作语义、已知局限、frontmatter 字段白名单 |
| D5 | 补充 LLM 输出容错策略、token 成本评估、`select_agent_for_sub_task` 签名变更 |
| D6 | 补全 `skill_update_completion_system` 完整职责，包括 apply 成功后置候选状态为 `Persisted` |
| D7 | `experience_kind_filter` 检查点从 `task_terminated_experience_trigger_system` 移到 `experience_collection_completion_system` |
| D8 | Task 新增字段拆为独立 Component `TaskInjectedSkill` 和 `TaskExperiencePolicy` |
| D9 | 显式推迟 skill 删除/退役机制，作为已知约束记录（§7） |

### 第二轮（`logs/2026-07-18-adr-004-skill-first-class-review-v2.md`）

| 评审编号 | 修正内容 |
|---|---|
| F12 | §3.2 和 §3.7 伪代码移除不存在的 `msg.candidate_ids`，改为从 `ExperienceStore` 获取候选（`aggregate_inbox_for_task` / `collect_top_level_governance_candidates`） |
| F13 | §3.8 治理伪代码从 `task.injected_skill` 改为通过 `Query<(&Task, Option<&TaskInjectedSkill>)>` 联合查询 |
| F14 | §3.1 伪代码移除占位符 `/* match task entity */`，改为联合查询 `Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>` |
| D10 | 补充 SystemParam 封装建议（`TaskExperienceQuery`），作为实施阶段优化 |
| D11 | 补充 `SkillUpdateCompletedMessage` 的 spawn 时机（由 orchestrator.rs 处理 `ToolAction::SubmitSkillUpdate` 时 spawn）和完整字段定义 |
| D12 | 明确用户确认策略：注入skill 路径绕过 governance（skill/knowledge 均无需确认），未注入skill 路径仍经 governance 走用户确认 |
| F15 | §3.2 spawn `ExperienceGovernanceRequestMessage` 时移除多余的 `candidate_ids` 字段（该 struct 只有 `task_id` 和 `agent_id`，候选已置为 `GovernancePending`，由 `governance_candidates_for_task` 自动发现） |

## 关联文件

- [src/domain/task.rs](../../src/domain/task.rs) — Task 字段拆为独立 Component
- [src/domain/work_item.rs](../../src/domain/work_item.rs) — `WorkItemType::SkillUpdate` 变体
- [src/domain/contribution.rs](../../src/domain/contribution.rs) — `ExperienceKindFilter`、`ExperienceCandidateStatus::Discarded`
- [src/infrastructure/skills/loader.rs](../../src/infrastructure/skills/loader.rs) — SkillRegistry + SkillEntry + frontmatter 新字段解析
- [src/systems/dispatch/brain_dispatch.rs](../../src/systems/dispatch/brain_dispatch.rs) — brain 选 skill
- [src/systems/dispatch/agent_selection.rs](../../src/systems/dispatch/agent_selection.rs) — `select_agent_for_sub_task` 签名扩展
- [src/systems/experience/collection.rs](../../src/systems/experience/collection.rs) — 持久Agent吸收分支 + kind_filter 检查
- [src/systems/experience/governance.rs](../../src/systems/experience/governance.rs) — self_updatable 检查
- [src/systems/experience/skill_update.rs](../../src/systems/experience/skill_update.rs) — 新文件，skill-updater workitem 系统
- [src/systems/tools/builtin/submit_skill_update.rs](../../src/systems/tools/builtin/submit_skill_update.rs) — 新工具
- [src/infrastructure/skills/diff.rs](../../src/infrastructure/skills/diff.rs) — 新文件，apply_skill_operations
