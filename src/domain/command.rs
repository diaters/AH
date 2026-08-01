//! 用户指令定义
//!
//! 定义 /btw、/finish 等用户指令，以及插件 slash command 识别。

/// 用户指令
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCommand {
    /// /btw - 创建子任务承接新话题
    NewTask { topic: String },
    /// /finish - 结束当前任务
    FinishCurrentTask,
    /// /clear - 删除当前任务（不触发终态处理链路）
    ClearCurrentTask,
    /// /summarize - 触发总结
    Summarize,
    /// /remember - 添加知识到 SharedKnowledgeBase
    Remember { content: String },
    /// /plugins - 列出已加载的插件
    ListPlugins,
    /// /reload-plugins - 重新加载所有插件
    ReloadPlugins,
    /// /reload-triggers - 重新加载事件触发配置
    ReloadTriggers,
    /// 插件贡献的 slash command，格式为 /plugin_id:command args
    PluginCommand {
        plugin_id: String,
        command: String,
        args: String,
    },
    /// 普通输入（非指令）
    PlainText(String),
}

impl UserCommand {
    /// 解析用户输入
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        if trimmed.starts_with("/btw ") {
            Self::NewTask {
                topic: trimmed[4..].trim().to_string(),
            }
        } else if trimmed == "/btw" {
            Self::NewTask {
                topic: String::new(),
            }
        } else if trimmed == "/finish" {
            Self::FinishCurrentTask
        } else if trimmed == "/clear" {
            Self::ClearCurrentTask
        } else if trimmed == "/summarize" {
            Self::Summarize
        } else if let Some(stripped) = trimmed.strip_prefix("/remember ") {
            Self::Remember {
                content: stripped.trim().to_string(),
            }
        } else if trimmed == "/remember" {
            Self::Remember {
                content: String::new(),
            }
        } else if trimmed == "/plugins" {
            Self::ListPlugins
        } else if trimmed == "/reload-plugins" {
            Self::ReloadPlugins
        } else if trimmed == "/reload-triggers" {
            Self::ReloadTriggers
        } else if let Some(rest) = trimmed.strip_prefix('/') {
            // 插件 slash command 识别：/plugin_id:command [args]
            // 必须在 `/` 之后、空格之前包含 `:`，且 `:` 两侧均不为空
            let first_space = rest.find(' ').unwrap_or(rest.len());
            let cmd_part = &rest[..first_space];
            if let Some(colon_pos) = cmd_part.find(':') {
                let plugin_id = &cmd_part[..colon_pos];
                let command = &cmd_part[colon_pos + 1..];
                if !plugin_id.is_empty() && !command.is_empty() {
                    let args = rest[first_space..].trim().to_string();
                    return Self::PluginCommand {
                        plugin_id: plugin_id.to_string(),
                        command: command.to_string(),
                        args,
                    };
                }
            }
            Self::PlainText(input.to_string())
        } else {
            Self::PlainText(input.to_string())
        }
    }

    /// 判断是否是指令
    pub fn is_command(&self) -> bool {
        !matches!(self, Self::PlainText(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clear() {
        let cmd = UserCommand::parse("/clear");
        assert_eq!(cmd, UserCommand::ClearCurrentTask);
        assert!(cmd.is_command());
    }
}
