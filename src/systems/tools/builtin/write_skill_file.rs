//! skill 沙盒文件写入工具
//!
//! 异步工具：worker 校验输入并产生 `ToolEffect::WriteSkillFile`，
//! `commit_tool_effects_system` 在主线程执行实际文件 I/O。

use crate::domain::{
    BuiltinTool, OwnedToolContext, ToolAction, ToolActionKind, ToolContext, ToolEffect, ToolError,
    ToolFuture, ToolWorkerOutput,
};
use crate::infrastructure::skills::diff::{ALLOWED_FILE_SUFFIXES, validate_skill_file_path};

/// 写入 skill 沙盒文件
///
/// 由 skill-creator Agent 调用，在沙盒目录下创建或覆盖文件。
/// 路径安全校验（禁止 `..` 遍历）与后缀白名单校验在 worker 内完成；
/// 实际文件写入由 `commit_tool_effects_system` 在主线程执行。
pub struct WriteSkillFileTool;

impl BuiltinTool for WriteSkillFileTool {
    fn name(&self) -> &str {
        "write_skill_file"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "write_skill_file is async-only, must go through run_async".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            let path = match input.get("path").and_then(|v| v.as_str()) {
                Some(p) => p.trim().to_string(),
                None => {
                    return Err(ToolError::InvalidInput("path is required".to_string()));
                }
            };
            let content = match input.get("content").and_then(|v| v.as_str()) {
                Some(c) => c.to_string(),
                None => {
                    return Err(ToolError::InvalidInput("content is required".to_string()));
                }
            };

            // 确认 skill 目录存在
            let skill_dir = match ctx.current_skill_dir {
                Some(ref d) => d.clone(),
                None => {
                    return Err(ToolError::InvalidInput(
                        "no skill directory in current context".to_string(),
                    ));
                }
            };

            // 路径校验：复用 validate_skill_file_path（与 read_skill_file 一致）
            if let Err(e) = validate_skill_file_path(&path, &skill_dir, ALLOWED_FILE_SUFFIXES) {
                return Err(ToolError::InvalidInput(format!("invalid path: {}", e)));
            }

            // 声明式写效果：sandbox_dir 在此嵌入，commit_tool_effects_system 直接使用
            Ok(ToolWorkerOutput::Effect(ToolEffect::WriteSkillFile {
                sandbox_dir: skill_dir,
                path,
                content,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::BuiltinTool;
    use std::path::PathBuf;

    fn test_ctx(skill_dir: Option<PathBuf>) -> OwnedToolContext {
        let mut ctx = OwnedToolContext::empty_for_test(300);
        ctx.current_skill_dir = skill_dir;
        ctx
    }

    fn run_async_blocking(
        input: serde_json::Value,
        ctx: OwnedToolContext,
    ) -> Result<ToolWorkerOutput, ToolError> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(WriteSkillFileTool.run_async(input, ctx))
    }

    #[test]
    fn write_skill_file_is_async() {
        assert_eq!(WriteSkillFileTool.kind(), ToolActionKind::Async);
    }

    #[test]
    fn write_skill_file_rejects_path_traversal() {
        let ctx = test_ctx(Some(PathBuf::from("/tmp/skill")));
        let result = run_async_blocking(
            serde_json::json!({
                "path": "../escape.md",
                "content": "evil"
            }),
            ctx,
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("..")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn write_skill_file_rejects_missing_skill_dir() {
        let ctx = test_ctx(None);
        let result = run_async_blocking(
            serde_json::json!({
                "path": "test.md",
                "content": "hello"
            }),
            ctx,
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(msg.contains("skill directory")),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn write_skill_file_produces_write_effect() {
        let ctx = test_ctx(Some(PathBuf::from("/tmp/skill")));
        let result = run_async_blocking(
            serde_json::json!({
                "path": "download.md",
                "content": "# Download\n\nSteps..."
            }),
            ctx,
        );
        let output = result.unwrap();
        match output {
            ToolWorkerOutput::Effect(ToolEffect::WriteSkillFile {
                sandbox_dir,
                path,
                content,
            }) => {
                assert_eq!(sandbox_dir, PathBuf::from("/tmp/skill"));
                assert_eq!(path, "download.md");
                assert!(content.contains("Download"));
            }
            other => panic!("expected WriteSkillFile effect, got {:?}", other),
        }
    }

    #[test]
    fn write_skill_file_rejects_disallowed_suffix() {
        let ctx = test_ctx(Some(PathBuf::from("/tmp/skill")));
        let result = run_async_blocking(
            serde_json::json!({
                "path": "evil.rs",
                "content": "fn main() {}"
            }),
            ctx,
        );
        assert!(result.is_err(), ".rs suffix should be rejected");
        match result.unwrap_err() {
            ToolError::InvalidInput(msg) => assert!(
                msg.contains("suffix"),
                "expected suffix error, got: {}",
                msg
            ),
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }
}
