//! 按需驱动的调度投影：业务状态仍由原系统维护。
use crate::{
    contracts::{Clock, RuntimeDeadlines, RuntimeWake},
    domain::*,
    prelude::*,
};
use bevy_app::{App, Last};
use chrono::{DateTime, Utc};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadlineKey {
    Retry(Entity),
    Wait(Entity),
    Tool(Entity),
    /// 在飞工具调用的前台预算到期（自动转后台）。
    ToolBackground(Entity),
    Inbox(Entity),
    Accounting(Entity),
    Persist,
    Maintenance,
    Background,
    Monitor,
    /// watchdog 巡检节拍。
    Watchdog,
}

/// 时间索引只缓存下一次处理时间，不复制 Task 或 ScheduleDefinition。
#[derive(Resource, Default)]
pub struct DemandSchedule {
    deadlines: RuntimeDeadlines<DeadlineKey>,
    wall: BTreeMap<DeadlineKey, DateTime<Utc>>,
    due: BTreeSet<DeadlineKey>,
    due_wall: BTreeMap<DeadlineKey, DateTime<Utc>>,
    identities: BTreeMap<DeadlineKey, String>,
    due_identities: BTreeMap<DeadlineKey, String>,
    attempts: BTreeMap<DeadlineKey, u32>,
    clock_sample: Option<(DateTime<Utc>, Instant)>,
}
impl DemandSchedule {
    pub(crate) fn sample_clock(&mut self, wall: DateTime<Utc>, monotonic: Instant) {
        self.clock_sample = Some((wall, monotonic));
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.next()
    }
    pub fn take_due(&mut self, now: Instant) -> bool {
        self.due = self.deadlines.take_due(now).into_iter().collect();
        self.due_wall.clear();
        self.due_identities.clear();
        for key in &self.due {
            if let Some(at) = self.wall.remove(key) {
                self.due_wall.insert(*key, at);
            }
            if let Some(id) = self.identities.remove(key) {
                self.due_identities.insert(*key, id);
            }
        }
        !self.due.is_empty()
    }
    pub fn waiters_due(&self) -> impl Iterator<Item = Entity> + '_ {
        self.due.iter().filter_map(|key| match key {
            DeadlineKey::Wait(entity) => Some(*entity),
            _ => None,
        })
    }
    pub fn is_due(&self, key: DeadlineKey) -> bool {
        self.due.contains(&key)
    }
    pub fn is_due_at(&self, key: DeadlineKey, at: DateTime<Utc>) -> bool {
        self.is_due(key) && self.due_wall.get(&key) == Some(&at)
    }
    /// 已登记的相对等待只认单调期限；新请求尚未投影时才检查其墙钟边界。
    pub fn expired_at(&self, key: DeadlineKey, at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        if self.wall.get(&key) == Some(&at) {
            return false;
        }
        self.is_due_at(key, at) || now >= at
    }
    pub fn wait_due(&self, entity: Entity, info: &WaitingForTasksInfo) -> bool {
        self.is_due_at(DeadlineKey::Wait(entity), info.timeout_at)
            && self.due_identities.get(&DeadlineKey::Wait(entity)) == Some(&info.tool_call_id)
    }
    pub fn wait_expired(
        &self,
        entity: Entity,
        info: &WaitingForTasksInfo,
        now: DateTime<Utc>,
    ) -> bool {
        let key = DeadlineKey::Wait(entity);
        if self.identities.get(&key) == Some(&info.tool_call_id)
            && self.wall.get(&key) == Some(&info.timeout_at)
        {
            return false;
        }
        self.wait_due(entity, info) || now >= info.timeout_at
    }
    fn project_wait(&mut self, entity: Entity, info: &WaitingForTasksInfo, clock: DateTime<Utc>) {
        let key = DeadlineKey::Wait(entity);
        if self.identities.get(&key) != Some(&info.tool_call_id) {
            self.clear(key);
        }
        self.project(key, info.timeout_at, clock);
        self.identities.insert(key, info.tool_call_id.clone());
    }
    pub(crate) fn background_ready(
        &mut self,
        now: DateTime<Utc>,
        interval: chrono::Duration,
    ) -> bool {
        self.periodic_ready(DeadlineKey::Background, now, interval)
    }
    /// 周期性工作的节拍：未登记或已到期时登记下一次并返回 true。
    pub(crate) fn periodic_ready(
        &mut self,
        key: DeadlineKey,
        now: DateTime<Utc>,
        interval: chrono::Duration,
    ) -> bool {
        if self.wall.get(&key).is_some_and(|at| *at > now) && !self.is_due(key) {
            return false;
        }
        let at = now
            .checked_add_signed(interval)
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        self.project(key, at, now);
        true
    }
    pub fn defer(&mut self, key: DeadlineKey, delay: Duration) {
        self.deadlines.set(key, Instant::now() + delay);
    }
    pub fn retry(&mut self, key: DeadlineKey) {
        let attempt = self.attempts.entry(key).or_default();
        let delay = Duration::from_secs(1 << (*attempt).min(5));
        *attempt = attempt.saturating_add(1);
        self.defer(key, delay);
    }
    pub fn blocked(&self, key: DeadlineKey) -> bool {
        self.attempts.contains_key(&key) && !self.due.contains(&key)
    }
    pub fn clear(&mut self, key: DeadlineKey) {
        self.deadlines.remove(key);
        self.wall.remove(&key);
        self.attempts.remove(&key);
        self.identities.remove(&key);
    }
    fn project(&mut self, key: DeadlineKey, at: DateTime<Utc>, clock: DateTime<Utc>) {
        if self.wall.get(&key) == Some(&at) {
            return;
        }
        let delay = (at - clock).to_std().unwrap_or_default();
        let origin = self
            .clock_sample
            .filter(|(wall, _)| *wall == clock)
            .map_or_else(Instant::now, |(_, monotonic)| monotonic);
        if let Some(deadline) = origin.checked_add(delay) {
            self.deadlines.set(key, deadline);
            self.wall.insert(key, at);
        }
    }
}

/// 有限的跨轮生产者集合；Clock、统计和长期记忆分数不驱动续跑。
fn continue_changed<T: Component>(query: Query<(), Changed<T>>, wake: Res<RuntimeWake>) {
    if !query.is_empty() {
        wake.notify();
    }
}
fn continue_removed<T: Component>(mut removed: RemovedComponents<T>, wake: Res<RuntimeWake>) {
    if removed.read().count() > 0 {
        wake.notify();
    }
}
fn continue_scheduler_change(state: Res<crate::triggers::SchedulerState>, wake: Res<RuntimeWake>) {
    if state.is_changed() {
        wake.notify();
    }
}

/// 在 Update 的 Commands 全部提交后登记下一轮与最近期限。
pub(crate) fn install_demand_drive(app: &mut App) {
    app.init_resource::<DemandSchedule>();
    macro_rules! track { ($($ty:ty),* $(,)?) => { $(
        app.add_systems(Last, continue_changed::<$ty>);
    )* }; }
    track!(
        Task,
        WaitingForTasksInfo,
        WorkItem,
        Agent,
        PendingDispatch,
        ToolCallingState,
        ShortTermMemory,
        TaskInbox,
        DeferredResult,
        Signal,
        UserInputMessage,
        InteractionRequest,
        ScheduleRequest,
        RetryReadyMessage,
        TriggerTaskMessage,
        AgentExecutionRequestMessage,
        AgentExecutionResultMessage,
        UserOutputMessage,
        SystemOutputMessage,
        ContinueTaskMessage,
        TaskTerminatedMessage,
        FinishTaskMessage,
        ClearTaskMessage,
        ToolExecutionRequestMessage,
        ToolExecutionResultMessage,
        ToolConfirmationRequestMessage,
        ToolConfirmationResponseMessage,
        ApprovalRequestMessage,
        ApprovalResultMessage,
        AgentSpawnRequestMessage,
        SubTaskBatchCreatedMessage,
        SubTaskCompletedMessage,
        ChatRoundStartedMessage,
        ChatRoundReadyMessage,
        SummarizationRequestMessage,
        ExperienceLookupRequest,
        ExperienceDraftRequest,
        ExperienceDraftReadyMessage,
        ToolEffectPending,
        PendingChannelSend,
        InboxDeliveryMessage,
        BackgroundJobControl,
        StopTaskMessage,
        StopRequested,
        BackgroundJob,
        MessageDispatchedHookPending,
        MessageReceivedHookPending,
        LlmResponseHookPending,
        ApprovalRequestedHookPending,
        ApprovalResolvedHookPending,
        NewlyCreatedTask,
        ToolCalledHookPending,
        ToolReturnedHookPending,
        AgentStoppingHookPending,
        LtmWriteHookPending,
        LtmEvictedHookPending,
        WorkItemLifecycleHookPending,
        crate::systems::ScheduleRunRecorded,
        crate::systems::schedule::PendingScheduleRecord
    );
    app.add_systems(
        Last,
        (
            continue_removed::<BackgroundJob>,
            continue_removed::<WorkItem>,
            continue_removed::<InFlightToolCall>,
            continue_removed::<Task>,
            continue_scheduler_change,
            project_deadlines,
        ),
    );
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn project_deadlines(
    clock: Res<Clock>,
    config: Res<LoopControlConfig>,
    mut schedule: ResMut<DemandSchedule>,
    tasks: Query<(Entity, &Task), Changed<Task>>,
    waits: Query<(Entity, &WaitingForTasksInfo), Changed<WaitingForTasksInfo>>,
    tools: Query<(Entity, &InFlightToolCall), Changed<InFlightToolCall>>,
    backgrounded: Query<Entity, Added<BackgroundedToolCall>>,
    inboxes: Query<(Entity, &Task, &TaskInbox, Option<&AutoWakeBudget>), Without<AskUserPending>>,
    jobs: Query<&BackgroundJob>,
    monitor: Res<crate::systems::MonitorDeliveries>,
    mut removed_tasks: RemovedComponents<Task>,
    mut removed_waits: RemovedComponents<WaitingForTasksInfo>,
    mut removed_tools: RemovedComponents<InFlightToolCall>,
    mut removed_inboxes: RemovedComponents<TaskInbox>,
    mut removed_records: RemovedComponents<crate::systems::schedule::PendingScheduleRecord>,
) {
    for entity in removed_records.read() {
        schedule.clear(DeadlineKey::Accounting(entity));
    }
    for entity in removed_tasks.read() {
        for key in [
            DeadlineKey::Retry(entity),
            DeadlineKey::Inbox(entity),
            DeadlineKey::Accounting(entity),
        ] {
            schedule.clear(key);
        }
    }
    for entity in removed_waits.read() {
        schedule.clear(DeadlineKey::Wait(entity));
    }
    for entity in removed_tools.read() {
        schedule.clear(DeadlineKey::Tool(entity));
        schedule.clear(DeadlineKey::ToolBackground(entity));
    }
    for entity in removed_inboxes.read() {
        schedule.clear(DeadlineKey::Inbox(entity));
    }
    for (entity, task) in &tasks {
        if task.status() == &TaskStatus::Waiting(WaitingReason::RetryBackoff)
            && let Some(at) = task.next_retry_at
        {
            schedule.project(DeadlineKey::Retry(entity), at, clock.0);
        } else {
            schedule.clear(DeadlineKey::Retry(entity));
        }
    }
    for (entity, wait) in &waits {
        schedule.project_wait(entity, wait, clock.0);
    }
    for (entity, call) in &tools {
        schedule.project(
            DeadlineKey::Tool(entity),
            call.started_at + call.timeout + chrono::Duration::milliseconds(1),
            clock.0,
        );
        if let Some(at) = call.background_at {
            schedule.project(DeadlineKey::ToolBackground(entity), at, clock.0);
        }
    }
    for entity in &backgrounded {
        schedule.clear(DeadlineKey::ToolBackground(entity));
    }
    for (entity, task, inbox, budget) in &inboxes {
        let key = DeadlineKey::Inbox(entity);
        if inbox.is_empty()
            || !matches!(
                task.status(),
                TaskStatus::Waiting(WaitingReason::User | WaitingReason::BackgroundJobs)
            )
            || task.pending_confirmation_id.is_some()
            || inbox.has_user_steer()
        {
            schedule.clear(key);
            continue;
        }
        let Some(first) = inbox.first_received_at() else {
            continue;
        };
        let mut at = first
            .checked_add_signed(chrono::Duration::milliseconds(
                config.notification_coalesce_ms.min(i64::MAX as u64) as i64,
            ))
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        if let Some(budget) = budget
            && budget.recent_wakes.len() >= config.max_auto_wakes as usize
        {
            if config.max_auto_wakes == 0 {
                schedule.clear(key);
                continue;
            }
            if let Some(oldest) = budget.recent_wakes.front() {
                at = at.max(
                    oldest
                        .checked_add_signed(
                            chrono::Duration::try_seconds(
                                config.auto_wake_window_secs.max(1).min(i64::MAX as u64) as i64,
                            )
                            .unwrap_or(chrono::Duration::MAX),
                        )
                        .unwrap_or(DateTime::<Utc>::MAX_UTC),
                );
            }
        }
        // 到期却仍等待其他条件时不能零间隔忙等；其条件变化会触发一轮。
        if at > clock.0 {
            schedule.project(key, at, clock.0);
        } else {
            schedule.clear(key);
        }
    }
    if !schedule.wall.contains_key(&DeadlineKey::Maintenance) {
        schedule.project(
            DeadlineKey::Maintenance,
            clock.0 + chrono::Duration::seconds(30),
            clock.0,
        );
    }
    // 当前 supervisor 的 IPC 是请求/响应，显式兼容轮询仅在存在后台任务时登记。
    if jobs
        .iter()
        .any(|job| matches!(job.source, JobSource::Shell { .. }))
    {
        if !schedule.wall.contains_key(&DeadlineKey::Background) {
            schedule.project(
                DeadlineKey::Background,
                clock.0
                    + chrono::Duration::milliseconds(
                        config.background_poll_ms.max(1).min(i64::MAX as u64) as i64,
                    ),
                clock.0,
            );
        }
    } else {
        schedule.clear(DeadlineKey::Background);
    }
    if let Some(at) = monitor.next_deadline(clock.0) {
        // 顺序受阻的批次等待前批回执，不以过去的 deadline 忙等。
        if at > clock.0 {
            schedule.project(DeadlineKey::Monitor, at, clock.0);
        } else {
            schedule.clear(DeadlineKey::Monitor);
        }
    } else {
        schedule.clear(DeadlineKey::Monitor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_projection_does_not_add_frame_processing_time() {
        let wall = Utc::now();
        let sampled_at = Instant::now() - Duration::from_secs(10);
        let mut schedule = DemandSchedule::default();
        schedule.sample_clock(wall, sampled_at);
        schedule.project(
            DeadlineKey::Monitor,
            wall + chrono::Duration::seconds(1),
            wall,
        );
        assert_eq!(
            schedule.next_deadline(),
            Some(sampled_at + Duration::from_secs(1))
        );
    }

    #[test]
    fn registered_timeout_survives_wall_clock_jumps() {
        let entity = World::new().spawn_empty().id();
        let now = Utc::now();
        let at = now + chrono::Duration::minutes(1);
        let key = DeadlineKey::Retry(entity);
        let mut schedule = DemandSchedule::default();
        schedule.project(key, at, now);
        assert!(!schedule.expired_at(key, at, now + chrono::Duration::hours(2)));
        schedule.take_due(Instant::now() + Duration::from_secs(61));
        assert!(schedule.expired_at(key, at, now - chrono::Duration::hours(2)));
    }

    #[test]
    fn an_old_timeout_cannot_complete_a_replacement_wait() {
        let entity = World::new().spawn_empty().id();
        let now = Utc::now();
        let mut info = WaitingForTasksInfo {
            target_task_ids: vec![TaskId::new()],
            timeout_at: now + chrono::Duration::milliseconds(10),
            tool_call_id: "old".into(),
            agent_id: AgentId::new(),
        };
        let mut schedule = DemandSchedule::default();
        schedule.project_wait(entity, &info, now);
        schedule.take_due(Instant::now() + Duration::from_secs(1));
        assert!(schedule.wait_due(entity, &info));
        info.tool_call_id = "replacement".into();
        assert!(!schedule.wait_due(entity, &info));
        info.timeout_at += chrono::Duration::hours(1);
        assert!(!schedule.wait_due(entity, &info));
    }

    #[test]
    fn storage_retry_is_bounded_and_other_keys_are_not_blocked() {
        let mut schedule = DemandSchedule::default();
        schedule.retry(DeadlineKey::Persist);
        assert!(schedule.blocked(DeadlineKey::Persist));
        assert!(!schedule.blocked(DeadlineKey::Maintenance));
        schedule.take_due(Instant::now() + Duration::from_secs(2));
        assert!(!schedule.blocked(DeadlineKey::Persist));
        schedule.clear(DeadlineKey::Persist);
        assert_eq!(schedule.next_deadline(), None);
    }

    #[test]
    fn unchanged_component_does_not_keep_driver_running() {
        let mut app = App::new();
        app.init_resource::<RuntimeWake>();
        app.add_systems(Last, continue_changed::<StopRequested>);
        app.world_mut().spawn(StopRequested);
        app.update();
        assert!(app.world().resource::<RuntimeWake>().take().runtime);
        app.update();
        assert!(!app.world().resource::<RuntimeWake>().take().runtime);
    }
}
