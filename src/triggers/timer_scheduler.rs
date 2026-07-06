//! cron timer scheduler
//!
//! 通过 `watch::Receiver<SchedulerState>` 接收热加载信号。无论是否配置
//! `triggers.toml`，scheduler 都会启动，以支持仅动态任务模式。
//!
//! 时区策略：
//! - cron 调度使用 `Local` 时区计算下一次触发时间（用户书写本地时间）
//! - 一次性任务以 `DateTime<Utc>` 比较，避免时区歧义
//! - 两者统一为 `DateTime<Utc>` 后取最早者作为 sleep 目标

use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use crossbeam_channel::Sender;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::domain::{ExternalInput, SignalSource};
use crate::triggers::config::build_schedules;
use crate::triggers::scheduled_task::{ScheduleSpec, ScheduledItem, SchedulerState};

/// 启动 timer scheduler。
///
/// 阻塞当前 tokio task。配置变更通过 `state_rx` 接收，重建 schedules。
/// 当 `state_rx` 的发送端关闭时返回 `Ok(())`。
///
/// cron 使用 `Local` 时区，一次性任务使用 `Utc` 比较；两者统一到
/// `DateTime<Utc>` 后取最早 deadline。一次性任务触发后立即从本地
/// `schedules` 副本移除，避免重复触发。
pub async fn run_timer_scheduler(
    input_tx: Sender<ExternalInput>,
    mut state_rx: watch::Receiver<SchedulerState>,
) -> anyhow::Result<()> {
    let initial = state_rx.borrow().clone();
    let mut schedules = build_all_schedules(&initial)?;
    info!(
        event = "TimerSchedulerStarted",
        static_routes = initial.static_routes().is_some() as usize,
        dynamic_tasks = initial.dynamic_tasks().len(),
        count = schedules.len(),
        "timer scheduler started"
    );

    loop {
        let now_utc = Utc::now();

        let next_cron: Option<(DateTime<Utc>, String)> = schedules
            .iter()
            .filter_map(|item| match item {
                ScheduledItem::Cron { schedule, kind } => schedule
                    .upcoming(Local)
                    .next()
                    .map(|t| (t.with_timezone(&Utc), kind.clone())),
                ScheduledItem::Once { .. } => None,
            })
            .min_by_key(|(t, _)| *t);

        // 一次性任务取最早触发时间（无论是否已过期），过期任务在唤醒后立即触发
        let next_once: Option<(DateTime<Utc>, String)> = schedules
            .iter()
            .filter_map(|item| match item {
                ScheduledItem::Once { at, kind, .. } => Some((*at, kind.clone())),
                _ => None,
            })
            .min_by_key(|(t, _)| *t);

        // 合并 cron 与一次性任务，取最早的 UTC 时间作为 sleep 目标
        let next_deadline: Option<&(DateTime<Utc>, String)> = next_cron
            .as_ref()
            .into_iter()
            .chain(next_once.as_ref())
            .min_by_key(|(t, _)| *t);

        let sleep_duration = next_deadline
            .map(|(t, _)| {
                let dur = t.signed_duration_since(now_utc);
                if dur < ChronoDuration::zero() {
                    ChronoDuration::zero()
                } else {
                    dur
                }
            })
            .map(|d| d.to_std().unwrap_or(Duration::from_secs(60)))
            .unwrap_or_else(|| Duration::from_secs(60));

        tokio::select! {
            _ = tokio::time::sleep(sleep_duration) => {
                // 触发到期的 cron 任务。`next_cron` 是 sleep 前计算的最早 cron
                // 触发时间；若 sleep 因其到期而唤醒，则 `cron_time <= now_utc`。
                if let Some((cron_time, cron_kind)) = &next_cron
                    && *cron_time <= now_utc
                {
                    debug!(
                        event = "TimerTriggered",
                        kind = %cron_kind,
                        next_at = %cron_time,
                        "timer fired"
                    );
                    let _ = input_tx.send(ExternalInput::Timer {
                        source: SignalSource("timer".to_string()),
                        kind: cron_kind.clone(),
                    });
                }
                // 触发并移除已到期的一次性任务。`next_once` 是最早的一次性触发
                // 时间；若 sleep 因其到期而唤醒，则该任务（及任何其他已过期的
                // 一次性任务）会在此时触发并从本地 schedules 副本移除。
                let mut i = 0;
                while i < schedules.len() {
                    if let ScheduledItem::Once { at, kind, .. } = &schedules[i]
                        && *at <= now_utc
                    {
                        debug!(
                            event = "TimerTriggered",
                            kind = %kind,
                            next_at = %at,
                            "timer fired"
                        );
                        let _ = input_tx.send(ExternalInput::Timer {
                            source: SignalSource("timer".to_string()),
                            kind: kind.clone(),
                        });
                        schedules.remove(i);
                        continue;
                    }
                    i += 1;
                }
            }
            res = state_rx.changed() => {
                // `changed()` 已将当前值标记为 seen，因此这里直接用
                // `borrow_and_update` 取最新配置即可。发送端关闭时退出。
                if res.is_err() {
                    return Ok(());
                }
                reload_state(&mut state_rx, &mut schedules);
            }
        }
    }
}

/// 合并静态路由与动态任务，构建统一调度列表。
///
/// - 静态路由仅在 `timer.enabled = true` 时纳入（保留 a37b859 的 gate 语义）
/// - 动态任务（`schedule_task` 工具创建）始终纳入
fn build_all_schedules(state: &SchedulerState) -> anyhow::Result<Vec<ScheduledItem>> {
    let mut items = Vec::new();
    if let Some(routes) = state.static_routes()
        && routes.timer.enabled
    {
        for (schedule, kind) in build_schedules(&routes.timer)? {
            items.push(ScheduledItem::Cron {
                schedule: Box::new(schedule),
                kind,
            });
        }
    }
    for task in state.dynamic_tasks() {
        match &task.schedule {
            ScheduleSpec::Once(at) => {
                items.push(ScheduledItem::Once {
                    id: task.id,
                    kind: task.kind.clone(),
                    at: *at,
                });
            }
            ScheduleSpec::Cron(schedule) => {
                items.push(ScheduledItem::Cron {
                    schedule: schedule.clone(),
                    kind: task.kind.clone(),
                });
            }
        }
    }
    Ok(items)
}

/// 重建 schedules。失败时保留旧值并记 warning（spec L87-111）。
fn reload_state(
    state_rx: &mut watch::Receiver<SchedulerState>,
    schedules: &mut Vec<ScheduledItem>,
) {
    state_rx.borrow_and_update();
    let new_state = state_rx.borrow().clone();
    match build_all_schedules(&new_state) {
        Ok(new_schedules) => {
            *schedules = new_schedules;
            info!(
                event = "TimerSchedulerReloaded",
                count = schedules.len(),
                "reloaded timer schedules"
            );
        }
        Err(e) => {
            warn!(
                event = "TimerSchedulerReloadFailed",
                error = %e,
                "keeping old schedules"
            );
        }
    }
}
