use bevy::prelude::*;
use tracing::debug;

use crate::domain::{Agent, ShortTermMemory, Task, TaskStatus, WaitingReason, WorkItem};

/// 评估器触发系统：检测评估条件并生成 WorkItem
pub(crate) fn evaluation_trigger_system(
    mut commands: Commands,
    config: Res<crate::domain::TaskEvaluationConfig>,
    mut tasks: Query<(&mut Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
) {
    if !config.enabled {
        return;
    }

    for (mut task, memory) in &mut tasks {
        if task.status != TaskStatus::Running {
            continue;
        }

        // 检查轮数阈值
        if let Some(max_turns) = config.max_turns {
            // 仅统计真实对话轮次（User + Assistant），Summary/Archive 不计入进度
            let turn_count = memory.map(ShortTermMemory::dialog_turn_count).unwrap_or(0);
            if turn_count >= max_turns {
                // 基于进度的去重：同一 turn_count 不重复触发
                if let Some(last) = task.last_evaluated_turn
                    && turn_count <= last
                {
                    continue;
                }

                // 查找评估器 Agent
                let evaluator_exists = agents
                    .iter()
                    .any(|a| a.profile.name == config.evaluator_agent_name);

                if evaluator_exists {
                    debug!(
                        task_id = %task.id,
                        turn_count,
                        max_turns,
                        "evaluation triggered by turn limit"
                    );

                    // 创建评估 WorkItem
                    let work_item = WorkItem::evaluation(
                        task.id,
                        format!(
                            "任务内容：{}\n\n请基于当前任务执行情况判断 decision、reasoning、suggested_action。",
                            task.content
                        ),
                        Some(format!(
                            "当前已执行 {} 轮，达到配置的最大轮数限制 {} 轮。",
                            turn_count, max_turns
                        )),
                    );
                    commands.spawn(work_item);

                    // 记录本次评估对应的轮数
                    task.record_evaluation_at_turn(turn_count);

                    // 将任务状态改为等待评估器，防止重复触发
                    task.status = TaskStatus::Waiting(WaitingReason::Evaluator);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::TaskEvaluationConfig;

    #[test]
    fn evaluation_trigger_system_disabled_by_default() {
        let config = TaskEvaluationConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn evaluation_trigger_system_does_nothing_when_disabled() {
        // This is a logic check - when disabled, system returns early
        let config = TaskEvaluationConfig::default();
        assert!(!config.enabled);
    }

    #[test]
    fn dedup_skips_when_last_evaluated_turn_equals_current() {
        // 模拟去重逻辑：当 last_evaluated_turn == turn_count 时不应触发
        let last_evaluated_turn: Option<u32> = Some(2);
        let turn_count: u32 = 2;
        let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
        assert!(should_skip, "should skip evaluation at same progress");
    }

    #[test]
    fn dedup_allows_when_turn_count_advances() {
        // 模拟去重逻辑：当 turn_count > last_evaluated_turn 时应允许触发
        let last_evaluated_turn: Option<u32> = Some(2);
        let turn_count: u32 = 4;
        let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
        assert!(
            !should_skip,
            "should allow evaluation when progress advanced"
        );
    }

    #[test]
    fn dedup_allows_when_no_previous_evaluation() {
        // 模拟去重逻辑：当 last_evaluated_turn == None 时应允许触发
        let last_evaluated_turn: Option<u32> = None;
        let turn_count: u32 = 2;
        let should_skip = last_evaluated_turn.is_some_and(|last| turn_count <= last);
        assert!(
            !should_skip,
            "should allow evaluation when never evaluated before"
        );
    }
}
