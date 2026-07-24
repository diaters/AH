//! 经验候选列表工具

use crate::domain::{ToolAction, ToolContext, ToolError};

/// 列出经验候选工具
///
/// 允许 Agent 查看当前任务收件箱中的经验候选。
pub struct ListExperienceCandidatesTool;

impl crate::domain::BuiltinTool for ListExperienceCandidatesTool {
    fn name(&self) -> &str {
        "list_experience_candidates"
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let items: Vec<serde_json::Value> = ctx
            .experience_store
            .list_for_task(ctx.current_task_id)
            .into_iter()
            .map(|candidate| {
                let kind = format!("{:?}", candidate.kind_hint);
                let summary = match &candidate.payload {
                    crate::domain::ExperienceCandidatePayload::Knowledge { content } => {
                        if content.chars().count() > 200 {
                            let truncated: String = content.chars().take(200).collect();
                            format!("{}…", truncated)
                        } else {
                            content.clone()
                        }
                    }
                    crate::domain::ExperienceCandidatePayload::Skill { description, .. } => {
                        description.clone()
                    }
                };
                serde_json::json!({
                    "candidate_id": candidate.candidate_id,
                    "title": candidate.title,
                    "kind": kind,
                    "status": format!("{:?}", candidate.status),
                    "summary": summary,
                })
            })
            .collect();

        Ok(ToolAction::Direct(serde_json::json!({
            "count": items.len(),
            "items": items,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BuiltinTool, ExperienceCandidate, ExperienceStore, SharedKnowledgeBase};

    #[test]
    fn list_experience_candidates_reads_current_task_inbox() {
        let knowledge = SharedKnowledgeBase::default();
        let mut store = ExperienceStore::default();
        let task_id = uuid::Uuid::new_v4();
        let agent_id = uuid::Uuid::new_v4();
        store.queue_for_parent(
            task_id,
            agent_id,
            ExperienceCandidate::knowledge(
                uuid::Uuid::new_v4(),
                task_id,
                agent_id,
                "shell timeout".to_string(),
                "shell_stop 默认等待退出".to_string(),
            ),
        );

        let ctx = ToolContext {
            knowledge: &knowledge,
            experience_store: &store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: task_id,
            current_agent_id: agent_id,
            current_origin_channel: None,
        };

        let tool = ListExperienceCandidatesTool;
        let action = tool.execute(&serde_json::json!({}), &ctx).unwrap();
        match action {
            ToolAction::Direct(value) => {
                assert_eq!(value["count"], 1);
                let item = &value["items"][0];
                assert_eq!(item["kind"], "Knowledge");
                assert_eq!(item["summary"], "shell_stop 默认等待退出");
                assert!(
                    item.get("kind_hint").is_none(),
                    "kind_hint should be replaced by kind"
                );
            }
            other => panic!("expected direct action, got {:?}", other),
        }
    }
}
