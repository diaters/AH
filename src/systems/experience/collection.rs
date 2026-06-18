use bevy::prelude::*;
use tracing::debug;

use crate::domain::{
    ConversationMessage, EntryRole, ExperienceCollectionCompletedMessage,
    ExperienceCollectionRequestMessage, ExperienceGovernanceRequestMessage, ExperienceStore,
    ShortTermMemory, SpaceToolRegistry, Task, TaskTerminatedMessage, WorkItem,
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
                "用户目标：{}\n\n请只调用 submit_experience_candidate 提交可复用经验候选。",
                task.content
            )
        } else {
            format!(
                "用户目标：{}\n\n任务结果摘要：{}\n\n请只调用 submit_experience_candidate 提交可复用经验候选。",
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

        commands.spawn(work_item);
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
            let msg = match entry.role {
                EntryRole::User => ConversationMessage::User {
                    content: entry.content.clone(),
                },
                EntryRole::Assistant => ConversationMessage::Assistant {
                    content: Some(entry.content.clone()),
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                },
                EntryRole::Summary => ConversationMessage::System {
                    content: entry.content.clone(),
                },
                EntryRole::Archive => continue,
            };
            messages.push(msg);
        }
    }

    messages
}

/// 经验收集完成处理系统：将非顶层候选标记为已汇聚，顶层候选推进到治理挂起。
pub(crate) fn experience_collection_completion_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    messages: Query<(Entity, &ExperienceCollectionCompletedMessage)>,
) {
    for (entity, msg) in &messages {
        if let Some(parent_task_id) = msg.parent_task_id {
            // 非顶层：消费父任务 inbox 中的子候选，标记为 Aggregated。
            let ids = store.aggregate_inbox_for_task(parent_task_id);
            debug!(
                event = "ExperienceCollectionAggregated",
                task_id = %msg.task_id,
                parent_task_id = %parent_task_id,
                aggregated_count = ids.len(),
                "aggregated child candidates into parent inbox"
            );
        } else {
            // 顶层：统一收束 root 候选与子层汇聚候选，推进到 GovernancePending 并触发治理。
            let ids = store.collect_top_level_governance_candidates(msg.task_id);
            if !ids.is_empty() {
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
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        ExperienceCandidate, ExperienceCandidateStatus, ExperienceStore, LongTermMemoryKind, TaskId,
    };

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
            LongTermMemoryKind::Fact,
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
            LongTermMemoryKind::Fact,
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
