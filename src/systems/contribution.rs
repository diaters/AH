use bevy::prelude::*;
use tracing::info;

use crate::{
    domain::{
        Agent, AgentKind, LongTermMemory, MemoryAbsorptionMessage,
        MemoryContributionRequestMessage, Task, TaskTerminatedMessage, TaskSummary,
    },
};

/// Agent 终止系统：检测任务型 Agent 销毁，生成贡献请求
pub(crate) fn agent_termination_system(
    mut commands: Commands,
    terminated: Query<(Entity, &TaskTerminatedMessage)>,
    agents: Query<(Entity, &Agent)>,
    tasks: Query<&Task>,
    long_memories: Query<&LongTermMemory>,
) {
    for (_entity, terminated_msg) in &terminated {
        // 查找绑定的任务型 Agent
        for (agent_entity, agent) in &agents {
            if agent.kind != AgentKind::TaskScoped {
                continue;
            }
            if agent.bound_task_id != Some(terminated_msg.task_id) {
                continue;
            }

            // 获取父 Agent ID
            let Some(parent_id) = agent.parent_id else {
                continue;
            };

            // 获取任务信息用于摘要
            let task = tasks.iter().find(|t| t.id == terminated_msg.task_id);
            let task_summary = task.map(|t| TaskSummary {
                task_id: t.id,
                goal: t.content.clone(),
                outcome: t.result_summary.clone(),
            });

            // 获取长期记忆
            let long_memory = long_memories.get(agent_entity).ok();

            // 生成贡献请求
            commands.spawn(MemoryContributionRequestMessage {
                contributor_id: agent.id,
                contributor_name: agent.profile.name.clone(),
                parent_id,
                memories: long_memory.map(|m| m.entries.clone()).unwrap_or_default(),
                task_summary: task_summary.unwrap_or_else(|| TaskSummary {
                    task_id: terminated_msg.task_id,
                    goal: String::new(),
                    outcome: String::new(),
                }),
            });

            info!(
                contributor = %agent.profile.name,
                parent_id = %parent_id,
                "generated memory contribution request"
            );
        }
        // Note: TaskTerminatedMessage is despawned by agent_factory_system
        // This system must run BEFORE agent_factory_system
    }
}

/// 记忆贡献处理系统：执行 LLM 评估并吸收记忆
pub(crate) fn memory_contribution_system(
    mut commands: Commands,
    requests: Query<(Entity, &MemoryContributionRequestMessage)>,
) {
    for (entity, request) in &requests {
        let parent_id = request.parent_id;
        let memories = request.memories.clone();

        // Phase 4.1: 简单策略 - 直接吸收所有记忆
        // Phase 4.2: 引入 LLM 评估
        commands.spawn(MemoryAbsorptionMessage {
            parent_id,
            absorbed: memories,
        });

        commands.entity(entity).despawn();
    }
}

/// 记忆吸收系统：将评估后的记忆写入父 Agent
pub(crate) fn memory_absorption_system(
    mut commands: Commands,
    absorptions: Query<(Entity, &MemoryAbsorptionMessage)>,
    agents: Query<(Entity, &Agent)>,
    mut long_memories: Query<&mut LongTermMemory>,
) {
    for (entity, absorption) in &absorptions {
        // 查找父 Agent
        let parent = agents.iter().find(|(_, a)| a.id == absorption.parent_id);

        if let Some((parent_entity, _parent)) = parent {
            // 找到父 Agent 的长期记忆并吸收
            if let Ok(mut memory) = long_memories.get_mut(parent_entity) {
                memory.absorb(absorption.absorbed.clone());
                info!(
                    parent_entity = ?parent_entity,
                    count = absorption.absorbed.len(),
                    "absorbed memories into parent agent"
                );
            }
        }

        commands.entity(entity).despawn();
    }
}
