//! SubTask 派发前置系统
//!
//! 扫描带 `SubTaskConfig` 的 Task，执行派发前置条件检查：
//! - DAG 依赖检查
//! - 兄弟任务结果收集（注入到 task content）
//! - AgentSpawnSpec 准备
//!
//! 准备完成后附加 `PendingDispatch` Component，由 `dispatch_system` 接管派发决策。

use crate::prelude::*;
use tracing::{debug, trace};

use crate::{
    app::Clock,
    domain::{
        AgentSpawnSpec, BatchTaskState, DispatchHint, DispatchKind, DispatchStrategy,
        PendingDispatch, SubTaskBatchState, SubTaskConfig, Task, TaskStatus,
    },
};

/// SubTask 派发前置系统
///
/// 在 `HarnessSet::Dispatch` 中运行，通过 `.before(dispatch_system)` 保证顺序。
pub fn subtask_dispatch_preparation_system(
    _clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<(Entity, &mut Task, &SubTaskConfig, Option<&PendingDispatch>)>,
    batch_states: Query<&SubTaskBatchState>,
) {
    for (entity, mut task, config, pending) in tasks.iter_mut() {
        // 已有 PendingDispatch 的跳过
        if pending.is_some() {
            continue;
        }

        // 只处理 Ready / Pending 状态
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // 已有 delegate 的跳过
        if task.delegate.is_some() {
            continue;
        }

        // 1. DAG 依赖检查
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

        // 2. 收集兄弟任务结果（注入到 task content）
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

        // 3. 注入兄弟任务结果到 task content
        if let Some(results) = &sibling_results {
            task.content = format!(
                "{}\n\n## 兄弟任务结果\n\n{}\n\n请基于以上兄弟任务的结果完成你的任务。你可以直接引用这些结果，无需重新计算或搜索。",
                task.content,
                results.join("\n\n")
            );
        }

        // 4. 准备 AgentSpawnSpec
        let spawn_spec = AgentSpawnSpec {
            name: config.child_agent_name.clone(),
            model: config.child_agent_model.clone(),
            allowed_tools: config.allowed_tools.clone(),
            parent_agent_id: Some(config.parent_agent_id),
        };

        // 5. 附加 PendingDispatch（走 BrainLlm 策略）
        commands.entity(entity).insert(PendingDispatch {
            kind: DispatchKind::Task,
            hint: DispatchHint {
                strategy: DispatchStrategy::BrainLlm,
                preferred_agent_name: None,
                required_skill_id: None,
                agent_spawn_spec: Some(spawn_spec),
            },
        });

        debug!(
            event = "SubTaskDispatchPrepared",
            task_id = %task.id,
            child_name = %config.child_agent_name,
            batch_id = %config.batch_id,
            has_sibling_results = sibling_results.is_some(),
            "sub-task prepared for dispatch"
        );
    }
}
