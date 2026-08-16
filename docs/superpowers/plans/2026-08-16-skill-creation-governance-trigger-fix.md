# `/skill` 候选治理触发修复实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 补全 `/skill` 链路断点——SkillCreation WorkItem 完成时将候选从 `Submitted` 推进到 `GovernancePending` 并触发治理审批，使创建的 skill 能走完确认与写回闭环。

**架构：** 仅改 `src/systems/transform/llm_response.rs`：`llm_response_system` 签名扩展（`ExperienceStore` 改 `ResMut`、新增 `SkillCreationContext` Query），`match work_item.work_type` 新增 `SkillCreation` 分支，复用 `collect_top_level_governance_candidates` 统一收束入口 + spawn `ExperienceGovernanceRequestMessage`。

**技术栈：** Rust + Bevy ECS；集成测试参照 `tests/experience_collection_workitem_flow.rs:91-179` 模式

**设计文档：** `docs/design/2026-08-16-skill-creation-governance-trigger-fix.md`

**验证命令：** `cargo test --all-features`（每个任务后运行相关子集）、最终运行 `cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`

---

## 文件结构

| 文件 | 职责 |
|------|------|
| `src/systems/transform/llm_response.rs` | 签名扩展 + `SkillCreation` 完成分支 |
| `tests/skill_creation_governance_flow.rs` | 新增：治理触发集成测试（有候选 → 推进+治理请求；无候选 → fail） |

---

### 任务 1：`llm_response_system` 签名扩展

**文件：**
- 修改：`src/systems/transform/llm_response.rs:724-742`（系统签名）、L8-23（imports）

- [ ] **步骤 1：修改 imports**

在 `src/systems/transform/llm_response.rs` 头部 `use crate::{... domain::{...} }` 中追加：

```rust
        ExperienceGovernanceRequestMessage, SkillCreationContext,
```

（与既有 `ExperienceCollectionCompletedMessage, ExperienceStore, ...` 条目并列，保持字母序。）

- [ ] **步骤 2：修改系统签名**

将 `llm_response_system`（L724）的：

```rust
    experience_store: Res<ExperienceStore>,
    profile_contexts: Query<&ProfileGenerationContext>,
```

替换为：

```rust
    mut experience_store: ResMut<ExperienceStore>,
    profile_contexts: Query<&ProfileGenerationContext>,
    skill_creation_contexts: Query<&SkillCreationContext>,
```

- [ ] **步骤 3：修复借用编译错误**

`ResMut` 后，既有只读用法 `has_experience_submission(&experience_store, ...)`（L812）与 `&experience_store` 传参均通过 `Deref` 兼容，无需修改；若有其他分支以 `&Res<ExperienceStore>` 显式类型传参导致编译错误，改为 `&experience_store` 即可。

运行：`cargo build`
预期：编译通过（本任务无行为变化）

- [ ] **步骤 4：Commit**

```bash
git add src/systems/transform/llm_response.rs
git commit -m "refactor: llm_response_system 扩展 ExperienceStore 可变访问与 SkillCreationContext 查询"
```

---

### 任务 2：`SkillCreation` 完成分支（TDD）

**文件：**
- 修改：`src/systems/transform/llm_response.rs:772`（`match work_item.work_type`）
- 创建：`tests/skill_creation_governance_flow.rs`

- [ ] **步骤 1：编写失败的集成测试**

创建 `tests/skill_creation_governance_flow.rs`（完整文件，参照 `tests/experience_collection_workitem_flow.rs` 的夹具模式）：

```rust
//! /skill 候选治理触发集成测试（2026-08-16 修复）
//!
//! 验证 SkillCreation WorkItem 完成后：
//! 1. 有候选提交 → 候选推进 GovernancePending + spawn 治理请求 + WorkItem 完成清理
//! 2. 无候选提交 → WorkItem fail 清理，不 spawn 治理请求

use std::sync::Arc;

use crossbeam_channel::unbounded;
use harness::{
    AgentExecutor, AgentExecutionOutput, AgentExecutionRequest, AgentExecutionResult,
    AgentRequestKind, ChannelId, DispatchHint, DispatchStrategy, ExecutorFuture, FrontendKind,
    HarnessConfig, ShortTermMemory, SkillCreationContext, Task, ExperienceCandidate,
    ExperienceCandidateStatus, WorkItem, WorkItemStatus, WorkItemType,
    build_harness_app, llm::ExecutorRegistry,
};

fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

struct TextExecutor;

impl AgentExecutor for TextExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async move {
            Ok(AgentExecutionOutput {
                content: harness::OutputContent::Text("已创建 skill".to_string()),
                reasoning_content: None,
            })
        })
    }
}

fn test_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: Some("test-api-key".to_string()),
            api_base: None,
        },
        brain: None,
        agents_config_path: "/nonexistent_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        max_tool_iterations: 5,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "/nonexistent_providers.toml".to_string(),
    }
}

/// 构造带候选的 SkillCreation 完成场景，返回 (app, task_id, candidate_id)
fn setup_with_candidate(submit_candidate: bool) -> (bevy_app::App, uuid::Uuid, uuid::Uuid) {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(TextExecutor);
    let executor_registry = ExecutorRegistry::from_single_executor(executor, "default");
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(
        test_config(),
        runtime,
        executor_registry,
        input_rx,
        vec![],
        harness::channels::ChannelManager::empty().0,
    );
    app.update();

    let task = Task::from_user_input_ready("创建新闻 skill", 3, default_channel());
    let task_id = task.id;
    app.world_mut().spawn((task, ShortTermMemory::default()));

    let governing_agent_id = uuid::Uuid::new_v4();
    let mut work_item = WorkItem::skill_creation(
        task_id,
        "create skill".to_string(),
        vec![],
        vec![],
        governing_agent_id,
    );
    let work_item_id = work_item.id;
    work_item.status = WorkItemStatus::Running;
    work_item.assigned_agent = Some(governing_agent_id);
    app.world_mut().spawn((
        work_item,
        SkillCreationContext {
            task_id,
            agent_id: governing_agent_id,
            agent_name: "default".to_string(),
            sandbox_dir: std::path::PathBuf::from("/tmp/test-sandbox"),
            skill_name: "daily-news".to_string(),
        },
    ));

    let candidate_id = uuid::Uuid::new_v4();
    if submit_candidate {
        let candidate = ExperienceCandidate::skill_new(
            candidate_id,
            task_id,
            governing_agent_id,
            "daily-news skill".to_string(),
            "daily-news".to_string(),
            "获取当天新闻".to_string(),
            "## 步骤\n1. 打开新闻网站".to_string(),
            vec![],
        );
        app.world_mut()
            .resource_mut::<harness::ExperienceStore>()
            .stage_root_candidate(candidate);
    }

    let result = AgentExecutionResult {
        task_id,
        agent_id: governing_agent_id,
        request_kind: AgentRequestKind::LlmCompletion,
        result: Ok(AgentExecutionOutput {
            content: harness::OutputContent::Text("已创建 skill".to_string()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: Some(work_item_id),
        conversation: None,
    };
    app.world_mut()
        .spawn(harness::AgentExecutionResultMessage { result });

    (app, task_id, candidate_id)
}

#[test]
fn skill_creation_completion_promotes_candidate_and_requests_governance() {
    let (mut app, task_id, candidate_id) = setup_with_candidate(true);

    app.update();

    // WorkItem 已清理
    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .filter(|wi| wi.work_type == WorkItemType::SkillCreation)
        .collect();
    assert!(
        work_items.is_empty(),
        "SkillCreation WorkItem should be despawned after completion"
    );

    // 候选推进到 GovernancePending
    let store = app.world().resource::<harness::ExperienceStore>();
    let status = store
        .candidates
        .get(&candidate_id)
        .map(|c| c.status.clone());
    assert_eq!(
        status,
        Some(ExperienceCandidateStatus::GovernancePending),
        "candidate should be promoted from Submitted to GovernancePending"
    );

    // 治理请求已 spawn
    let governance_requests = app
        .world_mut()
        .query::<&harness::domain::ExperienceGovernanceRequestMessage>()
        .iter(app.world())
        .filter(|m| m.task_id == task_id)
        .count();
    assert_eq!(
        governance_requests, 1,
        "exactly one governance request should be spawned for the task"
    );
}

#[test]
fn skill_creation_completion_without_candidate_fails_silently() {
    let (mut app, task_id, _candidate_id) = setup_with_candidate(false);

    app.update();

    let work_items: Vec<_> = app
        .world_mut()
        .query::<&WorkItem>()
        .iter(app.world())
        .filter(|wi| wi.work_type == WorkItemType::SkillCreation)
        .collect();
    assert!(
        work_items.is_empty(),
        "SkillCreation WorkItem should be despawned even without submission"
    );

    let governance_requests = app
        .world_mut()
        .query::<&harness::domain::ExperienceGovernanceRequestMessage>()
        .iter(app.world())
        .filter(|m| m.task_id == task_id)
        .count();
    assert_eq!(
        governance_requests, 0,
        "no governance request should be spawned without a submitted candidate"
    );
}
```

注意（执行前核对，不属于模糊步骤，是编译期必然暴露的适配点）：

- `SkillCreationContext` 的导入路径以 `tests/experience_collection_workitem_flow.rs` 顶部既有导出风格为准（`harness::SkillCreationContext` 或 `harness::domain::SkillCreationContext`）；`ExperienceCandidate`、`WorkItemStatus` 同理。
- `app.update()` 后治理系统可能在同帧消费 `ExperienceGovernanceRequestMessage` 并把候选推进到 `NeedsUserApproval`（治理系统注册于 Execution 集合）。因此第一个断言若得到 `NeedsUserApproval` 也算通过——将状态断言改为：

```rust
    let status = store
        .candidates
        .get(&candidate_id)
        .map(|c| c.status.clone());
    assert!(
        matches!(
            status,
            Some(ExperienceCandidateStatus::GovernancePending)
                | Some(ExperienceCandidateStatus::NeedsUserApproval)
        ),
        "candidate should be promoted beyond Submitted, got {:?}",
        status
    );
```

- 同理治理请求计数若被同帧消费 despawn，改为断言"候选已越过 Submitted"为主断言，治理消息计数为辅助断言（被消费为 0 时同样视为通过——治理请求是瞬态消息）。最终主断言集：WorkItem despawn + 候选状态越过了 `Submitted`。

- [ ] **步骤 2：运行测试验证失败**

运行：`cargo test --test skill_creation_governance_flow`
预期：`skill_creation_completion_promotes_candidate_and_requests_governance` FAIL——候选状态停在 `Submitted`（当前无 SkillCreation 分支）；`skill_creation_completion_without_candidate_fails_silently` 可能已 PASS（现状 WorkItem fall through 通用路径不 despawn——若 FAIL 于 WorkItem 未 despawn，同样证明断点）

- [ ] **步骤 3：实现 SkillCreation 分支**

在 `src/systems/transform/llm_response.rs` 的 `match work_item.work_type` 中、`WorkItemType::SkillUpdate =>` 分支（L937）之后、`_ => {}`（L985）之前插入：

```rust
                    WorkItemType::SkillCreation => {
                        // - ToolCalls（write_skill_file / submit_skill）：fall through，
                        //   由下方 tool calling loop 处理后续迭代。
                        // - text：skill-creator 结束。有候选提交则推进治理
                        //   （Submitted → GovernancePending + spawn 治理请求，
                        //   复用统一收束入口 collect_top_level_governance_candidates）；
                        //   无候选提交则 fail。最终文本继续走通用路径，
                        //   用户仍能收到创建结果回复。
                        // - error：对齐 SkillUpdate 错误路径。
                        match &result.result {
                            Ok(AgentExecutionOutput {
                                content: OutputContent::ToolCalls(_),
                                ..
                            }) => {
                                // 不 continue，让下面的 tool calling loop 处理 tool calls
                            }
                            Ok(_) => {
                                let had_submission = has_experience_submission(
                                    &experience_store,
                                    work_item.task_id,
                                );

                                if had_submission {
                                    // 统一收束入口：root 候选 Submitted → GovernancePending
                                    let advanced = experience_store
                                        .collect_top_level_governance_candidates(
                                            work_item.task_id,
                                        );
                                    if !advanced.is_empty() {
                                        let agent_id = skill_creation_contexts
                                            .get(work_item_entity)
                                            .ok()
                                            .map(|c| c.agent_id)
                                            .unwrap_or(
                                                work_item
                                                    .governing_agent_id
                                                    .unwrap_or(uuid::Uuid::nil()),
                                            );
                                        commands.spawn(ExperienceGovernanceRequestMessage {
                                            task_id: work_item.task_id,
                                            agent_id,
                                        });
                                        debug!(
                                            event = "SkillCreationGovernanceRequested",
                                            task_id = %work_item.task_id,
                                            candidate_count = advanced.len(),
                                            "skill creation candidate promoted to governance"
                                        );
                                    }

                                    if let Ok(mut wi) = work_items.get_mut(work_item_entity) {
                                        wi.1.complete();
                                        commands.entity(work_item_entity).insert(
                                            WorkItemLifecycleHookPending(
                                                HookPoint::OnWorkItemCompleted,
                                            ),
                                        );
                                    }
                                } else {
                                    warn!(
                                        event = "SkillCreationWorkItemNoSubmission",
                                        work_item_id = %work_item.id,
                                        task_id = %work_item.task_id,
                                        error = "LLM finished without successful submit_skill",
                                        error_type = "NoCandidateSubmission",
                                        "skill creation LLM finished without candidate, \
                                         cleaning up work item"
                                    );
                                    if let Ok(mut wi) = work_items.get_mut(work_item_entity) {
                                        wi.1.fail();
                                        commands.entity(work_item_entity).insert(
                                            WorkItemLifecycleHookPending(
                                                HookPoint::OnWorkItemFailed,
                                            ),
                                        );
                                    }
                                }

                                // despawn WorkItem；不 despawn result entity、不 continue：
                                // 最终文本继续走通用路径（result entity 由通用路径收尾
                                // despawn，见下方 commands.entity(entity).despawn()）
                                commands.entity(work_item_entity).despawn();
                            }
                            Err(_) => {
                                warn!(
                                    event = "SkillCreationWorkItemLlmFailed",
                                    work_item_id = %work_item.id,
                                    task_id = %work_item.task_id,
                                    error = "LLM execution returned Err",
                                    error_type = "LlmExecutionFailed",
                                    "skill creation LLM failed, cleaning up work item"
                                );
                                if let Ok(mut wi) = work_items.get_mut(work_item_entity) {
                                    wi.1.fail();
                                    commands.entity(work_item_entity).insert(
                                        WorkItemLifecycleHookPending(
                                            HookPoint::OnWorkItemFailed,
                                        ),
                                    );
                                }
                                commands.entity(work_item_entity).despawn();
                                commands.entity(entity).despawn();
                                continue;
                            }
                        }
                    }
```

- [ ] **步骤 4：运行测试验证通过**

运行：`cargo test --test skill_creation_governance_flow`
预期：2 个测试 PASS

- [ ] **步骤 5：回归验证**

运行：`cargo test --all-features`
预期：全部 PASS。重点关注 `skill_update_integration`、`experience_collection_workitem_flow`、`experience_candidate_flow`、`experience_layered_governance_flow` 无回归

- [ ] **步骤 6：Commit**

```bash
git add src/systems/transform/llm_response.rs tests/skill_creation_governance_flow.rs
git commit -m "feat: SkillCreation WorkItem 完成后触发候选治理审批，补全 /skill 链路闭环"
```

---

### 任务 3：全量验证

- [ ] **步骤 1：CI 全量检查**

运行：`cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
预期：格式化无 diff、clippy 无警告、全部测试 PASS

- [ ] **步骤 2：手动验证准备（可选，交付用户执行）**

QQ 发送 `/skill 创建一个 XX 的 skill` → 确认审批请求回到原会话通道 → 批准 → `.harness/assets/agents/<agent>/skills/<skill>/` 目录出现 → 新任务中该 skill 出现在 Agent prompt。

---

## 自检记录

- **规格覆盖度：** 设计文档变更点 1（完成分支 + 状态推进 + 治理请求 + complete/fail/despawn）→ 任务 2；变更点 2（无重复治理）与变更点 3（终态清理边界）为"无需改动"的论证性结论，由任务 2 步骤 5 的回归测试守护（`experience_layered_governance_flow` 等）；验证方案 6.1/6.2 → 任务 2 测试；6.3 手动验证 → 任务 3 步骤 2。
- **占位符扫描：** 无"待定/TODO/类似任务 N"；所有代码步骤含完整代码。
- **类型一致性：** `collect_top_level_governance_candidates(task_id: TaskId) -> Vec<uuid::Uuid>`（`src/domain/contribution.rs:393`）；`ExperienceGovernanceRequestMessage { task_id, agent_id }`（`contribution.rs:531`）；`WorkItem::skill_creation(task_id, prompt, conversation, tools, governing_agent_id)`（`work_item.rs:329`）；`ExperienceCandidate::skill_new(candidate_id, producer_task_id, producer_agent_id, title, name, description, instructions, file_refs)`（`contribution.rs:179`）；`SkillCreationContext { task_id, agent_id, agent_name, sandbox_dir, skill_name }`（`contribution.rs:708`）；`wi.1.complete()/fail()` 对齐 `ExperienceCollection` 分支（`llm_response.rs:821-837`）既有模式。
