//! 客户端计划管理；归属校验、持久化与统一调度器通知在 ECS 线程提交。
use crate::{
    contracts::{Clock, FrontendRegistry},
    domain::{
        ChannelId, EngineEvent, EventTarget, FrontendKind, InputSequence, ScheduleAction,
        ScheduleDefinition, ScheduleExpr, ScheduleInput, ScheduleRequest, ScheduleSpec,
    },
    infrastructure::schedules::ScheduleStore,
    prelude::*,
    triggers::{SchedulerState, SchedulerStateWatcher, update_scheduler_state_with_watcher},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Resource, Default)]
pub(crate) struct ScheduleSubscriptions(HashSet<ChannelId>);

fn owned(def: &ScheduleDefinition, channel: &ChannelId) -> bool {
    def.output_channel
        .as_ref()
        .is_some_and(|c| c.frontend == FrontendKind::Client && c.user_id == channel.user_id)
}
fn revision(def: &ScheduleDefinition) -> String {
    let fields = json!([
        def.id,
        def.name,
        def.content,
        def.schedule,
        def.enabled,
        def.notify,
        def.output_channel
    ]);
    format!("{:x}", Sha256::digest(fields.to_string().as_bytes()))
}
fn snapshot(state: &SchedulerState, channel: &ChannelId) -> Value {
    let schedules:Vec<_>=state.dynamic_tasks().iter().filter(|d|owned(d,channel)).map(|d|json!({
        "id":d.id,"name":d.name,"content":d.content,"schedule":d.schedule,"enabled":d.enabled,"notify":d.notify,
        "session_id":d.output_channel.as_ref().and_then(|c|c.thread_id.clone()),"revision":revision(d),"health":d.health,
        "next_trigger":if d.enabled {crate::domain::compute_next_trigger(d.schedule.spec())}else{None},
    })).collect();
    json!({"available":true,"schedules":schedules,"timezone":format!("server-local {}",chrono::Local::now().offset())})
}
fn validate(
    input: &ScheduleInput,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ScheduleExpr, String> {
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.content.trim().is_empty()
        || input.content.len() > 32 * 1024
    {
        return Err("invalid_schedule_input".into());
    }
    let schedule =
        ScheduleExpr::parse(&input.schedule).map_err(|_| "invalid_schedule".to_string())?;
    if input.enabled && matches!(schedule.spec(),ScheduleSpec::Once(at) if *at<=now) {
        return Err("schedule_in_past".into());
    }
    Ok(schedule)
}

fn apply(
    request: &ScheduleRequest,
    clock: &Clock,
    state: &mut SchedulerState,
    watcher: &SchedulerStateWatcher,
    store: Option<&ScheduleStore>,
) -> Result<Option<Value>, String> {
    if request.channel.frontend != FrontendKind::Client {
        return Err("invalid_channel".into());
    }
    if let ScheduleAction::List = &request.action {
        return Ok(None);
    }
    let store = store.ok_or_else(|| "schedule_storage_unavailable".to_string())?;
    let mut defs = state.dynamic_tasks().to_vec();
    match &request.action {
        ScheduleAction::List => unreachable!(),
        ScheduleAction::Create { input } => {
            if defs.len() >= 256 {
                return Err("schedule_capacity".into());
            }
            if defs
                .iter()
                .any(|d| d.name == input.name || d.id == request.command_id)
            {
                return Err("schedule_name_unavailable".into());
            }
            let expr = validate(input, clock.0)?;
            let mut def = ScheduleDefinition::new(
                request.command_id,
                Some(input.name.clone()),
                input.content.clone(),
                expr,
                Some(request.channel.clone()),
                clock.0,
            );
            def.enabled = input.enabled;
            def.notify = input.notify;
            defs.push(def);
        }
        ScheduleAction::Update {
            schedule_id,
            expected_revision,
            input,
        } => {
            let def = defs
                .iter_mut()
                .find(|d| d.id == *schedule_id && owned(d, &request.channel))
                .ok_or_else(|| "schedule_not_found".to_string())?;
            if &revision(def) != expected_revision {
                return Err("schedule_changed".into());
            }
            let expr = validate(input, clock.0)?;
            def.name = input.name.clone();
            def.content = input.content.clone();
            def.schedule = expr;
            def.enabled = input.enabled;
            def.notify = input.notify;
            if defs
                .iter()
                .any(|d| d.id != *schedule_id && d.name == input.name)
            {
                return Err("schedule_name_unavailable".into());
            }
        }
        ScheduleAction::Delete {
            schedule_id,
            expected_revision,
        } => {
            let def = defs
                .iter()
                .find(|d| d.id == *schedule_id && owned(d, &request.channel))
                .ok_or_else(|| "schedule_not_found".to_string())?;
            if &revision(def) != expected_revision {
                return Err("schedule_changed".into());
            }
            defs.retain(|d| d.id != *schedule_id);
        }
        ScheduleAction::History { schedule_id } => {
            if !defs
                .iter()
                .any(|d| d.id == *schedule_id && owned(d, &request.channel))
                && store
                    .client_owner(*schedule_id)
                    .map_err(|_| "schedule_storage_unavailable".to_string())?
                    .as_deref()
                    != Some(&request.channel.user_id)
            {
                return Err("schedule_not_found".into());
            }
            let records = store.recent_runs(*schedule_id, 50).map_err(|error| {
                tracing::error!(event="ClientScheduleHistoryFailed",%error);
                "schedule_storage_unavailable".to_string()
            })?;
            return Ok(Some(json!({"schedule_id":schedule_id,"records":records})));
        }
    }
    store.save(&defs).map_err(|error| {
        tracing::error!(event="ClientScheduleSaveFailed",%error);
        "schedule_storage_unavailable".to_string()
    })?;
    update_scheduler_state_with_watcher(state, watcher, |state| *state.dynamic_tasks_mut() = defs);
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn client_schedule_request_system(
    mut commands: Commands,
    clock: Res<Clock>,
    registry: Res<FrontendRegistry>,
    requests: Query<(Entity, &ScheduleRequest, Option<&InputSequence>)>,
    mut state: ResMut<SchedulerState>,
    watcher: Res<SchedulerStateWatcher>,
    store: Option<Res<ScheduleStore>>,
    mut subscriptions: ResMut<ScheduleSubscriptions>,
) {
    let mut requests: Vec<_> = requests.iter().collect();
    requests.sort_by_key(|(e, _, s)| s.map_or(e.to_bits(), |s| s.0));
    for (entity, request, _) in requests {
        commands.entity(entity).despawn();
        let result = apply(
            request,
            &clock,
            state.bypass_change_detection(),
            &watcher,
            store.as_deref(),
        );
        if result.is_ok()
            && !matches!(
                request.action,
                ScheduleAction::List | ScheduleAction::History { .. }
            )
        {
            state.set_changed();
        }
        if let Ok(history) = &result {
            subscriptions.0.insert(request.channel.clone());
            let mut data = snapshot(&state, &request.channel);
            if let Some(history) = history {
                data["history"] = history.clone();
            }
            let event = EngineEvent::AutomationSnapshot {
                target: EventTarget::Directed(vec![request.channel.clone()]),
                snapshot: data,
            };
            for frontend in &registry.frontends {
                frontend.push_event(event.clone());
            }
        }
        let event = EngineEvent::CommandResult {
            target: EventTarget::Directed(vec![request.channel.clone()]),
            command_id: request.command_id,
            outcome: result.map(|_| ()),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }
}

pub(crate) fn client_schedule_snapshot_system(
    state: Res<SchedulerState>,
    subscriptions: Res<ScheduleSubscriptions>,
    registry: Res<FrontendRegistry>,
) {
    if !state.is_changed() {
        return;
    }
    for channel in &subscriptions.0 {
        let event = EngineEvent::AutomationSnapshot {
            target: EventTarget::Directed(vec![channel.clone()]),
            snapshot: snapshot(&state, channel),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    fn channel(user: &str) -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Client,
            user_id: user.into(),
            thread_id: Some(Uuid::nil().to_string()),
        }
    }
    fn input() -> ScheduleInput {
        ScheduleInput {
            name: "daily".into(),
            content: "check status".into(),
            schedule: "cron:0 8 * * *".into(),
            enabled: true,
            notify: Default::default(),
        }
    }
    fn request(user: &str, action: ScheduleAction) -> ScheduleRequest {
        ScheduleRequest {
            channel: channel(user),
            command_id: Uuid::new_v4(),
            action,
        }
    }
    #[test]
    fn edits_persist_and_notify_scheduler_and_deleted_history_keeps_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(dir.path().join("schedules.json"));
        let mut state = SchedulerState::default();
        let (tx, mut rx) = tokio::sync::watch::channel(state.clone());
        let watcher = SchedulerStateWatcher(Some(tx));
        let clock = Clock::default();
        let create = request("alice", ScheduleAction::Create { input: input() });
        apply(&create, &clock, &mut state, &watcher, Some(&store)).unwrap();
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        let id = create.command_id;
        assert_eq!(store.load().unwrap()[0].id, id);
        let original = state.dynamic_tasks()[0].clone();
        let mut paused = input();
        paused.enabled = false;
        apply(
            &request(
                "alice",
                ScheduleAction::Update {
                    schedule_id: id,
                    expected_revision: revision(&original),
                    input: paused,
                },
            ),
            &clock,
            &mut state,
            &watcher,
            Some(&store),
        )
        .unwrap();
        assert!(rx.has_changed().unwrap());
        assert!(!store.load().unwrap()[0].enabled);
        let stale = apply(
            &request(
                "alice",
                ScheduleAction::Delete {
                    schedule_id: id,
                    expected_revision: revision(&original),
                },
            ),
            &clock,
            &mut state,
            &watcher,
            Some(&store),
        );
        assert_eq!(stale.unwrap_err(), "schedule_changed");
        let rev = revision(&state.dynamic_tasks()[0]);
        apply(
            &request(
                "alice",
                ScheduleAction::Delete {
                    schedule_id: id,
                    expected_revision: rev,
                },
            ),
            &clock,
            &mut state,
            &watcher,
            Some(&store),
        )
        .unwrap();
        assert!(state.dynamic_tasks().is_empty());
        assert!(store.load().unwrap().is_empty());
        assert_eq!(store.client_owner(id).unwrap().as_deref(), Some("alice"));
        assert!(
            apply(
                &request("alice", ScheduleAction::History { schedule_id: id }),
                &clock,
                &mut state,
                &watcher,
                Some(&store)
            )
            .is_ok()
        );
        assert_eq!(
            apply(
                &request("bob", ScheduleAction::History { schedule_id: id }),
                &clock,
                &mut state,
                &watcher,
                Some(&store)
            )
            .unwrap_err(),
            "schedule_not_found"
        );
    }
    #[test]
    fn io_failure_does_not_publish_or_modify_in_memory_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "keep").unwrap();
        let store = ScheduleStore::new(blocked.join("schedules.json"));
        let mut state = SchedulerState::default();
        let (tx, rx) = tokio::sync::watch::channel(state.clone());
        let watcher = SchedulerStateWatcher(Some(tx));
        let result = apply(
            &request("alice", ScheduleAction::Create { input: input() }),
            &Clock::default(),
            &mut state,
            &watcher,
            Some(&store),
        );
        assert_eq!(result.unwrap_err(), "schedule_storage_unavailable");
        assert!(state.dynamic_tasks().is_empty());
        assert!(!rx.has_changed().unwrap());
        assert_eq!(std::fs::read_to_string(blocked).unwrap(), "keep");
    }
    #[test]
    fn view_and_mutations_are_scoped_to_authenticated_user() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScheduleStore::new(dir.path().join("schedules.json"));
        let mut state = SchedulerState::default();
        let watcher = SchedulerStateWatcher::default();
        let clock = Clock::default();
        let create = request("alice", ScheduleAction::Create { input: input() });
        apply(&create, &clock, &mut state, &watcher, Some(&store)).unwrap();
        assert!(
            snapshot(&state, &channel("bob"))["schedules"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let rev = revision(&state.dynamic_tasks()[0]);
        assert_eq!(
            apply(
                &request(
                    "bob",
                    ScheduleAction::Delete {
                        schedule_id: create.command_id,
                        expected_revision: rev
                    }
                ),
                &clock,
                &mut state,
                &watcher,
                Some(&store)
            )
            .unwrap_err(),
            "schedule_not_found"
        );
        assert_eq!(store.load().unwrap().len(), 1);
    }
}
