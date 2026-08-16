use crate::prelude::Component;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tiktoken_rs::cl100k_base;
use tracing::debug;

/// 标记刚写入长期记忆、尚未触发 `on_long_term_memory_write` 观察 hook 的 Agent entity。
///
/// 由 `init_agent_memory_system` 或运行时写入长期记忆的系统附带，
/// 由 companion 系统 `on_ltm_write_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct LtmWriteHookPending;

/// 标记刚发生长期记忆驱逐、尚未触发 `on_long_term_memory_evicted` 观察 hook 的 Agent entity。
///
/// 由 `long_term_memory_decay_system` 在检测到驱逐后附带，
/// 由 companion 系统 `on_ltm_evicted_hook_system` 派发 hook 后移除。
#[derive(Component, Debug, Clone, Default)]
pub struct LtmEvictedHookPending;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_entry_new_creates_user_entry() {
        let entry = MemoryEntry::new(EntryRole::User, "hello");
        assert_eq!(entry.role, EntryRole::User);
        assert_eq!(entry.content, "hello");
    }

    #[test]
    fn short_term_memory_default_is_empty() {
        let memory = ShortTermMemory::default();
        assert!(memory.entries.is_empty());
        assert_eq!(memory.estimated_tokens, 0);
        assert!(memory.summary_prefix.is_none());
    }

    #[test]
    fn short_term_memory_add_entry_updates_tokens() {
        let mut memory = ShortTermMemory::default();
        memory.add_entry(EntryRole::User, "hello world", EntryMetadata::default());
        assert_eq!(memory.entries.len(), 1);
        assert!(memory.estimated_tokens > 0);
    }

    #[test]
    fn estimate_tokens_returns_positive() {
        let tokens = estimate_tokens("Hello, world!");
        assert!(tokens > 0);
    }

    #[test]
    fn long_term_memory_default_is_empty() {
        let memory = LongTermMemory::default();
        assert!(memory.entries.is_empty());
    }

    #[test]
    fn long_term_memory_entry_defaults_to_decay_ready_state() {
        let entry = LongTermMemoryEntry::new("Always prefer truthful shell semantics");

        assert_eq!(entry.reuse_count, 0);
        assert!(!entry.pin);
        assert_eq!(entry.importance, MemoryImportance::Medium);
        assert!(entry.decay_score > 0.0);
    }

    #[test]
    fn memory_snapshot_new_sets_current_schema_version() {
        let entry = LongTermMemoryEntry::new("test content");
        let snapshot = MemorySnapshot::new("test-agent", vec![entry]);

        assert_eq!(
            snapshot.schema_version,
            MemorySnapshot::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(snapshot.agent_name, "test-agent");
        assert_eq!(snapshot.entries.len(), 1);
    }

    #[test]
    fn memory_snapshot_round_trip_serialization() {
        let entry = LongTermMemoryEntry::new("important fact");
        let snapshot = MemorySnapshot::new("summarizer", vec![entry]);

        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: MemorySnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.agent_name, "summarizer");
        assert_eq!(deserialized.entries.len(), 1);
        assert_eq!(deserialized.entries[0].content, "important fact");
    }

    #[test]
    fn executable_memory_entry_keeps_asset_refs_readable() {
        let entry = ExecutableMemoryEntry {
            memory_id: uuid::Uuid::new_v4(),
            title: "shell smoke test".to_string(),
            intent: "run a reusable smoke test".to_string(),
            when_to_use: "after changing shell orchestration".to_string(),
            asset_refs: vec!["default-agent/asset-1-shell-smoke.sh".to_string()],
            dependency_refs: vec![],
        };

        assert_eq!(entry.asset_refs.len(), 1);
        assert!(entry.asset_refs[0].contains("shell-smoke"));
    }

    #[test]
    fn long_term_memory_entry_carries_source_traceability() {
        let mut entry = LongTermMemoryEntry::new("traceable fact");
        entry.source_candidate_id = Some(uuid::Uuid::new_v4());
        entry.source_task_id = Some(uuid::Uuid::new_v4());
        entry.agent_id = Some(uuid::Uuid::new_v4());

        assert!(entry.source_candidate_id.is_some());
        assert!(entry.source_task_id.is_some());
        assert!(entry.agent_id.is_some());
    }

    #[test]
    fn add_entry_dedups_by_source_candidate_id() {
        let mut memory = LongTermMemory::with_name("dedup-agent");
        let candidate_id = uuid::Uuid::new_v4();
        let mut entry = LongTermMemoryEntry::new("content");
        entry.source_candidate_id = Some(candidate_id);

        memory.add_entry(entry.clone());
        memory.add_entry(entry);

        assert_eq!(memory.entries.len(), 1);
    }

    #[test]
    fn add_entry_includes_tool_calls_tokens() {
        let mut stm = ShortTermMemory::default();
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "ls -la /very/long/path/that/should/contribute/tokens".to_string(),
            output: "file1.txt\nfile2.txt\nfile3.txt\nfile4.txt\nfile5.txt".to_string(),
            timestamp: chrono::Utc::now(),
        });

        let tokens_before = stm.estimated_tokens;
        stm.add_entry(EntryRole::Assistant, "done", metadata);

        // estimated_tokens should be strictly greater than just "done" tokens
        let content_only_tokens = estimate_tokens("done");
        assert!(
            stm.estimated_tokens > tokens_before + content_only_tokens,
            "add_entry should include tool_calls tokens, got {} but expected > {}",
            stm.estimated_tokens,
            tokens_before + content_only_tokens,
        );
    }

    #[test]
    fn recalculate_tokens_includes_tool_calls() {
        let mut stm = ShortTermMemory::default();
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "ls -la /very/long/path".to_string(),
            output: "file1.txt\nfile2.txt\nfile3.txt".to_string(),
            timestamp: chrono::Utc::now(),
        });
        stm.add_entry(EntryRole::Assistant, "result text", metadata);

        // Corrupt estimated_tokens manually, then recalculate
        stm.estimated_tokens = 0;
        stm.recalculate_tokens();

        let content_tokens = estimate_tokens("result text");
        let tool_tokens = estimate_tokens("ls -la /very/long/path")
            + estimate_tokens("file1.txt\nfile2.txt\nfile3.txt");
        assert!(
            stm.estimated_tokens >= content_tokens + tool_tokens,
            "recalculate_tokens should include tool_calls, got {}",
            stm.estimated_tokens,
        );
    }

    #[test]
    fn record_tool_call_updates_estimated_tokens() {
        let mut stm = ShortTermMemory::default();
        stm.add_entry(EntryRole::User, "hello", EntryMetadata::default());

        let tokens_before = stm.estimated_tokens;
        stm.record_tool_call(
            Some("call_1".to_string()),
            "shell_exec".to_string(),
            "ls -la /some/path".to_string(),
            "file1.txt\nfile2.txt\nfile3.txt".to_string(),
            chrono::Utc::now(),
        );

        assert!(
            stm.estimated_tokens > tokens_before,
            "record_tool_call should update estimated_tokens, got {} but was {}",
            stm.estimated_tokens,
            tokens_before,
        );
    }

    #[test]
    fn render_tool_calls_summary_format() {
        let tool_calls = vec![
            ToolCall {
                id: Some("call_1".to_string()),
                tool_name: "shell_exec".to_string(),
                input: "ls".to_string(),
                output: "file1.txt\nfile2.txt".to_string(),
                timestamp: chrono::Utc::now(),
            },
            ToolCall {
                id: Some("call_2".to_string()),
                tool_name: "shell_exec".to_string(),
                input: "cat x".to_string(),
                output: "content of x".to_string(),
                timestamp: chrono::Utc::now(),
            },
        ];

        let summary = render_tool_calls_summary(&tool_calls);
        assert!(
            summary.contains("shell_exec(\"ls\")"),
            "should contain tool call summary"
        );
        assert!(
            summary.contains("file1.txt"),
            "should contain truncated output"
        );
        assert!(
            summary.contains("shell_exec(\"cat x\")"),
            "should contain second tool call"
        );
    }

    /// 复现日志 harness_2026-08-15_23-56-36.jsonl 的分组场景：
    /// [User(78字符), Assistant(含大 tool_calls), User, Assistant] → 3 组
    fn log_scenario_stm() -> ShortTermMemory {
        let mut stm = ShortTermMemory::default();
        stm.add_entry(
            EntryRole::User,
            "帮我看今天的新闻",
            EntryMetadata::default(),
        );
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(ToolCall {
            id: Some("call_1".to_string()),
            tool_name: "shell_exec".to_string(),
            input: "playwright-cli browse".to_string(),
            output: "huge news page content".repeat(2000),
            timestamp: chrono::Utc::now(),
        });
        stm.add_entry(EntryRole::Assistant, String::new(), metadata);
        stm.add_entry(EntryRole::User, "总结一下", EntryMetadata::default());
        stm.add_entry(EntryRole::Assistant, "好的", EntryMetadata::default());
        stm
    }

    #[test]
    fn split_into_groups_tool_entry_forms_own_group() {
        let stm = log_scenario_stm();
        let groups = split_into_groups(&stm.entries);
        assert_eq!(groups, vec![vec![0], vec![1], vec![2, 3]]);
    }

    #[test]
    fn compressible_entry_count_protects_recent_groups() {
        let stm = log_scenario_stm();
        let groups = split_into_groups(&stm.entries);

        // preserve=2（默认）：保留工具组与最近对话组，仅组 0 可压缩（日志中的 78 字符）
        assert_eq!(compressible_entry_count(&groups, 2), 1);
        // preserve=1：工具组落入压缩区
        assert_eq!(compressible_entry_count(&groups, 1), 2);
        // 组数 <= preserve：无可压缩
        assert_eq!(compressible_entry_count(&groups, 3), 0);
        assert_eq!(compressible_entry_count(&groups, 4), 0);
        // 空分组
        assert_eq!(compressible_entry_count(&[], 2), 0);
    }

    #[test]
    fn drain_compressed_groups_removes_only_leading_groups() {
        let mut stm = log_scenario_stm();
        let tokens_before = stm.estimated_tokens;

        // 摘要完成后 drain（preserve=2 默认配置）：仅移除组 0 的 1 个 entry。
        // 与完成端流程一致：drain 后由 recalculate_tokens 反映 token 下降
        let removed = stm.drain_compressed_groups(2);
        stm.recalculate_tokens();
        assert_eq!(removed, 1);
        assert_eq!(stm.entries.len(), 3);
        assert!(stm.estimated_tokens < tokens_before);

        // 二次 drain：保护窗口已满，无进展 → 触发端将停止，循环终止
        assert_eq!(stm.drain_compressed_groups(2), 0);
        assert_eq!(stm.entries.len(), 3);
    }

    #[test]
    fn drain_compressed_groups_then_next_turn_exposes_tool_group() {
        // 终止后用户追加一轮对话，工具组落出保护窗口，可被后续压缩
        let mut stm = log_scenario_stm();
        stm.drain_compressed_groups(2);

        stm.add_entry(EntryRole::User, "新的问题", EntryMetadata::default());
        stm.add_entry(EntryRole::Assistant, "新的回答", EntryMetadata::default());

        // 此时分组 [[工具组], [对话组], [新对话组]]：工具组成为可压缩区
        let removed = stm.drain_compressed_groups(2);
        assert_eq!(removed, 1);
        assert!(stm
            .entries
            .iter()
            .all(|e| e.metadata.tool_calls.is_empty()));
    }
}

/// 估算文本的 token 数
pub fn estimate_tokens(text: &str) -> u32 {
    cl100k_base()
        .map(|enc| enc.encode_with_special_tokens(text).len() as u32)
        .unwrap_or_else(|_| (text.len() / 4) as u32) // fallback: 4 chars ≈ 1 token
}

/// 将 STM entries 按配对组切分。
///
/// 配对组定义：
/// - User 开启新的对话配对组
/// - Assistant（无 tool_calls）归入当前对话配对组
/// - Assistant（有 tool_calls）开启新的工具配对组（原子性锚点）
/// - Summary / Archive 归入最近的配对组
///
/// 触发端（`memory_compression_system`）与完成端
/// （`ShortTermMemory::drain_compressed_groups`）共用此分组逻辑，
/// 保证两端粒度对齐。
pub fn split_into_groups(entries: &[MemoryEntry]) -> Vec<Vec<usize>> {
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

/// 计算可压缩的 entry 数量：排除最后 `preserve_recent_turns` 个配对组后，
/// 前置各组包含的 entry 总数。组数不足时返回 0。
///
/// 触发端（`memory_compression_system`）与完成端
/// （`ShortTermMemory::drain_compressed_groups`）共用此选择逻辑，
/// 保证两端粒度对齐。
pub fn compressible_entry_count(groups: &[Vec<usize>], preserve_recent_turns: u32) -> usize {
    let preserve = preserve_recent_turns as usize;
    if groups.len() <= preserve {
        return 0;
    }
    groups
        .iter()
        .take(groups.len() - preserve)
        .map(|g| g.len())
        .sum()
}

/// 短期记忆条目。
///
/// `MemoryEntry` 仅用于 `ShortTermMemory` 的对话与摘要条目，
/// 不再作为长期记忆或共享知识的底层模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    pub role: EntryRole,
    pub content: String,
    pub metadata: EntryMetadata,
}

impl MemoryEntry {
    pub fn new(role: EntryRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            metadata: EntryMetadata::default(),
        }
    }

    pub fn with_metadata(mut self, metadata: EntryMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// 记忆条目角色
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EntryRole {
    User,
    Assistant,
    Summary,
    Archive,
}

/// 记忆条目元数据
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryMetadata {
    pub tool_calls: Vec<ToolCall>,
    pub resources: Vec<String>,
    pub reasoning: Option<String>,
    pub keywords: Vec<String>,
}

/// 工具调用记录
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: Option<String>,
    pub tool_name: String,
    pub input: String,
    pub output: String,
    pub timestamp: DateTime<Utc>,
}

/// 短期记忆（绑定 Task）
#[derive(Component, Default, Clone)]
pub struct ShortTermMemory {
    /// 完整对话条目
    pub entries: Vec<MemoryEntry>,
    /// 摘要前缀（压缩后的旧内容）
    pub summary_prefix: Option<String>,
    /// 当前 token 估算
    pub estimated_tokens: u32,
    /// 最后一次缓存命中的 token 数
    pub last_cached_tokens: Option<u32>,
}

impl ShortTermMemory {
    /// 计算真实对话轮数（仅 User + Assistant 配对，Summary/Archive 不计入）
    pub fn dialog_turn_count(&self) -> u32 {
        let dialog_entries = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.role, EntryRole::User | EntryRole::Assistant))
            .count();

        (dialog_entries / 2) as u32
    }

    /// 记录 Tool 调用
    ///
    /// 将 Tool 调用记录追加到最后一个 Assistant 条目的元数据中，
    /// 如果没有 Assistant 条目则创建一个新的。
    pub fn record_tool_call(
        &mut self,
        id: Option<String>,
        tool_name: String,
        input: String,
        output: String,
        timestamp: DateTime<Utc>,
    ) {
        let tokens_added = estimate_tokens(&input) + estimate_tokens(&output);
        let tool_call = ToolCall {
            id,
            tool_name,
            input,
            output,
            timestamp,
        };

        // 查找最后一个 Assistant 条目
        if let Some(last_entry) = self.entries.last_mut()
            && last_entry.role == EntryRole::Assistant
        {
            last_entry.metadata.tool_calls.push(tool_call);
            self.estimated_tokens += tokens_added;
            return;
        }

        // 如果没有 Assistant 条目，创建一个新的
        let mut metadata = EntryMetadata::default();
        metadata.tool_calls.push(tool_call);
        self.entries.push(MemoryEntry {
            role: EntryRole::Assistant,
            content: String::new(),
            metadata,
        });
        self.estimated_tokens += tokens_added;
    }

    /// 添加新条目
    pub fn add_entry(
        &mut self,
        role: EntryRole,
        content: impl Into<String>,
        metadata: EntryMetadata,
    ) {
        let content = content.into();
        let mut tokens_added = estimate_tokens(&content);
        for tc in &metadata.tool_calls {
            tokens_added += estimate_tokens(&tc.input);
            tokens_added += estimate_tokens(&tc.output);
        }
        self.estimated_tokens += tokens_added;
        let entry = MemoryEntry::new(role, content.clone()).with_metadata(metadata);
        self.entries.push(entry);
        debug!(
            event = "StmEntryAdded",
            role = ?role,
            content = %content,
            content_len = content.len(),
            entry_tokens = tokens_added,
            total_tokens = self.estimated_tokens,
            total_entries = self.entries.len(),
            "short term memory entry added"
        );
    }

    /// 重新计算 token 估算
    pub fn recalculate_tokens(&mut self) {
        let old_tokens = self.estimated_tokens;
        let mut total = 0u32;
        if let Some(summary) = &self.summary_prefix {
            total += estimate_tokens(summary);
        }
        for entry in &self.entries {
            total += estimate_tokens(&entry.content);
            for tc in &entry.metadata.tool_calls {
                total += estimate_tokens(&tc.input);
                total += estimate_tokens(&tc.output);
            }
        }
        self.estimated_tokens = total;
        debug!(
            event = "StmTokensRecalculated",
            old_tokens = old_tokens,
            new_tokens = total,
            entries_count = self.entries.len(),
            has_summary_prefix = self.summary_prefix.is_some(),
            "STM tokens recalculated"
        );
    }

    /// 摘要完成后移除已压缩的 entries。
    ///
    /// 与触发端 `memory_compression_system` 使用同一份
    /// `split_into_groups` + `compressible_entry_count` 选择逻辑：
    /// 移除的是被压缩组的前置 entries，保留最后 `preserve_recent_turns`
    /// 个配对组。返回实际移除的 entry 数。
    pub fn drain_compressed_groups(&mut self, preserve_recent_turns: u32) -> usize {
        let groups = split_into_groups(&self.entries);
        let count = compressible_entry_count(&groups, preserve_recent_turns);
        if count > 0 {
            self.entries.drain(0..count);
        }
        count
    }
}

/// 渲染工具调用摘要，用于 compress_text、prompt_builder 和还原逻辑。
///
/// 格式：`[Tool calls: tool_name("input") → output_preview; ...]`
/// 输出截断至 200 字符避免膨胀。
pub fn render_tool_calls_summary(tool_calls: &[ToolCall]) -> String {
    if tool_calls.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = tool_calls
        .iter()
        .map(|tc| {
            let output_preview = if tc.output.chars().count() > 200 {
                let truncated: String = tc.output.chars().take(200).collect();
                format!("{}...[truncated]", truncated)
            } else {
                tc.output.clone()
            };
            format!("{}(\"{}\") → {}", tc.tool_name, tc.input, output_preview)
        })
        .collect();
    format!("[Tool calls: {}]", parts.join("; "))
}

/// 长期记忆持久化快照。
///
/// JSON 文件不直接裸写 `Vec<LongTermMemoryEntry>`，而是使用带元信息的快照结构，
/// 便于后续兼容迁移和可调试性。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySnapshot {
    /// Agent 原始名称
    pub agent_name: String,
    /// 快照版本，用于后续兼容迁移
    pub schema_version: u32,
    /// 最后一次成功写盘时间
    pub updated_at: DateTime<Utc>,
    /// 当前 Agent 的全部长期记忆条目
    pub entries: Vec<LongTermMemoryEntry>,
}

impl MemorySnapshot {
    /// 当前快照版本
    pub const CURRENT_SCHEMA_VERSION: u32 = 2;

    /// 创建新的快照。
    pub fn new(agent_name: impl Into<String>, entries: Vec<LongTermMemoryEntry>) -> Self {
        Self {
            agent_name: agent_name.into(),
            schema_version: Self::CURRENT_SCHEMA_VERSION,
            updated_at: Utc::now(),
            entries,
        }
    }
}

/// 长期记忆重要度。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryImportance {
    Low,
    Medium,
    High,
    Critical,
}

/// Agent 长期记忆条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LongTermMemoryEntry {
    pub content: String,
    pub scope_tags: Vec<String>,
    pub importance: MemoryImportance,
    pub pin: bool,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub reuse_count: u32,
    pub decay_score: f32,
    pub source: String,
    pub confidence: f32,
    #[serde(default)]
    pub source_candidate_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub source_task_id: Option<super::TaskId>,
    #[serde(default)]
    pub agent_id: Option<super::AgentId>,
}

impl LongTermMemoryEntry {
    /// 创建默认可衰退的长期记忆条目。
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            scope_tags: Vec::new(),
            importance: MemoryImportance::Medium,
            pin: false,
            created_at: Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            decay_score: 1.0,
            source: "manual".to_string(),
            confidence: 0.8,
            source_candidate_id: None,
            source_task_id: None,
            agent_id: None,
        }
    }
}

/// 长期记忆（绑定 Agent）。
#[derive(Component, Default, Clone)]
pub struct LongTermMemory {
    /// 关联 Agent 的稳定名称，用于持久化身份锚点。
    pub agent_name: Option<String>,
    /// 长期记忆条目。
    pub entries: Vec<LongTermMemoryEntry>,
}

/// 可执行经验条目：经过治理和确认后的可执行经验，属于长期资产体系。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutableMemoryEntry {
    pub memory_id: uuid::Uuid,
    pub title: String,
    pub intent: String,
    pub when_to_use: String,
    pub asset_refs: Vec<String>,
    pub dependency_refs: Vec<String>,
}

impl LongTermMemory {
    /// 创建带 Agent 名称的长期记忆。
    pub fn with_name(agent_name: impl Into<String>) -> Self {
        Self {
            agent_name: Some(agent_name.into()),
            entries: Vec::new(),
        }
    }

    /// 添加长期记忆条目。
    pub fn add_entry(&mut self, entry: LongTermMemoryEntry) {
        if let Some(candidate_id) = entry.source_candidate_id
            && self
                .entries
                .iter()
                .any(|e| e.source_candidate_id == Some(candidate_id))
        {
            return;
        }
        self.entries.push(entry);
    }

    /// 添加兼容旧调用点的归档条目。
    pub fn add_archive(&mut self, content: impl Into<String>) {
        let content = content.into();
        debug!(
            event = "LtmArchiveAdded",
            content = %content,
            content_len = content.len(),
            total_entries = self.entries.len(),
            "long term memory archive added"
        );
        self.entries.push(LongTermMemoryEntry::new(content));
    }

    /// 吸收来自子 Agent 的长期记忆条目。
    pub fn absorb(&mut self, entries: Vec<LongTermMemoryEntry>) {
        let absorbing_count = entries.len();
        let total_before = self.entries.len();
        debug!(
            event = "LtmAbsorb",
            absorbing_count = absorbing_count,
            total_entries_before = total_before,
            total_entries_after = total_before + absorbing_count,
            absorbing_entries = ?entries.iter().map(|entry| &entry.content).collect::<Vec<_>>(),
            "long term memory absorbed entries"
        );
        self.entries.extend(entries);
    }
}
