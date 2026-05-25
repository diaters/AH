use bevy::prelude::*;
use tracing::{debug, trace};

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        AgentSpawnRequestMessage, BatchTaskState, EntryRole, LongTermMemory, ShortTermMemory,
        SpaceToolRegistry, SubTaskBatchState, SubTaskConfig, Task, TaskStatus, ToolPermission,
        WaitingReason,
    },
    llm::brain_system_prompt,
};

pub(crate) fn task_dispatch_system(
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
        };

        task.mark_waiting_for_agent(agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

const SUB_TASK_SYSTEM_PROMPT: &str = "\
你是一个专注于完成特定子任务的 AI Agent。请仔细阅读任务描述，认真完成分配给你的工作。

重要：请在回答的最后，用 <<<RESULT>>> 和 <<</RESULT>>> 标记包围你的核心结论或最终答案。
标记内的内容应当精炼、自包含，便于其他任务引用。

示例格式：
（你的详细分析和推理过程...）

<<<RESULT>>>
你的精炼结论
<<</RESULT>>>";

pub(crate) fn brain_dispatch_system(
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    mut commands: Commands,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>, Option<&SubTaskConfig>)>,
    agents: Query<&Agent>,
    batch_states: Query<&SubTaskBatchState>,
) {
    let Some(brain_config) = &settings.0.brain else {
        return;
    };
    if !brain_config.enabled {
        return;
    }

    let brain_agent = agents.iter().find(|a| {
        a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string())
    });

    let Some(brain_agent) = brain_agent else {
        debug!(
            event = "BrainAgentNotFound",
            "no brain agent found, skipping brain dispatch"
        );
        return;
    };

    let all_agent_descriptions: Vec<AgentDescription> = agents
        .iter()
        .filter(|a| a.kind == AgentKind::Persistent)
        .map(|agent| AgentDescription {
            name: agent.profile.name.clone(),
            model: agent.profile.model.clone(),
            tags: agent.capabilities.tags.clone(),
            description: agent.capabilities.description.clone(),
        })
        .collect();

    for (mut task, short_term, sub_task_config) in &mut tasks {
        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 子任务处理：检查 DAG 依赖，由 Brain 分发
        if let Some(config) = sub_task_config {
            // 检查 DAG 依赖是否满足
            let deps_satisfied = if config.depends_on.is_empty() {
                true
            } else if let Some(batch_state) = batch_states
                .iter()
                .find(|bs| bs.batch_id == config.batch_id)
            {
                config.depends_on.iter().all(|dep_name| {
                    batch_state.tasks.get(dep_name).is_some_and(|s| {
                        matches!(s.state, BatchTaskState::Done | BatchTaskState::Failed)
                    })
                })
            } else {
                false
            };

            if !deps_satisfied {
                trace!(
                    event = "SubTaskWaitingForDependencies",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    depends_on = ?config.depends_on,
                    "sub-task waiting for dependencies to complete"
                );
                continue;
            }

            // 收集依赖的兄弟任务结果
            let sibling_results = if !config.depends_on.is_empty() {
                if let Some(batch_state) = batch_states
                    .iter()
                    .find(|bs| bs.batch_id == config.batch_id)
                {
                    let mut results = Vec::new();
                    for dep_name in &config.depends_on {
                        if let Some(status) = batch_state.tasks.get(dep_name) {
                            let result_text = match &status.result_summary {
                                Some(summary) if !summary.is_empty() => summary.clone(),
                                _ => format!("[{}: 执行失败，无结果]", dep_name),
                            };
                            results.push(format!("### {}\n{}", dep_name, result_text));
                        }
                    }
                    if results.is_empty() {
                        None
                    } else {
                        Some(results)
                    }
                } else {
                    None
                }
            } else {
                None
            };

            // 选择匹配的 Agent（基于所需工具标签）
            let child_agent = agents
                .iter()
                .filter(|a| a.kind == AgentKind::Persistent)
                .filter(|a| !a.capabilities.tags.contains(&"brain".to_string()))
                .max_by_key(|a| {
                    a.capabilities
                        .tags
                        .iter()
                        .filter(|t| config.allowed_tools.contains(t))
                        .count()
                });

            if let Some(agent) = child_agent {
                debug!(
                    event = "SubTaskDispatched",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    selected_agent = %agent.profile.name,
                    batch_id = %config.batch_id,
                    "dispatching sub-task to agent"
                );

                let child_task_id = task.id;

                commands.spawn(AgentSpawnRequestMessage {
                    parent_agent_id: config.parent_agent_id,
                    task_id: child_task_id,
                    name: config.child_agent_name.clone(),
                    model: config.child_agent_model.clone(),
                    description: config.child_agent_name.clone(),
                    tools: config.allowed_tools.clone(),
                    task_prompt: if let Some(ref results) = sibling_results {
                        format!(
                            "{}\n\n## 兄弟任务结果\n\n{}\n\n请基于以上兄弟任务的结果完成你的任务。你可以直接引用这些结果，无需重新计算或搜索。",
                            task.content,
                            results.join("\n\n")
                        )
                    } else {
                        task.content.clone()
                    },
                    task_system_prompt: Some(SUB_TASK_SYSTEM_PROMPT.to_string()),
                });

                if sibling_results.is_some() {
                    debug!(
                        event = "SiblingResultsInjected",
                        task_id = %task.id,
                        child_name = %config.child_agent_name,
                        depends_on = ?config.depends_on,
                        "injected sibling task results into sub-task prompt"
                    );
                }

                task.status = TaskStatus::Waiting(WaitingReason::Agent);
                task.delegate = Some(agent.id);
                task.updated_at = clock.0;
            } else {
                debug!(
                    event = "SubTaskNoAgentAvailable",
                    task_id = %task.id,
                    child_name = %config.child_agent_name,
                    "no suitable agent found for sub-task"
                );
            }
            continue;
        }

        // 构建带历史对话的 prompt
        let prompt_with_history = build_prompt_with_history(&task.content, short_term);
        let prompt =
            brain_user_prompt_from_descriptions(&prompt_with_history, &all_agent_descriptions);

        let stm_entries = short_term.map(|s| s.entries.len()).unwrap_or(0);
        let stm_tokens = short_term.map(|s| s.estimated_tokens).unwrap_or(0);

        debug!(
            event = "BrainDispatch",
            task_id = %task.id,
            task_content = %task.content,
            task_status = ?task.status,
            brain_agent_id = %brain_agent.id,
            brain_agent_name = %brain_agent.profile.name,
            prompt_len = prompt.len(),
            stm_entries = stm_entries,
            stm_tokens = stm_tokens,
            available_agents = ?all_agent_descriptions.iter().map(|a| &a.name).collect::<Vec<_>>(),
            "brain dispatching task"
        );

        let request = AgentExecutionRequest {
            task_id: task.id,
            agent_id: brain_agent.id,
            request_kind: AgentRequestKind::BrainDecision,
            prompt,
            system_prompt: Some(brain_system_prompt()),
            tools: vec![],
            conversation: None,
        };

        task.mark_waiting_for_agent(brain_agent.id, clock.0);
        commands.spawn(AgentExecutionRequestMessage { request });
    }
}

struct AgentDescription {
    name: String,
    model: String,
    tags: Vec<String>,
    description: String,
}

fn brain_user_prompt_from_descriptions(task_content: &str, agents: &[AgentDescription]) -> String {
    let agent_descriptions: Vec<String> = agents
        .iter()
        .filter(|agent| !agent.tags.contains(&"brain".to_string()))
        .map(|agent| {
            format!(
                "- name: \"{}\"\n  model: \"{}\"\n  tags: {:?}\n  description: \"{}\"",
                agent.name, agent.model, agent.tags, agent.description,
            )
        })
        .collect();

    format!(
        r#"Task content: "{}"

Available agents:
{}

Select the best agent for this task and provide a delegate prompt."#,
        task_content,
        agent_descriptions.join("\n"),
    )
}

fn select_agent_with_memory<'a>(
    agents: impl Iterator<Item = (&'a Agent, Option<&'a LongTermMemory>)>,
    task_content: &str,
) -> Option<(&'a Agent, Option<&'a LongTermMemory>)> {
    let candidates: Vec<_> = agents
        .filter(|(a, _)| a.kind == AgentKind::Persistent)
        .filter(|(a, _)| !a.capabilities.tags.contains(&"brain".to_string()))
        .collect();

    let selected = candidates
        .iter()
        .max_by_key(|(a, _)| match_score(a, task_content));

    if let Some((agent, _ltm)) = selected {
        let score = match_score(agent, task_content);
        let all_scores: Vec<_> = candidates
            .iter()
            .map(|(a, _)| (a.profile.name.clone(), match_score(a, task_content)))
            .collect();
        debug!(
            event = "AgentScoring",
            selected_agent = %agent.profile.name,
            selected_score = score,
            all_candidates_scores = ?all_scores,
            task_content_preview = %task_content.chars().take(100).collect::<String>(),
            "agent scoring completed"
        );
    }

    selected.copied()
}

fn match_score(agent: &Agent, task_content: &str) -> usize {
    let lower = task_content.to_lowercase();
    agent
        .capabilities
        .tags
        .iter()
        .filter(|tag| lower.contains(&tag.to_lowercase()))
        .count()
}

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
                EntryRole::User => "User",
                EntryRole::Assistant => "Assistant",
                _ => continue,
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

/// 构建带历史对话的 prompt（Brain Agent 使用）
fn build_prompt_with_history(task_content: &str, short_term: Option<&ShortTermMemory>) -> String {
    let Some(stm) = short_term else {
        return task_content.to_string();
    };

    if stm.entries.is_empty() {
        return task_content.to_string();
    }

    // 构建历史对话
    let mut history = String::new();

    // 添加摘要前缀（如果有）
    if let Some(summary) = &stm.summary_prefix {
        history.push_str(&format!("[Previous context summary]\n{}\n\n", summary));
    }

    // 添加对话历史
    history.push_str("[Conversation history]\n");
    for entry in &stm.entries {
        let role = match entry.role {
            EntryRole::User => "User",
            EntryRole::Assistant => "Assistant",
            _ => continue,
        };
        history.push_str(&format!("{}: {}\n", role, entry.content));
    }

    // 组合成完整 prompt
    format!(
        "{}\n\n[Current request]\n{}",
        history.trim_end(),
        task_content
    )
}
