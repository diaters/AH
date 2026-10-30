//! 受管 stdout 事件投递：排队、平台回执与任务通知使用同一身份。

use crate::{
    channels::{ChannelManager, ChannelOutboundMessage, MessageKind},
    contracts::{Clock, FrontendRegistry},
    domain::{
        ChannelId, EngineEvent, EventTarget, FrontendKind, InboxDeliveryMessage, InboxItem,
        LoopControlConfig, MessageRole, SessionBackend, SessionHandleId, SessionOutputEvent,
        SystemOutputMessage, Task, TaskId,
    },
    ecs::EntityIndex,
    prelude::*,
    systems::NativeProcessBackend,
};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, VecDeque};
use tracing::{debug, info, warn};

const MAX_BATCH_BYTES: usize = 64 * 1024;
const MAX_HISTORY: usize = 128;
type DeliveryKey = (SessionHandleId, u64);

#[derive(Debug)]
enum DeliveryState {
    Collecting,
    Awaiting,
    RetryAt(DateTime<Utc>),
    Finished(DeliveryOutcome),
}

#[derive(Debug, Clone)]
enum DeliveryOutcome {
    Delivered,
    Failed(String),
    Terminated,
}

#[derive(Debug)]
struct PendingDelivery {
    task_id: TaskId,
    channel: Option<ChannelId>,
    last_sequence: u64,
    content: String,
    first_detected: DateTime<Utc>,
    collect_until: DateTime<Utc>,
    attempt: u8,
    queued_notified: bool,
    state: DeliveryState,
}

struct DeliveryReceipt {
    key: DeliveryKey,
    attempt: u8,
    result: Result<Option<String>, String>,
}

/// 同进程内的失败/送达记录；保留最近128批供诊断，不承诺跨重启恢复。
#[derive(Debug)]
pub struct MonitorDeliveryRecord {
    pub task_id: TaskId,
    pub session_id: SessionHandleId,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub content: String,
    pub status: String,
}

#[derive(Resource)]
pub struct MonitorDeliveries {
    pending: BTreeMap<DeliveryKey, PendingDelivery>,
    receipt_tx: crate::contracts::WakeSender<DeliveryReceipt>,
    receipt_rx: crossbeam_channel::Receiver<DeliveryReceipt>,
    records: VecDeque<MonitorDeliveryRecord>,
    last_poll: Option<DateTime<Utc>>,
}

impl Default for MonitorDeliveries {
    fn default() -> Self {
        let (receipt_tx, receipt_rx) = crossbeam_channel::unbounded();
        Self {
            pending: BTreeMap::new(),
            receipt_tx: receipt_tx.into(),
            receipt_rx,
            records: VecDeque::new(),
            last_poll: None,
        }
    }
}

impl MonitorDeliveries {
    pub(crate) fn set_wake(&mut self, wake: crate::contracts::RuntimeWake) {
        self.receipt_tx.set_wake(wake);
    }
    pub(crate) fn next_deadline(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.pending
            .values()
            .filter_map(|delivery| match delivery.state {
                DeliveryState::Collecting => Some(delivery.collect_until),
                DeliveryState::RetryAt(at) => Some(at),
                _ => None,
            })
            .filter(|at| *at > now)
            .min()
    }

    /// 最近的运行期投递结果；进程重启后不恢复。
    pub fn recent_records(&self) -> impl Iterator<Item = &MonitorDeliveryRecord> {
        self.records.iter()
    }

    pub(crate) fn has_pending_for_task(&self, id: TaskId) -> bool {
        self.pending.values().any(|delivery| delivery.task_id == id)
    }

    fn receive(&mut self, now: DateTime<Utc>) {
        while let Ok(receipt) = self.receipt_rx.try_recv() {
            let Some(delivery) = self.pending.get_mut(&receipt.key) else {
                continue;
            };
            if !matches!(delivery.state, DeliveryState::Awaiting)
                || delivery.attempt != receipt.attempt
            {
                continue;
            }
            match receipt.result {
                Ok(_) => delivery.state = DeliveryState::Finished(DeliveryOutcome::Delivered),
                Err(error) if delivery.attempt < 4 => {
                    let delay = 1_i64 << (delivery.attempt - 1);
                    delivery.state = DeliveryState::RetryAt(now + chrono::Duration::seconds(delay));
                    warn!(event = "MonitorDeliveryRetry", task_id = %delivery.task_id,
                        session_id = %receipt.key.0, sequence = receipt.key.1,
                        attempt = delivery.attempt, retry_secs = delay, error_type = "ChannelSend", error = %error,
                        "monitor notification will be retried; platform delivery not confirmed");
                }
                Err(error) => {
                    delivery.state = DeliveryState::Finished(DeliveryOutcome::Failed(error))
                }
            }
        }
    }

    fn enqueue(&mut self, event: SessionOutputEvent, channel: Option<ChannelId>, window_ms: u64) {
        let session = event.session_id;
        if self.pending.iter().any(|((id, first), pending)| {
            *id == session && event.sequence >= *first && event.sequence <= pending.last_sequence
        }) {
            return;
        }
        let content = if event.truncated {
            format!(
                "[输出行原长 {} 字节，以下为截断尾部]\n{}",
                event.original_bytes, event.content
            )
        } else {
            event.content
        };
        // 只能追加到该会话最后一批；填补更早批次的剩余空间会打乱序号及累计确认。
        let candidate = self
            .pending
            .iter_mut()
            .rev()
            .find(|((id, _), _)| *id == session)
            .filter(|(_, pending)| {
                matches!(pending.state, DeliveryState::Collecting)
                    && pending.content.len() + content.len() < MAX_BATCH_BYTES
            });
        if let Some((_, pending)) = candidate {
            pending.content.push('\n');
            pending.content.push_str(&content);
            pending.last_sequence = event.sequence;
        } else {
            self.pending.insert(
                (session, event.sequence),
                PendingDelivery {
                    task_id: event.task_id,
                    channel,
                    last_sequence: event.sequence,
                    content,
                    first_detected: event.detected_at,
                    collect_until: event.detected_at
                        + chrono::Duration::milliseconds(window_ms.min(i64::MAX as u64) as i64),
                    attempt: 0,
                    queued_notified: false,
                    state: DeliveryState::Collecting,
                },
            );
        }
        debug!(event = "MonitorOutputQueued", task_id = %event.task_id, session_id = %session,
            sequence = event.sequence, overflow = event.overflow, "stdout event queued for delivery");
    }
}

fn notify(
    commands: &mut Commands,
    key: DeliveryKey,
    delivery: &PendingDelivery,
    status: &str,
    now: DateTime<Utc>,
) {
    let mut end = delivery.content.len().min(1024);
    while !delivery.content.is_char_boundary(end) {
        end -= 1;
    }
    let preview = if status == "queued" {
        String::new()
    } else {
        format!(
            "\n{}{}",
            &delivery.content[..end],
            if end < delivery.content.len() {
                "\n[Agent上下文只保留摘要，完整告警独立投递]"
            } else {
                ""
            }
        )
    };
    commands.spawn(InboxDeliveryMessage { task_id: delivery.task_id,
        item: InboxItem::TaskNotification { job_id: key.0.0, source: "shell_output".into(), status: status.into(),
            summary: format!("session_id: {}\nsequence: {}..{}\nHarness负责告警投递，不要再次原样发送。排队不代表送达。{}", key.0, key.1, delivery.last_sequence, preview),
            received_at: now } });
}

/// 先收平台回执，再读取 backend 未确认事件；仅在终局送达/失败后确认消费。
#[allow(clippy::too_many_arguments)]
pub(crate) fn monitor_output_system(
    mut commands: Commands,
    clock: Res<Clock>,
    config: Res<LoopControlConfig>,
    backend: Res<NativeProcessBackend>,
    manager: Res<ChannelManager>,
    registry: Option<Res<FrontendRegistry>>,
    index: Res<EntityIndex>,
    tasks: Query<&Task>,
    mut deliveries: ResMut<MonitorDeliveries>,
    demand: Option<Res<crate::systems::demand::DemandSchedule>>,
    mut removed_jobs: RemovedComponents<crate::domain::BackgroundJob>,
) {
    let now = clock.0;
    deliveries.receive(now);
    let owner = |id: TaskId| {
        index
            .get_task(&id)
            .and_then(|entity| tasks.get(entity).ok())
    };
    let jobs_finished = removed_jobs.read().count() > 0;
    let should_poll = jobs_finished
        || demand.as_deref().is_some_and(|schedule| {
            schedule.is_due(crate::systems::demand::DeadlineKey::Background)
        })
        || deliveries.last_poll.is_none_or(|last| {
            now - last
                >= chrono::Duration::milliseconds(
                    config.background_poll_ms.min(i64::MAX as u64) as i64
                )
        });
    if should_poll {
        deliveries.last_poll = Some(now);
        match backend.poll_output_events() {
            Ok(mut events) => {
                events.sort_by_key(|event| (event.session_id, event.sequence));
                for event in events {
                    let channel = owner(event.task_id)
                        .and_then(|task| task.delivery_channel())
                        .cloned();
                    deliveries.enqueue(event, channel, config.notification_coalesce_ms);
                }
            }
            Err(error) => {
                warn!(event = "MonitorOutputPollFailed", error_type = "ProcessBackend", error = %error, "monitor output poll failed")
            }
        }
    }
    let keys: Vec<_> = deliveries.pending.keys().copied().collect();
    for key in keys {
        let alive = deliveries.pending.get(&key).is_some_and(|delivery| {
            owner(delivery.task_id).is_some_and(|task| !task.status().is_terminal())
        });
        let earlier = deliveries
            .pending
            .keys()
            .any(|other| other.0 == key.0 && other.1 < key.1);
        let receipt_tx = deliveries.receipt_tx.clone();
        let Some(delivery) = deliveries.pending.get_mut(&key) else {
            continue;
        };
        if !alive
            && !matches!(
                delivery.state,
                DeliveryState::Awaiting | DeliveryState::Finished(_)
            )
        {
            delivery.state = DeliveryState::Finished(DeliveryOutcome::Terminated);
        }
        let ready = match delivery.state {
            DeliveryState::Collecting => now >= delivery.collect_until,
            DeliveryState::RetryAt(at) => now >= at,
            _ => false,
        };
        if ready && !earlier {
            delivery.attempt += 1;
            let attempt = delivery.attempt;
            delivery.state = DeliveryState::Awaiting;
            let result = match delivery.channel.as_ref() {
                Some(channel) if channel.frontend == FrontendKind::Tui => {
                    let frontends: Vec<_> = registry
                        .as_ref()
                        .map(|registry| {
                            registry
                                .frontends
                                .iter()
                                .filter(|frontend| frontend.kind() == FrontendKind::Tui)
                                .collect()
                        })
                        .unwrap_or_default();
                    if frontends.is_empty() {
                        Err("TUI frontend is unavailable".to_string())
                    } else {
                        let event = EngineEvent::Text {
                            target: EventTarget::Directed(vec![channel.clone()]),
                            role: MessageRole::System,
                            content: delivery.content.clone(),
                            task_id: Some(delivery.task_id),
                        };
                        for frontend in frontends {
                            frontend.push_event(event.clone());
                        }
                        let _ = receipt_tx.send(DeliveryReceipt {
                            key,
                            attempt,
                            result: Ok(None),
                        });
                        Ok(())
                    }
                }
                Some(channel) => {
                    let tx = receipt_tx.clone();
                    manager
                        .send_with_result(
                            channel.frontend.channel_name().into(),
                            ChannelOutboundMessage {
                                recipient: channel.user_id.clone(),
                                thread_id: channel.thread_id.clone(),
                                content: delivery.content.clone(),
                                parse_mode: None,
                                reply_markup: None,
                                attachments: vec![],
                                message_kind: MessageKind::System,
                            },
                            move |result| {
                                let _ = tx.send(DeliveryReceipt {
                                    key,
                                    attempt,
                                    result,
                                });
                            },
                        )
                        .map_err(|error| error.to_string())
                }
                None => Err("task has no output channel".to_string()),
            };
            match result {
                Err(error) => {
                    warn!(event = "MonitorEnqueueFailed", task_id = %delivery.task_id, session_id = %key.0,
                        sequence = key.1, attempt, error_type = "OutboundQueue", error = %error,
                        "monitor notification was not accepted by outbound queue");
                    let _ = receipt_tx.send(DeliveryReceipt {
                        key,
                        attempt,
                        result: Err(error),
                    });
                }
                Ok(()) => {
                    if !delivery.queued_notified {
                        notify(&mut commands, key, delivery, "queued", now);
                        delivery.queued_notified = true;
                    }
                    info!(event = "MonitorDeliveryQueued", task_id = %delivery.task_id, session_id = %key.0,
                        sequence = key.1, attempt, detection_to_queue_ms = (now - delivery.first_detected).num_milliseconds(),
                        "monitor notification submitted; waiting for platform receipt");
                }
            }
        }
        if let DeliveryState::Finished(ref outcome) = delivery.state {
            if earlier {
                continue;
            }
            let outcome = outcome.clone();
            if let Err(error) = backend.ack_output_events(key.0, delivery.last_sequence) {
                warn!(event = "MonitorOutputAckFailed", task_id = %delivery.task_id, session_id = %key.0,
                    error_type = "ProcessBackend", error = %error, "delivery recorded but backend acknowledgement is pending");
                continue;
            }
            let status = match outcome {
                DeliveryOutcome::Delivered => "delivered".to_string(),
                DeliveryOutcome::Failed(ref error) => format!("failed: {error}"),
                DeliveryOutcome::Terminated => "terminated_before_queue".to_string(),
            };
            if alive {
                notify(&mut commands, key, delivery, &status, now);
            }
            if matches!(outcome, DeliveryOutcome::Failed(_)) {
                commands.spawn(SystemOutputMessage {
                    task_id: delivery.task_id,
                    content: format!(
                        "监控告警发送失败（已重试三次），会话 {}，事件 {}..{}。失败记录已保留。",
                        key.0, key.1, delivery.last_sequence
                    ),
                });
            }
            let record = MonitorDeliveryRecord {
                task_id: delivery.task_id,
                session_id: key.0,
                first_sequence: key.1,
                last_sequence: delivery.last_sequence,
                content: delivery.content.clone(),
                status,
            };
            info!(event = "MonitorDeliveryResolved", task_id = %record.task_id, session_id = %record.session_id,
                sequence = record.first_sequence, last_sequence = record.last_sequence, status = %record.status,
                content_bytes = record.content.len(), detection_to_result_ms = (now - delivery.first_detected).num_milliseconds(),
                "monitor platform result recorded for original task");
            deliveries.pending.remove(&key);
            deliveries.records.push_back(record);
            while deliveries.records.len() > MAX_HISTORY {
                deliveries.records.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channels::{
            Channel, ChannelError, ChannelInboundMessage, ChannelManager, ChannelOutboundMessage,
        },
        contracts::Clock,
        domain::{
            AgentId, ChannelId, FrontendKind, InboxDeliveryMessage, SessionBackend,
            SessionStartRequest, Task, WaitingReason,
        },
        ecs::EntityIndex,
        systems::NativeProcessBackend,
    };
    use async_trait::async_trait;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    struct RecordingChannel(Arc<Mutex<Vec<String>>>);
    #[async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &str {
            "qq"
        }
        async fn send(
            &self,
            message: &ChannelOutboundMessage,
        ) -> Result<Option<String>, ChannelError> {
            self.0.lock().unwrap().push(message.content.clone());
            Ok(None)
        }
        async fn listen(
            &self,
            _tx: crossbeam_channel::Sender<ChannelInboundMessage>,
        ) -> Result<(), ChannelError> {
            std::future::pending().await
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_stdout_reaches_platform_then_owner_inbox_without_waiting_for_exit() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (input_tx, _) = crossbeam_channel::unbounded();
        let (manager, handle, _) =
            ChannelManager::new(vec![Arc::new(RecordingChannel(sent.clone()))], input_tx).unwrap();
        let channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "monitor-test".into(),
            thread_id: None,
        };
        let now = chrono::Utc::now();
        let mut task = Task::from_user_input("monitor", 3, channel);
        task.mark_waiting(WaitingReason::BackgroundJobs, now);
        let task_id = task.id;
        let backend = NativeProcessBackend::default();
        let session = backend
            .start_session(SessionStartRequest {
                command: "printf 'ALERT-1\\n'; sleep 30".into(),
                session_name: None,
                cwd: None,
                env: HashMap::new(),
                timeout_secs: None,
                tail_lines: 20,
                owner_task_id: task_id,
                owner_agent_id: AgentId::new(),
                notify_on_exit: false,
                notify_on_output: true,
            })
            .unwrap();
        let mut app = App::new();
        app.insert_resource(Clock(now));
        app.insert_resource(backend.clone());
        app.insert_resource(manager.clone());
        app.init_resource::<MonitorDeliveries>();
        app.init_resource::<LoopControlConfig>();
        let entity = app.world_mut().spawn(task).id();
        let mut index = EntityIndex::default();
        index.tasks.insert(task_id, entity);
        app.insert_resource(index);
        app.add_systems(Update, monitor_output_system);
        for i in 0..100 {
            app.world_mut().resource_mut::<Clock>().0 =
                now + chrono::Duration::milliseconds(i * 20);
            app.update();
            if !sent.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // 无论断言是否失败都先清理真实进程。
        let running_before_cleanup = !backend.list_task_sessions(task_id).unwrap().is_empty();
        backend.stop_session(session.handle_id).unwrap();
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            app.update();
        }
        manager.shutdown();
        handle.await.unwrap();
        assert!(
            running_before_cleanup,
            "monitor must deliver while the script remains alive"
        );
        assert_eq!(sent.lock().unwrap().as_slice(), ["ALERT-1"]);
        let notifications: Vec<_> = app
            .world_mut()
            .query::<&InboxDeliveryMessage>()
            .iter(app.world())
            .collect();
        assert!(
            notifications
                .iter()
                .any(|delivery| delivery.task_id == task_id
                    && format!("{:?}", delivery.item).contains("delivered")),
            "platform success must return to original task"
        );
    }
    struct RetryChannel {
        attempts: Arc<std::sync::atomic::AtomicUsize>,
        failures: usize,
        gate: Option<Arc<tokio::sync::Notify>>,
    }
    #[async_trait]
    impl Channel for RetryChannel {
        fn name(&self) -> &str {
            "qq"
        }
        async fn send(&self, _: &ChannelOutboundMessage) -> Result<Option<String>, ChannelError> {
            let attempt = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            if attempt < self.failures {
                Err(ChannelError::NotConfigured)
            } else {
                Ok(None)
            }
        }
        async fn listen(
            &self,
            _: crossbeam_channel::Sender<ChannelInboundMessage>,
        ) -> Result<(), ChannelError> {
            std::future::pending().await
        }
    }

    struct DeliveryFixture {
        app: App,
        backend: NativeProcessBackend,
        manager: ChannelManager,
        manager_task: Option<tokio::task::JoinHandle<()>>,
        owner: Entity,
        task_id: TaskId,
        session_id: SessionHandleId,
        now: DateTime<Utc>,
    }
    impl DeliveryFixture {
        fn new(channel: Option<Arc<dyn Channel>>) -> Self {
            let (tx, _) = crossbeam_channel::unbounded();
            let (manager, manager_task) = match channel {
                Some(channel) => {
                    let (manager, task, _) = ChannelManager::new(vec![channel], tx).unwrap();
                    (manager, Some(task))
                }
                None => (ChannelManager::empty().0, None),
            };
            let now = Utc::now();
            let mut task = Task::from_user_input(
                "monitor",
                3,
                ChannelId {
                    frontend: FrontendKind::QQ,
                    user_id: "test-monitor".into(),
                    thread_id: None,
                },
            );
            task.mark_waiting(WaitingReason::BackgroundJobs, now);
            let task_id = task.id;
            let backend = NativeProcessBackend::default();
            let handle = backend
                .start_session(SessionStartRequest {
                    command: "printf 'EVENT\\n'; sleep 30".into(),
                    session_name: None,
                    cwd: None,
                    env: HashMap::new(),
                    timeout_secs: None,
                    tail_lines: 20,
                    owner_task_id: task_id,
                    owner_agent_id: AgentId::new(),
                    notify_on_exit: false,
                    notify_on_output: true,
                })
                .unwrap();
            let mut app = App::new();
            app.insert_resource(Clock(now));
            app.insert_resource(backend.clone());
            app.insert_resource(manager.clone());
            app.insert_resource(LoopControlConfig {
                notification_coalesce_ms: 0,
                background_poll_ms: 0,
                ..Default::default()
            });
            app.init_resource::<MonitorDeliveries>();
            let owner = app.world_mut().spawn(task).id();
            let mut index = EntityIndex::default();
            index.tasks.insert(task_id, owner);
            app.insert_resource(index);
            app.add_systems(Update, monitor_output_system);
            Self {
                app,
                backend,
                manager,
                manager_task,
                owner,
                task_id,
                session_id: handle.handle_id,
                now,
            }
        }
        async fn tick(&mut self, seconds: i64) {
            self.app.world_mut().resource_mut::<Clock>().0 =
                self.now + chrono::Duration::seconds(seconds);
            self.app.update();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        async fn close(self) {
            self.backend.stop_session(self.session_id).unwrap();
            self.manager.shutdown();
            if let Some(task) = self.manager_task {
                task.await.unwrap();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn platform_failures_retry_three_times_then_acknowledge_final_outcome() {
        for failures in [3, usize::MAX] {
            let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let channel = Arc::new(RetryChannel {
                attempts: attempts.clone(),
                failures,
                gate: None,
            });
            let mut f = DeliveryFixture::new(Some(channel));
            for second in 1..25 {
                f.tick(second).await;
                if f.app
                    .world()
                    .resource::<MonitorDeliveries>()
                    .recent_records()
                    .next()
                    .is_some()
                {
                    break;
                }
            }
            let state = f.app.world().resource::<MonitorDeliveries>();
            let record = state
                .recent_records()
                .next()
                .expect("every delivery must reach a final platform outcome");
            assert_eq!(record.task_id, f.task_id);
            assert_eq!(record.status.starts_with("failed"), failures == usize::MAX);
            assert_eq!(
                attempts.load(std::sync::atomic::Ordering::SeqCst),
                4,
                "initial attempt plus three retries"
            );
            assert!(
                f.backend.poll_output_events().unwrap().is_empty(),
                "only terminal outcomes release backend events"
            );
            f.close().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_outbound_queue_never_claims_notification_was_queued() {
        let mut f = DeliveryFixture::new(None);
        for second in 1..25 {
            f.tick(second).await;
            if f.app
                .world()
                .resource::<MonitorDeliveries>()
                .recent_records()
                .next()
                .is_some()
            {
                break;
            }
        }
        let messages: Vec<_> = f
            .app
            .world_mut()
            .query::<&InboxDeliveryMessage>()
            .iter(f.app.world())
            .collect();
        assert!(!messages.iter().any(|delivery| matches!(&delivery.item, InboxItem::TaskNotification { status, .. } if status == "queued")),
            "local queue rejection must not be reported as accepted");
        assert!(
            f.app
                .world()
                .resource::<MonitorDeliveries>()
                .recent_records()
                .any(|record| record.status.starts_with("failed"))
        );
        f.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_platform_receipt_stays_with_ended_task_and_does_not_wake_replacement() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        let channel = Arc::new(RetryChannel {
            attempts: attempts.clone(),
            failures: 0,
            gate: Some(gate.clone()),
        });
        let mut f = DeliveryFixture::new(Some(channel));
        for second in 1..10 {
            f.tick(second).await;
            if attempts.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                break;
            }
        }
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        let old_channel = f
            .app
            .world()
            .get::<Task>(f.owner)
            .unwrap()
            .origin_channel
            .clone()
            .unwrap();
        f.app
            .world_mut()
            .get_mut::<Task>(f.owner)
            .unwrap()
            .mark_done("ended", f.now);
        let replacement = Task::from_user_input("replacement", 3, old_channel);
        let replacement_id = replacement.id;
        let replacement_entity = f.app.world_mut().spawn(replacement).id();
        f.app
            .world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(replacement_id, replacement_entity);
        gate.notify_one();
        for second in 10..20 {
            f.tick(second).await;
        }
        assert!(
            f.app
                .world()
                .resource::<MonitorDeliveries>()
                .recent_records()
                .any(|record| record.task_id == f.task_id && record.status == "delivered")
        );
        assert!(
            !f.app
                .world_mut()
                .query::<&InboxDeliveryMessage>()
                .iter(f.app.world())
                .any(|delivery| delivery.task_id == replacement_id)
        );
        f.close().await;
    }
}
