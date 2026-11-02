//! 用量账本契约：执行层记账、客户端通道读取汇总，文件实现见 infrastructure。

use std::sync::Arc;

use chrono::{DateTime, Local, Utc};

use crate::domain::{TokenUsage, UsageContext, UsageRecord};

pub trait UsageLedger: Send + Sync + 'static {
    /// 记一次调用并按单价折算费用；写盘失败只告警，不影响任务执行。
    fn record(&self, ctx: &UsageContext, usage: &TokenUsage, at: DateTime<Utc>) -> UsageRecord;

    /// 读取 `[from, to)` 内的记录（本地时间），跳过损坏行。
    fn read_range(&self, from: DateTime<Local>, to: DateTime<Local>) -> Vec<UsageRecord>;
}

pub type SharedUsageLedger = Arc<dyn UsageLedger>;
