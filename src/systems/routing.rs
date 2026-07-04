use crate::prelude::*;
use tracing::debug;

use crate::{
    app::Clock,
    domain::{
        ContinueTaskMessage, CreateTaskMessage, EntryMetadata, EntryRole, ShortTermMemory, Task,
        TaskStatus, UserCommand, UserInputMessage, WaitingReason,
    },
};

/// 用户输入路由系统：判断是创建新任务还是继续现有任务
pub(crate) fn user_input_routing_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
) {
    for (entity, input) in &user_inputs {
        // 检查是否是命令（命令由 command_parse_system 处理）
        if UserCommand::parse(&input.content).is_command() {
            continue; // 跳过，由 command_parse_system 处理
        }

        // 查找是否有 Waiting(User) 状态的任务
        let waiting_tasks: Vec<_> = tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Waiting(WaitingReason::User)
                    && t.origin_channel == input.origin_channel
            })
            .collect();

        if let Some(task) = waiting_tasks.first() {
            debug!(
                event = "RoutingDecision",
                decision = "continue_existing",
                input = %input.content,
                input_len = input.content.len(),
                selected_task_id = %task.id,
                waiting_tasks_count = waiting_tasks.len(),
                input_channel = ?input.origin_channel,
                task_channel = ?task.origin_channel,
                waiting_tasks = ?waiting_tasks.iter().map(|t| (t.id, t.status.clone())).collect::<Vec<_>>(),
                "routing input to existing Waiting(User) task"
            );
            // 继续现有任务
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            debug!(
                event = "RoutingDecision",
                decision = "create_new",
                input = %input.content,
                input_len = input.content.len(),
                waiting_tasks_count = 0,
                "no waiting task, creating new task"
            );
            // 创建新任务
            commands.spawn(CreateTaskMessage {
                content: input.content.clone(),
                origin_channel: input.origin_channel.clone(),
            });
        }

        commands.entity(entity).despawn();
    }
}

/// 继续任务系统：将用户输入追加到任务
pub(crate) fn continue_task_system(
    mut commands: Commands,
    clock: Res<Clock>,
    continue_messages: Query<(Entity, &ContinueTaskMessage)>,
    mut tasks: Query<(&mut Task, Option<&mut ShortTermMemory>)>,
) {
    for (entity, msg) in &continue_messages {
        // 更新任务状态和追加用户输入到 ShortTermMemory
        if let Some((mut task, short_term)) = tasks.iter_mut().find(|(t, _)| t.id == msg.task_id) {
            let stm_entries_before = short_term.as_ref().map(|s| s.entries.len()).unwrap_or(0);
            let stm_tokens_before = short_term.as_ref().map(|s| s.estimated_tokens).unwrap_or(0);

            let old_status = task.status.clone();
            let prev_content = task.content.clone();
            task.status = TaskStatus::Ready;
            task.content = msg.user_input.clone();
            task.updated_at = clock.0;

            // 追加用户输入到 ShortTermMemory
            if let Some(mut stm) = short_term {
                stm.add_entry(EntryRole::User, &msg.user_input, EntryMetadata::default());
                let stm_entries_after = stm.entries.len();
                let stm_tokens_after = stm.estimated_tokens;
                debug!(
                    event = "TaskContinued",
                    task_id = %task.id,
                    user_input = %msg.user_input,
                    user_input_len = msg.user_input.len(),
                    prev_content = %prev_content,
                    new_content = %task.content,
                    old_status = ?old_status,
                    new_status = ?task.status,
                    stm_entries_before = stm_entries_before,
                    stm_entries_after = stm_entries_after,
                    stm_tokens_before = stm_tokens_before,
                    stm_tokens_after = stm_tokens_after,
                    "task continued with new user input"
                );
            } else {
                debug!(
                    event = "TaskContinued",
                    task_id = %task.id,
                    user_input = %msg.user_input,
                    prev_content = %prev_content,
                    new_content = %task.content,
                    old_status = ?old_status,
                    new_status = ?task.status,
                    has_stm = false,
                    "task continued (no STM attached)"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    use super::user_input_routing_system;
    use crate::domain::{
        ChannelId, ContinueTaskMessage, CreateTaskMessage, FrontendKind, Task, TaskStatus,
        UserInputMessage, WaitingReason,
    };

    fn telegram_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::Telegram,
            user_id: "tg-user".to_string(),
            thread_id: None,
        }
    }

    fn qq_channel() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq-user".to_string(),
            thread_id: None,
        }
    }

    fn make_waiting_task(channel: ChannelId) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: uuid::Uuid::new_v4(),
            content: "waiting".to_string(),
            creator: uuid::Uuid::nil(),
            delegate: None,
            status: TaskStatus::Waiting(WaitingReason::User),
            pending_confirmation_id: None,
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            last_error: None,
            multi_turn: true,
            parent_task_id: None,
            batch_id: None,
            origin_channel: channel,
            last_evaluated_turn: None,
        }
    }

    #[test]
    fn cross_channel_input_not_routed_to_other_channel_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        // Telegram 通道的 Waiting(User) 任务
        app.world_mut().spawn(make_waiting_task(telegram_channel()));

        // QQ 通道的纯文本输入
        app.world_mut().spawn(UserInputMessage {
            content: "hello from QQ".to_string(),
            origin_channel: qq_channel(),
        });

        app.update();

        // 断言：应生成 CreateTaskMessage（而非 ContinueTaskMessage）
        let create_count = app
            .world_mut()
            .query::<&CreateTaskMessage>()
            .iter(app.world())
            .count();
        let continue_count = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .count();
        assert_eq!(
            create_count, 1,
            "QQ input should create new task, not continue Telegram task"
        );
        assert_eq!(
            continue_count, 0,
            "no ContinueTaskMessage should be spawned"
        );
    }

    #[test]
    fn same_channel_input_routed_to_waiting_task() {
        let mut app = App::new();
        app.add_systems(Update, user_input_routing_system);

        let task = make_waiting_task(telegram_channel());
        let task_id = task.id;
        app.world_mut().spawn(task);

        app.world_mut().spawn(UserInputMessage {
            content: "hello from Telegram".to_string(),
            origin_channel: telegram_channel(),
        });

        app.update();

        let continue_msgs: Vec<&ContinueTaskMessage> = app
            .world_mut()
            .query::<&ContinueTaskMessage>()
            .iter(app.world())
            .collect();
        assert_eq!(continue_msgs.len(), 1);
        assert_eq!(continue_msgs[0].task_id, task_id);
    }
}
