//! 任务分发 System
//!
//! 将任务分发给合适的 Agent 执行。

use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::Clock,
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        LongTermMemory, ShortTermMemory, SpaceToolRegistry, Task, TaskStatus, ToolPermission,
    },
};

use super::agent_selection::{match_score, select_agent_with_memory};

/// 构建带历史对话和长期记忆的 prompt
fn build_prompt_with_context(
    task_content: &str,
    short_term: Option<&ShortTermMemory>,
    long_term: Option<&LongTermMemory>,
) -> String {
    let mut parts = Vec::new();

    // 1. 长期记忆（Agent 专属经验）
    if let Some(ltm) = long_term
        && !ltm.entries.is_empty()
    {
        let memory_text: String = ltm
            .entries
            .iter()
            .map(|e| &e.content)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("[Agent memory]\n{}", memory_text));
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
            history.push_str(&format!("{}: {}\n", role, entry.content));
        }

        parts.push(history.trim_end().to_string());
    }

    // 3. 当前请求（如果有上下文则添加前缀，否则直接返回）
    if parts.is_empty() {
        task_content.to_string()
    } else {
        parts.push(format!("[Current request]\n{}", task_content));
        parts.join("\n\n")
    }
}

/// 任务分发 System
///
/// 将任务分发给最合适的 Agent 执行。
pub fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>)>,
    agents: Query<(&Agent, Option<&LongTermMemory>)>,
    registry: Res<SpaceToolRegistry>,
) {
    for (mut task, short_term) in &mut tasks {
        // 子任务由 Brain 分发，普通 dispatch 不处理
        if task.parent_task_id.is_some() {
            continue;
        }

        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 收集候选 Agent 信息
        let candidates_info: Vec<_> = agents
            .iter()
            .filter(|(a, _)| a.kind == AgentKind::Persistent)
            .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
            .map(|(a, ltm)| {
                (
                    a.profile.name.clone(),
                    match_score(a, &task.content),
                    ltm.map(|l| l.entries.len()).unwrap_or(0),
                )
            })
            .collect();

        let Some((agent, long_term)) = select_agent_with_memory(agents.iter(), &task.content)
        else {
            debug!(
                event = "NoAgentAvailable",
                task_id = %task.id,
                task_content = %task.content,
                task_status = ?task.status,
                candidates_count = candidates_info.len(),
                candidates = ?candidates_info,
                "no available agent for task dispatch"
            );
            continue;
        };

        // 构建带历史对话和长期记忆的 prompt
        let prompt = build_prompt_with_context(&task.content, short_term, long_term);
        let stm_entries = short_term.map(|s| s.entries.len()).unwrap_or(0);
        let stm_tokens = short_term.map(|s| s.estimated_tokens).unwrap_or(0);
        let ltm_entries = long_term.map(|l| l.entries.len()).unwrap_or(0);

        debug!(
            event = "AgentSelected",
            task_id = %task.id,
            task_content = %task.content,
            task_status = ?task.status,
            selected_agent = %agent.profile.name,
            selected_agent_id = %agent.id,
            selection_reason = "highest_score",
            candidates = ?candidates_info,
            stm_entries = stm_entries,
            stm_tokens = stm_tokens,
            stm_recent_entries = ?short_term.map(|s| s.entries.iter().rev().take(3).map(|e| (&e.role, &e.content)).collect::<Vec<_>>()),
            ltm_entries = ltm_entries,
            "agent selected for task"
        );

        debug!(
            event = "PromptBuilt",
            task_id = %task.id,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            prompt_len = prompt.len(),
            prompt = %prompt,
            system_prompt = ?None::<String>,
            "execution request ready"
        );

        // 构建 tools 列表：从 registry 中筛选 Agent 有权限的工具（非 Deny）
        let tools: Vec<_> = registry
            .tools
            .values()
            .filter(|tool_def| {
                !matches!(
                    agent.tool_permissions.get_permission(&tool_def.name),
                    ToolPermission::Deny
                )
            })
            .cloned()
            .collect();

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: agent.id,
            request_kind: AgentRequestKind::LlmCompletion,
            prompt,
            system_prompt: None,
            tools,
            conversation: None,
            work_item_id: None,
        };

        task.mark_waiting_for_agent(agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryMetadata, EntryRole, ShortTermMemory};

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

        let prompt = build_prompt_with_context("do the task", Some(&stm), None);

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

        let prompt = build_prompt_with_context("do the task", Some(&stm), None);

        assert!(
            !prompt.contains("archived content"),
            "prompt should NOT include Archive entries, got: {}",
            prompt
        );
    }
}
