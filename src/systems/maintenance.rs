use bevy::prelude::*;
use uuid::Uuid;

use crate::{
    app::HarnessSettings,
    domain::{Agent, AgentCapabilities, AgentProfile, AgentStatus},
};

/// 在启动时创建一个默认静态 Agent 供 MVP 调度使用。
pub(crate) fn spawn_default_agent_system(mut commands: Commands, settings: Res<HarnessSettings>) {
    commands.spawn(Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: settings.0.default_agent_name.clone(),
            model: settings.0.llm.model.clone(),
        },
        status: AgentStatus::Idle,
        capabilities: AgentCapabilities {
            tags: vec!["llm".to_string(), "default".to_string()],
            description: "默认 LLM Agent，负责消费 MVP 执行请求".to_string(),
        },
    });

    if let Some(brain_config) = &settings.0.brain {
        if brain_config.enabled {
            commands.spawn(Agent {
                id: Uuid::new_v4(),
                profile: AgentProfile {
                    name: brain_config.agent_name.clone(),
                    model: brain_config.model.clone(),
                },
                status: AgentStatus::Idle,
                capabilities: AgentCapabilities {
                    tags: vec!["brain".to_string(), "dispatcher".to_string()],
                    description: "Brain Agent，负责调度决策".to_string(),
                },
            });
        }
    }
}

/// 为后续多 Agent 生命周期管理保留维护入口。
pub(crate) fn agent_factory_system() {}
