//! 用量记录与汇总：单条模型调用的用量记录，以及按时间范围聚合总计、每日、模型、
//! Agent、调用类型与根任务。持久化由 `contracts::UsageLedger` 的实现负责。

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, Local, TimeZone};
use serde::{Deserialize, Serialize};

use super::TaskId;

/// 单条用量记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub at: DateTime<chrono::Utc>,
    pub task_id: TaskId,
    /// 任务树根任务（用户发起的顶层任务），用于归属到客户端会话
    pub root_task_id: TaskId,
    pub agent: String,
    /// 调用类型：task / summarization / experience / watchdog
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    /// 折算费用（元）；未配置单价时为 None
    pub cost: Option<f64>,
}

/// 记账上下文：执行层在提交请求时已知的归属信息。
#[derive(Debug, Clone)]
pub struct UsageContext {
    pub task_id: TaskId,
    pub root_task_id: TaskId,
    pub agent: String,
    pub kind: String,
}

/// 查询范围（按本地时区）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRange {
    Today,
    Days7,
    Days30,
    Month,
}

impl UsageRange {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "today" => Some(Self::Today),
            "7d" => Some(Self::Days7),
            "30d" => Some(Self::Days30),
            "month" => Some(Self::Month),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::Month => "month",
        }
    }

    /// `[from, to)`：从起始日 0 点到明天 0 点。
    pub fn bounds(&self, now: DateTime<Local>) -> (DateTime<Local>, DateTime<Local>) {
        let midnight = |d: DateTime<Local>| {
            Local
                .with_ymd_and_hms(d.year(), d.month(), d.day(), 0, 0, 0)
                .earliest()
                .unwrap_or(d)
        };
        let today = midnight(now);
        let from = match self {
            Self::Today => today,
            Self::Days7 => midnight(today - Duration::days(6)),
            Self::Days30 => midnight(today - Duration::days(29)),
            Self::Month => midnight(today.with_day(1).unwrap_or(today)),
        };
        (from, midnight(today + Duration::hours(36)))
    }
}

/// 一组调用的累计；`cost` 只累计已配置单价的调用，`unpriced_calls` 标出缺口。
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct UsageBucket {
    pub key: String,
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cost: f64,
    pub unpriced_calls: u64,
}

impl UsageBucket {
    fn add(&mut self, r: &UsageRecord) {
        self.calls += 1;
        self.input_tokens += r.input_tokens;
        self.output_tokens += r.output_tokens;
        self.cached_input_tokens += r.cached_input_tokens;
        match r.cost {
            Some(cost) => self.cost += cost,
            None => self.unpriced_calls += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageSummary {
    pub range: &'static str,
    pub from: String,
    pub to: String,
    pub currency: &'static str,
    pub total: UsageBucket,
    /// 每天一项（含无调用的日期），按日期升序
    pub by_day: Vec<UsageBucket>,
    pub by_model: Vec<UsageBucket>,
    pub by_agent: Vec<UsageBucket>,
    pub by_kind: Vec<UsageBucket>,
    /// key 为根任务 ID，由客户端通道映射到会话
    pub by_root_task: Vec<UsageBucket>,
}

fn grouped(records: &[UsageRecord], key: impl Fn(&UsageRecord) -> String) -> Vec<UsageBucket> {
    let mut map: BTreeMap<String, UsageBucket> = BTreeMap::new();
    for r in records {
        let k = key(r);
        map.entry(k.clone())
            .or_insert_with(|| UsageBucket {
                key: k,
                ..Default::default()
            })
            .add(r);
    }
    let mut buckets: Vec<UsageBucket> = map.into_values().collect();
    // 费用优先、token 次之降序，便于直接展示"消耗最多"
    buckets.sort_by(|a, b| {
        b.cost
            .total_cmp(&a.cost)
            .then((b.input_tokens + b.output_tokens).cmp(&(a.input_tokens + a.output_tokens)))
    });
    buckets
}

pub fn summarize(records: &[UsageRecord], range: UsageRange, now: DateTime<Local>) -> UsageSummary {
    let (from, to) = range.bounds(now);
    let mut total = UsageBucket {
        key: "total".into(),
        ..Default::default()
    };
    let mut days: BTreeMap<String, UsageBucket> = BTreeMap::new();
    let mut day = from;
    while day < to {
        let key = day.format("%Y-%m-%d").to_string();
        days.insert(
            key.clone(),
            UsageBucket {
                key,
                ..Default::default()
            },
        );
        day += Duration::days(1);
    }
    for r in records {
        total.add(r);
        let key = r.at.with_timezone(&Local).format("%Y-%m-%d").to_string();
        if let Some(bucket) = days.get_mut(&key) {
            bucket.add(r);
        }
    }
    UsageSummary {
        range: range.as_str(),
        from: from.format("%Y-%m-%d").to_string(),
        to: (to - Duration::days(1)).format("%Y-%m-%d").to_string(),
        currency: "CNY",
        total,
        by_day: days.into_values().collect(),
        by_model: grouped(records, |r| format!("{}/{}", r.provider, r.model)),
        by_agent: grouped(records, |r| r.agent.clone()),
        by_kind: grouped(records, |r| r.kind.clone()),
        by_root_task: grouped(records, |r| r.root_task_id.to_string()),
    }
}
