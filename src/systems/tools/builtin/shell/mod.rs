//! 精简后的 shell builtin 只导出当前仍对 LLM 暴露的六个工具。

use std::collections::HashMap;

use crate::domain::ToolError;

mod exec;
mod input;
mod list;
mod read;
mod start;
mod stop;

pub use exec::ShellExecTool;
pub use input::ShellInputTool;
pub use list::ShellListTool;
pub use read::ShellReadTool;
pub use start::ShellStartTool;
pub use stop::ShellStopTool;

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
