//! Watchdog：外部 Control Loop 的领域契约。
//!
//! 定时驱动的 watchdog Agent 监督后台工具调用与运行中的子任务，判断是否取得实质进展、
//! 是否偏离所属任务的目标，并给出继续 / 调整 / 取消 / 重试决策。主 Agent 不必把上下文
//! 浪费在监督过程上：只有需要它知道的决策才会通知它。
//!
//! 本模块只含纯数据与纯函数（取证摘要、提示词、决策解析），ECS 侧见 `systems::watchdog`。

use serde::Deserialize;
use uuid::Uuid;

use crate::prelude::Component;

use super::TaskId;

/// watchdog Agent 的能力标签。
pub const WATCHDOG_TAG: &str = "watchdog";

/// 单个被监督对象在一次巡检中的标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WatchdogSubjectKey {
    /// 转后台的工具调用（`BackgroundJob.id`）。
    ToolJob(Uuid),
    /// 子任务批次中的一个子任务。
    SubTask(TaskId),
}

/// 被监督对象种类：决定允许的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogSubjectKind {
    ToolCall { tool_name: String },
    SubTask { name: String },
}

impl WatchdogSubjectKind {
    fn allowed_actions(&self) -> &'static [WatchdogAction] {
        match self {
            Self::ToolCall { .. } => &[
                WatchdogAction::Continue,
                WatchdogAction::Cancel,
                WatchdogAction::Retry,
                WatchdogAction::NotifyOwner,
            ],
            Self::SubTask { .. } => &[
                WatchdogAction::Continue,
                WatchdogAction::Adjust,
                WatchdogAction::Cancel,
            ],
        }
    }

    /// 面向模型与用户的对象描述。
    pub fn label(&self) -> String {
        match self {
            Self::ToolCall { tool_name } => format!("后台工具调用 {tool_name}"),
            Self::SubTask { name } => format!("子任务「{name}」"),
        }
    }
}

/// 一次巡检中提交给 watchdog 的对象快照。
#[derive(Debug, Clone)]
pub struct WatchdogSubject {
    pub key: WatchdogSubjectKey,
    pub kind: WatchdogSubjectKind,
    /// 作业 / 子任务的所属任务（决策通知对象）。
    pub owner_task_id: TaskId,
    /// 所属任务的目标（判断方向是否偏离）。
    pub goal: String,
    /// 已运行时长（秒）。
    pub elapsed_secs: i64,
    /// 进展证据（输出尾部、最近对话、todo 等）。
    pub evidence: String,
    /// 证据指纹：与上次巡检相同表示没有任何可观测变化。
    pub fingerprint: u64,
    /// 自上次巡检以来证据是否未变。
    pub unchanged_since_last_review: bool,
    /// 先前的 watchdog 决策（最近几条）。
    pub history: Vec<String>,
}

/// watchdog 决策动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogAction {
    /// 有进展或仍在合理范围，继续观察。
    Continue,
    /// 子任务：把调整建议注入子任务，修正执行策略。
    Adjust,
    /// 停止：取消后台工具调用 / 子任务，结果通知所属任务。
    Cancel,
    /// 后台工具调用：取消后以相同参数重跑（适合偶发卡死）。
    Retry,
    /// 后台工具调用：通知所属任务的 Agent 自行决定（附建议）。
    NotifyOwner,
}

impl WatchdogAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Adjust => "adjust",
            Self::Cancel => "cancel",
            Self::Retry => "retry",
            Self::NotifyOwner => "notify_owner",
        }
    }
}

/// 单个对象的判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchdogDecision {
    pub subject_id: String,
    /// progressing / stalled / off_track
    pub progress: String,
    pub action: WatchdogAction,
    pub guidance: Option<String>,
    pub reason: String,
}

#[derive(Deserialize)]
struct RawDecision {
    id: String,
    #[serde(default)]
    progress: String,
    action: WatchdogAction,
    #[serde(default)]
    guidance: Option<String>,
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
struct RawDecisions {
    decisions: Vec<RawDecision>,
}

/// 巡检 WorkItem 上记录的对象映射（提示词中的 `S1`… → 对象），供回复落地。
#[derive(Debug, Clone, Component)]
pub struct WatchdogReviewOrigin {
    pub subjects: Vec<(String, WatchdogSubject)>,
}

/// 构造巡检提示词。对象按 `S1..Sn` 编号。
pub fn build_watchdog_prompt(subjects: &[(String, WatchdogSubject)]) -> String {
    let mut prompt = String::from(
        "你是 Harness 的 watchdog，负责监督后台运行的工具调用和子任务，判断它们是否取得实质进展、是否偏离所属任务的目标。\n\
主 Agent 不会自己盯着这些后台工作，你的判断直接决定是否干预。原则：\n\
- 有可观测的推进（输出在增长、步骤在推进、todo 在完成）就 continue，不要因为耗时长就干预；编译、测试、下载本来就慢。\n\
- 证据长时间不变、反复执行同一失败操作、陷入循环或明显偏离目标时才干预。\n\
- 子任务可选 continue / adjust / cancel：adjust 时在 guidance 中给出具体可执行的调整建议；确认无望或继续只会浪费资源时 cancel。\n\
- 后台工具调用可选 continue / cancel / retry / notify_owner：命令疑似偶发卡死（网络、锁等待）且重跑无副作用时 retry；有副作用或需要所属任务换方案时 notify_owner 并在 guidance 中说明；确认无望时 cancel。\n\
- 参考历史决策：已经 adjust 或 retry 过仍无进展时，应升级为 cancel 或 notify_owner，不要重复同一动作。\n\n",
    );
    for (id, subject) in subjects {
        let actions: Vec<&str> = subject
            .kind
            .allowed_actions()
            .iter()
            .map(WatchdogAction::as_str)
            .collect();
        prompt.push_str(&format!(
            "## {id}：{}\n- 所属任务目标：{}\n- 已运行：{}s\n- 自上次巡检证据是否变化：{}\n- 允许的动作：{}\n",
            subject.kind.label(),
            subject.goal,
            subject.elapsed_secs,
            if subject.unchanged_since_last_review {
                "没有变化"
            } else {
                "有变化或首次巡检"
            },
            actions.join(" / "),
        ));
        if !subject.history.is_empty() {
            prompt.push_str("- 历史决策：\n");
            for entry in &subject.history {
                prompt.push_str(&format!("  - {entry}\n"));
            }
        }
        prompt.push_str("- 进展证据：\n```\n");
        prompt.push_str(&subject.evidence);
        prompt.push_str("\n```\n\n");
    }
    prompt.push_str(
        "只输出 JSON，不要输出其他文字：\n\
{\"decisions\":[{\"id\":\"S1\",\"progress\":\"progressing|stalled|off_track\",\"action\":\"continue\",\"guidance\":\"adjust/notify_owner 时必填\",\"reason\":\"一句话依据\"}]}\n\
每个对象都要给出决策。",
    );
    prompt
}

/// 解析 watchdog 回复；容忍代码块包裹与前后多余文字。
///
/// 不在本次巡检中的 id、对象种类不允许的动作会被丢弃（对应对象视为 continue）。
pub fn parse_watchdog_decisions(
    text: &str,
    subjects: &[(String, WatchdogSubject)],
) -> Result<Vec<WatchdogDecision>, String> {
    let start = text
        .find('{')
        .ok_or_else(|| "watchdog reply contains no JSON object".to_string())?;
    let end = text
        .rfind('}')
        .ok_or_else(|| "watchdog reply contains no JSON object".to_string())?;
    let raw: RawDecisions = serde_json::from_str(&text[start..=end])
        .map_err(|error| format!("invalid watchdog reply: {error}"))?;
    Ok(raw
        .decisions
        .into_iter()
        .filter_map(|decision| {
            let (_, subject) = subjects.iter().find(|(id, _)| *id == decision.id)?;
            if !subject.kind.allowed_actions().contains(&decision.action) {
                return None;
            }
            let guidance = decision.guidance.filter(|g| !g.trim().is_empty());
            // 需要建议的动作缺少建议时无法落地，降级为继续观察。
            if matches!(
                decision.action,
                WatchdogAction::Adjust | WatchdogAction::NotifyOwner
            ) && guidance.is_none()
            {
                return None;
            }
            Some(WatchdogDecision {
                subject_id: decision.id,
                progress: decision.progress,
                action: decision.action,
                guidance,
                reason: decision.reason,
            })
        })
        .collect())
}

/// 截断到 UTF-8 边界上的前 `max_chars` 个字符。
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…")
}

/// 证据指纹（进程内比较用，不持久化）。
pub fn evidence_fingerprint(evidence: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    evidence.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(kind: WatchdogSubjectKind) -> WatchdogSubject {
        WatchdogSubject {
            key: WatchdogSubjectKey::ToolJob(Uuid::new_v4()),
            kind,
            owner_task_id: TaskId::new(),
            goal: "build".into(),
            elapsed_secs: 300,
            evidence: "compiling".into(),
            fingerprint: 1,
            unchanged_since_last_review: true,
            history: vec!["retry: 卡在下载".into()],
        }
    }

    fn subjects() -> Vec<(String, WatchdogSubject)> {
        vec![
            (
                "S1".into(),
                subject(WatchdogSubjectKind::ToolCall {
                    tool_name: "shell_exec".into(),
                }),
            ),
            (
                "S2".into(),
                subject(WatchdogSubjectKind::SubTask {
                    name: "research".into(),
                }),
            ),
        ]
    }

    #[test]
    fn prompt_lists_subjects_with_allowed_actions_and_history() {
        let prompt = build_watchdog_prompt(&subjects());
        assert!(prompt.contains("## S1：后台工具调用 shell_exec"));
        assert!(prompt.contains("continue / cancel / retry / notify_owner"));
        assert!(prompt.contains("## S2：子任务「research」"));
        assert!(prompt.contains("continue / adjust / cancel"));
        assert!(prompt.contains("retry: 卡在下载"));
        assert!(prompt.contains("没有变化"));
    }

    #[test]
    fn parses_fenced_reply_and_drops_invalid_decisions() {
        let reply = r#"```json
{"decisions":[
  {"id":"S1","progress":"stalled","action":"retry","reason":"下载卡住"},
  {"id":"S2","progress":"stalled","action":"retry","reason":"子任务不支持重试"},
  {"id":"S9","progress":"stalled","action":"cancel","reason":"不存在"}
]}
```"#;
        let decisions = parse_watchdog_decisions(reply, &subjects()).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].subject_id, "S1");
        assert_eq!(decisions[0].action, WatchdogAction::Retry);
    }

    #[test]
    fn adjust_without_guidance_is_dropped() {
        let reply = r#"{"decisions":[
            {"id":"S2","progress":"stalled","action":"adjust","guidance":" ","reason":"x"},
            {"id":"S1","progress":"stalled","action":"notify_owner","guidance":"换镜像源","reason":"y"}
        ]}"#;
        let decisions = parse_watchdog_decisions(reply, &subjects()).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].guidance.as_deref(), Some("换镜像源"));
    }

    #[test]
    fn rejects_non_json_reply() {
        assert!(parse_watchdog_decisions("一切正常", &subjects()).is_err());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_chars("你好世界", 2), "你好…");
        assert_eq!(truncate_chars("abc", 5), "abc");
    }
}
