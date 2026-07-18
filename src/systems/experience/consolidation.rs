use crate::prelude::*;
use tracing::debug;

use crate::domain::{
    DispatchHint, DispatchKind, DispatchStrategy, ExperienceCandidate, ExperienceCandidatePayload,
    ExperienceCandidateStatus, ExperienceConsolidationRequestMessage, ExperienceKindHint,
    ExperienceStore, PendingDispatch, WorkItem, WorkItemType,
};

/// 经验合并触发系统：当非顶层汇聚完成且候选数 > 1 时，创建合并 WorkItem。
pub(crate) fn experience_consolidation_trigger_system(
    mut commands: Commands,
    mut store: ResMut<ExperienceStore>,
    requests: Query<(Entity, &ExperienceConsolidationRequestMessage)>,
) {
    for (entity, request) in &requests {
        // Collect candidate clones to release the immutable borrow before mutating
        let candidates: Vec<ExperienceCandidate> = request
            .candidate_ids
            .iter()
            .filter_map(|id| store.candidates.get(id).cloned())
            .collect();

        if candidates.len() <= 1 {
            debug!(
                event = "ExperienceConsolidationSkipped",
                task_id = %request.task_id,
                reason = "too_few_candidates",
                "skipping consolidation, <=1 candidates"
            );
            commands.entity(entity).despawn();
            continue;
        }

        let candidate_count = candidates.len();
        let prompt = build_consolidation_prompt(&candidates, &request.candidate_kind);

        // Mark original candidates as Superseded
        for id in &request.candidate_ids {
            if let Some(candidate) = store.candidates.get_mut(id) {
                candidate.status = ExperienceCandidateStatus::Superseded;
            }
        }

        debug!(
            event = "ExperienceConsolidationWorkItemCreated",
            task_id = %request.task_id,
            candidate_count,
            kind = ?request.candidate_kind,
            "spawning consolidation work item"
        );

        // Create a WorkItem for the LLM to process
        let work_item = WorkItem::experience_collection(
            request.task_id,
            prompt,
            Some(request.parent_task_id),
            Vec::new(), // no conversation context for consolidation
            Vec::new(), // tools will be set by the dispatch system
            request.governing_agent_id,
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

fn build_consolidation_prompt(
    candidates: &[ExperienceCandidate],
    kind: &ExperienceKindHint,
) -> String {
    let kind_str = match kind {
        ExperienceKindHint::Knowledge => "知识",
        ExperienceKindHint::Skill => "技能",
    };

    let mut prompt = format!(
        "你是一个经验整理助手。以下是同一任务下多个 Agent 提交的{}候选，请对它们进行去重和合并。\n\n## 输入候选\n\n",
        kind_str
    );

    for (i, candidate) in candidates.iter().enumerate() {
        prompt.push_str(&format!("### 候选 {}: {}\n\n", i + 1, candidate.title));
        match &candidate.payload {
            ExperienceCandidatePayload::Knowledge { content } => {
                prompt.push_str(&format!("{}\n\n", content));
            }
            ExperienceCandidatePayload::Skill {
                description,
                instructions,
                ..
            } => {
                prompt.push_str(&format!(
                    "描述：{}\n\n指令：{}\n\n",
                    description, instructions
                ));
            }
        }
    }

    prompt.push_str(&format!(
        "## 要求\n\n\
         1. 去除重复或高度相似的{}\n\
         2. 合并互补的{}为更完整的版本\n\
         3. 通过调用 submit_experience_candidate 提交合并后的候选（kind=\"{}\"）\n\
         4. 如果所有候选都是重复的，只提交一个最完整的版本\n\
         5. 不要提交任何原始候选，只提交合并后的版本\n",
        kind_str,
        kind_str,
        match kind {
            ExperienceKindHint::Knowledge => "knowledge",
            ExperienceKindHint::Skill => "skill",
        }
    ));

    prompt
}
