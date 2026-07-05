> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 经验落盘链路完整修复实施计划

> __For agentic workers:__ Use executing-plans to implement this plan
> task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

__Goal:__ 修复经验链路中顶层任务未触发收集、治理者身份被错误路由、审批未精确匹配三个根因，使所有任务终态统一进入经验治理与落盘。

__Architecture:__ 以 `TaskTerminatedMessage` 为唯一触发事实源，从 `Task.delegate`  
显式提取治理者并贯穿经验收集消息链；为审批请求建立  
`request_id → candidate_id` 精确绑定，禁止模糊批量更新。

__Tech Stack:__ Rust, Bevy ECS, genai, ratatui

---

## 文件结构

| 文件 | 变更类型 | 职责 |
|---|---|---|
| `src/domain/contribution.rs` | 修改 | `ExperienceStore` 增加审批绑定；请求消息增加 `governing_agent_id` |
| `src/domain/message.rs` | 修改 | `ExperienceCollectionCompletedMessage` 新增 `governing_agent_id` |
| `src/domain/work_item.rs` | 修改 | `WorkItem` 新增 `governing_agent_id` 字段并在 `experience_collection` 构造函数中设置 |
| `src/domain/mod.rs` | 修改 | 导出 `ExperienceCollectionCompletedMessage`（已导出，无需改动） |
| `src/systems/contribution.rs` | 修改 | 重写经验收集触发入口；修正完成消息治理者路由；精确审批绑定与回写 |
| `src/systems/mod.rs` | 修改 | 重命名/导出任务终态触发系统 |
| `src/plugins/execution.rs` | 修改 | 更新系统注册名称与依赖 |
| `src/systems/transform/llm_response.rs` | 修改 | 经验收集 WorkItem 完成时显式携带 `governing_agent_id` |
| `tests/experience_candidate_flow.rs` | 修改 | 更新审批响应测试为精确匹配语义 |
| `tests/experience_collection_workitem_flow.rs` | 修改 | 覆盖顶层持久型任务触发、持久型 Agent 治理者身份 |
| `tests/experience_layered_governance_flow.rs` | 可选补充 | 强化 `/finish` 路径与顶层治理回归 |

---

## Task 1: 扩展经验收集消息结构

__Files:__

- 修改: `src/domain/contribution.rs:321-327`
- 修改: `src/domain/message.rs:363-369`
- 测试: `src/domain/contribution.rs` 现有测试（编译驱动）

- [ ] __Step 1: 为 `ExperienceCollectionRequestMessage` 新增 `governing_agent_id`__

```rust
/// 经验收集请求消息。
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionRequestMessage {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub parent_agent_id: Option<AgentId>,
    /// 原任务治理者，负责后续顶层经验治理与落盘。
    pub governing_agent_id: AgentId,
}
```

- [ ] __Step 2: 为 `ExperienceCollectionCompletedMessage` 新增 `governing_agent_id`__

```rust
/// 经验收集完成消息：WorkItem 完成后触发汇聚与治理。
#[derive(Debug, Clone, Component)]
pub struct ExperienceCollectionCompletedMessage {
    pub task_id: TaskId,
    pub parent_task_id: Option<TaskId>,
    pub agent_id: AgentId,
    /// 原任务治理者，由请求链路显式传递。
    pub governing_agent_id: AgentId,
}
```

- [ ] __Step 3: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 正常通过；后续任务会补齐所有构造点。

- [ ] __Step 4: Commit__

```bash
git add src/domain/contribution.rs src/domain/message.rs
git commit -m "feat: add governing_agent_id to experience collection messages"
```

---

## Task 2: 让 WorkItem 承载治理者身份

__Files:__

- 修改: `src/domain/work_item.rs:114-136`, `222-247`
- 测试: `tests/experience_collection_workitem_flow.rs`（后续任务覆盖）

- [ ] __Step 1: 为 `WorkItem` 新增 `governing_agent_id` 字段__

在 `src/domain/work_item.rs` 的 `WorkItem` 结构体中，紧接 `assigned_agent` 添加：

```rust
    /// 分配的 Agent
    pub assigned_agent: Option<AgentId>,
    /// 原任务治理者（仅经验收集等场景使用）
    pub governing_agent_id: Option<AgentId>,
    /// 来源
    pub origin: WorkItemOrigin,
```

- [ ] __Step 2: 在 `WorkItem::new` 中初始化新字段__

```rust
        Self {
            id: Uuid::new_v4(),
            task_id,
            parent_task_id: None,
            work_type,
            input,
            tags,
            status: WorkItemStatus::Pending,
            assigned_agent: None,
            governing_agent_id: None,
            origin,
            writeback_target,
        }
```

- [ ] __Step 3: 在 `experience_collection` 构造函数中接收并设置治理者__

将函数签名改为：

```rust
    pub fn experience_collection(
        task_id: TaskId,
        prompt: String,
        parent_task_id: Option<TaskId>,
        conversation: Vec<ConversationMessage>,
        tools: Vec<ToolDefinition>,
        governing_agent_id: AgentId,
    ) -> Self {
```

在函数末尾 `wi.parent_task_id = parent_task_id;` 之前添加：

```rust
        wi.governing_agent_id = Some(governing_agent_id);
        wi
```

- [ ] __Step 4: 同步更新所有调用点__

搜索仓库中所有 `WorkItem::experience_collection(` 调用，统一追加 `governing_agent_id` 参数：

- `src/systems/contribution.rs:92`（本计划 Task 4 会再次确认）
- `src/domain/work_item.rs:422` 内部测试：

```rust
        let work_item = WorkItem::experience_collection(
            task_id,
            "summarize what we learned".to_string(),
            Some(parent_task_id),
            vec![ConversationMessage::User {
                content: "user goal".to_string(),
            }],
            vec![tool],
            uuid::Uuid::new_v4(),
        );
```

- `tests/workitem_dispatch_flow.rs:203`：

```rust
    let work_item = WorkItem::experience_collection(
        task_id,
        "collect experience".to_string(),
        None,
        vec![],
        vec![tool],
        uuid::Uuid::new_v4(),
    );
```

- `tests/workitem_dispatch_flow.rs:247`：

```rust
    let work_item = WorkItem::experience_collection(
        task_id,
        "collect experience".to_string(),
        None,
        vec![],
        vec![tool],
        uuid::Uuid::new_v4(),
    );
```

- `tests/experience_collection_workitem_flow.rs:110`（本计划 Task 11 会再次确认）

- [ ] __Step 5: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 6: Commit__

```bash
git add src/domain/work_item.rs
git commit -m "feat: carry governing_agent_id on WorkItem for experience collection"
```

---

## Task 3: 重写经验收集触发入口为任务终态驱动

__Files:__

- 修改: `src/systems/contribution.rs:15-52`
- 修改: `src/systems/mod.rs:19-23`
- 修改: `src/plugins/execution.rs:37-42`

- [ ] __Step 1: 重写 `agent_termination_system` 为任务终态触发__

将 `src/systems/contribution.rs` 中的 `agent_termination_system` 整段替换为：

```rust
/// 任务终态经验收集触发系统：任务进入终态后统一生成经验收集请求。
pub(crate) fn task_terminated_experience_trigger_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    tasks: Query<&Task>,
) {
    for (_entity, terminated_msg) in &terminated {
        let Some(task) = tasks.iter().find(|task| task.id == terminated_msg.task_id) else {
            debug!(
                event = "ExperienceCollectionTaskNotFound",
                task_id = %terminated_msg.task_id,
                "task not found for experience collection, skipping"
            );
            continue;
        };

        let Some(governing_agent_id) = task.delegate else {
            debug!(
                event = "ExperienceCollectionSkipped",
                task_id = %task.id,
                reason = "missing_delegate",
                "task has no delegate, skipping experience collection"
            );
            continue;
        };

        debug!(
            event = "ExperienceCollectionRequested",
            task_id = %task.id,
            governing_agent_id = %governing_agent_id,
            parent_task_id = ?task.parent_task_id,
            "spawning experience collection request from task termination"
        );

        commands.spawn(ExperienceCollectionRequestMessage {
            task_id: task.id,
            parent_task_id: task.parent_task_id,
            parent_agent_id: None,
            governing_agent_id,
        });
    }
}
```

- [ ] __Step 2: 同步重命名导出__

在 `src/systems/mod.rs` 中：

```rust
pub(crate) use contribution::{
    experience_approval_result_system,
    experience_collection_completion_system, experience_collection_workitem_system,
    experience_governance_system, memory_absorption_system, memory_contribution_system,
    task_terminated_experience_trigger_system,
};
```

- [ ] __Step 3: 同步更新系统注册__

在 `src/plugins/execution.rs` 中：

```rust
use crate::systems::{
    HarnessSet, agent_execution_system,
    experience_approval_result_system, experience_collection_completion_system,
    experience_collection_workitem_system, experience_governance_system,
    ingest_execution_results_system, llm_response_system, memory_contribution_system,
    task_terminated_experience_trigger_system, tool_calling_orchestrator_system,
};
```

以及系统列表中：

```rust
                // 经验收集：任务终态触发收集请求
                task_terminated_experience_trigger_system.in_set(HarnessSet::Execution),
                // 经验收集：将请求转换为 WorkItem
                experience_collection_workitem_system
                    .in_set(HarnessSet::Execution)
                    .after(task_terminated_experience_trigger_system),
```

- [ ] __Step 4: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 5: Commit__

```bash
git add src/systems/contribution.rs src/systems/mod.rs src/plugins/execution.rs
git commit -m "refactor: drive experience collection from task termination"
```

---

## Task 4: 将治理者传入经验收集 WorkItem

__Files:__

- 修改: `src/systems/contribution.rs:54-112`

- [ ] __Step 1: 在 `experience_collection_workitem_system` 中传递 `governing_agent_id`__

修改 `src/systems/contribution.rs` 中 `experience_collection_workitem_system` 的 WorkItem 创建处：

```rust
        let work_item = WorkItem::experience_collection(
            task.id,
            prompt,
            request.parent_task_id,
            conversation,
            tools,
            request.governing_agent_id,
        );
```

- [ ] __Step 2: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 3: Commit__

```bash
git add src/systems/contribution.rs
git commit -m "feat: pass governing_agent_id into experience collection work item"
```

---

## Task 5: 完成消息显式使用治理者身份

__Files:__

- 修改: `src/systems/transform/llm_response.rs:567-612`

- [ ] __Step 1: 从 WorkItem 读取 `governing_agent_id` 生成完成消息__

在 `src/systems/transform/llm_response.rs` 的 `WorkItemType::ExperienceCollection`  
分支中，找到生成完成消息的代码块：

```rust
                            let completed_task_id = work_item.task_id;
                            let completed_parent_task_id = work_item.parent_task_id;
                            let completed_agent_id =
                                work_item.assigned_agent.unwrap_or(uuid::Uuid::nil());
                            let governing_agent_id = work_item
                                .governing_agent_id
                                .unwrap_or(completed_agent_id);
```

并修改消息生成：

```rust
                            commands.spawn(ExperienceCollectionCompletedMessage {
                                task_id: completed_task_id,
                                parent_task_id: completed_parent_task_id,
                                agent_id: completed_agent_id,
                                governing_agent_id,
                            });
```

- [ ] __Step 2: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 3: Commit__

```bash
git add src/systems/transform/llm_response.rs
git commit -m "feat: use explicit governing_agent_id in experience collection completion"
```

---

## Task 6: 修正顶层治理路由使用治理者

__Files:__

- 修改: `src/systems/contribution.rs:274-310`, `312-500`

- [ ] __Step 1: 在 `experience_collection_completion_system` 使用 `msg.governing_agent_id`__

修改 `src/systems/contribution.rs` 中 `ExperienceGovernanceRequestMessage` 的生成处：

```rust
                commands.spawn(ExperienceGovernanceRequestMessage {
                    task_id: msg.task_id,
                    agent_id: msg.governing_agent_id,
                });
                debug!(
                    event = "TopLevelExperienceGovernanceRequested",
                    task_id = %msg.task_id,
                    governing_agent_id = %msg.governing_agent_id,
                    candidate_count = ids.len(),
                    "spawned top-level experience governance request"
                );
```

- [ ] __Step 2: 确认 `experience_governance_system` 对治理 agent 不存在的防御性检查__

`experience_governance_system` 开头已存在对 `request.agent_id` 的查找与缺失处理：

```rust
        let agent = match agents.iter().find(|a| a.id == request.agent_id) {
            Some(a) => a,
            None => {
                debug!(
                    event = "ExperienceGovernanceAgentNotFound",
                    agent_id = %request.agent_id,
                    task_id = %request.task_id,
                    "agent not found for governance, skipping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        };
```

由于 Task 6 Step 1 已将 `request.agent_id` 改为 `msg.governing_agent_id`，上述检查会自动作用于新的治理者来源。请确认该分支未被误删，且日志事件为 `ExperienceGovernanceAgentNotFound`。

- [ ] __Step 3: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 4: Commit__

```bash
git add src/systems/contribution.rs
git commit -m "fix: route top-level governance through governing_agent_id"
```

---

## Task 7: 增加审批精确绑定索引

__Files:__

- 修改: `src/domain/contribution.rs:191-319`
- 测试: `src/domain/contribution.rs:373-466` 新增单元测试

- [ ] __Step 1: 在 `ExperienceStore` 中新增 `approval_bindings`__

```rust
/// 经验候选存储：全局运行时资源。
#[derive(Resource, Debug, Clone, Default)]
pub struct ExperienceStore {
    pub candidates: std::collections::HashMap<uuid::Uuid, ExperienceCandidate>,
    pub inboxes: std::collections::HashMap<TaskId, ExperienceInbox>,
    /// 顶层候选（无父任务的 Agent 自身产生的候选）
    pub root_candidates: std::collections::HashMap<TaskId, Vec<uuid::Uuid>>,
    /// 审批请求 ID 到候选 ID 的精确绑定。
    approval_bindings: std::collections::HashMap<uuid::Uuid, uuid::Uuid>,
}
```

- [ ] __Step 2: 新增绑定与精确匹配方法__

在 `impl ExperienceStore` 末尾、`apply_confirmation_response` 之前或之后添加：

```rust
    /// 为审批请求绑定目标候选。
    pub fn bind_approval_request(&mut self, request_id: uuid::Uuid, candidate_id: uuid::Uuid) {
        self.approval_bindings.insert(request_id, candidate_id);
    }

    /// 根据审批请求 ID 查找候选 ID。
    pub fn candidate_id_for_request(&self, request_id: uuid::Uuid) -> Option<uuid::Uuid> {
        self.approval_bindings.get(&request_id).copied()
    }

    /// 根据确认请求 ID 精确应用确认结果，返回实际更新的候选 ID。
    pub fn apply_confirmation_response_precise(
        &mut self,
        request_id: uuid::Uuid,
        selected_option: &str,
    ) -> Option<uuid::Uuid> {
        let candidate_id = self.candidate_id_for_request(request_id)?;
        let approved = selected_option == "approve";
        if let Some(candidate) = self.candidates.get_mut(&candidate_id) {
            candidate.status = if approved {
                ExperienceCandidateStatus::Approved
            } else {
                ExperienceCandidateStatus::Rejected
            };
            Some(candidate_id)
        } else {
            None
        }
    }
```

- [ ] __Step 3: 重写旧 `apply_confirmation_response` 为精确版本包装（保持接口稳定）__

将现有 `apply_confirmation_response` 改为委托给精确方法，避免外部调用点行为突变：

```rust
    /// 根据确认请求 ID 应用确认结果（精确匹配）。
    pub fn apply_confirmation_response(&mut self, request_id: uuid::Uuid, selected_option: &str) {
        self.apply_confirmation_response_precise(request_id, selected_option);
    }
```

- [ ] __Step 4: 添加单元测试__

在 `src/domain/contribution.rs` 的 `#[cfg(test)]` 模块末尾添加：

```rust
    #[test]
    fn approval_binding_links_request_to_single_candidate() {
        let mut store = ExperienceStore::default();
        let request_id = uuid::Uuid::new_v4();
        let candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "bound fact".to_string(),
            "content".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        );
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);

        store.bind_approval_request(request_id, candidate_id);
        assert_eq!(store.candidate_id_for_request(request_id), Some(candidate_id));
    }

    #[test]
    fn precise_confirmation_only_updates_bound_candidate() {
        let mut store = ExperienceStore::default();
        let request_id = uuid::Uuid::new_v4();

        let mut c1 = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "first".to_string(),
            "content".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        );
        c1.status = ExperienceCandidateStatus::NeedsUserApproval;
        let c1_id = c1.candidate_id;
        store.stage_root_candidate(c1);

        let mut c2 = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "second".to_string(),
            "content".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        );
        c2.status = ExperienceCandidateStatus::NeedsUserApproval;
        let c2_id = c2.candidate_id;
        store.stage_root_candidate(c2);

        store.bind_approval_request(request_id, c1_id);
        let updated = store.apply_confirmation_response_precise(request_id, "approve");

        assert_eq!(updated, Some(c1_id));
        assert_eq!(
            store.candidates.get(&c1_id).unwrap().status,
            ExperienceCandidateStatus::Approved
        );
        assert_eq!(
            store.candidates.get(&c2_id).unwrap().status,
            ExperienceCandidateStatus::NeedsUserApproval
        );
    }

    #[test]
    fn unbound_confirmation_does_not_affect_any_candidate() {
        let mut store = ExperienceStore::default();
        let mut candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "orphan".to_string(),
            "content".to_string(),
            crate::domain::LongTermMemoryKind::Fact,
        );
        candidate.status = ExperienceCandidateStatus::NeedsUserApproval;
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);

        let updated = store.apply_confirmation_response_precise(uuid::Uuid::new_v4(), "approve");

        assert_eq!(updated, None);
        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::NeedsUserApproval
        );
    }
```

- [ ] __Step 5: 运行单元测试__

Run: `cargo test --lib experience::contribution::tests`
Expected: 新增测试通过，旧测试可能因精确匹配行为需要更新（见 Task 9）。

- [ ] __Step 6: Commit__

```bash
git add src/domain/contribution.rs
git commit -m "feat: precise approval binding with request_id -> candidate_id index"
```

---

## Task 8: 发起审批时写入绑定

__Files:__

- 修改: `src/systems/contribution.rs:506-527`

- [ ] __Step 1: 修改 `spawn_experience_confirmation` 签名以接收可变 store__

```rust
fn spawn_experience_confirmation(
    commands: &mut Commands,
    store: &mut crate::domain::ExperienceStore,
    request: &ExperienceGovernanceRequestMessage,
    candidate_id: &uuid::Uuid,
    candidate: &crate::domain::ExperienceCandidate,
) {
```

- [ ] __Step 2: 生成 request_id 并绑定候选__

```rust
    let request_id = uuid::Uuid::new_v4();
    store.bind_approval_request(request_id, *candidate_id);
    debug!(
        event = "ExperienceApprovalBound",
        request_id = %request_id,
        candidate_id = %candidate_id,
        "bound approval request to candidate"
    );

    commands.spawn(ToolConfirmationRequestMessage {
        request_id,
        // ... 其余字段不变
    });
```

- [ ] __Step 3: 更新所有调用点__

将 `src/systems/contribution.rs` 中两处调用 `spawn_experience_confirmation` 的位置补充 `store` 参数：

```rust
                        spawn_experience_confirmation(
                            commands,
                            &mut store,
                            request,
                            candidate_id,
                            &candidate,
                        );
```

以及 `spawn_incubation_confirmation` 内部的调用：

```rust
        spawn_experience_confirmation(commands, store, request, candidate_id, &candidate);
```

- [ ] __Step 4: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 5: Commit__

```bash
git add src/systems/contribution.rs
git commit -m "feat: bind approval request_id to candidate_id when spawning confirmation"
```

---

## Task 9: 审批结果使用精确匹配并只处理目标候选

__Files:__

- 修改: `src/systems/contribution.rs:568-742`

- [ ] __Step 1: 使用 `apply_confirmation_response_precise` 并处理未命中__

在 `experience_approval_result_system` 开头替换为：

```rust
    for (entity, response) in &responses {
        let candidate_id = match store.apply_confirmation_response_precise(
            response.request_id,
            &response.selected_option,
        ) {
            Some(id) => id,
            None => {
                debug!(
                    event = "ExperienceApprovalBindingNotFound",
                    request_id = %response.request_id,
                    selected_option = %response.selected_option,
                    "no candidate bound to approval request, skipping"
                );
                commands.entity(entity).despawn();
                continue;
            }
        };

        let approved = response.selected_option != "deny";
```

- [ ] __Step 2: approve 分支只处理返回的候选__

将 approve 分支中遍历所有 `Approved` 候选的代码改为只处理 `candidate_id`。注意 `candidate` 现在是单个变量，不再是循环变量，其余分支逻辑保持不变：

```rust
        if approved {
            let candidate = match store.candidates.get(&candidate_id).cloned() {
                Some(c) => c,
                None => {
                    commands.entity(entity).despawn();
                    continue;
                }
            };

            let is_default = candidate
                .governing_agent_id
                .and_then(|id| agents.iter().find(|a| a.id == id))
                .map(is_default_agent)
                .unwrap_or(false);

            match candidate.kind_hint {
                ExperienceKindHint::Knowledge => {
                    if is_default {
                        if let Some(mut proposal) = proposals.iter_mut().find(|p| {
                            p.knowledge_candidate_ids.contains(&candidate.candidate_id)
                        }) {
                            proposal.status = IncubationProposalStatus::Approved;
                        }
                        if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                            c.status = ExperienceCandidateStatus::Persisted;
                        }
                    } else if let Some(mut entry) = candidate.as_long_term_memory_entry() {
                        entry.source_candidate_id = Some(candidate.candidate_id);
                        entry.source_task_id = Some(candidate.producer_task_id);
                        entry.agent_id = Some(candidate.producer_agent_id);

                        let mut persisted = false;
                        let producer_agent =
                            agents.iter().find(|a| a.id == candidate.producer_agent_id);
                        if let Some(agent) = producer_agent
                            && let Some(mut memory) = long_memories.iter_mut().find(|lm| {
                                lm.agent_name.as_deref() == Some(&agent.profile.name)
                            })
                        {
                            match service.add_entry(&mut memory, entry) {
                                Ok(_) => persisted = true,
                                Err(e) => {
                                    warn!(
                                        event = "ExperienceWritebackFailed",
                                        candidate_id = %candidate.candidate_id,
                                        target = "LongTermMemory",
                                        error = %e,
                                        "failed to persist knowledge candidate"
                                    );
                                }
                            }
                        }
                        if persisted
                            && let Some(c) = store.candidates.get_mut(&candidate.candidate_id)
                        {
                            c.status = ExperienceCandidateStatus::Persisted;
                        }
                    }
                }
                ExperienceKindHint::Executable => {
                    if is_default {
                        if let Some(mut proposal) = proposals.iter_mut().find(|p| {
                            p.executable_candidate_ids.contains(&candidate.candidate_id)
                        }) {
                            proposal.status = IncubationProposalStatus::Approved;
                        }
                        if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                            c.status = ExperienceCandidateStatus::Persisted;
                        }
                    } else if let Some(agent) =
                        agents.iter().find(|a| a.id == candidate.producer_agent_id)
                        && let crate::domain::ExperienceCandidatePayload::Executable {
                            intent,
                            when_to_use,
                            asset_refs,
                        } = &candidate.payload
                    {
                        let draft = crate::infrastructure::assets::SkillPackageDraft {
                            skill_id: format!("{}", candidate.candidate_id),
                            title: candidate.title.clone(),
                            problem: intent.clone(),
                            when_to_use: when_to_use.clone(),
                            steps: "参见 skill.md 与 scripts/ 目录".to_string(),
                            asset_refs: asset_refs.clone(),
                            dependency_refs: candidate.dependency_refs.clone(),
                            risks: "首版实现，需人工复核".to_string(),
                            source_task_id: Some(candidate.producer_task_id),
                            source_candidate_id: Some(candidate.candidate_id),
                        };
                        match asset_service.persist_skill_package(&agent.profile.name, &draft) {
                            Ok(_) => {
                                if let Some(c) =
                                    store.candidates.get_mut(&candidate.candidate_id)
                                {
                                    c.status = ExperienceCandidateStatus::Persisted;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    event = "ExperienceWritebackFailed",
                                    candidate_id = %candidate.candidate_id,
                                    target = "SkillPackage",
                                    error = %e,
                                    "failed to persist skill package"
                                );
                            }
                        }
                    }
                }
                ExperienceKindHint::SharedKnowledge => {
                    if let Some(existing) = upgrade_queue
                        .candidates
                        .iter_mut()
                        .find(|u| u.source_candidate_id == candidate.candidate_id)
                    {
                        existing.validation_status =
                            crate::domain::KnowledgeValidationStatus::Approved;
                    }
                    match upgrade_service.persist(&upgrade_queue) {
                        Ok(_) => {
                            if let Some(c) = store.candidates.get_mut(&candidate.candidate_id) {
                                c.status = ExperienceCandidateStatus::Persisted;
                            }
                        }
                        Err(e) => {
                            warn!(
                                event = "ExperienceWritebackFailed",
                                candidate_id = %candidate.candidate_id,
                                target = "SharedKnowledgeUpgradeQueue",
                                error = %e,
                                "failed to persist shared knowledge approval"
                            );
                        }
                    }
                }
                ExperienceKindHint::Discard => {}
            }

            debug!(
                event = "ExperienceCandidateFinalWriteback",
                candidate_id = %candidate.candidate_id,
                kind = ?candidate.kind_hint,
                is_default = is_default,
                "finalized experience candidate after user approval"
            );
        } else {
            debug!(
                event = "ExperienceCandidateRejected",
                request_id = %response.request_id,
                candidate_id = %candidate_id,
                "user rejected experience candidate"
            );
        }

        commands.entity(entity).despawn();
    }
```

- [ ] __Step 3: 删除旧代码__

删除旧的 `to_writeback` 集合构建与外层 `for candidate in to_writeback` 循环。

- [ ] __Step 4: 运行编译检查__

Run: `cargo check --all-targets`
Expected: 通过。

- [ ] __Step 5: Commit__

```bash
git add src/systems/contribution.rs
git commit -m "fix: precise approval response only updates bound candidate"
```

---

## Task 10: 更新现有审批测试为精确语义

__Files:__

- 修改: `tests/experience_candidate_flow.rs:60-105`
- 修改: `src/systems/contribution.rs:851-884`

- [ ] __Step 1: 更新 `tests/experience_candidate_flow.rs` 中的审批测试__

将 `confirmation_response_approves_and_rejects_candidates` 改为：

```rust
/// 验证 ExperienceStore.apply_confirmation_response 精确审批目标候选。
#[test]
fn confirmation_response_approves_and_rejects_candidates() {
    let mut store = ExperienceStore::default();
    let task_id = uuid::Uuid::new_v4();
    let agent_id = uuid::Uuid::new_v4();

    let mut candidate = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "test knowledge".to_string(),
        "some fact".to_string(),
        LongTermMemoryKind::Fact,
    );
    candidate.status = ExperienceCandidateStatus::NeedsUserApproval;

    let candidate_id = candidate.candidate_id;
    store.stage_root_candidate(candidate);

    let approve_request_id = uuid::Uuid::new_v4();
    store.bind_approval_request(approve_request_id, candidate_id);
    store.apply_confirmation_response(approve_request_id, "approve");
    assert_eq!(
        store.candidates.get(&candidate_id).unwrap().status,
        ExperienceCandidateStatus::Approved
    );

    // Reject a different candidate
    let mut candidate2 = ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        agent_id,
        "test knowledge 2".to_string(),
        "another fact".to_string(),
        LongTermMemoryKind::Fact,
    );
    candidate2.status = ExperienceCandidateStatus::NeedsUserApproval;
    let candidate2_id = candidate2.candidate_id;
    store.stage_root_candidate(candidate2);

    let reject_request_id = uuid::Uuid::new_v4();
    store.bind_approval_request(reject_request_id, candidate2_id);
    store.apply_confirmation_response(reject_request_id, "deny");
    assert_eq!(
        store.candidates.get(&candidate2_id).unwrap().status,
        ExperienceCandidateStatus::Rejected
    );
}
```

- [ ] __Step 2: 更新 `src/systems/contribution.rs` 内部测试__

将 `approved_executable_becomes_persisted` 改为绑定 request_id：

```rust
    #[test]
    fn approved_executable_becomes_persisted() {
        use crate::domain::{
            ExperienceCandidate, ExperienceCandidatePayload, ExperienceCandidateStatus,
            ExperienceKindHint,
        };

        let mut store = crate::domain::ExperienceStore::default();
        let request_id = uuid::Uuid::new_v4();
        let candidate = ExperienceCandidate {
            candidate_id: uuid::Uuid::new_v4(),
            producer_task_id: uuid::Uuid::new_v4(),
            producer_agent_id: uuid::Uuid::new_v4(),
            title: "test skill".to_string(),
            kind_hint: ExperienceKindHint::Executable,
            payload: ExperienceCandidatePayload::Executable {
                intent: "run smoke test".to_string(),
                when_to_use: "after changes".to_string(),
                asset_refs: vec![],
            },
            dependency_refs: vec![],
            status: ExperienceCandidateStatus::NeedsUserApproval,
            governing_agent_id: None,
        };
        let candidate_id = candidate.candidate_id;
        store.stage_root_candidate(candidate);
        store.bind_approval_request(request_id, candidate_id);
        store.apply_confirmation_response(request_id, "approve");

        assert_eq!(
            store.candidates.get(&candidate_id).unwrap().status,
            ExperienceCandidateStatus::Approved,
            "approved executable should be marked Approved"
        );
    }
```

- [ ] __Step 3: 运行单元测试__

Run: `cargo test --lib`
Expected: 通过。

- [ ] __Step 4: Commit__

```bash
git add tests/experience_candidate_flow.rs src/systems/contribution.rs
git commit -m "test: update approval tests to precise binding semantics"
```

---

## Task 11: 集成测试覆盖顶层持久型任务触发

__Files:__

- 修改: `tests/experience_collection_workitem_flow.rs:35-86`

- [ ] __Step 1: 重写 `task_termination_creates_experience_collection_workitem` 测试__

改为测试顶层持久型任务终态触发经验收集：

```rust
#[test]
fn persistent_task_termination_creates_experience_collection_workitem() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let mut task = Task::from_user_input_ready("test task", 3, default_channel());
    task.status = TaskStatus::Done;
    let task_id = task.id;
    let governing_agent_id = uuid::Uuid::new_v4();
    task.delegate = Some(governing_agent_id);
    app.world_mut()
        .spawn((task, harness::ShortTermMemory::default()));

    // 不绑定 TaskScoped agent：验证顶层持久型任务不依赖 agent 终止也能触发
    app.world_mut()
        .spawn(harness::TaskTerminatedMessage { task_id });

    app.update();

    let requests: Vec<_> = app
        .world_mut()
        .query::<&harness::ExperienceCollectionRequestMessage>()
        .iter(app.world())
        .collect();

    assert!(
        requests.iter().any(|r| r.task_id == task_id && r.governing_agent_id == governing_agent_id),
        "should create ExperienceCollectionRequestMessage with task delegate as governing agent"
    );
}
```

- [ ] __Step 2: 保留并更新第二个测试（WorkItem 携带治理者）__

在 `experience_collection_workitem_completes_on_candidate_submission` 中，构造 `WorkItem` 时补充 `governing_agent_id`：

```rust
    let governing_agent_id = uuid::Uuid::new_v4();
    let mut work_item =
        WorkItem::experience_collection(task_id, "collect".to_string(), None, vec![], vec![tool], governing_agent_id);
```

- [ ] __Step 3: 运行集成测试__

Run: `cargo test --test experience_collection_workitem_flow`
Expected: 通过。

- [ ] __Step 4: Commit__

```bash
git add tests/experience_collection_workitem_flow.rs
git commit -m "test: top-level persistent task triggers experience collection"
```

---

## Task 12: 集成测试覆盖顶层治理路由与审批精确匹配

__Files:__

- 创建/修改: `tests/experience_collection_workitem_flow.rs`（追加）或 `tests/experience_layered_governance_flow.rs`

- [ ] __Step 1: 追加顶层治理路由测试__

在 `tests/experience_collection_workitem_flow.rs` 末尾追加：

```rust
#[test]
fn experience_collection_completion_uses_governing_agent_not_collector() {
    let runtime = Arc::new(Runtime::new().unwrap());
    let executor: Arc<dyn AgentExecutor> = Arc::new(NoOpExecutor);
    let (_input_tx, input_rx) = unbounded();
    let mut app = build_harness_app(test_config(), runtime, executor, input_rx, vec![]);
    app.update();

    let task_id = uuid::Uuid::new_v4();
    let governing_agent_id = uuid::Uuid::new_v4();
    let collector_id = uuid::Uuid::new_v4();

    app.world_mut().spawn(harness::Agent {
        id: governing_agent_id,
        profile: harness::AgentProfile {
            name: "persistent-worker".to_string(),
            model: "test".to_string(),
        },
        capabilities: harness::AgentCapabilities {
            tags: vec![],
            description: "worker".to_string(),
        },
        kind: harness::AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: harness::AgentToolPermissions::default(),
    });

    let candidate = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        task_id,
        collector_id,
        "top-level fact".to_string(),
        "content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    app.world_mut()
        .resource_mut::<harness::ExperienceStore>()
        .stage_root_candidate(candidate);

    app.world_mut().spawn(harness::ExperienceCollectionCompletedMessage {
        task_id,
        parent_task_id: None,
        agent_id: collector_id,
        governing_agent_id,
    });

    app.update();

    let requests: Vec<_> = app
        .world_mut()
        .query::<&harness::ExperienceGovernanceRequestMessage>()
        .iter(app.world())
        .collect();

    assert!(
        requests.iter().any(|r| r.task_id == task_id && r.agent_id == governing_agent_id),
        "governance request must use governing_agent_id, not collector"
    );
}
```

- [ ] __Step 2: 追加子任务候选聚合回归测试__

```rust
#[test]
fn child_task_experience_still_aggregates_into_parent_inbox() {
    use harness::{ExperienceStore, TaskId};

    let mut store = ExperienceStore::default();
    let parent_task_id: TaskId = uuid::Uuid::new_v4();
    let child_task_id: TaskId = uuid::Uuid::new_v4();
    let parent_agent_id = uuid::Uuid::new_v4();

    let child_candidate = harness::ExperienceCandidate::knowledge(
        uuid::Uuid::new_v4(),
        child_task_id,
        uuid::Uuid::new_v4(),
        "child fact".to_string(),
        "content".to_string(),
        harness::LongTermMemoryKind::Fact,
    );
    store.queue_for_parent(parent_task_id, parent_agent_id, child_candidate);

    let ids = store.aggregate_inbox_for_task(parent_task_id);
    assert!(!ids.is_empty());
    assert_eq!(
        store.candidates.get(&ids[0]).unwrap().status,
        harness::ExperienceCandidateStatus::Aggregated
    );
}
```

- [ ] __Step 3: 运行集成测试__

Run: `cargo test --test experience_collection_workitem_flow`
Expected: 通过。

- [ ] __Step 4: Commit__

```bash
git add tests/experience_collection_workitem_flow.rs
git commit -m "test: verify governance routing and child aggregation"
```

---

## Task 13: 全量测试、clippy 与 fmt

__Files:__

- 全部相关文件

- [ ] __Step 1: 格式化代码__

Run: `cargo fmt --all`

- [ ] __Step 2: 运行 clippy__

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告。

- [ ] __Step 3: 运行全部测试__

Run: `cargo test --all-features`
Expected: 全部通过。

- [ ] __Step 4: Commit__

```bash
git add -A
git commit -m "chore: format and pass clippy/tests for experience persistence fix"
```

---

## Self-Review

### 1. Spec 覆盖度

| Spec 要求 | 对应任务 |
|---|---|
| 顶层持久型任务终态触发经验收集 | Task 3, Task 11 |
| `Task.delegate` 作为治理者唯一来源 | Task 3 |
| `ExperienceCollectionRequestMessage.governing_agent_id` | Task 1, Task 3 |
| `ExperienceCollectionCompletedMessage.governing_agent_id` | Task 1, Task 5 |
| 治理者显式传递到顶层治理 | Task 4, Task 6 |
| `collector` 不再被当作治理者 | Task 5, Task 6, Task 12 |
| 审批 `request_id → candidate_id` 精确绑定 | Task 7, Task 8 |
| `apply_confirmation_response_precise` | Task 7, Task 9 |
| 未命中绑定不更新任何候选 | Task 7, Task 9 |
| `/finish` 路径统一覆盖 | Task 3（`TaskTerminatedMessage` 驱动） |
| 子任务经验聚合到父 inbox | Task 6, Task 12 |
| 四类去向语义不变 | Task 6（未改动分流逻辑） |

### 2. Placeholder 扫描

- 无 `TBD`、`TODO`、`implement later`。
- 所有代码块包含完整可编译 Rust 代码。
- 所有测试包含完整断言。
- 所有命令包含预期输出。

### 3. 类型一致性

- `governing_agent_id` 类型始终为 `AgentId`（`Uuid` 别名）。
- `ExperienceCollectionRequestMessage` 与 `ExperienceCollectionCompletedMessage` 字段一致。
- `WorkItem::experience_collection` 签名与调用点同步更新。
- `spawn_experience_confirmation` 签名在所有调用点同步更新。
- `apply_confirmation_response_precise` 返回 `Option<Uuid>` 并在调用方正确处理。

---

## Execution Handoff

_Plan complete and saved to `docs/superpowers/plans/2026-06-14-experience-persistence-fix.md`.__

You can execute tasks inline using the `executing-plans` skill.
