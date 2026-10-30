//! 主循环唤醒契约：通知可合并，业务数据不可丢弃。

use crate::prelude::Resource;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Instant,
};

#[derive(Debug, Default)]
pub struct WakeReason {
    pub runtime: bool,
    pub ui: bool,
}

/// 所有生产者共享的唤醒门铃。通知只占一个槽，payload 仍留在各自业务队列。
#[derive(Resource, Clone, Debug)]
pub struct RuntimeWake {
    flags: Arc<AtomicU8>,
    tx: crossbeam_channel::Sender<()>,
    rx: crossbeam_channel::Receiver<()>,
}

impl Default for RuntimeWake {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::bounded(1);
        Self {
            flags: Arc::new(AtomicU8::new(0)),
            tx,
            rx,
        }
    }
}

impl RuntimeWake {
    pub fn notify(&self) {
        self.signal(1);
    }
    pub fn notify_ui(&self) {
        self.signal(2);
    }
    fn signal(&self, flag: u8) {
        self.flags.fetch_or(flag, Ordering::Release);
        let _ = self.tx.try_send(());
    }
    /// 仅在一批工作开始前取走通知；休眠检查完成后不可再清空它。
    pub fn take(&self) -> WakeReason {
        let _ = self.rx.try_recv();
        let flags = self.flags.swap(0, Ordering::AcqRel);
        WakeReason {
            runtime: flags & 1 != 0,
            ui: flags & 2 != 0,
        }
    }
    pub fn receiver(&self) -> &crossbeam_channel::Receiver<()> {
        &self.rx
    }
}

/// 将“发送数据、成功后通知”封装为不可分开的生产者操作。
#[derive(Debug)]
pub struct WakeSender<T> {
    tx: Transport<T>,
    wake: Option<RuntimeWake>,
}
#[derive(Debug)]
enum Transport<T> {
    Async(tokio::sync::mpsc::UnboundedSender<T>),
    Sync(crossbeam_channel::Sender<T>),
}
impl<T> Clone for Transport<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Async(tx) => Self::Async(tx.clone()),
            Self::Sync(tx) => Self::Sync(tx.clone()),
        }
    }
}
impl<T> Clone for WakeSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: self.wake.clone(),
        }
    }
}
impl<T> From<tokio::sync::mpsc::UnboundedSender<T>> for WakeSender<T> {
    fn from(tx: tokio::sync::mpsc::UnboundedSender<T>) -> Self {
        Self {
            tx: Transport::Async(tx),
            wake: None,
        }
    }
}
impl<T> From<crossbeam_channel::Sender<T>> for WakeSender<T> {
    fn from(tx: crossbeam_channel::Sender<T>) -> Self {
        Self {
            tx: Transport::Sync(tx),
            wake: None,
        }
    }
}
impl<T> WakeSender<T> {
    pub fn new(tx: impl Into<Self>, wake: RuntimeWake) -> Self {
        let mut sender = tx.into();
        sender.set_wake(wake);
        sender
    }
    pub fn set_wake(&mut self, wake: RuntimeWake) {
        self.wake = Some(wake);
    }
    pub fn send(&self, value: T) -> Result<(), tokio::sync::mpsc::error::SendError<T>> {
        match &self.tx {
            Transport::Async(tx) => tx.send(value)?,
            Transport::Sync(tx) => tx
                .send(value)
                .map_err(|error| tokio::sync::mpsc::error::SendError(error.0))?,
        }
        if let Some(wake) = &self.wake {
            wake.notify();
        }
        Ok(())
    }
}

/// 可替换、可撤销的运行期期限。键的数量就是索引大小，不保留陈旧堆项。
#[derive(Debug)]
pub struct RuntimeDeadlines<K: Ord + Clone> {
    by_key: BTreeMap<K, Instant>,
    by_time: BTreeSet<(Instant, K)>,
}
impl<K: Ord + Clone> Default for RuntimeDeadlines<K> {
    fn default() -> Self {
        Self {
            by_key: BTreeMap::new(),
            by_time: BTreeSet::new(),
        }
    }
}
impl<K: Ord + Clone> RuntimeDeadlines<K> {
    pub fn set(&mut self, key: K, at: Instant) {
        self.remove(key.clone());
        self.by_key.insert(key.clone(), at);
        self.by_time.insert((at, key));
    }
    pub fn remove(&mut self, key: K) {
        if let Some(at) = self.by_key.remove(&key) {
            self.by_time.remove(&(at, key));
        }
    }
    pub fn next(&self) -> Option<Instant> {
        self.by_time.first().map(|(at, _)| *at)
    }
    pub fn take_due(&mut self, now: Instant) -> Vec<K> {
        let mut due = Vec::new();
        while self.next().is_some_and(|at| at <= now) {
            let (_, key) = self.by_time.pop_first().expect("deadline exists");
            self.by_key.remove(&key);
            due.push(key);
        }
        due
    }
}

#[derive(Resource)]
pub struct ExecutionResultSender(pub WakeSender<crate::domain::AgentExecutionResult>);
#[derive(Resource)]
pub struct ModelChainStateUpdateSender(pub WakeSender<crate::domain::ModelChainStateUpdate>);
#[derive(Resource)]
pub struct ToolResultSender(pub WakeSender<crate::domain::ToolAsyncResult>);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    #[test]
    fn coalesced_notification_preserves_every_payload() {
        let wake = RuntimeWake::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = WakeSender::new(tx, wake.clone());
        for value in 0..1000 {
            sender.send(value).unwrap();
        }
        assert!(wake.take().runtime);
        for value in 0..1000 {
            assert_eq!(rx.try_recv().unwrap(), value);
        }
        assert!(rx.try_recv().is_err());
        assert!(!wake.take().runtime);
    }

    #[test]
    fn arrival_between_check_and_wait_is_not_lost() {
        let wake = RuntimeWake::default();
        let barrier = Arc::new(Barrier::new(2));
        let worker_wake = wake.clone();
        let worker_barrier = barrier.clone();
        assert!(!wake.take().runtime);
        let worker = std::thread::spawn(move || {
            worker_wake.notify();
            worker_barrier.wait();
        });
        barrier.wait();
        let mut select = crossbeam_channel::Select::new();
        select.recv(wake.receiver());
        assert!(select.ready_timeout(Duration::ZERO).is_ok());
        assert!(wake.take().runtime);
        worker.join().unwrap();
    }

    #[test]
    fn ui_wakeup_does_not_schedule_ecs_and_failed_send_does_not_wake() {
        let wake = RuntimeWake::default();
        wake.notify_ui();
        let reason = wake.take();
        assert!(reason.ui);
        assert!(!reason.runtime);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<u8>();
        drop(rx);
        assert!(WakeSender::new(tx, wake.clone()).send(1).is_err());
        assert!(!wake.take().runtime);
    }

    #[test]
    fn replace_and_cancel_deadlines_do_not_leave_stale_entries() {
        let now = Instant::now();
        let mut deadlines = RuntimeDeadlines::default();
        deadlines.set("wait", now);
        deadlines.set("wait", now + Duration::from_secs(10));
        assert!(deadlines.take_due(now).is_empty());
        assert_eq!(deadlines.next(), Some(now + Duration::from_secs(10)));
        deadlines.remove("wait");
        assert_eq!(deadlines.next(), None);
        deadlines.set("retry", now);
        assert_eq!(deadlines.take_due(now), vec!["retry"]);
        assert_eq!(deadlines.next(), None);
    }
}
