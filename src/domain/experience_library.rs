//! 经验库领域类型：经用户确认后沉淀的试错经验。
//!
//! 每条经验是一份带 TOML front matter 的 Markdown，正文按固定章节组织
//! （适用场景 / 可行做法 / 走不通的路线 / 前提）。检索时由经验管理员判断相关性，
//! 返回给主 Agent 的内容由 harness 从原文逐字拼装，避免转述丢失命令与报错细节。

use std::collections::HashSet;

use bevy_ecs::prelude::Component;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{AgentId, AgentRequestKind, ChannelId};

/// 正文推荐的二级标题，按顺序出现。
pub const EXPERIENCE_SECTIONS: [&str; 4] = ["适用场景", "可行做法", "走不通的路线", "前提"];
pub const MAX_EXPERIENCE_TITLE_CHARS: usize = 120;
pub const MAX_EXPERIENCE_BODY_BYTES: usize = 32 * 1024;
/// 检索时交给经验管理员的目录上限；超出时先按词面重合度预筛。
pub const LIBRARIAN_INDEX_LIMIT: usize = 80;
/// 单次检索最多返回的经验条数。
pub const MAX_LOOKUP_MATCHES: usize = 3;
/// 目录中每条适用场景的字符上限。
const INDEX_SCENARIO_CHARS: usize = 300;
const FRONT_MATTER_FENCE: &str = "+++";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExperienceError {
    #[error("invalid experience id")]
    InvalidId,
    #[error("experience title is empty")]
    EmptyTitle,
    #[error("experience title exceeds {MAX_EXPERIENCE_TITLE_CHARS} characters")]
    TitleTooLong,
    #[error("experience body is empty")]
    EmptyBody,
    #[error("experience body exceeds {MAX_EXPERIENCE_BODY_BYTES} bytes")]
    BodyTooLarge,
    #[error("malformed experience file: {0}")]
    Malformed(String),
}

/// 一条经验。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceEntry {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 来源客户端会话，便于回看原始过程。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_session: Option<String>,
    pub body: String,
}

#[derive(Serialize, Deserialize)]
struct FrontMatter {
    title: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_session: Option<String>,
}

/// 经验列表项：客户端列表与管理员目录共用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExperienceSummary {
    pub id: String,
    pub title: String,
    pub scenario: String,
    pub updated_at: DateTime<Utc>,
}

/// 经验 ID 只接受规范小写 UUID，同时充当文件名，杜绝路径穿越。
pub fn is_valid_experience_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok_and(|parsed| parsed.hyphenated().to_string() == id)
}

impl ExperienceEntry {
    pub fn new(
        title: &str,
        body: &str,
        source_session: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, ExperienceError> {
        let entry = Self {
            id: Uuid::new_v4().to_string(),
            title: title.trim().to_string(),
            created_at: now,
            updated_at: now,
            source_session,
            body: body.trim().to_string(),
        };
        entry.validate()?;
        Ok(entry)
    }

    /// 以新内容覆盖标题与正文，保留创建时间与来源。
    pub fn revise(
        &mut self,
        title: &str,
        body: &str,
        now: DateTime<Utc>,
    ) -> Result<(), ExperienceError> {
        let mut next = self.clone();
        next.title = title.trim().to_string();
        next.body = body.trim().to_string();
        next.updated_at = now;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ExperienceError> {
        if !is_valid_experience_id(&self.id) {
            return Err(ExperienceError::InvalidId);
        }
        if self.title.is_empty() {
            return Err(ExperienceError::EmptyTitle);
        }
        if self.title.chars().count() > MAX_EXPERIENCE_TITLE_CHARS || self.title.contains('\n') {
            return Err(ExperienceError::TitleTooLong);
        }
        if self.body.is_empty() {
            return Err(ExperienceError::EmptyBody);
        }
        if self.body.len() > MAX_EXPERIENCE_BODY_BYTES {
            return Err(ExperienceError::BodyTooLarge);
        }
        Ok(())
    }

    pub fn to_markdown(&self) -> String {
        let front = FrontMatter {
            title: self.title.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            source_session: self.source_session.clone(),
        };
        let front = toml::to_string(&front).expect("front matter is always serializable");
        format!(
            "{FRONT_MATTER_FENCE}\n{front}{FRONT_MATTER_FENCE}\n\n{}\n",
            self.body
        )
    }

    pub fn from_markdown(id: &str, text: &str) -> Result<Self, ExperienceError> {
        if !is_valid_experience_id(id) {
            return Err(ExperienceError::InvalidId);
        }
        let rest = text
            .strip_prefix(FRONT_MATTER_FENCE)
            .and_then(|rest| rest.strip_prefix('\n'))
            .ok_or_else(|| ExperienceError::Malformed("missing front matter".into()))?;
        let end = rest
            .find(&format!("\n{FRONT_MATTER_FENCE}"))
            .ok_or_else(|| ExperienceError::Malformed("unterminated front matter".into()))?;
        let front: FrontMatter = toml::from_str(&rest[..end])
            .map_err(|error| ExperienceError::Malformed(error.to_string()))?;
        let body = rest[end + 1 + FRONT_MATTER_FENCE.len()..]
            .trim()
            .to_string();
        let entry = Self {
            id: id.to_string(),
            title: front.title.trim().to_string(),
            created_at: front.created_at,
            updated_at: front.updated_at,
            source_session: front.source_session,
            body,
        };
        entry.validate()?;
        Ok(entry)
    }

    /// 读取 `## 章节名` 下的内容，直到下一个二级标题。
    pub fn section(&self, name: &str) -> Option<&str> {
        let mut start = None;
        let mut offset = 0;
        for line in self.body.split_inclusive('\n') {
            let heading = line.trim_end().strip_prefix("## ").map(str::trim);
            match (start, heading) {
                (None, Some(title)) if title == name => start = Some(offset + line.len()),
                (Some(begin), Some(_)) => return Some(self.body[begin..offset].trim()),
                _ => {}
            }
            offset += line.len();
        }
        start.map(|begin| self.body[begin..].trim())
    }

    /// 检索用的适用场景摘要；缺少章节时退回正文开头。
    pub fn scenario(&self) -> String {
        let text = self
            .section(EXPERIENCE_SECTIONS[0])
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.body);
        truncate_chars(text, INDEX_SCENARIO_CHARS)
    }

    pub fn summary(&self) -> ExperienceSummary {
        ExperienceSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            scenario: self.scenario(),
            updated_at: self.updated_at,
        }
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(limit).collect();
    out.push('…');
    out
}

// ============ 词面预筛 ============

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}

fn is_ascii_term_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '/')
}

/// 提取检索词：中文按相邻两字切分，ASCII 按单词（保留下划线、点号等，便于命中命令与报错码）。
pub fn search_terms(text: &str) -> HashSet<String> {
    let mut terms = HashSet::new();
    let mut cjk_run: Vec<char> = Vec::new();
    let mut word = String::new();
    let flush_word = |word: &mut String, terms: &mut HashSet<String>| {
        let term = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if term.len() >= 2 {
            terms.insert(term.to_ascii_lowercase());
        }
        word.clear();
    };
    let flush_cjk = |run: &mut Vec<char>, terms: &mut HashSet<String>| {
        for pair in run.windows(2) {
            terms.insert(pair.iter().collect());
        }
        run.clear();
    };
    for ch in text.chars() {
        if is_cjk(ch) {
            flush_word(&mut word, &mut terms);
            cjk_run.push(ch);
        } else if is_ascii_term_char(ch) {
            flush_cjk(&mut cjk_run, &mut terms);
            word.push(ch);
        } else {
            flush_word(&mut word, &mut terms);
            flush_cjk(&mut cjk_run, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_cjk(&mut cjk_run, &mut terms);
    terms
}

/// 按与查询的词面重合度排序，返回得分大于零的前 `limit` 条。
pub fn rank_by_overlap<'a>(
    query: &str,
    entries: &'a [ExperienceEntry],
    limit: usize,
) -> Vec<&'a ExperienceEntry> {
    let query_terms = search_terms(query);
    let mut scored: Vec<(usize, &ExperienceEntry)> = entries
        .iter()
        .map(|entry| {
            let terms = search_terms(&format!("{}\n{}", entry.title, entry.body));
            (query_terms.intersection(&terms).count(), entry)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated_at.cmp(&a.1.updated_at)));
    scored.into_iter().take(limit).map(|(_, e)| e).collect()
}

/// 管理员目录：库较小时全量，否则按词面预筛。
pub fn librarian_index<'a>(
    query: &str,
    entries: &'a [ExperienceEntry],
) -> Vec<&'a ExperienceEntry> {
    if entries.len() <= LIBRARIAN_INDEX_LIMIT {
        entries.iter().collect()
    } else {
        rank_by_overlap(query, entries, LIBRARIAN_INDEX_LIMIT)
    }
}

// ============ 管理员提示与输出 ============

/// 从模型输出中取出 JSON 对象（容忍代码围栏与前后说明文字）。
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExperienceMatch {
    pub id: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct LookupDecision {
    #[serde(default)]
    matches: Vec<ExperienceMatch>,
}

pub fn parse_lookup_decision(text: &str) -> Result<Vec<ExperienceMatch>, String> {
    let json = extract_json_object(text).ok_or("librarian reply has no JSON object")?;
    let decision: LookupDecision =
        serde_json::from_str(json).map_err(|error| format!("invalid librarian JSON: {error}"))?;
    let mut seen = HashSet::new();
    Ok(decision
        .matches
        .into_iter()
        .filter(|m| seen.insert(m.id.clone()))
        .take(MAX_LOOKUP_MATCHES)
        .collect())
}

pub fn build_lookup_prompt(situation: &str, index: &[&ExperienceEntry]) -> String {
    let catalog: Vec<String> = index
        .iter()
        .map(|entry| {
            format!(
                "- id: {}\n  标题：{}\n  适用场景：{}",
                entry.id,
                entry.title,
                entry.scenario().replace('\n', " ")
            )
        })
        .collect();
    format!(
        "请判断经验库中是否有能直接帮助处理下面这个情况的经验。\n\n\
         ## 当前情况\n{situation}\n\n\
         ## 经验库目录\n{}\n\n\
         判断标准：\n\
         - 只选择适用场景与当前情况实质相同的经验：同一个工具、系统、报错或目标，做法可以直接迁移。\n\
         - 只是话题相近、但做法无法迁移的不要选。\n\
         - 最多选 {MAX_LOOKUP_MATCHES} 条；没有合适的就返回空数组，不要硬凑。\n\n\
         只输出 JSON，不要其他文字：{{\"matches\":[{{\"id\":\"目录中的 id\",\"reason\":\"一句话说明为什么适用\"}}]}}",
        catalog.join("\n")
    )
}

/// 按管理员选择逐字拼装经验原文；目录外的 id 被忽略。
pub fn render_lookup_result(
    matches: &[ExperienceMatch],
    entries: &[ExperienceEntry],
) -> Option<String> {
    let blocks: Vec<String> = matches
        .iter()
        .filter_map(|m| {
            let entry = entries.iter().find(|e| e.id == m.id)?;
            let reason = if m.reason.trim().is_empty() {
                String::new()
            } else {
                format!("相关原因：{}\n", m.reason.trim())
            };
            Some(format!(
                "### 经验：{}\n{reason}更新于：{}\n\n{}",
                entry.title,
                entry.updated_at.format("%Y-%m-%d"),
                entry.body
            ))
        })
        .collect();
    (!blocks.is_empty()).then(|| {
        format!(
            "以下是经验库中与当前情况相关的历史经验（原文）。优先采用其中验证过的做法，\
             避开已记录为走不通的路线；若环境前提不同，请先核实再沿用。\n\n{}",
            blocks.join("\n\n")
        )
    })
}

pub const NO_MATCHING_EXPERIENCE: &str = "经验库中没有与当前情况匹配的经验。";

/// 起草结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperienceDraftContent {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub merge_target: Option<String>,
}

pub fn parse_draft(text: &str) -> Result<ExperienceDraftContent, String> {
    let json = extract_json_object(text).ok_or("librarian reply has no JSON object")?;
    let mut draft: ExperienceDraftContent =
        serde_json::from_str(json).map_err(|error| format!("invalid draft JSON: {error}"))?;
    draft.title = draft.title.trim().to_string();
    draft.body = draft.body.trim().to_string();
    draft.merge_target = draft
        .merge_target
        .map(|id| id.trim().to_string())
        .filter(|id| is_valid_experience_id(id));
    if draft.title.is_empty() || draft.body.is_empty() {
        return Err("draft title or body is empty".into());
    }
    Ok(draft)
}

pub fn build_draft_prompt(
    transcript: &str,
    note: Option<&str>,
    similar: &[&ExperienceEntry],
) -> String {
    let note = note
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| format!("## 用户补充说明\n{n}\n\n"))
        .unwrap_or_default();
    let existing = if similar.is_empty() {
        "（无）".to_string()
    } else {
        similar
            .iter()
            .map(|e| format!("### id: {}\n标题：{}\n\n{}", e.id, e.title, e.body))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let sections = EXPERIENCE_SECTIONS
        .iter()
        .map(|s| format!("## {s}"))
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "用户希望把下面这次会话中的尝试过程沉淀为一条经验，供以后遇到类似问题时直接复用，避免重复走已经验证过走不通的路线。\n\n\
         {note}## 会话记录\n{transcript}\n\n\
         ## 经验库中可能相关的已有经验\n{existing}\n\n\
         撰写要求：\n\
         1. 重点记录试错过程：试过哪些方案、失败时的现象或报错原文、失败原因；以及最终可行的做法（具体命令、步骤、参数）。\n\
         2. 只写会话记录中能确认的事实，不要补充未验证的做法；如果最终没有成功，在「可行做法」中如实写明尚未找到可行方案。\n\
         3. 「适用场景」写清用户会怎么描述这类需求、会遇到的现象或报错原文，便于以后检索。\n\
         4. 如果与某条已有经验是同一个问题，把新信息合并进该条（保留仍然有效的旧内容），merge_target 填它的 id；否则 merge_target 为 null。\n\
         5. body 使用 Markdown，依次包含二级标题：{sections}。title 不超过 {MAX_EXPERIENCE_TITLE_CHARS} 字。\n\n\
         只输出 JSON，不要其他文字：{{\"title\":\"...\",\"body\":\"...\",\"merge_target\":null}}"
    )
}

// ============ ECS 组件与消息 ============

/// 经验检索的发起方，挂在 ExperienceLookup WorkItem 上。
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub enum ExperienceLookupOrigin {
    /// 主 Agent 调用 `search_experience`，结果回填该次工具调用。
    ToolCall {
        agent_id: AgentId,
        request_kind: AgentRequestKind,
        tool_call_id: String,
    },
    /// 新任务开局预查，命中时投递到任务收件箱。
    Preflight,
}

/// 经验检索请求：由工具编排或开局预查生成，检索系统负责读库并派发管理员 WorkItem。
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ExperienceLookupRequest {
    pub task_id: super::TaskId,
    pub situation: String,
    pub origin: ExperienceLookupOrigin,
}

/// 客户端发起的经验起草请求（由 ingress 生成）。
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ExperienceDraftRequest {
    pub channel: ChannelId,
    pub draft_id: Uuid,
    pub transcript: String,
    pub note: Option<String>,
}

/// 起草 WorkItem 的回送目标。
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ExperienceDraftOrigin {
    pub channel: ChannelId,
    pub draft_id: Uuid,
}

/// 起草完成消息，由通知系统推送到客户端。
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct ExperienceDraftReadyMessage {
    pub channel: ChannelId,
    pub draft_id: Uuid,
    pub outcome: Result<ExperienceDraftContent, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, body: &str) -> ExperienceEntry {
        ExperienceEntry::new(title, body, None, Utc::now()).unwrap()
    }

    const BODY: &str = "## 适用场景\n鸿蒙 hdc 安装 HAP 报错 9568322\n\n## 可行做法\n重新签名后 `hdc install -r`\n\n## 走不通的路线\n- 直接重装：仍报 9568322\n\n## 前提\nDevEco 5.0";

    #[test]
    fn markdown_round_trip_preserves_entry() {
        let mut original = entry("HAP 安装签名错误", BODY);
        original.source_session = Some("s1".into());
        let parsed = ExperienceEntry::from_markdown(&original.id, &original.to_markdown()).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn rejects_non_canonical_ids() {
        assert!(!is_valid_experience_id("../etc/passwd"));
        assert!(!is_valid_experience_id(
            "6F9619FF-8B86-D011-B42D-00C04FC964FF"
        ));
        assert!(is_valid_experience_id(&Uuid::new_v4().to_string()));
        assert_eq!(
            ExperienceEntry::from_markdown("x", "+++\n+++\n"),
            Err(ExperienceError::InvalidId)
        );
    }

    #[test]
    fn validates_title_and_body() {
        assert_eq!(
            ExperienceEntry::new(" ", "b", None, Utc::now()),
            Err(ExperienceError::EmptyTitle)
        );
        assert_eq!(
            ExperienceEntry::new("t", " ", None, Utc::now()),
            Err(ExperienceError::EmptyBody)
        );
        let long = "长".repeat(MAX_EXPERIENCE_TITLE_CHARS + 1);
        assert_eq!(
            ExperienceEntry::new(&long, "b", None, Utc::now()),
            Err(ExperienceError::TitleTooLong)
        );
    }

    #[test]
    fn section_extraction_and_scenario_fallback() {
        let e = entry("t", BODY);
        assert_eq!(e.section("可行做法"), Some("重新签名后 `hdc install -r`"));
        assert_eq!(e.section("前提"), Some("DevEco 5.0"));
        assert!(e.scenario().contains("9568322"));
        let plain = entry("t", "只有一段描述");
        assert_eq!(plain.scenario(), "只有一段描述");
    }

    #[test]
    fn search_terms_split_cjk_bigrams_and_ascii_words() {
        let terms = search_terms("监控电源，报错 ECONNREFUSED: shell_exec");
        assert!(terms.contains("监控"));
        assert!(terms.contains("电源"));
        assert!(terms.contains("econnrefused"));
        assert!(terms.contains("shell_exec"));
    }

    #[test]
    fn rank_by_overlap_skips_unrelated_entries() {
        let power = entry("监控电源", "## 适用场景\n笔记本电源状态监控");
        let research = entry("查资料", "## 适用场景\n检索论文");
        let entries = vec![power, research];
        let ranked = rank_by_overlap("帮我监控一下电源", &entries, 5);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].title, "监控电源");
    }

    #[test]
    fn lookup_decision_parsing_tolerates_fences_and_dedups() {
        let id = Uuid::new_v4().to_string();
        let text = format!(
            "```json\n{{\"matches\":[{{\"id\":\"{id}\",\"reason\":\"同一报错\"}},{{\"id\":\"{id}\"}}]}}\n```"
        );
        let matches = parse_lookup_decision(&text).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].reason, "同一报错");
        assert!(parse_lookup_decision("没有 JSON").is_err());
        assert!(
            parse_lookup_decision("{\"matches\":[]}")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn render_lookup_result_is_verbatim_and_ignores_unknown_ids() {
        let e = entry("HAP 安装签名错误", BODY);
        let matches = vec![
            ExperienceMatch {
                id: e.id.clone(),
                reason: "同一报错".into(),
            },
            ExperienceMatch {
                id: Uuid::new_v4().to_string(),
                reason: String::new(),
            },
        ];
        let text = render_lookup_result(&matches, std::slice::from_ref(&e)).unwrap();
        assert!(text.contains(BODY));
        assert!(text.contains("相关原因：同一报错"));
        assert_eq!(text.matches("### 经验：").count(), 1);
        assert!(render_lookup_result(&matches[1..], &[e]).is_none());
    }

    #[test]
    fn draft_parsing_drops_invalid_merge_target() {
        let draft =
            parse_draft("{\"title\":\" 标题 \",\"body\":\"正文\",\"merge_target\":\"../x\"}")
                .unwrap();
        assert_eq!(draft.title, "标题");
        assert_eq!(draft.merge_target, None);
        assert!(parse_draft("{\"title\":\"\",\"body\":\"x\"}").is_err());
    }

    #[test]
    fn prompts_include_catalog_and_existing_entries() {
        let e = entry("HAP 安装签名错误", BODY);
        let lookup = build_lookup_prompt("安装报错 9568322", &[&e]);
        assert!(lookup.contains(&e.id));
        assert!(lookup.contains("9568322"));
        let draft = build_draft_prompt("用户: 安装失败", Some("重点记签名"), &[&e]);
        assert!(draft.contains("重点记签名"));
        assert!(draft.contains(&format!("id: {}", e.id)));
        assert!(draft.contains("## 走不通的路线"));
    }
}
