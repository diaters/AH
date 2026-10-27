//! 图片查看工具
//!
//! 校验本地图片后返回 `[IMAGE:<绝对路径>]` 标记；工具结果进入对话后，
//! 由 LLM adapter 在视觉模型请求中把标记晚绑定为真实图片（见 `src/llm/image.rs`）。
//!
//! 异步工具：worker 内只做文件元数据校验，纯读操作。

use crate::domain::{
    BuiltinTool, MAX_IMAGE_BYTES, OwnedToolContext, ToolAction, ToolActionKind, ToolContext,
    ToolError, ToolFuture, ToolWorkerOutput, image_mime_type,
};

/// 查看本地图片文件。
///
/// 参数：
/// - `path`：图片路径，相对路径基于进程工作目录解析
///
/// 校验：普通文件、扩展名在 png/jpeg/gif/webp 白名单内、不超过 `MAX_IMAGE_BYTES`。
pub struct ViewImageTool;

impl BuiltinTool for ViewImageTool {
    fn name(&self) -> &str {
        "view_image"
    }

    fn kind(&self) -> ToolActionKind {
        ToolActionKind::Async
    }

    fn execute(&self, _: &serde_json::Value, _: &ToolContext) -> Result<ToolAction, ToolError> {
        // Async 工具不会走到这里（dispatch 按 kind 分流）；快速失败防误调
        Err(ToolError::InternalState(
            "view_image is async-only, must go through run_async".to_string(),
        ))
    }

    fn run_async(&self, input: serde_json::Value, _ctx: OwnedToolContext) -> ToolFuture {
        Box::pin(async move {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|p| !p.is_empty())
                .ok_or_else(|| ToolError::InvalidInput("missing path".to_string()))?;

            if image_mime_type(path).is_none() {
                return Err(ToolError::InvalidInput(
                    "unsupported image format: only png/jpeg/gif/webp".to_string(),
                ));
            }
            let abs_path = std::fs::canonicalize(path)
                .map_err(|e| ToolError::InvalidInput(format!("cannot access {path}: {e}")))?;
            let meta = std::fs::metadata(&abs_path)
                .map_err(|e| ToolError::InvalidInput(format!("cannot access {path}: {e}")))?;
            if !meta.is_file() {
                return Err(ToolError::InvalidInput(format!(
                    "{path} is not a regular file"
                )));
            }
            if meta.len() > MAX_IMAGE_BYTES {
                return Err(ToolError::InvalidInput(format!(
                    "image larger than {} MiB",
                    MAX_IMAGE_BYTES / (1024 * 1024)
                )));
            }

            let abs = abs_path.display().to_string();
            Ok(ToolWorkerOutput::Value(serde_json::json!({
                "path": abs,
                "bytes": meta.len(),
                "image": format!("[IMAGE:{abs}]"),
            })))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn run(path: Option<&str>) -> Result<ToolWorkerOutput, ToolError> {
        let input = match path {
            Some(p) => serde_json::json!({ "path": p }),
            None => serde_json::json!({}),
        };
        ViewImageTool
            .run_async(input, OwnedToolContext::default())
            .await
    }

    #[tokio::test]
    async fn returns_image_marker_for_valid_image() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, b"\x89PNG fake").unwrap();

        let output = run(Some(path.to_str().unwrap())).await.unwrap();

        let ToolWorkerOutput::Value(value) = output else {
            panic!("expected value output");
        };
        let abs = std::fs::canonicalize(&path).unwrap();
        assert_eq!(value["path"], abs.display().to_string());
        assert_eq!(value["bytes"], 9);
        assert_eq!(value["image"], format!("[IMAGE:{}]", abs.display()));
    }

    #[tokio::test]
    async fn rejects_missing_path_argument() {
        assert!(matches!(run(None).await, Err(ToolError::InvalidInput(_))));
        assert!(matches!(
            run(Some("")).await,
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn rejects_nonexistent_file() {
        assert!(matches!(
            run(Some("/nonexistent/a.png")).await,
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn rejects_unsupported_format_and_directory() {
        let dir = TempDir::new().unwrap();
        let bmp = dir.path().join("a.bmp");
        std::fs::write(&bmp, b"BM").unwrap();
        let fake_dir = dir.path().join("dir.png");
        std::fs::create_dir(&fake_dir).unwrap();

        assert!(matches!(
            run(Some(bmp.to_str().unwrap())).await,
            Err(ToolError::InvalidInput(_))
        ));
        assert!(matches!(
            run(Some(fake_dir.to_str().unwrap())).await,
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_image() {
        let dir = TempDir::new().unwrap();
        let big = dir.path().join("big.png");
        std::fs::write(&big, vec![0u8; (MAX_IMAGE_BYTES + 1) as usize]).unwrap();

        assert!(matches!(
            run(Some(big.to_str().unwrap())).await,
            Err(ToolError::InvalidInput(_))
        ));
    }
}
