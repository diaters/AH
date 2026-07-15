# Agent 元信息 LLM 生成与动态更新设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效 |
| 创建日期 | 2026-07-13 |
| 适用阶段 | 经验治理模块增强 |
| 相关文档 | `docs/design/2026-06-06-workitem-boundary-design.md`、`docs/current-state.md` |

---

## 1. 背景

当前经验治理模块在孵化新 Agent 时，Agent 的 name、tags、description 均通过非语义化的硬编码生成：

- `name`：`format!("incubated-{}", task_id)`，即 UUID 拼接，不可读
- `tags`：硬编码 `vec!["incubated".to_string()]`，无能力语义
- `description`：由 [`build_incubated_agent_description`](../../src/systems/experience/writeback.rs) 基于候选标题机械拼接

此外，持久型 Agent 在积累经验后，其 tags 和 description 无法随能力演进进行更新，导致 Agent 的元信息与其真实能力逐渐脱节。

### 1.1 目标

- 孵化时由 LLM 基于经验候选生成语义化的 name/tags/description
- 持久型 Agent 积累经验后，自动评估并提议更新 tags/description
- 更新需用户审批，审批时支持“拒绝并反馈”驱动 LLM 重新生成
- name 仅在孵化时确定，后续不可变更
- tags/description 更新后立即对运行中 Agent 生效

### 1.2 非目标

- 工具权限（tools）的 LLM 生成——维持 `None`，后续独立处理
- Agent model 的 LLM 推荐——继续继承 default Agent 的 model
- Agent profile 的删除或退役流程

---

## 2. 设计决策

### 2.1 孵化流程：LLM 生成 profile

#### 决策 1：管线位置

在治理系统（`experience_governance_system`）之后、审批之前插入新的 profile generation WorkItem 阶段。

治理系统发现需要孵化时，不再直接构造 `AgentProfile` 并创建 proposal + confirmation，而是：

1. 将候选标记为新状态 `ProfileGenerationPending`
2. Spawn 一个 profile generation WorkItem
3. WorkItem 完成后，用 LLM 返回的 profile 创建 proposal 并发起用户审批

#### 决策 2：专用 profile-designer Agent

新增 `profile-designer` Agent，在 `agents.toml` 中预配置，类似 `collector`、`evaluator` 等角色 Agent。
其 system prompt 专门优化为"根据经验候选生成 Agent 角色名、核心能力标签和职责描述"。

同时服务孵化（生成新 profile）和更新（评估 + 生成更新后 profile）两个场景。

#### 决策 3：审批支持“拒绝并反馈”

审批界面展示 LLM 生成的 profile（name/tags/description），用户可以：

- __批准__：直接批准 LLM 生成的 profile
- __拒绝__：终止孵化/更新流程
- __拒绝并反馈__：提供评审建议，反馈注入 LLM 上下文，驱动重新生成，进入新一轮审批

"拒绝并反馈"是一个通用机制，不针对 profile 审批定制 UI。用户只需输入文本反馈，由 LLM 理解反馈后重新生成。
`reject_with_feedback` 不占用异常计数，且不再有上限约束，用户可多次反馈直到满意。LLM 异常的计数与失败路径详见 3.7 节与 4.3 节。

### 2.2 更新流程：已有 Agent 的 tags/description 演进

#### 决策 4：触发时机

每次持久型 Agent 的经验写入 LTM 或 SkillPackage 成功后，额外 spawn 一个 profile 更新评估 WorkItem。

两条写回路径对称触发，统一由 `profile-designer` Agent 执行评估。

__频率控制__：为避免高频写回导致过多 LLM 调用，代码中预留冷却期接口（`ProfileUpdateCooldown` Resource），默认不启用。后续可通过配置启用，如同一 Agent 在 N 分钟内最多触发一次更新评估。首次实现先每次触发，但接口已就位。

#### 决策 5：tags 更新语义

整体替换。profile-designer 读取现有 profile + 新经验后，生成全新的 tags 列表和 description。

与"增量输入，全量输出"原则一致：输入只需新增条目，但输出是完整的最终状态。

#### 决策 6：两个专用工具

引入两个 LLM 工具，仅限 `profile-designer` Agent 使用：

- `submit_profile_update`：提交更新后的 profile（携带 name/tags/description）
- `skip_profile_update`：明确表示不需要更新

系统通过工具调用本身区分两种结果，不依赖"LLM 未调用工具"的猜测。

#### 决策 7：立即生效与原子性

profile 更新被用户批准后，采用__先文件后 ECS__ 的两阶段提交：

1. __第一阶段__：写入 `agents.toml`（修改已有条目，非追加）。若写入失败，终止流程并标记候选为 `WritebackFailed`，不影响 ECS 状态。
2. __第二阶段__：文件写入成功后，通过 `Commands::entity(...).insert()` 更新 ECS 中对应 Agent 实体的 `AgentCapabilities` 组件。
   若 ECS 更新失败（实体不存在等），记录 `warn!` 告警并标记为不一致状态——文件已是最新值，下次重启后自动恢复一致。

Brain 调度时立即使用新的 tags。短暂不一致（文件已更新但 ECS 未更新）只会导致当前帧使用旧 tags，影响可忽略。

#### 决策 8：LLM 上下文范围

更新评估时，LLM 仅获取：

- 刚写入的新增 LTM/Skill 条目（title + content/payload）
- Agent 当前的 name/tags/description

不传入全部历史 LTM 条目，控制 token 开销。判断逻辑为"这条新经验是否带来了现有 tags/description 未覆盖的新能力或职责变化"。

### 2.3 安全与一致性

#### 决策 9：name 唯一性双重保障

1. __Prompt 注入__：profile generation WorkItem 的 prompt 中注入现有 Agent name 列表，指示 LLM 不要使用已存在的名字。
   当前 Agent 数量通常为个位数，全部注入不影响 token 预算；若未来 Agent 数量增长，可限制为最近 N 个或按 tag 分组注入。
2. __写回兜底__：`writeback_incubation_proposal` 中检查 LLM 生成的 name 是否已存在，重名时自动追加数字后缀（如 `physics-specialist-2`）。
   需新增 `append_or_rename` 方法（见 3.6），现有 `append` 方法的静默跳过行为不满足需求。

#### 决策 10：受保护标签集合

系统标签 `{"incubated", "default"}` 受保护：

- __`incubated`__：孵化时系统自动注入到 LLM 生成的 tags 中；profile 更新时，即使 LLM 全量替换的输出中不包含 `incubated`，系统也会自动补回
- __`default`__：LLM 不得生成此标签；写回时从 LLM 输出中过滤；现有 Agent 的 `default` 状态在更新时不可被改变（有则保留，无则不可添加）

实现为统一的 `protected_tags` 集合，在写入前统一过滤。

---

## 3. 架构变更

### 3.1 新增领域类型

```rust
/// profile 生成请求消息：治理系统产出，驱动 profile generation WorkItem。
#[derive(Debug, Clone, Component)]
pub struct ProfileGenerationRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
    /// 孵化场景为 None，更新场景为 Some(现有完整 profile)。
    pub existing_profile: Option<ExistingAgentProfile>,
    /// 标识是孵化还是更新。
    pub kind: ProfileGenerationKind,
    /// 用户拒绝时的评审反馈，用于 LLM 重新生成。None 表示首次生成。
    pub feedback: Option<String>,
    /// 异常计数器，仅累计 LLM 异常（未调工具 / 互斥冲突 / Err / 调用非相关工具）。
    /// LLM 成功调工具后由 orchestrator 归 0。reject_with_feedback 不占用计数。
    pub exception_count: u32,
}

const MAX_PROFILE_EXCEPTIONS: u32 = 3;

/// profile 生成场景。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileGenerationKind {
    Incubation,
    Update,
}

/// 现有 Agent 的完整 profile（用于更新场景的 LLM 上下文）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingAgentProfile {
    pub name: String,
    pub tags: Vec<String>,
    pub description: String,
}

/// profile 生成完成消息：WorkItem 完成后产出。
#[derive(Debug, Clone, Component)]
pub struct ProfileGenerationCompletedMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    /// LLM 生成的 profile；None 表示 skip_profile_update（不需要更新）。
    pub generated_profile: Option<GeneratedProfile>,
    pub kind: ProfileGenerationKind,
}

/// LLM 生成的 profile 内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedProfile {
    pub name: String,
    pub tags: Vec<String>,
    pub description: String,
}
```

### 3.2 新增 LLM 工具

#### submit_profile_update

```json
{
  "name": "submit_profile_update",
  "description": "提交生成或更新后的 Agent profile。孵化场景 name 作为最终 Agent 名称；更新场景 name 仅作参考，系统会强制使用原 name（不可变更）。",
  "parameters": {
    "type": "object",
    "properties": {
      "name": {
        "type": "string",
        "description": "Agent 角色名，简洁有力，如 'physics-specialist'"
      },
      "tags": {
        "type": "array",
        "items": { "type": "string" },
        "description": "核心能力标签列表，如 ['physics', 'calculation']"
      },
      "description": {
        "type": "string",
        "description": "Agent 职责描述，一到两句话概括"
      }
    },
    "required": ["name", "tags", "description"]
  }
}
```

#### skip_profile_update

```json
{
  "name": "skip_profile_update",
  "description": "明确表示现有 Agent profile 不需要更新。",
  "parameters": {
    "type": "object",
    "properties": {}
  }
}
```

__工具约束__：

- 两个工具互斥，LLM 同时调用 `submit_profile_update` 和 `skip_profile_update` 触发重试（`exception_count +1`）
- ProfileGeneration WorkItem 只允许调用 `submit_profile_update` 或 `skip_profile_update`，调用其他工具即违规，触发重试（`exception_count +1`）

### 3.3 新增 profile-designer Agent 配置

在 `agents.toml`（及 `agents.toml.example`）中新增：

```toml
[[agent]]
name = "profile-designer"
tags = ["profile", "design"]
description = "Agent 元信息设计师，负责根据经验候选生成 Agent 角色名、能力标签和职责描述"

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"

[agent.tools]
default_permission = "Deny"
submit_profile_update = "Allow"
skip_profile_update = "Allow"
```

__孵化 Agent 的 model 继承__：孵化时继承 default Agent 的完整 `models` 链（而非单 `model` 字符串），确保孵化出的 Agent 具备多模型降级能力。
`IncubatedAgentRecord` 需扩展为携带 `models: Vec<ModelChainEntry>`，写回时同时写入 `models` 链。
若 default Agent 使用单 `model` 字段（向后兼容），则自动转换为单元素 `models` 链。

### 3.4 管线变更

#### 孵化流程（修改后）

```text
task_terminated_experience_trigger_system
    ↓
experience_collection_workitem_system
    ↓
experience_collection_completion_system
    ↓
experience_governance_system
    ↓ (IncubationProposal 路径)
profile_generation_workitem_system        ← 新增
    ↓
profile_generation_completion_system     ← 新增
    ↓ (用 LLM profile 创建 proposal + 发起审批)
experience_approval_result_system
    ↓
experience_writeback_system
```

#### 更新流程（新增）

```text
experience_writeback_system
    ↓ (LTM/SkillPackage 写回成功后)
profile_update_trigger_system             ← 新增
    ↓
profile_generation_workitem_system        ← 复用（kind=Update）
    ↓
profile_generation_completion_system      ← 复用
    ↓ (需要更新时发起审批)
experience_approval_result_system         ← 复用
    ↓
profile_update_writeback_system           ← 新增（更新 agents.toml + ECS 组件）
```

### 3.5 经验候选状态扩展

在 `ExperienceCandidateStatus` 中新增：

```rust
pub enum ExperienceCandidateStatus {
    // ... 现有状态 ...
    ProfileGenerationPending,  // 等待 profile generation WorkItem 完成
}
```

### 3.6 IncubatedAgentRegistry 扩展

新增 `update` 方法，用于修改已有 Agent 条目：

```rust
impl IncubatedAgentRegistry {
    /// 追加新 Agent（现有方法，不变）
    pub fn append(&self, config_path: &str, record: &IncubatedAgentRecord) -> Result<()>;

    /// 追加新 Agent，重名时自动追加数字后缀。
    /// 与 append 的区别：append 发现重名静默跳过，
    /// append_or_rename 会修改 name 后重试。
    pub fn append_or_rename(
        &self,
        config_path: &str,
        record: &mut IncubatedAgentRecord,
    ) -> Result<()>;

    /// 更新已有 Agent 的 tags 和 description
    pub fn update(
        &self,
        config_path: &str,
        agent_name: &str,
        new_tags: &[String],
        new_description: &str,
    ) -> Result<()>;
}
```

- `append_or_rename`：读取 `agents.toml` 检查 name 是否已存在。若重名，追加 `-2`、`-3` 后缀直到唯一，然后写入。record 的 name 字段会被就地修改为最终写入的 name。
- `update` 方法读取 `agents.toml`，按 name 查找条目，替换 tags 和 description，原子写回。model、models、tools、skills 等其他字段不变。
- `IncubatedAgentRecord` 扩展：新增 `models: Vec<ModelChainEntry>` 字段，与现有 `model` 字段并存。
  写回时优先写入 `models` 链；若 `models` 为空则回退到 `model` 字符串。

### 3.7 审批“拒绝并反馈”机制

当前审批仅支持“批准/拒绝”二元选择，拒绝后候选直接终止。设计中将“拒绝”升级为**“拒绝并反馈”**——用户可附带评审建议，反馈被注入 LLM 上下文，驱动重新生成。

#### 设计原则

- __通用机制__：不针对 profile 审批定制 UI，任何审批均可选择“拒绝并反馈”
- __LLM 重新思考__：用户不直接编辑字段，而是提供文本反馈，由 LLM 理解反馈后重新生成
- __`ConfirmationOption` 不变__：现有 struct 无需修改，通过新增标准选项 `id = "reject_with_feedback"` 实现

#### 数据流

```text
用户选择 “拒绝并反馈”
    ↓
TUI 进入文本输入模式（复用现有 InputBar）
    ↓
用户输入评审反馈，按 Enter 提交
    ↓
UserAction::Confirmation {
    option_id: "reject_with_feedback",
    feedback: Some("name 太笼统，建议改为 quantum-physics；tags 缺少 'simulation'")
}
    ↓
ToolConfirmationResponseMessage 携带 feedback 文本
    ↓
experience_approval_result_system 检测到 reject_with_feedback
    ↓
重新 spawn ProfileGenerationRequestMessage {
    feedback: Some("..."),
    exception_count: prev,  // reject_with_feedback 不占用计数
}
    ↓
profile generation WorkItem prompt 包含：
  - 上一次生成的 profile
  - 用户反馈
  - 指令：“根据用户反馈重新生成”
    ↓
LLM 重新生成 → 新一轮审批
```

#### 类型扩展

需要扩展从商端到系统侧的完整反馈携带链路：

```rust
// src/domain/frontend.rs — TUI 路径
pub enum UserAction {
    Text { channel: ChannelId, content: String },
    Confirmation {
        channel: ChannelId,
        request_id: Uuid,
        option_id: String,
        /// 用户审批反馈文本（选择“拒绝并反馈”时携带）
        feedback: Option<String>,
    },
}

// src/channels/traits.rs — IM 通道入向路径
pub struct InboundConfirmation {
    pub request_id: Uuid,
    pub option: String,
    pub label: Option<String>,
    /// 用户反馈文本（拒绝并反馈时携带）
    pub feedback: Option<String>,
}

// src/channels/traits.rs — ExternalInput 也需扩展
pub enum ExternalInput {
    TextWithChannel { channel: ChannelId, content: String },
    Confirmation {
        request_id: Uuid,
        option: String,
        /// 用户反馈文本（拒绝并反馈时携带）
        feedback: Option<String>,
    },
}

// src/domain/message.rs — 系统侧响应消息
pub struct ToolConfirmationResponseMessage {
    pub request_id: Uuid,
    pub selected_option: String,
    /// 用户反馈文本（拒绝并反馈时携带）
    pub feedback: Option<String>,
}
```

`ChannelInboundMessage::to_external_input()` 同步传递 `feedback` 字段。现有审批流程中 `feedback` 默认为 `None`，不影响已有逻辑。

#### TUI 交互

profile 审批的选项列表：

```text
┌─────────────────────────────────────────┐
│  REVIEW  submit_profile_update          │
│  from: profile-designer                 │
│                                         │
│  {                                      │
│    "name": "physics-specialist",        │
│    "tags": ["physics", "calculation"],  │
│    "description": "负责物理计算..."      │
│  }                                      │
│                                         │
│  ◉ Approve           ← 直接批准           │
│  ○ Reject            ← 直接拒绝           │
│  ○ Reject & Feedback ← 拒绝并提供反馈      │
│                                         │
│  ↑↓ 选择  Enter 确认  Esc 跳过            │
└─────────────────────────────────────────┘
```

选择 “Reject & Feedback” 后，TUI 切换到文本输入模式：

```text
┌─────────────────────────────────────────┐
│  FEEDBACK                               │
│  请输入评审建议，LLM 将根据反馈重新生成：    │
│                                         │
│  > name 太笼统，建议改为 quantum-physics_|
│                                         │
│  Enter 提交  Esc 取消                    │
└─────────────────────────────────────────┘
```

复用现有 `InputBar` 的字符输入逻辑，无需多行编辑器。

#### IM 通道交互（Telegram / QQ）

IM 通道的审批当前是“点击=立即确认”模式（Telegram Inline Keyboard / QQ 编号回复），不支持点击后再输入文本。需要引入__两步交互__：

__步骤 1__：用户选择“Reject & Feedback”选项

- Telegram：点击 `Reject & Feedback` 按钮，`callback_data = "{request_id}:reject_with_feedback"`
- QQ：回复数字编号（如 `3`）或选项名称

__步骤 2__：通道进入“等待反馈”状态，提示用户输入

```text
📝 请输入评审建议：

你的反馈将发送给 LLM，用于重新生成 Agent profile。
直接发送文本消息即可。
```

通道侧新增 `pending_feedback` 状态（类似现有 `pending_approvals`）：

```rust
struct PendingFeedback {
    request_id: Uuid,
    recipient: String,
    created_at: u64,
}

pending_feedback: Arc<RwLock<HashMap<String, PendingFeedback>>>,
// key 为 recipient（如 "user:12345" 或 "group:67890"）
```

__步骤 3__：用户发送文本消息，通道捕获为 feedback

- 通道检测到 `pending_feedback` 中有该 recipient 的记录
- 将用户发送的文本作为 `feedback`，与 `request_id` 和 `option = "reject_with_feedback"`
  组装为 `InboundConfirmation { feedback: Some(text) }`
- 清除 `pending_feedback` 记录
- 如果用户发送 `/cancel` 或超时（如 5 分钟），取消反馈，视为普通拒绝

__QQ 通道简化方案__：QQ 已支持文本回复匹配，用户可在选择“拒绝并反馈”后直接在同一消息中追加反馈文本，格式为 `3 反馈内容`（编号 + 空格 + 反馈）。通道解析后直接携带 feedback，无需两步交互。

#### 异常计数器

`MAX_PROFILE_EXCEPTIONS = 3`。异常计数器（`exception_count`）仅累计 LLM 异常（未调工具 / 互斥冲突 / Err / 调用非相关工具），达到上限后走失败路径（详见 4.3 节）。

- LLM 成功调用工具后，由 orchestrator 将 `exception_count` 归 0
- `reject_with_feedback` 透传不变，不占用 `exception_count`，且不再有上限约束
- 选项列表始终保留 “Reject & Feedback”，用户可多次反馈直到满意

#### 审批路由

profile 审批复用现有 `ToolConfirmationRequestMessage` 消息类型和审批流。与经验治理现有审批一致：

- `approval_context` 字段填充描述信息（如 `"Agent profile generation for task {task_id}"`）
- 审批消息通过现有 `EngineEvent::ApprovalRequest` 派发，TUI 和 IM 通道均可见
- 不需要额外的路由机制，与现有经验审批路径完全一致

### 3.8 受保护标签过滤

在写回前统一执行标签过滤：

```rust
const PROTECTED_TAGS: &[&str] = &["incubated", "default"];

/// 过滤 LLM 输出中的受保护标签，并补回系统标签。
fn sanitize_tags(
    llm_tags: Vec<String>,
    existing_tags: &[String],
) -> Vec<String> {
    let mut result: Vec<String> = llm_tags
        .into_iter()
        .filter(|t| !PROTECTED_TAGS.contains(&t.as_str()))
        .collect();

    // 补回系统标签
    for tag in existing_tags {
        if PROTECTED_TAGS.contains(&tag.as_str()) && !result.contains(tag) {
            result.push(tag.clone());
        }
    }

    // 去重
    result.sort_unstable();
    result.dedup();

    result
}
```

孵化场景：`existing_tags` 为空，`incubated` 由系统在写回时注入。
更新场景：`existing_tags` 为 Agent 当前 tags，受保护标签从 existing 中保留。

---

## 4. 系统编排

### 4.1 新增系统注册

在 `execution.rs` 的 `HarnessSet::Execution` 中注册新系统：

```rust
// profile generation：孵化场景
profile_generation_workitem_system
    .in_set(HarnessSet::Execution)
    .after(experience_governance_system),

// profile generation 完成处理
profile_generation_completion_system
    .in_set(HarnessSet::Execution)
    .after(crate::systems::llm_response_system)
    .before(experience_approval_result_system),

// profile 更新触发：写回后触发
profile_update_trigger_system
    .in_set(HarnessSet::Execution)
    .after(experience_writeback_system),

// profile 更新写回
profile_update_writeback_system
    .in_set(HarnessSet::Execution)
    .after(experience_approval_result_system),
```

### 4.2 profile generation WorkItem

WorkItem 构建模式与经验收集 WorkItem 一致：

- 只暴露 `submit_profile_update` 和 `skip_profile_update` 两个工具
- Prompt 包含所有候选的 title + payload
- 孵化场景：prompt 中额外注入现有 Agent name 列表（避免重名）
- 更新场景：prompt 中注入当前 profile（name/tags/description）+ 新增经验条目
- __重试场景__（`feedback` 存在时）：prompt 中额外注入上一次生成的 profile + 用户反馈文本 + 指令“根据用户反馈重新生成 profile”

### 4.3 错误处理

#### 异常重试

以下场景视为 LLM 异常，触发自动重试（`exception_count +1`）：

- LLM 未调用任何工具
- LLM 同时调用 `submit_profile_update` 和 `skip_profile_update`（互斥冲突）
- LLM 调用工具返回 `Err`（含字段校验失败，如 name 为空、tags 为空）
- LLM 调用了非相关工具（ProfileGeneration WorkItem 只允许调用 `submit_profile_update` 或 `skip_profile_update`，调用其他工具即违规）

LLM 成功调用工具后，由 orchestrator 将 `exception_count` 归 0。

#### 失败路径

`exception_count` 达到 `MAX_PROFILE_EXCEPTIONS(3)` 后走失败路径：

- __孵化场景__：候选标记为 `ProfileGenerationFailed`，清理 LLM 上下文，删除 proposal，
  触发 `OnAgentProfileGenerationFailed` hook，通过 `SystemOutputMessage` 通知用户
- __更新场景__：静默跳过（保持现有 profile 不变），清理 LLM 上下文

#### profile-designer 缺失

`profile-designer` Agent 缺失时直接失败（不再回退）：启动时记录 `warn!` 日志，运行时记录 `error!` 日志，并通过 `SystemOutputMessage` 通知用户检查配置。

#### 正常路径

| 场景 | 处理策略 |
|------|----------|
| `skip_profile_update`（更新场景） | 正常路径，不是错误——表示不需要更新 |
| `reject_with_feedback` | 透传不变，不占用 `exception_count`，且不再有上限约束 |

### 4.4 插件 Hook

新增以下 hook 事件，复用现有 `experience_hook.rs` 的派发机制：

| Hook 名称 | 触发时机 | 参数 |
|-----------|----------|------|
| `on_agent_profile_generated` | profile generation WorkItem 完成 | `agent_name`, `tags`, `description`, `kind` |
| `on_agent_profile_generation_failed` | profile generation 达到异常上限失败 | `agent_id`, `kind` |
| `on_agent_profile_updated` | profile 更新写回成功（用户审批后） | `agent_name`, `old_tags`, `new_tags`, `old_desc`, `new_desc` |
| `on_agent_incubated` | 孵化 Agent 写入 agents.toml 成功 | `agent_name`, `tags`, `description` |

`on_agent_profile_generated` 在 LLM 生成后、用户审批前触发，允许插件观察但不阻止。
`on_agent_profile_generation_failed` 在异常计数达到上限后触发，用于通知插件 profile 生成失败。
`on_agent_profile_updated` 和 `on_agent_incubated` 在写回成功后触发。

---

## 5. 影响范围

### 5.1 修改文件

| 文件 | 变更内容 |
|------|----------|
| `src/domain/contribution.rs` | 新增 profile generation 相关类型；`ExperienceCandidateStatus` 新增 `ProfileGenerationPending` |
| `src/domain/agent.rs` | `AgentProfile` 可选扩展（或复用现有） |
| `src/systems/experience/governance.rs` | `spawn_incubation_confirmation` 改为 spawn profile generation request |
| `src/systems/experience/writeback.rs` | `writeback_incubation_proposal` 使用 LLM 生成的 profile；写回时 name 唯一性兜底 |
| `src/systems/experience/mod.rs` | 新增 `profile_generation` 模块 |
| `src/systems/experience/profile_generation.rs` | 新文件：WorkItem 创建 + 完成处理系统 |
| `src/systems/experience/profile_update.rs` | 新文件：更新触发 + 更新写回系统 |
| `src/infrastructure/incubation/agent_registry.rs` | 新增 `append_or_rename`、`update`；`IncubatedAgentRecord` 加 `models` |
| `src/plugins/execution.rs` | 注册新系统 |
| `src/domain/tool_*` | 新增 `submit_profile_update` / `skip_profile_update` 工具定义与处理 |
| `src/domain/frontend.rs` | `UserAction::Confirmation` 新增 `feedback: Option<String>` 字段 |
| `src/domain/message.rs` | `ToolConfirmationResponseMessage` 新增 `feedback: Option<String>` 字段 |
| `src/channels/traits.rs` | `InboundConfirmation`/`ExternalInput::Confirmation` 加 `feedback`；`to_external_input()` 传递 |
| `src/channels/telegram.rs` | 新增 `pending_feedback` 状态；两步交互（点击按钮 → 提示输入 → 捕获文本） |
| `src/channels/qq.rs` | `try_match_approval_reply` 支持 `编号 + 反馈文本` 格式解析 |
| `src/channels/frontend.rs` | `ApprovalRequest` 出向消息包含 `Reject & Feedback` 选项 |
| `src/systems/experience/experience_hook.rs` | 新增 profile 相关 hook 派发 |
| `src/systems/experience/approval.rs` | 检测 `reject_with_feedback`，携带 feedback 重 spawn 生成请求 |
| `src/tui/` | 审批选项新增“Reject & Feedback”，选择后进入文本输入模式 |
| `agents.toml.example` | 新增 `profile-designer` Agent 配置 |

### 5.2 不受影响

- 经验收集流程（`collection.rs`）不变
- 经验合并流程（`consolidation.rs`）不变
- LTM 写回逻辑（`writeback_to_long_term_memory`）不变
- SkillPackage 写回逻辑（`writeback_to_skill_package`）不变
- 经验 hook 派发（`experience_hook.rs`）的现有 hook 不变，新增 profile 相关 hook（见 4.4）
- `ConfirmationOption`（`src/domain/confirmation.rs`）不变，无需新增字段

---

## 6. 测试计划

### 6.1 单元测试

- `sanitize_tags`：受保护标签过滤、补回与去重逻辑
- `IncubatedAgentRegistry::append_or_rename`：重名追加后缀、不重名直接写入
- `IncubatedAgentRegistry::update`：修改已有条目、不存在的 name 返回错误
- `ProfileGenerationKind`：孵化与更新场景的区分
- profile 字段校验：name 为空（孵化场景）、tags 为空时返回错误
- 异常计数器逻辑：`exception_count` 达到 `MAX_PROFILE_EXCEPTIONS` 后走失败路径；LLM 成功调工具后归 0

### 6.2 集成测试

- __孵化端到端__：default Agent 经验 → 收集 → 治理 → profile generation → 审批 → 写回，
  验证 agents.toml 中新增条目的 name/tags/description 为 LLM 生成值，且包含 `models` 链
- __name 唯一性兜底__：LLM 生成重名时 `append_or_rename` 自动追加后缀
- __受保护标签__：LLM 输出含 `default` 时被过滤；孵化结果包含 `incubated`；LLM 输出重复标签时被去重
- __更新评估-需要更新__：持久型 Agent LTM 写回 → profile 更新评估 → LLM 提议更新 → 审批 → 验证 ECS 组件和 agents.toml 同步更新
- __更新评估-不需要更新__：LLM 调用 `skip_profile_update` → 静默结束，无审批请求
- __拒绝并反馈__：用户拒绝并提供反馈 → LLM 根据反馈重新生成 → 验证新 profile 反映了反馈内容 → 第二轮审批通过
- __reject_with_feedback 不限次__：连续多次拒绝并反馈，选项列表始终包含 “Reject & Feedback”，`exception_count` 不增加
- __IM 通道拒绝并反馈-Telegram__：用户点击 “Reject & Feedback” 按钮 → Bot 提示输入 → 用户发送文本 → 验证 `InboundConfirmation` 携带 feedback → LLM 重新生成
- __IM 通道拒绝并反馈-QQ__：用户回复 `3 name 太笼统` → 验证 feedback 被正确解析 → LLM 重新生成
- __IM 通道反馈取消__：用户点击 “Reject & Feedback” 后发送 `/cancel` → 验证视为普通拒绝
- __name 不可更新__：更新场景下 LLM 生成的 name 被系统忽略，写回时使用原 name
- __孵化失败重试→删除__：LLM 未调工具达到 `MAX_PROFILE_EXCEPTIONS` → 候选标记
  `ProfileGenerationFailed` + 清理 context + 删除 proposal + 触发 hook + 通知用户
- __LLM 没调工具重试成功__：首次未调工具（`exception_count +1`）→ 第二次成功调工具 → `exception_count` 归 0 → 进入审批
- __LLM 没调工具重试失败__：连续 3 次未调工具 → 走失败路径
- __互斥冲突重试__：LLM 同时调用两个工具 → `exception_count +1` → 重试
- __exception_count 归 0__：LLM 异常后成功调工具 → 验证 `exception_count` 归 0
- __profile-designer 缺失失败__：配置中无 profile-designer → 启动 warn + 运行时 error + 通知用户
- __更新原子性__：agents.toml 写入成功但 ECS 更新失败 → 记录告警日志，下次启动恢复一致

---

## 7. 开放问题

- profile-designer Agent 的 model 配置：当前建议使用与 collector 相同的 model，后续可根据实际生成质量调整
- profile 更新评估的频率优化：当前每次 LTM/Skill 写回都触发，代码中已预留 `ProfileUpdateCooldown` 接口，后续可通过配置启用冷却期
- “拒绝并反馈”在其他审批场景（如工具执行、经验候选）中的推广：当前仅 profile 审批启用，后续可推广为通用机制
