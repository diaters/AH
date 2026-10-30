//! 在统一输入顺序之后校验目标，复用现有停止、结束与工具结果路径。
use crate::{
    contracts::{Clock, FrontendRegistry},
    domain::{
        AskUserPending, EngineEvent, EventTarget, FinishTaskMessage, FrontendKind, InputBlocked,
        InteractionRequest, StopTaskMessage, Task, TaskId, TaskInteraction, TaskStatus,
        TurnGeneration, WaitingReason,
    },
    prelude::*,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub(crate) fn revision(
    task: &Task,
    generation: Option<&TurnGeneration>,
    question: Option<&AskUserPending>,
) -> String {
    let state = format!(
        "{}|{:?}|{}|{}|{:?}|{:?}",
        task.id,
        task.status(),
        task.updated_at,
        generation.map_or(0, |g| g.0),
        task.pending_confirmation_id,
        question.map(|p| &p.tool_call_id)
    );
    format!("{:x}", Sha256::digest(state.as_bytes()))
}

fn available(task: &Task) -> (bool, bool) {
    let active = task.parent_task_id.is_none() && !task.status().is_terminal();
    (
        active
            && !(task.status() == &TaskStatus::Waiting(WaitingReason::User)
                && task.pending_confirmation_id.is_none()),
        active,
    )
}

pub(crate) fn interaction_request_system(
    mut commands: Commands,
    clock: Res<Clock>,
    registry: Res<FrontendRegistry>,
    requests: Query<(Entity, &InteractionRequest), Without<InputBlocked>>,
    mut tasks: Query<(
        Entity,
        &mut Task,
        Option<&TurnGeneration>,
        Option<&AskUserPending>,
    )>,
) {
    let mut handled = HashSet::new();
    for (entity, request) in &requests {
        commands.entity(entity).despawn();
        let task_id = match &request.interaction {
            TaskInteraction::Stop { task_id, .. }
            | TaskInteraction::Finish { task_id, .. }
            | TaskInteraction::Answer { task_id, .. } => *task_id,
        };
        let outcome = (|| {
            if request.channel.frontend != FrontendKind::Client {
                return Err("invalid_channel".into());
            }
            let (task_entity, mut task, generation, question) = tasks
                .iter_mut()
                .find(|(_, task, _, _)| {
                    task.id == task_id && task.origin_channel.as_ref() == Some(&request.channel)
                })
                .ok_or_else(|| "task_not_found".to_string())?;
            if task.status().is_terminal() {
                return Err("task_ended".into());
            }
            if !handled.insert(task_id) {
                return Err("task_changed".into());
            }
            match &request.interaction {
                TaskInteraction::Stop {
                    expected_revision, ..
                }
                | TaskInteraction::Finish {
                    expected_revision, ..
                } => {
                    if &revision(&task, generation, question) != expected_revision {
                        return Err("task_changed".into());
                    }
                    let (stop, finish) = available(&task);
                    match &request.interaction {
                        TaskInteraction::Stop { .. } if stop => {
                            commands.spawn(StopTaskMessage { task_id });
                        }
                        TaskInteraction::Finish { .. } if finish => {
                            commands.spawn(FinishTaskMessage { task_id });
                        }
                        _ => return Err("operation_unavailable".into()),
                    }
                }
                TaskInteraction::Answer {
                    question_id,
                    answer,
                    ..
                } => {
                    let pending = question
                        .filter(|p| &p.tool_call_id == question_id)
                        .filter(|_| task.status() == &TaskStatus::Waiting(WaitingReason::AskUser))
                        .ok_or_else(|| "question_expired".to_string())?;
                    if answer.trim().is_empty() || answer.len() > 32 * 1024 {
                        return Err("invalid_answer".into());
                    }
                    super::routing::complete_ask_user(
                        &mut commands,
                        task_entity,
                        &mut task,
                        pending,
                        answer,
                        &clock,
                    );
                }
            }
            Ok(())
        })();
        let event = EngineEvent::CommandResult {
            target: EventTarget::Directed(vec![request.channel.clone()]),
            command_id: request.command_id,
            outcome,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn interaction_state_system(
    registry: Res<FrontendRegistry>,
    tasks: Query<
        (&Task, Option<&TurnGeneration>, Option<&AskUserPending>),
        Or<(
            Changed<Task>,
            Changed<TurnGeneration>,
            Added<AskUserPending>,
        )>,
    >,
    mut last: Local<HashMap<TaskId, String>>,
) {
    // 没有原生前端时不引入额外快照、散列和广播工作。
    if !registry.has_frontend(FrontendKind::Client) {
        return;
    }
    for (task, generation, question) in &tasks {
        let Some(channel) = task
            .origin_channel
            .as_ref()
            .filter(|c| c.frontend == FrontendKind::Client)
        else {
            continue;
        };
        let rev = revision(task, generation, question);
        if last.get(&task.id) == Some(&rev) {
            continue;
        }
        last.insert(task.id, rev.clone());
        let (can_stop, can_finish) = available(task);
        let event = EngineEvent::TaskInteractionChanged {
            target: EventTarget::Directed(vec![channel.clone()]),
            task_id: task.id,
            revision: rev,
            question_id: question.map(|p| p.tool_call_id.clone()),
            can_stop,
            can_finish,
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
        if task.status().is_terminal() {
            last.remove(&task.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AgentId, ChannelId, Frontend, ToolExecutionResultMessage, UserAction};
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    struct Capture(Arc<Mutex<Vec<EngineEvent>>>);
    impl Frontend for Capture {
        fn kind(&self) -> FrontendKind {
            FrontendKind::Client
        }
        fn push_event(&self, event: EngineEvent) {
            self.0.lock().unwrap().push(event);
        }
        fn poll_actions(&self) -> Vec<UserAction> {
            vec![]
        }
    }
    fn channel(user: &str) -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Client,
            user_id: user.into(),
            thread_id: Some("session".into()),
        }
    }
    fn setup() -> (App, Arc<Mutex<Vec<EngineEvent>>>) {
        let events = Arc::new(Mutex::new(vec![]));
        let mut app = App::new();
        app.init_resource::<Clock>();
        app.insert_resource(FrontendRegistry {
            frontends: vec![Box::new(Capture(events.clone()))],
        });
        app.add_systems(Update, interaction_request_system);
        (app, events)
    }
    fn request(app: &mut App, channel: ChannelId, interaction: TaskInteraction) -> Uuid {
        let id = Uuid::new_v4();
        app.world_mut().spawn(InteractionRequest {
            channel,
            command_id: id,
            interaction,
        });
        id
    }
    fn outcome(events: &Arc<Mutex<Vec<EngineEvent>>>, id: Uuid) -> Result<(), String> {
        events
            .lock()
            .unwrap()
            .iter()
            .find_map(|e| match e {
                EngineEvent::CommandResult {
                    command_id,
                    outcome,
                    ..
                } if *command_id == id => Some(outcome.clone()),
                _ => None,
            })
            .expect("runtime receipt")
    }
    #[test]
    fn stale_and_cross_user_controls_cannot_stop_another_task() {
        let (mut app, events) = setup();
        let task = Task::from_user_input("work", 0, channel("alice"));
        let task_id = task.id;
        let rev = revision(&task, None, None);
        let entity = app.world_mut().spawn(task).id();
        let foreign = request(
            &mut app,
            channel("bob"),
            TaskInteraction::Stop {
                task_id,
                expected_revision: rev.clone(),
            },
        );
        app.update();
        assert_eq!(outcome(&events, foreign), Err("task_not_found".into()));
        app.world_mut().entity_mut(entity).insert(TurnGeneration(1));
        let stale = request(
            &mut app,
            channel("alice"),
            TaskInteraction::Stop {
                task_id,
                expected_revision: rev,
            },
        );
        app.update();
        assert_eq!(outcome(&events, stale), Err("task_changed".into()));
        assert_eq!(
            app.world_mut()
                .query::<&StopTaskMessage>()
                .iter(app.world())
                .count(),
            0
        );
        let rev = revision(
            app.world().get::<Task>(entity).unwrap(),
            Some(&TurnGeneration(1)),
            None,
        );
        let valid = request(
            &mut app,
            channel("alice"),
            TaskInteraction::Stop {
                task_id,
                expected_revision: rev.clone(),
            },
        );
        let duplicate = request(
            &mut app,
            channel("alice"),
            TaskInteraction::Stop {
                task_id,
                expected_revision: rev,
            },
        );
        app.update();
        assert!(outcome(&events, valid).is_ok());
        assert!(outcome(&events, duplicate).is_err());
        assert_eq!(
            app.world_mut()
                .query::<&StopTaskMessage>()
                .iter(app.world())
                .count(),
            1
        );
    }
    #[test]
    fn answers_match_original_question_and_produce_one_original_tool_result() {
        let (mut app, events) = setup();
        let mut task = Task::from_user_input("work", 0, channel("alice"));
        let task_id = task.id;
        task.mark_waiting(WaitingReason::AskUser, chrono::Utc::now());
        let entity = app
            .world_mut()
            .spawn((
                task,
                AskUserPending {
                    tool_call_id: "question-new".into(),
                    agent_id: AgentId::new(),
                },
            ))
            .id();
        let stale = request(
            &mut app,
            channel("alice"),
            TaskInteraction::Answer {
                task_id,
                question_id: "question-old".into(),
                answer: "late answer".into(),
            },
        );
        app.update();
        assert_eq!(outcome(&events, stale), Err("question_expired".into()));
        assert!(app.world().get::<AskUserPending>(entity).is_some());
        let valid = request(
            &mut app,
            channel("alice"),
            TaskInteraction::Answer {
                task_id,
                question_id: "question-new".into(),
                answer: "answer".into(),
            },
        );
        app.update();
        assert!(outcome(&events, valid).is_ok());
        assert!(app.world().get::<AskUserPending>(entity).is_none());
        let result = app
            .world_mut()
            .query::<&ToolExecutionResultMessage>()
            .single(app.world())
            .unwrap();
        assert_eq!(result.tool_call_id.as_deref(), Some("question-new"));
        assert_eq!(result.tool_output.as_ref().unwrap()["answer"], "answer");
    }
    #[test]
    fn ended_task_does_not_release_control_to_new_task_in_same_session() {
        let (mut app, events) = setup();
        let mut old = Task::from_user_input("old", 0, channel("alice"));
        let old_id = old.id;
        old.mark_done("done", chrono::Utc::now());
        app.world_mut().spawn(old);
        let new = Task::from_user_input("new", 0, channel("alice"));
        let new_id = new.id;
        app.world_mut().spawn(new);
        let id = request(
            &mut app,
            channel("alice"),
            TaskInteraction::Finish {
                task_id: old_id,
                expected_revision: "old".into(),
            },
        );
        app.update();
        assert_eq!(outcome(&events, id), Err("task_ended".into()));
        assert!(
            !app.world_mut()
                .query::<&FinishTaskMessage>()
                .iter(app.world())
                .any(|m| m.task_id == new_id)
        );
    }

    #[test]
    fn structured_commands_share_text_fifo_barrier() {
        use crate::domain::{InputSequence, UserInputMessage};
        let (mut app, _) = setup();
        app.add_systems(
            Update,
            super::super::conversation::conversation_input_order_system
                .before(interaction_request_system),
        );
        let task = Task::from_user_input("work", 0, channel("alice"));
        let task_id = task.id;
        let rev = revision(&task, None, None);
        app.world_mut().spawn(task);
        let text = app
            .world_mut()
            .spawn((
                UserInputMessage {
                    origin_channel: channel("alice"),
                    content: "earlier".into(),
                    sender: Some("alice".into()),
                },
                InputSequence(1),
            ))
            .id();
        let control = app
            .world_mut()
            .spawn((
                InteractionRequest {
                    channel: channel("alice"),
                    command_id: Uuid::new_v4(),
                    interaction: TaskInteraction::Finish {
                        task_id,
                        expected_revision: rev,
                    },
                },
                InputSequence(2),
            ))
            .id();
        app.update();
        assert!(app.world().get::<InputBlocked>(control).is_some());
        assert_eq!(
            app.world_mut()
                .query::<&FinishTaskMessage>()
                .iter(app.world())
                .count(),
            0
        );
        app.world_mut().despawn(text);
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&FinishTaskMessage>()
                .iter(app.world())
                .count(),
            1
        );
    }
}
