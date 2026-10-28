//! 审批者身份校验
//!
//! 群聊中通道以群为粒度，白名单内任何成员都能回复审批。本系统在审批响应被消费前校验
//! 回复者身份：只有任务发起人或通道配置的 `approvers` 能放行；没有发起人的任务
//! （事件任务、定时任务）在未配置 `approvers` 时保持原行为。不满足时丢弃该响应，
//! 任务继续等待审批。

use crate::prelude::*;
use tracing::warn;

use crate::{
    domain::{
        FrontendKind, SystemOutputMessage, Task, TaskId, TaskInitiator,
        ToolConfirmationResponseMessage,
    },
    ecs::EntityIndex,
    systems::HarnessSettings,
};

/// 沿父链查找根任务时的步数上限，防御异常数据形成环。
const MAX_TASK_DEPTH: usize = 64;

/// 取任务所属任务树根任务的发起人。
///
/// `lookup` 返回任务的 `(父任务 ID, 发起人)`；任务不存在时返回 None。
/// 父任务已不存在时，以当前任务为根。
pub(crate) fn root_initiator(
    task_id: TaskId,
    lookup: impl Fn(TaskId) -> Option<(Option<TaskId>, Option<String>)>,
) -> Option<String> {
    let (mut parent, mut initiator) = lookup(task_id)?;
    for _ in 0..MAX_TASK_DEPTH {
        let Some(parent_id) = parent else {
            return initiator;
        };
        let Some(next) = lookup(parent_id) else {
            return initiator;
        };
        (parent, initiator) = next;
    }
    initiator
}

/// 审批回复者是否有权放行该任务的审批。
pub(crate) fn approval_allowed(
    initiator: Option<&str>,
    approvers: &[String],
    sender: &str,
) -> bool {
    if approvers.iter().any(|a| a == sender) {
        return true;
    }
    match initiator {
        Some(initiator) => initiator == sender,
        None => approvers.is_empty(),
    }
}

/// 通道配置中的代审名单。
fn approvers_for<'a>(settings: &'a HarnessSettings, frontend: &FrontendKind) -> &'a [String] {
    let channels = &settings.0.channels;
    match frontend {
        FrontendKind::QQ => channels.qq.as_ref().map(|c| c.approvers.as_slice()),
        FrontendKind::Telegram => channels.telegram.as_ref().map(|c| c.approvers.as_slice()),
        _ => None,
    }
    .unwrap_or(&[])
}

/// 丢弃无权审批者的审批响应，并提示会话通道。
pub fn approval_sender_gate_system(
    mut commands: Commands,
    settings: Res<HarnessSettings>,
    index: Res<EntityIndex>,
    responses: Query<(Entity, &ToolConfirmationResponseMessage)>,
    tasks: Query<(&Task, Option<&TaskInitiator>)>,
) {
    for (entity, response) in &responses {
        // 本地前端（TUI）与内部审批没有回复者身份，视为可信
        let Some(sender) = response.sender.as_deref() else {
            continue;
        };
        let Some((task, _)) = tasks
            .iter()
            .find(|(t, _)| t.pending_confirmation_id == Some(response.request_id))
        else {
            continue;
        };
        let Some(channel) = task
            .routing_policy
            .approval_channel()
            .or(task.origin_channel.as_ref())
        else {
            continue;
        };
        let initiator = root_initiator(task.id, |id| {
            let (t, initiator) = index.get_task(&id).and_then(|e| tasks.get(e).ok())?;
            Some((t.parent_task_id, initiator.map(|i| i.0.clone())))
        });
        let approvers = approvers_for(&settings, &channel.frontend);
        if approval_allowed(initiator.as_deref(), approvers, sender) {
            continue;
        }

        warn!(
            event = "ApprovalSenderRejected",
            task_id = %task.id,
            request_id = %response.request_id,
            sender = %sender,
            initiator = ?initiator,
            "approval reply from non-initiator rejected"
        );
        commands.spawn(SystemOutputMessage {
            task_id: task.id,
            content: "仅任务发起人或审批人可以审批此操作，审批仍在等待中。".to_string(),
        });
        commands.entity(entity).despawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::config::{ChannelConfigs, QqConfig};
    use crate::domain::{ChannelId, TaskRoutingPolicy};
    use crate::systems::HarnessConfig;
    use std::collections::HashMap;

    #[test]
    fn approval_allowed_rules() {
        let none: Vec<String> = Vec::new();
        let admins = vec!["admin".to_string()];
        assert!(approval_allowed(Some("a"), &none, "a"));
        assert!(!approval_allowed(Some("a"), &none, "b"));
        assert!(approval_allowed(Some("a"), &admins, "admin"));
        assert!(approval_allowed(None, &none, "anyone"));
        assert!(!approval_allowed(None, &admins, "anyone"));
        assert!(approval_allowed(None, &admins, "admin"));
    }

    #[test]
    fn root_initiator_walks_to_root() {
        let root = TaskId::new();
        let child = TaskId::new();
        let grandchild = TaskId::new();
        let tree: HashMap<TaskId, (Option<TaskId>, Option<String>)> = HashMap::from([
            (root, (None, Some("a".to_string()))),
            (child, (Some(root), None)),
            (grandchild, (Some(child), None)),
        ]);
        let lookup = |id| tree.get(&id).cloned();
        assert_eq!(root_initiator(grandchild, lookup), Some("a".to_string()));
        assert_eq!(root_initiator(TaskId::new(), lookup), None);
    }

    fn group() -> ChannelId {
        ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "group:g".to_string(),
            thread_id: None,
        }
    }

    fn world(approvers: Vec<String>) -> World {
        let mut world = World::new();
        world.insert_resource(HarnessSettings(HarnessConfig {
            channels: ChannelConfigs {
                qq: Some(QqConfig {
                    app_id: "id".to_string(),
                    app_secret: "secret".to_string(),
                    allowed_users: Vec::new(),
                    approvers,
                    pairing_enabled: false,
                    pairing_code: None,
                }),
                ..ChannelConfigs::default()
            },
            ..HarnessConfig::default()
        }));
        world.init_resource::<EntityIndex>();
        world
    }

    /// 生成一个等待审批的群聊根任务和它的子任务，返回 (子任务审批请求 ID)。
    fn pending_child_approval(world: &mut World, initiator: Option<&str>) -> uuid::Uuid {
        let root = Task::from_user_input("root", 0, group());
        let root_id = root.id;
        let mut root_entity = world.spawn(root);
        if let Some(initiator) = initiator {
            root_entity.insert(TaskInitiator(initiator.to_string()));
        }
        let root_entity = root_entity.id();
        let mut child = Task::from_user_input("child", 0, group());
        child.parent_task_id = Some(root_id);
        let request_id = uuid::Uuid::new_v4();
        child.pending_confirmation_id = Some(request_id);
        let child_id = child.id;
        let child_entity = world.spawn(child).id();
        let mut index = world.resource_mut::<EntityIndex>();
        index.tasks.insert(root_id, root_entity);
        index.tasks.insert(child_id, child_entity);
        request_id
    }

    fn respond(world: &mut World, request_id: uuid::Uuid, sender: Option<&str>) -> bool {
        world.spawn(ToolConfirmationResponseMessage {
            request_id,
            selected_option: "allow_once".to_string(),
            feedback: None,
            sender: sender.map(str::to_string),
        });
        let mut schedule = Schedule::default();
        schedule.add_systems(approval_sender_gate_system);
        schedule.run(world);
        let kept = world
            .query::<&ToolConfirmationResponseMessage>()
            .iter(world)
            .count()
            == 1;
        let notices = world.query::<&SystemOutputMessage>().iter(world).count();
        assert_eq!(notices, usize::from(!kept), "notice iff rejected");
        kept
    }

    #[test]
    fn initiator_can_approve_child_task() {
        let mut world = world(Vec::new());
        let request_id = pending_child_approval(&mut world, Some("a"));
        assert!(respond(&mut world, request_id, Some("a")));
    }

    #[test]
    fn other_group_member_is_rejected() {
        let mut world = world(Vec::new());
        let request_id = pending_child_approval(&mut world, Some("a"));
        assert!(!respond(&mut world, request_id, Some("b")));
    }

    #[test]
    fn configured_approver_can_approve_for_others() {
        let mut world = world(vec!["admin".to_string()]);
        let request_id = pending_child_approval(&mut world, Some("a"));
        assert!(respond(&mut world, request_id, Some("admin")));
    }

    #[test]
    fn local_frontend_reply_passes() {
        let mut world = world(Vec::new());
        let request_id = pending_child_approval(&mut world, Some("a"));
        assert!(respond(&mut world, request_id, None));
    }

    #[test]
    fn task_without_initiator_follows_approvers() {
        let mut open = world(Vec::new());
        let request_id = pending_child_approval(&mut open, None);
        assert!(respond(&mut open, request_id, Some("anyone")));

        let mut restricted = world(vec!["admin".to_string()]);
        let request_id = pending_child_approval(&mut restricted, None);
        assert!(!respond(&mut restricted, request_id, Some("anyone")));
    }

    #[test]
    fn event_task_uses_approval_channel_frontend() {
        let mut world = world(vec!["admin".to_string()]);
        let mut task =
            Task::from_trigger("event", 0, TaskRoutingPolicy::event(Some(group()), None));
        let request_id = uuid::Uuid::new_v4();
        task.pending_confirmation_id = Some(request_id);
        let id = task.id;
        let entity = world.spawn(task).id();
        world.resource_mut::<EntityIndex>().tasks.insert(id, entity);
        assert!(!respond(&mut world, request_id, Some("anyone")));
    }
}
