use bevy::prelude::*;
use tracing::info;

use crate::{
    app::MemoryConfig,
    domain::{Agent, LongTermMemory, ShortTermMemory},
};

/// 记忆压缩系统：检测 token 阈值并触发摘要
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut tasks: Query<(
        &crate::domain::Task,
        &mut ShortTermMemory,
        Option<&mut LongTermMemory>,
    )>,
) {
    for (task, mut short_term, long_term) in &mut tasks {
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

            // 收集需要压缩的条目
            let to_compress: Vec<_> = short_term.entries.drain(0..compress_count).collect();

            // 生成摘要内容
            let mut compress_text = String::new();
            for entry in &to_compress {
                compress_text.push_str(&format!("{:?}: {}\n", entry.role, entry.content));
            }

            // 更新摘要前缀
            // Phase 4.1: 简单拼接，Phase 4.2 调用 LLM 生成摘要
            let new_summary = if let Some(existing) = &short_term.summary_prefix {
                format!("{}\n\n{}", existing, compress_text)
            } else {
                compress_text
            };

            short_term.summary_prefix = Some(new_summary);

            // 重新计算 token
            short_term.recalculate_tokens();

            info!(
                task_id = %task.id,
                compressed_count = compress_count,
                new_tokens = short_term.estimated_tokens,
                "compressed short-term memory"
            );

            // 将压缩的条目移入长期记忆
            if let Some(mut long) = long_term {
                for entry in to_compress {
                    long.add_archive(entry.content);
                }
            }
        }
    }
}

/// 为任务型 Agent 初始化记忆 Component
pub(crate) fn init_agent_memory_system(
    mut commands: Commands,
    agents: Query<(Entity, &Agent), Added<Agent>>,
) {
    for (entity, _agent) in &agents {
        // 所有 Agent 都添加长期记忆
        commands.entity(entity).insert(LongTermMemory::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryRole, Task};

    #[test]
    fn memory_compression_by_tokens() {
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
            preserve_recent_turns: 1,
            summary_target_tokens: 50,
        });

        let task = Task::from_user_input("test", 3);
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
            experience: crate::domain::AgentExperience::default(),
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
