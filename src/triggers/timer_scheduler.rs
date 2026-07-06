//! cron timer scheduler
//!
//! 通过 `watch::Receiver<SchedulerState>` 接收热加载信号。无论是否配置
//! `triggers.toml`，scheduler 都会启动，以支持仅动态任务模式。
//!
//! 注意：本文件目前仅处理静态 Timer 路由（`SchedulerState.static_routes`）。
//! 本地时区与动态任务调度在后续 task 中实现。

use std::time::Duration;

use chrono::Utc;
use cron::Schedule;
use crossbeam_channel::Sender;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::domain::{ExternalInput, SignalSource};
use crate::triggers::config::build_schedules;
use crate::triggers::scheduled_task::SchedulerState;

/// 启动 timer scheduler。
///
/// 阻塞当前 tokio task。配置变更通过 `state_rx` 接收，重建 schedules。
/// 当 `state_rx` 的发送端关闭时返回 `Ok(())`。
pub async fn run_timer_scheduler(
    input_tx: Sender<ExternalInput>,
    mut state_rx: watch::Receiver<SchedulerState>,
) -> anyhow::Result<()> {
    let initial = state_rx.borrow().clone();
    let mut schedules = build_schedules_from_state(&initial)?;
    info!(
        event = "TimerSchedulerStarted",
        count = schedules.len(),
        "timer scheduler started"
    );

    loop {
        let now = Utc::now();
        let next = schedules
            .iter()
            .filter_map(|(s, kind)| s.upcoming(Utc).next().map(|t| (t, kind.clone())))
            .min_by_key(|(t, _)| *t);

        match next {
            Some((next_time, kind)) => {
                let dur = (next_time - now)
                    .to_std()
                    .unwrap_or(Duration::from_secs(60));
                tokio::select! {
                    _ = tokio::time::sleep(dur) => {
                        debug!(
                            event = "TimerTriggered",
                            kind = %kind,
                            next_at = %next_time,
                            "timer fired"
                        );
                        let _ = input_tx.send(ExternalInput::Timer {
                            source: SignalSource("timer".to_string()),
                            kind,
                        });
                    }
                    res = state_rx.changed() => {
                        // `changed()` 已将当前值标记为 seen，因此这里直接用
                        // `borrow_and_update` 取最新配置即可。发送端关闭时退出。
                        if res.is_err() {
                            return Ok(());
                        }
                        reload_schedules(&mut state_rx, &mut schedules);
                    }
                }
            }
            None => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                    res = state_rx.changed() => {
                        if res.is_err() {
                            return Ok(());
                        }
                        reload_schedules(&mut state_rx, &mut schedules);
                    }
                }
            }
        }
    }
}

/// 从 `SchedulerState` 的静态路由构建 schedules。
///
/// `static_routes` 为 `None` 时返回空 vec（仅动态任务模式）。
/// `timer.enabled = false` 时跳过静态 timer 路由（返回空 vec），
/// 但不影响后续 task 中实现的动态任务。
fn build_schedules_from_state(state: &SchedulerState) -> anyhow::Result<Vec<(Schedule, String)>> {
    let Some(routes) = state.static_routes() else {
        return Ok(Vec::new());
    };
    if routes.timer.enabled {
        build_schedules(&routes.timer)
    } else {
        Ok(Vec::new())
    }
}

/// 重建 schedules。失败时保留旧值并记 warning（spec L87-111）。
fn reload_schedules(
    state_rx: &mut watch::Receiver<SchedulerState>,
    schedules: &mut Vec<(Schedule, String)>,
) {
    let new_state = state_rx.borrow_and_update().clone();
    match build_schedules_from_state(&new_state) {
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
