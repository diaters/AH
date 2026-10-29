use crate::prelude::*;
use tracing::debug;

use crate::{
    domain::MemoryConfig,
    domain::{
        Agent, LongTermMemory, LongTermMemoryEntry, LtmEvictedHookPending, LtmWriteHookPending,
        MemoryImportance, ShortTermMemory, SummarizationRequestMessage, SummarizationTrigger, Task,
        TaskStatus, WaitingReason, WorkItem, WorkItemStatus, WorkItemType, build_compress_text,
        compressible_group_indices, render_tool_calls_summary,
    },
    infrastructure::memory::LongTermMemoryService,
};

/// 记忆压缩系统：检测 token 阈值并触发摘要请求
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut commands: Commands,
    tasks: Query<(&Task, &ShortTermMemory)>,
    work_items: Query<&WorkItem>,
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
            // 在飞保护：该 task 已有未完成的 Summarization WorkItem 时不重复触发，
            // 避免 follow-up 将任务标回 Running 后产生并发摘要
            let has_inflight = work_items.iter().any(|wi| {
                wi.work_type == WorkItemType::Summarization
                    && wi.task_id == task.id
                    && !matches!(
                        wi.status,
                        WorkItemStatus::Completed | WorkItemStatus::Failed
                    )
            });
            if has_inflight {
                continue;
            }

            // 一次全压：除最后 1 个进行中组外，全部历史组进入摘要
            let compressible = compressible_group_indices(&short_term.entries);
            if compressible.is_empty() {
                // 无可压缩内容（只有进行中的最后 1 组）→ 不触发，等待新对话
                // 把该组挤出保护窗口后再压缩（结构性收敛，无需状态机）
                continue;
            }
            let compress_text = build_compress_text(
                &short_term.entries,
                short_term.summary_prefix.as_deref(),
                render_tool_calls_summary,
            );

            // 发送摘要请求而非直接拼接
            debug!(
                event = "CompressionTriggered",
                task_id = %task.id,
                current_tokens = short_term.estimated_tokens,
                threshold = config.compression_threshold_tokens,
                groups_to_compress = compressible.len(),
                entries_to_compress = compressible.iter().map(|g| g.len()).sum::<usize>(),
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
    // 旧记录首次只建立基线；回拨不扣分，也不能把基线移回过去。
    for entry in entries.iter_mut() {
        let Some(previous) = entry.decay_updated_at else {
            entry.decay_updated_at = Some(now);
            continue;
        };
        if now <= previous {
            continue;
        }
        let since = entry
            .last_accessed_at
            .map_or(previous, |at| at.max(previous));
        let seconds = (now - since)
            .to_std()
            .map_or(0.0, |duration| duration.as_secs_f64());
        let importance_discount = match entry.importance {
            MemoryImportance::Low => 0.0,
            MemoryImportance::Medium => 0.05,
            MemoryImportance::High => 0.1,
            MemoryImportance::Critical => 0.2,
        };
        let reuse_discount = (entry.reuse_count as f64 * 0.02).min(0.2);
        let rate = 0.5 - importance_discount - reuse_discount;
        let exact = (entry.decay_score as f64 + entry.decay_remainder
            - rate * seconds / (30.0 * 86400.0))
            .clamp(0.0, 1.0);
        entry.decay_score = exact as f32;
        entry.decay_remainder = exact - entry.decay_score as f64;
        entry.decay_updated_at = Some(now.max(since));
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
    mut service: ResMut<LongTermMemoryService>,
    clock: Res<crate::contracts::Clock>,
    mut last_run: Local<Option<chrono::DateTime<chrono::Utc>>>,
    mut needs_flush: Local<std::collections::HashSet<Entity>>,
) {
    let now = clock.0;
    if last_run.is_some_and(|last| now - last < chrono::Duration::minutes(1)) {
        return;
    }
    *last_run = Some(now);
    needs_flush.retain(|entity| agents.contains(*entity));
    for (entity, _agent, mut memory) in &mut agents {
        if memory.entries.is_empty() && !needs_flush.contains(&entity) {
            continue;
        }
        let evicted = apply_memory_decay(&mut memory.entries, now);
        if memory.agent_name.is_some() {
            match service.flush(&memory) {
                Ok(()) => {
                    needs_flush.remove(&entity);
                }
                Err(error) => {
                    needs_flush.insert(entity);
                    tracing::warn!(event = "MemoryDecayPersistFailed", %error, "will retry on next maintenance");
                }
            }
        }
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
    fn legacy_decay_starts_from_existing_score_without_catch_up() {
        let now = chrono::Utc::now();
        let mut entry = LongTermMemoryEntry::new("legacy");
        entry.created_at = now - chrono::Duration::days(90);
        entry.importance = MemoryImportance::Low;
        entry.decay_score = 0.8;
        let mut entries = vec![entry];
        assert!(apply_memory_decay(&mut entries, now).is_empty());
        assert_eq!(entries[0].decay_score, 0.8);
        assert!(apply_memory_decay(&mut entries, now).is_empty());
        assert_eq!(entries[0].decay_score, 0.8);
        apply_memory_decay(&mut entries, now + chrono::Duration::days(30));
        assert!((entries[0].decay_score - 0.3).abs() < 0.000001);
        let checkpoint = entries.clone();
        apply_memory_decay(&mut entries, now + chrono::Duration::days(30));
        assert_eq!(entries, checkpoint);
    }

    #[test]
    fn old_json_without_decay_checkpoints_is_compatible() {
        let mut value = serde_json::to_value(LongTermMemoryEntry::new("old-format")).unwrap();
        value.as_object_mut().unwrap().remove("decay_updated_at");
        value.as_object_mut().unwrap().remove("decay_remainder");
        let mut entries = vec![serde_json::from_value(value).unwrap()];
        apply_memory_decay(&mut entries, chrono::Utc::now());
        assert_eq!(entries[0].decay_score, 1.0);
        assert!(entries[0].decay_updated_at.is_some());
    }

    #[test]
    fn decay_depends_on_elapsed_time_not_number_of_updates() {
        let now = chrono::Utc::now();
        let mut entry = LongTermMemoryEntry::new("kept");
        entry.importance = MemoryImportance::Low;
        let mut sparse = vec![entry.clone()];
        let mut frequent = vec![entry];
        apply_memory_decay(&mut sparse, now);
        apply_memory_decay(&mut frequent, now);
        for hour in 1..=720 {
            apply_memory_decay(&mut frequent, now + chrono::Duration::hours(hour));
        }
        apply_memory_decay(&mut sparse, now + chrono::Duration::days(30));
        assert_eq!(frequent[0].decay_score, sparse[0].decay_score);
        let saved = serde_json::to_string(&frequent).unwrap();
        let mut restored = serde_json::from_str(&saved).unwrap();
        apply_memory_decay(&mut restored, now + chrono::Duration::days(31));
        apply_memory_decay(&mut frequent, now + chrono::Duration::days(31));
        assert_eq!(restored, frequent);
    }

    #[test]
    fn memory_compression_by_tokens() {
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
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
                decay_updated_at: None,
                decay_remainder: 0.0,
                source: "test".to_string(),
                confidence: 0.7,
                source_candidate_id: None,
                source_task_id: None,
                agent_id: None,
            }],
        };

        let _evicted = apply_memory_decay(&mut memory.entries, now);

        apply_memory_decay(&mut memory.entries, now + chrono::Duration::days(30));
        // 首次建立基线，随后按实际经过的 30 天扣分；Critical 不驱逐。
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

    #[test]
    fn compression_skips_when_summarization_workitem_inflight() {
        use crate::domain::{SummarizationTrigger, WorkItem};
        use bevy_ecs::system::RunSystemOnce;

        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
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
        let task_id = task.id;
        let entity = world.spawn((task, ShortTermMemory::default())).id();
        {
            let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
            for i in 0..15 {
                stm.add_entry(
                    EntryRole::User,
                    format!(
                        "This is message number {} with sufficiently long content to exceed threshold",
                        i
                    ),
                    Default::default(),
                );
            }
        }

        // 该 task 已有一个在飞的 Summarization WorkItem
        let mut wi = WorkItem::summarization(
            task_id,
            "compress".to_string(),
            100,
            SummarizationTrigger::TokenThreshold,
        );
        wi.start();
        world.spawn(wi);

        // 前置条件：STM 必须明确超过阈值，否则测试退化为"未触发"而非"在飞保护"
        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert!(
            stm.estimated_tokens > 100,
            "test precondition: STM must exceed threshold, got {}",
            stm.estimated_tokens,
        );

        let _ = world.run_system_once(super::memory_compression_system);

        let requests = world
            .query::<&crate::domain::SummarizationRequestMessage>()
            .iter(&world)
            .count();
        assert_eq!(
            requests, 0,
            "must not trigger a new summarization while one is inflight"
        );
    }
}
