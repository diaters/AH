use std::cmp::Reverse;

use crate::domain::{LongTermMemory, LongTermMemoryEntry, MemoryImportance, estimate_tokens};

/// prompt 注入预算。
#[derive(Debug, Clone, Copy)]
pub struct MemorySelectionBudget {
    pub max_core_entries: usize,
    pub max_relevant_entries: usize,
    pub max_relevant_tokens: u32,
}

/// 选中的长期记忆集合。
#[derive(Debug, Default)]
pub struct SelectedLongTermMemories {
    pub core: Vec<LongTermMemoryEntry>,
    pub relevant: Vec<LongTermMemoryEntry>,
}

/// 根据当前任务选择需要注入 prompt 的长期记忆。
pub fn select_long_term_memories(
    task_content: &str,
    long_term: &LongTermMemory,
    budget: MemorySelectionBudget,
) -> SelectedLongTermMemories {
    let task_keywords = extract_keywords(task_content);

    let mut core: Vec<_> = long_term
        .entries
        .iter()
        .filter(|entry| entry.pin && entry.confidence >= 0.8 && entry.decay_score > 0.2)
        .cloned()
        .collect();
    core.sort_by_key(|entry| {
        (
            Reverse(entry.importance),
            Reverse((entry.confidence * 100.0) as i32),
            Reverse(entry.last_accessed_at.unwrap_or(entry.created_at)),
        )
    });
    core.truncate(budget.max_core_entries);

    let mut relevant: Vec<_> = long_term
        .entries
        .iter()
        .filter(|entry| !entry.pin)
        .filter(|entry| entry.decay_score > 0.2)
        .filter_map(|entry| {
            let score = relevance_score(&task_keywords, entry);
            (score > 0).then_some((score, entry.clone()))
        })
        .collect();

    relevant.sort_by_key(|(score, entry)| {
        (
            Reverse(*score),
            Reverse(entry.importance),
            Reverse(entry.reuse_count),
            Reverse(entry.last_accessed_at.unwrap_or(entry.created_at)),
            Reverse((entry.confidence * 100.0) as i32),
        )
    });

    let mut selected_relevant = Vec::new();
    let mut used_tokens = 0;
    for (_, entry) in relevant.into_iter().take(budget.max_relevant_entries) {
        let entry_tokens = estimate_tokens(&entry.content);
        if used_tokens + entry_tokens > budget.max_relevant_tokens {
            continue;
        }
        used_tokens += entry_tokens;
        selected_relevant.push(entry);
    }

    SelectedLongTermMemories {
        core,
        relevant: selected_relevant,
    }
}

/// 提取用于轻量相关性匹配的关键词。
fn extract_keywords(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|segment| segment.len() >= 2)
        .map(|segment| segment.to_lowercase())
        .collect()
}

/// 计算长期记忆和当前任务之间的轻量相关性分数。
fn relevance_score(task_keywords: &[String], entry: &LongTermMemoryEntry) -> u32 {
    let content_keywords = extract_keywords(&entry.content);

    let keyword_matches = content_keywords
        .iter()
        .filter(|keyword| task_keywords.contains(keyword))
        .count() as u32;
    let tag_matches = entry
        .scope_tags
        .iter()
        .filter(|tag| {
            task_keywords
                .iter()
                .any(|keyword| keyword == &tag.to_lowercase())
        })
        .count() as u32;
    let importance_weight = match entry.importance {
        MemoryImportance::Low => 0,
        MemoryImportance::Medium => 1,
        MemoryImportance::High => 2,
        MemoryImportance::Critical => 3,
    };

    keyword_matches * 3 + tag_matches * 5 + importance_weight
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LongTermMemoryKind, MemoryImportance};

    #[test]
    fn select_long_term_memories_skips_unrelated_and_low_decay_entries() {
        let mut long_term = LongTermMemory::default();
        long_term.entries.push(LongTermMemoryEntry {
            content: "Shell tools should expose honest waiting semantics".to_string(),
            kind: LongTermMemoryKind::Constraint,
            scope_tags: vec!["shell".to_string()],
            importance: MemoryImportance::Critical,
            pin: true,
            created_at: chrono::Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            decay_score: 1.0,
            source: "test".to_string(),
            confidence: 1.0,
        });
        long_term.entries.push(LongTermMemoryEntry {
            content: "frontend color tweak".to_string(),
            kind: LongTermMemoryKind::Preference,
            scope_tags: vec!["ui".to_string()],
            importance: MemoryImportance::Low,
            pin: false,
            created_at: chrono::Utc::now(),
            last_accessed_at: None,
            reuse_count: 0,
            decay_score: 0.1,
            source: "test".to_string(),
            confidence: 0.7,
        });

        let selected = select_long_term_memories(
            "fix shell timeout",
            &long_term,
            MemorySelectionBudget {
                max_core_entries: 5,
                max_relevant_entries: 5,
                max_relevant_tokens: 800,
            },
        );

        assert_eq!(selected.core.len(), 1);
        assert!(
            selected.core[0]
                .content
                .contains("honest waiting semantics")
        );
        assert!(selected.relevant.is_empty());
    }
}
