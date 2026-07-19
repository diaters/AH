use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    Agent, AgentKind, ConversationMessage, DispatchHint, DispatchKind, DispatchStrategy, EntryRole,
    ExperienceCollectionCompletedMessage, ExperienceCollectionRequestMessage,
    ExperienceConsolidationRequestMessage, ExperienceGovernanceRequestMessage, ExperienceKindHint,
    ExperienceStore, LlmToolCall, PendingDispatch, ShortTermMemory, SpaceToolRegistry, Task,
    TaskExperiencePolicy, TaskInjectedSkill, TaskTerminatedMessage, WorkItem, WorkItemType,
};

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

/// 经验收集 WorkItem 创建系统：将收集请求转换为独立 WorkItem。
pub(crate) fn experience_collection_workitem_system(
    mut commands: Commands,
    requests: Query<(Entity, &ExperienceCollectionRequestMessage)>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
    registry: Res<SpaceToolRegistry>,
) {
    for (entity, request) in &requests {
        let Some((task, stm)) = tasks.iter().find(|(t, _)| t.id == request.task_id) else {
            debug!(
                event = "ExperienceCollectionTaskNotFound",
                task_id = %request.task_id,
                "task not found for experience collection, skipping"
            );
            commands.entity(entity).despawn();
            continue;
        };

        let conversation = build_experience_collection_conversation(task, stm);

        let prompt = if task.result_summary.is_empty() {
            format!(
                "用户目标：{}\n\n请调用 submit_experience_candidate 提交可复用经验候选。\n\n注意：\n- 如果提炼的内容包含具体命令、指令或操作步骤，请使用 kind=skill\n- 如果只是纯事实性知识，请使用 kind=knowledge\n\nSKILL.md 格式要求（kind=skill 时）：\n- instructions 字段必须是 markdown 格式，至少包含 1 个 `## Section` 二级标题\n- 推荐使用 `## Overview` / `## Usage` / `## Examples` / `## Edge Cases` / `## Limitations` 等 section\n- 复杂 skill 可在二级标题下使用 `### Subsection` 三级标题组织内容\n- 不要使用 `####` 或更深层级（update 端不支持作为 operation 锚点）\n- 落盘前框架会做 validate_skill_structure 校验，不符合则候选置 WritebackFailed",
                task.content
            )
        } else {
            format!(
                "用户目标：{}\n\n任务结果摘要：{}\n\n请调用 submit_experience_candidate 提交可复用经验候选。\n\n注意：\n- 如果提炼的内容包含具体命令、指令或操作步骤，请使用 kind=skill\n- 如果只是纯事实性知识，请使用 kind=knowledge\n\nSKILL.md 格式要求（kind=skill 时）：\n- instructions 字段必须是 markdown 格式，至少包含 1 个 `## Section` 二级标题\n- 推荐使用 `## Overview` / `## Usage` / `## Examples` / `## Edge Cases` / `## Limitations` 等 section\n- 复杂 skill 可在二级标题下使用 `### Subsection` 三级标题组织内容\n- 不要使用 `####` 或更深层级（update 端不支持作为 operation 锚点）\n- 落盘前框架会做 validate_skill_structure 校验，不符合则候选置 WritebackFailed",
                task.content, task.result_summary
            )
        };

        let tools: Vec<crate::domain::ToolDefinition> = registry
            .iter()
            .filter(|tool| tool.name == "submit_experience_candidate")
            .cloned()
            .collect();

        let work_item = WorkItem::experience_collection(
            task.id,
            prompt,
            request.parent_task_id,
            conversation,
            tools,
            request.governing_agent_id,
        );

        debug!(
            event = "ExperienceCollectionWorkItemCreated",
            task_id = %request.task_id,
            work_item_id = %work_item.id,
            has_conversation = work_item.input.context.conversation.is_some(),
            tools_count = work_item.input.context.tools.len(),
            "spawning experience collection work item"
        );

        commands.spawn((
            work_item,
            PendingDispatch {
                kind: DispatchKind::WorkItem(WorkItemType::ExperienceCollection),
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: None,
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ));
        commands.entity(entity).despawn();
    }
}

/// 构建经验收集的净化对话材料。
fn build_experience_collection_conversation(
    task: &Task,
    stm: Option<&ShortTermMemory>,
) -> Vec<crate::domain::ConversationMessage> {
    let mut messages = Vec::new();

    messages.push(ConversationMessage::User {
        content: format!("用户目标：{}", task.content),
    });

    if !task.result_summary.is_empty() {
        messages.push(ConversationMessage::User {
            content: format!("任务结果摘要：{}", task.result_summary),
        });
    }

    if let Some(stm) = stm {
        for entry in stm
            .entries
            .iter()
            .filter(|e| !matches!(e.role, EntryRole::Archive))
        {
            match entry.role {
                EntryRole::User => {
                    messages.push(ConversationMessage::User {
                        content: entry.content.clone(),
                    });
                }
                EntryRole::Assistant => {
                    // 保留 tool_calls 信息，让 collector 能看到操作步骤
                    let tool_calls: Vec<LlmToolCall> = entry
                        .metadata
                        .tool_calls
                        .iter()
                        .enumerate()
                        .map(|(i, tc)| LlmToolCall {
                            id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                            name: tc.tool_name.clone(),
                            arguments: tc.input.clone(),
                        })
                        .collect();

                    messages.push(ConversationMessage::Assistant {
                        content: Some(entry.content.clone()),
                        tool_calls,
                        reasoning_content: None,
                    });

                    // 追加工具结果（截断至 500 字符避免上下文膨胀）
                    for (i, tc) in entry.metadata.tool_calls.iter().enumerate() {
                        let truncated_output = if tc.output.chars().count() > 500 {
                            let truncated: String = tc.output.chars().take(500).collect();
                            format!("{}...[truncated]", truncated)
                        } else {
                            tc.output.clone()
                        };
                        messages.push(ConversationMessage::Tool {
                            tool_call_id: tc.id.clone().unwrap_or_else(|| format!("tc_{}", i)),
                            content: truncated_output,
                        });
                    }
                }
                EntryRole::Summary => {
                    messages.push(ConversationMessage::System {
                        content: entry.content.clone(),
                    });
                }
                EntryRole::Archive => continue,
            }
        }
    }

    messages
}

/// 经验收集完成处理系统：将非顶层候选标记为已汇聚，顶层候选推进到治理挂起。
///
/// 持久Agent吸收分支（ADR-004 §3.1）：当 task 的 delegate 是持久Agent时，
/// 候选不进入父任务 inbox，而是按 kind_hint 分流到 skill-updater / LTM。
pub(crate) fn experience_collection_completion_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    agents: Query<&Agent>,
    tasks: Query<(
        &Task,
        Option<&TaskInjectedSkill>,
        Option<&TaskExperiencePolicy>,
    )>,
    messages: Query<(Entity, &ExperienceCollectionCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        let candidate_ids: Vec<uuid::Uuid> = if let Some(parent_task_id) = msg.parent_task_id {
            store.aggregate_inbox_for_task(parent_task_id)
        } else {
            store.collect_top_level_governance_candidates(msg.task_id)
        };

        let Some((task, injected_skill_component, policy_component)) =
            tasks.iter().find(|(t, _, _)| t.id == msg.task_id)
        else {
            commands.entity(entity).despawn();
            continue;
        };

        let delegate_is_persistent = task
            .delegate
            .and_then(|aid| agents.iter().find(|a| a.id == aid))
            .map(|a| a.kind == AgentKind::Persistent)
            .unwrap_or(false);

        if delegate_is_persistent {
            let injected_skill = injected_skill_component.and_then(|is| is.skill_id.clone());
            let policy = policy_component.map(|p| p.kind_filter);
            crate::systems::experience::skill_update::route_persistent_agent_experience(
                &mut commands,
                &mut store,
                msg,
                task,
                injected_skill,
                policy,
                &candidate_ids,
            );
            commands.entity(entity).despawn();
            continue;
        }

        // 非持久Agent：保留原聚合 / 顶层治理触发逻辑。
        if let Some(parent_task_id) = msg.parent_task_id {
            // 非顶层：消费父任务 inbox 中的子候选，标记为 Aggregated。
            debug!(
                event = "ExperienceCollectionAggregated",
                task_id = %msg.task_id,
                parent_task_id = %parent_task_id,
                aggregated_count = candidate_ids.len(),
                "aggregated child candidates into parent inbox"
            );

            // 汇聚后检查是否需要合并
            if candidate_ids.len() > 1 {
                let candidates: Vec<_> = candidate_ids
                    .iter()
                    .filter_map(|id| store.candidates.get(id))
                    .collect();

                let mut knowledge_ids: Vec<uuid::Uuid> = Vec::new();
                let mut skill_ids: Vec<uuid::Uuid> = Vec::new();
                for candidate in &candidates {
                    match candidate.kind_hint {
                        ExperienceKindHint::Knowledge => {
                            knowledge_ids.push(candidate.candidate_id);
                        }
                        ExperienceKindHint::Skill => {
                            skill_ids.push(candidate.candidate_id);
                        }
                    }
                }

                if knowledge_ids.len() > 1 {
                    commands.spawn(ExperienceConsolidationRequestMessage {
                        task_id: msg.task_id,
                        parent_task_id,
                        governing_agent_id: msg.governing_agent_id,
                        candidate_kind: ExperienceKindHint::Knowledge,
                        candidate_ids: knowledge_ids,
                    });
                }
                if skill_ids.len() > 1 {
                    commands.spawn(ExperienceConsolidationRequestMessage {
                        task_id: msg.task_id,
                        parent_task_id,
                        governing_agent_id: msg.governing_agent_id,
                        candidate_kind: ExperienceKindHint::Skill,
                        candidate_ids: skill_ids,
                    });
                }
            }
        } else {
            // 顶层：统一收束 root 候选与子层汇聚候选，推进到 GovernancePending 并触发治理。
            if !candidate_ids.is_empty() {
                commands.spawn(ExperienceGovernanceRequestMessage {
                    task_id: msg.task_id,
                    agent_id: msg.governing_agent_id,
                });
                debug!(
                    event = "TopLevelExperienceGovernanceRequested",
                    task_id = %msg.task_id,
                    governing_agent_id = %msg.governing_agent_id,
                    candidate_count = candidate_ids.len(),
                    "spawned top-level experience governance request"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{ExperienceCandidate, ExperienceCandidateStatus, ExperienceStore, TaskId};

    #[test]
    fn experience_collection_completion_aggregates_child_candidates() {
        let parent_task_id: TaskId = uuid::Uuid::new_v4();
        let child_task_id: TaskId = uuid::Uuid::new_v4();
        let parent_agent_id = uuid::Uuid::new_v4();

        let mut store = ExperienceStore::default();

        // 子层候选进入父层 inbox
        let child_candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            child_task_id,
            uuid::Uuid::new_v4(),
            "child fact".to_string(),
            "content".to_string(),
        );
        store.queue_for_parent(parent_task_id, parent_agent_id, child_candidate);

        // 汇聚：消费 inbox
        let ids = store.aggregate_inbox_for_task(parent_task_id);
        assert!(!ids.is_empty());
        assert_eq!(
            store.candidates.get(&ids[0]).unwrap().status,
            ExperienceCandidateStatus::Aggregated
        );

        // 顶层：暂存 root 候选并推进到治理
        let root_candidate = ExperienceCandidate::knowledge(
            uuid::Uuid::new_v4(),
            parent_task_id,
            parent_agent_id,
            "root fact".to_string(),
            "root content".to_string(),
        );
        store.stage_root_candidate(root_candidate);
        let governance_ids = store.promote_root_candidates_to_governance(parent_task_id);
        assert!(!governance_ids.is_empty());
        assert_eq!(
            store.candidates.get(&governance_ids[0]).unwrap().status,
            ExperienceCandidateStatus::GovernancePending
        );
    }
}
