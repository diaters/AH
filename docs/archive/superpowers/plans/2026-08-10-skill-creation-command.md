# `/skill` Slash Command 实施计划

> **状态：已归档（2026-08-14）** — 本计划已执行完毕（11 个 Task 全部完成，随
> `feat/skill-creation-command` 分支合入）。
> 相关能力已记录在 [docs/current-state.md](../../current-state.md)。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `/skill <意图描述>` slash command，让用户通过自然语言意图驱动 LLM 为当前任务的 Agent 无中生有生成新 skill。

**Architecture:** 用户输入 `/skill <intent>` → command_parse_system 解析并查找活跃任务的 Agent → spawn SkillCreationRequestMessage → skill_creation_workitem_system 创建 WorkItem + 沙盒目录 → skill-creator Agent 执行（使用 write_skill_file / read_skill_file / submit_skill 工具）→ orchestrator 验证并构造 ExperienceCandidate → 确认流程 → skill_creation_writeback_system rename 写回 + SkillRegistry 注册。

**Tech Stack:** Rust, Bevy ECS, serde_json, std::fs

## Global Constraints

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS，所有新类型必须是 Component 或 Resource
- 错误处理：库 crate 使用 thiserror，应用使用 anyhow
- 日志：使用 tracing 宏，遵循 docs/logs.md 规范
- Serde 兼容：`is_new` 字段加 `#[serde(default)]`
- 模式匹配兼容：`ExperienceCandidatePayload::Skill` 解构使用 `..` 忽略 `is_new`
- 提交信息：Conventional Commits
- 测试：单元测试与实现文件放在一起，`#[cfg(test)]`
- 参考设计文档：`docs/design/2026-08-10-skill-creation-command-design.md`

---

### Task 1: 领域层 — ExperienceCandidatePayload::Skill 新增 is_new 字段

**Files:**
- Modify: `src/domain/contribution.rs:78-91` (ExperienceCandidatePayload::Skill)
- Modify: `src/domain/contribution.rs:143-172` (ExperienceCandidate::skill() constructor)

**Interfaces:**
- Consumes: None (foundation task)
- Produces: `ExperienceCandidatePayload::Skill { is_new: bool }` with `#[serde(default)]`, `ExperienceCandidate::skill_new()` constructor, `is_skill_new()` helper function

- [x] **Step 1: Add `is_new` field to `ExperienceCandidatePayload::Skill` with `#[serde(default)]`**

In `src/domain/contribution.rs`, the `Skill` variant already has `is_new` with `#[serde(default)]` (line 88-89). Verify this is correct and no changes needed. Read the file to confirm.

- [x] **Step 2: Add `skill_new()` constructor to `ExperienceCandidate`**

In `src/domain/contribution.rs`, after the existing `skill()` method (ends at line 172), add a new constructor:

```rust
/// 创建 Skill 类候选（新建场景，is_new = true）。
#[allow(clippy::too_many_arguments)]
pub fn skill_new(
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
            is_new: true,
        },
        dependency_refs: Vec::new(),
        status: ExperienceCandidateStatus::Submitted,
        governing_agent_id: None,
        derived_from_candidate_ids: Vec::new(),
    }
}
```

- [x] **Step 3: Add `is_skill_new()` helper function**

After the `skill_new()` constructor, add:

```rust
/// 判断 Skill 候选是否为新建（/skill 命令创建）。
pub fn is_skill_new(payload: &ExperienceCandidatePayload) -> bool {
    matches!(payload, ExperienceCandidatePayload::Skill { is_new: true, .. })
}
```

- [x] **Step 4: Fix existing `skill()` constructor to explicitly set `is_new: false`**

The code currently does NOT compile because `is_new` was added to the `Skill` variant but the `skill()` constructor at line 161 doesn't include it. Fix:

In the existing `skill()` method (line 161), add `is_new: false` to the payload construction:

```rust
payload: ExperienceCandidatePayload::Skill {
    name,
    description,
    instructions,
    file_refs,
    is_new: false,
},
```

- [x] **Step 5: Fix all `ExperienceCandidatePayload::Skill` pattern matches for `is_new` compatibility**

The code currently has compilation errors from missing `is_new` in pattern matches. Fix each file:

**`src/systems/experience/writeback.rs:231`** — add `..` to the let-binding:
```rust
let crate::domain::ExperienceCandidatePayload::Skill {
    name,
    description,
    instructions,
    file_refs,
    ..
} = &candidate.payload
```

**`src/systems/experience/writeback.rs:356`** — add `..`:
```rust
&& let crate::domain::ExperienceCandidatePayload::Skill {
    name,
    description,
    instructions,
    file_refs,
    ..
} = &candidate.payload
```

**`src/systems/tools/orchestrator.rs:1348`** — add `..`:
```rust
ExperienceCandidatePayload::Skill {
    name,
    description,
    instructions,
    file_refs,
    ..
}
```

Also check test files:
- `tests/experience_candidate_flow.rs:46` and `:130`
- `tests/experience_layered_governance_flow.rs:74` and `:227`
- `src/systems/experience/approval.rs:263`

All need `is_new` field added or `..` in pattern.

- [x] **Step 6: Write unit test for `skill_new()` and `is_skill_new()`**

In the `#[cfg(test)]` block of `contribution.rs`:

```rust
#[test]
fn skill_new_constructor_sets_is_new_true() {
    let candidate = ExperienceCandidate::skill_new(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "new skill".to_string(),
        "my-skill".to_string(),
        "desc".to_string(),
        "instructions".to_string(),
        vec![],
    );
    assert!(is_skill_new(&candidate.payload));
    match &candidate.payload {
        ExperienceCandidatePayload::Skill { is_new, .. } => assert!(*is_new),
        _ => panic!("expected Skill payload"),
    }
}

#[test]
fn skill_constructor_sets_is_new_false() {
    let candidate = ExperienceCandidate::skill(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "existing skill".to_string(),
        "my-skill".to_string(),
        "desc".to_string(),
        "instructions".to_string(),
        vec![],
    );
    assert!(!is_skill_new(&candidate.payload));
}

#[test]
fn skill_payload_serde_default_is_new() {
    let json = r#"{"name":"x","description":"d","instructions":"i","file_refs":[]}"#;
    let payload: ExperienceCandidatePayload = serde_json::from_str(
        &format!("{{\"Skill\":{}}}", json),
    ).unwrap();
    match &payload {
        ExperienceCandidatePayload::Skill { is_new, .. } => assert!(!is_new),
        _ => panic!("expected Skill"),
    }
}
```

- [x] **Step 7: Run `cargo test --all-features` to verify all existing tests pass**

Run: `cargo test --all-features`
Expected: All tests pass, no compilation errors from `is_new` field addition

- [x] **Step 8: Commit**

```bash
git add src/domain/contribution.rs
git commit -m "feat(contribution): add is_new field to Skill payload and skill_new constructor"
```

---

### Task 2: 领域层 — SkillCreationContext + SkillCreationRequestMessage + SkillCreationWritebackMessage + WorkItemType::SkillCreation

**Files:**
- Modify: `src/domain/contribution.rs` (add SkillCreationContext)
- Modify: `src/domain/message.rs` (add SkillCreationRequestMessage, SkillCreationWritebackMessage)
- Modify: `src/domain/work_item.rs` (add WorkItemType::SkillCreation + required_tag + skill_creation() factory)
- Modify: `src/domain/space.rs` (add ToolAction::SubmitSkillCandidate)

**Interfaces:**
- Consumes: Task 1 (ExperienceCandidatePayload::Skill with is_new)
- Produces: `SkillCreationContext` Component, `SkillCreationRequestMessage` Component, `SkillCreationWritebackMessage` Component, `WorkItemType::SkillCreation`, `ToolAction::SubmitSkillCandidate { name, description }`

- [x] **Step 1: Add `SkillCreationContext` to `src/domain/contribution.rs`**

After the existing `SkillUpdateContext` struct (line 650), add:

```rust
/// skill-creator workitem 的上下文 Component
///
/// 由 skill_creation_workitem_system 在 spawn `WorkItemType::SkillCreation` workitem 时
/// 一并注入到同一 entity，供 orchestrator（工具执行）和 writeback_system 通过 Query 读取。
#[derive(Component, Debug, Clone)]
pub struct SkillCreationContext {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub agent_name: String,
    pub sandbox_dir: PathBuf,
    pub skill_name: String,
}
```

Note: needs `use std::path::PathBuf;` — check if it's already imported.

- [x] **Step 2: Add `SkillCreationRequestMessage` and `SkillCreationWritebackMessage` to `src/domain/message.rs`**

After the existing `SkillUpdateRequestMessage` (line 542), add:

```rust
/// /skill 命令触发的 skill 创建请求消息。
///
/// 由 command_parse_system spawn，由 skill_creation_workitem_system 消费，
/// 构造 skill-creator WorkItem。
#[derive(Debug, Clone, Component)]
pub struct SkillCreationRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub agent_name: String,
    pub intent: String,
}

/// skill 创建写回消息：用户确认后由 approval system insert 到 WorkItem entity，
/// 由 skill_creation_writeback_system 消费执行 rename 写回。
#[derive(Debug, Clone, Component)]
pub struct SkillCreationWritebackMessage {
    pub candidate_id: uuid::Uuid,
    pub task_id: TaskId,
}
```

- [x] **Step 3: Add `WorkItemType::SkillCreation` and `required_tag` to `src/domain/work_item.rs`**

Add to `WorkItemType` enum after `SkillUpdate`:

```rust
/// skill 创建工作项：由 skill-creator Agent 消费，根据用户意图生成新 skill
SkillCreation,
```

Add to `required_tag()` match:

```rust
WorkItemType::SkillCreation => "skill-creator",
```

Add `skill_creation()` factory method after `skill_update()`:

```rust
/// 创建 skill 创建工作项
///
/// 具体的 `SkillCreationContext` 由调用方作为独立 Component 注入到同一 entity，
/// 不存储在 WorkItem 中。multi_turn = true：skill-creator 需要多轮工具调用。
pub fn skill_creation(
    task_id: TaskId,
    prompt: String,
    conversation: Vec<ConversationMessage>,
    tools: Vec<ToolDefinition>,
    governing_agent_id: AgentId,
) -> Self {
    let context = WorkItemContext {
        conversation: Some(conversation),
        tools,
        system_prompt: None,
    };
    let input = WorkItemInput { prompt, context };
    let mut wi = Self::new(
        task_id,
        WorkItemType::SkillCreation,
        input,
        WorkItemOrigin::ExperienceCollection,
        WorkItemWritebackTarget::ExperienceInbox,
    );
    wi.governing_agent_id = Some(governing_agent_id);
    wi
}
```

- [x] **Step 4: Add `ToolAction::SubmitSkillCandidate` to `src/domain/space.rs`**

Add to `ToolAction` enum after `SubmitSkillUpdate`:

```rust
SubmitSkillCandidate { name: String, description: String },
```

- [x] **Step 5: Write unit tests**

In `work_item.rs` tests, add:

```rust
#[test]
fn required_tag_skill_creation() {
    assert_eq!(WorkItemType::SkillCreation.required_tag(), "skill-creator");
}
```

- [x] **Step 6: Run `cargo test --all-features` to verify compilation and tests**

Run: `cargo test --all-features`
Expected: All tests pass

- [x] **Step 7: Commit**

```bash
git add src/domain/contribution.rs src/domain/message.rs src/domain/work_item.rs src/domain/space.rs
git commit -m "feat(domain): add SkillCreation types, messages, WorkItemType and ToolAction variant"
```

---

### Task 3: 工具层 — submit_skill + write_skill_file 工具

**Files:**
- Create: `src/systems/tools/builtin/submit_skill.rs`
- Create: `src/systems/tools/builtin/write_skill_file.rs`
- Modify: `src/domain/tool_async.rs` (add ToolEffect::WriteSkillFile)
- Modify: `src/systems/tools/builtin/mod.rs` (register new tools)

**Interfaces:**
- Consumes: Task 2 (ToolAction::SubmitSkillCandidate)
- Produces: `SubmitSkillTool`, `WriteSkillFileTool`, `ToolEffect::WriteSkillFile { path: String, content: String }`

- [x] **Step 1: Add `ToolEffect::WriteSkillFile` variant to `src/domain/tool_async.rs`**

Add to the `ToolEffect` enum after the `ScheduleTask` variant:

```rust
/// 写入 skill 沙盒文件：由 write_skill_file 工具声明，commit_tool_effects_system 在主线程落账。
WriteSkillFile {
    /// 相对沙盒路径
    path: String,
    /// 文件内容
    content: String,
},
```

- [x] **Step 2: Create `src/systems/tools/builtin/submit_skill.rs`**

Following the exact pattern of `submit_skill_update.rs`:

```rust
use crate::domain::{ToolAction, ToolContext, ToolError};
use crate::infrastructure::skills::BuiltinTool;

/// submit_skill 工具：skill-creator 专用提交工具（Sync）。
///
/// 工具只解析参数返回 ToolAction，验证和候选构造由 orchestrator 执行。
pub struct SubmitSkillTool;

impl BuiltinTool for SubmitSkillTool {
    fn name(&self) -> &str {
        "submit_skill"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if name.is_empty() {
            return Err(ToolError::InvalidInput(
                "name is required and must not be empty".to_string(),
            ));
        }
        if description.is_empty() {
            return Err(ToolError::InvalidInput(
                "description is required and must not be empty".to_string(),
            ));
        }

        Ok(ToolAction::SubmitSkillCandidate {
            name: name.to_string(),
            description: description.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, SharedKnowledgeBase, TaskId, ToolContext};
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext<'static> {
        // SAFETY: test-only; references outlive the call.
        let knowledge = Box::leak(Box::new(SharedKnowledgeBase::default()));
        let store = Box::leak(Box::new(crate::domain::ExperienceStore::default()));
        ToolContext {
            knowledge,
            experience_store: store,
            default_wait_tasks_timeout_secs: 60,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 30,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: TaskId::nil(),
            current_agent_id: AgentId::nil(),
            current_origin_channel: None,
            current_skill_dir: None,
        }
    }

    #[test]
    fn submit_skill_returns_submit_action() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let action = tool
            .execute(
                &serde_json::json!({
                    "name": "code-review",
                    "description": "审查代码质量"
                }),
                &ctx,
            )
            .unwrap();

        match action {
            ToolAction::SubmitSkillCandidate { name, description } => {
                assert_eq!(name, "code-review");
                assert_eq!(description, "审查代码质量");
            }
            other => panic!("expected SubmitSkillCandidate, got: {:?}", other),
        }
    }

    #[test]
    fn submit_skill_rejects_empty_name() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "",
                "description": "desc"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("name")
        ));
    }

    #[test]
    fn submit_skill_rejects_empty_description() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "my-skill",
                "description": ""
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("description")
        ));
    }

    #[test]
    fn submit_skill_rejects_missing_name() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "description": "desc"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("name")
        ));
    }

    #[test]
    fn submit_skill_rejects_missing_description() {
        let ctx = test_ctx();
        let tool = SubmitSkillTool;
        let result = tool.execute(
            &serde_json::json!({
                "name": "my-skill"
            }),
            &ctx,
        );
        assert!(matches!(
            result,
            Err(ToolError::InvalidInput(msg)) if msg.contains("description")
        ));
    }
}
```

- [x] **Step 3: Create `src/systems/tools/builtin/write_skill_file.rs`**

Async tool following the pattern of shell_exec but simpler:

```rust
use crate::domain::{ToolActionKind, ToolContext, ToolError};
use crate::domain::tool_async::{ToolAsyncResult, ToolEffect};
use crate::infrastructure::skills::BuiltinTool;
use crate::infrastructure::skills::diff::ALLOWED_FILE_SUFFIXES;

/// write_skill_file 工具：skill-creator 专用文件写入工具（Async）。
///
/// Worker 中执行：从 ToolContext.current_skill_dir 获取沙盒根路径 →
/// 路径安全验证 → 后缀白名单验证 → 创建父目录 → 声明式写效果。
pub struct WriteSkillFileTool;

impl BuiltinTool for WriteSkillFileTool {
    fn name(&self) -> &str {
        "write_skill_file"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn run_async(
        &self,
        input: &serde_json::Value,
        ctx: &crate::domain::OwnedToolContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolAsyncResult> + Send + '_>> {
        let input = input.clone();
        let ctx = ctx.clone();

        Box::pin(async move {
            let path = match input.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.trim().to_string(),
                None => {
                    return ToolAsyncResult::completed(
                        "",
                        Err(ToolError::InvalidInput("path is required".to_string())),
                    );
                }
            };
            let content = match input.get("content").and_then(|v| v.as_str()) {
                Some(c) => c.to_string(),
                None => {
                    return ToolAsyncResult::completed(
                        "",
                        Err(ToolError::InvalidInput("content is required".to_string())),
                    );
                }
            };

            // 获取沙盒目录
            let skill_dir = match &ctx.current_skill_dir {
                Some(d) => d.clone(),
                None => {
                    return ToolAsyncResult::completed(
                        "",
                        Err(ToolError::InvalidInput(
                            "no skill directory in current context".to_string(),
                        )),
                    );
                }
            };

            // 路径安全验证：不允许 ../ 逃逸
            if path.contains("..") {
                return ToolAsyncResult::completed(
                    "",
                    Err(ToolError::InvalidInput(format!(
                        "path must not contain '..': {}",
                        path
                    ))),
                );
            }

            // 后缀白名单验证（与 read_skill_file 保持一致）
            let suffix = std::path::Path::new(&path)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            // 无后缀的文件（如 SKILL.md 中 .md 会被检测到）也需要检查
            if !path.ends_with('/') && !suffix.is_empty() {
                let dot_suffix = format!(".{}", suffix);
                if !ALLOWED_FILE_SUFFIXES.contains(&dot_suffix.as_str()) {
                    return ToolAsyncResult::completed(
                        "",
                        Err(ToolError::InvalidInput(format!(
                            "file suffix '.{}' not allowed; allowed: {:?}",
                            suffix, ALLOWED_FILE_SUFFIXES
                        ))),
                    );
                }
            } else if suffix.is_empty() {
                // 无后缀文件（如 Makefile）不允许
                // 但 SKILL.md 等有后缀的通过上面的检查
                // 这里检查：如果路径不含 '.'，视为无后缀
                let file_name = std::path::Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if !file_name.contains('.') {
                    return ToolAsyncResult::completed(
                        "",
                        Err(ToolError::InvalidInput(format!(
                            "file without extension not allowed: {}",
                            path
                        ))),
                    );
                }
            }

            // 声明式写效果：交由 commit_tool_effects_system 落账
            ToolAsyncResult::effect("", ToolEffect::WriteSkillFile { path, content })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::tool_async::ToolWorkerPayload;

    #[test]
    fn write_skill_file_is_async() {
        let tool = WriteSkillFileTool;
        assert_eq!(tool.kind(), ToolActionKind::Async);
    }

    #[tokio::test]
    async fn write_skill_file_rejects_path_traversal() {
        let tool = WriteSkillFileTool;
        let mut ctx = crate::domain::OwnedToolContext::default();
        ctx.current_skill_dir = Some(std::path::PathBuf::from("/tmp/sandbox"));

        let result = tool
            .run_async(
                &serde_json::json!({
                    "path": "../escape.md",
                    "content": "bad"
                }),
                &ctx,
            )
            .await;

        match result.payload {
            ToolWorkerPayload::Completed(Err(ToolError::InvalidInput(msg))) => {
                assert!(msg.contains(".."));
            }
            other => panic!("expected InvalidInput error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn write_skill_file_rejects_missing_skill_dir() {
        let tool = WriteSkillFileTool;
        let ctx = crate::domain::OwnedToolContext::default(); // current_skill_dir = None

        let result = tool
            .run_async(
                &serde_json::json!({
                    "path": "test.md",
                    "content": "hello"
                }),
                &ctx,
            )
            .await;

        match result.payload {
            ToolWorkerPayload::Completed(Err(ToolError::InvalidInput(msg))) => {
                assert!(msg.contains("no skill directory"));
            }
            other => panic!("expected InvalidInput error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn write_skill_file_produces_write_effect() {
        let tool = WriteSkillFileTool;
        let mut ctx = crate::domain::OwnedToolContext::default();
        ctx.current_skill_dir = Some(std::path::PathBuf::from("/tmp/sandbox"));

        let result = tool
            .run_async(
                &serde_json::json!({
                    "path": "SKILL.md",
                    "content": "---\nname: test\n---\nbody"
                }),
                &ctx,
            )
            .await;

        match result.payload {
            ToolWorkerPayload::Effect(ToolEffect::WriteSkillFile { path, content }) => {
                assert_eq!(path, "SKILL.md");
                assert!(content.contains("test"));
            }
            other => panic!("expected WriteSkillFile effect, got: {:?}", other),
        }
    }
}
```

- [x] **Step 4: Register new tools in `src/systems/tools/builtin/mod.rs`**

Add module declarations and re-exports:

```rust
pub mod submit_skill;
pub mod write_skill_file;

pub use submit_skill::SubmitSkillTool;
pub use write_skill_file::WriteSkillFileTool;
```

- [x] **Step 5: Register tools in the tool registry**

Find where `BuiltinToolExecutors` is populated (likely in `src/systems/tools/mod.rs` or a plugin), and add:

```rust
executors.register(Box::new(SubmitSkillTool));
executors.register(Box::new(WriteSkillFileTool));
```

- [x] **Step 6: Add `WriteSkillFile` arm to `commit_tool_effects_system`**

Find the `commit_tool_effects_system` (in `src/systems/tools/` or similar) and add a match arm for `ToolEffect::WriteSkillFile { path, content }` that:
1. Resolves the full path from the WorkItem's `SkillCreationContext.sandbox_dir` + `path`
2. Creates parent directories
3. Writes the file content
4. Spawns a `ToolExecutionResultMessage` with success result

- [x] **Step 7: Run `cargo test --all-features`**

Run: `cargo test --all-features`
Expected: All tests pass

- [x] **Step 8: Commit**

```bash
git add src/domain/tool_async.rs src/systems/tools/builtin/submit_skill.rs src/systems/tools/builtin/write_skill_file.rs src/systems/tools/builtin/mod.rs
git commit -m "feat(tools): add submit_skill and write_skill_file builtin tools with WriteSkillFile effect"
```

---

### Task 4: 基础设施层 — SkillLoader 过滤 .sandbox 目录

**Files:**
- Modify: `src/infrastructure/skills/loader.rs:68-86` (load_skills)
- Modify: `src/infrastructure/skills/loader.rs:120-156` (build_registry)

**Interfaces:**
- Consumes: None
- Produces: SkillLoader that skips `.sandbox` directories in both `load_skills()` and `build_registry()`

- [x] **Step 1: Add `.sandbox` filter to `load_skills()`**

In `src/infrastructure/skills/loader.rs`, modify the `filter_map` in `load_skills()` (line 74) to skip entries whose file name starts with `.`:

```rust
.filter_map(|entry| {
    let path = entry.ok()?.path();
    // 跳过隐藏目录（如 .sandbox）
    if path.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false) {
        return None;
    }
    let skill_md = path.join("SKILL.md");
    if skill_md.exists() {
        let content = std::fs::read_to_string(&skill_md).ok()?;
        parse_skill_md(&content, path)
    } else {
        None
    }
})
```

- [x] **Step 2: Add `.sandbox` filter to `build_registry()`**

In the inner loop of `build_registry()` (line 129), add the same filter:

```rust
for skill_entry in skill_entries.flatten() {
    let skill_path_raw = skill_entry.path();
    // 跳过隐藏目录（如 .sandbox）
    if skill_path_raw
        .file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
    {
        continue;
    }
    let skill_path = skill_entry.path().join("SKILL.md");
    let skill_dir = skill_entry.path();
    // ... rest unchanged
}
```

- [x] **Step 3: Write unit test**

In the existing `registry_build_tests` module:

```rust
#[test]
fn build_registry_skips_hidden_directories() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".harness").join("assets").join("agents");

    // 正常 skill
    write_skill(
        &agents_dir,
        "agent-a",
        "coding",
        "---\nname: coding\ndescription: coding skill\n---\n\n## Usage\n\nDo it.\n",
    );
    // .sandbox 下的未完成 skill
    write_skill(
        &agents_dir,
        "agent-a",
        ".sandbox",
        "---\nname: draft\ndescription: draft\n---\n\n## Usage\n\nDraft.\n",
    );

    let loader = SkillLoader {
        base_dir: agents_dir.clone(),
    };
    let registry = loader.build_registry();

    assert_eq!(registry.skills.len(), 1, "should skip .sandbox directory");
    assert!(registry.get(&SkillId::new("agent-a", "coding")).is_some());
    assert!(registry.get(&SkillId::new("agent-a", ".sandbox")).is_none());
}

#[test]
fn load_skills_skips_hidden_directories() {
    let tmp = TempDir::new().unwrap();
    let agents_dir = tmp.path().join(".harness").join("assets").join("agents");

    write_skill(
        &agents_dir,
        "agent-a",
        "coding",
        "---\nname: coding\ndescription: coding skill\n---\n\n## Usage\n\nDo it.\n",
    );
    write_skill(
        &agents_dir,
        "agent-a",
        ".sandbox",
        "---\nname: draft\ndescription: draft\n---\n\n## Usage\n\nDraft.\n",
    );

    let loader = SkillLoader {
        base_dir: agents_dir,
    };
    let skills = loader.load_skills("agent-a");

    assert_eq!(skills.len(), 1, "should skip .sandbox directory");
    assert_eq!(skills[0].name, "coding");
}
```

- [x] **Step 4: Run tests**

Run: `cargo test --all-features`
Expected: All tests pass

- [x] **Step 5: Commit**

```bash
git add src/infrastructure/skills/loader.rs
git commit -m "fix(skills): filter .sandbox directories in load_skills and build_registry"
```

---

### Task 5: 系统层 — command_parse_system 处理 CreateSkill 分支

**Files:**
- Modify: `src/systems/command.rs` (add CreateSkill match arm)

**Interfaces:**
- Consumes: Task 2 (SkillCreationRequestMessage)
- Produces: command_parse_system spawns SkillCreationRequestMessage when user types `/skill`

- [x] **Step 1: Add `UserCommand::CreateSkill` match arm to `command_parse_system`**

In `src/systems/command.rs`, add a new match arm after `ClearCurrentTask`:

```rust
UserCommand::CreateSkill { intent } => {
    if intent.is_empty() {
        eprintln!("[skill] usage: /skill <intent description>");
    } else {
        // 查找当前活跃任务的 Agent（与 /finish 同逻辑：同 channel、非终态）
        let current_task = tasks.iter().find(|(t, _)| {
            !t.status.is_terminal()
                && t.origin_channel == Some(input.origin_channel.clone())
        });

        if let Some((task, _)) = current_task {
            // TODO: 获取 Agent 信息 — 需要从 task 的 delegate 或 ToolCallingState 中获取
            // 当前 MVP：使用 task 的 creator 作为 agent_id 的占位
            debug!(
                event = "SkillCreationCommandReceived",
                task_id = %task.id,
                intent = %intent,
                "spawning skill creation request"
            );
            commands.spawn(crate::domain::SkillCreationRequestMessage {
                task_id: task.id,
                agent_id: task.creator, // 占位，后续由 workitem 系统修正
                agent_name: String::new(), // 占位
                intent,
            });
        } else {
            eprintln!("[skill] no active task — /skill requires an active task");
        }
    }
    commands.entity(entity).despawn();
}
```

**Important:** The `agent_id` and `agent_name` fields need real values. Look at how the existing `/finish` command finds the active task, and how the `task.delegate` or `ToolCallingState` provides agent information. The design doc says "查找当前活跃任务的 Agent"，so we need to find the agent currently executing the task. Check how `task.delegate` works and use it if available.

- [x] **Step 2: Verify import for `SkillCreationRequestMessage`**

Ensure the import at the top of `command.rs` includes the new message type. It's likely imported through `crate::domain::*` or needs explicit addition.

- [x] **Step 3: Write unit test**

In the `#[cfg(test)]` module of `command.rs`, add a test similar to the existing `clear_command_spawns_clear_task_message`:

```rust
#[test]
fn skill_command_spawns_creation_request() {
    use crate::domain::{FrontendKind, SkillCreationRequestMessage, Task, TaskStatus};

    let mut app = App::new();
    app.insert_resource(MemoryConfig::default());
    app.insert_resource(SharedKnowledgeBase::default());
    app.insert_resource(PendingKnowledgeWriteHooks::default());
    app.add_systems(Update, command_parse_system);

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    let task_id = uuid::Uuid::new_v4();
    app.world_mut().spawn((
        Task {
            id: task_id,
            content: "active task".to_string(),
            creator: uuid::Uuid::new_v4(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    app.world_mut().spawn(UserInputMessage {
        content: "/skill 做代码审查".to_string(),
        origin_channel: channel,
    });

    app.update();

    let msgs: Vec<&SkillCreationRequestMessage> = app
        .world_mut()
        .query::<&SkillCreationRequestMessage>()
        .iter(app.world())
        .collect();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].intent, "做代码审查");
    assert_eq!(msgs[0].task_id, task_id);
}

#[test]
fn skill_command_empty_intent_prints_usage() {
    // /skill with no argument should print usage and not spawn request
    // This is hard to test directly (eprintln), but we can verify no message is spawned
    use crate::domain::{FrontendKind, SkillCreationRequestMessage, Task, TaskStatus};

    let mut app = App::new();
    app.insert_resource(MemoryConfig::default());
    app.insert_resource(SharedKnowledgeBase::default());
    app.insert_resource(PendingKnowledgeWriteHooks::default());
    app.add_systems(Update, command_parse_system);

    let channel = ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "test".to_string(),
        thread_id: None,
    };
    let now = chrono::Utc::now();
    app.world_mut().spawn((
        Task {
            id: uuid::Uuid::new_v4(),
            content: "active task".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Running,
            pending_confirmation_id: None,
            input_summary: "test".to_string(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: false,
            parent_task_id: None,
            batch_id: None,
            origin_channel: Some(channel.clone()),
            routing_policy: crate::domain::TaskRoutingPolicy::conversational(channel.clone()),
            last_evaluated_turn: None,
        },
        ShortTermMemory::default(),
    ));

    app.world_mut().spawn(UserInputMessage {
        content: "/skill".to_string(),
        origin_channel: channel,
    });

    app.update();

    let msgs: Vec<&SkillCreationRequestMessage> = app
        .world_mut()
        .query::<&SkillCreationRequestMessage>()
        .iter(app.world())
        .collect();
    assert!(msgs.is_empty(), "empty intent should not spawn request");
}
```

- [x] **Step 4: Run tests**

Run: `cargo test --all-features`
Expected: All tests pass

- [x] **Step 5: Commit**

```bash
git add src/systems/command.rs
git commit -m "feat(command): handle /skill command in command_parse_system"
```

---

### Task 6: 系统层 — skill_creation_workitem_system + skill_creation_writeback_system

**Files:**
- Create: `src/systems/experience/skill_creation.rs`
- Modify: `src/systems/experience/mod.rs` (add module + re-exports)
- Modify: `src/plugins/execution.rs` (register new systems)

**Interfaces:**
- Consumes: Task 2 (SkillCreationContext, SkillCreationRequestMessage, SkillCreationWritebackMessage, WorkItemType::SkillCreation), Task 1 (is_skill_new, skill_new)
- Produces: `skill_creation_workitem_system`, `skill_creation_writeback_system`

- [x] **Step 1: Create `src/systems/experience/skill_creation.rs`**

This is the largest new file. It contains two systems:

```rust
use crate::prelude::*;
use std::path::PathBuf;
use tracing::{debug, warn};

use crate::domain::{
    AgentId, ExperienceCandidate, ExperienceCandidateStatus, ExperienceKindHint,
    ExperienceStore, PendingDispatch, PendingExperienceHooks, SkillCreationContext,
    SkillCreationRequestMessage, SkillCreationWritebackMessage, TaskId, ToolDefinition,
    ToolExecutorKind, ToolPermission, ToolSchema, WorkItem, WorkItemType,
};
use crate::infrastructure::skills::{SkillLoader, SkillRegistry};
use crate::user_plugins::hook_point::HookPoint;

/// skill 创建 workitem 系统：消费 SkillCreationRequestMessage，创建 WorkItem。
///
/// 1. 创建 .sandbox/<draft-name>/ 沙盒目录
/// 2. 从 SkillRegistry 获取已有 skill 列表
/// 3. 从 Agent 组件获取 profile（通过 task → delegate → agent 链路）
/// 4. 构造 prompt
/// 5. 过滤工具（submit_skill + write_skill_file + read_skill_file）
/// 6. 创建 WorkItem + SkillCreationContext + PendingDispatch
pub(crate) fn skill_creation_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &SkillCreationRequestMessage)>,
    skill_loader: Res<SkillLoader>,
    skill_registry: Res<SkillRegistry>,
) {
    for (entity, request) in &requests {
        let task_id = request.task_id;
        let agent_name = &request.agent_name;
        let intent = &request.intent;

        // 创建沙盒目录
        let skills_dir = skill_loader.base_dir.join(agent_name).join("skills");
        let sandbox_name = format!("_draft_{}", chrono::Utc::now().timestamp_millis());
        let sandbox_dir = skills_dir.join(".sandbox").join(&sandbox_name);
        if let Err(e) = std::fs::create_dir_all(&sandbox_dir) {
            warn!(
                event = "SkillCreationSandboxCreationFailed",
                path = %sandbox_dir.display(),
                error = %e,
                "failed to create sandbox directory"
            );
            commands.entity(entity).despawn();
            continue;
        }

        // 获取已有 skill 列表
        let existing_skills = skill_loader.load_skills(agent_name);
        let skills_summary: Vec<String> = existing_skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect();

        // 构造 prompt
        let prompt = format!(
            "请为 Agent「{}」创建新 skill。\n\n\
             ## 用户意图\n\n{}\n\n\
             ## 已有 skill 列表\n\n{}\n\n\
             ## SKILL.md 模板规范\n\n\
             SKILL.md 必须包含 YAML frontmatter：\n\
             ```\n\
             ---\n\
             name: <skill 名称>\n\
             description: <一句话描述>\n\
             version: 1\n\
             self_updatable: true\n\
             ---\n\
             ```\n\n\
             body 要求：\n\
             - 至少包含一个 ## 二级标题\n\
             - 第一个 ## 标题下必须有实质内容\n\n\
             ## 工作流程\n\n\
             1. 使用 read_skill_file 读取已有 skill，理解现有能力和风格\n\
             2. 使用 write_skill_file 创建 SKILL.md 和辅助文件\n\
             3. 完成后调用 submit_skill(name, description) 提交",
            agent_name,
            intent,
            if skills_summary.is_empty() {
                "(无)".to_string()
            } else {
                skills_summary.join("\n")
            }
        );

        // 过滤工具
        let tools = vec![
            make_tool_def("submit_skill", "提交创建的 skill", "object", &[
                ("name", "string", "skill 名称"),
                ("description", "string", "skill 描述"),
            ]),
            make_tool_def("write_skill_file", "写入沙盒文件", "object", &[
                ("path", "string", "相对沙盒路径"),
                ("content", "string", "文件内容"),
            ]),
            make_tool_def("read_skill_file", "读取已有 skill 文件", "object", &[
                ("path", "string", "相对 skill 目录路径"),
            ]),
        ];

        // 创建 WorkItem
        let work_item = WorkItem::skill_creation(
            task_id,
            prompt,
            vec![], // 无对话历史
            tools,
            request.agent_id,
        );

        let creation_context = SkillCreationContext {
            task_id,
            agent_id: request.agent_id,
            agent_name: agent_name.clone(),
            sandbox_dir: sandbox_dir.clone(),
            skill_name: String::new(), // LLM 提交时更新
        };

        debug!(
            event = "SkillCreationWorkItemCreated",
            task_id = %task_id,
            agent_name = %agent_name,
            sandbox_dir = %sandbox_dir.display(),
            intent = %intent,
            "spawned skill-creator WorkItem"
        );

        commands.spawn((
            work_item,
            creation_context,
            PendingDispatch {
                kind: crate::domain::DispatchKind::WorkItem(WorkItemType::SkillCreation),
                hint: crate::domain::DispatchHint {
                    strategy: crate::domain::DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));

        commands.entity(entity).despawn();
    }
}

/// skill 创建写回系统：消费 SkillCreationWritebackMessage，执行 rename 写回。
///
/// 1. 检查正式目录是否同名 → 同名则拒绝
/// 2. rename 沙盒到正式目录
/// 3. SkillRegistry 同步注册
/// 4. 候选状态置 Persisted
/// 5. despawn WorkItem 实体
pub(crate) fn skill_creation_writeback_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    mut skill_registry: ResMut<SkillRegistry>,
    skill_loader: Res<SkillLoader>,
    messages: Query<(Entity, &SkillCreationWritebackMessage, &SkillCreationContext, &WorkItem)>,
) {
    for (entity, message, context, _work_item) in &messages {
        let skill_name = &context.skill_name;
        let sandbox_dir = &context.sandbox_dir;

        // 检查正式目录是否同名
        let target_dir = skill_loader
            .base_dir
            .join(&context.agent_name)
            .join("skills")
            .join(skill_name);

        if target_dir.exists() {
            warn!(
                event = "SkillCreationNameConflict",
                skill_name = %skill_name,
                target_dir = %target_dir.display(),
                "skill with same name already exists, rejecting writeback"
            );
            // 候选状态保持不变，用户可通过 skill-updater 更新
            // 不 despawn，让用户在 TUI 看到冲突信息
            continue;
        }

        // rename 沙盒到正式目录
        if let Err(e) = std::fs::rename(sandbox_dir, &target_dir) {
            warn!(
                event = "SkillCreationRenameFailed",
                sandbox_dir = %sandbox_dir.display(),
                target_dir = %target_dir.display(),
                error = %e,
                "failed to rename sandbox to target directory"
            );
            if let Some(c) = store.candidates.get_mut(&message.candidate_id) {
                c.status = ExperienceCandidateStatus::WritebackFailed;
            }
            continue;
        }

        // SkillRegistry 同步注册
        let skill_id = crate::infrastructure::skills::SkillId::new(
            context.agent_name.clone(),
            skill_name.clone(),
        );
        let skill_md_path = target_dir.join("SKILL.md");
        if let Ok(content) = std::fs::read_to_string(&skill_md_path) {
            if let Some(loaded) = crate::infrastructure::skills::loader::parse_skill_md(
                &content,
                target_dir.clone(),
            ) {
                let entry = crate::infrastructure::skills::SkillEntry {
                    skill_id,
                    name: loaded.name,
                    description: loaded.description,
                    instructions: loaded.instructions,
                    version: loaded.version,
                    owner_agent_name: context.agent_name.clone(),
                    self_updatable: loaded.self_updatable,
                };
                skill_registry.upsert(entry);

                debug!(
                    event = "SkillCreationRegistryUpserted",
                    skill_name = %skill_name,
                    agent_name = %context.agent_name,
                    "new skill registered in SkillRegistry"
                );
            }
        }

        // 候选状态置 Persisted
        if let Some(c) = store.candidates.get_mut(&message.candidate_id) {
            c.status = ExperienceCandidateStatus::Persisted;
        }

        debug!(
            event = "SkillCreationWritebackCompleted",
            skill_name = %skill_name,
            candidate_id = %message.candidate_id,
            "skill creation writeback completed"
        );

        commands.entity(entity).despawn();
    }
}

/// 辅助函数：构造 ToolDefinition
fn make_tool_def(
    name: &str,
    description: &str,
    param_type: &str,
    properties: &[(&str, &str, &str)],
) -> ToolDefinition {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (prop_name, prop_type, prop_desc) in properties {
        props.insert(
            prop_name.to_string(),
            serde_json::json!({
                "type": prop_type,
                "description": prop_desc,
            }),
        );
        required.push(prop_name.to_string());
    }

    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters: ToolSchema::from(serde_json::json!({
            "type": param_type,
            "properties": props,
            "required": required,
        })),
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin(name.to_string()),
        required_tag: None,
    }
}
```

Note: Check the actual `DispatchHint` and `DispatchStrategy` types — they may use different variant names. Read the current `skill_update_workitem_system` for the exact pattern.

- [x] **Step 2: Register module in `src/systems/experience/mod.rs`**

Add:

```rust
pub mod skill_creation;

pub use skill_creation::{skill_creation_workitem_system, skill_creation_writeback_system};
```

- [x] **Step 3: Register systems in `src/plugins/execution.rs`**

Following the pattern of `skill_update_workitem_system` and `skill_update_completion_system`:

```rust
// In the appropriate system set, after experience_governance_system:
skill_creation_workitem_system
    .after(experience_governance_system)
    .in_set(HarnessSet::Execution),

// After experience_approval_result_system:
skill_creation_writeback_system
    .after(experience_approval_result_system)
    .in_set(HarnessSet::Execution),
```

Check the exact placement by reading the current `execution.rs` and matching the `skill_update` system positions.

- [x] **Step 4: Run `cargo test --all-features`**

Run: `cargo test --all-features`
Expected: Compiles and passes

- [x] **Step 5: Commit**

```bash
git add src/systems/experience/skill_creation.rs src/systems/experience/mod.rs src/plugins/execution.rs
git commit -m "feat(experience): add skill_creation_workitem_system and skill_creation_writeback_system"
```

---

### Task 7: 治理与审批集成 — governance is_new 路由 + approval SkillCreation 目标

**Files:**
- Modify: `src/systems/experience/governance.rs` (add is_new early return in Skill branch)
- Modify: `src/systems/experience/approval.rs` (handle SkillCreation destination)

**Interfaces:**
- Consumes: Task 1 (is_skill_new), Task 2 (SkillCreationWritebackMessage, SkillCreationContext, ExperienceWritebackDestination::SkillCreation)
- Produces: governance routes `is_new == true` to `SkillCreation`, approval inserts `SkillCreationWritebackMessage` to WorkItem entity

- [x] **Step 1: Add `is_new` early return in governance `ExperienceKindHint::Skill` branch**

In `src/systems/experience/governance.rs`, at the start of the `ExperienceKindHint::Skill =>` branch (line 95), before the `if is_default` check, add:

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
```

Add the import at the top:

```rust
use crate::domain::is_skill_new;
```

- [x] **Step 2: Handle `ExperienceWritebackDestination::SkillCreation` in governance**

After the existing `SkillUpdate` destination handling (the block that spawns `SkillUpdateRequestMessage`), add a new arm. When `decision.destination == SkillCreation`, the candidate needs user confirmation (already set by `requires_user_confirmation: true`), so it should go through the confirmation flow. Check the existing flow for `SkillPackage` destination — `SkillCreation` should follow the same pattern.

The key insight: `SkillCreation` has `requires_user_confirmation: true`, so it enters the confirmation branch (line 198). In that branch, `spawn_experience_confirmation` is called, which creates a `ToolConfirmationRequestMessage`. After user approves, `experience_approval_result_system` picks up the approval. We need to handle the `SkillCreation` destination there.

- [x] **Step 3: Handle `SkillCreation` destination in `experience_approval_result_system`**

In `src/systems/experience/approval.rs`, in the `experience_approval_result_system` function, when the decision's destination is `SkillCreation`, instead of spawning `ExperienceWritebackRequestMessage`, we insert `SkillCreationWritebackMessage` to the WorkItem entity.

Find the section where the approval spawns `ExperienceWritebackRequestMessage` (line 119). Before that, add a check for `SkillCreation`:

```rust
// SkillCreation destination：insert SkillCreationWritebackMessage to WorkItem entity
if decision.destination == ExperienceWritebackDestination::SkillCreation {
    // 通过 task_id 遍历找到 WorkItem entity
    let wi_query: Query<(Entity, &WorkItem, &SkillCreationContext)> = /* need to add */;
    // ...
}
```

Wait — the approval system currently doesn't have a Query for `SkillCreationContext`. We need to add it as a parameter. Read the current function signature and add a new Query parameter:

```rust
skill_creation_contexts: Query<(Entity, &SkillCreationContext, &WorkItem)>,
```

Then, when `decision.destination == SkillCreation`:

```rust
if decision.destination == ExperienceWritebackDestination::SkillCreation {
    // 通过 task_id 找到带 SkillCreationContext 的 WorkItem entity
    if let Some((wi_entity, _, _)) = skill_creation_contexts
        .iter()
        .find(|(_, ctx, _)| ctx.task_id == decision.source_task_id)
    {
        commands.entity(wi_entity).insert(SkillCreationWritebackMessage {
            candidate_id,
            task_id: decision.source_task_id,
        });
        debug!(
            event = "SkillCreationWritebackMessageInserted",
            candidate_id = %candidate_id,
            task_id = %decision.source_task_id,
            "inserted SkillCreationWritebackMessage to WorkItem entity"
        );
    } else {
        warn!(
            event = "SkillCreationContextNotFound",
            candidate_id = %candidate_id,
            task_id = %decision.source_task_id,
            "no SkillCreationContext found for approved candidate"
        );
    }
    commands.entity(decision_entity).despawn();
    commands.entity(entity).despawn();
    continue;
}
```

Add the necessary imports.

- [x] **Step 4: Write unit test for governance is_new routing**

In `governance.rs` tests:

```rust
#[test]
fn governance_routes_is_new_skill_to_skill_creation() {
    let mut app = make_governance_app();

    let agent_id = uuid::Uuid::new_v4();
    let task_id = uuid::Uuid::new_v4();
    let candidate_id = uuid::Uuid::new_v4();

    register_agent(&mut app, agent_id, make_agent(agent_id, "worker", &["llm"]));
    register_task(&mut app, task_id, make_task(task_id), None);

    // Skill 类候选，is_new = true
    let mut candidate = make_skill_candidate(candidate_id, task_id, agent_id);
    // Override payload to set is_new = true
    candidate.payload = ExperienceCandidatePayload::Skill {
        name: "new-skill".to_string(),
        description: "a new skill".to_string(),
        instructions: "do something".to_string(),
        file_refs: vec![],
        is_new: true,
    };
    candidate.status = ExperienceCandidateStatus::GovernancePending;
    stage_candidate(&mut app, candidate);

    app.world_mut()
        .spawn(ExperienceGovernanceRequestMessage { task_id, agent_id });

    app.update();

    let decisions = governance_decision_destinations(&mut app);
    assert_eq!(
        decisions,
        vec![ExperienceWritebackDestination::SkillCreation],
        "is_new=true Skill should route to SkillCreation"
    );
}
```

- [x] **Step 5: Run tests**

Run: `cargo test --all-features`
Expected: All tests pass

- [x] **Step 6: Commit**

```bash
git add src/systems/experience/governance.rs src/systems/experience/approval.rs
git commit -m "feat(governance): route is_new Skill candidates to SkillCreation destination and handle approval"
```

---

### Task 8: Orchestrator 集成 — SubmitSkillCandidate 处理 + context_queries 扩展 + current_skill_dir 填充

**Files:**
- Modify: `src/systems/tools/orchestrator.rs` (SubmitSkillCandidate arm + context_queries extension)
- Modify: `src/systems/tools/dispatch.rs` (current_skill_dir from SkillCreationContext/SkillUpdateContext)
- Modify: `src/systems/tools/async_dispatch.rs` (same)

**Interfaces:**
- Consumes: Task 2 (ToolAction::SubmitSkillCandidate, SkillCreationContext), Task 1 (skill_new, is_skill_new)
- Produces: orchestrator handles SubmitSkillCandidate with validation, dispatch fills current_skill_dir

- [x] **Step 1: Extend `context_queries` in orchestrator to include `SkillCreationContext`**

In `src/systems/tools/orchestrator.rs`, change the `context_queries` type from:

```rust
context_queries: &Query<(
    Entity,
    Option<&ProfileGenerationContext>,
    Option<&SkillUpdateContext>,
    &WorkItem,
)>,
```

to:

```rust
context_queries: &Query<(
    Entity,
    Option<&ProfileGenerationContext>,
    Option<&SkillUpdateContext>,
    Option<&SkillCreationContext>,
    &WorkItem,
)>,
```

Update all `context_queries` usages in the function to destructure the new tuple element (4th position for `SkillCreationContext`). Search for all `context_queries` references and add the new Option element.

- [x] **Step 2: Add `ToolAction::SubmitSkillCandidate` match arm in orchestrator**

After the existing `SubmitSkillUpdate` arm, add a new arm. This is the core validation logic:

```rust
Ok(ToolAction::SubmitSkillCandidate { name, description }) => {
    // 从 context_queries 找到带 SkillCreationContext 的 WorkItem
    let wi_entity_opt = context_queries.iter().find_map(|(e, _, _, creation_ctx, wi)| {
        if wi.task_id == request.request.task_id && creation_ctx.is_some() {
            Some((e, creation_ctx.unwrap().clone()))
        } else {
            None
        }
    });

    let Some((wi_entity, creation_ctx)) = wi_entity_opt else {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InternalState("SubmitSkillCandidate without SkillCreationContext".to_string()),
        );
        continue; // or return — check the surrounding pattern
    };

    let sandbox_dir = &creation_ctx.sandbox_dir;

    // 1. 验证 SKILL.md 存在
    let skill_md_path = sandbox_dir.join("SKILL.md");
    if !skill_md_path.exists() {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("SKILL.md not found in sandbox directory".to_string()),
        );
        continue;
    }

    // 2. 解析 frontmatter 验证
    let skill_md_content = match std::fs::read_to_string(&skill_md_path) {
        Ok(c) => c,
        Err(e) => {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::InternalState(format!("failed to read SKILL.md: {}", e)),
            );
            continue;
        }
    };

    // 解析 frontmatter
    let parsed = crate::infrastructure::skills::loader::parse_skill_md(
        &skill_md_content,
        sandbox_dir.clone(),
    );
    let Some(parsed) = parsed else {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("SKILL.md frontmatter is invalid or missing".to_string()),
        );
        continue;
    };

    // 验证 name 非空、description 非空、version == 1
    if parsed.name.is_empty() {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("SKILL.md frontmatter 'name' must not be empty".to_string()),
        );
        continue;
    }
    if parsed.description.is_empty() {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("SKILL.md frontmatter 'description' must not be empty".to_string()),
        );
        continue;
    }
    if parsed.version != 1 {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput(format!(
                "new skill must have version 1, got {}",
                parsed.version
            )),
        );
        continue;
    }

    // 3. 路径安全验证：扫描沙盒目录下所有文件
    let sandbox_canonical = match sandbox_dir.canonicalize() {
        Ok(c) => c,
        Err(e) => {
            spawn_tool_error(
                commands,
                request_entity,
                request,
                ToolError::InternalState(format!("failed to canonicalize sandbox dir: {}", e)),
            );
            continue;
        }
    };

    let mut path_safe = true;
    if let Ok(entries) = std::fs::read_dir(sandbox_dir) {
        for entry in entries.flatten() {
            if let Ok(canonical) = entry.path().canonicalize() {
                if !canonical.starts_with(&sandbox_canonical) {
                    path_safe = false;
                    break;
                }
            }
        }
    }
    if !path_safe {
        spawn_tool_error(
            commands,
            request_entity,
            request,
            ToolError::InvalidInput("sandbox contains files outside sandbox directory".to_string()),
        );
        continue;
    }

    // 4. 验证通过 — 扫描沙盒目录生成 file_refs
    let file_refs = scan_sandbox_files(sandbox_dir);

    // 5. 从 SKILL.md 读取 instructions
    let instructions = parsed.instructions;

    // 6. 构造完整 ExperienceCandidate
    let candidate_id = uuid::Uuid::new_v4();
    let candidate = crate::domain::ExperienceCandidate::skill_new(
        candidate_id,
        request.request.task_id,
        request.request.agent_id,
        format!("新建 skill: {}", name),
        name.clone(),
        description.clone(),
        instructions,
        file_refs,
    );

    // 7. 入队 ExperienceStore
    experience_store.stage_root_candidate(candidate);

    // 8. 推入 PendingExperienceHooks
    pending_experience_hooks
        .0
        .push((HookPoint::OnExperienceCandidateSubmitted, candidate_id));

    // 9. 更新 SkillCreationContext.skill_name
    commands.entity(wi_entity).insert(SkillCreationContext {
        skill_name: name.clone(),
        ..creation_ctx.clone()
    });

    // 10. spawn 成功结果
    spawn_tool_success(
        commands,
        request_entity,
        request,
        serde_json::json!({
            "status": "submitted",
            "candidate_id": candidate_id.to_string(),
            "skill_name": name,
        }),
    );
}
```

Add the `scan_sandbox_files` helper function (at module level or within the orchestrator file):

```rust
fn scan_sandbox_files(sandbox_dir: &std::path::Path) -> Vec<crate::domain::SkillFileRef> {
    let mut refs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sandbox_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                if file_name == "SKILL.md" {
                    continue; // 排除 SKILL.md
                }
                let relative = path.strip_prefix(sandbox_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                let role = infer_file_role(&relative);
                refs.push(crate::domain::SkillFileRef {
                    path: relative,
                    role,
                });
            }
        }
    }
    refs
}

fn infer_file_role(path: &str) -> crate::domain::SkillFileRole {
    if path.ends_with(".sh") || path.ends_with(".py") {
        crate::domain::SkillFileRole::Script
    } else if path.ends_with(".md") {
        crate::domain::SkillFileRole::Reference
    } else {
        crate::domain::SkillFileRole::Asset
    }
}
```

Also add a `spawn_tool_success` helper (or reuse existing patterns). Check if there's already a pattern for spawning successful tool results in the orchestrator.

- [x] **Step 3: Fill `current_skill_dir` in `dispatch.rs` sync path**

In `src/systems/tools/dispatch.rs`, modify the `ToolContext` construction (around line 254-272). Add Query parameters for `SkillCreationContext` and `SkillUpdateContext`, and resolve `current_skill_dir`:

```rust
// 需要添加的 Query 参数（在系统函数签名中）
creation_contexts: Query<&SkillCreationContext>,
update_contexts: Query<&SkillUpdateContext>,

// 构造 ToolContext 时
let current_skill_dir = if let Some(wi_entity) = request.work_item_entity {
    // 优先检查 SkillCreationContext
    if let Ok(ctx) = creation_contexts.get(wi_entity) {
        Some(ctx.sandbox_dir.clone())
    }
    // 其次检查 SkillUpdateContext
    else if let Ok(ctx) = update_contexts.get(wi_entity) {
        skill_loader.skill_md_path(&ctx.skill_id)
            .parent().map(|p| p.to_path_buf())
    } else {
        None
    }
} else {
    None
};
```

Then use `current_skill_dir` instead of `None` in the `ToolContext` construction.

Note: The dispatch system may need `skill_loader: Res<SkillLoader>` added as a parameter. Check what resources are already available.

- [x] **Step 4: Fill `current_skill_dir` in `async_dispatch.rs` async path**

Same pattern as sync path. In the `OwnedToolContext` construction (around line 176-193), resolve `current_skill_dir` from the WorkItem entity's context components.

- [x] **Step 5: Run `cargo test --all-features`**

Run: `cargo test --all-features`
Expected: Compiles and passes

- [x] **Step 6: Commit**

```bash
git add src/systems/tools/orchestrator.rs src/systems/tools/dispatch.rs src/systems/tools/async_dispatch.rs
git commit -m "feat(orchestrator): handle SubmitSkillCandidate and fill current_skill_dir from context"
```

---

### Task 9: 沙盒清理 — task_termination_system + clear_task_system

**Files:**
- Modify: `src/systems/transform/task_lifecycle.rs`

**Interfaces:**
- Consumes: Task 2 (SkillCreationContext), Task 1 (ExperienceStore)
- Produces: sandbox cleanup on task termination and /clear

- [x] **Step 1: Add sandbox cleanup to `task_termination_system`**

In `src/systems/transform/task_lifecycle.rs`, after the existing termination logic, add cleanup for `SkillCreationContext`:

The key logic (from the design doc §4.5):
- Task enters terminal state → query WorkItems with `SkillCreationContext`
- Check candidate status from `ExperienceStore`:
  - `Persisted` / `Rejected` / `Discarded` → delete sandbox + despawn WorkItem
  - `NeedsUserApproval` → don't clean (user may still approve)
  - `Submitted` / `GovernancePending` etc. → delete sandbox + despawn + candidate `Discarded`

This requires adding `ExperienceStore` and `SkillCreationContext` Query parameters to `task_termination_system`. Read the current function signature and add the new parameters.

- [x] **Step 2: Add sandbox cleanup to `clear_task_system`**

Same pattern but more aggressive: `/clear` always cleans up regardless of candidate status.

```rust
// In clear_task_system, after existing cleanup:
// Clean up skill creation sandboxes for this task
for (wi_entity, ctx, _) in skill_creation_contexts.iter() {
    if ctx.task_id == task_id {
        // Force cleanup regardless of candidate status
        let _ = std::fs::remove_dir_all(&ctx.sandbox_dir);
        // Mark associated candidates as Discarded
        // ... (search ExperienceStore by producer_task_id)
        commands.entity(wi_entity).despawn();
    }
}
```

- [x] **Step 3: Run `cargo test --all-features`**

Run: `cargo test --all-features`
Expected: Compiles and passes

- [x] **Step 4: Commit**

```bash
git add src/systems/transform/task_lifecycle.rs
git commit -m "feat(lifecycle): add sandbox cleanup for skill creation on task termination and clear"
```

---

### Task 10: 配置变更 — agents.toml skill-creator 声明 + skill-updater 权限修复

**Files:**
- Modify: `agents.toml`

**Interfaces:**
- Consumes: None
- Produces: skill-creator agent configuration, skill-updater read_skill_file permission

- [x] **Step 1: Add `skill-creator` agent to `agents.toml`**

After the existing `skill-updater` agent section, add:

```toml
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
```

- [x] **Step 2: Fix skill-updater `read_skill_file` permission**

In the existing `skill-updater` agent's `[agent.tools]` section, add:

```toml
read_skill_file = "Allow"
```

- [x] **Step 3: Verify agents.toml parses correctly**

Run: `cargo test --all-features`
Expected: No parse errors from agents.toml loading

- [x] **Step 4: Commit**

```bash
git add agents.toml
git commit -m "feat(config): add skill-creator agent and fix skill-updater read_skill_file permission"
```

---

### Task 11: 全局验证 — 编译 + 测试 + clippy

**Files:**
- All previously modified files

**Interfaces:**
- Consumes: All previous tasks
- Produces: Verified, working implementation

- [x] **Step 1: Run full test suite**

Run: `cargo test --all-features`
Expected: All tests pass

- [x] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

- [x] **Step 3: Run formatter check**

Run: `cargo fmt --all --check`
Expected: No formatting issues

- [x] **Step 4: Fix any issues found**

Address any compilation errors, clippy warnings, or test failures.

- [x] **Step 5: Final commit if needed**

```bash
git add -A
git commit -m "chore: fix clippy warnings and formatting"
```
