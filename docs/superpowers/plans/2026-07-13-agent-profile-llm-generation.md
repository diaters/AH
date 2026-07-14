# Agent 元信息 LLM 生成与动态更新 实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 将经验治理模块中 Agent 元信息（name/tags/description）的生成从硬编码改为 LLM 驱动，支持孵化时生成语义化 profile 和持久型 Agent 的 profile 动态更新，审批时支持"拒绝并反馈"驱动 LLM 重新生成。

**架构：** 在治理系统后、审批前插入 profile generation WorkItem 阶段，由专用 profile-designer Agent 通过两个 LLM 工具（submit_profile_update / skip_profile_update）生成或跳过 profile。审批支持"拒绝并反馈"机制，用户反馈注入 LLM 上下文驱动重新生成，设有重试上限。更新流程在 LTM/SkillPackage 写回后触发评估，审批通过后先写文件再更新 ECS 组件。

**技术栈：** Rust + Bevy ECS + ratatui TUI + genai LLM

**设计文档：** `docs/design/2026-07-13-agent-profile-llm-generation-design.md`

---

## 文件结构

### 新建文件

| 文件 | 职责 |
|------|------|
| `src/systems/experience/profile_generation.rs` | profile generation WorkItem 创建 + 完成处理系统 |
| `src/systems/experience/profile_update.rs` | profile 更新触发 + 更新写回系统 |
| `src/systems/tools/builtin/submit_profile_update.rs` | submit_profile_update 工具执行器 |
| `src/systems/tools/builtin/skip_profile_update.rs` | skip_profile_update 工具执行器 |
| `tests/profile_generation_flow.rs` | 孵化端到端集成测试 |
| `tests/profile_update_flow.rs` | 更新流程集成测试 |
| `tests/profile_reject_feedback_flow.rs` | 拒绝并反馈集成测试 |

### 修改文件

| 文件 | 变更 |
|------|------|
| `src/domain/contribution.rs` | 新增领域类型 + ExperienceCandidateStatus 扩展 |
| `src/domain/message.rs` | ToolConfirmationResponseMessage 新增 feedback |
| `src/domain/frontend.rs` | UserAction::Confirmation 新增 feedback |
| `src/channels/traits.rs` | InboundConfirmation + ExternalInput::Confirmation 新增 feedback |
| `src/systems/experience/mod.rs` | 新增模块声明 |
| `src/systems/experience/governance.rs` | 孵化路径改为 spawn profile generation request |
| `src/systems/experience/approval.rs` | 检测 reject_with_feedback，重新 spawn 生成请求 |
| `src/systems/experience/writeback.rs` | 使用 LLM profile + append_or_rename |
| `src/systems/experience/experience_hook.rs` | 新增 3 个 hook 派发 |
| `src/systems/tools/mod.rs` | 注册两个新工具 |
| `src/systems/tools/builtin/mod.rs` | 新增模块声明 |
| `src/infrastructure/incubation/agent_registry.rs` | append_or_rename + update + models 字段 |
| `src/plugins/execution.rs` | 注册新系统 |
| `src/tui/app.rs` | AppMode 新增 Feedback，处理反馈输入 |
| `src/tui/chat.rs` | 审批卡片渲染 Reject & Feedback 选项 |
| `src/tui/status.rs` | Feedback 模式快捷键提示 |
| `src/channels/telegram.rs` | pending_feedback 状态 + 两步交互 |
| `src/channels/qq.rs` | try_match_approval_reply 支持反馈解析 |
| `src/channels/frontend.rs` | ApprovalRequest 出向含 Reject & Feedback |
| `agents.toml.example` | 新增 profile-designer Agent |

---

## 任务 1：领域类型与 sanitize_tags

**文件：**
- 修改：`src/domain/contribution.rs`
- 测试：`src/domain/contribution.rs`（`#[cfg(test)]`）

- [ ] **步骤 1：编写 sanitize_tags 单元测试**

在 `src/domain/contribution.rs` 的 `#[cfg(test)]` 模块中添加：

```rust
#[test]
fn sanitize_tags_filters_protected_and_deduplicates() {
    use super::sanitize_tags;
    // LLM 输出包含受保护标签和重复标签
    let llm_tags = vec![
        "physics".to_string(),
        "default".to_string(),
        "incubated".to_string(),
        "physics".to_string(),
        "calculation".to_string(),
    ];
    let existing_tags = vec!["incubated".to_string()];

    let result = sanitize_tags(llm_tags, &existing_tags);

    // 受保护标签从 LLM 输出中过滤
    assert!(!result.contains(&"default".to_string()));
    // incubated 从 existing 中补回
    assert!(result.contains(&"incubated".to_string()));
    // 去重
    assert_eq!(result.iter().filter(|t| t == &"physics").count(), 1);
    // 保留非保护标签
    assert!(result.contains(&"calculation".to_string()));
}

#[test]
fn sanitize_tags_empty_existing_for_incubation() {
    use super::sanitize_tags;
    let llm_tags = vec!["physics".to_string(), "calculation".to_string()];
    let result = sanitize_tags(llm_tags, &[]);
    assert!(result.contains(&"physics".to_string()));
    assert!(result.contains(&"calculation".to_string()));
    assert!(!result.contains(&"incubated".to_string()));
    // incubated 由写回逻辑注入，不在 sanitize_tags 中
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib sanitize_tags`
预期：FAIL，函数未定义

- [ ] **步骤 3：实现 sanitize_tags 和领域类型**

在 `src/domain/contribution.rs` 中添加：

```rust
use serde::{Deserialize, Serialize};

/// profile 生成请求消息。
#[derive(Debug, Clone, Component)]
pub struct ProfileGenerationRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
    pub existing_profile: Option<ExistingAgentProfile>,
    pub kind: ProfileGenerationKind,
    pub feedback: Option<String>,
    pub retry_count: u32,
}

pub const MAX_PROFILE_GENERATION_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileGenerationKind {
    Incubation,
    Update,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingAgentProfile {
    pub name: String,
    pub tags: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Component)]
pub struct ProfileGenerationCompletedMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub generated_profile: Option<GeneratedProfile>,
    pub kind: ProfileGenerationKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedProfile {
    pub name: String,
    pub tags: Vec<String>,
    pub description: String,
}

const PROTECTED_TAGS: &[&str] = &["incubated", "default"];

pub fn sanitize_tags(llm_tags: Vec<String>, existing_tags: &[String]) -> Vec<String> {
    let mut result: Vec<String> = llm_tags
        .into_iter()
        .filter(|t| !PROTECTED_TAGS.contains(&t.as_str()))
        .collect();

    for tag in existing_tags {
        if PROTECTED_TAGS.contains(&tag.as_str()) && !result.contains(tag) {
            result.push(tag.clone());
        }
    }

    result.sort_unstable();
    result.dedup();

    result
}
```

在 `ExperienceCandidateStatus` 枚举中添加 `ProfileGenerationPending` 变体。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib sanitize_tags`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/domain/contribution.rs
git commit -m "feat(domain): add profile generation types and sanitize_tags"
```

---

## 任务 2：Feedback 字段链路扩展

**文件：**
- 修改：`src/domain/frontend.rs`、`src/domain/message.rs`、`src/channels/traits.rs`
- 测试：`src/channels/traits.rs`（`#[cfg(test)]`）

- [ ] **步骤 1：编写 to_external_input 传递 feedback 的测试**

在 `src/channels/traits.rs` 的测试模块中添加：

```rust
#[test]
fn to_external_input_propagates_feedback() {
    let msg = ChannelInboundMessage {
        channel_name: "telegram".to_string(),
        sender_id: "u1".to_string(),
        chat_id: "c1".to_string(),
        thread_id: None,
        content: String::new(),
        timestamp_secs: 0,
        confirmation: Some(InboundConfirmation {
            request_id: Uuid::nil(),
            option: "reject_with_feedback".to_string(),
            label: None,
            feedback: Some("name should be more specific".to_string()),
        }),
    };
    match msg.to_external_input() {
        crate::domain::ExternalInput::Confirmation { feedback, .. } => {
            assert_eq!(feedback.as_deref(), Some("name should be more specific"));
        }
        _ => panic!("expected Confirmation"),
    }
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib to_external_input_propagates_feedback`
预期：FAIL，`InboundConfirmation` 无 `feedback` 字段

- [ ] **步骤 3：扩展类型**

在 `src/channels/traits.rs` 中为 `InboundConfirmation` 添加：
```rust
pub feedback: Option<String>,
```

在 `ExternalInput::Confirmation` 中添加：
```rust
pub feedback: Option<String>,
```

更新 `to_external_input()` 传递 `feedback`：
```rust
if let Some(ref confirmation) = self.confirmation {
    return crate::domain::ExternalInput::Confirmation {
        request_id: confirmation.request_id,
        option: confirmation.option.clone(),
        feedback: confirmation.feedback.clone(),
    };
}
```

在 `src/domain/frontend.rs` 中为 `UserAction::Confirmation` 添加：
```rust
pub feedback: Option<String>,
```

在 `src/domain/message.rs` 中为 `ToolConfirmationResponseMessage` 添加：
```rust
pub feedback: Option<String>,
```

全局搜索所有构造 `UserAction::Confirmation`、`ToolConfirmationResponseMessage`、`InboundConfirmation`、`ExternalInput::Confirmation` 的位置，添加 `feedback: None`。

- [ ] **步骤 4：编译并运行测试**

运行：`cargo test --lib to_external_input_propagates_feedback`
预期：PASS

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无警告（所有构造点已补 feedback: None）

- [ ] **步骤 5：Commit**

```bash
git add src/domain/frontend.rs src/domain/message.rs src/channels/traits.rs
git commit -m "feat(domain): add feedback field to confirmation chain"
```

---

## 任务 3：IncubatedAgentRegistry 扩展

**文件：**
- 修改：`src/infrastructure/incubation/agent_registry.rs`
- 测试：`src/infrastructure/incubation/agent_registry.rs`（`#[cfg(test)]`）

- [ ] **步骤 1：编写 append_or_rename 测试**

```rust
#[test]
fn append_or_rename_adds_suffix_on_duplicate() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("agents.toml");
    // 预写入一个 agent
    let initial = r#"
[[agent]]
name = "physics-specialist"
model = "deepseek-chat"
tags = ["physics"]
description = "test"
"#;
    std::fs::write(&config_path, initial).unwrap();

    let registry = IncubatedAgentRegistry;
    let mut record = IncubatedAgentRecord {
        name: "physics-specialist".to_string(),
        model: "deepseek-chat".to_string(),
        models: vec![],
        tags: vec!["physics".to_string()],
        description: "test".to_string(),
        tools: None,
        skills: None,
    };

    registry.append_or_rename(config_path.to_str().unwrap(), &mut record).unwrap();
    assert_eq!(record.name, "physics-specialist-2");
}

#[test]
fn update_modifies_tags_and_description() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("agents.toml");
    let initial = r#"
[[agent]]
name = "physics-specialist"
model = "deepseek-chat"
tags = ["physics"]
description = "old"
"#;
    std::fs::write(&config_path, initial).unwrap();

    let registry = IncubatedAgentRegistry;
    registry.update(
        config_path.to_str().unwrap(),
        "physics-specialist",
        &["physics".to_string(), "quantum".to_string()],
        "new description",
    ).unwrap();

    let content = std::fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("quantum"));
    assert!(content.contains("new description"));
    // model 不变
    assert!(content.contains("deepseek-chat"));
}
```

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --lib append_or_rename`
预期：FAIL，方法未定义

- [ ] **步骤 3：实现 append_or_rename 和 update**

为 `IncubatedAgentRecord` 添加 `models: Vec<ModelChainEntry>` 字段（引用 `crate::domain::ModelChainEntry`）。

实现 `append_or_rename`：读取 agents.toml，检查 name 是否已存在。若重名，追加 `-2`、`-3` 后缀直到唯一，修改 record.name 并调用现有 append 逻辑写入。

实现 `update`：读取 agents.toml，按 name 查找条目，替换 tags 和 description，原子写回。

更新所有构造 `IncubatedAgentRecord` 的位置，添加 `models: vec![]`。

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --lib append_or_rename && cargo test --lib update_modifies`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add src/infrastructure/incubation/agent_registry.rs
git commit -m "feat(infra): add append_or_rename and update to IncubatedAgentRegistry"
```

---

## 任务 4：LLM 工具定义与注册

**文件：**
- 创建：`src/systems/tools/builtin/submit_profile_update.rs`、`src/systems/tools/builtin/skip_profile_update.rs`
- 修改：`src/systems/tools/builtin/mod.rs`、`src/systems/tools/mod.rs`

- [ ] **步骤 1：实现 submit_profile_update 工具执行器**

创建 `src/systems/tools/builtin/submit_profile_update.rs`：

```rust
use crate::domain::{AgentExecutionOutput, OutputContent, ToolAction, ToolExecutor};
use crate::prelude::*;

pub struct SubmitProfileUpdateTool;

impl ToolExecutor for SubmitProfileUpdateTool {
    fn execute(
        &self,
        input: serde_json::Value,
    ) -> Result<ToolAction, Box<dyn std::error::Error + Send + Sync>> {
        let name = input["name"]
            .as_str()
            .ok_or("missing name")?
            .to_string();
        let tags: Vec<String> = input["tags"]
            .as_array()
            .ok_or("missing tags")?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let description = input["description"]
            .as_str()
            .ok_or("missing description")?
            .to_string();

        Ok(ToolAction::SubmitProfileUpdate {
            name,
            tags,
            description,
        })
    }
}
```

创建 `src/systems/tools/builtin/skip_profile_update.rs`：

```rust
use crate::domain::{ToolAction, ToolExecutor};
use crate::prelude::*;

pub struct SkipProfileUpdateTool;

impl ToolExecutor for SkipProfileUpdateTool {
    fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<ToolAction, Box<dyn std::error::Error + Send + Sync>> {
        Ok(ToolAction::SkipProfileUpdate)
    }
}
```

- [ ] **步骤 2：在 ToolAction 枚举中添加变体**

在 `src/domain/` 中找到 `ToolAction` 枚举，添加：

```rust
SubmitProfileUpdate {
    name: String,
    tags: Vec<String>,
    description: String,
},
SkipProfileUpdate,
```

- [ ] **步骤 3：注册工具**

在 `src/systems/tools/builtin/mod.rs` 中添加模块声明和 re-export。

在 `src/systems/tools/mod.rs` 的 `register_builtin_tools` 中添加两个工具的注册，schema 参照设计文档 3.2 节。`required_tag` 设为 `Some("profile")` 以限制仅 profile-designer 可用。

- [ ] **步骤 4：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 5：Commit**

```bash
git add src/systems/tools/builtin/submit_profile_update.rs src/systems/tools/builtin/skip_profile_update.rs src/systems/tools/builtin/mod.rs src/systems/tools/mod.rs src/domain/
git commit -m "feat(tools): add submit_profile_update and skip_profile_update tools"
```

---

## 任务 5：profile-designer Agent 配置

**文件：**
- 修改：`agents.toml.example`

- [ ] **步骤 1：添加 profile-designer 配置**

在 `agents.toml.example` 中添加：

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

- [ ] **步骤 2：Commit**

```bash
git add agents.toml.example
git commit -m "feat(config): add profile-designer agent configuration"
```

---

## 任务 6：Profile Generation 系统

**文件：**
- 创建：`src/systems/experience/profile_generation.rs`
- 修改：`src/systems/experience/mod.rs`

- [ ] **步骤 1：实现 profile_generation_workitem_system**

创建 `src/systems/experience/profile_generation.rs`。

此系统消费 `ProfileGenerationRequestMessage`，创建 WorkItem（与 `experience_collection_workitem_system` 模式一致）：

- 查找 profile-designer Agent
- 构建 prompt：
  - 孵化场景：候选 title + payload + 现有 Agent name 列表
  - 更新场景：现有 profile + 新增经验条目
  - 重试场景：上一次 profile + 用户反馈 + "根据反馈重新生成"
- 只暴露 `submit_profile_update` 和 `skip_profile_update` 两个工具
- Spawn `WorkItem` + `AgentExecutionRequest`

- [ ] **步骤 2：实现 profile_generation_completion_system**

此系统监听 LLM 响应中 `submit_profile_update` / `skip_profile_update` 工具调用结果：

- `submit_profile_update`：解析 name/tags/description，校验非空，spawn `ProfileGenerationCompletedMessage { generated_profile: Some(...) }`
- `skip_profile_update`：spawn `ProfileGenerationCompletedMessage { generated_profile: None }`
- 校验失败或超时：标记候选为 `WritebackFailed`，孵化场景回退硬编码 name

完成消息产出后，由后续逻辑创建 proposal + 发起审批（含 Reject & Feedback 选项）。

- [ ] **步骤 3：在 mod.rs 中声明模块**

在 `src/systems/experience/mod.rs` 中添加：
```rust
pub mod profile_generation;
```

- [ ] **步骤 4：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 5：Commit**

```bash
git add src/systems/experience/profile_generation.rs src/systems/experience/mod.rs
git commit -m "feat(experience): add profile generation workitem and completion systems"
```

---

## 任务 7：治理系统修改

**文件：**
- 修改：`src/systems/experience/governance.rs`

- [ ] **步骤 1：修改 spawn_incubation_confirmation**

将 `spawn_incubation_confirmation` 改为不再直接构造 `AgentProfile` 和发起审批，而是：

1. 将候选标记为 `ProfileGenerationPending`
2. Spawn `ProfileGenerationRequestMessage { kind: Incubation, feedback: None, retry_count: 0 }`
3. 删除 `store.merge_into_proposal(...)` 调用（profile 现在由 LLM 生成）

- [ ] **步骤 2：在 profile_generation_completion_system 中创建 proposal 和审批**

完成系统收到 `GeneratedProfile` 后：

1. 对 tags 执行 `sanitize_tags`，孵化场景手动注入 `incubated`
2. 调用 `store.merge_into_proposal` 使用 LLM 生成的 name/tags/description
3. 发起审批，选项包含 `Approve`、`Reject`、`Reject & Feedback`（若 retry_count < MAX）
4. 审批 `tool_input` 中展示完整 profile 供用户审阅

- [ ] **步骤 3：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 4：运行现有经验治理测试**

运行：`cargo test --lib experience_governance`
预期：PASS（现有测试可能需要适配新的流程）

- [ ] **步骤 5：Commit**

```bash
git add src/systems/experience/governance.rs src/systems/experience/profile_generation.rs
git commit -m "refactor(experience): governance spawns profile generation instead of hardcoded profile"
```

---

## 任务 8：审批系统修改 — 拒绝并反馈

**文件：**
- 修改：`src/systems/experience/approval.rs`

- [ ] **步骤 1：在 experience_approval_result_system 中处理 reject_with_feedback**

在 `else` 分支（用户拒绝）中，检测 `response.selected_option == "reject_with_feedback"`：

```rust
if response.selected_option == "reject_with_feedback" {
    // 用户拒绝并反馈，重新 spawn profile generation request
    if let Some(feedback) = &response.feedback {
        commands.spawn(ProfileGenerationRequestMessage {
            task_id: /* 从候选获取 */,
            agent_id: /* 从候选获取 */,
            candidate_ids: /* 从候选获取 */,
            existing_profile: /* 从 proposal 获取 */,
            kind: /* 原始 kind */,
            feedback: Some(feedback.clone()),
            retry_count: /* prev + 1 */,
        });
        // 候选回到 ProfileGenerationPending
        if let Some(c) = store.candidates.get_mut(&candidate_id) {
            c.status = ExperienceCandidateStatus::ProfileGenerationPending;
        }
        commands.entity(entity).despawn();
        continue;
    }
}
```

- [ ] **步骤 2：编写单元测试**

测试 `reject_with_feedback` 触发重新生成、retry_count 递增、达到上限后不再触发。

- [ ] **步骤 3：运行测试验证通过**

运行：`cargo test --lib reject_with_feedback`
预期：PASS

- [ ] **步骤 4：Commit**

```bash
git add src/systems/experience/approval.rs
git commit -m "feat(experience): handle reject_with_feedback in approval system"
```

---

## 任务 9：Profile 更新系统

**文件：**
- 创建：`src/systems/experience/profile_update.rs`

- [ ] **步骤 1：实现 profile_update_trigger_system**

在 `experience_writeback_system` 之后运行，检测 LTM/SkillPackage 写回成功后：

1. 查找写回的 Agent 的 name/tags/description
2. 查找新增的经验条目
3. Spawn `ProfileGenerationRequestMessage { kind: Update, existing_profile: Some(...), feedback: None, retry_count: 0 }`

- [ ] **步骤 2：实现 profile_update_writeback_system**

在 `experience_approval_result_system` 之后运行，检测更新审批通过后：

1. 第一阶段：调用 `IncubatedAgentRegistry::update` 写入 agents.toml
2. 第二阶段：通过 `Commands::entity(...).insert()` 更新 ECS `AgentCapabilities` 组件
3. 失败处理：文件写入失败标记 `WritebackFailed`；ECS 更新失败记录 `warn!`

- [ ] **步骤 3：在 mod.rs 中声明模块**

- [ ] **步骤 4：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 5：Commit**

```bash
git add src/systems/experience/profile_update.rs src/systems/experience/mod.rs
git commit -m "feat(experience): add profile update trigger and writeback systems"
```

---

## 任务 10：写回系统修改

**文件：**
- 修改：`src/systems/experience/writeback.rs`

- [ ] **步骤 1：修改 writeback_incubation_proposal**

- 使用 LLM 生成的 profile（从 proposal 获取）替代硬编码 name
- 调用 `append_or_rename` 替代 `append`，实现重名后缀兜底
- 构建 `IncubatedAgentRecord` 时填充 `models` 链（从 default Agent 继承）
- tags 经 `sanitize_tags` 过滤后注入 `incubated`

- [ ] **步骤 2：更新现有测试**

适配新的 profile 来源和 `append_or_rename` 调用。

- [ ] **步骤 3：运行测试**

运行：`cargo test --lib writeback`
预期：PASS

- [ ] **步骤 4：Commit**

```bash
git add src/systems/experience/writeback.rs
git commit -m "refactor(experience): writeback uses LLM profile and append_or_rename"
```

---

## 任务 11：系统注册

**文件：**
- 修改：`src/plugins/execution.rs`

- [ ] **步骤 1：注册新系统**

在 `execution.rs` 的 `HarnessSet::Execution` 中注册：

```rust
profile_generation_workitem_system
    .in_set(HarnessSet::Execution)
    .after(experience_governance_system),

profile_generation_completion_system
    .in_set(HarnessSet::Execution)
    .after(crate::systems::llm_response_system)
    .before(experience_approval_result_system),

profile_update_trigger_system
    .in_set(HarnessSet::Execution)
    .after(experience_writeback_system),

profile_update_writeback_system
    .in_set(HarnessSet::Execution)
    .after(experience_approval_result_system),
```

- [ ] **步骤 2：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 3：Commit**

```bash
git add src/plugins/execution.rs
git commit -m "feat(plugins): register profile generation and update systems"
```

---

## 任务 12：TUI 拒绝并反馈

**文件：**
- 修改：`src/tui/app.rs`、`src/tui/chat.rs`、`src/tui/status.rs`

- [ ] **步骤 1：新增 AppMode::Feedback**

在 `src/tui/app.rs` 的 `AppMode` 枚举中添加：

```rust
Feedback {
    request_id: Uuid,
    feedback_buffer: String,
    cursor_position: usize,
},
```

- [ ] **步骤 2：实现 Feedback 模式按键处理**

在 `handle_key_event` 中添加 `AppMode::Feedback` 分支：

- 字符输入：追加到 `feedback_buffer`（复用 `InputBar` 的字符处理逻辑）
- `Enter`：发送 `UserAction::Confirmation { option_id: "reject_with_feedback", feedback: Some(buffer) }`，切回 Chat 或下一个审批
- `Esc`：取消反馈，回到选项列表

- [ ] **步骤 3：在审批选择中检测 Reject & Feedback**

在 `handle_approval_key` 的 `Enter` 处理中，检测选中选项的 `id == "reject_with_feedback"`，切换到 `AppMode::Feedback` 而非直接发送 Confirmation。

- [ ] **步骤 4：渲染 Feedback 模式**

在 `src/tui/chat.rs` 中添加 Feedback 模式的渲染：标题行 + 提示文本 + 输入行。

在 `src/tui/status.rs` 中添加 Feedback 模式的快捷键提示。

- [ ] **步骤 5：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 6：Commit**

```bash
git add src/tui/
git commit -m "feat(tui): add reject & feedback mode for profile approval"
```

---

## 任务 13：IM 通道拒绝并反馈

**文件：**
- 修改：`src/channels/telegram.rs`、`src/channels/qq.rs`、`src/channels/frontend.rs`

- [ ] **步骤 1：Telegram — 新增 pending_feedback 状态**

在 `TelegramChannel` 中添加：
```rust
pending_feedback: Arc<RwLock<HashMap<String, PendingFeedback>>>,
```

在 `listen()` 中处理 `callback_query` 时，检测 `option == "reject_with_feedback"`：
- 不立即生成 `InboundConfirmation`，而是记录到 `pending_feedback`
- 发送提示消息"请输入评审建议："

在 `listen()` 处理文本消息时，先检查 `pending_feedback`：
- 有记录：将文本作为 feedback，组装 `InboundConfirmation { option: "reject_with_feedback", feedback: Some(text) }`
- 无记录：正常文本消息处理

处理 `/cancel`：清除 `pending_feedback`，发送普通拒绝确认。

- [ ] **步骤 2：QQ — try_match_approval_reply 支持反馈**

修改 `try_match_approval_reply`，在匹配到 `reject_with_feedback` 选项编号后，检查消息中编号后是否有额外文本：
- `3` → 普通拒绝，进入两步交互
- `3 name 太笼统` → 直接携带 feedback

QQ 也实现 `pending_feedback` 状态用于两步交互场景。

- [ ] **步骤 3：ChannelFrontend — 出向消息包含 Reject & Feedback 选项**

在 `src/channels/frontend.rs` 的 `ApprovalRequest` 处理中，确保选项列表中包含 `Reject & Feedback`（由系统侧构造选项时决定，ChannelFrontend 只负责渲染）。

- [ ] **步骤 4：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 5：Commit**

```bash
git add src/channels/telegram.rs src/channels/qq.rs src/channels/frontend.rs
git commit -m "feat(channels): add reject & feedback support for Telegram and QQ"
```

---

## 任务 14：插件 Hook

**文件：**
- 修改：`src/systems/experience/experience_hook.rs`、`src/user_plugins/hook_point.rs`

- [ ] **步骤 1：新增 HookPoint 变体**

在 `HookPoint` 枚举中添加：
```rust
OnAgentProfileGenerated,
OnAgentProfileUpdated,
OnAgentIncubated,
```

- [ ] **步骤 2：在对应系统派发 hook**

- `on_agent_profile_generated`：在 `profile_generation_completion_system` 中，LLM 生成 profile 后派发
- `on_agent_profile_updated`：在 `profile_update_writeback_system` 中，写回成功后派发
- `on_agent_incubated`：在 `writeback_incubation_proposal` 中，写入 agents.toml 成功后派发

- [ ] **步骤 3：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 4：Commit**

```bash
git add src/systems/experience/experience_hook.rs src/user_plugins/hook_point.rs src/systems/experience/profile_generation.rs src/systems/experience/profile_update.rs src/systems/experience/writeback.rs
git commit -m "feat(hooks): add on_agent_profile_generated, updated, incubated hooks"
```

---

## 任务 15：集成测试

**文件：**
- 创建：`tests/profile_generation_flow.rs`、`tests/profile_update_flow.rs`、`tests/profile_reject_feedback_flow.rs`

- [ ] **步骤 1：编写孵化端到端测试**

创建 `tests/profile_generation_flow.rs`：
- 模拟 default Agent 经验 → 收集 → 治理 → profile generation → 审批 approve → 写回
- 验证 agents.toml 中新增条目的 name/tags/description 为 LLM 生成值
- 验证包含 `models` 链
- 验证包含 `incubated` 标签

- [ ] **步骤 2：编写拒绝并反馈测试**

创建 `tests/profile_reject_feedback_flow.rs`：
- 首次生成 → 用户选择 reject_with_feedback 并提供反馈
- LLM 重新生成 → 第二轮审批 approve
- 验证 retry_count 递增
- 验证重试上限：连续 3 次后选项不含 Reject & Feedback

- [ ] **步骤 3：编写更新流程测试**

创建 `tests/profile_update_flow.rs`：
- 持久型 Agent LTM 写回 → profile 更新评估 → LLM 提议更新 → 审批 → 验证 ECS 和 agents.toml 同步
- skip_profile_update → 静默结束

- [ ] **步骤 4：运行全部测试**

运行：`cargo test --all-features`
预期：PASS

- [ ] **步骤 5：Commit**

```bash
git add tests/profile_generation_flow.rs tests/profile_update_flow.rs tests/profile_reject_feedback_flow.rs
git commit -m "test: add integration tests for profile generation, update, and reject & feedback"
```

---

## 自检

### 规格覆盖度

| 设计文档章节 | 对应任务 |
|-------------|----------|
| 3.1 领域类型 | 任务 1 |
| 3.2 LLM 工具 | 任务 4 |
| 3.3 profile-designer 配置 | 任务 5 |
| 3.4 管线变更 | 任务 6, 7, 9, 11 |
| 3.5 候选状态扩展 | 任务 1 |
| 3.6 IncubatedAgentRegistry 扩展 | 任务 3 |
| 3.7 拒绝并反馈机制 | 任务 2, 8, 12, 13 |
| 3.8 受保护标签过滤 | 任务 1 |
| 4.1 系统注册 | 任务 11 |
| 4.2 WorkItem 构建 | 任务 6 |
| 4.3 错误处理 | 任务 6, 7 |
| 4.4 插件 Hook | 任务 14 |
| 5.1 修改文件 | 全部任务覆盖 |
| 6.1-6.2 测试计划 | 任务 15 + 各任务内单元测试 |

### 类型一致性

- `ProfileGenerationRequestMessage` 在任务 1 定义，任务 6/7/8/9 使用 — 字段名一致
- `GeneratedProfile.name: String`（非 Option）— 任务 1 定义，任务 4/6 使用
- `sanitize_tags(llm_tags, existing_tags)` 签名 — 任务 1 定义，任务 7/10 使用
- `append_or_rename(config_path, &mut record)` 签名 — 任务 3 定义，任务 10 使用
- `InboundConfirmation.feedback: Option<String>` — 任务 2 定义，任务 13 使用
