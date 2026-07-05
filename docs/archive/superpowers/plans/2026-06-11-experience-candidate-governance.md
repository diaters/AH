> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# Experience Candidate Governance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将经验沉淀链路从“子 Agent 直接写回父 Agent 长期记忆”重构为“经验候选生成、逐层上传、顶层治理、选择性落盘/孵化”的极简闭环，并把 Agent 级 Skill 收敛为 `ExecutableMemoryEntry`。

**Architecture:** 首版保留现有 `LongTermMemoryEntry` 作为知识类落盘模型，引入最小 `ExperienceCandidate`、`ExperienceInbox` 和 `ExecutableMemoryEntry` 作为增量扩展。经验候选通过新的内置工具 `submit_experience_candidate` / `list_experience_candidates` 进入运行时资源，旧的 `memory_contribution_system` 被重构为“收集、入箱、治理”三段式流程。为避免把脚本内容塞进长期记忆，本计划同时新增一个轻量 `AgentAssetService`，负责将文本资产写入 `.harness/assets/agents/` 并向候选和可执行记忆返回稳定引用。

**Tech Stack:** Rust, Bevy ECS, serde, chrono, uuid, anyhow, tracing, cargo test, markdownlint

---

## Scope Check

本计划覆盖以下已确认设计要求：

- 经验沉淀从“直接吸收长期记忆”改为“候选生成 -> Inbox -> 顶层治理”
- 子任务结束后与父任务结束后都允许再开一轮经验收敛对话
- `Agent Skill` 不作为独立系统实现，而是落为 `ExecutableMemoryEntry`
- `ExperienceCandidate`、长期记忆、可执行记忆和资产引用都保持人类可读
- `default Agent` 不直接落盘长期资产，只能生成孵化提案
- 知识类候选允许自动落盘，可执行类和带资产依赖的候选必须进入确认

本计划刻意不引入：

- 向量检索
- 候选相似度合并
- 复杂资产版本治理
- 通用 Skill 执行框架
- 候选直接参与主任务上下文推理

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/domain/contribution.rs` | 定义 `ExperienceCandidate`、`ExperienceInbox`、`ExperienceStore`、候选状态与治理辅助方法 |
| `src/domain/memory.rs` | 保留 `LongTermMemoryEntry`，新增 `ExecutableMemoryEntry` |
| `src/domain/message.rs` | 新增经验收集、治理、孵化和确认相关消息 |
| `src/domain/space.rs` | 扩展 `ToolContext` 与 `ToolAction` 以支持候选提交和候选读取 |
| `src/domain/mod.rs` | 导出新的经验候选、可执行记忆和消息类型 |
| `src/infrastructure/assets/mod.rs` | 导出轻量资产仓模块 |
| `src/infrastructure/assets/service.rs` | 实现 `AgentAssetService`，负责文本资产写入和读取 |
| `src/infrastructure/mod.rs` | 暴露新的 `assets` 模块 |
| `src/systems/tools/builtin/submit_experience_candidate.rs` | 实现候选提交工具 |
| `src/systems/tools/builtin/list_experience_candidates.rs` | 实现候选列表工具 |
| `src/systems/tools/builtin/mod.rs` | 导出两个新工具 |
| `src/systems/tools/mod.rs` | 注册两个新工具并补测试 |
| `src/systems/tools/dispatch.rs` | 处理 `SubmitExperienceCandidate` 动作，将资产写仓并把候选入箱 |
| `src/systems/contribution.rs` | 用经验候选链路替代旧的直接记忆写回链路 |
| `src/systems/maintenance.rs` | 在经验收集未完成前延迟销毁 task-scoped agent |
| `src/plugins/execution.rs` | 注册经验收集与治理系统 |
| `src/plugins/memory.rs` | 注册经验治理后的落盘系统 |
| `tests/experience_candidate_flow.rs` | 覆盖候选提交、入箱、治理、默认 Agent 孵化提案 |
| `tests/memory_persistence_flow.rs` | 覆盖知识类自动落盘与可执行记忆资产引用持久化 |
| `docs/current-state.md` | 更新对外能力描述 |
| `docs/TODO.md` | 标记对应待办项的状态和后续边界 |

---

### Task 1: 定义经验候选与可执行记忆领域骨架

**Files:**
- Modify: `src/domain/contribution.rs`
- Modify: `src/domain/memory.rs`
- Modify: `src/domain/message.rs`
- Modify: `src/domain/mod.rs`

- [ ] **Step 1: 先写会失败的领域测试**

在 `src/domain/contribution.rs` 的测试模块中追加：

```rust
#[test]
fn experience_store_queues_candidate_for_parent_task() {
    let owner_task_id = uuid::Uuid::new_v4();
    let owner_agent_id = uuid::Uuid::new_v4();
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "shell timeout knowledge".to_string(),
        "shell_stop 默认会等待退出".to_string(),
        crate::domain::LongTermMemoryKind::Fact,
    );

    let mut store = ExperienceStore::default();
    store.queue_for_parent(owner_task_id, owner_agent_id, candidate.clone());

    let inbox = store.inboxes.get(&owner_task_id).unwrap();
    assert_eq!(inbox.owner_agent_id, owner_agent_id);
    assert_eq!(inbox.candidate_ids, vec![candidate.candidate_id]);
    assert_eq!(
        store.candidates.get(&candidate.candidate_id).unwrap().status,
        ExperienceCandidateStatus::Queued
    );
}
```

在 `src/domain/memory.rs` 的测试模块中追加：

```rust
#[test]
fn executable_memory_entry_keeps_asset_refs_readable() {
    let entry = ExecutableMemoryEntry {
        memory_id: uuid::Uuid::new_v4(),
        title: "shell smoke test".to_string(),
        intent: "run a reusable smoke test".to_string(),
        when_to_use: "after changing shell orchestration".to_string(),
        asset_refs: vec!["default-agent/asset-1-shell-smoke.sh".to_string()],
        dependency_refs: vec![],
    };

    assert_eq!(entry.asset_refs.len(), 1);
    assert!(entry.asset_refs[0].contains("shell-smoke"));
}
```

- [ ] **Step 2: 运行测试确认新类型尚不存在**

Run:

```bash
cargo test -q experience_store_queues_candidate_for_parent_task -- --nocapture
cargo test -q executable_memory_entry_keeps_asset_refs_readable -- --nocapture
```

Expected: FAIL，报错提示 `ExperienceCandidate`、`ExperienceStore` 或 `ExecutableMemoryEntry` 未定义。

- [ ] **Step 3: 实现最小领域模型与消息类型**

在 `src/domain/contribution.rs` 中加入如下骨架，并保留现有 `TaskSummary`：

```rust
use bevy::prelude::{Component, Resource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{AgentId, LongTermMemoryKind, TaskId};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceKindHint {
    Knowledge,
    Executable,
    SharedKnowledge,
    Discard,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidateStatus {
    Submitted,
    Queued,
    NeedsUserApproval,
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperienceCandidatePayload {
    Knowledge {
        content: String,
        memory_kind: LongTermMemoryKind,
    },
    Executable {
        intent: String,
        when_to_use: String,
        asset_refs: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceCandidate {
    pub candidate_id: uuid::Uuid,
    pub producer_task_id: TaskId,
    pub producer_agent_id: AgentId,
    pub title: String,
    pub kind_hint: ExperienceKindHint,
    pub payload: ExperienceCandidatePayload,
    pub dependency_refs: Vec<String>,
    pub status: ExperienceCandidateStatus,
}

impl ExperienceCandidate {
    pub fn knowledge(
        candidate_id: uuid::Uuid,
        producer_task_id: TaskId,
        producer_agent_id: AgentId,
        title: String,
        content: String,
        memory_kind: LongTermMemoryKind,
    ) -> Self {
        Self {
            candidate_id,
            producer_task_id,
            producer_agent_id,
            title,
            kind_hint: ExperienceKindHint::Knowledge,
            payload: ExperienceCandidatePayload::Knowledge { content, memory_kind },
            dependency_refs: Vec::new(),
            status: ExperienceCandidateStatus::Submitted,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExperienceInbox {
    pub owner_task_id: TaskId,
    pub owner_agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ExperienceStore {
    pub candidates: HashMap<uuid::Uuid, ExperienceCandidate>,
    pub inboxes: HashMap<TaskId, ExperienceInbox>,
}
```

在 `src/domain/memory.rs` 中加入最小可执行记忆结构：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutableMemoryEntry {
    pub memory_id: uuid::Uuid,
    pub title: String,
    pub intent: String,
    pub when_to_use: String,
    pub asset_refs: Vec<String>,
    pub dependency_refs: Vec<String>,
}
```

在 `src/domain/message.rs` 中新增：

```rust
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub parent_task_id: Option<TaskId>,
    pub parent_agent_id: Option<AgentId>,
}

#[derive(Debug, Clone, Component)]
pub struct ExperienceGovernanceRequestMessage {
    pub task_id: TaskId,
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, Component)]
pub struct IncubationProposal {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub candidate_ids: Vec<uuid::Uuid>,
}
```

并在 `src/domain/mod.rs` 中导出：

```rust
pub use contribution::{
    ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus, ExperienceInbox,
    ExperienceKindHint, ExperienceStore, IncubationProposal, TaskSummary,
};
pub use memory::ExecutableMemoryEntry;
pub use message::{ExperienceCollectionRequestMessage, ExperienceGovernanceRequestMessage};
```

- [ ] **Step 4: 运行领域测试**

Run:

```bash
cargo test -q experience_store_queues_candidate_for_parent_task -- --nocapture
cargo test -q executable_memory_entry_keeps_asset_refs_readable -- --nocapture
```

Expected: PASS，且 `src/domain` 编译通过。

- [ ] **Step 5: 提交**

```bash
git add src/domain/contribution.rs src/domain/memory.rs src/domain/message.rs src/domain/mod.rs
git commit -m "feat: add experience candidate domain models"
```

---

### Task 2: 新增轻量 Agent 资产仓

**Files:**
- Create: `src/infrastructure/assets/mod.rs`
- Create: `src/infrastructure/assets/service.rs`
- Modify: `src/infrastructure/mod.rs`
- Test: `src/infrastructure/assets/service.rs`

- [ ] **Step 1: 先写资产仓失败测试**

在 `src/infrastructure/assets/service.rs` 新建测试模块并先写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn persist_text_assets_returns_readable_refs() {
        let dir = TempDir::new().unwrap();
        let service = AgentAssetService::new(dir.path().join("agents"));
        let refs = service
            .persist_text_assets(
                "default-agent",
                &[ExperienceAssetDraft {
                    name: "shell-smoke.sh".to_string(),
                    content: "echo ok\n".to_string(),
                }],
            )
            .unwrap();

        assert_eq!(refs.len(), 1);
        assert!(refs[0].contains("default-agent"));
        assert!(std::fs::read_to_string(dir.path().join("agents").join(&refs[0])).is_ok());
    }
}
```

- [ ] **Step 2: 运行测试确认服务不存在**

Run:

```bash
cargo test -q persist_text_assets_returns_readable_refs -- --nocapture
```

Expected: FAIL，报错提示 `AgentAssetService` 或 `ExperienceAssetDraft` 未定义。

- [ ] **Step 3: 实现最小资产服务**

在 `src/infrastructure/assets/mod.rs` 中写入：

```rust
pub mod service;

pub use service::{AgentAssetService, ExperienceAssetDraft};
```

在 `src/infrastructure/assets/service.rs` 中实现：

```rust
use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use bevy::prelude::Resource;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperienceAssetDraft {
    pub name: String,
    pub content: String,
}

#[derive(Resource, Debug, Clone)]
pub struct AgentAssetService {
    base_dir: PathBuf,
}

impl AgentAssetService {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self { base_dir: base_dir.into() }
    }

    pub fn default_path() -> Self {
        Self::new(".harness/assets/agents")
    }

    pub fn persist_text_assets(
        &self,
        agent_name: &str,
        drafts: &[ExperienceAssetDraft],
    ) -> Result<Vec<String>> {
        let agent_dir = self.base_dir.join(agent_name);
        fs::create_dir_all(&agent_dir)
            .with_context(|| format!("failed to create asset dir {}", agent_dir.display()))?;

        drafts
            .iter()
            .map(|draft| {
                let file_name = format!("{}-{}", Uuid::new_v4(), draft.name);
                let relative = format!("{}/{}", agent_name, file_name);
                let path = self.base_dir.join(&relative);
                fs::write(&path, &draft.content)
                    .with_context(|| format!("failed to write asset {}", path.display()))?;
                Ok(relative)
            })
            .collect()
    }
}
```

在 `src/infrastructure/mod.rs` 中补：

```rust
pub mod assets;
```

- [ ] **Step 4: 运行资产仓测试**

Run:

```bash
cargo test -q persist_text_assets_returns_readable_refs -- --nocapture
```

Expected: PASS，且生成的资产引用可回读文本文件。

- [ ] **Step 5: 提交**

```bash
git add src/infrastructure/assets/mod.rs src/infrastructure/assets/service.rs src/infrastructure/mod.rs
git commit -m "feat: add agent asset service"
```

---

### Task 3: 增加经验候选提交与读取工具

**Files:**
- Modify: `src/domain/space.rs`
- Modify: `src/systems/tools/mod.rs`
- Modify: `src/systems/tools/dispatch.rs`
- Create: `src/systems/tools/builtin/submit_experience_candidate.rs`
- Create: `src/systems/tools/builtin/list_experience_candidates.rs`
- Modify: `src/systems/tools/builtin/mod.rs`

- [ ] **Step 1: 先写两个工具的失败测试**

在 `src/systems/tools/builtin/submit_experience_candidate.rs` 中先写：

```rust
#[test]
fn submit_experience_candidate_returns_submit_action() {
    let knowledge = crate::domain::SharedKnowledgeBase::default();
    let store = crate::domain::ExperienceStore::default();
    let ctx = crate::domain::ToolContext {
        knowledge: &knowledge,
        experience_store: &store,
        default_wait_tasks_timeout_secs: 300,
        shell_default_tail_lines: 50,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 60,
        shell_default_stop_timeout_secs: 5,
        current_task_id: uuid::Uuid::new_v4(),
        current_agent_id: uuid::Uuid::new_v4(),
    };

    let tool = SubmitExperienceCandidateTool;
    let action = tool.execute(
        &serde_json::json!({
            "title": "shell timeout note",
            "kind_hint": "knowledge",
            "payload": {
                "content": "shell_stop 默认等待退出",
                "memory_kind": "Fact"
            },
            "dependency_refs": []
        }),
        &ctx,
    ).unwrap();

    assert!(matches!(action, crate::domain::ToolAction::SubmitExperienceCandidate(_)));
}
```

在 `src/systems/tools/builtin/list_experience_candidates.rs` 中先写：

```rust
#[test]
fn list_experience_candidates_reads_current_task_inbox() {
    let knowledge = crate::domain::SharedKnowledgeBase::default();
    let mut store = crate::domain::ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    store.queue_for_parent(
        task_id,
        agent_id,
        crate::domain::ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            task_id,
            agent_id,
            "shell timeout".to_string(),
            "shell_stop 默认等待退出".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        ),
    );

    let ctx = crate::domain::ToolContext {
        knowledge: &knowledge,
        experience_store: &store,
        default_wait_tasks_timeout_secs: 300,
        shell_default_tail_lines: 50,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 60,
        shell_default_stop_timeout_secs: 5,
        current_task_id: task_id,
        current_agent_id: agent_id,
    };

    let tool = ListExperienceCandidatesTool;
    let action = tool.execute(&serde_json::json!({}), &ctx).unwrap();
    match action {
        crate::domain::ToolAction::Direct(value) => {
            assert_eq!(value["count"], 1);
        }
        other => panic!("expected direct action, got {:?}", other),
    }
}
```

- [ ] **Step 2: 运行测试确认工具和上下文尚未扩展**

Run:

```bash
cargo test -q submit_experience_candidate_returns_submit_action -- --nocapture
cargo test -q list_experience_candidates_reads_current_task_inbox -- --nocapture
```

Expected: FAIL，报错提示 `experience_store`、`SubmitExperienceCandidateTool` 或 `ToolAction::SubmitExperienceCandidate` 不存在。

- [ ] **Step 3: 扩展 ToolContext、ToolAction 与工具注册**

在 `src/domain/space.rs` 中扩展：

```rust
pub enum ToolAction {
    Direct(serde_json::Value),
    SubmitExperienceCandidate(ExperienceCandidateSubmission),
    SpawnAgent { name: String, model: Option<String>, description: String, tools: Vec<String> },
    // 其余现有分支保持不变
}

pub struct ToolContext<'a> {
    pub knowledge: &'a SharedKnowledgeBase,
    pub experience_store: &'a ExperienceStore,
    pub default_wait_tasks_timeout_secs: u64,
    pub shell_default_tail_lines: usize,
    pub shell_max_tail_lines: usize,
    pub shell_default_exec_timeout_secs: u64,
    pub shell_default_stop_timeout_secs: u64,
    pub current_task_id: TaskId,
    pub current_agent_id: AgentId,
}
```

在 `src/systems/tools/builtin/submit_experience_candidate.rs` 中实现最小工具：

```rust
pub struct SubmitExperienceCandidateTool;

impl crate::domain::BuiltinTool for SubmitExperienceCandidateTool {
    fn name(&self) -> &str {
        "submit_experience_candidate"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::domain::ToolContext,
    ) -> Result<crate::domain::ToolAction, crate::domain::ToolError> {
        let title = input
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::domain::ToolError::InvalidInput("missing title".to_string()))?;

        Ok(crate::domain::ToolAction::SubmitExperienceCandidate(
            crate::domain::ExperienceCandidateSubmission::from_json(
                ctx.current_task_id,
                ctx.current_agent_id,
                title,
                input,
            )?,
        ))
    }
}
```

在 `src/systems/tools/builtin/list_experience_candidates.rs` 中实现：

```rust
pub struct ListExperienceCandidatesTool;

impl crate::domain::BuiltinTool for ListExperienceCandidatesTool {
    fn name(&self) -> &str {
        "list_experience_candidates"
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        ctx: &crate::domain::ToolContext,
    ) -> Result<crate::domain::ToolAction, crate::domain::ToolError> {
        let items: Vec<serde_json::Value> = ctx
            .experience_store
            .list_for_task(ctx.current_task_id)
            .into_iter()
            .map(|candidate| {
                serde_json::json!({
                    "candidate_id": candidate.candidate_id,
                    "title": candidate.title,
                    "kind_hint": format!("{:?}", candidate.kind_hint),
                    "status": format!("{:?}", candidate.status),
                })
            })
            .collect();

        Ok(crate::domain::ToolAction::Direct(serde_json::json!({
            "count": items.len(),
            "items": items,
        })))
    }
}
```

并在 `src/systems/tools/mod.rs` / `src/systems/tools/builtin/mod.rs` 中注册、导出两个工具。

- [ ] **Step 4: 在 dispatch 里处理提交动作**

在 `src/systems/tools/dispatch.rs` 的 `handle_tool_action` 中新增：

```rust
ToolAction::SubmitExperienceCandidate(submission) => {
    let parent = tasks
        .iter()
        .find(|(_, t)| t.id == request.request.task_id)
        .and_then(|(_, task)| task.parent_task_id);

    let asset_refs = asset_service
        .persist_text_assets(&agent.profile.name, &submission.inline_assets)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    let candidate = submission.into_candidate(asset_refs);
    if let Some(parent_task_id) = parent {
        let parent_agent_id = agent.parent_id.unwrap_or(request.request.agent_id);
        experience_store.queue_for_parent(parent_task_id, parent_agent_id, candidate.clone());
    } else {
        experience_store.stage_root_candidate(candidate.clone());
    }

    spawn_tool_success(
        commands,
        entity,
        request,
        serde_json::json!({
            "candidate_id": candidate.candidate_id,
            "status": format!("{:?}", candidate.status),
        }),
    );
}
```

- [ ] **Step 5: 运行工具测试并提交**

Run:

```bash
cargo test -q submit_experience_candidate_returns_submit_action -- --nocapture
cargo test -q list_experience_candidates_reads_current_task_inbox -- --nocapture
cargo test -q executor_knowledge_search -- --nocapture
```

Expected: PASS，且旧 `knowledge_search` 工具测试不回归。

Commit:

```bash
git add src/domain/space.rs src/systems/tools/mod.rs src/systems/tools/dispatch.rs src/systems/tools/builtin/mod.rs src/systems/tools/builtin/submit_experience_candidate.rs src/systems/tools/builtin/list_experience_candidates.rs
git commit -m "feat: add experience candidate tools"
```

---

### Task 4: 用候选收集链路替换旧的直接记忆写回

**Files:**
- Modify: `src/systems/contribution.rs`
- Modify: `src/systems/maintenance.rs`
- Modify: `src/plugins/execution.rs`
- Modify: `src/plugins/memory.rs`
- Modify: `src/systems/mod.rs`

- [ ] **Step 1: 先写会失败的收集链路测试**

在 `src/systems/contribution.rs` 的测试模块中追加：

```rust
#[test]
fn task_scoped_agent_termination_spawns_experience_collection_request() {
    let task_id = uuid::Uuid::new_v4();
    let parent_id = uuid::Uuid::new_v4();
    let agent = crate::domain::Agent {
        id: uuid::Uuid::new_v4(),
        profile: crate::domain::AgentProfile {
            name: "worker".to_string(),
            model: "test".to_string(),
        },
        capabilities: crate::domain::AgentCapabilities {
            tags: vec![],
            description: "worker".to_string(),
        },
        kind: crate::domain::AgentKind::TaskScoped,
        parent_id: Some(parent_id),
        bound_task_id: Some(task_id),
        tool_permissions: crate::domain::AgentToolPermissions::default(),
    };

    let request = build_experience_collection_request(&agent, task_id, Some(uuid::Uuid::new_v4()));
    assert_eq!(request.task_id, task_id);
    assert_eq!(request.agent_id, agent.id);
    assert_eq!(request.parent_agent_id, Some(parent_id));
}
```

- [ ] **Step 2: 运行测试确认新 helper 不存在**

Run:

```bash
cargo test -q task_scoped_agent_termination_spawns_experience_collection_request -- --nocapture
```

Expected: FAIL，报错提示 `build_experience_collection_request` 未定义。

- [ ] **Step 3: 重构 contribution 系统为“触发收集 + 入箱”**

在 `src/systems/contribution.rs` 中保留 `agent_termination_system` 入口，但将其改写为只做“触发经验收集请求”，并新增一个小 helper：

```rust
fn build_experience_collection_request(
    agent: &Agent,
    task_id: uuid::Uuid,
    parent_task_id: Option<uuid::Uuid>,
) -> ExperienceCollectionRequestMessage {
    ExperienceCollectionRequestMessage {
        task_id,
        agent_id: agent.id,
        parent_task_id,
        parent_agent_id: agent.parent_id,
    }
}
```

然后将系统主体改成：

```rust
pub(crate) fn agent_termination_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    agents: Query<&Agent>,
    tasks: Query<&Task>,
) {
    for (_entity, terminated_msg) in &terminated {
        for agent in &agents {
            if agent.kind != AgentKind::TaskScoped
                || agent.bound_task_id != Some(terminated_msg.task_id)
            {
                continue;
            }

            let parent_task_id = tasks
                .iter()
                .find(|task| task.id == terminated_msg.task_id)
                .and_then(|task| task.parent_task_id);

            commands.spawn(build_experience_collection_request(
                agent,
                terminated_msg.task_id,
                parent_task_id,
            ));
        }
    }
}
```

同时新增 `experience_collection_dispatch_system`，复用原 Task 的 `ShortTermMemory` 构造一次 follow-up 执行请求，并只暴露 `submit_experience_candidate`：

```rust
pub(crate) fn experience_collection_dispatch_system(
    mut commands: Commands,
    requests: Query<(Entity, &ExperienceCollectionRequestMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
    registry: Res<SpaceToolRegistry>,
) {
    for (entity, request) in &requests {
        let Some(agent) = agents.iter().find(|a| a.id == request.agent_id) else {
            continue;
        };
        let Some((task, stm)) = tasks.iter().find(|(task, _)| task.id == request.task_id) else {
            continue;
        };

        let tools = registry
            .iter()
            .filter(|tool| tool.name == "submit_experience_candidate")
            .cloned()
            .collect();

        commands.spawn(AgentExecutionRequestMessage {
            request: AgentExecutionRequest {
                task_id: task.id,
                agent_id: agent.id,
                request_kind: AgentRequestKind::LlmCompletion,
                prompt: format!("当前任务已结束。请只调用 submit_experience_candidate 提交可复用经验候选。任务结果摘要：{}", task.result_summary),
                system_prompt: Some("你正在进行任务后经验收敛。不要继续解题，不要输出普通文本，只提交结构化经验候选。".to_string()),
                tools,
                conversation: stm.map(build_experience_collection_conversation),
                work_item_id: None,
            },
        });

        commands.entity(entity).despawn();
    }
}
```

- [ ] **Step 4: 延迟 task-scoped agent 清理直到收集完成**

在 `src/systems/maintenance.rs` 中增加对 `ExperienceCollectionTracker` 的检查，避免 task-scoped agent 在候选提交前被销毁：

```rust
fn handle_termination(
    commands: &mut Commands,
    agents: &Query<(Entity, &Agent)>,
    tracker: &ExperienceCollectionTracker,
    task_id: TaskId,
) {
    if tracker.pending_task_ids.contains(&task_id) {
        return;
    }

    for (entity, agent) in agents.iter() {
        if agent.kind == AgentKind::TaskScoped && agent.bound_task_id == Some(task_id) {
            commands.entity(entity).despawn();
        }
    }
}
```

并在候选成功提交后从 `ExperienceCollectionTracker` 中移除该任务，使下一帧可以正常清理 agent。

- [ ] **Step 5: 运行收集链路测试并提交**

Run:

```bash
cargo test -q task_scoped_agent_termination_spawns_experience_collection_request -- --nocapture
cargo test -q task_scoped_agent_lifecycle -- --nocapture
```

Expected: PASS，且 task-scoped agent 在经验候选提交完成前不会被提前清理。

Commit:

```bash
git add src/systems/contribution.rs src/systems/maintenance.rs src/plugins/execution.rs src/plugins/memory.rs src/systems/mod.rs
git commit -m "refactor: replace memory writeback with experience collection"
```

---

### Task 5: 落地顶层治理、默认 Agent 孵化提案与回归文档

**Files:**
- Modify: `src/systems/contribution.rs`
- Modify: `src/plugins/execution.rs`
- Modify: `tests/memory_persistence_flow.rs`
- Create: `tests/experience_candidate_flow.rs`
- Modify: `docs/current-state.md`
- Modify: `docs/TODO.md`

- [ ] **Step 1: 先写治理与孵化的集成测试**

新建 `tests/experience_candidate_flow.rs`：

```rust
use harness::{
    domain::{
        Agent, AgentCapabilities, AgentKind, AgentProfile, AgentToolPermissions, ExperienceCandidate,
        ExperienceCandidateStatus, ExperienceKindHint, ExperienceStore, LongTermMemory,
        LongTermMemoryKind,
    },
    infrastructure::memory::{JsonFileMemoryStore, LongTermMemoryService, MemoryRepository},
};

#[test]
fn knowledge_candidate_is_persisted_for_persistent_agent() {
    let mut store = ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();
    let candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "shell timeout".to_string(),
        "shell_stop 默认等待退出".to_string(),
        LongTermMemoryKind::Fact,
    );

    store.stage_root_candidate(candidate.clone());
    let ids = store.root_candidates_for_task(task_id);
    assert_eq!(ids, vec![candidate.candidate_id]);
}

#[test]
fn executable_candidate_requires_user_approval() {
    let candidate = ExperienceCandidate {
        candidate_id: uuid::Uuid::new_v4(),
        producer_task_id: uuid::Uuid::new_v4(),
        producer_agent_id: uuid::Uuid::new_v4(),
        title: "shell smoke test".to_string(),
        kind_hint: ExperienceKindHint::Executable,
        payload: harness::domain::ExperienceCandidatePayload::Executable {
            intent: "run smoke test".to_string(),
            when_to_use: "after shell changes".to_string(),
            asset_refs: vec!["default-agent/script.sh".to_string()],
        },
        dependency_refs: vec![],
        status: ExperienceCandidateStatus::Submitted,
    };

    assert!(candidate.requires_user_confirmation());
}
```

- [ ] **Step 2: 运行测试确认治理 helper 尚不存在**

Run:

```bash
cargo test -q knowledge_candidate_is_persisted_for_persistent_agent -- --nocapture
cargo test -q executable_candidate_requires_user_approval -- --nocapture
```

Expected: FAIL，报错提示 `stage_root_candidate` 或 `requires_user_confirmation` 未定义。

- [ ] **Step 3: 实现顶层治理规则**

在 `src/systems/contribution.rs` 中新增 `experience_governance_system`，按以下规则处理顶层候选：

```rust
pub(crate) fn experience_governance_system(
    mut commands: Commands,
    mut experience_store: ResMut<ExperienceStore>,
    mut long_term_service: ResMut<LongTermMemoryService>,
    mut long_memories: Query<&mut LongTermMemory>,
    agents: Query<&Agent>,
    requests: Query<(Entity, &ExperienceGovernanceRequestMessage)>,
) {
    for (entity, request) in &requests {
        let Some(agent) = agents.iter().find(|a| a.id == request.agent_id) else {
            commands.entity(entity).despawn();
            continue;
        };

        let candidate_ids = experience_store.root_candidates_for_task(request.task_id);
        if agent.capabilities.tags.contains(&"default".to_string()) {
            commands.spawn(IncubationProposal {
                task_id: request.task_id,
                agent_id: agent.id,
                candidate_ids,
            });
            commands.entity(entity).despawn();
            continue;
        }

        for candidate_id in candidate_ids {
            let Some(candidate) = experience_store.candidates.get_mut(&candidate_id) else {
                continue;
            };

            if candidate.requires_user_confirmation() {
                candidate.status = ExperienceCandidateStatus::NeedsUserApproval;
                continue;
            }

            if let Some(mut memory) = long_memories.iter_mut().find(|m| m.agent_name.as_deref() == Some(&agent.profile.name)) {
                if let Some(entry) = candidate.as_long_term_memory_entry() {
                    long_term_service.add_entry(&mut memory, entry).unwrap();
                    candidate.status = ExperienceCandidateStatus::Approved;
                }
            }
        }

        commands.entity(entity).despawn();
    }
}
```

再在 `ExperienceCandidate` 上实现两个辅助方法：

```rust
impl ExperienceCandidate {
    pub fn requires_user_confirmation(&self) -> bool {
        matches!(self.kind_hint, ExperienceKindHint::Executable)
            || matches!(
                &self.payload,
                ExperienceCandidatePayload::Executable { asset_refs, .. } if !asset_refs.is_empty()
            )
    }

    pub fn as_long_term_memory_entry(&self) -> Option<LongTermMemoryEntry> {
        match &self.payload {
            ExperienceCandidatePayload::Knowledge { content, memory_kind } => {
                Some(LongTermMemoryEntry::new(*memory_kind, content.clone()))
            }
            ExperienceCandidatePayload::Executable { .. } => None,
        }
    }
}
```

- [ ] **Step 4: 用现有确认通道承接 MVP 的用户确认**

在 `src/systems/contribution.rs` 中对 `NeedsUserApproval` 的候选复用现有确认 UI 通道，明确把确认对象命名为治理动作而非伪装成 shell 工具：

```rust
commands.spawn(ToolConfirmationRequestMessage {
    request_id: uuid::Uuid::new_v4(),
    task_id: request.task_id,
    agent_id: request.agent_id,
    tool_name: "experience_governance".to_string(),
    tool_input: serde_json::json!({
        "candidate_id": candidate.candidate_id,
        "title": candidate.title,
        "kind_hint": format!("{:?}", candidate.kind_hint),
    }),
    options: crate::domain::ConfirmationOption::default_options(),
    source: crate::domain::ConfirmationSource::User,
    parent_agent_id: None,
});
```

然后新增一个小系统消费 `ToolConfirmationResponseMessage`，只处理 `tool_name == "experience_governance"` 的请求：

```rust
pub(crate) fn experience_approval_result_system(
    mut responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
    mut store: ResMut<ExperienceStore>,
) {
    for (_entity, response) in &mut responses {
        store.apply_confirmation_response(response.request_id, &response.selected_option);
    }
}
```

- [ ] **Step 5: 运行集成测试、更新文档并提交**

Run:

```bash
cargo test -q knowledge_candidate_is_persisted_for_persistent_agent -- --nocapture
cargo test -q executable_candidate_requires_user_approval -- --nocapture
cargo test -q memory_persistence_flow -- --nocapture
```

Expected: PASS，且顶层普通持久型 Agent 可自动落知识类候选，`default` tag 持久型 Agent 只会生成孵化提案。

文档同步：

- 在 `docs/current-state.md` 的“记忆治理”与“待完善”中补充经验候选治理、`Executable Memory` 和 `default Agent` 孵化规则
- 在 `docs/TODO.md` 中勾选“Agent 级 Skill 功能”和“经验贡献系统架构重设计”，并保留资产治理、复杂确认流、复杂依赖图等后续项

Commit:

```bash
git add src/systems/contribution.rs src/plugins/execution.rs tests/experience_candidate_flow.rs tests/memory_persistence_flow.rs docs/current-state.md docs/TODO.md
git commit -m "feat: add experience candidate governance flow"
```

---

## Self-Review

### Spec Coverage

- `ExperienceCandidate` / `ExperienceInbox` / `ExecutableMemoryEntry`：由 Task 1 落地
- 资产外置与可读性：由 Task 2 落地
- `submit_experience_candidate` / `list_experience_candidates`：由 Task 3 落地
- 子任务/父任务经验收集与逐层上传：由 Task 4 落地
- 顶层治理、`default Agent` 孵化、确认与文档同步：由 Task 5 落地

### Placeholder Scan

- 本计划不包含占位表达或“后续再补”式描述
- 每个任务都包含了明确文件、测试名、命令和关键代码骨架

### Type Consistency

- 统一使用 `ExperienceCandidate`、`ExperienceStore`、`ExperienceCollectionRequestMessage`
- 统一将可执行经验落盘类型命名为 `ExecutableMemoryEntry`
- 统一使用 `submit_experience_candidate` / `list_experience_candidates` 作为工具名
