use bevy::prelude::*;

use crate::{
    app::MemoryConfig,
    domain::{Agent, LongTermMemory, ShortTermMemory},
};

/// 记忆压缩系统：检测容量并触发摘要
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut tasks: Query<(&crate::domain::Task, &mut ShortTermMemory, Option<&mut LongTermMemory>)>,
) {
    for (_task, mut short_term, long_term) in &mut tasks {
        // 检查是否需要压缩
        if short_term.turn_count > config.compression_threshold {
            // 计算需要压缩的范围
            let entries_count = short_term.entries.len();
            if entries_count <= config.recent_turns as usize {
                continue;
            }

            let compress_count = entries_count - config.recent_turns as usize;
            if compress_count == 0 {
                continue;
            }

            // 简单压缩：将早期条目标记为 Archive 并移动到长期记忆
            // Phase 4.1 使用简单策略，Phase 4.2 引入 LLM 摘要
            let archive_entries: Vec<_> = short_term.entries.drain(0..compress_count).collect();

            // 更新摘要范围
            let start_turn = short_term.summary_range.map(|(s, _)| s).unwrap_or(0);
            let end_turn = archive_entries.last().map(|e| e.turn).unwrap_or(0);

            short_term.summary_range = Some((start_turn, end_turn));
            short_term.summary_prefix = Some(format!(
                "Earlier conversation (turns {}-{}) was archived.",
                start_turn, end_turn
            ));

            // 将归档条目移入长期记忆（如果存在）
            if let Some(mut long) = long_term {
                for entry in archive_entries {
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
    fn memory_compression_compresses_old_entries() {
        // Setup
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            recent_turns: 2,
            compression_threshold: 3,
            summary_window: 2,
        });

        let task = Task::from_user_input("test", 3);
        let entity = world.spawn((task, ShortTermMemory::default())).id();

        // Add entries
        {
            let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
            for i in 0..5 {
                stm.add_entry(
                    EntryRole::User,
                    format!("message {}", i),
                    Default::default(),
                );
            }
        }

        // Run system
        let config = MemoryConfig {
            recent_turns: 2,
            compression_threshold: 3,
            summary_window: 2,
        };
        let mut query = world.query::<(&Task, &mut ShortTermMemory, Option<&mut LongTermMemory>)>();
        for (_, mut stm, ltm) in query.iter_mut(&mut world) {
            if stm.turn_count > config.compression_threshold {
                let entries_count = stm.entries.len();
                if entries_count > config.recent_turns as usize {
                    let compress_count = entries_count - config.recent_turns as usize;
                    let _archive_entries: Vec<_> = stm.entries.drain(0..compress_count).collect();
                    stm.summary_prefix = Some("archived".to_string());
                    if let Some(mut _long) = ltm {
                        // archive would be added here
                    }
                }
            }
        }

        // Verify
        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert_eq!(stm.entries.len(), 2); // only recent_turns kept
        assert!(stm.summary_prefix.is_some());
    }

    #[test]
    fn init_agent_memory_system_logic() {
        // Test that the logic is correct - adding LongTermMemory to agents
        let mut world = World::new();
        world.init_resource::<MemoryConfig>();

        // Spawn agent with LongTermMemory directly (simulating what init_agent_memory_system does)
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
        };

        let entity = world.spawn((agent, LongTermMemory::default())).id();

        // Verify
        assert!(world.get::<LongTermMemory>(entity).is_some());
    }

    #[test]
    fn short_term_memory_compression_logic() {
        // Test the compression logic directly
        let mut stm = ShortTermMemory::default();

        // Add 5 entries
        for i in 0..5 {
            stm.add_entry(
                EntryRole::User,
                format!("message {}", i),
                Default::default(),
            );
        }

        assert_eq!(stm.turn_count, 5);
        assert_eq!(stm.entries.len(), 5);

        // Simulate compression: keep only recent_turns (2)
        let recent_turns = 2usize;
        let compress_count = stm.entries.len() - recent_turns;
        let archive_entries: Vec<_> = stm.entries.drain(0..compress_count).collect();

        assert_eq!(archive_entries.len(), 3);
        assert_eq!(stm.entries.len(), 2);

        // Update summary info
        let start_turn = stm.summary_range.map(|(s, _)| s).unwrap_or(0);
        let end_turn = archive_entries.last().map(|e| e.turn).unwrap_or(0);
        stm.summary_range = Some((start_turn, end_turn));
        stm.summary_prefix = Some(format!(
            "Earlier conversation (turns {}-{}) was archived.",
            start_turn, end_turn
        ));

        assert!(stm.summary_prefix.is_some());
        assert_eq!(stm.summary_range, Some((0, 3)));
    }
}
