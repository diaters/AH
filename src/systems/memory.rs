use bevy::prelude::*;
use tracing::debug;

use crate::{
    app::MemoryConfig,
    domain::{
        Agent, LongTermMemory, LongTermMemoryEntry, MemoryImportance, ShortTermMemory,
        SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus, WaitingReason,
    },
    infrastructure::memory::LongTermMemoryService,
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
    service: Res<LongTermMemoryService>,
) {
    for (entity, agent) in &agents {
        let agent_name = &agent.profile.name;
        let mut memory = LongTermMemory::with_name(agent_name);

        // 从持久层加载已有长期记忆
        match service.load_entries(agent_name) {
            entries if !entries.is_empty() => {
                debug!(
                    event = "LongTermMemoryLoaded",
                    agent_id = %agent.id,
                    agent_name = %agent_name,
                    entries_count = entries.len(),
                    "restored persisted long-term memory"
                );
                memory.entries = entries;
            }
            _ => {
                debug!(
                    event = "LongTermMemoryLoaded",
                    agent_id = %agent.id,
                    agent_name = %agent_name,
                    entries_count = 0,
                    "no persisted memory found, starting with empty memory"
                );
            }
        }

        commands.entity(entity).queue_handled(
            |mut entity: EntityWorldMut| {
                entity.insert(memory);
            },
            |_, _| {},
        );
    }
}

/// 根据最近访问时间、重要度和复用次数更新长期记忆衰退分数。
pub(crate) fn apply_memory_decay(
    entries: &mut [LongTermMemoryEntry],
    now: chrono::DateTime<chrono::Utc>,
) {
    for entry in entries {
        let age_days = now
            .signed_duration_since(entry.last_accessed_at.unwrap_or(entry.created_at))
            .num_days()
            .unsigned_abs() as f32;

        let base_penalty = (age_days / 30.0).min(0.5);
        let importance_bonus = match entry.importance {
            MemoryImportance::Low => 0.0,
            MemoryImportance::Medium => 0.05,
            MemoryImportance::High => 0.1,
            MemoryImportance::Critical => 0.2,
        };
        let reuse_bonus = (entry.reuse_count as f32 * 0.02).min(0.2);

        entry.decay_score =
            (entry.decay_score - base_penalty + importance_bonus + reuse_bonus).clamp(0.0, 1.0);
    }
}

/// 周期性执行长期记忆衰退治理，压低长期未访问且低价值条目的分数。
pub(crate) fn long_term_memory_decay_system(mut agents: Query<(&Agent, &mut LongTermMemory)>) {
    let now = chrono::Utc::now();
    for (_agent, mut memory) in &mut agents {
        apply_memory_decay(&mut memory.entries, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ChannelId, EntryRole, FrontendKind, LongTermMemoryEntry, LongTermMemoryKind,
        MemoryImportance, Task,
    };

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
        use crate::infrastructure::memory::{JsonFileMemoryStore, MemoryRepository};

        // 测试：LongTermMemoryService 可以从持久层加载记忆
        let dir = tempfile::TempDir::new().unwrap();
        let store = JsonFileMemoryStore::new(dir.path().join("agents"));
        let repo = MemoryRepository::new(Box::new(store));
        let service = LongTermMemoryService::new(repo);

        // 无持久数据时返回空
        let entries = service.load_entries("nonexistent-agent");
        assert!(entries.is_empty());

        // LongTermMemory::with_name 正确设置 agent_name
        let memory = LongTermMemory::with_name("test-agent");
        assert_eq!(memory.agent_name.as_deref(), Some("test-agent"));
        assert!(memory.entries.is_empty());
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

    #[test]
    fn decay_system_marks_stale_long_term_entries_inactive() {
        let now = chrono::Utc::now();
        let mut memory = LongTermMemory {
            agent_name: None,
            entries: vec![LongTermMemoryEntry {
                content: "stale note".to_string(),
                kind: LongTermMemoryKind::Fact,
                scope_tags: vec![],
                importance: MemoryImportance::Low,
                pin: false,
                created_at: now - chrono::Duration::days(30),
                last_accessed_at: Some(now - chrono::Duration::days(30)),
                reuse_count: 0,
                decay_score: 0.25,
                source: "test".to_string(),
                confidence: 0.7,
            }],
        };

        apply_memory_decay(&mut memory.entries, now);

        assert!(memory.entries[0].decay_score < 0.25);
    }
}
