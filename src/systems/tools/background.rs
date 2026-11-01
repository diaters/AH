//! 工具调用自动转后台（异步 ReAct Loop）。
//!
//! 可后台化工具的任务级调用超过前台预算仍未返回时：
//! - 立即给工具循环回一个 `running_in_background` 合成结果（含 `job_id`），LLM 继续推理；
//!   不依赖该结果的工作可以继续，依赖它时由 LLM 结束本轮；
//! - worker 继续运行，挂起实体挂上 `BackgroundedToolCall`，并登记 `JobSource::ToolCall`
//!   后台作业：任务不会在结果送达前进入终态；
//! - 结果到达时由 ingest 投递为 `<task-notification>`，空闲唤醒开启新的一轮。
//!
//! 失联超时（sweeper）、父任务终态取消（cancel_monitor）对转后台的调用继续生效；
//! `/stop` 只中断当前回合，不取消已转后台的调用。

use tracing::debug;

use crate::contracts::Clock;
use crate::domain::{
    BackgroundedToolCall, InFlightToolCall, JobSource, StopRequested, Task, ToolRequestPending,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;
use crate::systems::demand::{DeadlineKey, DemandSchedule};

use super::ingest_tool_results::spawn_tool_result_message;

/// 转后台合成结果中给 LLM 的行动指引。
fn background_notice(job_id: uuid::Uuid, elapsed_secs: i64) -> String {
    format!(
        "工具已运行 {elapsed_secs}s 仍未结束，已自动转为后台执行，进程不受影响。\
完成后结果会以 <task-notification source=\"tool\" job=\"{job_id}\"> 送达并自动开启新的一轮。\
不依赖该结果的工作可以继续；后续步骤依赖该结果时，请直接结束本轮回复并简要说明在等待什么，\
不要轮询或重复调用同一命令。确认不再需要时调用 stop_background_job。"
    )
}

/// 到达前台预算的在飞调用转后台。
#[allow(clippy::type_complexity)]
pub fn tool_auto_background_system(
    mut commands: Commands,
    clock: Res<Clock>,
    index: Res<EntityIndex>,
    tasks: Query<(&Task, Has<StopRequested>)>,
    calls: Query<(Entity, &ToolRequestPending, &InFlightToolCall), Without<BackgroundedToolCall>>,
    schedule: Option<Res<DemandSchedule>>,
) {
    let now = clock.0;
    for (entity, pending, in_flight) in &calls {
        let Some(at) = in_flight.background_at else {
            continue;
        };
        let due = schedule.as_deref().map_or(now >= at, |schedule| {
            schedule.expired_at(DeadlineKey::ToolBackground(entity), at, now)
        });
        if !due {
            continue;
        }
        let task_id = pending.original_request.task_id;
        // 所属任务已终态或正在 /stop：交给 cancel_monitor 取消，不再转后台。
        let Some(owner) = index.get_task(&task_id).filter(|owner| {
            tasks
                .get(*owner)
                .is_ok_and(|(task, stopping)| !task.status.is_terminal() && !stopping)
        }) else {
            continue;
        };

        let job_id = crate::systems::loop_control::spawn_background_job(
            &mut commands,
            owner,
            task_id,
            JobSource::ToolCall {
                tool_call_id: pending.tool_call_id.clone(),
                tool_name: pending.tool_name.clone(),
            },
            in_flight.started_at,
        );
        commands.entity(entity).insert(BackgroundedToolCall {
            job_id,
            detached_at: now,
            silent: false,
            note: None,
        });
        let elapsed_secs = (now - in_flight.started_at).num_seconds();
        spawn_tool_result_message(
            &mut commands,
            pending,
            Ok(serde_json::json!({
                "status": "running_in_background",
                "job_id": job_id.to_string(),
                "elapsed_secs": elapsed_secs,
                "message": background_notice(job_id, elapsed_secs),
            })),
        );
        debug!(
            event = "ToolCallBackgrounded",
            task_id = %task_id,
            job_id = %job_id,
            tool_call_id = %pending.tool_call_id,
            tool_name = %pending.tool_name,
            elapsed_secs,
            "tool call exceeded foreground budget; moved to background"
        );
    }
}
