# ADR-004: Skill 成为一等公民与经验治理改造

## 状态

Proposed（v8 — 已根据七轮评审报告修正：v1 `logs/2026-07-18-adr-004-skill-first-class-review.md`、
v2 `logs/2026-07-18-adr-004-skill-first-class-review-v2.md`、v3 用户反馈 F15、v4 实施偏差修正 D13/D14、
v6 实施 review 修正 D15、v7 Phase 4 skill update 实施修正 D16/D17/D18、
v8 两端对齐改造 D19：generation 端结构校验 + update 端三级标题级 operation + replace_body 兜底）

## 生效范围

本决策自 2026-07-17 起提出，关联设计文档：

- `docs/design/2026-07-13-agent-profile-llm-generation-design.md`（profile-designer 现有职责）
- `docs/design/2026-06-06-workitem-boundary-design.md`（WorkItem 边界）
- `docs/current-state.md`

## 背景

当前 skill 在代码库中不是一等公民：

- `LoadedSkill` 仅在 prompt 拼装时临时构造（[loader.rs:22-27](../../src/infrastructure/skills/loader.rs#L22-L27)），没有版本号、没有注册表
- skill 通过 `agent.profile.name` 字符串间接关联（[task_dispatch.rs:215](../../src/systems/dispatch/task_dispatch.rs#L215)），
  无法被 brain 显式选择
- brain 子任务派发路径（[brain_dispatch.rs:237-294](../../src/systems/dispatch/brain_dispatch.rs#L237-L294)）完全不加载 skill
- 经验汇聚当前为两级（子→父 inbox、父终态合并 root+inbox），所有候选都透传到父 Agent
- `writeback_to_skill_package` 直接落盘 SKILL.md，没有版本管理、没有 diff 机制

本决策需要在三个维度同时改造：

1. __Skill 数据模型升级__：让 skill 成为可被 brain 选择、带版本、可注册的一等公民
2. __Brain 派发改造__：brain 在派发子任务时，为 Agent 选择 0 或 1 个 skill 注入
3. __经验治理改造__：持久Agent吸收子经验（含 knowledge 和 skill 类），skill 类经验触发 skill-updater workitem，避免子经验层层透传到顶层

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

__generation 端写入字段对齐__（D19）：`persist_skill_package` 落盘时显式写入
`name + description + self_updatable: true` 三个字段（不写 `version`，由解析层缺省值为 1 兜底）。
原 v7 实现只写 `name + description`，依赖解析层缺省值兜底 `self_updatable=true`，
本次显式写入以提升透明度并与 update 端 frontmatter 白名单 `[name, description, self_updatable]`
对齐。`replace_frontmatter` 操作为 upsert 语义：字段存在则替换，不存在则追加。

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

评审 D8 指出 `Task` 当前已有 21 个字段（[task.rs:71-100](../../src/domain/task.rs#L71-L100)），作为 Bevy Component 已偏大。
新增字段不再塞进 `Task`，改为独立 Component：

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

- __非标准 JSON__：解析失败计入重试次数
- __`skill_name` 为字符串 `"None"` / `""`__：等价于 `null`，不注入 skill
- __额外字段__：忽略，不视为错误
- __`agent_name` 不存在或不在候选列表__：计入重试次数

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

__token 成本评估__（评审 D5）：最坏情况下同一决策点调用 LLM `max_retries + 1` 次。结合 brain 本身 dispatch 的一次 LLM 调用，
单次子任务派发最坏 LLM 调用次数 = `max_retries + 2`。这是可接受的成本，因为子任务派发不是高频路径。

__错误类型设计__（修订 v5）：`parse_brain_skill_selection` 必须使用 `thiserror` 定义的 typed error（`BrainSkillSelectionError`），
符合 AGENTS.md 库 crate 错误处理规范。变体划分：

- `InvalidJson`：LLM 输出非合法 JSON 或清洗后仍无法解析
- `AgentNotInCandidates`：`agent_name` 不在候选列表中
- `SkillNotOwned`：`skill_name` 不属于该 agent 名下 skill 集合

`String` 错误类型不允许在库 crate 中使用。

#### 2.4 `select_agent_for_sub_task` 函数签名变更

> __状态：已被取代__（2026-07-18）
>
> 本节原设想在 `select_agent_for_sub_task` 内部用 LLM 选 Agent + skill 并返回 `Option<SkillId>`。
> 该思路在派发架构统一（参见 `docs/design/2026-07-18-dispatch-architecture-unification-design.md`）
> 之后已被取代：Brain LLM 现在整体决策输出 JSON `{agent_name, skill_name?}`，
> 由 `build_brain_execution_request`（`src/systems/dispatch/brain_llm_builder.rs`）+
> `parse_brain_skill_selection`（`src/systems/dispatch/brain_dispatch.rs`）+
> `brain_decision_system`（`src/systems/transform/brain_decision.rs`）形成统一闭环。
> 候选 Agent 名下 skills 清单由 `SkillRegistry` 注入 Brain LLM prompt（按 agent 嵌套渲染）。
> 因此 `select_agent_for_sub_task_with_skill` 函数从未在源码中实现，原签名扩展也不再适用。
> 本节内容保留作为历史背景，不再作为实施依据。

评审 D5 指出当前签名（[agent_selection.rs:96-162](../../src/systems/dispatch/agent_selection.rs#L96-L162)）只返回 Agent，
改造后需要同时返回 skill 选择结果。新签名：

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

改造 [collection.rs:165-215](../../src/systems/experience/collection.rs#L165-L215) 的 `experience_collection_completion_system`。
当前函数签名不包含 `agents: Query<&Agent>`，需要扩展。

__联合查询模式__（评审 F14 修正）：使用 `Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>` 联合查询，
避免占位符。

__SystemParam 封装__（评审 D10 建议）：参数膨胀到 7 个时，可考虑将 `agents`、`injected_skills`、`task_experience_policies`
封装为 `#[derive(SystemParam)]` 的 `TaskExperienceQuery`，降低签名复杂度。不阻塞当前设计，作为实施阶段优化。

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

__`governing_agent_id` 取值明确__（评审 D2）：持久Agent吸收路径下，`governing_agent_id` 始终为 `task.delegate`（即持久Agent自身）。
无论是 knowledge 类写 LTM 还是 skill 类走 skill-updater，写回路径都以持久Agent自己为归属。

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

__用户确认策略明确__（评审 D12）：

- __持久Agent + 注入skill + skill 经验__：不经过 governance 的用户确认环节，直接走 skill-updater workitem。
  理由：skill-updater 本身是经验驱动的自我迭代，skill 更新属于框架内部治理，且 skill 有版本快照支持回退
- __持久Agent + 注入skill + knowledge 经验__：直接写 LTM，无需用户确认。
  理由：knowledge 写入持久Agent自己的 LTM，影响范围局部
- __持久Agent + 未注入skill__：__仍经 governance 走用户确认__。
  理由：这条路径会形成新 skill（`writeback_to_skill_package`）或新 Agent profile（孵化路径），
  属于跨 Agent 影响的操作，维持现有策略不变

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

__brain 不可见性__：`skill-updater` 的 task 由经验治理系统直接 spawn，不经过 brain 选 Agent 路径。
`skill-updater` 的 tags 含 `skill-updater`，使其不进入 `select_agent_for_sub_task` 候选
（因为 [agent_selection.rs:100-103](../../src/systems/dispatch/agent_selection.rs#L100-L103) 过滤 `Persistent` 且 tags 不含 `"brain"`，
需要同时扩展过滤 `skill-updater` 等"内部角色"tag）。

__引导方案__（评审 D3）：

- skill-updater 自身的初始 skill 内容由人工编写一份 SKILL.md，预置在 `.harness/assets/agents/skill-updater/skills/skill-update/SKILL.md`
- 该 skill 的 frontmatter 标 `self_updatable: false`，避免 skill-updater 自己更新自己
- skill-updater 的 knowledge 类经验仍写入自己的 LTM，由自己的 LTM 维护
- skill-updater 自身的 profile（name/tags/description）在 `agents.toml` 中声明，不通过 profile-designer 生成

#### 3.5 skill-updater 的输入输出契约

__输入__（workitem payload + SkillUpdateContext）：

- 原 skill 的完整 instruction（从 SkillRegistry 取）
- 原 skill 的 version（从 SkillRegistry 取）
- 触发更新的那条 skill 经验原文（从 ExperienceStore 取）

__输出__（通过新工具 `submit_skill_update`）：

LLM 仅提交 `operations` + `rationale`；`skill_id` / `base_version` / `new_version` 由 orchestrator
从 `SkillUpdateContext`（Component，挂在 WorkItem entity 上）服务端权威注入，避免 LLM 臆造 `skill_id`
或拼错 `base_version`（D16）。

```json
{
  "operations": [
    {"action": "replace_section", "section": "## Usage", "content": "..."},
    {"action": "add_section", "after": "## Usage", "section": "## Edge Cases", "content": "..."},
    {"action": "remove_section", "section": "## Legacy"},
    {"action": "replace_subsection", "section": "## Usage", "subsection": "### Advanced", "content": "..."},
    {"action": "add_subsection", "section": "## Usage", "after": "### Basic",
     "subsection": "### Edge Cases", "content": "..."},
    {"action": "remove_subsection", "section": "## Usage", "subsection": "### Legacy"},
    {"action": "replace_frontmatter", "field": "description", "value": "..."},
    {"action": "replace_body", "content": "..."}
  ],
  "rationale": "为什么这么改"
}
```

__operation 颗粒度__（D19）：v7 仅支持 4 种 operation（3 种二级标题级 + 1 种 frontmatter 字段级），
对含 `###` 子章节的 SKILL.md 颗粒度过大——LLM 修改子章节需重写整章，污染其他子章节风险高。
v8 新增 3 种三级标题级 operation（`replace_subsection` / `add_subsection` / `remove_subsection`）
和 1 种兜底 operation（`replace_body`），共 8 种。`replace_body` 在 prompt 中标注"慎用，
仅当原 body 无 `##` 标题或需整体重构时使用"，软约束 LLM 优先用细粒度 operation。

__候选 payload 传递__（D19）：`candidate_payload_text` 保留 v7 扁平化文本格式（自然语言形式
LLM 易读），但在 prompt 中显式说明候选类型（Skill / Knowledge），帮助 updater 选择策略
（Skill 类候选倾向 `add_section` / `replace_subsection`，Knowledge 类候选倾向 `replace_section` 整章）。

__prompt 内容__（D18）：skill-updater 的 prompt 现在包含完整 SKILL.md 内容（frontmatter + 所有 section
标题），让 LLM 看到真实结构而非幻觉 section 名。早期版本只把 `SkillEntry.instructions` 字段塞进 prompt，
但 `instructions` 只是 `SkillEntry` 的一个 String 字段，并不等同于磁盘上的 SKILL.md 真实结构。

#### 3.6 结构化 diff 操作的 markdown 解析策略（评审 D4，D19 扩展）

__章节定义__：markdown 章节由 `##`（二级标题）开始，到下一个 `##` 或文件末尾结束。
子章节由 `###`（三级标题）开始，到下一个 `###` 或 `##` 或 body 末尾结束。
`####` 及更深层级属于父子章节内容的一部分，不作为 operation 锚点。

__操作语义__（v8 共 8 种 operation）：

二级标题级（v7 已有）：

- `replace_section(section, content)`：替换从 `## {section}` 到下一个 `##` 之间的所有内容（含子章节，保留 `## {section}` 标题行）
- `add_section(after, section, content)`：在 `## {after}` 章节完整内容之后（即下一个 `##` 之前）插入新 `## {section}` 章节
- `remove_section(section)`：删除从 `## {section}` 到下一个 `##` 之间的所有内容（含标题行）
- `replace_frontmatter(field, value)`：修改 frontmatter 中指定字段的值，upsert 语义（字段存在则替换，不存在则追加）

三级标题级（v8 新增，D19）：

- `replace_subsection(section, subsection, content)`：在 `## {section}` 范围内定位 `### {subsection}`，
  替换其内容（保留 `### {subsection}` 标题行）。`section` 必须指定以消除跨 section 同名 subsection 歧义
- `add_subsection(section, after, subsection, content)`：在 `## {section}` 范围内 `### {after}` 之后
  插入新 `### {subsection}` 子章节
- `remove_subsection(section, subsection)`：删除 `## {section}` 下的 `### {subsection}` 子章节（含标题行）

兜底（v8 新增，D19）：

- `replace_body(content)`：整体替换 body，frontmatter 不变。prompt 软约束标注"慎用"

__find_subsection_range 语义__：

```rust
fn find_subsection_range(body: &str, section: &str, subsection: &str) -> Option<(usize, usize)>;
```

1. 先调用 `find_section_range(body, section)` 定位父 section 范围 `[section_start, section_end)`
2. 在 `[section_start+1, section_end)` 范围内查找 `trim() == subsection.trim()` 的行
3. subsection 结束行 = 下一个 `###` 或 `##` 或 `section_end`
4. 未找到返回 `None`，caller 转为 `ApplyError::SubsectionNotFound(section, subsection)`

__已知局限__（不阻塞实施，但需在测试中覆盖）：

1. __标题重名__：同层级同名章节/子章节，匹配第一个。`find_section_range` 与 `find_subsection_range`
   均需在匹配到第一个时记录 `tracing::warn!` 日志，包含 section/subsection 名与 body 行数
   （v8 D19 修复 ADR-004 v7 已知局限 1，原描述"apply 函数记录 warning 日志"未落地的偏差）
2. __frontmatter 字段白名单__：仅允许修改 `name`、`description`、`self_updatable` 三个字段
   （`version` 由框架自动递增，不允许 LLM 直接改）
3. __解析策略__：基于行级前缀匹配 `##` / `###` / `^[a-z_]+:`，不引入完整 markdown 解析器
   （依赖原则：优先纯 Rust，避免新依赖）
4. __标题规范化__（v8 D19 修复 ADR-004 v7 实现偏差 D）：`find_section_range` / `find_subsection_range`
   在比较标题行时使用 `l.trim() == header.trim()`，而非 v7 实现的 `l.trim_start() == header`，
   避免尾部空格导致 `"## Usage "` 匹配失败
5. __dry-run 同步校验__（D18 + D19 扩展）：orchestrator 在 `SubmitSkillUpdate` 分支提前做 dry-run apply，
   section/subsection 不存在或 frontmatter 字段不在白名单时立即以 `ToolError::InvalidInput` 同步返回给 LLM
   （而非异步抛到 `skill_update_completion_system`）。dry-run 通过后再 insert 完成消息。
   v8 D19 在 dry-run 后追加 `validate_skill_structure` post-apply 校验（详见 §3.7）

#### 3.7 generation 端结构校验与 post-apply 校验（D19 新增）

__问题背景__：v7 generation 端（`persist_skill_package`）直接落盘 LLM 提交的 `instructions`，
无任何结构校验；update 端的章节级 diff operation 假设 SKILL.md body 至少包含 1 个 `##` 标题。
两端粒度不对称导致 LLM 生成的 SKILL.md 可能无章节结构，update 端的 section operation 失去稳定锚点。

__`validate_skill_structure` 函数__（新增于 `src/infrastructure/skills/diff.rs`）：

```rust
pub fn validate_skill_structure(instructions: &str) -> Result<(), SkillStructureError>;

#[derive(Debug, Error)]
pub enum SkillStructureError {
    #[error("instructions must contain at least one `##` heading")]
    NoSectionHeading,
    #[error("first `##` heading must have non-empty content")]
    EmptyFirstSection,
}
```

__generation 端 dry-run 校验__（`persist_skill_package` 落盘前）：

1. 调用 `validate_skill_structure(&draft.instructions)`
2. 失败则返回 `SkillStructureError`，由 `writeback_to_skill_package` 传播
3. 候选状态置为 `WritebackFailed`（复用现有 `ExperienceCandidateStatus::WritebackFailed` 变体），记录 warn 日志
4. LLM 不会自动重试（generation 端无重试机制，失败后由用户决定是否手动重新触发）

__update 端 post-apply 校验__（orchestrator dry-run 之后追加）：

1. `apply_skill_operations(&content, &operations)` 成功后，对 apply 后的 body 调用
   `validate_skill_structure(&new_body)`
2. 失败则整体回滚（D13 语义），以 `ToolError::InvalidInput` 同步返回给 LLM
3. 防止 LLM 用 `replace_body` 或 `remove_section` 删除所有章节标题

__generation 端 prompt 约束__（D19）：

修改 `collection.rs` 的 prompt 模板，加入 SKILL.md 格式约束：

```text
- skill 的 instructions 字段必须是 markdown 格式，至少包含 1 个 `## Section` 二级标题
- 推荐使用 `## Overview` / `## Usage` / `## Examples` / `## Edge Cases` / `## Limitations` 等 section
- 复杂 skill 可在二级标题下使用 `### Subsection` 三级标题组织内容
- 不要使用 `####` 或更深层级，update 端不支持作为 operation 锚点
```

同时修改 `submit_experience_candidate` 工具描述，明确 `instructions` 字段格式要求。

__不强制 `## Overview`__（D19 决策修正）：原 grilling 阶段曾考虑强制 `## Overview` 存在，
经讨论认为该 section 对简单 skill 冗余（与 frontmatter `description` 重复），
改为推荐但非强制。dry-run 只校验"至少 1 个 `##` 标题 + 首个 section 非空"。

#### 3.8 循环防护（评审 D7 修正）

__修正__：`experience_kind_filter` 检查点从 `task_terminated_experience_trigger_system`（[collection.rs:12](../../src/systems/experience/collection.rs#L12)）
移到 `experience_collection_completion_system`（[collection.rs:165-215](../../src/systems/experience/collection.rs#L165-L215)），
具体实现在 §3.2 的 `route_persistent_agent_experience` 函数内。

__理由__（评审 D7）：`task_terminated_experience_trigger_system` 只 spawn `ExperienceCollectionRequestMessage`，根本不接触候选，
kind 此时未知（由 LLM 在 `submit_experience_candidate` 调用时才确定）。filter 必须在收集完成后、进入汇聚/治理前检查。

__实现__（评审 F12 修正：候选 ID 从 `ExperienceStore` 获取，不依赖 `msg.candidate_ids`）：
已在 §3.2 的 `route_persistent_agent_experience` 函数中展示。核心逻辑：

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

__新增候选状态__（`ExperienceCandidateStatus`）：

```rust
pub enum ExperienceCandidateStatus {
    // ... existing
    Discarded,    // 新增：被 kind_filter 过滤
}
```

#### 3.9 `self_updatable` 检查

在 [governance.rs:64-103](../../src/systems/experience/governance.rs#L64-L103) 的治理决策中，针对 skill 类候选增加检查。

__评审 F13 修正__：`injected_skill` 已拆为独立 Component `TaskInjectedSkill`，治理系统应通过 `Query<&TaskInjectedSkill>` 查询，
不能用 `task.injected_skill`。`experience_governance_system` 的函数签名需要扩展：

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
                // 不可自更新的 skill 产生 Skill 经验：标记 Discarded（v6 修订 D15）
                // 不强行降级为 Knowledge（payload 形态不匹配，会导致 writeback 失败）
                // 也不走 skill-updater（会自指循环）
                // 真正需要变更该 skill 的，应通过 IncubationProposal 提案新 skill
                candidate.status = Discarded;
                return None;  // 不产生治理决议
            }
        } else {
            // 持久Agent未注入 skill：形成新 skill
            destination = SkillPackage;
        }
    }
}
```

__注意__：注入skill 路径的候选在 §3.2 已被 `route_persistent_agent_experience` 直接走 skill-updater，不会进入 governance。
因此 §3.8 的 `SkillUpdate` destination 分支实际上只处理"候选已通过 governance 路径流入但发现 `injected_skill` 存在"的边界情况
——但按 §3.2 的设计，这种情况不会发生。为防御性编程，governance 仍检查 `injected_skill`，若存在则 redirect 到 skill-updater 路径。

__v6 修订 D15__：原 v3-v5 设计为 self_updatable=false 时"降级 kind_hint 为 Knowledge 并写入 LTM"。
实施时发现该路径存在两个问题：

1. __语义不诚实__：Skill payload（`file_refs` / `instructions` / `description`）与 Knowledge payload（`content: String`）形态不同，
   `as_long_term_memory_entry()` 对 Skill payload 返回 `None`，导致 writeback 失败
2. __循环防护冗余__：skill-updater 自身的任务会标 `KnowledgeOnly` filter（§3.7），
   skill 候选在 collection 阶段就被 `Discarded`，永远不会到达 governance 的 self_updatable 检查

修订为：self_updatable=false 的 skill 候选直接标记 `Discarded` + warn 日志（`SkillCandidateDiscardedNotSelfUpdatable`），
让 LLM 在下一轮重新评估。若确实需要变更该 skill，应通过 `IncubationProposal` 提案新 skill。

### 4. skill_update_completion_system 职责（评审 D6 补全、D11 补充）

新增系统 `skill_update_completion_system`，完整职责：

1. 接收 `SkillUpdateCompletedMessage`
2. 从 `SkillUpdateContext` 读取 `experience_candidate_id`
3. apply skill operations 到 SKILL.md（调用 `apply_skill_operations`）
4. apply 成功：
   - 把当前 SKILL.md 复制到 `history/v{base_version}.md`
   - 调用 `cleanup_skill_history` 保留 3 代
   - 刷新 SkillRegistry 中对应 `SkillEntry`（`version` 递增，`instructions` 更新）
   - __将候选状态置为 `Persisted`__（触发
     [profile_update.rs:23-111](../../src/systems/experience/profile_update.rs#L23-L111)
     的 profile-designer 评估）
5. apply 失败：
   - 候选状态保持原状（不置 `Persisted`）
   - 记录 error 日志
   - 触发重试或失败处理（详见 4.1）

__`SkillUpdateCompletedMessage` 的 insert 时机__（评审 D11 补充，D17 修正）：

复用现有 WorkItem 完成流程，不单独 spawn entity。具体路径：

1. skill-updater Agent 调用 `submit_skill_update` 工具
2. orchestrator.rs 处理 `ToolAction::SubmitSkillUpdate`：
   - 先对 `operations` 做 dry-run apply 校验，失败则同步返回 `ToolError::InvalidInput`（D18）
   - dry-run 通过后，把 `SkillUpdateCompletedMessage` insert 到 WorkItem entity
     （与 `SkillUpdateContext` 同 entity，D17）
3. `skill_update_completion_system` 通过同 entity Component 联合查询拿到
   `SkillUpdateContext` + `SkillUpdateCompletedMessage`，执行上述 5 步职责

__不通过__ `WorkItemCompletedMessage` 触发，因为 `WorkItemCompletedMessage` 是通用的，
不携带 skill 更新所需的具体 payload（operations、rationale 等）。`SkillUpdateCompletedMessage` 需要携带：

```rust
#[derive(Debug, Clone, Component)]
pub struct SkillUpdateCompletedMessage {
    pub work_item_id: Uuid,  // 保留用于日志，通过同 entity 查询获取
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

> 说明（D17）：`SkillUpdateCompletedMessage` 由 `Event` 改为 `Component`，不再 spawn 独立 entity，
> 而是 insert 到 WorkItem entity 上。`work_item_id` 字段保留用于日志追溯，但 `skill_update_completion_system`
> 不再用它做 Uuid 反查，而是直接通过同 entity 上的 `SkillUpdateContext` 拿 context。

#### 4.1 apply 失败处理

__修订（v5）__：原 v4 §4.1 描述"section 不存在 / frontmatter 字段不在白名单时跳过该 operation 并继续"。
实施时发现该语义会导致 SKILL.md 处于部分更新状态，难以让 LLM 从错误中恢复，也增加测试与审计复杂度。
经评审决定改为：__任何 apply 错误都整体回滚__，让 LLM 在下一轮重试中纠正完整 diff。

__修订（v7，D18）__：失败路径明确分为两条：

- (a) __dry-run 失败__（orchestrator 同步路径）：orchestrator 在 `SubmitSkillUpdate` 分支提前做
  dry-run apply 校验，section 不存在或 frontmatter 字段不在白名单时立即以 `ToolError::InvalidInput`
  同步返回给 LLM。此时 `SkillUpdateCompletedMessage` 还未 insert，候选状态不变，LLM 可在同一对话中
  修正 `operations` 重新提交
- (b) __completion_system apply 失败__（异步路径）：dry-run 通过后，`skill_update_completion_system`
  在真正写盘前的极小 TOCTOU 窗口内或 IO 错误发生时整体回滚，候选状态不变，记录 warn 日志

修订后的失败处理语义：

- 章节不存在：返回 `ApplyError::SectionNotFound`，整体回滚，候选状态不变
- 子章节不存在：返回 `ApplyError::SubsectionNotFound(section, subsection)`（v8 D19 新增），整体回滚，候选状态不变
- frontmatter 字段不在白名单：返回 `ApplyError::FieldNotWhitelisted`，整体回滚，候选状态不变
- post-apply 结构校验失败：apply 后 body 无 `##` 标题或首个 section 空，返回 `ApplyError::StructureInvalid`（v8 D19 新增），整体回滚，候选状态不变
- 文件 IO 错误：整体回滚，候选状态不变
- 读文件失败（如 SKILL.md 不存在）：返回 `ToolError::InternalState`（框架状态错误，区分于 LLM 输入错误）
- 任何错误均通过 `skill_update_completion_system` 记录 `SkillUpdateApplyFailed` warn 日志
  （含 `task_id`、`skill_id`、`error`、`error_type`），候选保持 `GovernanceResolved`，
  skill-updater Agent 可在下一轮重新提交完整 diff

> TOCTOU 风险注释（D18）：dry-run 与真正 apply 之间存在时间窗口，理论上 SKILL.md 可能被外部修改。
> 当前 orchestrator 与 completion_system 串行执行，且 skill-updater 是唯一写入方，风险可接受。
> 未来若引入并发写入，需要加文件锁或原子写。

#### 4.2 与 profile-designer 的链路闭合

skill-updater 写完 SKILL.md 后，候选状态置 `Persisted`，
[profile_update_trigger_system](../../src/systems/experience/profile_update.rs#L23-L111) 检测到 `Persisted` 后
会 spawn `ProfileGenerationRequestMessage { kind: Update }`，profile-designer 评估 agent profile 是否需要更新。两者不冲突。

### 5. profile-designer 与 skill-updater 的边界

| 场景 | 路径 | 涉及组件 |
|---|---|---|
| default Agent 产生 Skill/Knowledge 经验 | IncubationProposal→profile-designer→审批→writeback | profile-designer+writeback |
| 持久Agent+注入skill 产生 skill 经验 | __新路径__：collection→skill-updater→写SKILL.md | skill-updater+writeback+profile-designer |
| 持久Agent+注入skill 产生 knowledge 经验 | LTM路径→写持久Agent LTM→profile-designer评估 | writeback+profile-designer |
| 持久Agent+未注入skill 产生 skill 经验 | SkillPackage路径→形成新skill | writeback_to_skill_package+profile-designer |
| 持久Agent+未注入skill 产生 knowledge 经验 | LongTermMemory 路径→profile-designer 评估 | writeback+profile-designer |
| 临时Agent 产生经验 | 现有路径：进入父任务 inbox | queue_for_parent |

__职责切分__：

- profile-designer：管孵化期 profile 生成和持久Agent profile 评估更新（不写 SKILL.md）
- skill-updater：管成熟期 skill 迭代（只更新已有 skill，不形成新 skill）
- 两者写入路径不冲突：skill-updater 写 `<skill>/SKILL.md`，profile-designer 写 `agents.toml`

__触发 profile-designer 的条件不变__：
[profile_update.rs:23-111](../../src/systems/experience/profile_update.rs#L23-L111) 检测 `Persisted` 状态。
skill-updater 写完 SKILL.md 后，candidate 状态也置 `Persisted`，profile-designer 仍会被触发评估。

### 6. 执行 Agent 能看到 skill 元信息

改造 `submit_experience_candidate` 工具的 prompt（[collection.rs:75-83](../../src/systems/experience/collection.rs#L75-L83)），
显式告诉 LLM：

- 当前 task 注入的 skill 是什么（name + description + instructions）
- 如果经验用于改进当前 skill，请使用 `kind=skill`
- 如果经验是事实性知识，请使用 `kind=knowledge`

__generation 端格式约束__（D19 扩展）：prompt 中追加 SKILL.md 格式约束：

- `instructions` 字段必须是 markdown 格式，至少包含 1 个 `## Section` 二级标题
- 推荐使用 `## Overview` / `## Usage` / `## Examples` / `## Edge Cases` / `## Limitations` 等 section
- 复杂 skill 可在 `##` 下使用 `### Subsection` 三级标题组织内容
- 不要使用 `####` 或更深层级（update 端不支持作为 operation 锚点）
- 落盘前框架会做 `validate_skill_structure` 校验，不符合则候选置 `WritebackFailed`

同时修改 `submit_experience_candidate` 工具的 description 字段，明确 `instructions` 格式要求。

### 7. skill 删除/退役机制（评审 D9 — 显式推迟）

本 ADR __不引入__ skill 删除/退役机制。理由：

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
- `SkillUpdateCompletedMessage`（Component，D17 修正：原为 `Event`，现 insert 到 WorkItem entity 上）
- `ToolError::InternalState` 变体（区分框架状态错误与 LLM 输入错误，D16/D18）
- `ToolExecutionRequestMessage.work_item_entity: Option<Entity>` 字段（D17 修正：用于在
  orchestrator 处理 `submit_skill_update` 时 O(1) 查询 context）
- `SkillUpdateOperation::ReplaceSubsection` / `AddSubsection` / `RemoveSubsection` / `ReplaceBody`
  变体（v8 D19 新增：三级标题级 operation + body 兜底）
- `ApplyError::SubsectionNotFound(section, subsection)` 变体（v8 D19 新增）
- `ApplyError::StructureInvalid` 变体（v8 D19 新增：post-apply 结构校验失败）
- `SkillStructureError` 枚举（v8 D19 新增：`NoSectionHeading` / `EmptyFirstSection`）
- `validate_skill_structure` 函数（v8 D19 新增）
- `find_subsection_range` 函数（v8 D19 新增）

### 修改

- `LoadedSkill`：新增 `version: u32`、`self_updatable: bool`
- `SkillLoader`：解析 frontmatter 新字段，构造 `SkillRegistry`
- SKILL.md frontmatter：新增 `version`、`self_updatable`
- `SubTaskConfig`：`child_agent_name` 字段在 brain_dispatch 中被读取
- `HarnessConfig`：新增 `[brain.skill_selection]` 配置段
- `select_agent_for_sub_task`：函数签名扩展，新增 `skill_registry` 参数，返回值新增 `Option<SkillId>`
- `experience_collection_completion_system`：新增 `agents` Query 参数，
  `tasks` 改为联合查询 `Query<(&Task, Option<&TaskInjectedSkill>,
  Option<&TaskExperiencePolicy>)>`（评审 F14 + D10）
- `experience_governance_system`：新增 `skill_registry: Res<SkillRegistry>` 和
  `tasks: Query<(&Task, Option<&TaskInjectedSkill>)>` 参数（评审 F13）
- `persist_skill_package`（v8 D19）：落盘 SKILL.md 时显式写入
  `name + description + self_updatable: true` 三个 frontmatter 字段
- `collection.rs` prompt 模板（v8 D19）：追加 SKILL.md 格式约束
- `submit_experience_candidate` 工具 description（v8 D19）：明确 `instructions` 格式要求
- `apply_skill_operations`（v8 D19）：支持 4 种新 operation variant，post-apply 追加
  `validate_skill_structure` 校验
- `find_section_range`（v8 D19）：标题行比较改用 `l.trim() == header.trim()`，匹配第一个时记录 warn 日志
- orchestrator `SubmitSkillUpdate` 分支（v8 D19）：dry-run apply 后追加 post-apply 结构校验，
  失败以 `ToolError::InvalidInput` 同步返回

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
   - `replace_frontmatter`：字段在白名单 / 不在白名单 / 字段不存在（upsert 追加）
   - `replace_subsection`：父 section 存在 / 不存在，子 section 存在 / 不存在（v8 D19）
   - `add_subsection`：父 section 存在 / 不存在，`after` 子 section 存在 / 不存在（v8 D19）
   - `remove_subsection`：父 section 存在 / 不存在，子 section 存在 / 不存在（v8 D19）
   - `replace_body`：替换后 frontmatter 不变（v8 D19）
5. `apply_skill_operations` 章节匹配：同层级同名章节匹配第一个，记录 warning（v8 D19 修复 ADR-004 v7 已知局限 1 落地）
6. `apply_skill_operations` 标题规范化：`"## Usage "` 尾部空格能正确匹配（v8 D19 修复 ADR-004 v7 实现偏差 D）
7. `validate_skill_structure`：无 `##` 标题 / 首个 section 空 / 合规三种情况（v8 D19）
8. `apply_skill_operations` post-apply 校验：`replace_body` 删除所有 `##` 标题时返回 `StructureInvalid`（v8 D19）
9. `cleanup_skill_history`：保留 3 代，超过的删除
10. `experience_kind_filter` 过滤：`KnowledgeOnly` 下 skill 候选被标记 `Discarded`
11. `self_updatable=false` 的 skill 候选被标记 `Discarded`（v6 D15 修正）
12. `parse_brain_skill_selection` 容错：标准 JSON、`skill_name: "None"`、`skill_name: ""`、额外字段、非标准 JSON
13. `ExperienceKindFilter` 默认值为 `All`
14. `persist_skill_package` 落盘前结构校验：合规 / 无 `##` 标题两种情况（v8 D19）

### 集成测试

1. __brain 选 skill 成功路径__：task 适合某 skill，brain 选 Agent+skill，skill 注入 prompt，任务执行
2. __brain 选 skill 失败重试__：brain 选错 skill 名字，重试，达到上限 fallback
3. __持久Agent + 注入skill + skill 经验__：完整路径——collection 拦截 → skill-updater workitem →
   submit_skill_update → SKILL.md 更新 → 候选置 `Persisted` → profile-designer 评估
4. __持久Agent + 注入skill + knowledge 经验__：knowledge 写入持久Agent LTM，不进父 inbox
5. __持久Agent + 未注入skill + skill 经验__：走原 writeback_to_skill_package 路径，
   persist_skill_package 落盘前校验通过 / 失败两种情况（v8 D19）
6. __临时Agent + 经验__：走原 queue_for_parent 路径
7. __skill-updater 自指循环防护__：skill-updater 产生 skill 候选，`self_updatable=false` 标记 `Discarded`
8. __skill-updater kind filter 防护__：skill-updater 的 task 标 `KnowledgeOnly`，skill 候选被标记 `Discarded`
9. __skill 版本递增__：连续两次 skill 更新，version 正确递增，history 保留 3 代
10. __skill 回退保护__：apply 失败，SKILL.md 不变，history 不写入，候选状态不变
11. __持久Agent + 注入skill 路径不进父 inbox__：验证父 Agent 的 `ExperienceInbox` 中无对应候选
12. __subsection operation 端到端__：含 `###` 子章节的 SKILL.md 通过 `replace_subsection` /
    `add_subsection` / `remove_subsection` 更新，结果符合预期（v8 D19）
13. __replace_body 兜底__：无 `##` 标题的 SKILL.md 通过 `replace_body` 重写为含章节结构（v8 D19）
14. __post-apply 结构校验回滚__：`replace_body` 删除所有 `##` 标题，dry-run 同步返回
    `ToolError::InvalidInput`，SKILL.md 不变（v8 D19）

## 开放问题（留给实施阶段细化）

1. `SkillRegistry` 运行期更新的同步机制：skill-updater 写入后是立即同步还是通过事件异步刷新
2. `skill-updater` workitem 的 governing_agent_id：建议为触发它的持久Agent
3. brain LLM 选 skill 的 prompt 模板：需要单独设计，不在本 ADR 范围内
4. skill-updater 自身 skill 的初始内容：需要在 `agents.toml` 中预配置
5. skill 删除/退役机制：显式推迟，作为已知约束（§7）
6. ~~__file_refs 的 updater 支持__（v8 D19 显式推迟）~~：已由 [ADR-006](ADR-006-skill-updater-multi-file-support.md) 接替
   （落盘到 `scripts/` / `references/` / `assets/` 子目录），update 端无对应 operation。
   若需更新 file_refs 引用的文件，由人工编辑或重新生成 skill。未来若有实际需求，
   单独 ADR 推进 `add_file` / `remove_file` / `replace_file` operation → 已由 ADR-006 实现
7. __plugin skill frontmatter 缺失__（v8 D19 调研发现）：`plugins/harness-demo/skills/demo-skill.md`
   无 frontmatter，`parse_skill_md` 会返回 None，无法被加载。作为独立任务修复，不在 v8 改造范围
8. __skill-updater Agent kind 声明__（v8 D19 调研发现）：`agents.toml` 中 `skill-updater` 未显式声明
   `kind = "Persistent"`，需确认解析逻辑的缺省值。作为独立任务修复，不在 v8 改造范围

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
| D7 | `kind_filter` 检查点从 `task_terminated_experience_trigger_system` 移到 `experience_collection_completion_system` |
| D8 | Task 新增字段拆为独立 Component `TaskInjectedSkill` 和 `TaskExperiencePolicy` |
| D9 | 显式推迟 skill 删除/退役机制，作为已知约束记录（§7） |

### 第二轮（`logs/2026-07-18-adr-004-skill-first-class-review-v2.md`）

| 评审编号 | 修正内容 |
|---|---|
| F12 | §3.2/§3.7 移除 `msg.candidate_ids`，改从 `ExperienceStore` 获取候选（`aggregate_inbox_for_task` 等） |
| F13 | §3.8 治理伪代码从 `task.injected_skill` 改为通过 `Query<(&Task, Option<&TaskInjectedSkill>)>` 联合查询 |
| F14 | §3.1 移除占位符，改为联合查询 `Query<(&Task, Option<&TaskInjectedSkill>, Option<&TaskExperiencePolicy>)>` |
| D10 | 补充 SystemParam 封装建议（`TaskExperienceQuery`），作为实施阶段优化 |
| D11 | 补充 `SkillUpdateCompletedMessage` spawn 时机（orchestrator.rs 处理 `ToolAction::SubmitSkillUpdate`）和字段定义 |
| D12 | 明确用户确认策略：注入skill 路径绕过 governance（skill/knowledge 均无需确认），未注入skill 路径仍经 governance 走用户确认 |
| F15 | §3.2 移除 `candidate_ids`，候选置 `GovernancePending`，由 `governance_candidates_for_task` 自动发现 |

### 第四轮（实施偏差修正 — 2026-07-18）

| 评审编号 | 修正内容 |
|---|---|
| D13 | §4.1 修正：原"section 不存在 / field 不在白名单时跳过并继续"改为"任何 apply 错误都整体回滚"。部分更新会让 SKILL.md 处于不一致状态，整体回滚更安全。 |
| D14 | §2.3 补充错误类型：`parse_brain_skill_selection` 使用 `thiserror` 定义的 `BrainSkillSelectionError`，符合库 crate 规范。 |

### 第五轮（实施 review 修正 — 2026-07-18）

| 评审编号 | 修正内容 |
|---|---|
| D15 | §3.8 修正：self_updatable=false 不降级 Knowledge，改 Discarded + warn。payload 不匹配致 writeback 失败。 |

### 第六轮（Phase 4 skill update 实施修正 — 2026-07-19）

| 评审编号 | 修正概要 |
|---|---|
| D16 | §3.5 修正：工具参数收敛为 `operations` + `rationale`（Bug A） |
| D17 | §4 + 数据结构清单：完成消息改为 Component insert（Bug B） |
| D18 | §3.6 + §4.1 修正：dry-run 同步校验 + 完整 SKILL.md prompt（Bug C） |

详细内容：

- D16（Bug A）：`submit_skill_update` 工具参数从 `skill_id + base_version + new_version + operations + rationale`
  收敛为 `operations` + `rationale`；`skill_id` / `base_version` / `new_version` 由 orchestrator
  从 `SkillUpdateContext` 服务端权威注入，避免 LLM 臆造 `skill_id`
- D17（Bug B）：`SkillUpdateCompletedMessage` 改为 Component，由 orchestrator insert 到 WorkItem
  entity 上；`skill_update_completion_system` 通过同 entity Component 联合查询拿 context，
  不再用 `work_item_id` 反查；新增 `ToolExecutionRequestMessage.work_item_entity: Option<Entity>`
  字段
- D18（Bug C）：orchestrator 在 `SubmitSkillUpdate` 分支提前做 dry-run 同步校验，错误以
  `ToolError::InvalidInput` 同步反馈给 LLM；prompt 现在包含完整 SKILL.md 内容
  （frontmatter + section 标题），让 LLM 看到真实结构

### 第七轮（两端对齐改造 — 2026-07-19，v8）

| 评审编号 | 修正概要 |
|---|---|
| D19 | §1.2 / §3.5 / §3.6 / §3.7（新）/ §4.1 / §6 修正：两端对齐改造（详见下文） |

详细内容（D19 子决策共 11 项，通过 grilling skill 与用户逐项确认）：

1. __对齐方向__：C 双向对齐（generation + update 端都改）
2. __颗粒度细化__：2.2 三级标题级 operation（`replace_subsection` / `add_subsection` /
   `remove_subsection`）+ `replace_body` 兜底
3. __generation 端约束强度__：3.3 硬约束（prompt + 工具描述 + 落盘前 dry-run 校验）
4. __强制 section__：取消原 grilling 中提议的"强制 `## Overview`"，dry-run 只校验
   "至少 1 个 `##` 标题 + 首个 section 非空"
5. __frontmatter 字段__：5.4 generation 端写 3 字段（`name + description + self_updatable`），
   不扩展 update 端白名单
6. __file_refs updater 支持__：6.4 暂不实现，标记 future work → 已由 [ADR-006](ADR-006-skill-updater-multi-file-support.md) 接替
7. __candidate payload 传递__：7.1 保留扁平化文本，补充候选类型显式说明
8. __dry-run 校验规则__：8.2 简化版（generation 端校验"至少 1 个 `##`"，update 端校验
   "apply 后至少 1 个 `##`"）
9. __replace_body 约束__：10.2 prompt 软约束警示，不强制兜底场景
10. __附带修复__：11.2 仅修 A/D（diff.rs 相关：warning 日志 + trim 规范化），
    B/C（plugin skill frontmatter / skill-updater kind 声明）作为独立任务
11. __实施路径__：12.3 先 ADR 后实施 + 单一 ADR 升级（本次 v8 升级即为阶段 1 产物）

__与 v7 的关键差异__：

- v7 update 端仅 4 种 operation（粗粒度），v8 扩展到 8 种（含 3 种 subsection + 1 种 body 兜底）
- v7 generation 端无结构校验，v8 新增 `validate_skill_structure` + `persist_skill_package` 落盘前校验
- v7 generation 端 frontmatter 只写 2 字段，v8 改为 3 字段
- v7 `find_section_range` 用 `trim_start()` 比较且无 warning 日志，v8 改用 `trim()` 比较 + warn 日志
- v7 ADR-004 已知局限 1（warning 日志）未落地，v8 显式修复并补充 `find_subsection_range` 同样规则

## 关联文件

- [src/domain/task.rs](../../src/domain/task.rs) — Task 字段拆为独立 Component
- [src/domain/work_item.rs](../../src/domain/work_item.rs) — `WorkItemType::SkillUpdate` 变体
- [src/domain/contribution.rs](../../src/domain/contribution.rs) —
  `ExperienceKindFilter`、`ExperienceCandidateStatus::Discarded`、
  `SkillUpdateOperation`（v8 含 8 种 variant）
- [src/infrastructure/skills/loader.rs](../../src/infrastructure/skills/loader.rs) —
  SkillRegistry + SkillEntry + frontmatter 新字段解析
- [src/infrastructure/skills/diff.rs](../../src/infrastructure/skills/diff.rs) —
  `apply_skill_operations`、`find_section_range`、
  `find_subsection_range`（v8 新增）、
  `validate_skill_structure`（v8 新增）、`FRONTMATTER_WHITELIST`、
  `ApplyError`（v8 新增 `SubsectionNotFound` / `StructureInvalid`）、
  `SkillStructureError`（v8 新增）
- [src/infrastructure/assets/service.rs](../../src/infrastructure/assets/service.rs) —
  `persist_skill_package`（v8 D19 改为 3 字段 frontmatter +
  落盘前 `validate_skill_structure` 校验）
- [src/systems/dispatch/brain_dispatch.rs](../../src/systems/dispatch/brain_dispatch.rs) —
  brain 选 skill
- [src/systems/dispatch/agent_selection.rs](../../src/systems/dispatch/agent_selection.rs) —
  `select_agent_for_sub_task` 签名扩展
- [src/systems/experience/collection.rs](../../src/systems/experience/collection.rs) —
  持久Agent吸收分支 + kind_filter 检查 + prompt
  （v8 D19 追加 SKILL.md 格式约束）
- [src/systems/experience/governance.rs](../../src/systems/experience/governance.rs) —
  self_updatable 检查
- [src/systems/experience/skill_update.rs](../../src/systems/experience/skill_update.rs) —
  skill-updater workitem 系统 + `candidate_payload_text`
  （v8 D19 补充候选类型显式说明）
- [src/systems/tools/builtin/submit_skill_update.rs](../../src/systems/tools/builtin/submit_skill_update.rs) —
  新工具
- [submit_experience_candidate.rs](../../src/systems/tools/builtin/submit_experience_candidate.rs) —
  generation 端 LLM 入口（v8 D19 工具 description
  追加格式要求）
- [src/systems/tools/orchestrator.rs](../../src/systems/tools/orchestrator.rs) —
  `ToolAction::SubmitSkillUpdate` 处理 + dry-run 校验
  （v8 D19 追加 post-apply 结构校验）
