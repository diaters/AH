//! Tool 结果处理 System
//!
//! 处理 Tool 执行结果，记录 ToolCall，恢复原 Task。

use bevy::prelude::*;
use tracing::{debug, warn};

use crate::{
    app::{Clock, HarnessSettings},
    domain::{
        ShortTermMemory, Task, ToolCallingState, ToolExecutionResultMessage,
    },
};

/// Tool 结果处理 System
///
/// 处理 Tool 执行结果，记录 ToolCall，恢复原 Task。
/// 当 ToolCallingState 存在时保留 ToolExecutionResultMessage，由 orchestrator 清理。
pub fn tool_result_system(
    mut commands: Commands,
    clock: Res<Clock>,
    mut results: Query<(Entity, &mut ToolExecutionResultMessage)>,
    mut tasks: Query<(&Task, Option<&mut ShortTermMemory>)>,
    calling_states: Query<&ToolCallingState>,
    _settings: Res<HarnessSettings>,
) {
    for (entity, mut result) in &mut results {
        if result.processed {
            continue;
        }

        // 查找对应的 Task 及其 ShortTermMemory
        let mut found_task = false;
        for (task, short_term_memory) in &mut tasks {
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

                    // 记录 ToolCall 到 ShortTermMemory
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
