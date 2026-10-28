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
pub fn tool_result_system(
    mut commands: Commands,
    clock: Res<Clock>,
    mut results: Query<(Entity, &mut ToolExecutionResultMessage)>,
    mut tasks: Query<(Entity, &Task, Option<&mut ShortTermMemory>)>,
    calling_states: Query<&ToolCallingState>,
    _settings: Res<HarnessSettings>,
) {
    for (entity, mut result) in &mut results {
        if result.processed {
            continue;
        }

        // 查找对应的 Task 及其 ShortTermMemory
        let mut found_task = false;
        for (task_entity, task, short_term_memory) in &mut tasks {
            if task.id != result.result.task_id {
                continue;
            }
            found_task = true;

            match &result.tool_output {
                Ok(output) => {
                    let output_str =
                        serde_json::to_string(output).unwrap_or_else(|_| output.to_string());
                    debug!(
                        event = "ToolExecuted",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        success = true,
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
                    if let Some(mut stm) = short_term_memory {
                        stm.record_tool_call(
                            result.tool_call_id.clone(),
                            result.tool_name.clone(),
                            serde_json::to_string(output).unwrap_or_default(),
                            output_str,
                            clock.0,
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        event = "ToolExecutionFailed",
                        tool_name = %result.tool_name,
                        task_id = %task.id,
                        agent_id = %result.result.agent_id,
                        success = false,
                        error = %e,
                        "tool execution failed"
                    );
                }
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

        let trusted = app.world().get::<TrustedImages>(entity).expect("registered");
        assert_eq!(
            trusted.0.iter().cloned().collect::<Vec<_>>(),
            vec!["/tmp/ok.png".to_string()]
        );
    }
}

