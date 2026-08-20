//! 调度规格领域类型
//!
//! 动态任务调度规格：一次性或 cron 周期。
//! list 工具与 commit 系统共用的下一次触发计算也随类型同住。

use chrono::{DateTime, Local, Utc};
use cron::Schedule;

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
}
