//! Tool 调用循环编排
//!
//! 自 `transform/llm_response.rs` 按知识域归位至此（P2 重组，纯搬家）。
//! 承载工具调用循环的编排知识：`ToolCallingState` 的查找与严格匹配协议、
//! 工具预算耗尽的合成结果生成、结果收集与 follow-up LLM 请求生成。

use crate::prelude::*;
use tracing::{debug, trace, warn};

use crate::{
    contracts::Clock,
    domain::{
        AgentExecutionOutput, AgentExecutionRequest, AgentExecutionRequestMessage,
        AgentExecutionResult, AgentId, AgentRequestKind, ConversationMessage, FailureReason,
        LlmToolCall, MessageDispatchedHookPending, OutputContent, Task, TaskId, TaskStatus,
        ToolCalledHookPending, ToolCallingState, ToolDefinition, ToolExecutionRequestMessage,
        ToolExecutionResultMessage, ToolReturnedHookPending, WaitingReason, WorkItem,
    },
    ecs::EntityIndex,
};

/// 绝对硬上限倍数：iteration 超过此值 × max_iterations 时强制失败任务
pub(crate) const HARD_LIMIT_MULTIPLIER: u32 = 2;

/// `ToolCallingState` 的快照，用于在 `llm_response_system` 内避开 Bevy 借用冲突。
///
/// 字段语义与 `ToolCallingState` 一致；`work_item_id` 区分 Task 级（None）与
/// WorkItem 级（Some）调用循环，是 `(task_id, work_item_id)` 索引键的一部分。
pub(crate) struct CallingStateInfo {
    pub(crate) entity: Entity,
    pub(crate) task_id: TaskId,
    pub(crate) iteration: u32,
    pub(crate) max_iterations: u32,
    pub(crate) conversation: Vec<ConversationMessage>,
    pub(crate) tools: Vec<ToolDefinition>,
    pub(crate) request_kind: AgentRequestKind,
    pub(crate) work_item_id: Option<uuid::Uuid>,
}

/// 在 `state_info` 中按 `(task_id, work_item_id)` 严格匹配查找 ToolCallingState。
///
/// 设计原则：WorkItem 是执行单位，Task 是组织单位。同一 Task 下不同 WorkItem
/// 的工具循环互不复用 State。这避免了"collector 残留 State 被 skill-updater
/// 误复用"类的循环 bug。
///
/// `work_item_id = None` 表示 Task 级调用，仅匹配 `work_item_id = None` 的 State；
/// `work_item_id = Some(x)` 表示 WorkItem 级调用，仅匹配 `work_item_id = Some(x)`
/// 的 State。跨 Task / 跨 WorkItem 都不会误匹配。
pub(crate) fn find_calling_state(
    state_info: &[CallingStateInfo],
    task_id: TaskId,
    work_item_id: Option<uuid::Uuid>,
) -> Option<&CallingStateInfo> {
    state_info
        .iter()
        .find(|i| i.task_id == task_id && i.work_item_id == work_item_id)
}

/// 生成合成 ToolExecutionResultMessage，用于向 LLM 传达工具预算已耗尽
///
/// 合成结果跳过 ToolCalledHookPending（不 spawn），因此不会触发 on_tool_called hook。
/// 合成结果附加 ToolReturnedHookPending，会进入 on_tool_returned hook 流水线。
pub(crate) fn spawn_synthetic_limit_result(
    commands: &mut Commands,
    task_id: TaskId,
    agent_id: AgentId,
    tool_call_id: &str,
    tool_name: &str,
    iteration: u32,
    max_iterations: u32,
) {
    let tool_output = Ok(serde_json::json!({
        "exit_code": 1,
        "status": "tool_budget_exhausted",
        "output": format!(
            "[TOOL_BUDGET_EXHAUSTED] 本轮工具调用次数已达上限 ({}/{})。请总结你目前取得的进展，并向用户说明下一步需要什么，等待用户决策是否继续。",
            iteration, max_iterations
        )
    }));

    let result = AgentExecutionResult {
        task_id,
        agent_id,
        request_kind: AgentRequestKind::ToolExecution {
            tool_name: tool_name.to_string(),
        },
        result: Ok(AgentExecutionOutput {
            content: OutputContent::Text(String::new()),
            reasoning_content: None,
        }),
        prompt: String::new(),
        system_prompt: None,
        tools: vec![],
        reasoning_content: None,
        work_item_id: None,
        conversation: None,
    };

    commands.spawn((
        ToolExecutionResultMessage {
            result,
            tool_name: tool_name.to_string(),
            tool_output,
            tool_call_id: Some(tool_call_id.to_string()),
            processed: false,
            original_tool_output: None,
        },
        ToolReturnedHookPending,
    ));
}

/// 处理 LLM 响应中的 ToolCalls 分支（自 `llm_response.rs` 迁入，纯搬家）。
///
/// 职责：更新/创建 `ToolCallingState`、执行迭代限制三段策略
/// （绝对硬上限 → WorkItem 隔离 → 普通任务预算耗尽合成结果），
/// 并为每个调用 spawn `ToolExecutionRequestMessage`。
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_tool_calls_response(
    commands: &mut Commands,
    task: &mut Task,
    state_info: &[CallingStateInfo],
    result: &AgentExecutionResult,
    calls: &[LlmToolCall],
    reasoning_content: &Option<String>,
    now: chrono::DateTime<chrono::Utc>,
    max_tool_iterations: u32,
    work_items: &Query<(Entity, &mut WorkItem)>,
) {
    // Check for existing ToolCallingState (follow-up iteration).
    // 按 (task_id, work_item_id) 严格匹配：避免误复用同 Task 下
    // 其他 WorkItem 残留的 State（如 collector 残留被 skill-updater 误用）。
    let existing = find_calling_state(state_info, task.id, result.work_item_id);

    if let Some(info) = existing {
        let new_iteration = info.iteration + 1;
        // Despawn old state and create updated one
        let mut new_conversation = info.conversation.clone();
        new_conversation.push(ConversationMessage::Assistant {
            content: None,
            tool_calls: calls.to_vec(),
            reasoning_content: reasoning_content.clone(),
        });

        if new_iteration > info.max_iterations {
            // 绝对硬上限：任何情况下都强制失败，防止无限循环
            if new_iteration > HARD_LIMIT_MULTIPLIER * info.max_iterations {
                warn!(
                    event = "ToolCallingHardLimitExceeded",
                    task_id = %task.id,
                    iteration = new_iteration,
                    max_iterations = info.max_iterations,
                    "tool calling exceeded absolute hard limit"
                );
                if info.work_item_id.is_none() {
                    task.mark_failed_reason(
                        FailureReason::AgentError,
                        format!(
                            "tool calling exceeded absolute hard limit ({}/{})",
                            new_iteration, info.max_iterations
                        ),
                        now,
                    );
                }
                commands.entity(info.entity).despawn();
                return;
            }

            if info.work_item_id.is_some() {
                // WorkItem 保持硬失败语义，但不修改原任务状态
                warn!(
                    event = "ToolCallingLimitExceeded",
                    task_id = %task.id,
                    work_item_id = ?info.work_item_id,
                    iteration = new_iteration,
                    max_iterations = info.max_iterations,
                    "work item tool calling exceeded max iterations"
                );
                commands.entity(info.entity).despawn();
                return;
            }

            // 普通任务：生成合成 tool result
            debug!(
                event = "ToolBudgetExhausted",
                task_id = %task.id,
                iteration = new_iteration,
                max_iterations = info.max_iterations,
                "tool budget exhausted, returning synthetic result"
            );

            for call in calls {
                spawn_synthetic_limit_result(
                    commands,
                    task.id,
                    result.agent_id,
                    &call.id,
                    &call.name,
                    info.iteration,
                    info.max_iterations,
                );
            }

            // 更新 ToolCallingState，记录这些 tool_call_id 正在等待合成结果
            let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
            commands.entity(info.entity).despawn();
            commands.spawn(ToolCallingState {
                task_id: task.id,
                agent_id: result.agent_id,
                pending_tool_call_ids: pending_ids,
                iteration: new_iteration,
                max_iterations: info.max_iterations,
                conversation: new_conversation,
                tools: info.tools.clone(),
                request_kind: info.request_kind.clone(),
                work_item_id: info.work_item_id,
            });

            // 不生成真实 ToolExecutionRequestMessage，避免真实工具执行
            // 将任务设为 Waiting(ToolExecution)，使 tool_calling_orchestrator_system
            // 入口检查允许此状态继续处理合成结果
            if info.work_item_id.is_none() && !task.status.is_terminal() {
                task.mark_waiting(WaitingReason::ToolExecution, now);
            }
            return;
        }

        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();

        debug!(
            event = "ToolCallingStateUpdated",
            task_id = %task.id,
            iteration = new_iteration,
            pending_count = calls.len(),
            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "tool calling state updated for follow-up iteration"
        );

        commands.entity(info.entity).despawn();
        commands.spawn(ToolCallingState {
            task_id: task.id,
            agent_id: result.agent_id,
            pending_tool_call_ids: pending_ids,
            iteration: new_iteration,
            max_iterations: info.max_iterations,
            conversation: new_conversation,
            tools: info.tools.clone(),
            request_kind: info.request_kind.clone(),
            work_item_id: info.work_item_id,
        });
    } else {
        // First iteration: create new ToolCallingState
        // 优先使用 request.conversation（结构化路径），仅在为空时构造纯文本路径
        let conversation = if result.conversation.as_ref().is_some_and(|c| !c.is_empty()) {
            // 结构化路径：使用已有的 conversation（从 STM 还原），追加本轮 Assistant
            let mut conv = result.conversation.clone().unwrap();
            conv.push(ConversationMessage::Assistant {
                content: None,
                tool_calls: calls.to_vec(),
                reasoning_content: reasoning_content.clone(),
            });
            conv
        } else {
            // 纯文本路径：现有逻辑
            let mut conversation = Vec::new();
            if let Some(sp) = &result.system_prompt {
                conversation.push(ConversationMessage::System {
                    content: sp.clone(),
                });
            }
            conversation.push(ConversationMessage::User {
                content: result.prompt.clone(),
            });
            conversation.push(ConversationMessage::Assistant {
                content: None,
                tool_calls: calls.to_vec(),
                reasoning_content: reasoning_content.clone(),
            });
            conversation
        };

        let pending_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let max_iterations = max_tool_iterations;

        debug!(
            event = "ToolCallingStateCreated",
            task_id = %task.id,
            agent_id = %result.agent_id,
            iteration = 1,
            pending_count = pending_ids.len(),
            tools = ?calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            max_iterations = max_iterations,
            "created tool calling state"
        );

        commands.spawn(ToolCallingState {
            task_id: task.id,
            agent_id: result.agent_id,
            pending_tool_call_ids: pending_ids,
            iteration: 1,
            max_iterations,
            conversation,
            tools: result.tools.clone(),
            request_kind: result.request_kind.clone(),
            work_item_id: result.work_item_id,
        });
    }

    // Spawn ToolExecutionRequestMessage for each call
    // 反查 WorkItem entity：用于将 SkillUpdateCompletedMessage 等"工具产物"
    // 直接 insert 到 WorkItem entity 上（替代用 work_item_id 反查）。
    let work_item_entity: Option<Entity> = result.work_item_id.and_then(|wid| {
        work_items
            .iter()
            .find(|(_, wi)| wi.id == wid)
            .map(|(e, _)| e)
    });
    for call in calls {
        let tool_input: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
        commands.spawn((
            ToolCalledHookPending,
            ToolExecutionRequestMessage {
                request: AgentExecutionRequest {
                    task_id: task.id,
                    agent_id: result.agent_id,
                    request_kind: AgentRequestKind::ToolExecution {
                        tool_name: call.name.clone(),
                    },
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    conversation: None,
                    work_item_id: result.work_item_id,
                    model_override: None,
                },
                tool_name: call.name.clone(),
                tool_input,
                pending_confirmation_id: None,
                tool_call_id: Some(call.id.clone()),
                pending_confirmation_options: None,
                work_item_entity,
                confirmed_once: false,
            },
        ));
    }

    // Set task to Waiting(ToolExecution) — but not for ExperienceCollection WorkItems
    // since their original task is already in terminal state
    if result.work_item_id.is_none() {
        task.mark_waiting(WaitingReason::ToolExecution, now);
        debug!(
            event = "ToolCallsReceived",
            task_id = %task.id,
            tool_count = calls.len(),
            "task waiting for tool execution"
        );
    }
}

/// Tool 调用循环协调器
///
/// 收集 Tool 执行结果，构建对话历史，生成后续 LLM 请求。
pub fn tool_calling_orchestrator_system(
    clock: Res<Clock>,
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut calling_states: Query<(Entity, &mut ToolCallingState)>,
    tool_results: Query<(Entity, &ToolExecutionResultMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (state_entity, mut state) in &mut calling_states {
        if state.pending_tool_call_ids.is_empty() {
            continue;
        }

        // 仅在任务处于”由 tool calling loop 驱动的等待态”时继续，否则跳过
        // ExperienceCollection WorkItem 的原任务可能已是终态，但仍需继续 tool calling
        let is_work_item = state.work_item_id.is_some();
        let task_is_waiting = index
            .get_task(&state.task_id)
            .and_then(|e| tasks.get(e).ok())
            .map(|t| {
                matches!(
                    t.status,
                    TaskStatus::Waiting(
                        WaitingReason::ToolExecution
                            | WaitingReason::Session { .. }
                            | WaitingReason::SubTaskBatch { .. }
                    )
                )
            })
            .unwrap_or(false);
        if !task_is_waiting && !is_work_item {
            continue;
        }
        // For WorkItem tool calls, skip if there are no tool results yet even if task isn't waiting
        if is_work_item && !task_is_waiting {
            // Check if there are any collected results for this state
            let has_results = tool_results.iter().any(|r| {
                r.1.tool_call_id
                    .as_ref()
                    .is_some_and(|id| state.pending_tool_call_ids.contains(id))
            });
            if !has_results {
                continue;
            }
        }

        // Collect matching tool results
        let mut collected: Vec<(Entity, String, String)> = Vec::new();
        let mut remaining_ids: Vec<String> = state.pending_tool_call_ids.clone();

        for (result_entity, result) in &tool_results {
            if let Some(ref call_id) = result.tool_call_id
                && remaining_ids.contains(call_id)
            {
                let content = match &result.tool_output {
                    Ok(val) => serde_json::to_string(val).unwrap_or_else(|_| val.to_string()),
                    Err(e) => format!("error: {}", e),
                };
                collected.push((result_entity, call_id.clone(), content));
                remaining_ids.retain(|id| id != call_id);
            }
        }

        // Not all results ready yet
        if !remaining_ids.is_empty() {
            trace!(
                event = "ToolCallingPending",
                task_id = %state.task_id,
                iteration = state.iteration,
                pending_count = remaining_ids.len(),
                total_count = state.pending_tool_call_ids.len(),
                "waiting for remaining tool results"
            );
            continue;
        }

        // All results collected — add to conversation
        debug!(
            event = "ToolCallingResultsCollected",
            task_id = %state.task_id,
            iteration = state.iteration,
            result_count = collected.len(),
            "all tool results collected, building follow-up request"
        );

        for (_, call_id, content) in &collected {
            state.conversation.push(ConversationMessage::Tool {
                tool_call_id: call_id.clone(),
                content: content.clone(),
            });
        }

        // Clear pending IDs (all collected)
        state.pending_tool_call_ids.clear();

        // Despawn consumed result entities
        for (entity, _, _) in &collected {
            commands.entity(*entity).despawn();
        }

        // Check iteration limit
        if state.iteration >= state.max_iterations {
            // 绝对硬上限
            if state.iteration > HARD_LIMIT_MULTIPLIER * state.max_iterations {
                warn!(
                    event = "ToolCallingHardLimitExceeded",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "tool calling exceeded absolute hard limit on result collection"
                );
                // WorkItem 不修改原任务状态
                if state.work_item_id.is_none()
                    && let Some(mut task) = index
                        .get_task(&state.task_id)
                        .and_then(|e| tasks.get_mut(e).ok())
                {
                    task.mark_failed_reason(
                        crate::domain::FailureReason::AgentError,
                        format!(
                            "tool calling exceeded absolute hard limit ({}/{})",
                            state.iteration, state.max_iterations
                        ),
                        clock.0,
                    );
                }
                commands.entity(state_entity).despawn();
                continue;
            }

            if state.work_item_id.is_some() {
                // WorkItem 保持现有隔离语义：不修改原任务状态，停止 follow-up
                warn!(
                    event = "ToolCallingLimitExceeded",
                    task_id = %state.task_id,
                    work_item_id = ?state.work_item_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "work item tool calling reached max iterations"
                );
                commands.entity(state_entity).despawn();
                continue;
            } else {
                // 普通任务：允许 LLM 再响应一次，用于总结和询问用户
                // 任务当前处于 Waiting(ToolExecution) 状态，
                // tool_calling_orchestrator_system 入口检查允许此状态继续处理
                debug!(
                    event = "ToolBudgetExhaustedAllowingSummary",
                    task_id = %state.task_id,
                    iteration = state.iteration,
                    max_iterations = state.max_iterations,
                    "tool budget exhausted, allowing LLM to summarize"
                );
            }
        }

        // 在 follow-up 中保留当前通道上下文，避免多轮 tool calling 后丢失来源信息。
        let system_prompt = index
            .get_task(&state.task_id)
            .and_then(|e| tasks.get(e).ok())
            .and_then(|task| task.origin_channel.as_ref())
            .map(|ch| ch.to_prompt_context());

        // Spawn follow-up LLM request with conversation
        let request = AgentExecutionRequest {
            task_id: state.task_id,
            agent_id: state.agent_id,
            request_kind: state.request_kind.clone(),
            prompt: String::new(),
            system_prompt,
            tools: state.tools.clone(),
            conversation: Some(state.conversation.clone()),
            work_item_id: state.work_item_id,
            model_override: None,
        };

        debug!(
            event = "ToolCallingFollowUp",
            task_id = %state.task_id,
            agent_id = %state.agent_id,
            iteration = state.iteration,
            conversation_messages = state.conversation.len(),
            tools_count = state.tools.len(),
            "spawning follow-up LLM request with tool results"
        );

        commands.spawn((
            AgentExecutionRequestMessage { request },
            MessageDispatchedHookPending,
        ));

        // Set task back to Waiting(Agent) — 但不修改 ExperienceCollection 关联的原任务状态
        if state.work_item_id.is_none()
            && let Some(mut task) = index
                .get_task(&state.task_id)
                .and_then(|e| tasks.get_mut(e).ok())
            && matches!(
                task.status,
                TaskStatus::Waiting(
                    WaitingReason::ToolExecution
                        | WaitingReason::Session { .. }
                        | WaitingReason::SubTaskBatch { .. }
                )
            )
        {
            task.mark_waiting(WaitingReason::Agent, clock.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小 CallingStateInfo 用于查找测试。
    fn make_state_info(
        task_id: crate::domain::TaskId,
        work_item_id: Option<uuid::Uuid>,
    ) -> CallingStateInfo {
        CallingStateInfo {
            entity: Entity::PLACEHOLDER,
            task_id,
            iteration: 1,
            max_iterations: 10,
            conversation: vec![],
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id,
        }
    }

    /// 同 Task 下不同 WorkItem 的 ToolCallingState 不应互相复用。
    /// 这是本次 bug 修复的核心：skill-updater 不应误用 collector 残留的 State。
    #[test]
    fn find_calling_state_strict_matches_work_item_id() {
        let task_id = crate::domain::TaskId::new();
        let collector_wi = uuid::Uuid::new_v4();
        let skill_updater_wi = uuid::Uuid::new_v4();

        // 模拟 collector 残留 State（work_item_id = collector_wi）
        // 与 skill-updater 当前响应（work_item_id = skill_updater_wi）
        let state_info = vec![
            make_state_info(task_id, Some(collector_wi)),
            make_state_info(task_id, Some(skill_updater_wi)),
        ];

        // 查找 skill-updater 的 State：必须严格匹配 work_item_id
        let found = find_calling_state(&state_info, task_id, Some(skill_updater_wi));
        assert!(
            found.is_some(),
            "should find state for matching work_item_id"
        );
        assert_eq!(
            found.unwrap().work_item_id,
            Some(skill_updater_wi),
            "must not reuse state from a different work item"
        );

        // 查找 collector 的 State：仍能找到
        let found_collector = find_calling_state(&state_info, task_id, Some(collector_wi));
        assert!(found_collector.is_some());
        assert_eq!(found_collector.unwrap().work_item_id, Some(collector_wi));

        // 查找不存在的 work_item_id：返回 None
        let other_wi = uuid::Uuid::new_v4();
        let found_other = find_calling_state(&state_info, task_id, Some(other_wi));
        assert!(
            found_other.is_none(),
            "should not return state for non-matching work_item_id"
        );
    }

    /// Task 级调用（work_item_id = None）应能找到 Task 级 State（work_item_id = None），
    /// 但不应误匹配 WorkItem 级 State。
    #[test]
    fn find_calling_state_task_level_excludes_work_item_states() {
        let task_id = crate::domain::TaskId::new();
        let work_item_id = uuid::Uuid::new_v4();

        // 混合：一个 WorkItem 级 State + 一个 Task 级 State
        let state_info = vec![
            make_state_info(task_id, Some(work_item_id)),
            make_state_info(task_id, None),
        ];

        // Task 级查找：返回 Task 级 State，不返回 WorkItem 级
        let found = find_calling_state(&state_info, task_id, None);
        assert!(found.is_some(), "should find task-level state");
        assert_eq!(
            found.unwrap().work_item_id,
            None,
            "must not return work-item state when looking for task-level state"
        );
    }

    /// 只存在 WorkItem 级 State 时，Task 级查找不应误匹配。
    /// 这防止"Task 级调用复用 WorkItem 残留 State"的反向 bug。
    #[test]
    fn find_calling_state_task_level_returns_none_if_only_work_item_states_exist() {
        let task_id = crate::domain::TaskId::new();
        let work_item_id = uuid::Uuid::new_v4();

        let state_info = vec![make_state_info(task_id, Some(work_item_id))];

        let found = find_calling_state(&state_info, task_id, None);
        assert!(
            found.is_none(),
            "must not return work-item state for task-level lookup"
        );
    }
}
