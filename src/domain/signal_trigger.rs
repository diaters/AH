//! Signal 事件触发领域模型
//!
//! 定义事件来源、触发载荷与事件到任务的路由注册表。

use std::collections::HashMap;

use crate::prelude::Resource;

use super::ChannelId;

/// 标识一个 Signal 的来源，主要用于日志与诊断。
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SignalSource(pub String);

/// 触发器配置文件路径，供 `reload_triggers_system` 读取。
///
/// 从 `HarnessConfig::triggers_config_path` 在装配期投影注入，
/// 让 triggers 模块不依赖上层配置聚合（P0 依赖方向治理）。
#[derive(Resource, Default)]
pub struct TriggersConfigPath(pub Option<String>);

/// 事件任务的触发载荷。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TaskTrigger {
    Webhook {
        kind: String,
        body: serde_json::Value,
    },
    Timer {
        kind: String,
    },
}

/// 事件到任务的数据驱动路由。
///
/// 由 `triggers.toml` 反序列化得到，`build_task_input` 使用 `render_template` 渲染 prompt。
#[derive(Clone, Debug)]
pub struct EventTaskRoute {
    pub prompt_template: String,
    pub approval_channel: Option<ChannelId>,
    pub approval_context: String,
}

impl EventTaskRoute {
    /// 渲染任务输入 prompt。
    pub fn build_task_input(&self, trigger: &TaskTrigger) -> anyhow::Result<String> {
        Ok(crate::domain::render_template(
            &self.prompt_template,
            trigger,
        ))
    }

    /// 返回审批上下文（数据驱动，直接克隆）。
    pub fn build_approval_context(&self, _trigger: &TaskTrigger) -> String {
        self.approval_context.clone()
    }
}

/// Signal 触发路由表。
#[derive(Resource, Default)]
pub struct SignalTriggerRegistry {
    webhook_routes: HashMap<String, EventTaskRoute>,
    timer_routes: HashMap<String, EventTaskRoute>,
}

impl SignalTriggerRegistry {
    /// 注册 webhook 事件路由。
    pub fn register_webhook(&mut self, kind: impl Into<String>, route: EventTaskRoute) {
        self.webhook_routes.insert(kind.into(), route);
    }

    /// 注册 timer 事件路由。
    pub fn register_timer(&mut self, kind: impl Into<String>, route: EventTaskRoute) {
        self.timer_routes.insert(kind.into(), route);
    }

    /// 查找某个触发器对应的路由。
    pub fn route(&self, trigger: &TaskTrigger) -> Option<&EventTaskRoute> {
        match trigger {
            TaskTrigger::Webhook { kind, .. } => self.webhook_routes.get(kind),
            TaskTrigger::Timer { kind } => self.timer_routes.get(kind),
        }
    }

    /// 查询某个 webhook kind 是否已注册（用于重复检测）。
    pub fn webhook_route(&self, kind: &str) -> Option<&EventTaskRoute> {
        self.webhook_routes.get(kind)
    }

    /// 查询某个 timer kind 是否已注册（用于重复检测）。
    pub fn timer_route(&self, kind: &str) -> Option<&EventTaskRoute> {
        self.timer_routes.get(kind)
    }

    /// 返回 webhook 路由数量（用于测试断言）。
    pub fn webhook_route_count(&self) -> usize {
        self.webhook_routes.len()
    }

    /// 返回 timer 路由数量（用于测试断言）。
    pub fn timer_route_count(&self) -> usize {
        self.timer_routes.len()
    }
}
