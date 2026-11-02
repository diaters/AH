//! 用量账本：每次模型调用的 token 用量与折算费用，按月追加到 JSONL。
//!
//! 文件：`<dir>/usage-YYYY-MM.jsonl`（按本地时区分月）。费用在记账时按当时单价算好写入，
//! 之后修改单价不改写历史；未配置单价的模型 `cost` 为 None。

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Local, Months, Utc};
use tracing::warn;

use crate::contracts::UsageLedger;
use crate::domain::{ModelPrice, ProvidersConfig, TokenUsage, UsageContext, UsageRecord};

#[derive(Debug)]
pub struct FileUsageLedger {
    dir: PathBuf,
    /// (provider, model) → 单价
    prices: HashMap<(String, String), ModelPrice>,
    write_lock: Mutex<()>,
}

impl FileUsageLedger {
    pub fn new(dir: PathBuf, prices: HashMap<(String, String), ModelPrice>) -> Self {
        Self {
            dir,
            prices,
            write_lock: Mutex::new(()),
        }
    }

    /// 默认目录 `.harness/usage`，单价取自 providers.toml（未加载时为空）。
    pub fn from_providers(config: Option<&ProvidersConfig>) -> Self {
        let prices = config
            .map(|c| {
                c.provider
                    .iter()
                    .flat_map(|p| {
                        p.pricing
                            .iter()
                            .map(|(model, price)| ((p.name.clone(), model.clone()), *price))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self::new(PathBuf::from(".harness/usage"), prices)
    }

    pub fn price(&self, provider: &str, model: &str) -> Option<ModelPrice> {
        self.prices
            .get(&(provider.to_string(), model.to_string()))
            .copied()
    }

    fn file_for(&self, at: DateTime<Local>) -> PathBuf {
        self.dir
            .join(format!("usage-{:04}-{:02}.jsonl", at.year(), at.month()))
    }

    fn append(&self, record: &UsageRecord) -> std::io::Result<()> {
        let _guard = self.write_lock.lock().expect("usage ledger lock poisoned");
        std::fs::create_dir_all(&self.dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file_for(record.at.with_timezone(&Local)))?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        file.write_all(&line)
    }
}

impl UsageLedger for FileUsageLedger {
    /// 记一次调用；写盘失败只告警，不影响任务执行。
    fn record(&self, ctx: &UsageContext, usage: &TokenUsage, at: DateTime<Utc>) -> UsageRecord {
        let record = UsageRecord {
            at,
            task_id: ctx.task_id,
            root_task_id: ctx.root_task_id,
            agent: ctx.agent.clone(),
            kind: ctx.kind.clone(),
            provider: usage.provider.clone(),
            model: usage.model.clone(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost: self
                .price(&usage.provider, &usage.model)
                .map(|p| p.cost(usage)),
        };
        if let Err(error) = self.append(&record) {
            warn!(event = "UsageRecordWriteFailed", task_id = %ctx.task_id, error = %error,
                "failed to append usage record");
        }
        record
    }

    /// 读取 `[from, to)` 内的记录（本地时间），跳过损坏行。
    fn read_range(&self, from: DateTime<Local>, to: DateTime<Local>) -> Vec<UsageRecord> {
        let mut records = Vec::new();
        let mut month = from.with_day(1).unwrap_or(from);
        while month < to {
            if let Ok(content) = std::fs::read_to_string(self.file_for(month)) {
                records.extend(
                    content
                        .lines()
                        .filter_map(|line| serde_json::from_str::<UsageRecord>(line).ok())
                        .filter(|r| {
                            let local = r.at.with_timezone(&Local);
                            local >= from && local < to
                        }),
                );
            }
            match month.checked_add_months(Months::new(1)) {
                Some(next) => month = next,
                None => break,
            }
        }
        records
    }
}

#[cfg(test)]
mod tests;
