//! 等待任务 System
//!
//! 处理 wait_tasks 工具和 shell.wait 的等待逻辑。

use bevy::prelude::*;

use crate::{
    app::Clock,
    contracts::SessionBackend,
    domain::{
        AgentExecutionOutput, AgentExecutionResult, AgentRequestKind, OutputContent,
        SubTaskCompletedMessage, Task, ToolExecutionResultMessage, WaitingForSessionInfo,
        WaitingForTasksInfo,
    },
};

use super::orchestrator::{collect_task_results, spawn_wait_result_message};

/// 子任务完成时检查是否有任务在等待（事件驱动优化）
pub fn on_subtask_completed_check_waiting(
    messages: Query<(Entity, &SubTaskCompletedMessage)>,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    all_tasks: Query<&Task>,
    mut commands: Commands,
) {
    for (_msg_entity, msg) in &messages {
        // 检查是否有任务在等待这个完成的子任务
        for (entity, task, info) in &waiting_tasks {
            if info.target_task_ids.contains(&msg.child_task_id) {
                // 检查是否所有目标都完成
                let all_terminal = info.target_task_ids.iter().all(|id| {
                    all_tasks
                        .iter()
                        .any(|t| t.id == *id && t.status.is_terminal())
                });

                if all_terminal {
                    let results = collect_task_results(&info.target_task_ids, &all_tasks);
                    spawn_wait_result_message(&mut commands, task.id, info, results, false);
                    commands.entity(entity).remove::<WaitingForTasksInfo>();
                }
            }
        }
    }
}

/// 轮询检查等待中的任务（超时兜底）
pub fn check_waiting_tasks_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForTasksInfo)>,
    all_tasks: Query<&Task>,
) {
    for (entity, task, info) in &waiting_tasks {
        let timed_out = clock.0 >= info.timeout_at;

        // 检查所有目标任务是否都已终态
        let all_terminal = info.target_task_ids.iter().all(|id| {
            all_tasks
                .iter()
                .any(|t| t.id == *id && t.status.is_terminal())
        });

        if timed_out || all_terminal {
            let results = collect_task_results(&info.target_task_ids, &all_tasks);
            spawn_wait_result_message(&mut commands, task.id, info, results, timed_out);

            // 移除等待信息组件
            commands.entity(entity).remove::<WaitingForTasksInfo>();
        }
    }
}

/// 轮询检查等待中的 shell 会话（超时或完成时返回结果）
pub fn check_waiting_sessions_system(
    clock: Res<Clock>,
    mut commands: Commands,
    waiting_tasks: Query<(Entity, &Task, &WaitingForSessionInfo)>,
    backend: Res<crate::systems::tools::backend::NativeProcessBackend>,
) {
    for (entity, task, info) in &waiting_tasks {
        let timed_out = clock.0 >= info.timeout_at;
        let handle = backend
            .wait_session(crate::domain::SessionWaitRequest {
                handle_id: info.handle_id,
                timeout_secs: 0,
                tail_lines: info.return_tail_lines,
            })
            .ok()
            .flatten();

        if timed_out || handle.is_some() {
            commands.spawn(ToolExecutionResultMessage {
                result: AgentExecutionResult {
                    task_id: task.id,
                    agent_id: info.agent_id,
                    request_kind: AgentRequestKind::LlmCompletion,
                    result: Ok(AgentExecutionOutput {
                        content: OutputContent::Text("shell_wait completed".to_string()),
                        reasoning_content: None,
                    }),
                    prompt: String::new(),
                    system_prompt: None,
                    tools: vec![],
                    reasoning_content: None,
                    work_item_id: None,
                },
                tool_name: "shell_wait".to_string(),
                tool_output: Ok(match handle {
                    Some(handle) => serde_json::json!(handle),
                    None => serde_json::json!({
                        "handle_id": info.handle_id.to_string(),
                        "status": "running",
                        "timed_out": true
                    }),
                }),
                tool_call_id: Some(info.tool_call_id.clone()),
                processed: false,
            });

            commands.entity(entity).remove::<WaitingForSessionInfo>();
        }
    }
}
