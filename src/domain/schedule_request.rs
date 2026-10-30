//! 用户直接管理自己在客户端通道创建的动态计划。
use super::{ChannelId, NotifyPolicy};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInput {
    pub name: String,
    pub content: String,
    pub schedule: String,
    pub enabled: bool,
    #[serde(default)]
    pub notify: NotifyPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleAction {
    List,
    Create {
        input: ScheduleInput,
    },
    Update {
        schedule_id: Uuid,
        expected_revision: String,
        input: ScheduleInput,
    },
    Delete {
        schedule_id: Uuid,
        expected_revision: String,
    },
    History {
        schedule_id: Uuid,
    },
}

#[derive(Debug, Clone, crate::prelude::Component)]
pub struct ScheduleRequest {
    pub channel: ChannelId,
    pub command_id: Uuid,
    pub action: ScheduleAction,
}
