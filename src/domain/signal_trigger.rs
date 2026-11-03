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

/// Webhook 事件到任务的数据驱动路由。
///
/// 由 `triggers.toml` 的 `[[webhook.routes]]` 反序列化得到，`build_task_input`
/// 使用 `render_template` 渲染 prompt。
#[derive(Clone, Debug)]
pub struct EventTaskRoute {
    pub prompt_template: String,
    pub approval_channel: Option<ChannelId>,
    pub approval_context: String,
}

impl EventTaskRoute {
    /// 用 webhook 的 kind 与请求 body 渲染任务输入 prompt。
    pub fn build_task_input(&self, kind: &str, body: &serde_json::Value) -> String {
        crate::domain::render_template(&self.prompt_template, kind, body)
    }
}

/// Webhook 触发路由表。
///
/// 定时触发（`TaskTrigger::Timer`）只服务 `schedule_task` 动态定时任务，不经此表。
#[derive(Resource, Default)]
pub struct SignalTriggerRegistry {
    webhook_routes: HashMap<String, EventTaskRoute>,
}

impl SignalTriggerRegistry {
    /// 注册 webhook 事件路由。
    pub fn register_webhook(&mut self, kind: impl Into<String>, route: EventTaskRoute) {
        self.webhook_routes.insert(kind.into(), route);
    }

    /// 查找某个 webhook kind 的路由。
    pub fn webhook_route(&self, kind: &str) -> Option<&EventTaskRoute> {
        self.webhook_routes.get(kind)
    }

    /// 返回 webhook 路由数量（用于测试断言）。
    pub fn webhook_route_count(&self) -> usize {
        self.webhook_routes.len()
    }
}
