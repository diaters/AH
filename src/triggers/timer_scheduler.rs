//! cron timer scheduler
//!
//! 通过 `watch::Receiver<TriggerConfig>` 接收热加载信号。

use std::time::Duration;

use chrono::Utc;
use cron::Schedule;
use crossbeam_channel::Sender;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::domain::{ExternalInput, SignalSource};
use crate::triggers::config::TriggerConfig;
use crate::triggers::config::build_schedules;

/// 启动 timer scheduler。
///
/// 阻塞当前 tokio task。配置变更通过 `config_rx` 接收，重建 schedules。
/// 当 `config_rx` 的发送端关闭时返回 `Ok(())`。
pub async fn run_timer_scheduler(
    input_tx: Sender<ExternalInput>,
    mut config_rx: watch::Receiver<TriggerConfig>,
) -> anyhow::Result<()> {
    let initial = config_rx.borrow().clone();
    let mut schedules = build_schedules(&initial.timer)?;
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
                    res = config_rx.changed() => {
                        // `changed()` 已将当前值标记为 seen，因此这里直接用
                        // `borrow_and_update` 取最新配置即可。发送端关闭时退出。
                        if res.is_err() {
                            return Ok(());
                        }
                        reload_schedules(&mut config_rx, &mut schedules);
                    }
                }
            }
            None => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                    res = config_rx.changed() => {
                        if res.is_err() {
                            return Ok(());
                        }
                        reload_schedules(&mut config_rx, &mut schedules);
                    }
                }
            }
        }
    }
}

/// 重建 schedules。失败时保留旧值并记 warning（spec L87-111）。
fn reload_schedules(
    config_rx: &mut watch::Receiver<TriggerConfig>,
    schedules: &mut Vec<(Schedule, String)>,
) {
    let new_config = config_rx.borrow_and_update().clone();
    match build_schedules(&new_config.timer) {
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
