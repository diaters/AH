//! Brain 分发 System
//!
//! 使用 Brain Agent 进行任务分发决策。
//!
//! ## Brain Agent 选择规则
//!
//! 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的那个。
//! 这允许灵活配置多个 Brain Agent（如不同模型），同时保持确定性选择。

use bevy::prelude::*;
use tracing::{debug, trace};

use crate::{
    app::{Clock, HarnessSettings},
    contracts::{AgentCapabilitySummary, BrainSelectionPolicy, FirstBrainPolicy},
    domain::{
        Agent, AgentExecutionRequest, AgentExecutionRequestMessage, AgentKind, AgentRequestKind,
        AgentSpawnRequestMessage, BatchTaskState, EntryRole, LongTermMemory, ShortTermMemory,
        SubTaskBatchState, SubTaskConfig, Task, TaskStatus, WaitingReason,
    },
    llm::brain_system_prompt,
};

use super::agent_selection::select_agent_for_sub_task;

const SUB_TASK_SYSTEM_PROMPT: &str = "\
你是一个专注于完成特定子任务的 AI Agent。请仔细阅读任务描述，认真完成分配给你的工作。

重要：请在回答的最后，用 <<<RESULT>>> 和 <<</RESULT>>> 标记包围你的核心结论或最终答案。
标记内的内容应当精炼、自包含，便于其他任务引用。

示例格式：
（你的详细分析和推理过程...）

<<<RESULT>>>
你的精炼结论
<<</RESULT>>>";

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

/// Brain 分发 System
///
/// 使用 Brain Agent 进行任务分发决策。
///
/// ## Brain Agent 选择
///
/// 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的那个。
pub fn brain_dispatch_system(
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

    // 通过 Tag 查找所有带 "brain" 标签的 Agent，选择配置中最前的
    let brain_candidates: Vec<AgentCapabilitySummary> = agents
        .iter()
        .filter(|a| a.kind == AgentKind::Persistent && a.capabilities.tags.contains(&"brain".to_string()))
        .map(AgentCapabilitySummary::from_agent)
        .collect();

    let brain_policy = FirstBrainPolicy;
    let Some(brain_agent_id) = brain_policy.select_brain(&brain_candidates) else {
        debug!(
            event = "BrainAgentNotFound",
            "no brain agent found with 'brain' tag, skipping brain dispatch"
        );
        return;
    };

    let brain_agent = agents.iter().find(|a| a.id == brain_agent_id).unwrap();

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
            let child_agent = select_agent_for_sub_task(
                agents.iter().map(|a| (a, None::<&LongTermMemory>)),
                &task.content,
            );

            if let Some((agent, _ltm)) = child_agent {
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
