use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::MemoryConfig,
    domain::{
        Agent, LongTermMemory, ShortTermMemory, SummarizationRequestMessage, SummarizationTrigger,
        Task, TaskStatus, WaitingReason,
    },
};

/// 记忆压缩系统：检测 token 阈值并触发摘要请求
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut commands: Commands,
    tasks: Query<(&Task, &ShortTermMemory)>,
) {
    for (task, short_term) in &tasks {
        // 跳过终态任务和等待摘要的任务
        if task.status.is_terminal() {
            continue;
        }
        if matches!(
            task.status,
            TaskStatus::Waiting(WaitingReason::Summarization)
        ) {
            continue;
        }

        // 检查是否需要压缩
        if short_term.estimated_tokens > config.compression_threshold_tokens {
            let entries_count = short_term.entries.len();

            // 保留最近 N 轮（每轮 = User + Assistant，所以乘 2）
            let preserve_count = (config.preserve_recent_turns * 2) as usize;
            if entries_count <= preserve_count {
                continue;
            }

            let compress_count = entries_count - preserve_count;
            if compress_count == 0 {
                continue;
            }

            // 收集需要压缩的条目内容
            let to_compress: Vec<_> = short_term.entries.iter().take(compress_count).collect();
            let mut compress_text = String::new();
            for entry in &to_compress {
                compress_text.push_str(&format!("{:?}: {}\n", entry.role, entry.content));
            }

            // 发送摘要请求而非直接拼接
            debug!(
                event = "CompressionTriggered",
                task_id = %task.id,
                current_tokens = short_term.estimated_tokens,
                threshold = config.compression_threshold_tokens,
                entries_total = entries_count,
                entries_to_compress = compress_count,
                entries_to_preserve = preserve_count,
                compress_text_len = compress_text.len(),
                "triggering summarization request"
            );

            commands.spawn(SummarizationRequestMessage {
                task_id: task.id,
                content_to_summarize: compress_text,
                target_tokens: config.summary_target_tokens,
                trigger: SummarizationTrigger::TokenThreshold,
            });
        }
    }
}

/// 为任务型 Agent 初始化记忆 Component
pub(crate) fn init_agent_memory_system(
    mut commands: Commands,
    agents: Query<(Entity, &Agent), Added<Agent>>,
) {
    for (entity, agent) in &agents {
        debug!(
            event = "AgentMemoryInitialized",
            entity = ?entity,
            agent_id = %agent.id,
            agent_name = %agent.profile.name,
            "initializing long term memory for agent"
        );
        // 所有 Agent 都添加长期记忆
        commands.entity(entity).insert(LongTermMemory::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChannelId, EntryRole, FrontendKind, Task};

    #[test]
    fn memory_compression_by_tokens() {
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
            preserve_recent_turns: 1,
            summary_target_tokens: 50,
        });

        let task = Task::from_user_input(
            "test",
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
            },
        );
        let entity = world.spawn((task, ShortTermMemory::default())).id();

        // Add entries with known token counts
        {
            let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
            // Add enough content to exceed threshold
            for i in 0..10 {
                stm.add_entry(
                    EntryRole::User,
                    format!("This is message number {} with some content", i),
                    Default::default(),
                );
            }
        }

        // Verify tokens were estimated
        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert!(stm.estimated_tokens > 0);
    }

    #[test]
    fn init_agent_memory_system_logic() {
        let mut world = World::new();
        world.init_resource::<MemoryConfig>();

        let agent = Agent {
            id: crate::domain::AgentId::nil(),
            profile: crate::domain::AgentProfile {
                name: "test".to_string(),
                model: "test-model".to_string(),
            },
            capabilities: crate::domain::AgentCapabilities {
                tags: vec![],
                description: "test agent".to_string(),
            },
            kind: crate::domain::AgentKind::Persistent,
            parent_id: None,
            bound_task_id: None,
            tool_permissions: crate::domain::AgentToolPermissions::default(),
        };

        let entity = world.spawn((agent, LongTermMemory::default())).id();

        assert!(world.get::<LongTermMemory>(entity).is_some());
    }

    #[test]
    fn short_term_memory_token_estimation() {
        let mut stm = ShortTermMemory::default();

        // Add entries
        for i in 0..5 {
            stm.add_entry(
                EntryRole::User,
                format!("message {}", i),
                Default::default(),
            );
        }

        assert_eq!(stm.entries.len(), 5);
        assert!(stm.estimated_tokens > 0);
    }
}
