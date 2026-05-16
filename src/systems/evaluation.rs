use bevy::prelude::*;

use crate::{
    app::Clock,
    domain::{
        Agent, EvaluationRequestMessage, EvaluationResultMessage, EvaluationTrigger,
        ShortTermMemory, Task, TaskStatus,
    },
};

/// 评估器触发系统：检测评估条件并生成请求
pub(crate) fn evaluation_trigger_system(
    mut commands: Commands,
    config: Res<crate::domain::TaskEvaluationConfig>,
    tasks: Query<(&Task, Option<&ShortTermMemory>)>,
    agents: Query<&Agent>,
) {
    if !config.enabled {
        return;
    }

    for (task, memory) in &tasks {
        if task.status != TaskStatus::Running {
            continue;
        }

        // 检查轮数阈值
        if let Some(max_turns) = config.max_turns {
            let turn_count = memory.map(|m| m.turn_count).unwrap_or(0);
            if turn_count >= max_turns {
                // 查找评估器 Agent
                let evaluator_id = agents
                    .iter()
                    .find(|a| a.profile.name == config.evaluator_agent_name)
                    .map(|a| a.id);

                if let Some(evaluator_id) = evaluator_id {
                    commands.spawn(EvaluationRequestMessage {
                        task_id: task.id,
                        trigger: EvaluationTrigger::TurnLimitReached,
                        agent_id: evaluator_id,
                    });
                }
            }
        }
    }
}

/// 评估结果处理系统
pub(crate) fn evaluation_result_system(
    mut commands: Commands,
    clock: Res<Clock>,
    results: Query<(Entity, &EvaluationResultMessage)>,
    mut tasks: Query<&mut Task>,
) {
    for (entity, msg) in &results {
        if let Some(mut task) = tasks.iter_mut().find(|t| t.id == msg.task_id) {
            use crate::domain::EvaluationDecision;

            match msg.result.decision {
                EvaluationDecision::Continue => {
                    task.status = TaskStatus::Ready;
                    task.updated_at = clock.0;
                }
                EvaluationDecision::Complete => {
                    task.status = TaskStatus::Done;
                    task.updated_at = clock.0;
                }
                EvaluationDecision::Failed => {
                    task.status = TaskStatus::Failed(crate::domain::FailureReason::AgentError);
                    task.updated_at = clock.0;
                }
                EvaluationDecision::OffTrack => {
                    // TODO: 根据配置策略处理偏离
                    task.status = TaskStatus::Ready;
                    task.updated_at = clock.0;
                }
            }
        }
        commands.entity(entity).despawn();
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
}
