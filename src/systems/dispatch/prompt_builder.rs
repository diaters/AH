//! 共享 prompt 构建模块
//!
//! 提供带历史对话、长期记忆与通道上下文的 prompt 构建，
//! 供 `dispatch_system` 的 DirectDelegate 分支使用。

use crate::domain::{ChannelId, LongTermMemory, ShortTermMemory, render_tool_calls_summary};

use super::memory_selection::{MemorySelectionBudget, select_long_term_memories};

/// 构建带历史对话、长期记忆与当前通道信息的 prompt。
///
/// 顺序：
/// 1. 长期记忆（Agent 专属经验，按预算筛选 core + relevant）
/// 2. 短期记忆（对话历史，含 Summary 前缀；Archive 跳过）
/// 3. 当前通道上下文（用于 LLM 路由回源会话）
/// 4. 当前请求
pub(crate) fn build_prompt_with_context(
    task_content: &str,
    short_term: Option<&ShortTermMemory>,
    long_term: Option<&LongTermMemory>,
    origin_channel: Option<&ChannelId>,
) -> String {
    let mut parts = Vec::new();

    // 1. 长期记忆（Agent 专属经验）
    if let Some(ltm) = long_term
        && !ltm.entries.is_empty()
    {
        let selected = select_long_term_memories(
            task_content,
            ltm,
            MemorySelectionBudget {
                max_core_entries: 5,
                max_relevant_entries: 5,
                max_relevant_tokens: 800,
            },
        );

        append_memory_section(&mut parts, "[Core agent memory]", &selected.core);
        append_memory_section(&mut parts, "[Relevant agent memory]", &selected.relevant);
    }

    // 2. 短期记忆（对话历史）
    if let Some(stm) = short_term
        && !stm.entries.is_empty()
    {
        let mut history = String::new();

        // 添加摘要前缀（如果有）
        if let Some(summary) = &stm.summary_prefix {
            history.push_str(&format!("[Previous context summary]\n{}\n\n", summary));
        }

        // 添加对话历史
        history.push_str("[Conversation history]\n");
        for entry in &stm.entries {
            let role = match entry.role {
                crate::domain::EntryRole::User => "User",
                crate::domain::EntryRole::Assistant => "Assistant",
                crate::domain::EntryRole::Summary => "System note",
                crate::domain::EntryRole::Archive => continue,
            };
            let mut line = format!("{}: {}", role, entry.content);
            // 防御性渲染 tool_calls（结构化路径不可用时的降级）
            if !entry.metadata.tool_calls.is_empty() {
                line.push_str(&format!("\n  {}", render_tool_calls_summary(&entry.metadata.tool_calls)));
            }
            history.push_str(&line);
            history.push('\n');
        }

        parts.push(history.trim_end().to_string());
    }

    // 3. 当前通道上下文，帮助 LLM 正确路由文件/消息到来源会话
    if let Some(ch) = origin_channel {
        parts.push(ch.to_prompt_context());
    }

    // 4. 当前请求
    parts.push(format!("[Current request]\n{}", task_content));
    parts.join("\n\n")
}

/// 将选中的长期记忆格式化为 prompt 分段并追加到结果中。
fn append_memory_section(
    parts: &mut Vec<String>,
    title: &str,
    entries: &[crate::domain::LongTermMemoryEntry],
) {
    if entries.is_empty() {
        return;
    }

    let content = entries
        .iter()
        .map(|entry| format!("- {}", entry.content))
        .collect::<Vec<_>>()
        .join("\n");
    parts.push(format!("{title}\n{content}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ChannelId, EntryMetadata, EntryRole, FrontendKind, LongTermMemory, LongTermMemoryEntry,
        MemoryImportance, ShortTermMemory,
    };

    /// 创建一个测试用的 ChannelId。
    fn make_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "12345".to_string(),
            thread_id: None,
        }
    }

    #[test]
    fn prompt_includes_summary_entries_as_system_notes() {
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, "user message", EntryMetadata::default());
        stm.add_entry(
            EntryRole::Assistant,
            "assistant response",
            EntryMetadata::default(),
        );
        // 模拟 AutoCorrect 注入的纠偏上下文
        let metadata = EntryMetadata {
            keywords: vec![
                "evaluation".to_string(),
                "offtrack".to_string(),
                "autocorrect".to_string(),
            ],
            ..Default::default()
        };
        stm.add_entry(
            EntryRole::Summary,
            "[Evaluation AutoCorrect] refocus on original goal",
            metadata,
        );

        let prompt =
            build_prompt_with_context("do the task", Some(&stm), None, Some(&make_channel()));

        assert!(
            prompt.contains("System note: [Evaluation AutoCorrect] refocus on original goal"),
            "prompt should include Summary entry as System note, got: {}",
            prompt
        );
    }

    #[test]
    fn prompt_excludes_archive_entries() {
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, "user message", EntryMetadata::default());
        stm.add_entry(
            EntryRole::Archive,
            "archived content",
            EntryMetadata::default(),
        );

        let prompt =
            build_prompt_with_context("do the task", Some(&stm), None, Some(&make_channel()));

        assert!(
            !prompt.contains("archived content"),
            "prompt should NOT include Archive entries, got: {}",
            prompt
        );
    }

    #[test]
    fn prompt_includes_only_core_and_relevant_long_term_memory() {
        let long_term = LongTermMemory {
            agent_name: None,
            entries: vec![
                LongTermMemoryEntry {
                    content: "Always keep shell tools truthful".to_string(),
                    scope_tags: vec!["shell".to_string()],
                    importance: MemoryImportance::Critical,
                    pin: true,
                    created_at: chrono::Utc::now(),
                    last_accessed_at: None,
                    reuse_count: 0,
                    decay_score: 1.0,
                    source: "migration".to_string(),
                    confidence: 1.0,
                    source_candidate_id: None,
                    source_task_id: None,
                    agent_id: None,
                },
                LongTermMemoryEntry {
                    content: "Use bounded timeout handling for shell commands".to_string(),
                    scope_tags: vec!["shell".to_string()],
                    importance: MemoryImportance::High,
                    pin: false,
                    created_at: chrono::Utc::now(),
                    last_accessed_at: None,
                    reuse_count: 0,
                    decay_score: 1.0,
                    source: "migration".to_string(),
                    confidence: 0.9,
                    source_candidate_id: None,
                    source_task_id: None,
                    agent_id: None,
                },
                LongTermMemoryEntry {
                    content: "Unrelated frontend palette note".to_string(),
                    scope_tags: vec!["ui".to_string()],
                    importance: MemoryImportance::Low,
                    pin: false,
                    created_at: chrono::Utc::now(),
                    last_accessed_at: None,
                    reuse_count: 0,
                    decay_score: 0.1,
                    source: "migration".to_string(),
                    confidence: 0.6,
                    source_candidate_id: None,
                    source_task_id: None,
                    agent_id: None,
                },
            ],
        };

        let prompt = build_prompt_with_context(
            "please improve shell timeout behavior",
            None,
            Some(&long_term),
            Some(&make_channel()),
        );

        assert!(prompt.contains("[Core agent memory]"));
        assert!(prompt.contains("Always keep shell tools truthful"));
        assert!(prompt.contains("[Relevant agent memory]"));
        assert!(prompt.contains("Use bounded timeout handling for shell commands"));
        assert!(!prompt.contains("Unrelated frontend palette note"));
    }
}
