//! 用户指令定义
//!
//! 定义 /btw、/finish 等用户指令。

/// 用户指令
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCommand {
    /// /btw - 创建子任务承接新话题
    NewTask { topic: String },
    /// /finish - 结束当前任务
    FinishCurrentTask,
    /// /summarize - 触发总结
    Summarize,
    /// /remember - 添加知识到 SpaceKnowledge
    Remember { content: String },
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
        } else {
            Self::PlainText(input.to_string())
        }
    }

    /// 判断是否是指令
    pub fn is_command(&self) -> bool {
        !matches!(self, Self::PlainText(_))
    }
}
