//! 精简后的 shell builtin 只导出当前仍对 LLM 暴露的六个工具。
//!
//! 会话型五工具（start / read / list / input / stop）合并在 `shell_tools.rs`；
//! `shell_exec` 体量独立，留在 `exec.rs`。

use std::collections::HashMap;

use crate::domain::ToolError;

mod exec;
mod shell_tools;

pub use exec::ShellExecTool;
pub use shell_tools::{
    ShellInputTool, ShellListTool, ShellReadTool, ShellStartTool, ShellStopTool,
};

/// 解析 shell 工具输入中的环境变量对象。
fn parse_env_map(input: &serde_json::Value) -> Result<HashMap<String, String>, ToolError> {
    let Some(env) = input.get("env") else {
        return Ok(HashMap::new());
    };

    let object = env
        .as_object()
        .ok_or_else(|| ToolError::InvalidInput("'env' must be an object".to_string()))?;

    object
        .iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .ok_or_else(|| ToolError::InvalidInput(format!("'env.{key}' must be a string")))?;
            Ok((key.clone(), value.to_string()))
        })
        .collect()
}
