//! ADR-006：skill 文件读取工具
//!
//! 对 skill-creator 与 skill-updater 共用（均持有 `skill` tag），
//! 用于读取 skill 目录下的子文件内容。
//!
//! 异步工具：worker 内做路径校验并直接读取文件内容，返回
//! `ToolWorkerOutput::Value`（纯读操作，worker 内执行安全）。

use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolError, ToolFuture,
    ToolWorkerOutput,
};
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

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "read_skill_file is async-only, must go through run_async".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
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

            // 从 OwnedToolContext 获取 skill 目录
            let skill_dir = match ctx.current_skill_dir {
                Some(ref d) => d.clone(),
                None => {
                    return Err(ToolError::InvalidInput(
                        "no skill directory in current context".to_string(),
                    ));
                }
            };

            // 路径校验
            let abs_path = validate_skill_file_path(&path, &skill_dir, ALLOWED_FILE_SUFFIXES)
                .map_err(|e| ToolError::InvalidInput(format!("invalid path: {}", e)))?;

            // 读取文件
            let content = std::fs::read_to_string(&abs_path)
                .map_err(|e| ToolError::InvalidInput(format!("failed to read file: {}", e)))?;

            Ok(ToolWorkerOutput::Value(serde_json::json!({
                "path": path,
                "content": content,
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_ctx(skill_dir: Option<PathBuf>) -> OwnedToolContext {
        OwnedToolContext {
            current_skill_dir: skill_dir,
            ..Default::default()
        }
    }

    fn run(
        tool: &ReadSkillFileTool,
        input: serde_json::Value,
        ctx: OwnedToolContext,
    ) -> Result<ToolWorkerOutput, ToolError> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(tool.run_async(input, ctx))
    }

    #[test]
    fn run_async_reads_existing_md_file() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();
        std::fs::write(skill_dir.join("download.md"), "# Download\n\nSteps...").unwrap();

        let tool = ReadSkillFileTool;
        let result = run(
            &tool,
            serde_json::json!({ "path": "download.md" }),
            test_ctx(Some(skill_dir)),
        );
        match result {
            Ok(ToolWorkerOutput::Value(val)) => {
                assert_eq!(val["path"], "download.md");
                assert!(val["content"].as_str().unwrap().contains("Download"));
            }
            other => panic!("expected Value, got: {:?}", other),
        }
    }

    #[test]
    fn run_async_reads_existing_py_file() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("scripts/run.py"),
            "#!/usr/bin/env python3\nprint('hi')",
        )
        .unwrap();

        let tool = ReadSkillFileTool;
        let result = run(
            &tool,
            serde_json::json!({ "path": "scripts/run.py" }),
            test_ctx(Some(skill_dir)),
        );
        match result {
            Ok(ToolWorkerOutput::Value(val)) => {
                assert!(val["content"].as_str().unwrap().contains("python3"));
            }
            other => panic!("expected Value, got: {:?}", other),
        }
    }

    #[test]
    fn run_async_rejects_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();

        let tool = ReadSkillFileTool;
        let result = run(
            &tool,
            serde_json::json!({ "path": "../../etc/passwd" }),
            test_ctx(Some(skill_dir)),
        );
        assert!(result.is_err(), "path traversal should be rejected");
    }

    #[test]
    fn run_async_rejects_disallowed_suffix() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();
        std::fs::write(skill_dir.join("evil.rs"), "fn main() {}").unwrap();

        let tool = ReadSkillFileTool;
        let result = run(
            &tool,
            serde_json::json!({ "path": "evil.rs" }),
            test_ctx(Some(skill_dir)),
        );
        assert!(result.is_err(), ".rs suffix should be rejected");
    }

    #[test]
    fn run_async_rejects_missing_path() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().to_path_buf();

        let tool = ReadSkillFileTool;
        let result = run(&tool, serde_json::json!({}), test_ctx(Some(skill_dir)));
        assert!(result.is_err());
    }

    #[test]
    fn run_async_rejects_no_skill_dir_in_context() {
        let tool = ReadSkillFileTool;
        let result = run(
            &tool,
            serde_json::json!({ "path": "download.md" }),
            test_ctx(None),
        );
        assert!(
            result.is_err(),
            "should reject when no skill_dir in context"
        );
    }

    #[test]
    fn execute_returns_internal_state_async_only() {
        // 迁移到 async bridge 后，execute 不再被 dispatch 调用，快速失败防误调
        use crate::domain::{ExperienceStore, SharedKnowledgeBase};
        static KNOWLEDGE: std::sync::OnceLock<SharedKnowledgeBase> = std::sync::OnceLock::new();
        static STORE: std::sync::OnceLock<ExperienceStore> = std::sync::OnceLock::new();
        let knowledge = KNOWLEDGE.get_or_init(SharedKnowledgeBase::default);
        let store = STORE.get_or_init(ExperienceStore::default);
        let ctx = ToolContext {
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
            current_skill_dir: None,
        };
        let tool = ReadSkillFileTool;
        let result = tool.execute(&serde_json::json!({ "path": "download.md" }), &ctx);
        assert!(
            matches!(result, Err(ToolError::InternalState(_))),
            "expected InternalState (async-only), got {:?}",
            result
        );
    }
}
