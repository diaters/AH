//! 定时运行的真实发送回执；排队不计为通知成功。

use crate::{channels::traits::DeliveryCallback, domain::TaskId, prelude::Resource};
use std::collections::HashMap;

#[derive(Default)]
struct DeliveryCounts {
    pending: usize,
    notified: u32,
    failure: Option<String>,
}
struct Receipt {
    task_id: TaskId,
    result: Result<Option<String>, String>,
}

#[derive(Resource)]
pub struct ScheduleDeliveryTracker {
    by_task: HashMap<TaskId, DeliveryCounts>,
    tx: crate::contracts::WakeSender<Receipt>,
    rx: crossbeam_channel::Receiver<Receipt>,
}
impl Default for ScheduleDeliveryTracker {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            by_task: HashMap::new(),
            tx: tx.into(),
            rx,
        }
    }
}
impl ScheduleDeliveryTracker {
    pub(crate) fn set_wake(&mut self, wake: crate::contracts::RuntimeWake) {
        self.tx.set_wake(wake);
    }
    /// 在入队前登记；同步拒绝须调用 reject，已接受请求由回调完成。
    pub fn begin(&mut self, task_id: TaskId) -> DeliveryCallback {
        self.by_task.entry(task_id).or_default().pending += 1;
        let tx = self.tx.clone();
        Box::new(move |result| {
            let _ = tx.send(Receipt { task_id, result });
        })
    }
    pub fn reject(&mut self, task_id: TaskId, error: String) {
        if let Some(counts) = self.by_task.get_mut(&task_id) {
            counts.pending = counts.pending.saturating_sub(1);
            counts.failure = Some(error);
        }
    }
    pub fn poll_receipts(&mut self) {
        while let Ok(receipt) = self.rx.try_recv() {
            let Some(counts) = self.by_task.get_mut(&receipt.task_id) else {
                continue;
            };
            counts.pending = counts.pending.saturating_sub(1);
            match receipt.result {
                Ok(_) => counts.notified = counts.notified.saturating_add(1),
                Err(error) => counts.failure = Some(error),
            }
        }
    }
    pub fn pending_for(&self, id: TaskId) -> usize {
        self.by_task.get(&id).map_or(0, |counts| counts.pending)
    }
    pub fn notified_for(&self, id: TaskId) -> u32 {
        self.by_task.get(&id).map_or(0, |counts| counts.notified)
    }
    pub fn failure_for(&self, id: TaskId) -> Option<&str> {
        self.by_task
            .get(&id)
            .and_then(|counts| counts.failure.as_deref())
    }
    pub fn forget(&mut self, id: TaskId) {
        self.by_task.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_send_is_not_success_until_real_callback_arrives() {
        let mut tracker = ScheduleDeliveryTracker::default();
        let id = TaskId::new();
        let callback = tracker.begin(id);
        tracker.poll_receipts();
        assert_eq!(tracker.pending_for(id), 1);
        assert_eq!(tracker.notified_for(id), 0);
        callback(Ok(None));
        tracker.poll_receipts();
        assert_eq!(tracker.pending_for(id), 0);
        assert_eq!(tracker.notified_for(id), 1);
        assert!(tracker.failure_for(id).is_none());
    }
    #[test]
    fn rejected_and_failed_sends_do_not_count_and_late_receipts_do_not_resurrect() {
        let mut tracker = ScheduleDeliveryTracker::default();
        let id = TaskId::new();
        let callback = tracker.begin(id);
        callback(Err("network timeout".into()));
        tracker.poll_receipts();
        assert_eq!(tracker.notified_for(id), 0);
        assert_eq!(tracker.failure_for(id), Some("network timeout"));
        let dropped = tracker.begin(id);
        tracker.reject(id, "queue closed".into());
        drop(dropped);
        assert_eq!(tracker.pending_for(id), 0);
        let late = tracker.begin(id);
        tracker.forget(id);
        late(Ok(None));
        tracker.poll_receipts();
        assert_eq!(tracker.notified_for(id), 0);
        assert_eq!(tracker.pending_for(id), 0);
    }
}
