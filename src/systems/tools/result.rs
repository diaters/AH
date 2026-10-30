//! Tool 结果处理 System
//!
//! 处理 Tool 执行结果，记录 ToolCall，恢复原 Task。

use crate::prelude::*;
use tracing::{debug, warn};

use crate::{
    contracts::Clock,
    domain::{ShortTermMemory, Task, ToolCallingState, ToolExecutionResultMessage, TrustedImages},
    systems::HarnessSettings,
};

/// 结果中的图片标记可被信任的工具（会校验本地图片后返回标记）。
const VIEW_IMAGE_TOOL_NAME: &str = "view_image";

/// Tool 结果处理 System
///
/// 处理 Tool 执行结果，记录 ToolCall，恢复原 Task。
/// 当 ToolCallingState 存在时保留 ToolExecutionResultMessage，由 orchestrator 清理。
#[allow(clippy::too_many_arguments)]
pub fn tool_result_system(
    mut commands: Commands,
    clock: Res<Clock>,
    mut results: Query<(Entity, &mut ToolExecutionResultMessage)>,
    mut tasks: Query<(
        Entity,
        &Task,
        Option<&mut ShortTermMemory>,
        Option<&crate::domain::ScheduledRun>,
    )>,
    calling_states: Query<&ToolCallingState>,
    _settings: Res<HarnessSettings>,
    registry: Option<Res<crate::contracts::FrontendRegistry>>,
    agents: Query<&crate::domain::Agent>,
) {
    for (entity, mut result) in &mut results {
        if result.processed {
            continue;
        }

        // 查找对应的 Task 及其 ShortTermMemory
        let mut found_task = false;
        for (task_entity, task, mut short_term_memory, scheduled_run) in &mut tasks {
            if task.id != result.result.task_id {
                continue;
            }
            found_task = true;
            if let Some(registry) = registry
                .as_deref()
                .filter(|r| r.has_frontend(crate::domain::FrontendKind::Client))
                && let Some(channel) = task
                    .routing_policy
                    .output_channel()
                    .filter(|c| c.frontend == crate::domain::FrontendKind::Client)
                && result.result.work_item_id.is_none()
            {
                let output = match &result.tool_output {
                    Ok(value) => value.to_string(),
                    Err(error) => error.to_string(),
                };
                let truncated = output.chars().count() > 12800;
                let event = crate::domain::EngineEvent::ToolCallCompleted {
                    target: crate::domain::EventTarget::Directed(vec![channel.clone()]),
                    task_id: task.id,
                    call_id: result.tool_call_id.clone(),
                    agent_name: agents
                        .iter()
                        .find(|a| a.id == result.result.agent_id)
                        .map(|a| a.profile.name.clone())
                        .unwrap_or_else(|| result.result.agent_id.to_string()),
                    tool_name: result.tool_name.clone(),
                    success: result.tool_output.is_ok(),
                    output: output.chars().take(12800).collect(),
                    truncated,
                    late_result: task.status().is_terminal(),
                };
                for frontend in registry
                    .frontends
                    .iter()
                    .filter(|f| f.kind() == crate::domain::FrontendKind::Client)
                {
                    frontend.push_event(event.clone());
                }
            }

            if result.result.work_item_id.is_none()
                && let Some(stm) = short_term_memory.as_mut()
                && let Some(id) = result.tool_call_id.as_deref()
            {
                let content = match &result.tool_output {
                    Ok(output) => serde_json::to_string(output).unwrap_or_default(),
                    Err(error) => format!("Error: {error}"),
                };
                stm.protocol.record_tool_result(id, content);
            }

            match &result.tool_output {
                Ok(output) => {
                    let output_str =
                        serde_json::to_string(output).unwrap_or_else(|_| output.to_string());
                    debug!(
                        event = "ToolExecuted",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        tool_returned = true,
                        output = %output_str,
                        output_len = output_str.len(),
                        "tool execution completed"
                    );

                    // 若原始输出被插件替换，记录审计日志。
                    if let Some(ref original) = result.original_tool_output {
                        let original_str = serde_json::to_string(original)
                            .unwrap_or_else(|_| original.to_string());
                        warn!(
                            event = "ToolOutputReplacedByPlugin",
                            tool_name = %result.tool_name,
                            tool_call_id = %result.tool_call_id.as_deref().unwrap_or(""),
                            task_id = %task.id,
                            original_output = %original_str,
                            "tool output was replaced by on_tool_returned hook plugin"
                        );
                    }

                    // view_image 已校验并登记本地图片；插件替换过的输出不可信，不登记
                    if result.tool_name == VIEW_IMAGE_TOOL_NAME
                        && result.original_tool_output.is_none()
                    {
                        TrustedImages::register(&mut commands, task_entity, &output_str);
                    }

                    // Only persist tool_output here.
                    // The original tool_input payload is intentionally not written into STM,
                    // which avoids leaking raw shell_input request text into memory.
                    if let Some(stm) = short_term_memory.as_mut() {
                        stm.record_tool_call(
                            result.tool_call_id.clone(),
                            result.tool_name.clone(),
                            String::new(),
                            output_str,
                            clock.0,
                        );
                    }
                }
                Err(e) => {
                    if scheduled_run.is_some()
                        && result.result.work_item_id.is_none()
                        && matches!(e, crate::domain::ToolError::PermissionDenied(_))
                    {
                        commands
                            .entity(task_entity)
                            .insert(crate::domain::ScheduledRunFailure(format!(
                                "{}: {e}",
                                result.tool_name
                            )));
                    }
                    warn!(
                        event = "ToolExecutionFailed",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        tool_returned = false,
                        error = %e,
                        "tool execution failed"
                    );
                }
            }
            if let Some(stm) = short_term_memory.as_mut() {
                stm.estimated_tokens = stm.estimated_tokens.max(stm.protocol.estimated_tokens());
            }
            break;
        }

        if !found_task {
            warn!(
                event = "ToolResultTaskNotFound",
                task_id = %result.result.task_id,
                tool_name = %result.tool_name,
                "tool result has no matching task"
            );
        }

        // Mark as processed to prevent re-handling on subsequent frames
        result.processed = true;

        // Only despawn if no ToolCallingState is tracking this result
        let should_keep = result.tool_call_id.as_ref().is_some_and(|call_id| {
            calling_states
                .iter()
                .any(|s| s.pending_tool_call_ids.contains(call_id))
        });
        if !should_keep {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentExecutionResult, AgentId, AgentRequestKind, ChannelId, FrontendKind, ToolError,
    };

    fn result_message(
        task: &Task,
        tool_name: &str,
        output: Result<serde_json::Value, ToolError>,
    ) -> ToolExecutionResultMessage {
        ToolExecutionResultMessage {
            result: AgentExecutionResult {
                task_id: task.id,
                agent_id: AgentId::nil(),
                request_kind: AgentRequestKind::LlmCompletion,
                result: Err(crate::domain::ExecutionError::Unknown(String::new())),
                prompt: String::new(),
                system_prompt: None,
                tools: vec![],
                reasoning_content: None,
                work_item_id: None,
                conversation: None,
                turn_generation: None,
            },
            tool_name: tool_name.to_string(),
            tool_output: output,
            tool_call_id: None,
            processed: false,
            original_tool_output: None,
        }
    }

    /// 只有 view_image 的结果登记为可信图片；其他工具输出里的标记（如 shell 输出）不登记。
    #[test]
    fn only_view_image_results_register_trusted_images() {
        let mut app = App::new();
        app.insert_resource(Clock::default());
        app.insert_resource(HarnessSettings::default_test());
        app.add_systems(Update, tool_result_system);
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "local".to_string(),
            thread_id: None,
        };
        let task = Task::from_user_input("goal", 3, channel);
        app.world_mut().spawn(result_message(
            &task,
            "view_image",
            Ok(serde_json::json!({"image": "[IMAGE:/tmp/ok.png]"})),
        ));
        app.world_mut().spawn(result_message(
            &task,
            "shell_exec",
            Ok(serde_json::json!({"output": "[IMAGE:/etc/secret.png]"})),
        ));
        let entity = app
            .world_mut()
            .spawn((task, ShortTermMemory::default()))
            .id();

        app.update();

        let trusted = app
            .world()
            .get::<TrustedImages>(entity)
            .expect("registered");
        assert_eq!(
            trusted.0.iter().cloned().collect::<Vec<_>>(),
            vec!["/tmp/ok.png".to_string()]
        );
    }
    #[test]
    fn native_observation_keeps_routing_bounds_and_errors() {
        use crate::domain::{EngineEvent, EventTarget, Frontend, UserAction};
        use std::sync::{Arc, Mutex};
        struct Capture(Arc<Mutex<Vec<EngineEvent>>>);
        impl Frontend for Capture {
            fn kind(&self) -> FrontendKind {
                FrontendKind::Client
            }
            fn poll_actions(&self) -> Vec<UserAction> {
                vec![]
            }
            fn push_event(&self, event: EngineEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let events = Arc::new(Mutex::new(vec![]));
        let mut app = App::new();
        app.init_resource::<Clock>();
        app.insert_resource(HarnessSettings::default_test());
        app.insert_resource(crate::contracts::FrontendRegistry {
            frontends: vec![Box::new(Capture(events.clone()))],
        });
        app.add_systems(Update, tool_result_system);
        let channel = ChannelId {
            frontend: FrontendKind::Client,
            user_id: "alice".into(),
            thread_id: Some("owned-session".into()),
        };
        let task = Task::from_user_input("goal", 3, channel.clone());
        let task_id = task.id;
        app.world_mut().spawn(result_message(
            &task,
            "shell_read",
            Ok(serde_json::json!("界".repeat(13000))),
        ));
        app.world_mut().spawn(result_message(
            &task,
            "shell_exec",
            Err(ToolError::PermissionDenied("blocked".into())),
        ));
        app.world_mut().spawn(task);
        let local = Task::from_user_input(
            "private",
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "local".into(),
                thread_id: None,
            },
        );
        app.world_mut().spawn(result_message(
            &local,
            "shell_exec",
            Ok(serde_json::json!("secret")),
        ));
        app.world_mut().spawn(local);
        app.update();
        app.update();
        let captured = events.lock().unwrap();
        assert_eq!(
            captured.len(),
            2,
            "one event per result; local results stay local"
        );
        for event in captured.iter() {
            let EngineEvent::ToolCallCompleted {
                target,
                task_id: id,
                success,
                output,
                truncated,
                ..
            } = event
            else {
                panic!("unexpected event")
            };
            assert_eq!(*id, task_id);
            assert!(
                matches!(target,EventTarget::Directed(channels) if channels==&vec![channel.clone()])
            );
            if *success {
                assert!(*truncated);
                assert_eq!(output.chars().count(), 12800);
            } else {
                assert!(output.contains("blocked"));
                assert!(!truncated);
            }
        }
    }
}
