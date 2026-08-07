//! ADR-006：skill 文件读取工具
//!
//! 仅对 skill-updater Agent 开放，用于读取 skill 目录下的子文件内容。

use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};
use crate::infrastructure::skills::diff::{ALLOWED_FILE_SUFFIXES, validate_skill_file_path};

/// 读取 skill 目录下的子文件内容。
///
/// 参数：
/// - `path`：相对于 skill 目录的文件路径（如 `download.md`、`scripts/redmine_download.py`）
///
/// 路径校验：在 skill 目录内 + 后缀白名单。
/// SKILL.md 不需要通过此工具读取——其内容已在 prompt 中提供。
pub struct ReadSkillFileTool;

impl BuiltinTool for ReadSkillFileTool {
    fn name(&self) -> &str {
        "read_skill_file"
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing path".to_string()))?
            .to_string();

        if path.is_empty() {
            return Err(ToolError::InvalidInput(
                "path must not be empty".to_string(),
            ));
        }

        // 从 ToolContext 获取 skill 目录
        let skill_dir = ctx.current_skill_dir.as_ref().ok_or_else(|| {
            ToolError::InvalidInput("no skill directory in current context".to_string())
        })?;

        // 路径校验
        let abs_path = validate_skill_file_path(&path, skill_dir, ALLOWED_FILE_SUFFIXES)
            .map_err(|e| ToolError::InvalidInput(format!("invalid path: {}", e)))?;

        // 读取文件
        let content = std::fs::read_to_string(&abs_path)
            .map_err(|e| ToolError::InvalidInput(format!("failed to read file: {}", e)))?;

        Ok(ToolAction::Direct(serde_json::json!({
            "path": path,
            "content": content,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ExperienceStore, SharedKnowledgeBase};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_ctx(skill_dir: Option<PathBuf>) -> ToolContext<'static> {
        static KNOWLEDGE: std::sync::OnceLock<SharedKnowledgeBase> = std::sync::OnceLock::new();
        static STORE: std::sync::OnceLock<ExperienceStore> = std::sync::OnceLock::new();
        let knowledge = KNOWLEDGE.get_or_init(SharedKnowledgeBase::default);
        let store = STORE.get_or_init(ExperienceStore::default);
        ToolContext {
            knowledge,
            experience_store: store,
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
            current_origin_channel: None,
            current_skill_dir: skill_dir,
        }
    }

    #[test]
    fn read_existing_md_file() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();
        std::fs::write(skill_dir.join("download.md"), "# Download\n\nSteps...").unwrap();

        let ctx = test_ctx(Some(skill_dir));
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({ "path": "download.md" }), &ctx);
        match result {
            Ok(ToolAction::Direct(val)) => {
                assert_eq!(val["path"], "download.md");
                assert!(val["content"].as_str().unwrap().contains("Download"));
            }
            other => panic!("expected Direct, got: {:?}", other),
        }
    }

    #[test]
    fn read_existing_py_file() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("scripts/run.py"),
            "#!/usr/bin/env python3\nprint('hi')",
        )
        .unwrap();

        let ctx = test_ctx(Some(skill_dir));
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({ "path": "scripts/run.py" }), &ctx);
        match result {
            Ok(ToolAction::Direct(val)) => {
                assert!(val["content"].as_str().unwrap().contains("python3"));
            }
            other => panic!("expected Direct, got: {:?}", other),
        }
    }

    #[test]
    fn reject_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();

        let ctx = test_ctx(Some(skill_dir));
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({ "path": "../../etc/passwd" }), &ctx);
        assert!(result.is_err(), "path traversal should be rejected");
    }

    #[test]
    fn reject_disallowed_suffix() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();
        std::fs::write(skill_dir.join("evil.rs"), "fn main() {}").unwrap();

        let ctx = test_ctx(Some(skill_dir));
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({ "path": "evil.rs" }), &ctx);
        assert!(result.is_err(), ".rs suffix should be rejected");
    }

    #[test]
    fn reject_missing_path() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();

        let ctx = test_ctx(Some(skill_dir));
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({}), &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn reject_no_skill_dir_in_context() {
        let ctx = test_ctx(None);
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({ "path": "download.md" }), &ctx);
        assert!(
            result.is_err(),
            "should reject when no skill_dir in context"
        );
    }
}
