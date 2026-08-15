# `/skill` Slash Command 设计文档

> __状态：当前有效__

## 1. 背景

AI Harness 已有 skill 体系支持 Agent 的能力注入：Agent 通过
`.harness/assets/agents/<agent>/skills/<skill>/SKILL.md` 加载静态 skill，skill-updater Agent
可在任务结束后根据经验候选更新已有 skill。

但当前缺少__用户主动创建新 skill__ 的入口。用户若想为某个 Agent 添加新 skill，只能手动创建
SKILL.md 文件。本设计增加 `/skill <意图描述>` slash command，让用户通过自然语言意图驱动
LLM 为当前任务的 Agent 无中生有生成新 skill。

## 2. 设计决策汇总

| # | 决策点 | 结论 |
|---|--------|------|
| D1 | Skill 存放位置 | `.harness/assets/agents/<agent>/skills/<skill_name>/` |
| D2 | 创建方式 | 无中生有创建新 skill |
| D3 | 命令语法 | `/skill <意图描述>`，`UserCommand::CreateSkill { intent: String }` |
| D4 | 执行 Agent | 专用 `skill-creator` Agent，声明在 `agents.toml` |
| D5 | 确认机制 | 复用 ExperienceCandidate 确认流程 |
| D6 | 新建/更新区分 | `ExperienceCandidatePayload::Skill` 新增 `is_new: bool` 字段 |
| D7 | Prompt 上下文 | 意图 + 已有 skill 列表 + SKILL.md 模板 + agent profile；其余由工具自主获取 |
| D8 | 沙盒路径 | `.harness/assets/agents/<agent>/skills/.sandbox/<skill_name>/` |
| D9 | 提交信号 | 专用 `submit_skill` 工具 |
| D10 | 格式验证 | SKILL.md 存在 + frontmatter 合规 + 路径安全（无后缀白名单） |
| D11 | 写回方式 | rename 原子移动，同名冲突拒绝 |
| D12 | 标记字段 | `is_new: bool`（D6 同条） |
| D13 | system prompt | 硬编码在 `agents.toml` |
| D14 | 无活跃任务 | 报错拒绝 |
| D15 | 空意图 | 报错，提示 usage |
| D16 | 沙盒清理 | 随任务终态自动清理 |
| D17 | SkillRegistry 注册 | 写回时同步注册 |
| D18 | 权限控制 | 无额外控制，确认流程已是闸门 |
| D19 | 同名 skill | 严格拒绝，引导 skill-updater |

## 3. 核心数据流

```text
用户输入: /skill <intent>
    │
    ▼
command_parse_system
    │ 解析为 CreateSkill { intent }
    │ 查找当前活跃任务的 Agent
    │ 无活跃任务 → 报错退出
    │
    ▼
SkillCreationRequestMessage（spawn）
    │ 携带: task_id, agent_id, intent, sandbox_dir
    │
    ▼
skill_creation_workitem_system（消费 request）
    │ 1. 创建 .sandbox/<skill_name>/ 沙盒目录
    │ 2. 从 SkillRegistry 获取已有 skill 列表
    │ 3. 从 Agent 组件获取 profile
    │ 4. 构造 prompt（意图 + 已有 skill 列表 + SKILL.md 模板 + agent profile）
    │ 5. 从 SpaceToolRegistry 过滤工具（submit_skill + write_skill_file）
    │ 6. 创建 WorkItem::skill_creation(...)
    │ 7. 附加 SkillCreationContext + PendingDispatch
    │
    ▼
dispatch_system
    │ 按 WorkItemType::SkillCreation.required_tag() → "skill-creator"
    │ 查找 tag 匹配的 Persistent Agent
    │ 派发 AgentExecutionRequestMessage
    │
    ▼
skill-creator Agent 执行
    │ LLM 自由创建文件（调用 write_skill_file 写入沙盒）
    │ LLM 调用 submit_skill(name, description) 提交
    │
    ▼
submit_skill 工具 / orchestrator
    │ 1. 格式验证（SKILL.md 存在 + frontmatter + 路径安全）
    │ 2. 验证失败 → 返回错误信息给 LLM，允许修复重提
    │ 3. 验证通过 → 产出 ExperienceCandidate (is_new: true)
    │    扫描沙盒目录生成 file_refs
    │
    ▼
ExperienceCandidate 确认流程
    │ candidate.status = NeedsUserApproval
    │ 用户在 TUI 审批界面确认
    │
    ▼
skill_creation_writeback_system（新建专用写回）
    │ 1. 检查正式目录是否同名 → 同名则拒绝
    │ 2. rename .sandbox/<name>/ → <name>/
    │ 3. SkillRegistry 同步注册
    │ 4. candidate.status = Persisted
    │ 5. despawn WorkItem 实体
    │
    ▼
后续任务可加载新 skill
```

## 4. 变更清单

### 4.1 领域层变更

#### `src/domain/command.rs`

- `UserCommand` 枚举新增 `CreateSkill { intent: String }` 变体
- `parse()` 新增 `/skill` 前缀匹配逻辑

#### `src/domain/contribution.rs`

- `ExperienceCandidatePayload::Skill` 新增 `is_new: bool` 字段，默认 `false`
- `ExperienceCandidate::skill()` 构造函数新增 `is_new` 参数（向后兼容：所有现有调用传 `false`）
- 新增 `SkillCreationContext` Component：

  ```rust
  #[derive(Component, Debug, Clone)]
  pub struct SkillCreationContext {
      pub task_id: TaskId,       // 用于沙盒清理时关联终态 task（N1 修复）
      pub agent_id: AgentId,
      pub agent_name: String,
      pub sandbox_dir: PathBuf,
      pub skill_name: String,
  }
  ```

#### `src/domain/message.rs`

- 新增 `SkillCreationRequestMessage`：

  ```rust
  #[derive(Debug, Clone, Component)]
  pub struct SkillCreationRequestMessage {
      pub task_id: TaskId,
      pub agent_id: AgentId,
      pub agent_name: String,
      pub intent: String,
  }
  ```

#### `src/domain/work_item.rs`

- `WorkItemType` 枚举新增 `SkillCreation` 变体
- `required_tag()` 新增 `SkillCreation => "skill-creator"` 映射
- `WorkItem` 新增 `skill_creation()` 工厂方法
  - `multi_turn = true`（C4 修复）：skill-creator 需要多轮工具调用（`read_skill_file` 读已有
    skill → `write_skill_file` 写沙盒 → `submit_skill` 提交），与 `skill_update` WorkItem 的
    `multi_turn = true` 一致

### 4.2 系统层变更

#### `src/systems/command.rs`

- `command_parse_system` 的 match 新增 `UserCommand::CreateSkill { intent }` 分支：
  - 空意图 → `eprintln!("[skill] usage: /skill <intent description>")` + despawn
  - 查找当前活跃任务的 Agent（与 `/finish` 同逻辑：同 channel、非终态）
  - 无活跃任务 → `eprintln!("[skill] no active task — /skill requires an active task")` + despawn
  - 有活跃任务 → spawn `SkillCreationRequestMessage`

#### 新增 `src/systems/experience/skill_creation.rs`

- `skill_creation_workitem_system`：消费 `SkillCreationRequestMessage`，创建 WorkItem
  - 创建沙盒目录 `.sandbox/<intent-derived-name>/`（从 intent 提取简短名称，或使用时间戳兜底）
  - 构造 prompt：意图 + 已有 skill 列表（名称+描述）+ SKILL.md 模板规范 + Agent profile
  - 从 SpaceToolRegistry 过滤工具：`submit_skill`、`write_skill_file`、`read_skill_file`
  - spawn `(WorkItem, SkillCreationContext, PendingDispatch)`

- `skill_creation_writeback_system`：消费用户确认后的 `ExperienceCandidate`（`is_new == true`）
  - 从 `SkillCreationContext` 获取 `sandbox_dir` 和 `skill_name`
  - 检查正式目录同名 → 拒绝
  - `std::fs::rename(sandbox_dir, target_dir)` 原子移动
  - `SkillRegistry::upsert()` 同步注册
  - 候选状态置 `Persisted`
  - despawn 实体

### 4.3 工具层变更

#### 新增 `src/systems/tools/builtin/submit_skill.rs`

- `SubmitSkillTool`：skill-creator 专用提交工具（S7 修复：Sync 工具，只解析参数返回 `ToolAction`，不执行 I/O——与 `submit_skill_update` 完全对齐）
  - 参数：`name: String`（skill 名称）、`description: String`（skill 描述）
  - 参数校验：`name` 非空、`description` 非空，否则返回 `ToolError::InvalidInput`
  - 返回 `ToolAction::SubmitSkillCandidate { name, description }`——验证（SKILL.md 存在、
    frontmatter 合规、路径安全、扫描 file_refs）和 `ExperienceCandidate` 构造由 orchestrator
    在主线程执行（与 `submit_skill_update` 的 dry-run 验证同模式）
  - __依赖 4.10 节__：orchestrator 验证时通过 `SkillCreationContext.sandbox_dir` 获取沙盒
    路径，该字段由 4.10 节的 dispatch 路径填充。4.10 节修复必须优先于或同步于本工具实施
    （S3 修复）

#### 新增 `src/systems/tools/builtin/write_skill_file.rs`

- `WriteSkillFileTool`：skill-creator 专用文件写入工具（S7 修复：Async 工具，符合"禁止新增 Sync 工具"约束）
  - 参数：`path: String`（相对沙盒路径）、`content: String`
  - __依赖 4.10 节__：沙盒根路径通过 `ToolContext.current_skill_dir` 获取，该字段由 4.10 节的 dispatch 路径填充。4.10 节修复必须优先于或同步于本工具实施（S3 修复）
  - `kind()` override 为 `ToolActionKind::Async`
  - Worker 中执行：从 `ToolContext.current_skill_dir` 获取沙盒根路径 → 路径安全验证（不允许 `../` 逃逸）→ 创建必要的父目录 → 写入文件 → 返回成功/失败
  - 新增 `ToolEffect::WriteSkillFile { path: String, content: String }` 变体
    （`src/domain/tool_async.rs`），由 `commit_tool_effects_system` 在主线程落账（文件 I/O
    量小，可在 commit 中同步执行）
  - 路径安全验证与后缀白名单一致性：遵循 `ALLOWED_FILE_SUFFIXES`（与 5.8 节一致）

### 4.4 基础设施层变更

#### `src/infrastructure/skills/loader.rs`

- `SkillLoader::load_skills()` 和 `SkillLoader::build_registry()` 均跳过 `.sandbox` 等 `.` 开头的子目录（S2 修复）
  - `load_skills()`：加载单个 agent 的 skill，用于注入 Agent prompt
  - `build_registry()`：启动时扫描所有 agent 的 skill 构建 `SkillRegistry`
  - 如果只修改 `load_skills()` 不修改 `build_registry()`，启动时 `build_registry()` 会将 `.sandbox` 下的未完成 skill 注册到 `SkillRegistry`，导致同名冲突检查误判

#### `src/infrastructure/skills/diff.rs`

- `validate_skill_file_path` 无需变更，沙盒路径验证在 `WriteSkillFileTool` 中独立处理

### 4.5 配置变更

#### `agents.toml`

新增 `skill-creator` Agent 声明，并同步修复 skill-updater 的 `read_skill_file` 权限 gap（N2 修复）：

````toml
[[agent]]
name = "skill-creator"
tags = ["skill-creator"]
description = "技能创建专家，根据用户意图为指定 Agent 设计并生成新 skill"
system_prompt = """
你是一名技能创建专家。根据用户的意图描述，为指定 Agent 创建新 skill。

## 工作流程

1. 使用 read_skill_file 读取已有 skill，理解现有能力和风格
2. 在沙盒目录下自由创建 skill 文件（SKILL.md + 辅助文件）
3. 使用 write_skill_file 创建或修改沙盒中的文件
4. 完成后调用 submit_skill(name, description) 提交

## SKILL.md 规范

SKILL.md 是 skill 的入口文件，必须存在于 skill 目录根路径。

frontmatter 格式：
```
---
name: <skill 名称>
description: <一句话描述>
version: 1
self_updatable: true
---
```

body 要求：
- 至少包含一个 ## 二级标题
- 第一个 ## 标题下必须有实质内容

## 文件创建规则

- path 必须为相对路径，不允许 `../` 逃逸
- 可以创建多文件 skill：辅助 markdown、脚本、配置等
- 所有文件必须在沙盒目录内

## 质量要求

- 与已有 skill 形成联动，避免功能重叠
- 指令清晰、步骤明确，Agent 可直接执行
- 优先与 Agent 的定位（tags、description）一致"""

[[agent.models]]
provider = "kimi-k2"
model = "Kimi-K2.6"

[[agent.models]]
provider = "deepseek"
model = "deepseek-v4-flash"

[agent.tools]
default_permission = "Deny"
submit_skill = "Allow"
write_skill_file = "Allow"
read_skill_file = "Allow"
````

#### skill-updater 权限修复（N2）

现有 skill-updater 的 `agents.toml` 配置中未声明 `read_skill_file = "Allow"`，虽然
`skill_update_workitem_system` 将 `read_skill_file` 过滤到了 WorkItem 工具集中，但 Agent 的
`effective_permission` 会在运行时拒绝。此为已有 bug，在本次变更中一并修复：

```toml
# skill-updater 现有配置
[[agent]]
name = "skill-updater"
# ...（省略未变字段）

[agent.tools]
default_permission = "Deny"
submit_skill_update = "Allow"
read_skill_file = "Allow"   # 新增：修复已有 gap，使 ADR-006 描述的多文件更新能力生效
```

#### `src/systems/transform/task_lifecycle.rs`

- __`task_termination_system`__ 中增加沙盒清理逻辑（S6 修复：需检查候选状态，避免竞态）：
  - 任务从非终态转入终态（Completed/Failed）时，查询带 `SkillCreationContext` 的 WorkItem entity
  - 通过 `SkillCreationContext.task_id` 关联终态 task
  - 从 `ExperienceStore` 查询关联候选的状态，按状态决定清理策略：
    - 候选已 `Persisted` / `Rejected` / `Discarded` → 删除沙盒目录 + despawn WorkItem
      entity（writeback 已完成或已拒绝）
    - 候选仍在 `NeedsUserApproval` → __不清理沙盒、不 despawn WorkItem__（用户确认后
      `skill_creation_writeback_system` rename 沙盒到正式目录，rename 成功后沙盒目录已移走，
      无需额外清理）
    - 候选仍在 `Submitted` / `GovernancePending` 等（skill-creator 还在执行或候选未到确认
      环节）→ 删除沙盒目录 + despawn WorkItem entity + 候选置 `Discarded`
  - __不新增"despawn WorkItem entity"的默认行为__（与现有模式一致：`task_termination_system`
    当前不 despawn WorkItem，WorkItem 由各自 completion_system despawn）。仅在上述安全条件
    下 despawn

- __`clear_task_system`__ 中同样增加沙盒清理逻辑（S4 修复）：
  - `/clear` 命令跳过终态处理链路，如果只在 `task_termination_system` 清理，`/clear` 后沙盒会残留
  - `/clear` 是用户主动取消，语义上更激进：候选无论处于何种状态，都应清理沙盒 + 候选置
    `Discarded` + despawn WorkItem
  - 复用同一候选状态检查逻辑
    `cleanup_skill_creation_sandbox(commands, store, sandbox_dir, candidate_status, force_cleanup)`，
    `clear_task_system` 传入 `force_cleanup = true`

### 4.7 ToolAction 扩展

#### `src/domain/space.rs`

- `ToolAction` 枚举新增 `SubmitSkillCandidate { name: String, description: String }` 变体（S7 修复）
  - 注意：`ToolAction` 定义在 `src/domain/space.rs`，不是 `src/domain/execution.rs`
  - 变体只携带 LLM 提交的 `name` 和 `description`，不含完整 `ExperienceCandidate`——与
    `submit_skill_update` 模式完全对齐（工具只解析参数，验证和候选构造由 orchestrator 执行）
  - orchestrator 处理此 ToolAction 时：从 `SkillCreationContext.sandbox_dir` 获取沙盒路径 →
    执行验证（SKILL.md 存在、frontmatter 合规、路径安全）→ 验证通过后构造完整
    `ExperienceCandidate::skill_new(...)` → 扫描沙盒生成 `file_refs` → 更新
    `SkillCreationContext.skill_name` → 入队 `ExperienceStore`
  - 这与 `ToolAction::SubmitSkillUpdate { operations, rationale }` 模式一致：工具只提供
    LLM 意图参数，服务端权威数据（skill_id、base_version、sandbox 路径等）由 orchestrator
    从 Context 注入

### 4.8 写回路径集成

#### `src/domain/contribution.rs`

- `ExperienceWritebackDestination` 枚举新增 `SkillCreation` 变体
  - 治理系统对 `is_new == true` 的 Skill 候选路由到此目标
  - 与现有 `SkillUpdate`（更新已有 skill）对称

#### `src/systems/experience/governance.rs`

- `experience_governance_system` 对 `is_new == true` 的候选：
  - __插入位置__（N3 修复）：在 `ExperienceKindHint::Skill` 分支入口处、`if is_default`
    之前，加 `if is_new` 早返回
  - `/skill` 命令创建的 WorkItem 任务__没有注入 skill__（创建新 skill，不是执行已有 skill），
    如果不在此处拦截，`is_new == true` 的候选会走到 `else` 分支（未注入 skill），被路由到
    `SkillPackage` 而非 `SkillCreation`
  - 具体代码：

    ```rust
    ExperienceKindHint::Skill => {
        // 优先检查 is_new：/skill 命令创建的新 skill 候选
        if is_skill_new(&candidate.payload) {
            Some(ExperienceGovernanceDecision {
                candidate_id: *candidate_id,
                destination: ExperienceWritebackDestination::SkillCreation,
                requires_user_confirmation: true,
                decision_rationale: "new skill creation -> rename writeback".to_string(),
                source_task_id: request.task_id,
            })
        } else if is_default {
            // ... 现有逻辑不变
        }
    }
    ```

  - `requires_user_confirmation: true`（与 D5 一致，复用确认流程）
  - 不经过 skill-updater 路由

#### `src/systems/experience/approval.rs`

- `experience_approval_result_system` 处理确认后：
  - `ExperienceWritebackDestination::SkillCreation` → 将 `SkillCreationWritebackMessage`
    __insert 到已有 WorkItem entity__（S1/S5 修复），而非 spawn 独立 entity
  - 而非走 `ExperienceWritebackRequestMessage`（那是 `SkillPackage`/`SkillUpdate` 路径）
  - insert 到 WorkItem entity 的方式与 `skill_update_completion_system` 消费
    `SkillUpdateCompletedMessage` 完全对齐：消息与 `SkillCreationContext` 在同一 entity，
    可直接通过同 entity 查询 Context
  - approval system 通过 `task_id` → 遍历 `Query<(Entity, &WorkItem, &SkillCreationContext)>`
    找到 WorkItem entity，然后 `commands.entity(wi_entity).insert(SkillCreationWritebackMessage
    { ... })`

#### `src/domain/message.rs`

新增 `SkillCreationWritebackMessage`：

```rust
#[derive(Debug, Clone, Component)]
pub struct SkillCreationWritebackMessage {
    pub candidate_id: uuid::Uuid,
    pub task_id: TaskId,
}
```

`skill_creation_writeback_system` 消费此消息，通过同 entity 查询 `SkillCreationContext`
（与 `skill_update_completion_system` 消费 `SkillUpdateCompletedMessage` 同模式：消息由
approval system insert 到 WorkItem entity 上，该 entity 已有 `SkillCreationContext + WorkItem`）。

### 4.9 Orchestrator 集成

#### `src/systems/tools/orchestrator.rs`

- `handle_tool_action` 新增 `ToolAction::SubmitSkillCandidate` 匹配臂（S7 修复：与
  `SubmitSkillUpdate` 完全对齐，工具只提供参数，验证和候选构造由 orchestrator 执行）：
  1. 从 `SkillCreationContext` 读取 `sandbox_dir`
  2. 验证 `SKILL.md` 存在
  3. 解析 frontmatter 验证 `name` 非空、`description` 非空、`version == 1`
  4. 扫描沙盒目录验证所有文件路径在沙盒内（路径安全）
  5. 验证失败 → spawn `ToolExecutionResultMessage`（含错误信息，LLM 可据此修复重提）
  6. 验证通过 → 扫描沙盒目录生成 `file_refs`
  7. 从 SKILL.md 读取 `instructions`（body 部分）
  8. 构造完整 `ExperienceCandidate::skill_new(...)`（`is_new: true`）
  9. 将候选入队到 `ExperienceStore`
  10. 推入 `PendingExperienceHooks`
  11. 更新 `SkillCreationContext.skill_name`：
      `commands.entity(wi_entity).insert(SkillCreationContext { skill_name: action.name.clone(),
      ..context.clone() })`（C5 修复）
  12. spawn `ToolExecutionResultMessage`（成功信息）
  13. despawn 工具调用请求实体

- `context_queries` 参数类型扩展：
  - 现有：`Query<(Entity, Option<&ProfileGenerationContext>, Option<&SkillUpdateContext>, &WorkItem)>`
  - 新增：`Query<(Entity, Option<&ProfileGenerationContext>, Option<&SkillUpdateContext>, Option<&SkillCreationContext>, &WorkItem)>`
  - `submit_skill` 工具不需要从 `SkillCreationContext` 读取服务端权威字段（name/description
    由 LLM 直接传入），但 orchestrator 需要确认 WorkItem entity 上存在 `SkillCreationContext`
    作为前置条件

### 4.10 ToolContext.current_skill_dir 填充

#### `src/systems/tools/dispatch.rs`（同步派发路径）

- 工具执行前构造 `ToolContext` 时，从 WorkItem entity 上的 Context 组件填充 `current_skill_dir`：
  - `SkillCreationContext.sandbox_dir` → skill-creator 的沙盒目录
  - `SkillUpdateContext` → skill-updater 的 skill 目录（修复已有 gap）
- 需要在 dispatch 函数中增加对 `SkillCreationContext`/`SkillUpdateContext` 的 Query 参数

#### `src/systems/tools/async_dispatch.rs`（异步派发路径）

- 同理，在构造 `OwnedToolContext` 时从 WorkItem entity 的 Context 组件填充 `current_skill_dir`

### 4.11 Schedule 注册

#### `src/plugins/execution.rs`

新增系统注册到 `HarnessSet::Execution`：

- `skill_creation_workitem_system`：
  - 在 `experience_governance_system` 之后（类似 `skill_update_workitem_system` 的位置）
  - 消费 `SkillCreationRequestMessage`，创建 WorkItem

- `skill_creation_writeback_system`：
  - 在 `experience_approval_result_system` 之后（类似 `skill_update_completion_system` 的位置）
  - 消费 `SkillCreationWritebackMessage`，执行 rename 写回

### 4.12 工具注册

#### `src/systems/tools/builtin/mod.rs`

- 新增 `pub use submit_skill::SubmitSkillTool;`
- 新增 `pub use write_skill_file::WriteSkillFileTool;`

#### `src/systems/tools/mod.rs`

- 在工具注册函数中新增：

  ```rust
  executors.register(Box::new(SubmitSkillTool));
  executors.register(Box::new(WriteSkillFileTool));
  ```

## 5. 关键设计细节

### 5.1 沙盒目录约定

路径：`.harness/assets/agents/<agent>/skills/.sandbox/<skill_name>/`

- `.sandbox` 前缀确保 `SkillLoader::load_skills()` 不误加载未完成的 skill
- `SkillLoader` 在 `load_skills()` 的 `filter_map` 中增加 `.starts_with(".")` 过滤

__沙盒目录命名__：`skill_creation_workitem_system` 创建沙盒时使用临时目录名（如
`_draft_<timestamp>`），因为此时 LLM 尚未确定 skill 名称。LLM 通过
`submit_skill(name, description)` 提交时传入最终 `name`。orchestrator 在处理
`ToolAction::SubmitSkillCandidate` 验证通过后，通过 `commands.entity(wi_entity).insert()`
将 `SkillCreationContext.skill_name` 更新为 LLM 提交的名称（C5 修复）。`sandbox_dir`
保持不变（临时名）。最终 rename 时以 `SkillCreationContext.skill_name` 为准，与沙盒目录名
无关。

### 5.2 ExperienceCandidate 的 is_new 字段

在 `ExperienceCandidatePayload::Skill` 中新增 `is_new: bool`：

```rust
pub enum ExperienceCandidatePayload {
    Knowledge { content: String },
    Skill {
        name: String,
        description: String,
        instructions: String,
        file_refs: Vec<SkillFileRef>,
        is_new: bool,  // 新增，默认 false
    },
}
```

所有现有调用点传入 `false`，`submit_skill` 工具传入 `true`。

__Serde 兼容性__：`is_new` 字段必须加 `#[serde(default)]`，否则反序列化已有数据（不含此字段）会失败：

```rust
Skill {
    name: String,
    description: String,
    instructions: String,
    file_refs: Vec<SkillFileRef>,
    #[serde(default)]
    is_new: bool,  // 默认 false，兼容已有序列化数据
}
```

__模式匹配兼容__：`ExperienceCandidatePayload::Skill` 现有约 8+ 处解构模式（`writeback.rs`、
`consolidation.rs`、`skill_update.rs`、`profile_generation.rs`、`approval.rs`、
`orchestrator.rs`、`list_experience_candidates.rs`、`contribution.rs`）。新增 `is_new` 字段后，
所有 `Skill { name, description, ... }` 解构必须使用 `..` 忽略或显式添加 `is_new`。推荐
统一使用 `..` 忽略，减少后续变更影响面。

__构造函数策略__：保留现有 `ExperienceCandidate::skill()` 签名不变（`is_new` 默认 `false`），
新增 `ExperienceCandidate::skill_new()` 构造函数（`is_new` 为 `true`）。避免修改所有现有
调用点。

确认后的治理系统根据 `is_new` 分支：

- `is_new == false` → 走现有 skill-updater 写回路径
- `is_new == true` → 走新建 skill rename 写回路径

### 5.3 格式验证规则

验证由 orchestrator 在处理 `ToolAction::SubmitSkillCandidate` 时执行（S7 修复：与
`submit_skill_update` 的 dry-run 验证同模式），不在 `submit_skill` 工具中执行：

1. __SKILL.md 存在__：`sandbox_dir/SKILL.md` 必须存在
2. __Frontmatter 合规__：解析 frontmatter，`name` 非空、`description` 非空、`version == 1`
   （新建 skill 版本固定为 1，避免 LLM 误传 `version: 2`；C3 修复）
3. __路径安全__：扫描沙盒目录下所有文件，验证 canonical path 在沙盒目录内

验证失败时 orchestrator spawn `ToolExecutionResultMessage`（含 `ToolError` 信息），LLM 可据此修复后重新调用 `submit_skill`。

### 5.4 同名冲突处理

`skill_creation_writeback_system` 在 rename 前检查目标目录是否已存在：

```rust
let target_dir = skills_dir.join(&skill_name);
if target_dir.exists() {
    warn!(event = "SkillNameConflict", ...);
    // 拒绝写回，候选状态保持，提示用户使用 skill-updater
    return;
}
```

### 5.5 file_refs 生成

orchestrator 验证通过后，扫描沙盒目录（递归，排除 `SKILL.md`），为每个文件生成 `SkillFileRef`：

```rust
fn scan_sandbox_files(sandbox_dir: &Path) -> Vec<SkillFileRef> {
    walkdir(sandbox_dir)
        .filter(|f| f != SKILL.md)
        .map(|f| SkillFileRef {
            path: relative_path(f, sandbox_dir),
            role: infer_role(f),  // 按后缀推断：.sh/.py → Script, .md → Reference, else → Asset
        })
        .collect()
}
```

### 5.6 SkillLoader 对 .sandbox 的过滤

`SkillLoader::load_skills()` 和 `SkillLoader::build_registry()` 在遍历 skill 目录时，均跳过 `.` 开头的子目录（S2 修复）：

```rust
// 两个方法共用的过滤逻辑
entries.filter_map(|entry| {
    let path = entry.ok()?.path();
    // 跳过隐藏目录（如 .sandbox）
    if path.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false) {
        return None;
    }
    let skill_md = path.join("SKILL.md");
    // ...
})
```

如果只修改 `load_skills()` 不修改 `build_registry()`，启动时 `build_registry()` 会将 `.sandbox` 下的未完成 skill 注册到 `SkillRegistry`，导致同名冲突检查误判。

### 5.7 current_skill_dir 填充机制

`ToolContext.current_skill_dir` 当前在同步/异步派发路径中始终为 `None`，导致 `ReadSkillFileTool` 无法工作（已有 gap，同样影响 skill-updater）。

修复方案：在 `dispatch.rs` 和 `async_dispatch.rs` 构造 `ToolContext`/`OwnedToolContext` 时，从 WorkItem entity 上的 Context 组件读取：

```rust
// dispatch.rs 中（同步路径）
let current_skill_dir = if let Some(wi_entity) = request.work_item_entity {
    // 优先检查 SkillCreationContext
    if let Ok((_, creation_ctx)) = creation_query.get(wi_entity) {
        Some(creation_ctx.sandbox_dir.clone())
    }
    // 其次检查 SkillUpdateContext
    else if let Ok((_, update_ctx)) = update_query.get(wi_entity) {
        skill_loader.skill_md_path(&update_ctx.skill_id)
            .parent().map(|p| p.to_path_buf())
    } else {
        None
    }
} else {
    None
};
```

此修复同时解决 skill-creator 和 skill-updater 的 `current_skill_dir` 缺失问题。

### 5.8 read_skill_file 与 write_skill_file 的后缀白名单一致性

`ReadSkillFileTool` 使用 `ALLOWED_FILE_SUFFIXES`（`.md`, `.py`, `.sh`, `.toml`, `.txt`,
`.json`）限制可读文件后缀。`WriteSkillFileTool` 设计为无后缀限制（D10 决策：不加后缀白名单）。

潜在问题：skill-creator 写入 `.yaml` 文件后，无法用 `read_skill_file` 读回。

解决方案：`WriteSkillFileTool` 的路径验证使用与 `ReadSkillFileTool` 相同的
`ALLOWED_FILE_SUFFIXES` 白名单。虽然 D10 决策格式验证不加后缀白名单，但工具级限制是
合理的——写入和读取应保持一致。如果 `.yaml` 不在白名单中，skill-creator 既不能写也不能读，
语义一致。若后续需要支持更多后缀，在 `ALLOWED_FILE_SUFFIXES` 中统一添加即可。

## 6. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/domain/command.rs` | 修改 | 新增 `CreateSkill` 变体 + `parse()` 逻辑 |
| `src/domain/contribution.rs` | 修改 | Skill payload 新增 `is_new`，新增 `SkillCreationContext` |
| `src/domain/message.rs` | 修改 | 新增 `SkillCreationRequestMessage`、`SkillCreationWritebackMessage` |
| `src/domain/work_item.rs` | 修改 | 新增 `SkillCreation` 变体 + `required_tag` + `skill_creation()` 工厂 |
| `src/domain/space.rs` | 修改 | `ToolAction` 新增 `SubmitSkillCandidate { name, description }`（非 `execution.rs`） |
| `src/domain/tool_async.rs` | 修改 | `ToolEffect` 新增 `WriteSkillFile` 变体（S7 修复） |
| `src/systems/command.rs` | 修改 | 新增 `CreateSkill` 分支处理 |
| `src/systems/experience/skill_creation.rs` | __新增__ | workitem 创建与 writeback 写回系统 |
| `src/systems/tools/builtin/submit_skill.rs` | __新增__ | `SubmitSkillTool` |
| `src/systems/tools/builtin/write_skill_file.rs` | __新增__ | `WriteSkillFileTool` |
| `src/systems/tools/builtin/mod.rs` | 修改 | 注册新工具 |
| `src/systems/tools/orchestrator.rs` | 修改 | `SubmitSkillCandidate` 匹配臂 + `context_queries` 扩展 |
| `src/systems/tools/dispatch.rs` | 修改 | 从 `SkillCreationContext` 填充 `current_skill_dir` |
| `src/systems/tools/async_dispatch.rs` | 修改 | 同步填充 `current_skill_dir` |
| `src/systems/transform/task_lifecycle.rs` | 修改 | 终态清理沙盒 |
| `src/systems/experience/governance.rs` | 修改 | `is_new == true` 路由到 `SkillCreation` |
| `src/systems/experience/approval.rs` | 修改 | SkillCreation 目标 insert 写回消息到 WorkItem entity |
| `src/infrastructure/skills/loader.rs` | 修改 | `load_skills()` 和 `build_registry()` 均跳过 `.sandbox` |
| `agents.toml` | 修改 | 新增 `skill-creator` Agent 声明 + skill-updater `read_skill_file` 权限修复 |
| `src/plugins/execution.rs` | 修改 | 注册新系统到 `HarnessSet::Execution` |

## 7. 验证方案

### 7.1 单元测试

- `UserCommand::parse("/skill 做代码审查")` → `CreateSkill { intent: "做代码审查" }`
- `UserCommand::parse("/skill")` → `CreateSkill { intent: "" }`
- `submit_skill` 工具验证逻辑（参数校验）：name 为空 → 错误；description 为空 → 错误
- `write_skill_file` 工具验证逻辑：`../` 逃逸 → 错误；正常路径 → 成功写入；`kind()` 返回 `ToolActionKind::Async`（S7 验证）
- `SkillLoader::load_skills()` 和 `SkillLoader::build_registry()` 均跳过 `.sandbox` 目录
- `is_new == true` 的 Skill 候选在 governance system 中路由到 `ExperienceWritebackDestination::SkillCreation`（N3 验证）
- `current_skill_dir` 在 dispatch/async_dispatch 路径从 `SkillCreationContext`/`SkillUpdateContext` 正确填充（4.10 节验证）
- orchestrator 处理 `SubmitSkillCandidate` 时的验证逻辑：缺少 SKILL.md → 错误；frontmatter
  不合规 → 错误；`version != 1` → 错误；路径逃逸 → 错误；验证通过 → 构造完整
  `ExperienceCandidate`（S7 验证）
- `SkillRegistry::upsert()` 后新 skill 可被 `SkillRegistry::get()` 查到（D17 验证）

### 7.2 集成测试

- __完整创建流程__：`/skill intent` → WorkItem 创建 → skill-creator 执行 →
  ExperienceCandidate 生成 → 用户确认 → rename 写回 → SkillRegistry 注册
- __无活跃任务__：`/skill intent` → 报错提示
- __空意图__：`/skill` → 报错提示
- __同名冲突__：已有 skill 同名 → 写回拒绝
- __验证失败重试__：submit_skill 缺少 SKILL.md → 错误反馈 → LLM 修复后重新提交
- __沙盒清理（终态）__：任务完成/失败 → 沙盒目录被删除
- __沙盒清理与候选确认竞态__（S6）：候选在 `NeedsUserApproval` 状态时任务终态 → 沙盒不被清理，用户确认后 writeback 仍可成功
- __沙盒清理（/clear）__：任务 `/clear` → 沙盒目录被删除，候选置 `Discarded`（S4 验证）
- __skill-updater read_skill_file__：skill-updater 调用 `read_skill_file` 成功读取子文件（N2 修复后验证）

### 7.3 手动验证

- 启动应用，执行 `/skill 帮我做代码审查`，观察完整流程
- 在 TUI 确认界面批准/拒绝
- 确认后检查 `.harness/assets/agents/<agent>/skills/<skill>/` 目录存在
- 执行新任务，确认新 skill 被加载到 Agent prompt
