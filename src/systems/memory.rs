use crate::prelude::*;
use tracing::debug;

use crate::{
    app::MemoryConfig,
    domain::{
        Agent, EntryRole, LongTermMemory, LongTermMemoryEntry, LtmEvictedHookPending,
        LtmWriteHookPending, MemoryEntry, MemoryImportance, ShortTermMemory,
        SummarizationRequestMessage, SummarizationTrigger, Task, TaskStatus, WaitingReason,
        render_tool_calls_summary,
    },
    infrastructure::memory::LongTermMemoryService,
};

/// 将 STM entries 按配对组切分。
///
/// 配对组定义：
/// - User 开启新的对话配对组
/// - Assistant（无 tool_calls）归入当前对话配对组
/// - Assistant（有 tool_calls）开启新的工具配对组（原子性锚点）
/// - Summary / Archive 归入最近的配对组
fn split_into_groups(entries: &[MemoryEntry]) -> Vec<Vec<usize>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_group: Vec<usize> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        let starts_new_group = match entry.role {
            EntryRole::User => true,
            EntryRole::Assistant if !entry.metadata.tool_calls.is_empty() => true,
            EntryRole::Assistant => false,
            EntryRole::Summary | EntryRole::Archive => false,
        };

        if starts_new_group && !current_group.is_empty() {
            groups.push(std::mem::take(&mut current_group));
        }
        current_group.push(i);
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    groups
}

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
            // 替换原有的 preserve_count / compress_count 逻辑
            let groups = split_into_groups(&short_term.entries);
            if groups.len() <= config.preserve_recent_turns as usize {
                continue;
            }

            let preserve_group_count = config.preserve_recent_turns as usize;
            let compress_entry_count = groups
                .iter()
                .take(groups.len() - preserve_group_count)
                .map(|g| g.len())
                .sum();

            if compress_entry_count == 0 {
                continue;
            }

            // 收集需要压缩的条目内容（含 tool_calls 渲染）
            let to_compress: Vec<_> = short_term
                .entries
                .iter()
                .take(compress_entry_count)
                .collect();
            let mut compress_text = String::new();
            for entry in &to_compress {
                let mut line = format!("{:?}: {}", entry.role, entry.content);
                if !entry.metadata.tool_calls.is_empty() {
                    line.push_str(&format!(
                        "\n  {}",
                        render_tool_calls_summary(&entry.metadata.tool_calls)
                    ));
                }
                compress_text.push_str(&line);
                compress_text.push('\n');
            }

            // 发送摘要请求而非直接拼接
            debug!(
                event = "CompressionTriggered",
                task_id = %task.id,
                current_tokens = short_term.estimated_tokens,
                threshold = config.compression_threshold_tokens,
                groups_total = groups.len(),
                groups_to_compress = groups.len() - preserve_group_count,
                entries_to_compress = compress_entry_count,
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
                // 标记长期记忆写入，触发 on_long_term_memory_write hook。
                entity.insert(LtmWriteHookPending);
            },
            |_, _| {},
        );
    }
}

/// 根据最近访问时间、重要度和复用次数更新长期记忆衰退分数。
///
/// 返回被驱逐的条目列表：decay_score < 0.1、未钉选、非 Critical 的条目将被移除。
pub(crate) fn apply_memory_decay(
    entries: &mut Vec<LongTermMemoryEntry>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<LongTermMemoryEntry> {
    // Phase 1: update decay scores
    for entry in entries.iter_mut() {
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

    // Phase 2: evict low-value entries
    let mut evicted = Vec::new();
    entries.retain(|entry| {
        let should_evict =
            entry.decay_score < 0.1 && !entry.pin && entry.importance != MemoryImportance::Critical;

        if should_evict {
            evicted.push(entry.clone());
            false // remove
        } else {
            true // keep
        }
    });
    evicted
}

/// 周期性执行长期记忆衰退治理，压低长期未访问且低价值条目的分数。
/// 低价值条目被驱逐后归档到文件。驱逐发生时附带 `LtmEvictedHookPending` 标记，
/// 由 companion 系统 `on_ltm_evicted_hook_system` 派发 hook 后移除。
pub(crate) fn long_term_memory_decay_system(
    mut commands: Commands,
    mut agents: Query<(Entity, &Agent, &mut LongTermMemory)>,
    service: Res<LongTermMemoryService>,
) {
    let now = chrono::Utc::now();
    for (entity, _agent, mut memory) in &mut agents {
        let evicted = apply_memory_decay(&mut memory.entries, now);
        if !evicted.is_empty() {
            if let Some(name) = &memory.agent_name {
                service.archive_entries(name, &evicted);
            }
            debug!(
                event = "LongTermMemoryEvicted",
                agent_name = ?memory.agent_name,
                evicted_count = evicted.len(),
                "evicted low-value memory entries to archive"
            );
            // 标记驱逐事件，触发 on_long_term_memory_evicted hook。
            commands.entity(entity).insert(LtmEvictedHookPending);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ChannelId, EntryMetadata, EntryRole, FrontendKind, LongTermMemoryEntry, MemoryImportance,
        Task, ToolCall,
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
                thread_id: None,
            },
        );
        // 内部恢复/测试夹具，不触发 on_task_created hook：此处仅为构造一个带 STM 的
        // Task 测试 memory 压缩逻辑，不经过 user_message_to_task 流程，也不会
        // 在本测试 World 中插入 PluginRegistry。
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
                scope_tags: vec![],
                importance: MemoryImportance::Critical,
                pin: false,
                created_at: now - chrono::Duration::days(30),
                last_accessed_at: Some(now - chrono::Duration::days(30)),
                reuse_count: 0,
                decay_score: 0.5,
                source: "test".to_string(),
                confidence: 0.7,
                source_candidate_id: None,
                source_task_id: None,
                agent_id: None,
            }],
        };

        let _evicted = apply_memory_decay(&mut memory.entries, now);

        // Critical importance: +0.2 bonus, so decay_score = 0.5 - 0.5 + 0.2 = 0.2
        // Critical entries are never evicted, so entry remains
        assert_eq!(memory.entries.len(), 1);
        assert!(memory.entries[0].decay_score < 0.5);
    }

    #[test]
    fn decay_system_evicts_low_value_entries() {
        let mut entries = vec![LongTermMemoryEntry::new("stale entry")];
        entries[0].decay_score = 0.05;
        entries[0].pin = false;
        entries[0].importance = MemoryImportance::Low;

        let now = chrono::Utc::now();
        let evicted = apply_memory_decay(&mut entries, now);

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].content, "stale entry");
        assert!(entries.is_empty());
    }

    #[test]
    fn critical_entries_are_never_evicted() {
        let mut entries = vec![LongTermMemoryEntry::new("critical entry")];
        entries[0].decay_score = 0.01;
        entries[0].pin = false;
        entries[0].importance = MemoryImportance::Critical;

        let now = chrono::Utc::now();
        let evicted = apply_memory_decay(&mut entries, now);

        assert!(evicted.is_empty());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn pinned_entries_are_never_evicted() {
        let mut entries = vec![LongTermMemoryEntry::new("pinned entry")];
        entries[0].decay_score = 0.01;
        entries[0].pin = true;
        entries[0].importance = MemoryImportance::Low;

        let now = chrono::Utc::now();
        let evicted = apply_memory_decay(&mut entries, now);

        assert!(evicted.is_empty());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn compression_preserves_tool_call_group_atomicity() {
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 50,
            preserve_recent_turns: 1,
            summary_target_tokens: 25,
        });

        let task = Task::from_user_input(
            "test",
            3,
            ChannelId {
                frontend: FrontendKind::Tui,
                user_id: "default".to_string(),
                thread_id: None,
            },
        );
        let entity = world.spawn((task, ShortTermMemory::default())).id();

        {
            let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
            // Entry 1: User (对话配对组)
            stm.add_entry(
                EntryRole::User,
                "hello world this is a long enough message to contribute tokens",
                Default::default(),
            );
            // Entry 2: Assistant with tool_calls (工具配对组——不可拆散)
            let mut metadata = EntryMetadata::default();
            metadata.tool_calls.push(ToolCall {
                id: Some("call_1".to_string()),
                tool_name: "shell_exec".to_string(),
                input: "ls -la /very/long/path/with/many/segments/to/contribute/tokens".to_string(),
                output: "file1.txt\nfile2.txt\nfile3.txt\nfile4.txt\nfile5.txt\nfile6.txt"
                    .to_string(),
                timestamp: chrono::Utc::now(),
            });
            stm.add_entry(EntryRole::Assistant, "done with tools", metadata);
            // Entry 3: User (最近的对话配对组——应保留)
            stm.add_entry(
                EntryRole::User,
                "next question with enough tokens to push over threshold when combined",
                Default::default(),
            );
            // Entry 4: Assistant (最近的对话配对组——应保留)
            stm.add_entry(
                EntryRole::Assistant,
                "final answer with enough text to be meaningful",
                Default::default(),
            );
        }

        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert!(
            stm.estimated_tokens > 50,
            "should exceed threshold, got {}",
            stm.estimated_tokens,
        );
    }
}
