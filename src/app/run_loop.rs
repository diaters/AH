//! 主循环：TUI 与 headless 共用。
//!
//! 有数据或到期工作才执行 ECS；跨轮工作连续推进，其他时间阻塞等待通知。
//! 与前端形态相关的部分由 [`LoopHooks`] 提供。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use bevy_app::App;
use tracing::{debug, info, warn};

use super::app_is_idle;
use crate::contracts::{BufferedIngress, FrontendRegistry, RuntimeWake};
use crate::domain::ShutdownState;
use crate::systems::demand::DemandSchedule;

/// 主循环中与前端形态相关的部分。
pub trait LoopHooks {
    /// 处理输入与引擎事件，返回是否有可见变化；纯 UI 活动不会触发 ECS。
    fn poll(&mut self) -> bool;
    /// 渲染；`active` 表示本批有可见变化，前端自行限制绘制频率。
    fn render(&mut self, _active: bool) {}
    /// 前端要求立即退出（不等待运行时空闲）。
    fn should_quit(&self) -> bool {
        false
    }
    /// UI 动画或关闭宽限期的下一次检查；不会单独触发 ECS。
    fn next_deadline(&self) -> Option<Instant> {
        None
    }
    /// 周期心跳日志的附加字段（仅 debug 级别）。
    fn heartbeat(&self, _tick: u64) {}
}

/// 驱动 ECS 至关闭；返回实际 ECS 更新次数。任务未结束不等于当前有工作可推进。
pub fn run_loop(app: &mut App, hooks: &mut impl LoopHooks) -> u64 {
    let wake = app.world().resource::<RuntimeWake>().clone();
    let input_rx = app
        .world()
        .resource::<crate::domain::InputReceiver>()
        .0
        .clone();
    let registry = app.world().resource::<FrontendRegistry>();
    let actions: Vec<_> = registry
        .frontends
        .iter()
        .filter_map(|f| f.action_receiver())
        .collect();
    let legacy_poll = registry.frontends.iter().any(|f| f.requires_polling());
    let mut input_open = true;
    let mut actions_open = vec![true; actions.len()];
    let mut tick = 0u64;
    let mut bootstrap = true;
    let mut heartbeat_at = Instant::now() + Duration::from_secs(30);
    let mut legacy_at = Instant::now();
    let mut batch_started = Instant::now();
    let mut batch_ticks = 0u32;
    loop {
        let reason = wake.take();
        let mut visible_activity = hooks.poll();
        let now = Instant::now();
        let due = app
            .world_mut()
            .resource_mut::<DemandSchedule>()
            .take_due(now);
        let buffered = app.world().resource::<BufferedIngress>();
        let ready = bootstrap
            || reason.runtime
            || due
            || !buffered.inputs.is_empty()
            || !buffered.actions.is_empty()
            || !input_rx.is_empty()
            || actions.iter().any(|rx| !rx.is_empty())
            || (legacy_poll && now >= legacy_at);
        if ready {
            bootstrap = false;
            app.update();
            tick += 1;
            batch_ticks += 1;
            legacy_at = Instant::now() + Duration::from_millis(150);
            // 输出在同一批绘制前消费；UI 活动本身无需推动 ECS。
            visible_activity |= hooks.poll();
        }
        let shutdown = app.world().resource::<ShutdownState>().requested;
        if shutdown && app_is_idle(app.world_mut()) {
            info!(
                event = "GracefulShutdown",
                tick, "shutdown requested and app is idle, exiting"
            );
            break;
        }
        if hooks.should_quit() {
            info!(event = "UserQuit", tick, "frontend requested quit, exiting");
            break;
        }
        hooks.render(visible_activity);
        if now >= heartbeat_at {
            debug!(
                event = "MainLoopHeartbeat",
                tick, shutdown, "demand-driven main loop heartbeat"
            );
            hooks.heartbeat(tick);
            heartbeat_at = Instant::now() + Duration::from_secs(30);
        }
        if batch_ticks >= 8 || batch_started.elapsed() >= Duration::from_millis(4) {
            thread::yield_now();
            batch_ticks = 0;
            batch_started = Instant::now();
        }
        // 不清空这里到达的通知：它们正是阻塞接收需要看见的数据。
        let mut deadline = heartbeat_at;
        if let Some(at) = app.world().resource::<DemandSchedule>().next_deadline() {
            deadline = deadline.min(at);
        }
        if let Some(at) = hooks.next_deadline() {
            deadline = deadline.min(at);
        }
        if legacy_poll {
            deadline = deadline.min(legacy_at);
        }
        let mut select = crossbeam_channel::Select::new();
        let wake_index = select.recv(wake.receiver());
        let input_index = input_open.then(|| select.recv(&input_rx));
        let action_indices: Vec<_> = actions
            .iter()
            .enumerate()
            .filter(|(i, _)| actions_open[*i])
            .map(|(i, rx)| (select.recv(rx), i))
            .collect();
        if let Ok(operation) =
            select.select_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            let index = operation.index();
            if index == wake_index {
                let _ = operation.recv(wake.receiver());
            } else if Some(index) == input_index {
                match operation.recv(&input_rx) {
                    Ok(input) => app
                        .world_mut()
                        .resource_mut::<BufferedIngress>()
                        .inputs
                        .push_back(input),
                    Err(_) => input_open = false,
                }
            } else if let Some((_, i)) = action_indices
                .iter()
                .find(|(selected, _)| *selected == index)
            {
                match operation.recv(&actions[*i]) {
                    Ok(action) => app
                        .world_mut()
                        .resource_mut::<BufferedIngress>()
                        .actions
                        .push(action),
                    Err(_) => actions_open[*i] = false,
                }
            }
        }
    }
    tick
}

/// headless 运行形态的主循环钩子：无终端输入与渲染，只根据关闭信号决定退出。
///
/// 第一个信号（`signals` 计数 ≥ 1）后等待运行时空闲以优雅退出；空闲判定把等待用户的
/// 多轮任务也算作活跃，因此超过 `grace` 仍未空闲，或收到第二个信号时，直接退出。
pub struct HeadlessLoop {
    signals: Arc<AtomicUsize>,
    grace: Duration,
    first_signal_at: Option<Instant>,
}

impl HeadlessLoop {
    pub fn new(signals: Arc<AtomicUsize>, grace: Duration) -> Self {
        Self {
            signals,
            grace,
            first_signal_at: None,
        }
    }
}

impl LoopHooks for HeadlessLoop {
    fn next_deadline(&self) -> Option<Instant> {
        self.first_signal_at.map(|at| at + self.grace)
    }
    fn poll(&mut self) -> bool {
        if self.first_signal_at.is_none() && self.signals.load(Ordering::SeqCst) > 0 {
            self.first_signal_at = Some(Instant::now());
            info!(
                event = "HeadlessShutdownSignal",
                grace_secs = self.grace.as_secs(),
                "shutdown signal received, waiting for idle"
            );
        }
        false
    }

    fn should_quit(&self) -> bool {
        if self.signals.load(Ordering::SeqCst) >= 2 {
            return true;
        }
        let expired = self
            .first_signal_at
            .is_some_and(|at| at.elapsed() >= self.grace);
        if expired {
            warn!(
                event = "HeadlessShutdownGraceExpired",
                "runtime not idle within grace period, exiting anyway"
            );
        }
        expired
    }
}
