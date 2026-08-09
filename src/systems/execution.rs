use crate::prelude::*;
use tracing::{debug, info, warn};

use crate::domain::{AgentExecutionRequestMessage, AgentExecutionResult, AgentRequestKind, Task};
use crate::{
    app::{AsyncRuntime, Clock, ExecutionResultSender, ModelChainStateUpdateSender},
    llm::ExecutorRegistry,
};

/// 消费执行请求并把任务提交给异步运行时。
#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_execution_system(
    clock: Res<Clock>,
    runtime: Res<AsyncRuntime>,
    executor_registry: Res<ExecutorRegistry>,
    result_sender: Res<ExecutionResultSender>,
    state_update_sender: Res<ModelChainStateUpdateSender>,
    mut commands: Commands,
    requests: Query<(Entity, &AgentExecutionRequestMessage)>,
    mut tasks: Query<&mut Task>,
    agents: Query<(
        Entity,
        &crate::domain::Agent,
        Option<&crate::domain::ModelChainState>,
    )>,
) {
    for (entity, message) in &requests {
        let request = message.request.clone();
        let registry = executor_registry.clone();
        let sender = result_sender.0.clone();
        let state_sender = state_update_sender.0.clone();

        for mut task in &mut tasks {
            if task.id == request.task_id {
                // 只有 LlmCompletion 请求才标记任务为 Running
                // BrainDecision 和 Summarization 不改变任务状态
                if request.request_kind == AgentRequestKind::LlmCompletion {
                    // 只有非终态任务才标记为 Running
                    if !task.status.is_terminal() {
                        debug!(
                            event = "TaskMarkedRunning",
                            task_id = %task.id,
                            old_status = ?task.status,
                            "marking task as Running"
                        );
                        task.mark_running(clock.0);
                    }
                }
                break;
            }
        }

        // 查找 Agent 的 ModelChainState
        let chain_snapshot = agents
            .iter()
            .find(|(_, agent, _)| agent.id == request.agent_id)
            .and_then(|(_, _, state)| state.cloned());

        debug!(
            event = "ExecutionSubmitted",
            task_id = %request.task_id,
            agent_id = %request.agent_id,
            request_kind = ?request.request_kind,
            prompt_len = request.prompt.len(),
            has_system_prompt = request.system_prompt.is_some(),
            has_model_chain = chain_snapshot.is_some(),
            "submitting execution request to async runtime"
        );

        runtime.0.spawn(async move {
            let result = if let Some(mut chain) = chain_snapshot {
                // 有 ModelChainState：执行带降级的请求
                execute_with_fallback_logic(
                    &registry,
                    &mut chain,
                    request.clone(),
                    state_sender.clone(),
                )
                .await
            } else {
                // 无 ModelChainState：使用默认 executor（向后兼容）
                let default_executor = registry
                    .get("default")
                    .or_else(|| registry.executors.values().next().cloned())
                    .expect("no executor available");

                default_executor.execute(request.clone()).await
            };

            let reasoning_content = result
                .as_ref()
                .ok()
                .and_then(|o| o.reasoning_content.clone());
            let _ = sender.send(AgentExecutionResult {
                task_id: request.task_id,
                agent_id: request.agent_id,
                request_kind: request.request_kind,
                result,
                prompt: request.prompt.clone(),
                system_prompt: request.system_prompt.clone(),
                tools: request.tools.clone(),
                reasoning_content,
                work_item_id: request.work_item_id,
                conversation: request.conversation.clone(),
            });
        });

        commands.entity(entity).despawn();
    }
}

/// 执行带降级逻辑的请求
async fn execute_with_fallback_logic(
    registry: &ExecutorRegistry,
    chain_state: &mut crate::domain::ModelChainState,
    mut request: crate::domain::AgentExecutionRequest,
    state_sender: tokio::sync::mpsc::UnboundedSender<crate::domain::ModelChainStateUpdate>,
) -> Result<crate::domain::AgentExecutionOutput, crate::domain::ExecutionError> {
    let original_index = chain_state.active_index;

    loop {
        // 获取当前优先级的 provider 和 model
        let entry = chain_state.current_entry();
        let executor = registry.get(&entry.provider).ok_or_else(|| {
            crate::domain::ExecutionError::Unknown(format!(
                "provider '{}' not found",
                entry.provider
            ))
        })?;

        // 设置 model_override
        request.model_override = Some(entry.model.clone());

        // 执行
        let result = executor.execute(request.clone()).await;

        match result {
            Ok(output) => {
                // 成功，若发生过降级，发送状态更新
                if chain_state.active_index != original_index {
                    let _ = state_sender.send(crate::domain::ModelChainStateUpdate {
                        agent_id: request.agent_id,
                        new_active_index: chain_state.active_index,
                        cooldown_until: chain_state.cooldown_until,
                        previous_model: chain_state.chain[original_index].model.clone(),
                        new_model: entry.model.clone(),
                    });
                }
                return Ok(output);
            }
            Err(error) => {
                // 检查是否应降级
                if error.is_fallback_eligible() {
                    let cooldown_secs = chain_state
                        .current_entry()
                        .fallback_cooldown_secs
                        .unwrap_or_else(|| registry.default_cooldown_secs());

                    if chain_state.step_fallback(cooldown_secs) {
                        // 降级成功，继续循环
                        warn!(
                            event = "ModelFallback",
                            agent_id = %request.agent_id,
                            to_provider = %chain_state.current_provider(),
                            to_model = %chain_state.current_model(),
                            cooldown_secs,
                            "falling back to next model"
                        );
                        continue;
                    }
                }

                // 所有优先级耗尽或错误不可降级
                warn!(
                    event = "ModelChainExhausted",
                    agent_id = %request.agent_id,
                    provider = %chain_state.current_provider(),
                    model = %chain_state.current_model(),
                    error = %error,
                    "model chain exhausted"
                );
                return Err(error);
            }
        }
    }
}

/// 处理 ModelChainState 状态更新消息
pub(crate) fn model_chain_state_update_system(
    mut state_rx: ResMut<crate::app::ModelChainStateUpdateReceiver>,
    mut agents: Query<(&crate::domain::Agent, &mut crate::domain::ModelChainState)>,
) {
    while let Ok(update) = state_rx.0.try_recv() {
        for (agent, mut state) in &mut agents {
            if agent.id == update.agent_id {
                state.active_index = update.new_active_index;
                state.cooldown_until = update.cooldown_until;

                info!(
                    event = "ModelChainStateUpdated",
                    agent_id = %update.agent_id,
                    active_index = update.new_active_index,
                    new_model = %update.new_model,
                    "model chain state updated"
                );
            }
        }
    }
}
