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
                serde_json::json!({
                    "candidate_id": candidate.candidate_id,
                    "title": candidate.title,
                    "kind_hint": format!("{:?}", candidate.kind_hint),
                    "status": format!("{:?}", candidate.status),
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
    use crate::domain::{
        BuiltinTool, ExperienceCandidate, ExperienceStore, SharedKnowledgeBase,
    };

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
            current_task_id: task_id,
            current_agent_id: agent_id,
        };

        let tool = ListExperienceCandidatesTool;
        let action = tool.execute(&serde_json::json!({}), &ctx).unwrap();
        match action {
            ToolAction::Direct(value) => {
                assert_eq!(value["count"], 1);
            }
            other => panic!("expected direct action, got {:?}", other),
        }
    }
}
