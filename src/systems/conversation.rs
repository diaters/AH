//! 用户会话的输入顺序与结束屏障；不维护可变的“焦点任务”。
use crate::domain::{
    ChannelId, InputBlocked, InputSequence, SessionBackend, Task, TaskId, ToolRequestPending,
    UserCommand, UserInputMessage,
};
use crate::prelude::*;
use crate::systems::NativeProcessBackend;
use std::collections::{HashMap, HashSet};

/// 完整 ChannelId 上唯一的用户顶层任务；内部子任务与输出型事件不占名额。
pub(crate) fn is_user_root(task: &Task, channel: &ChannelId) -> bool {
    task.parent_task_id.is_none() && task.origin_channel.as_ref() == Some(channel)
}

/// 终态不等于资源已经释放。保留原根任务及其树的身份，直到实际 worker/会话结束。
pub(crate) fn channel_is_closing(
    channel: &ChannelId,
    tasks: &[&Task],
    pending: &HashSet<TaskId>,
    backend: Option<&NativeProcessBackend>,
) -> bool {
    for root in tasks
        .iter()
        .filter(|task| is_user_root(task, channel) && task.status.is_terminal())
    {
        let mut tree = HashSet::from([root.id]);
        loop {
            let before = tree.len();
            for task in tasks {
                if task
                    .parent_task_id
                    .is_some_and(|parent| tree.contains(&parent))
                {
                    tree.insert(task.id);
                }
            }
            if tree.len() == before {
                break;
            }
        }
        if tasks
            .iter()
            .any(|task| tree.contains(&task.id) && !task.status.is_terminal())
            || pending.iter().any(|id| tree.contains(id))
            || backend.is_some_and(|backend| {
                tree.iter().any(|id| {
                    backend
                        .list_task_sessions(*id)
                        .map_or(true, |sessions| !sessions.is_empty())
                })
            })
        {
            return true;
        }
    }
    false
}

/// 批次中的普通输入可一起路由；命令是 FIFO 屏障，防止命令先于前面的正文执行。
/// 命令之后的消息留在 ECS，下一帧在命令及关闭屏障之后继续处理。
#[allow(clippy::too_many_arguments)]
pub(crate) fn conversation_input_order_system(
    mut commands: Commands,
    mut deferred_logged: Local<HashSet<Entity>>,
    inputs: Query<(Entity, &UserInputMessage, Option<&InputSequence>)>,
    interactions: Query<(
        Entity,
        &crate::domain::InteractionRequest,
        Option<&InputSequence>,
    )>,
    tasks: Query<&Task>,
    inflight_llm: Option<Res<crate::domain::InflightLlmRequests>>,
    pending: Query<&ToolRequestPending>,
    queued: Query<&crate::domain::ToolExecutionRequestMessage>,
    queued_llm: Query<&crate::domain::AgentExecutionRequestMessage>,
    clears: Query<&crate::domain::ClearTaskMessage>,
    backend: Option<Res<NativeProcessBackend>>,
) {
    let tasks: Vec<_> = tasks.iter().collect();
    let mut pending: std::collections::HashSet<_> = pending
        .iter()
        .map(|p| p.original_request.task_id)
        .chain(queued.iter().map(|request| request.request.task_id))
        .chain(
            queued_llm
                .iter()
                .filter(|request| request.request.work_item_id.is_none())
                .map(|request| request.request.task_id),
        )
        .collect();
    if let Some(inflight) = inflight_llm.as_ref() {
        pending.extend(
            tasks
                .iter()
                .filter(|task| inflight.in_flight(task.id) > 0)
                .map(|task| task.id),
        );
    }
    let mut groups: HashMap<ChannelId, Vec<_>> = HashMap::new();
    for (entity, input, sequence) in &inputs {
        groups
            .entry(input.origin_channel.clone())
            .or_default()
            .push((
                entity,
                UserCommand::parse(&input.content).is_command(),
                sequence.map_or(entity.to_bits(), |seq| seq.0),
                false,
            ));
    }
    for (entity, input, sequence) in &interactions {
        groups.entry(input.channel.clone()).or_default().push((
            entity,
            true,
            sequence.map_or(entity.to_bits(), |seq| seq.0),
            true,
        ));
    }
    for (channel, mut inputs) in groups {
        inputs.sort_by_key(|(_, _, sequence, _)| *sequence);
        let closing = channel_is_closing(&channel, &tasks, &pending, backend.as_deref())
            || clears.iter().any(|clear| {
                tasks
                    .iter()
                    .any(|task| task.id == clear.task_id && is_user_root(task, &channel))
            });
        let mut barrier = false;
        for (position, (entity, command, sequence_number, interaction)) in
            inputs.into_iter().enumerate()
        {
            let blocked = (closing && !interaction) || barrier || (position > 0 && command);
            if blocked {
                if deferred_logged.insert(entity) {
                    tracing::debug!(event = "ConversationInputDeferred", channel = ?channel,
                        input_sequence = sequence_number, closing,
                        "input retained behind conversation command or cleanup barrier");
                }
                commands.entity(entity).insert(InputBlocked);
            } else {
                deferred_logged.remove(&entity);
                commands.entity(entity).remove::<InputBlocked>();
            }
            barrier |= command;
        }
    }
    deferred_logged.retain(|entity| inputs.contains(*entity) || interactions.contains(*entity));
}
