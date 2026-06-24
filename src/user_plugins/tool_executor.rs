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
    pub plugin_id: String,
    pub tool_id: String,
}

impl RhaiToolExecutor {
    /// 生成命名空间全名，格式为 `plugin_id:tool_id`
    pub fn namespaced_name(&self) -> String {
        format!("{}:{}", self.plugin_id, self.tool_id)
    }
}

impl BuiltinTool for RhaiToolExecutor {
    fn name(&self) -> &str {
        // BuiltinTool::name 返回的是注册名，此处用固定字符串
        // 实际 namespaced_name 在注册时作为 key 使用。
        // 由于 BuiltinTool::name 需要返回 &str，我们无法返回动态字符串。
        // 使用 tool_id 作为基准，注册时以 namespaced_name 为 key。
        &self.tool_id
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
        let executor = RhaiToolExecutor {
            plugin_id: "alpha".to_string(),
            tool_id: "search".to_string(),
        };
        assert_eq!(executor.namespaced_name(), "alpha:search");
    }
}
