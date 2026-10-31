//! 请求期上下文注入管道
//!
//! 所有 `AgentExecutionRequestMessage` 在 `Execution` 首位汇合，本系统在此统一：
//! - 打上回合代次（`/stop` 后过期结果据此作废）；
//! - 取出 `TaskInbox` 积压（Durable）：追加到请求对话尾部，同步写入
//!   `ToolCallingState.conversation` 与 `ShortTermMemory`；
//! - 新一轮首个请求回填进度清单快照（不写 STM，见 ADR-007）。
//!
//! 只处理任务级 `LlmCompletion` 请求；摘要、评估与 WorkItem 请求不受影响。

use tracing::debug;

use crate::domain::{
    AgentExecutionRequest, AgentExecutionRequestMessage, AgentRequestKind, ConversationMessage,
    EntryMetadata, EntryRole, ShortTermMemory, TaskInbox, TaskTodoList, ToolCallingState,
    TrustedImages, TurnGeneration, render_inbox_items, render_todos,
};
use crate::ecs::EntityIndex;
use crate::prelude::*;

/// 进度清单回填使用的工具名（与 `update_todos` 工具保持一致）。
pub(crate) const UPDATE_TODOS_TOOL_NAME: &str = "update_todos";

/// 把文本作为 User 内容追加到对话尾部：尾部已是 User 消息时合并，避免连续 User 轮次。
pub(crate) fn push_user_text(conversation: &mut Vec<ConversationMessage>, text: &str) {
    if let Some(ConversationMessage::User { content }) = conversation.last_mut() {
        content.push_str("\n\n");
        content.push_str(text);
    } else {
        conversation.push(ConversationMessage::User {
            content: text.to_string(),
        });
    }
}

/// 把一段文本追加到请求尾部；纯 prompt 路径追加到 prompt 末尾。
fn append_user_text(request: &mut AgentExecutionRequest, text: &str) {
    match request.conversation.as_mut() {
        Some(conversation) => push_user_text(conversation, text),
        None => {
            if !request.prompt.is_empty() {
                request.prompt.push_str("\n\n");
            }
            request.prompt.push_str(text);
        }
    }
}

#[allow(clippy::type_complexity)]
pub(crate) fn context_attachment_system(
    mut commands: Commands,
    index: Res<EntityIndex>,
    mut requests: Query<&mut AgentExecutionRequestMessage>,
    mut tasks: Query<(
        Option<&mut TaskInbox>,
        Option<&mut ShortTermMemory>,
        Option<&TurnGeneration>,
        Option<&TaskTodoList>,
        Option<&TrustedImages>,
    )>,
    mut calling_states: Query<&mut ToolCallingState>,
) {
    for mut message in &mut requests {
        let request = &mut message.request;
        if request.request_kind != AgentRequestKind::LlmCompletion
            || request.work_item_id.is_some()
            || request.turn_generation.is_some()
        {
            continue;
        }
        let Some(task_entity) = index.get_task(&request.task_id) else {
            continue;
        };
        let Ok((inbox, short_term, generation, todos, trusted_images)) = tasks.get_mut(task_entity)
        else {
            continue;
        };

        request.turn_generation = Some(generation.map(|g| g.0).unwrap_or_default());
        request.trusted_images = trusted_images
            .map(|t| t.0.iter().cloned().collect())
            .unwrap_or_default();

        let mut calling_state = calling_states
            .iter_mut()
            .find(|s| s.task_id == request.task_id && s.work_item_id.is_none());
        let is_turn_start = calling_state.is_none();

        // Durable：收件箱积压
        if let Some(mut inbox) = inbox
            && !inbox.is_empty()
        {
            if inbox.has_user_steer() {
                commands
                    .entity(task_entity)
                    .remove::<crate::domain::BackgroundReportTurn>();
            }
            let items = inbox.drain_all();
            let text = render_inbox_items(&items);
            append_user_text(request, &text);
            if let Some(state) = calling_state.as_mut() {
                push_user_text(&mut state.conversation, &text);
            }
            if let Some(mut stm) = short_term {
                stm.add_entry(EntryRole::User, &text, EntryMetadata::default());
            }
            debug!(
                event = "InboxDrainedIntoRequest",
                task_id = %request.task_id,
                item_count = items.len(),
                is_turn_start = is_turn_start,
                "inbox items attached to llm request"
            );
        }

        // Ephemeral：新一轮首个请求回填进度清单快照
        if is_turn_start
            && let Some(todos) = todos
            && !todos.items.is_empty()
        {
            let snapshot = format!(
                "<todo_snapshot>\n当前进度清单（如有变化请调用 {UPDATE_TODOS_TOOL_NAME} 更新）：\n{}\n</todo_snapshot>",
                render_todos(&todos.items)
            );
            append_user_text(request, &snapshot);
            debug!(
                event = "TodoSnapshotAttached",
                task_id = %request.task_id,
                todo_count = todos.items.len(),
                "todo snapshot attached to turn-start request"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        AgentId, ChannelId, FrontendKind, InboxItem, Task, TaskId, TodoItem, TodoStatus,
    };

    fn channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "local".to_string(),
            thread_id: None,
        }
    }

    fn request(
        task_id: TaskId,
        conversation: Option<Vec<ConversationMessage>>,
    ) -> AgentExecutionRequest {
        AgentExecutionRequest {
            task_id,
            agent_id: AgentId::nil(),
            request_kind: AgentRequestKind::LlmCompletion,
            prompt: "base prompt".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation,
            work_item_id: None,
            model_override: None,
            turn_generation: None,
            vision: false,
            trusted_images: Vec::new(),
        }
    }

    fn tool_loop_conversation() -> Vec<ConversationMessage> {
        vec![
            ConversationMessage::User {
                content: "goal".to_string(),
            },
            ConversationMessage::Assistant {
                content: None,
                tool_calls: vec![],
                reasoning_content: None,
            },
            ConversationMessage::Tool {
                tool_call_id: "call-1".to_string(),
                content: "result".to_string(),
            },
        ]
    }

    fn calling_state(task_id: TaskId, conversation: Vec<ConversationMessage>) -> ToolCallingState {
        ToolCallingState {
            task_id,
            agent_id: AgentId::nil(),
            pending_tool_call_ids: vec![],
            iteration: 1,
            max_iterations: 5,
            conversation,
            tools: vec![],
            request_kind: AgentRequestKind::LlmCompletion,
            work_item_id: None,
        }
    }

    fn setup() -> (App, Entity, TaskId) {
        let mut app = App::new();
        app.insert_resource(EntityIndex::default());
        app.add_systems(Update, context_attachment_system);
        let task = Task::from_user_input("goal", 3, channel());
        let task_id = task.id;
        let entity = app
            .world_mut()
            .spawn((task, ShortTermMemory::default()))
            .id();
        app.world_mut()
            .resource_mut::<EntityIndex>()
            .tasks
            .insert(task_id, entity);
        (app, entity, task_id)
    }

    fn inbox_with_steer(content: &str) -> TaskInbox {
        let mut inbox = TaskInbox::default();
        inbox.push(InboxItem::UserSteer {
            content: content.to_string(),
            received_at: chrono::Utc::now(),
        });
        inbox
    }

    fn sent_request(app: &mut App) -> AgentExecutionRequest {
        app.world_mut()
            .query::<&AgentExecutionRequestMessage>()
            .single(app.world())
            .unwrap()
            .request
            .clone()
    }

    #[test]
    fn request_carries_task_trusted_images() {
        let (mut app, entity, task_id) = setup();
        app.world_mut().entity_mut(entity).insert(TrustedImages(
            ["/tmp/a.png".to_string()].into_iter().collect(),
        ));
        app.world_mut().spawn(AgentExecutionRequestMessage {
            request: request(task_id, None),
        });
        app.update();

        assert_eq!(
            sent_request(&mut app).trusted_images,
            vec!["/tmp/a.png".to_string()]
        );
    }

    #[test]
    fn steer_is_appended_after_tool_results_and_mirrored_to_state_and_stm() {
        let (mut app, entity, task_id) = setup();
        app.world_mut()
            .entity_mut(entity)
            .insert(inbox_with_steer("use sqlite instead"));
        let state_entity = app
            .world_mut()
            .spawn(calling_state(task_id, tool_loop_conversation()))
            .id();
        app.world_mut().spawn(AgentExecutionRequestMessage {
            request: request(task_id, Some(tool_loop_conversation())),
        });
        app.update();

        let sent = sent_request(&mut app);
        assert_eq!(sent.turn_generation, Some(0));
        let conversation = sent.conversation.unwrap();
        assert!(matches!(
            conversation.last(),
            Some(ConversationMessage::User { content }) if content.contains("use sqlite instead")
        ));
        let state = app.world().get::<ToolCallingState>(state_entity).unwrap();
        assert!(matches!(
            state.conversation.last(),
            Some(ConversationMessage::User { content }) if content.contains("<user_steer>")
        ));
        let stm = app.world().get::<ShortTermMemory>(entity).unwrap();
        assert_eq!(stm.entries.len(), 1);
        assert_eq!(stm.entries[0].role, EntryRole::User);
        assert!(app.world().get::<TaskInbox>(entity).unwrap().is_empty());
    }

    #[test]
    fn prompt_only_request_gets_steer_appended_to_prompt() {
        let (mut app, entity, task_id) = setup();
        app.world_mut()
            .entity_mut(entity)
            .insert(inbox_with_steer("shorter please"));
        app.world_mut().spawn(AgentExecutionRequestMessage {
            request: request(task_id, None),
        });
        app.update();

        let sent = sent_request(&mut app);
        assert!(sent.prompt.starts_with("base prompt"));
        assert!(sent.prompt.contains("shorter please"));
    }

    #[test]
    fn steer_merges_into_trailing_user_message() {
        let mut conversation = vec![ConversationMessage::User {
            content: "first".to_string(),
        }];
        push_user_text(&mut conversation, "second");
        assert_eq!(conversation.len(), 1);
        assert!(matches!(
            &conversation[0],
            ConversationMessage::User { content } if content == "first\n\nsecond"
        ));
    }

    #[test]
    fn work_item_and_non_completion_requests_are_untouched() {
        let (mut app, entity, task_id) = setup();
        app.world_mut()
            .entity_mut(entity)
            .insert(inbox_with_steer("keep me"));
        let mut work_item = request(task_id, None);
        work_item.work_item_id = Some(uuid::Uuid::new_v4());
        app.world_mut()
            .spawn(AgentExecutionRequestMessage { request: work_item });
        app.update();

        let sent = sent_request(&mut app);
        assert_eq!(sent.turn_generation, None);
        assert_eq!(sent.prompt, "base prompt");
        assert_eq!(app.world().get::<TaskInbox>(entity).unwrap().len(), 1);
    }

    #[test]
    fn generation_stamp_follows_task_component() {
        let (mut app, entity, task_id) = setup();
        app.world_mut().entity_mut(entity).insert(TurnGeneration(3));
        app.world_mut().spawn(AgentExecutionRequestMessage {
            request: request(task_id, None),
        });
        app.update();
        assert_eq!(sent_request(&mut app).turn_generation, Some(3));
    }

    #[test]
    fn todo_snapshot_only_at_turn_start_and_not_in_stm() {
        let (mut app, entity, task_id) = setup();
        app.world_mut().entity_mut(entity).insert(TaskTodoList {
            items: vec![TodoItem {
                content: "write tests".to_string(),
                status: TodoStatus::InProgress,
            }],
        });
        app.world_mut().spawn(AgentExecutionRequestMessage {
            request: request(
                task_id,
                Some(vec![ConversationMessage::User {
                    content: "next turn".to_string(),
                }]),
            ),
        });
        app.update();
        let sent = sent_request(&mut app);
        let conversation = sent.conversation.unwrap();
        assert!(matches!(
            conversation.last(),
            Some(ConversationMessage::User { content })
                if content.starts_with("next turn") && content.contains("▶ write tests")
        ));
        assert!(
            app.world()
                .get::<ShortTermMemory>(entity)
                .unwrap()
                .entries
                .is_empty()
        );

        // 循环中（存在 ToolCallingState）不再回填
        let (mut app, entity, task_id) = setup();
        app.world_mut().entity_mut(entity).insert(TaskTodoList {
            items: vec![TodoItem {
                content: "write tests".to_string(),
                status: TodoStatus::InProgress,
            }],
        });
        app.world_mut()
            .spawn(calling_state(task_id, tool_loop_conversation()));
        app.world_mut().spawn(AgentExecutionRequestMessage {
            request: request(task_id, Some(tool_loop_conversation())),
        });
        app.update();
        let sent = sent_request(&mut app);
        assert!(matches!(
            sent.conversation.unwrap().last(),
            Some(ConversationMessage::Tool { .. })
        ));
    }
}
