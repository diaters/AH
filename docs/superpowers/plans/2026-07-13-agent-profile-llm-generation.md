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

- [ ] **步骤 4：更新现有状态机测试**

`candidate_status_machine_has_required_states` 测试硬编码 `assert_eq!(statuses.len(), 12);`（[contribution.rs](../../src/domain/contribution.rs) 的 `#[cfg(test)]` 模块）。添加 `ProfileGenerationPending` 后变 13，需同步更新：

```rust
assert_eq!(statuses.len(), 13);
```

并在 `statuses` 数组中添加 `ExperienceCandidateStatus::ProfileGenerationPending`。

- [ ] **步骤 5：运行测试验证通过**

运行：`cargo test --lib sanitize_tags && cargo test --lib candidate_status_machine`
预期：PASS

- [ ] **步骤 6：Commit**

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

需要修改 4 个文件中的类型（参考设计文档 3.7 节类型扩展）：

**文件 1：`src/channels/traits.rs`**
- 为 `InboundConfirmation` 添加 `pub feedback: Option<String>,`
- `ExternalInput::Confirmation` 定义在 `src/domain/message.rs`（不在 traits.rs），但 `to_external_input()` 在此文件中构造它，需更新：
```rust
if let Some(ref confirmation) = self.confirmation {
    return crate::domain::ExternalInput::Confirmation {
        request_id: confirmation.request_id,
        option: confirmation.option.clone(),
        feedback: confirmation.feedback.clone(),
    };
}
```

**文件 2：`src/domain/message.rs`**
- 为 `ExternalInput::Confirmation` 添加 `pub feedback: Option<String>,`（[message.rs:135-138](../../src/domain/message.rs)）
- 为 `ToolConfirmationResponseMessage` 添加 `pub feedback: Option<String>,`（[message.rs:343-346](../../src/domain/message.rs)）

**文件 3：`src/domain/frontend.rs`**
- 为 `UserAction::Confirmation` 添加 `pub feedback: Option<String>,`（[frontend.rs:165-169](../../src/domain/frontend.rs)）

**构造点补全：**
全局搜索（`grep -rn "ExternalInput::Confirmation\|UserAction::Confirmation\|ToolConfirmationResponseMessage\|InboundConfirmation" src/`）所有构造这些类型的位置，添加 `feedback: None`。重点检查：
- `src/channels/telegram.rs`、`src/channels/qq.rs`、`src/channels/frontend.rs` 中 `InboundConfirmation` 构造点
- `src/systems/input/` 中 `ExternalInput::Confirmation` 和 `UserAction::Confirmation` 构造点
- `src/systems/tools/confirmation.rs` 或类似位置中 `ToolConfirmationResponseMessage` 构造点

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
- 修改：`src/systems/tools/builtin/mod.rs`、`src/systems/tools/mod.rs`、`src/domain/space.rs`

**参考现有模式：** `submit_experience_candidate.rs` 使用 `BuiltinTool` trait（非 `ToolExecutor`），签名 `execute(&self, input: &serde_json::Value, ctx: &ToolContext) -> Result<ToolAction, ToolError>`。`ToolAction` 枚举定义在 [space.rs:188](../../src/domain/space.rs)。

- [ ] **步骤 1：在 ToolAction 枚举中添加变体**

在 `src/domain/space.rs` 的 `ToolAction` 枚举中添加：

```rust
/// 提交 profile 更新（孵化场景生成新 profile，更新场景提议新 tags/description）
SubmitProfileUpdate {
    name: String,
    tags: Vec<String>,
    description: String,
},
/// 跳过 profile 更新（更新场景下 LLM 认为不需要更新）
SkipProfileUpdate,
```

- [ ] **步骤 2：实现 submit_profile_update 工具执行器**

创建 `src/systems/tools/builtin/submit_profile_update.rs`：

```rust
//! profile 提交工具

use crate::domain::{ToolAction, ToolContext, ToolError};
use crate::domain::BuiltinTool;

/// 提交 profile 更新工具
///
/// 由 profile-designer Agent 调用，提交生成或更新后的 Agent profile。
pub struct SubmitProfileUpdateTool;

impl BuiltinTool for SubmitProfileUpdateTool {
    fn name(&self) -> &str {
        "submit_profile_update"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing name".to_string()))?
            .to_string();

        let tags: Vec<String> = input
            .get("tags")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidInput("missing tags".to_string()))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing description".to_string()))?
            .to_string();

        if name.is_empty() {
            return Err(ToolError::InvalidInput("name must not be empty".to_string()));
        }
        if tags.is_empty() {
            return Err(ToolError::InvalidInput("tags must not be empty".to_string()));
        }
        if description.is_empty() {
            return Err(ToolError::InvalidInput(
                "description must not be empty".to_string(),
            ));
        }

        Ok(ToolAction::SubmitProfileUpdate {
            name,
            tags,
            description,
        })
    }
}
```

- [ ] **步骤 3：实现 skip_profile_update 工具执行器**

创建 `src/systems/tools/builtin/skip_profile_update.rs`：

```rust
//! profile 跳过工具

use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};

/// 跳过 profile 更新工具
///
/// 由 profile-designer Agent 调用，明确表示现有 Agent profile 不需要更新。
pub struct SkipProfileUpdateTool;

impl BuiltinTool for SkipProfileUpdateTool {
    fn name(&self) -> &str {
        "skip_profile_update"
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        Ok(ToolAction::SkipProfileUpdate)
    }
}
```

- [ ] **步骤 4：在 mod.rs 中声明模块和 re-export**

在 `src/systems/tools/builtin/mod.rs` 中添加：

```rust
mod submit_profile_update;
mod skip_profile_update;

pub use submit_profile_update::SubmitProfileUpdateTool;
pub use skip_profile_update::SkipProfileUpdateTool;
```

- [ ] **步骤 5：注册工具**

在 `src/systems/tools/mod.rs` 的 `register_builtin_tools` 中添加两个工具的注册（参照 [mod.rs:237-308](../../src/systems/tools/mod.rs) 的 `submit_experience_candidate` 注册模式），schema 参照设计文档 3.2 节。`required_tag` 设为 `Some("profile")` 以限制仅 profile-designer 可用。

```rust
// Profile update tools (仅 profile-designer 可用)
registry.register(ToolDefinition {
    name: "submit_profile_update".to_string(),
    description: "提交生成或更新后的 Agent profile。孵化场景 name 作为最终 Agent 名称；更新场景 name 仅作参考，系统会强制使用原 name（不可变更）。".to_string(),
    parameters: ToolSchema {
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Agent 角色名，简洁有力"},
                "tags": {"type": "array", "items": {"type": "string"}, "description": "核心能力标签列表"},
                "description": {"type": "string", "description": "Agent 职责描述，一到两句话概括"}
            },
            "required": ["name", "tags", "description"]
        }),
    },
    default_permission: ToolPermission::Allow,
    executor: ToolExecutorKind::Builtin("submit_profile_update".to_string()),
    required_tag: Some("profile".to_string()),
});
executors.register(Box::new(SubmitProfileUpdateTool));

registry.register(ToolDefinition {
    name: "skip_profile_update".to_string(),
    description: "明确表示现有 Agent profile 不需要更新。".to_string(),
    parameters: ToolSchema {
        schema: serde_json::json!({"type": "object", "properties": {}, "required": []}),
    },
    default_permission: ToolPermission::Allow,
    executor: ToolExecutorKind::Builtin("skip_profile_update".to_string()),
    required_tag: Some("profile".to_string()),
});
executors.register(Box::new(SkipProfileUpdateTool));
```

同时在文件顶部的 `use self::builtin::{...}` 导入中添加 `SubmitProfileUpdateTool, SkipProfileUpdateTool`。

- [ ] **步骤 6：处理 ToolAction::SubmitProfileUpdate 和 SkipProfileUpdate 的分发**

`ToolAction` 的分发在 `src/systems/tools/dispatch.rs` 或 `orchestrator.rs` 中。需搜索 `match tool_action` 或 `ToolAction::` 的位置，为新变体添加处理分支。由于这两个工具的执行结果是生成 `ProfileGenerationCompletedMessage`（在任务 6 的 completion 系统中处理），dispatch 层应直接返回成功结果（不需要执行副作用），例如：

```rust
ToolAction::SubmitProfileUpdate { name, tags, description } => {
    // profile 数据由 profile_generation_completion_system 从 LLM 响应中提取
    // dispatch 层只需返回确认结果
    Ok(serde_json::json!({"status": "submitted", "name": name, "tags": tags, "description": description}))
}
ToolAction::SkipProfileUpdate => {
    Ok(serde_json::json!({"status": "skipped"}))
}
```

**注意：** 实际的 profile 提取逻辑在任务 6 的 `profile_generation_completion_system` 中实现，此步骤只需保证 dispatch 层能编译通过且返回合理结果。

- [ ] **步骤 7：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [ ] **步骤 8：Commit**

```bash
git add src/domain/space.rs src/systems/tools/builtin/submit_profile_update.rs src/systems/tools/builtin/skip_profile_update.rs src/systems/tools/builtin/mod.rs src/systems/tools/mod.rs src/systems/tools/dispatch.rs src/systems/tools/orchestrator.rs
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
- 修改：`src/systems/experience/mod.rs`、`src/domain/work_item.rs`、`src/systems/transform/llm_response.rs`

**参考现有模式：**
- WorkItem 工厂方法：`WorkItem::experience_collection(...)` 在 [work_item.rs:235](../../src/domain/work_item.rs)
- LLM 响应处理：`llm_response_system` 在 [llm_response.rs](../../src/systems/transform/llm_response.rs) 中根据 `work_item.work_type` 分发到不同 handler（如 `handle_evaluation_work_item_result`、`handle_summarization_work_item_result`）
- WorkItemType 枚举在 [work_item.rs:16](../../src/domain/work_item.rs)

### 步骤概览

本任务分为 5 个子步骤：
1. 在 `WorkItemType` 中添加 `ProfileGeneration` 变体 + `WorkItem::profile_generation(...)` 工厂方法
2. 实现 `profile_generation_workitem_system`（消费请求消息，创建 WorkItem）
3. 在 `llm_response_system` 中添加 `handle_profile_generation_work_item_result` handler
4. 实现 `profile_generation_completion_system`（消费完成消息，创建 proposal + 发起审批）
5. 在 `mod.rs` 中声明模块和导出

- [ ] **步骤 1：添加 WorkItemType 变体和工厂方法**

在 `src/domain/work_item.rs` 的 `WorkItemType` 枚举中添加：

```rust
/// profile 生成工作项（孵化场景生成新 profile，更新场景评估并生成更新后 profile）
ProfileGeneration,
```

在 `impl WorkItem` 中添加工厂方法（参照 `experience_collection` 模式）：

```rust
/// 创建 profile 生成工作项
pub fn profile_generation(
    task_id: TaskId,
    prompt: String,
    conversation: Vec<ConversationMessage>,
    tools: Vec<ToolDefinition>,
    governing_agent_id: AgentId,
    kind: crate::domain::ProfileGenerationKind,
) -> Self {
    let tags = TagSet::from_tags(["profile"]);
    let system_prompt = match kind {
        crate::domain::ProfileGenerationKind::Incubation => {
            "你是一名 Agent 元信息设计师。请根据提供的经验候选，生成一个新 Agent 的角色名、能力标签和职责描述。\
             必须调用 submit_profile_update 提交结果，或调用 skip_profile_update 表示无法生成。".to_string()
        }
        crate::domain::ProfileGenerationKind::Update => {
            "你是一名 Agent 元信息设计师。请评估现有 Agent profile 是否需要根据新经验更新 tags/description。\
             若需要更新，调用 submit_profile_update 提交新 profile；若不需要，调用 skip_profile_update。".to_string()
        }
    };
    let context = WorkItemContext {
        conversation: Some(conversation),
        tools,
        system_prompt: Some(system_prompt),
    };
    let input = WorkItemInput { prompt, context };
    let mut wi = Self::new(
        task_id,
        WorkItemType::ProfileGeneration,
        input,
        tags,
        WorkItemOrigin::ExperienceCollection, // 复用，或新增变体
        WorkItemWritebackTarget::ExperienceInbox,
    );
    wi.governing_agent_id = Some(governing_agent_id);
    wi
}
```

**注意：** `WorkItemOrigin` 和 `WorkItemWritebackTarget` 枚举可能需要新增变体，或在现有变体中复用。执行时检查这两个枚举的现有定义，选择最合适的复用方式（参考 `experience_collection` 使用 `WorkItemOrigin::ExperienceCollection` + `WorkItemWritebackTarget::ExperienceInbox`）。

- [ ] **步骤 2：实现 profile_generation_workitem_system**

创建 `src/systems/experience/profile_generation.rs`。

此系统消费 `ProfileGenerationRequestMessage`，参照 [collection.rs:55-113](../../src/systems/experience/collection.rs) 的 `experience_collection_workitem_system` 模式：

```rust
use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    Agent, ExperienceCandidatePayload, ExperienceStore, ProfileGenerationKind,
    ProfileGenerationRequestMessage, SpaceToolRegistry, WorkItem,
};

pub(crate) fn profile_generation_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &ProfileGenerationRequestMessage)>,
    agents: Query<&Agent>,
    store: Res<ExperienceStore>,
    registry: Res<SpaceToolRegistry>,
) {
    for (entity, request) in &requests {
        // 1. 查找 profile-designer Agent（按 tags 匹配 "profile"）
        let profile_designer = agents.iter().find(|a| {
            a.capabilities.tags.iter().any(|t| t == "profile")
        });
        let profile_designer_id = match profile_designer {
            Some(a) => a.id,
            None => {
                tracing::warn!(
                    event = "ProfileDesignerNotFound",
                    task_id = %request.task_id,
                    "profile-designer agent not found, falling back to incubated-{task_id}"
                );
                // 孵化场景回退硬编码 name（设计文档 4.3 错误处理）
                // 直接 spawn completion 消息，跳过 LLM 调用
                handle_profile_designer_missing(&mut commands, request);
                commands.entity(entity).despawn();
                continue;
            }
        };

        // 2. 构建 prompt（根据 kind 和 feedback）
        let prompt = build_profile_generation_prompt(request, &store, &agents);

        // 3. 收集工具定义（仅 submit_profile_update 和 skip_profile_update）
        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| {
                tool.name == "submit_profile_update" || tool.name == "skip_profile_update"
            })
            .cloned()
            .collect();

        // 4. 构建 conversation（无历史对话，仅作为 WorkItem 上下文占位）
        let conversation = Vec::new();

        // 5. 创建 WorkItem 并分配给 profile-designer
        let mut work_item = WorkItem::profile_generation(
            request.task_id,
            prompt,
            conversation,
            tools,
            request.agent_id,
            request.kind.clone(),
        );
        work_item.assign(profile_designer_id);

        debug!(
            event = "ProfileGenerationWorkItemCreated",
            task_id = %request.task_id,
            agent_id = %request.agent_id,
            kind = ?request.kind,
            retry_count = request.retry_count,
            has_feedback = request.feedback.is_some(),
            "spawning profile generation work item"
        );

        commands.spawn(work_item);
        commands.entity(entity).despawn();
    }
}
```

`build_profile_generation_prompt` 函数根据 `kind` 和 `feedback` 构建不同 prompt：

```rust
fn build_profile_generation_prompt(
    request: &ProfileGenerationRequestMessage,
    store: &ExperienceStore,
    agents: &Query<&Agent>,
) -> String {
    let mut prompt = String::new();

    match request.kind {
        ProfileGenerationKind::Incubation => {
            prompt.push_str("## 任务\n\n根据以下经验候选，为一个新 Agent 生成元信息（name、tags、description）。\n\n");

            // 注入候选材料
            prompt.push_str("## 经验候选\n\n");
            for id in &request.candidate_ids {
                if let Some(candidate) = store.candidates.get(id) {
                    prompt.push_str(&format!("### {}\n\n", candidate.title));
                    if let ExperienceCandidatePayload::Knowledge { content } = &candidate.payload {
                        prompt.push_str(&format!("{}\n\n", content));
                    } else if let ExperienceCandidatePayload::Skill { name, description, instructions, .. } = &candidate.payload {
                        prompt.push_str(&format!("技能名：{}\n描述：{}\n指令：{}\n\n", name, description, instructions));
                    }
                }
            }

            // 注入现有 Agent name 列表（避免重名，设计文档决策 9）
            let existing_names: Vec<&str> = agents.iter().map(|a| a.profile.name.as_str()).collect();
            prompt.push_str(&format!(
                "## 现有 Agent 名称（避免重复）\n\n{}\n\n",
                existing_names.join(", ")
            ));

            prompt.push_str("## 要求\n\n");
            prompt.push_str("1. name：简洁有力，使用 kebab-case，如 'physics-specialist'\n");
            prompt.push_str("2. tags：3-5 个核心能力标签，不含 'incubated' 或 'default'（系统会自动注入）\n");
            prompt.push_str("3. description：一到两句话概括 Agent 职责\n");
            prompt.push_str("4. 必须调用 submit_profile_update 提交结果\n");
        }
        ProfileGenerationKind::Update => {
            prompt.push_str("## 任务\n\n评估现有 Agent profile 是否需要根据新经验更新 tags/description。\n\n");

            // 注入现有 profile
            if let Some(existing) = &request.existing_profile {
                prompt.push_str("## 当前 Agent profile\n\n");
                prompt.push_str(&format!("- name: {}\n", existing.name));
                prompt.push_str(&format!("- tags: {}\n", existing.tags.join(", ")));
                prompt.push_str(&format!("- description: {}\n\n", existing.description));
            }

            // 注入新增经验条目
            prompt.push_str("## 新增经验条目\n\n");
            for id in &request.candidate_ids {
                if let Some(candidate) = store.candidates.get(id) {
                    prompt.push_str(&format!("- {}\n", candidate.title));
                }
            }
            prompt.push_str("\n");

            prompt.push_str("## 要求\n\n");
            prompt.push_str("1. 若新经验带来了现有 tags/description 未覆盖的新能力，调用 submit_profile_update 提交更新后的完整 profile\n");
            prompt.push_str("2. name 字段会被系统忽略（name 不可变更），但仍需填写\n");
            prompt.push_str("3. 若不需要更新，调用 skip_profile_update\n");
        }
    }

    // 重试场景：注入用户反馈
    if let Some(feedback) = &request.feedback {
        prompt.push_str(&format!(
            "## 用户评审反馈\n\n用户对上一次生成的 profile 提出以下反馈，请根据反馈重新生成：\n\n{}\n\n",
            feedback
        ));
    }

    prompt
}
```

`handle_profile_designer_missing` 函数处理 profile-designer Agent 不存在的情况（设计文档 4.3 错误处理：孵化场景回退硬编码 name）：

```rust
fn handle_profile_designer_missing(
    commands: &mut Commands,
    request: &ProfileGenerationRequestMessage,
) {
    // 孵化场景：spawn 回退 profile（硬编码 name）
    // 更新场景：静默跳过（不更新现有 profile）
    match request.kind {
        ProfileGenerationKind::Incubation => {
            let fallback_profile = crate::domain::GeneratedProfile {
                name: format!("incubated-{}", request.task_id),
                tags: vec![],  // 由写回逻辑注入 incubated
                description: String::new(),
            };
            commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
                task_id: request.task_id,
                agent_id: request.agent_id,
                generated_profile: Some(fallback_profile),
                kind: request.kind.clone(),
            });
        }
        ProfileGenerationKind::Update => {
            // 静默跳过，不 spawn 完成消息
            debug!(
                event = "ProfileUpdateSkippedNoDesigner",
                task_id = %request.task_id,
                "profile-designer not found, skipping update evaluation"
            );
        }
    }
}
```

- [ ] **步骤 3：在 llm_response_system 中添加 ProfileGeneration handler**

在 `src/systems/transform/llm_response.rs` 的 `llm_response_system` 中，参照 `WorkItemType::ExperienceCollection` 的处理模式（[llm_response.rs:650-711](../../src/systems/transform/llm_response.rs)），为 `WorkItemType::ProfileGeneration` 添加 handler。

在 `match work_item.work_type` 中添加分支：

```rust
WorkItemType::ProfileGeneration => {
    match &result.result {
        Ok(AgentExecutionOutput {
            content: OutputContent::ToolCalls(_),
            ..
        }) => {
            // 不 continue，让下面的 tool calling loop 处理 tool calls
            // submit_profile_update / skip_profile_update 工具调用会触发 orchestrator
            // orchestrator 中的 ToolAction::SubmitProfileUpdate / SkipProfileUpdate 分支
            // 会 spawn ProfileGenerationCompletedMessage
        }
        Ok(_) => {
            // LLM 返回普通文本（未调用工具）：视为失败
            // 孵化场景回退硬编码 name，更新场景静默跳过
            handle_profile_generation_no_tool_call(
                &mut commands,
                work_item,
                entity,
                work_item_entity,
            );
            continue;
        }
        Err(_) => {
            // LLM 调用失败：同上处理
            handle_profile_generation_no_tool_call(
                &mut commands,
                work_item,
                entity,
                work_item_entity,
            );
            continue;
        }
    }
}
```

`handle_profile_generation_no_tool_call` 函数处理 LLM 未调用工具的情况：

```rust
fn handle_profile_generation_no_tool_call(
    commands: &mut Commands,
    work_item: &WorkItem,
    result_entity: Entity,
    work_item_entity: Entity,
) {
    // 孵化场景回退硬编码 name（设计文档 4.3）
    // 更新场景静默跳过
    let governing_agent_id = work_item.governing_agent_id.unwrap_or(uuid::Uuid::nil());
    commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
        task_id: work_item.task_id,
        agent_id: governing_agent_id,
        generated_profile: Some(crate::domain::GeneratedProfile {
            name: format!("incubated-{}", work_item.task_id),
            tags: vec![],
            description: String::new(),
        }),
        kind: crate::domain::ProfileGenerationKind::Incubation, // 从 work_item 获取实际 kind
    });
    commands.entity(work_item_entity).despawn();
    commands.entity(result_entity).despawn();
}
```

**注意：** `ProfileGenerationKind` 需要从 WorkItem 中获取。可以考虑在 WorkItem 中添加 `metadata` 字段携带 kind，或通过其他方式传递。执行时根据现有 WorkItem 结构选择最合适的传递方式。

- [ ] **步骤 4：在 orchestrator.rs 中处理 ToolAction::SubmitProfileUpdate / SkipProfileUpdate**

在 `src/systems/tools/orchestrator.rs` 中（参照 `ToolAction::SubmitExperienceCandidate` 处理模式 [orchestrator.rs:602](../../src/systems/tools/orchestrator.rs)），为新变体添加处理分支：

```rust
Ok(ToolAction::SubmitProfileUpdate { name, tags, description }) => {
    // 从 request 中获取 kind（需通过 work_item_id 查找 WorkItem，或通过其他方式传递）
    let generated_profile = crate::domain::GeneratedProfile {
        name,
        tags,
        description,
    };
    commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        generated_profile: Some(generated_profile),
        kind: /* 从 work_item 获取 */,
    });
    // spawn 工具结果给 LLM
    spawn_tool_result(commands, request_entity, request, &serde_json::json!({"status": "submitted"}));
}
Ok(ToolAction::SkipProfileUpdate) => {
    commands.spawn(crate::domain::ProfileGenerationCompletedMessage {
        task_id: request.request.task_id,
        agent_id: request.request.agent_id,
        generated_profile: None,
        kind: /* 从 work_item 获取 */,
    });
    spawn_tool_result(commands, request_entity, request, &serde_json::json!({"status": "skipped"}));
}
```

**关键问题：kind 传递。** `ProfileGenerationKind` 需要从 `ProfileGenerationRequestMessage` 传递到 `ProfileGenerationCompletedMessage`。可选方案：
- 方案 A：在 WorkItem 中添加 `metadata: Option<serde_json::Value>` 字段携带 kind
- 方案 B：通过 `ExperienceStore` 中间存储（以 task_id 为 key）
- 方案 C：在 `ProfileGenerationCompletedMessage` 中添加 `kind` 字段时，由 `profile_generation_completion_system` 从 `ExperienceStore` 中查找

执行时选择最简方案。推荐方案 B：在 `profile_generation_workitem_system` 中将 `kind` 存入 `ExperienceStore` 的临时字段（如 `profile_generation_kind: HashMap<TaskId, ProfileGenerationKind>`），`profile_generation_completion_system` 消费时取出。

- [ ] **步骤 5：实现 profile_generation_completion_system**

在 `src/systems/experience/profile_generation.rs` 中添加：

```rust
pub(crate) fn profile_generation_completion_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut pending_hooks: ResMut<crate::domain::PendingExperienceHooks>,
    agents: Query<&Agent>,
    messages: Query<(Entity, &crate::domain::ProfileGenerationCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        match msg.kind {
            crate::domain::ProfileGenerationKind::Incubation => {
                handle_incubation_profile_completed(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    &agents,
                    msg,
                );
            }
            crate::domain::ProfileGenerationKind::Update => {
                handle_update_profile_completed(
                    &mut commands,
                    &mut store,
                    &mut pending_hooks,
                    msg,
                );
            }
        }
        commands.entity(entity).despawn();
    }
}
```

`handle_incubation_profile_completed` 函数（任务 7 步骤 2 会扩展此函数）：

```rust
fn handle_incubation_profile_completed(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    pending_hooks: &mut crate::domain::PendingExperienceHooks,
    agents: &Query<&Agent>,
    msg: &crate::domain::ProfileGenerationCompletedMessage,
) {
    let task_id = msg.task_id;
    let agent_id = msg.agent_id;

    let Some(generated) = &msg.generated_profile else {
        // skip_profile_update 或回退：孵化场景必须有 profile
        // 若为 None，使用硬编码回退
        tracing::warn!(
            event = "IncubationProfileMissing",
            task_id = %task_id,
            "incubation completed without profile, using fallback"
        );
        return;
    };

    // 1. 对 tags 执行 sanitize_tags，孵化场景手动注入 incubated
    let mut sanitized_tags = crate::domain::sanitize_tags(generated.tags.clone(), &[]);
    if !sanitized_tags.contains(&"incubated".to_string()) {
        sanitized_tags.push("incubated".to_string());
    }

    // 2. 查找 default Agent 以继承 models 链
    let default_agent = agents.iter().find(|a| {
        a.capabilities.tags.iter().any(|t| t == "default")
    });

    // 3. 调用 store.merge_into_proposal 使用 LLM 生成的 name/tags/description
    let agent_profile = crate::domain::AgentProfile {
        name: generated.name.clone(),
        model: default_agent
            .map(|a| a.profile.model.clone())
            .unwrap_or_default(),
    };
    // merge_into_proposal 需要 candidate，从 store 中查找该 task 的候选
    if let Some(candidate_id) = store.governance_candidates_for_task(task_id).first().copied() {
        if let Some(candidate) = store.candidates.get(&candidate_id).cloned() {
            store.merge_into_proposal(task_id, agent_id, agent_profile, &candidate);
        }
    }

    // 4. 发起审批（选项包含 Approve、Reject、Reject & Feedback）
    // retry_count < MAX 时包含 Reject & Feedback
    spawn_profile_approval(commands, store, task_id, agent_id, &generated.name, &sanitized_tags, &generated.description, 0);

    // 5. 派发 on_agent_profile_generated hook
    pending_hooks.0.push((
        crate::user_plugins::hook_point::HookPoint::OnAgentProfileGenerated,
        task_id, // 注：实际应传递 candidate_id，执行时调整
    ));

    tracing::info!(
        event = "ProfileGenerationCompleted",
        task_id = %task_id,
        name = %generated.name,
        tags = ?sanitized_tags,
        "incubation profile generated, awaiting approval"
    );
}
```

`spawn_profile_approval` 函数发起审批（参照 [governance.rs:161-222](../../src/systems/experience/governance.rs) 的 `spawn_experience_confirmation` 模式）：

```rust
fn spawn_profile_approval(
    commands: &mut Commands,
    store: &mut ExperienceStore,
    task_id: TaskId,
    agent_id: AgentId,
    name: &str,
    tags: &[String],
    description: &str,
    retry_count: u32,
) {
    let request_id = uuid::Uuid::new_v4();
    // 绑定到第一个候选（执行时根据实际候选绑定逻辑调整）
    if let Some(candidate_id) = store.governance_candidates_for_task(task_id).first().copied() {
        store.bind_approval_request(request_id, candidate_id);
    }

    // 构建审批选项
    let mut options = vec![
        crate::domain::ConfirmationOption {
            id: "approve".to_string(),
            label: "批准".to_string(),
            description: "批准 LLM 生成的 profile".to_string(),
        },
        crate::domain::ConfirmationOption {
            id: "reject".to_string(),
            label: "拒绝".to_string(),
            description: "拒绝此 profile，终止孵化".to_string(),
        },
    ];
    if retry_count < crate::domain::MAX_PROFILE_GENERATION_RETRIES {
        options.push(crate::domain::ConfirmationOption {
            id: "reject_with_feedback".to_string(),
            label: "拒绝并反馈".to_string(),
            description: "拒绝并提供评审建议，LLM 将重新生成".to_string(),
        });
    }

    commands.spawn(crate::domain::ToolConfirmationRequestMessage {
        request_id,
        task_id,
        agent_id,
        tool_name: "profile_generation".to_string(),
        tool_input: serde_json::json!({
            "name": name,
            "tags": tags,
            "description": description,
            "retry_count": retry_count,
        }),
        options,
        source: crate::domain::ConfirmationSource::User,
        parent_agent_id: None,
        approval_context: Some(format!("Agent profile generation for task {}", task_id)),
    });
}
```

`handle_update_profile_completed` 函数（任务 9 步骤 2 会扩展此函数）：

```rust
fn handle_update_profile_completed(
    commands: &mut Commands,
    _store: &mut ExperienceStore,
    pending_hooks: &mut crate::domain::PendingExperienceHooks,
    msg: &crate::domain::ProfileGenerationCompletedMessage,
) {
    // 更新场景：若有 generated_profile，发起审批；若无（skip），静默结束
    if let Some(generated) = &msg.generated_profile {
        // 发起更新审批（复用 spawn_profile_approval，但 kind=Update）
        tracing::info!(
            event = "ProfileUpdateProposed",
            task_id = %msg.task_id,
            "profile update proposed, awaiting approval"
        );
        // 具体审批逻辑在任务 9 中实现
    } else {
        tracing::info!(
            event = "ProfileUpdateSkipped",
            task_id = %msg.task_id,
            "profile update skipped by LLM"
        );
    }
    // 派发 on_agent_profile_generated hook
    pending_hooks.0.push((
        crate::user_plugins::hook_point::HookPoint::OnAgentProfileGenerated,
        msg.task_id,
    ));
}
```

- [ ] **步骤 6：在 mod.rs 中声明模块和导出**

在 `src/systems/experience/mod.rs` 中添加：

```rust
pub mod profile_generation;

pub(crate) use profile_generation::{
    profile_generation_completion_system, profile_generation_workitem_system,
};
```

- [ ] **步骤 7：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误（可能有 warning 提示未使用的函数，任务 7/9 会使用它们）

- [ ] **步骤 8：Commit**

```bash
git add src/systems/experience/profile_generation.rs src/systems/experience/mod.rs src/domain/work_item.rs src/systems/transform/llm_response.rs src/systems/tools/orchestrator.rs
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

**修改任务 6 创建的 `src/systems/experience/profile_generation.rs` 中的 `handle_incubation_profile_completed` 函数**（任务 6 仅写了骨架，此步骤完成具体实现）：

1. 对 tags 执行 `sanitize_tags`，孵化场景手动注入 `incubated`
2. 调用 `store.merge_into_proposal` 使用 LLM 生成的 name/tags/description
3. 发起审批（调用任务 6 已声明的 `spawn_profile_approval` 函数），选项包含 `Approve`、`Reject`、`Reject & Feedback`（若 retry_count < MAX）
4. 审批 `tool_input` 中展示完整 profile 供用户审阅
5. 派发 `OnAgentProfileGenerated` hook（任务 14 完成具体派发）

**注意：** 任务 6 已声明 `handle_incubation_profile_completed` 和 `spawn_profile_approval` 的骨架代码，此步骤填充实际逻辑。

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

- [ ] **步骤 3：在 mod.rs 中声明模块和导出**

在 `src/systems/experience/mod.rs` 中添加：

```rust
pub mod profile_update;

pub(crate) use profile_update::{
    profile_update_trigger_system, profile_update_writeback_system,
};
```

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

在 `src/user_plugins/hook_point.rs` 的 `HookPoint` 枚举中添加（参照 [hook_point.rs:11-35](../../src/user_plugins/hook_point.rs)）：
```rust
OnAgentProfileGenerated,
OnAgentProfileUpdated,
OnAgentIncubated,
```

**同时更新 3 处配套实现（参考 [hook_point.rs:43-72](../../src/user_plugins/hook_point.rs)、[hook_point.rs:81-106](../../src/user_plugins/hook_point.rs)、[hook_point.rs:113-139](../../src/user_plugins/hook_point.rs)）：**

1. **`FromStr` 实现**（`from_str` 方法）— 添加 3 个匹配分支：
```rust
"on_agent_profile_generated" => Ok(Self::OnAgentProfileGenerated),
"on_agent_profile_updated" => Ok(Self::OnAgentProfileUpdated),
"on_agent_incubated" => Ok(Self::OnAgentIncubated),
```

2. **`as_serialized` 方法** — 添加 3 个匹配分支：
```rust
Self::OnAgentProfileGenerated => "on_agent_profile_generated",
Self::OnAgentProfileUpdated => "on_agent_profile_updated",
Self::OnAgentIncubated => "on_agent_incubated",
```

3. **`parses_all_known_points` 测试**（[hook_point.rs:113](../../src/user_plugins/hook_point.rs)）— 在测试数组中添加 3 个字符串：
```rust
"on_agent_profile_generated",
"on_agent_profile_updated",
"on_agent_incubated",
```

**注意：** `#[serde(rename_all = "snake_case")]` 已自动处理序列化，无需额外 Serde 配置。

- [ ] **步骤 2：在对应系统派发 hook**

- `on_agent_profile_generated`：在 `profile_generation_completion_system` 中，LLM 生成 profile 后派发
- `on_agent_profile_updated`：在 `profile_update_writeback_system` 中，写回成功后派发
- `on_agent_incubated`：在 `writeback_incubation_proposal` 中，写入 agents.toml 成功后派发

- [ ] **步骤 3：编译验证**

运行：`cargo clippy --all-targets --all-features -- -D warnings`
预期：无错误

- [x] **步骤 4：Commit**

```bash
git add src/systems/experience/experience_hook.rs src/user_plugins/hook_point.rs src/systems/experience/profile_generation.rs src/systems/experience/profile_update.rs src/systems/experience/writeback.rs
git commit -m "feat(hooks): add on_agent_profile_generated, updated, incubated hooks"
```

---

## 任务 15：集成测试

**文件：**
- 创建：`tests/profile_generation_flow.rs`、`tests/profile_update_flow.rs`、`tests/profile_reject_feedback_flow.rs`

- [x] **步骤 1：编写孵化端到端测试**

创建 `tests/profile_generation_flow.rs`：
- 模拟 default Agent 经验 → 收集 → 治理 → profile generation → 审批 approve → 写回
- 验证 agents.toml 中新增条目的 name/tags/description 为 LLM 生成值
- 验证包含 `models` 链
- 验证包含 `incubated` 标签

**LLM 模拟机制（参照 [llm_tool_calling_flow.rs](../../tests/llm_tool_calling_flow.rs)、[experience_collection_workitem_flow.rs](../../tests/experience_collection_workitem_flow.rs)）：**

实现 `AgentExecutor` trait 的 mock struct，根据 `request.conversation` 是否存在判断调用轮次，首轮返回 `OutputContent::ToolCalls` 触发 `submit_profile_update` 工具调用，后续轮次返回 `OutputContent::Text`。模式：

```rust
struct ProfileDesignerMockExecutor;

impl AgentExecutor for ProfileDesignerMockExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let response = if request.conversation.is_some() {
            // 后续轮次：返回普通文本结束 loop
            AgentExecutionOutput {
                content: harness::OutputContent::Text("profile submitted".to_string()),
                reasoning_content: None,
            }
        } else {
            // 首轮：触发 submit_profile_update 工具调用
            AgentExecutionOutput {
                content: harness::OutputContent::ToolCalls(vec![LlmToolCall {
                    id: "call_profile".to_string(),
                    name: "submit_profile_update".to_string(),
                    arguments: r#"{"name":"physics-specialist","tags":["physics","calculation"],"description":"Physics specialist agent"}"#.to_string(),
                }]),
                reasoning_content: None,
            }
        };
        Box::pin(async move { Ok(response) })
    }
}
```

通过 `ExecutorRegistry::from_single_executor` 注册 mock，传入 `build_harness_app`。审批通过插入 `ToolConfirmationResponseMessage` 模拟用户选择 `approve` 选项。

- [x] **步骤 2：编写拒绝并反馈测试**

创建 `tests/profile_reject_feedback_flow.rs`：
- 首次生成 → 用户选择 reject_with_feedback 并提供反馈
- LLM 重新生成 → 第二轮审批 approve
- 验证 retry_count 递增
- 验证重试上限：连续 3 次后选项不含 Reject & Feedback

**LLM 模拟：** mock executor 根据 `request.conversation` 中 tool 消息数量判断当前 retry_count，返回不同的 profile 内容以验证反馈注入。审批通过插入 `ToolConfirmationResponseMessage { selected_option: "reject_with_feedback", feedback: Some("...") }` 模拟用户反馈。

- [x] **步骤 3：编写更新流程测试**

创建 `tests/profile_update_flow.rs`：
- 持久型 Agent LTM 写回 → profile 更新评估 → LLM 提议更新 → 审批 → 验证 ECS 和 agents.toml 同步
- skip_profile_update → 静默结束

**LLM 模拟：** 提议更新场景使用首轮返回 `submit_profile_update` ToolCalls 的 mock；skip 场景使用首轮返回 `skip_profile_update` ToolCalls 的 mock。

- [x] **步骤 4：运行全部测试**

运行：`cargo test --all-features`
预期：PASS

- [x] **步骤 5：Commit**

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
