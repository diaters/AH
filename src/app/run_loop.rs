//! 主循环：TUI 与 headless 共用。
//!
//! 每帧：前端轮询输入 → `app.update()` → 退出判定 → 前端渲染 → 按空闲状态休眠。
//! 与前端形态相关的部分由 [`LoopHooks`] 提供。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use bevy_app::App;
use tracing::{debug, info, warn};

use super::app_is_idle;
use crate::domain::ShutdownState;
use crate::systems::HarnessSettings;

/// 主循环中与前端形态相关的部分。
pub trait LoopHooks {
    /// 处理本帧输入与引擎事件，返回本帧是否有活动（影响轮询间隔与是否渲染）。
    fn poll(&mut self) -> bool;
    /// 渲染；`active` 为本帧有活动或运行时非空闲。
    fn render(&mut self, _active: bool) {}
    /// 前端要求立即退出（不等待运行时空闲）。
    fn should_quit(&self) -> bool {
        false
    }
    /// 周期心跳日志的附加字段（仅 debug 级别）。
    fn heartbeat(&self, _tick: u64) {}
}

/// 心跳日志间隔（帧）。
const HEARTBEAT_TICKS: u64 = 300;

/// 驱动 ECS 直到运行时请求关闭且空闲，或前端要求退出；返回已运行的帧数。
pub fn run_loop(app: &mut App, hooks: &mut impl LoopHooks) -> u64 {
    let mut tick: u64 = 0;
    loop {
        let had_activity = hooks.poll();

        app.update();

        let shutdown_requested = app.world().resource::<ShutdownState>().requested;
        let idle = app_is_idle(app.world_mut());
        let settings = &app.world().resource::<HarnessSettings>().0;
        let sleep_ms = if idle && !had_activity {
            settings.idle_poll_ms
        } else {
            settings.active_poll_ms
        };

        if tick.is_multiple_of(HEARTBEAT_TICKS) {
            debug!(
                event = "MainLoopHeartbeat",
                tick, shutdown_requested, idle, sleep_ms, "main loop heartbeat"
            );
            hooks.heartbeat(tick);
        }

        if shutdown_requested && idle {
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

        hooks.render(!idle || had_activity);

        thread::sleep(Duration::from_millis(sleep_ms));
        tick += 1;
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
