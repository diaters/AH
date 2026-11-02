//! load_skill：按名称读取技能的完整说明。
//!
//! 系统提示只列技能名称与描述；Agent 需要时调用本工具取回 SKILL.md 正文与技能目录。
//! 异步工具：worker 内只读 `SkillCatalog` 快照，不触碰 ECS。

use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError, ToolFuture,
    ToolWorkerOutput,
};
use crate::infrastructure::skills::{SkillCatalog, relativize_to_workspace};

pub struct LoadSkillTool {
    catalog: SkillCatalog,
}

impl LoadSkillTool {
    pub fn new(catalog: SkillCatalog) -> Self {
        Self { catalog }
    }
}

impl BuiltinTool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        Err(ToolError::InternalState(
            "load_skill is async-only, must go through run_async".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, _ctx: OwnedToolContext) -> ToolFuture {
        let catalog = self.catalog.clone();
        Box::pin(async move {
            let name = input
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .ok_or_else(|| ToolError::InvalidInput("missing name".to_string()))?;
            let skill = catalog.find(name).ok_or_else(|| {
                ToolError::InvalidInput(format!(
                    "unknown skill '{name}'; use a name from the skill list in the system prompt"
                ))
            })?;
            Ok(ToolWorkerOutput::Value(serde_json::json!({
                "name": skill.name,
                "skill_dir": relativize_to_workspace(&skill.skill_dir).display().to_string(),
                "note": "说明中的相对路径（脚本、模板等）基于 skill_dir",
                "dependencies": skill.dependencies,
                "instructions": skill.instructions,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::skills::LoadedSkill;
    use std::path::PathBuf;

    fn tool() -> LoadSkillTool {
        let catalog = SkillCatalog::default();
        catalog.replace(vec![LoadedSkill {
            name: "redmine".to_string(),
            description: "d".to_string(),
            instructions: "1. 下载日志".to_string(),
            dependencies: vec!["base".to_string()],
            agents: vec![],
            skill_dir: PathBuf::from(".harness/assets/skills/redmine"),
        }]);
        LoadSkillTool::new(catalog)
    }

    #[tokio::test]
    async fn returns_full_instructions_and_dir() {
        let output = tool()
            .run_async(
                serde_json::json!({"name": "redmine"}),
                OwnedToolContext::default(),
            )
            .await
            .unwrap();
        let ToolWorkerOutput::Value(value) = output else {
            panic!("expected value output");
        };
        assert_eq!(value["instructions"], "1. 下载日志");
        assert_eq!(value["skill_dir"], ".harness/assets/skills/redmine");
        assert_eq!(value["dependencies"], serde_json::json!(["base"]));
    }

    #[tokio::test]
    async fn rejects_unknown_or_missing_name() {
        for input in [serde_json::json!({"name": "nope"}), serde_json::json!({})] {
            let result = tool().run_async(input, OwnedToolContext::default()).await;
            assert!(matches!(result, Err(ToolError::InvalidInput(_))));
        }
    }
}
