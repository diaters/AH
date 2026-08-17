//! 运行时资源契约
//!
//! 被 systems 层需要的运行时资源抽象：时钟、前端注册表与异步运行时。
//! 三者非领域概念，归属契约层（domain 与 systems 之间）。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::runtime::Runtime;

use crate::domain::{Frontend, FrontendKind};
use crate::prelude::Resource;

/// tokio 运行时句柄，作为 Resource 注入 World。
#[derive(Resource, Clone)]
pub struct AsyncRuntime(pub Arc<Runtime>);

/// 全局时钟资源（由 `tick_clock_system` 每帧推进）。
#[derive(Resource)]
pub struct Clock(pub DateTime<Utc>);

impl Default for Clock {
    fn default() -> Self {
        Self(Utc::now())
    }
}

/// 前端注册表资源。
#[derive(Resource)]
pub struct FrontendRegistry {
    pub frontends: Vec<Box<dyn Frontend>>,
}

impl FrontendRegistry {
    /// 检查指定类型的 frontend 是否已在注册表中。
    /// 注意：返回 true 仅表示该 frontend 类型已注册，不保证底层 channel 当前可用
    ///（channel 可用性由 ChannelManager 的运行时发送结果覆盖）。
    pub fn has_frontend(&self, kind: FrontendKind) -> bool {
        self.frontends.iter().any(|f| f.kind() == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EngineEvent, UserAction};

    #[test]
    fn frontend_registry_has_frontend_checks_kind() {
        struct DummyFrontend(FrontendKind);
        impl Frontend for DummyFrontend {
            fn kind(&self) -> FrontendKind {
                self.0.clone()
            }
            fn push_event(&self, _event: EngineEvent) {}
            fn poll_actions(&self) -> Vec<UserAction> {
                vec![]
            }
        }

        let registry = FrontendRegistry {
            frontends: vec![
                Box::new(DummyFrontend(FrontendKind::Tui)),
                Box::new(DummyFrontend(FrontendKind::QQ)),
            ],
        };
        assert!(registry.has_frontend(FrontendKind::Tui));
        assert!(registry.has_frontend(FrontendKind::QQ));
        assert!(!registry.has_frontend(FrontendKind::Telegram));
    }
}
