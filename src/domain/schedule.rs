//! 调度规格领域类型
//!
//! 动态任务调度规格：一次性或 cron 周期。
//! list 工具与 commit 系统共用的下一次触发计算也随类型同住。
//!
//! `ScheduleDefinition` 是动态定时任务的唯一真源：调度器状态、工具快照与持久化
//! 都直接使用它，不再维护按 kind 索引的第二份账本。

use std::str::FromStr;

use chrono::{DateTime, Duration, Local, NaiveDateTime, TimeZone, Utc};
use cron::Schedule;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use super::{ChannelId, TaskId};
use crate::prelude::Component;

/// 动态任务调度规格：一次性或 cron 周期。
///
/// `Cron` 使用 `Box<Schedule>` 以避免 `cron::Schedule`（约 248 字节）撑大
/// 整个枚举（clippy::large_enum_variant）。
#[derive(Debug, Clone)]
pub enum ScheduleSpec {
    Once(DateTime<Utc>),
    Cron(Box<Schedule>),
}

/// 计算 `ScheduleSpec` 的下一次触发时间（UTC）。
///
/// - `Once(at)` 直接返回 `Some(at)`
/// - `Cron(schedule)` 通过 `Local` 时区计算下一次触发，再转换为 UTC；
///   若 cron 无下一次触发（理论上不会发生，因为 cron 表达式永远匹配未来某个时刻），
///   则返回 `None`
pub(crate) fn compute_next_trigger(schedule: &ScheduleSpec) -> Option<DateTime<Utc>> {
    match schedule {
        ScheduleSpec::Once(at) => Some(*at),
        ScheduleSpec::Cron(schedule) => schedule
            .upcoming(Local)
            .next()
            .map(|t| t.with_timezone(&Utc)),
    }
}

/// 调度表达式解析错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleParseError {
    #[error("schedule must start with 'once:' or 'cron:'")]
    MissingPrefix,
    #[error("invalid once time: {0}")]
    InvalidOnceTime(String),
    #[error("ambiguous or invalid local time")]
    AmbiguousLocalTime,
    #[error("invalid cron: {0}")]
    InvalidCron(String),
}

/// 带原始文本的调度表达式。
///
/// `cron::Schedule` 不可序列化，因此保留 `raw` 作为持久化与展示形态，
/// 反序列化时重新解析。一次性时间统一规范化为 `once:<UTC RFC 3339>`，
/// 避免重启后按本地时区重解析产生歧义。
#[derive(Debug, Clone)]
pub struct ScheduleExpr {
    raw: String,
    spec: ScheduleSpec,
}

impl ScheduleExpr {
    /// 解析 `once:` / `cron:` 前缀的调度表达式。
    ///
    /// - `once:<RFC3339 或 naive local>`：一次性触发（是否在未来由调用方判断）。
    /// - `cron:<5 字段>`：周期性触发，内部补齐为 7 字段（`"0 {user_cron} *"`）。
    pub fn parse(raw: &str) -> Result<Self, ScheduleParseError> {
        if let Some(rest) = raw.strip_prefix("once:") {
            let at = parse_once_time(rest)?.with_timezone(&Utc);
            Ok(Self {
                raw: format!("once:{}", at.to_rfc3339()),
                spec: ScheduleSpec::Once(at),
            })
        } else if let Some(rest) = raw.strip_prefix("cron:") {
            let schedule = Schedule::from_str(&format!("0 {} *", rest))
                .map_err(|e| ScheduleParseError::InvalidCron(e.to_string()))?;
            Ok(Self {
                raw: raw.to_string(),
                spec: ScheduleSpec::Cron(Box::new(schedule)),
            })
        } else {
            Err(ScheduleParseError::MissingPrefix)
        }
    }

    /// 测试构造器：直接给定调度规格，用于表达 `cron:` 语法无法描述的秒级调度。
    ///
    /// 产物不保证 `raw` 能被 [`ScheduleExpr::parse`] 还原，不应持久化。
    pub fn from_spec_for_test(raw: impl Into<String>, spec: ScheduleSpec) -> Self {
        Self {
            raw: raw.into(),
            spec,
        }
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn spec(&self) -> &ScheduleSpec {
        &self.spec
    }

    pub fn is_once(&self) -> bool {
        matches!(self.spec, ScheduleSpec::Once(_))
    }
}

/// 先尝试带时区偏移的 RFC 3339，再回退到无偏移的本地时间。
fn parse_once_time(s: &str) -> Result<DateTime<Local>, ScheduleParseError> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .map_err(|e| ScheduleParseError::InvalidOnceTime(e.to_string()))?;
    Local
        .from_local_datetime(&naive)
        .single()
        .ok_or(ScheduleParseError::AmbiguousLocalTime)
}

impl Serialize for ScheduleExpr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for ScheduleExpr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// 定时运行的最终回复投递策略；失败告警始终独立于此策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyPolicy {
    #[default]
    Reply,
    Explicit,
}

/// 定时任务运行健康信息，随定义持久化。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScheduleHealth {
    /// 上次触发时间（含补跑与手动运行），补跑判定的起点。
    #[serde(default)]
    pub last_fired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_success_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub alerting: bool,
}

/// 动态定时任务定义：由 `schedule_task` 创建，持久化到 `ScheduleStore`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleDefinition {
    pub id: Uuid,
    /// 用户可读名称，全局唯一，用于管理命令定位。
    pub name: String,
    /// 每次运行的任务提示词。
    pub content: String,
    pub schedule: ScheduleExpr,
    pub output_channel: Option<ChannelId>,
    #[serde(default)]
    pub notify: NotifyPolicy,
    #[serde(default)]
    pub report_of: Option<Uuid>,
    /// 暂停时为 `false`：timer 不再调度，触发也会被跳过。
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub health: ScheduleHealth,
}

impl ScheduleDefinition {
    /// 新建一条启用状态的定义；`name` 缺省时取 id 前 8 位。
    pub fn new(
        id: Uuid,
        name: Option<String>,
        content: String,
        schedule: ScheduleExpr,
        output_channel: Option<ChannelId>,
        created_at: DateTime<Utc>,
    ) -> Self {
        let name = name.unwrap_or_else(|| id.simple().to_string()[..8].to_string());
        Self {
            id,
            name,
            content,
            schedule,
            output_channel,
            notify: NotifyPolicy::Reply,
            report_of: None,
            enabled: true,
            created_at,
            health: ScheduleHealth::default(),
        }
    }

    /// 调度器与信号使用的 kind 字符串：`scheduled:<id>`。
    pub fn kind(&self) -> String {
        schedule_kind(self.id)
    }
}

/// 管理类工具定位定时任务的方式：按 kind（`scheduled:<id>`）或按名称。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleSelector {
    Kind(String),
    Name(String),
}

impl ScheduleSelector {
    /// 从工具入参读取 `kind` 或 `name`（`kind` 优先），二者都缺失或为空时报错。
    pub fn from_input(input: &serde_json::Value) -> Result<Self, String> {
        let field = |key: &str| {
            input
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        field("kind")
            .map(Self::Kind)
            .or_else(|| field("name").map(Self::Name))
            .ok_or_else(|| "missing required parameter: kind or name".to_string())
    }

    pub fn matches(&self, def: &ScheduleDefinition) -> bool {
        match self {
            Self::Kind(kind) => def.kind() == *kind,
            Self::Name(name) => def.name == *name,
        }
    }

    /// 用于工具结果回显的定位值。
    pub fn label(&self) -> &str {
        match self {
            Self::Kind(s) | Self::Name(s) => s,
        }
    }
}

/// 补跑窗口：错过的运行距今不超过该时长才补跑。
pub const CATCH_UP_WINDOW: Duration = Duration::days(7);

/// 错过运行的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUp {
    /// 没有错过的运行。
    None,
    /// 在窗口内错过了运行：只补跑最近一次（`missed_at`）。
    Fire { missed_at: DateTime<Utc> },
    /// 错过的运行都已超出窗口：不补跑。
    Expired { missed_at: DateTime<Utc> },
}

/// 判定定义在 `now` 时刻是否有错过的运行。
///
/// - cron：考察 `last_fired_at`（缺省 `created_at`）之后、`now` 及之前的应触发时刻，
///   窗口内取最近一次为 `Fire`；仅窗口外存在时为 `Expired`。
/// - once：触发时间已到时，距今不超过窗口为 `Fire`，否则为 `Expired`。
pub fn catch_up_decision(
    def: &ScheduleDefinition,
    now: DateTime<Utc>,
    window: Duration,
) -> CatchUp {
    match def.schedule.spec() {
        ScheduleSpec::Once(at) if *at > now => CatchUp::None,
        ScheduleSpec::Once(at) if now - *at <= window => CatchUp::Fire { missed_at: *at },
        ScheduleSpec::Once(at) => CatchUp::Expired { missed_at: *at },
        ScheduleSpec::Cron(schedule) => {
            let since = def.health.last_fired_at.unwrap_or(def.created_at);
            let window_start = since.max(now - window);
            let latest_in_window = schedule
                .after(&window_start.with_timezone(&Local))
                .map(|t| t.with_timezone(&Utc))
                .take_while(|t| *t <= now)
                .last();
            if let Some(missed_at) = latest_in_window {
                return CatchUp::Fire { missed_at };
            }
            match schedule
                .after(&since.with_timezone(&Local))
                .map(|t| t.with_timezone(&Utc))
                .next()
            {
                Some(first) if first <= window_start => CatchUp::Expired { missed_at: first },
                _ => CatchUp::None,
            }
        }
    }
}

/// 触发来源标记：附在 `TriggerTaskMessage` 实体上，缺省表示按调度正常触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub enum ScheduleFire {
    /// 补跑错过的运行。
    CatchUp { missed_at: DateTime<Utc> },
    /// 用户经 `update_scheduled_task` 的 `run_now` 手动触发。
    Manual,
}

/// 标记 Task 是某个动态定时任务的一次运行，用于重叠判定与运行记账。
///
/// 由触发路由附在 `CreateTaskMessage` 实体上，`user_message_to_task_system` 拷到新 Task 上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Component)]
pub struct ScheduledRun {
    pub schedule_id: Uuid,
    /// 路由接受触发的时间；手工构造的运行回退到 Task 创建时间。
    pub fired_at: Option<DateTime<Utc>>,
    pub notify: NotifyPolicy,
}

impl ScheduledRun {
    pub fn new(schedule_id: Uuid) -> Self {
        Self {
            schedule_id,
            fired_at: None,
            notify: NotifyPolicy::Reply,
        }
    }
}

/// 本轮无人值守执行被权限边界阻断，不能由模型最终文字覆盖为成功。
#[derive(Debug, Clone, Component)]
pub struct ScheduledRunFailure(pub String);

/// 一次触发的最终记账结果，跳过和错过不增加失败次数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Notified,
    Silent,
    Failed,
    SkippedOverlap,
    Missed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRunRecord {
    pub task_id: Option<TaskId>,
    pub schedule_id: Uuid,
    pub fired_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: RunOutcome,
    pub error: Option<String>,
    pub result_summary: String,
    pub notification_count: u32,
}

/// 可信运行统计；total 含无变化的 Silent 运行。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub total: u64,
    pub notified: u64,
    pub failed: u64,
    pub skipped: u64,
    pub missed: u64,
}

/// 由定义 id 派生 kind 字符串。
pub fn schedule_kind(id: Uuid) -> String {
    format!("scheduled:{id}")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use chrono::Timelike;

    use super::*;

    /// `compute_next_trigger` 对 `Once(at)` 直接返回 `Some(at)`。
    #[test]
    fn compute_next_trigger_for_once_returns_some_at() {
        let at = Utc::now() + chrono::Duration::days(7);
        let schedule = ScheduleSpec::Once(at);
        let next = compute_next_trigger(&schedule);
        assert_eq!(next, Some(at));
    }

    /// `compute_next_trigger` 对 `Cron(schedule)` 返回下一次本地时区触发时间（转 UTC）。
    /// 工作日 9:00 cron 至少存在一个未来触发点。
    #[test]
    fn compute_next_trigger_for_cron_returns_next_upcoming() {
        let cron_schedule = cron::Schedule::from_str("0 0 9 * * * *").unwrap();
        let schedule = ScheduleSpec::Cron(Box::new(cron_schedule));
        let next = compute_next_trigger(&schedule).expect("cron must have a next trigger");
        // 转回 Local 验证小时为 9
        let local_next = next.with_timezone(&Local);
        assert_eq!(local_next.hour(), 9, "next trigger should be at local 9:00");
    }

    #[test]
    fn schedule_expr_cron_roundtrips_through_json() {
        let expr = ScheduleExpr::parse("cron:*/10 * * * *").unwrap();
        assert!(!expr.is_once());
        let json = serde_json::to_string(&expr).unwrap();
        assert_eq!(json, "\"cron:*/10 * * * *\"");
        let back: ScheduleExpr = serde_json::from_str(&json).unwrap();
        assert_eq!(back.raw(), "cron:*/10 * * * *");
        assert!(matches!(back.spec(), ScheduleSpec::Cron(_)));
    }

    #[test]
    fn schedule_expr_once_normalizes_to_utc_and_roundtrips() {
        let expr = ScheduleExpr::parse("once:2030-01-02T08:00:00+08:00").unwrap();
        assert!(expr.is_once());
        assert_eq!(expr.raw(), "once:2030-01-02T00:00:00+00:00");
        let back: ScheduleExpr =
            serde_json::from_str(&serde_json::to_string(&expr).unwrap()).unwrap();
        match back.spec() {
            ScheduleSpec::Once(at) => assert_eq!(at.to_rfc3339(), "2030-01-02T00:00:00+00:00"),
            other => panic!("expected once, got {other:?}"),
        }
    }

    #[test]
    fn schedule_expr_rejects_bad_input() {
        assert_eq!(
            ScheduleExpr::parse("daily").unwrap_err(),
            ScheduleParseError::MissingPrefix
        );
        assert!(matches!(
            ScheduleExpr::parse("cron:not a cron").unwrap_err(),
            ScheduleParseError::InvalidCron(_)
        ));
        assert!(matches!(
            ScheduleExpr::parse("once:tomorrow").unwrap_err(),
            ScheduleParseError::InvalidOnceTime(_)
        ));
        assert!(serde_json::from_str::<ScheduleExpr>("\"cron:bad\"").is_err());
    }

    #[test]
    fn schedule_definition_defaults_name_and_derives_kind() {
        let id = Uuid::new_v4();
        let def = ScheduleDefinition::new(
            id,
            None,
            "check forum".to_string(),
            ScheduleExpr::parse("cron:0 8 * * *").unwrap(),
            None,
            Utc::now(),
        );
        assert_eq!(def.name, id.simple().to_string()[..8]);
        assert_eq!(def.kind(), format!("scheduled:{id}"));
        assert!(def.enabled);
        assert_eq!(def.health, ScheduleHealth::default());
    }

    #[test]
    fn schedule_definition_roundtrips_and_tolerates_missing_health() {
        let def = ScheduleDefinition::new(
            Uuid::new_v4(),
            Some("forum-daily".to_string()),
            "report".to_string(),
            ScheduleExpr::parse("cron:0 8 * * *").unwrap(),
            None,
            Utc::now(),
        );
        let mut value = serde_json::to_value(&def).unwrap();
        value.as_object_mut().unwrap().remove("health");
        let back: ScheduleDefinition = serde_json::from_value(value).unwrap();
        assert_eq!(back.name, "forum-daily");
        assert_eq!(back.schedule.raw(), "cron:0 8 * * *");
        assert_eq!(back.health, ScheduleHealth::default());
    }

    #[test]
    fn schedule_selector_reads_kind_or_name_and_matches() {
        let def = ScheduleDefinition::new(
            Uuid::new_v4(),
            Some("forum-watch".to_string()),
            "x".to_string(),
            ScheduleExpr::parse("cron:0 8 * * *").unwrap(),
            None,
            Utc::now(),
        );
        let by_kind =
            ScheduleSelector::from_input(&serde_json::json!({ "kind": def.kind() })).unwrap();
        assert!(by_kind.matches(&def));
        let by_name =
            ScheduleSelector::from_input(&serde_json::json!({ "name": " forum-watch " })).unwrap();
        assert_eq!(by_name, ScheduleSelector::Name("forum-watch".to_string()));
        assert!(by_name.matches(&def));
        assert!(!ScheduleSelector::Name("other".to_string()).matches(&def));
        assert!(ScheduleSelector::from_input(&serde_json::json!({ "kind": "" })).is_err());
    }

    fn cron_def(
        cron: &str,
        created_at: DateTime<Utc>,
        last_fired_at: Option<DateTime<Utc>>,
    ) -> ScheduleDefinition {
        let mut def = ScheduleDefinition::new(
            Uuid::new_v4(),
            None,
            "x".to_string(),
            ScheduleExpr::parse(cron).unwrap(),
            None,
            created_at,
        );
        def.health.last_fired_at = last_fired_at;
        def
    }

    fn once_def(at: DateTime<Utc>) -> ScheduleDefinition {
        ScheduleDefinition::new(
            Uuid::new_v4(),
            None,
            "x".to_string(),
            ScheduleExpr::parse(&format!("once:{}", at.to_rfc3339())).unwrap(),
            None,
            at - Duration::days(30),
        )
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn catch_up_cron_without_missed_run_is_none() {
        // 每小时整点；上次 10:00 触发，现在 10:30 —— 尚未错过
        let def = cron_def(
            "cron:0 * * * *",
            utc("2030-01-01T00:00:00Z"),
            Some(utc("2030-01-02T10:00:00Z")),
        );
        assert_eq!(
            catch_up_decision(&def, utc("2030-01-02T10:30:00Z"), CATCH_UP_WINDOW),
            CatchUp::None
        );
    }

    #[test]
    fn catch_up_cron_fires_only_latest_missed_run() {
        // 上次 10:00，进程停到 13:30：错过 11/12/13 点，只补跑最近一次 13:00
        let def = cron_def(
            "cron:0 * * * *",
            utc("2030-01-01T00:00:00Z"),
            Some(utc("2030-01-02T10:00:00Z")),
        );
        assert_eq!(
            catch_up_decision(&def, utc("2030-01-02T13:30:00Z"), CATCH_UP_WINDOW),
            CatchUp::Fire {
                missed_at: utc("2030-01-02T13:00:00Z")
            }
        );
    }

    #[test]
    fn catch_up_cron_uses_created_at_when_never_fired() {
        let def = cron_def("cron:0 * * * *", utc("2030-01-02T10:15:00Z"), None);
        assert_eq!(
            catch_up_decision(&def, utc("2030-01-02T11:05:00Z"), CATCH_UP_WINDOW),
            CatchUp::Fire {
                missed_at: utc("2030-01-02T11:00:00Z")
            }
        );
    }

    #[test]
    fn catch_up_cron_long_outage_still_fires_latest_within_window() {
        // 每小时任务停机 30 天：最近一次错过的运行仍在窗口内
        let def = cron_def(
            "cron:0 * * * *",
            utc("2030-01-01T00:00:00Z"),
            Some(utc("2030-01-01T00:00:00Z")),
        );
        assert_eq!(
            catch_up_decision(&def, utc("2030-01-31T05:20:00Z"), CATCH_UP_WINDOW),
            CatchUp::Fire {
                missed_at: utc("2030-01-31T05:00:00Z")
            }
        );
    }

    #[test]
    fn catch_up_cron_missed_only_outside_window_is_expired() {
        // 每月 1 日 00:00 UTC 附近（本地时区可能偏移，仍是月度量级）：上次 1 月触发，
        // 2 月那次在 2 月 20 日看来已超出 7 天窗口
        let def = cron_def(
            "cron:0 0 1 * *",
            utc("2029-12-01T00:00:00Z"),
            Some(utc("2030-01-10T00:00:00Z")),
        );
        assert!(matches!(
            catch_up_decision(&def, utc("2030-02-20T00:00:00Z"), CATCH_UP_WINDOW),
            CatchUp::Expired { .. }
        ));
    }

    #[test]
    fn catch_up_once_future_past_and_expired() {
        let now = utc("2030-01-10T12:00:00Z");
        assert_eq!(
            catch_up_decision(&once_def(utc("2030-01-11T00:00:00Z")), now, CATCH_UP_WINDOW),
            CatchUp::None
        );
        assert_eq!(
            catch_up_decision(&once_def(utc("2030-01-09T00:00:00Z")), now, CATCH_UP_WINDOW),
            CatchUp::Fire {
                missed_at: utc("2030-01-09T00:00:00Z")
            }
        );
        assert_eq!(
            catch_up_decision(&once_def(utc("2030-01-01T00:00:00Z")), now, CATCH_UP_WINDOW),
            CatchUp::Expired {
                missed_at: utc("2030-01-01T00:00:00Z")
            }
        );
    }
}
