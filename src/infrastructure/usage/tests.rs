use super::*;
use crate::domain::{TaskId, UsageRange, summarize_usage as summarize};
use chrono::TimeZone;
use tempfile::TempDir;

fn usage(model: &str, input: u64, cached: u64, output: u64) -> TokenUsage {
    TokenUsage {
        provider: "qianfan".into(),
        model: model.into(),
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
        reasoning_tokens: 0,
    }
}

fn ctx(agent: &str, kind: &str, root: TaskId) -> UsageContext {
    UsageContext {
        task_id: TaskId::new(),
        root_task_id: root,
        agent: agent.into(),
        kind: kind.into(),
    }
}

fn ledger(dir: &TempDir) -> FileUsageLedger {
    let mut prices = HashMap::new();
    prices.insert(
        ("qianfan".to_string(), "priced".to_string()),
        ModelPrice {
            input: 2.0,
            output: 8.0,
            cached_input: Some(0.5),
        },
    );
    FileUsageLedger::new(dir.path().join("usage"), prices)
}

fn local(y: i32, m: u32, d: u32, h: u32) -> DateTime<Local> {
    Local.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}

#[test]
fn price_cost_counts_cached_input_separately() {
    let price = ModelPrice {
        input: 2.0,
        output: 8.0,
        cached_input: Some(0.5),
    };
    // 未命中 200k×2 + 命中 800k×0.5 + 输出 100k×8 = 0.4 + 0.4 + 0.8
    let cost = price.cost(&usage("m", 1_000_000, 800_000, 100_000));
    assert!((cost - 1.6).abs() < 1e-9, "{cost}");
}

#[test]
fn record_appends_monthly_files_and_reads_range_across_months() {
    let dir = TempDir::new().unwrap();
    let ledger = ledger(&dir);
    let root = TaskId::new();
    let oct = local(2026, 10, 31, 23).with_timezone(&Utc);
    let nov = local(2026, 11, 1, 9).with_timezone(&Utc);
    let priced = ledger.record(
        &ctx("main", "task", root),
        &usage("priced", 1000, 0, 500),
        oct,
    );
    let unpriced = ledger.record(&ctx("main", "task", root), &usage("free", 10, 0, 5), nov);
    assert_eq!(priced.cost, Some((1000.0 * 2.0 + 500.0 * 8.0) / 1e6));
    assert_eq!(unpriced.cost, None);
    assert!(dir.path().join("usage/usage-2026-10.jsonl").is_file());
    assert!(dir.path().join("usage/usage-2026-11.jsonl").is_file());

    let all = ledger.read_range(local(2026, 10, 1, 0), local(2026, 12, 1, 0));
    assert_eq!(all, vec![priced, unpriced.clone()]);
    let november = ledger.read_range(local(2026, 11, 1, 0), local(2026, 12, 1, 0));
    assert_eq!(november, vec![unpriced]);
}

#[test]
fn corrupt_lines_are_skipped() {
    let dir = TempDir::new().unwrap();
    let ledger = ledger(&dir);
    let at = local(2026, 11, 2, 10).with_timezone(&Utc);
    ledger.record(
        &ctx("main", "task", TaskId::new()),
        &usage("priced", 1, 0, 1),
        at,
    );
    let path = dir.path().join("usage/usage-2026-11.jsonl");
    let mut content = std::fs::read_to_string(&path).unwrap();
    content.push_str("{broken\n");
    std::fs::write(&path, content).unwrap();
    assert_eq!(
        ledger
            .read_range(local(2026, 11, 1, 0), local(2026, 12, 1, 0))
            .len(),
        1
    );
}

#[test]
fn summary_groups_and_fills_empty_days() {
    let dir = TempDir::new().unwrap();
    let ledger = ledger(&dir);
    let root_a = TaskId::new();
    let root_b = TaskId::new();
    let now = local(2026, 11, 2, 15);
    let records = vec![
        ledger.record(
            &ctx("main", "task", root_a),
            &usage("priced", 1000, 0, 500),
            local(2026, 11, 2, 9).with_timezone(&Utc),
        ),
        ledger.record(
            &ctx("summarizer", "summarization", root_a),
            &usage("free", 300, 0, 100),
            local(2026, 10, 31, 9).with_timezone(&Utc),
        ),
        ledger.record(
            &ctx("main", "task", root_b),
            &usage("priced", 100, 0, 50),
            local(2026, 11, 2, 11).with_timezone(&Utc),
        ),
    ];
    let summary = summarize(&records, UsageRange::Days7, now);
    assert_eq!(summary.from, "2026-10-27");
    assert_eq!(summary.to, "2026-11-02");
    assert_eq!(summary.by_day.len(), 7);
    assert_eq!(summary.by_day[6].calls, 2);
    assert_eq!(summary.by_day[4].calls, 1);
    assert_eq!(summary.total.calls, 3);
    assert_eq!(summary.total.unpriced_calls, 1);
    assert_eq!(summary.total.input_tokens, 1400);
    assert_eq!(summary.by_root_task[0].key, root_a.to_string());
    assert_eq!(summary.by_kind.len(), 2);
    assert_eq!(summary.by_model[0].key, "qianfan/priced");
}

#[test]
fn range_bounds_follow_local_calendar() {
    let now = local(2026, 11, 2, 15);
    assert_eq!(
        UsageRange::Today.bounds(now),
        (local(2026, 11, 2, 0), local(2026, 11, 3, 0))
    );
    assert_eq!(UsageRange::Month.bounds(now).0, local(2026, 11, 1, 0));
    assert_eq!(UsageRange::Days30.bounds(now).0, local(2026, 10, 4, 0));
    assert!(UsageRange::parse("year").is_none());
}
