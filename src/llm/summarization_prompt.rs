/// 构建摘要的系统 prompt，定义记忆摘要专家的角色和要求。
///
/// 此函数保留供测试使用；生产环境中 system_prompt 已移至 agents.toml。
#[allow(dead_code)]
pub fn summarization_system_prompt() -> String {
    r#"你是一个记忆摘要专家。你的任务是将对话历史压缩为简洁的摘要。

要求：
1. 保留关键事实、决策、待办事项
2. 保留重要的人物、时间、地点信息
3. 去除重复和无关内容
4. 保持摘要的可读性和连贯性
5. 目标长度：不超过指定的 token 数

输出格式：直接输出摘要内容，不需要额外说明。"#
        .to_string()
}

/// 构建摘要的用户 prompt，包含待压缩的对话历史和目标 token 数限制。
pub fn summarization_user_prompt(content: &str, target_tokens: u32) -> String {
    format!(
        "请将以下对话历史压缩为摘要，目标长度不超过 {} tokens：\n\n{}",
        target_tokens, content
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_not_empty() {
        let prompt = summarization_system_prompt();
        assert!(!prompt.is_empty());
        assert!(prompt.contains("摘要"));
    }

    #[test]
    fn user_prompt_contains_content() {
        let prompt = summarization_user_prompt("test content", 1000);
        assert!(prompt.contains("test content"));
        assert!(prompt.contains("1000"));
    }
}
