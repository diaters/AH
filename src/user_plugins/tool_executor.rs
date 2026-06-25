//! 插件 Tool 执行器
//!
//! 提供以 `plugin_id:tool_id` 为命名空间的插件 Tool 执行能力。
//! 当前为 stub 实现，Phase 8 集成测试时补充完整 Rhai 执行逻辑。

use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};

/// 插件贡献的 Tool 执行器
///
/// 每个 RhaiToolExecutor 对应一个插件贡献的 tool，
/// 以 `plugin_id:tool_id` 形式注册到 BuiltinToolExecutors。
pub struct RhaiToolExecutor {
    /// 命名空间全名（`plugin_id:tool_id`），同时作为 BuiltinToolExecutors 的 key。
    namespaced: String,
}

impl RhaiToolExecutor {
    pub fn new(plugin_id: &str, tool_id: &str) -> Self {
        Self {
            namespaced: format!("{}:{}", plugin_id, tool_id),
        }
    }
}

impl BuiltinTool for RhaiToolExecutor {
    fn name(&self) -> &str {
        &self.namespaced
    }

    fn execute(
        &self,
        _input: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolAction, ToolError> {
        // Stub：Phase 8 集成测试时补充完整 Rhai 执行逻辑。
        Ok(ToolAction::Direct(serde_json::Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_name_format() {
        let executor = RhaiToolExecutor::new("alpha", "search");
        assert_eq!(executor.name(), "alpha:search");
    }
}
