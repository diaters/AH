//! 经验候选列表工具

use crate::domain::{
    ExperienceCandidatePayload, OwnedToolContext, ToolAction, ToolActionKind, ToolContext,
    ToolError, ToolFuture, ToolWorkerOutput,
};

/// 列出经验候选工具
///
/// 允许 Agent 查看当前任务收件箱中的经验候选。
///
/// 已迁移上异步桥：`kind() == Async`，`run_async` 从 `OwnedToolContext.experience_candidates`
/// 快照读候选（dispatch 已按 `task_id` 过滤），不再访问 borrowed `ExperienceStore`。
pub struct ListExperienceCandidatesTool;

impl crate::domain::BuiltinTool for ListExperienceCandidatesTool {
    fn name(&self) -> &str {
        "list_experience_candidates"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "list_experience_candidates is async-only".to_string(),
        ))
    }

    fn run_async(&self, _input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            // 缺快照：dispatch 未注入经验候选列表
            let candidates = ctx.experience_candidates.ok_or_else(|| {
                ToolError::InternalState("experience candidates snapshot missing".to_string())
            })?;
            // 缺 current_task_id：dispatch 未注入当前任务 ID（defensive——快照已按
            // task_id 过滤，本字段不参与过滤，仅作为 dispatch 接线完整性校验）
            if ctx.current_task_id.is_none() {
                return Err(ToolError::InternalState(
                    "current_task_id missing".to_string(),
                ));
            }

            let items: Vec<serde_json::Value> = candidates
                .iter()
                .map(|candidate| {
                    let kind = format!("{:?}", candidate.kind_hint);
                    let summary = match &candidate.payload {
                        ExperienceCandidatePayload::Knowledge { content } => {
                            if content.chars().count() > 200 {
                                let truncated: String = content.chars().take(200).collect();
                                format!("{}…", truncated)
                            } else {
                                content.clone()
                            }
                        }
                        ExperienceCandidatePayload::Skill { description, .. } => {
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

            Ok(ToolWorkerOutput::Value(serde_json::json!({
                "count": items.len(),
                "items": items,
            })))
        })
    }
}
