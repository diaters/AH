//! 客户端用量查询：读取运行时用量账本，按会话归属后返回汇总。
//!
//! 总计、每日、模型、Agent、调用类型为整个实例的用量；会话明细只列当前用户的会话，
//! 其余（IM 通道、定时任务、其他用户）合并为 `other`。

use std::collections::{BTreeMap, HashMap};

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde_json::{Value, json};

use super::{Client, ClientError};
use crate::domain::{UsageBucket, UsageRange, summarize_usage as summarize};

/// 会话明细最多返回的条数。
const TOP_SESSIONS: usize = 10;

pub(super) async fn summary(
    State(client): State<Client>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ClientError> {
    let device = super::server::authenticate(&client, &headers)?;
    let ledger = client.usage.as_ref().ok_or(ClientError::NotFound)?;
    let range = match params.get("range") {
        Some(raw) => UsageRange::parse(raw).ok_or(ClientError::Invalid("unknown usage range"))?,
        None => UsageRange::Days7,
    };
    let now = chrono::Local::now();
    let (from, to) = range.bounds(now);
    let records = ledger.read_range(from, to);
    let summary = summarize(&records, range, now);
    let task_sessions = client
        .store
        .lock()
        .expect("client state mutex poisoned")
        .task_sessions(&device.user_id);
    let (sessions, other) = by_session(&summary.by_root_task, &task_sessions);

    let mut body = serde_json::to_value(&summary).map_err(|_| ClientError::StorageUnavailable)?;
    if let Some(object) = body.as_object_mut() {
        object.remove("by_root_task");
        object.insert("sessions".into(), json!(sessions));
        object.insert("other".into(), json!(other));
    }
    Ok(Json(body))
}

/// 根任务归属到会话（同一会话的多个根任务合并）；不属于该用户会话的合并到 other。
fn by_session(
    roots: &[UsageBucket],
    task_sessions: &HashMap<String, (uuid::Uuid, String)>,
) -> (Vec<Value>, UsageBucket) {
    let mut sessions: BTreeMap<uuid::Uuid, (String, UsageBucket)> = BTreeMap::new();
    let mut other = UsageBucket {
        key: "other".into(),
        ..Default::default()
    };
    for root in roots {
        let target = match task_sessions.get(&root.key) {
            Some((id, title)) => {
                &mut sessions
                    .entry(*id)
                    .or_insert_with(|| {
                        (
                            title.clone(),
                            UsageBucket {
                                key: id.to_string(),
                                ..Default::default()
                            },
                        )
                    })
                    .1
            }
            None => &mut other,
        };
        target.calls += root.calls;
        target.input_tokens += root.input_tokens;
        target.output_tokens += root.output_tokens;
        target.cached_input_tokens += root.cached_input_tokens;
        target.cost += root.cost;
        target.unpriced_calls += root.unpriced_calls;
    }
    let mut list: Vec<(String, UsageBucket)> = sessions.into_values().collect();
    list.sort_by(|a, b| {
        b.1.cost.total_cmp(&a.1.cost).then(
            (b.1.input_tokens + b.1.output_tokens).cmp(&(a.1.input_tokens + a.1.output_tokens)),
        )
    });
    let values = list
        .into_iter()
        .take(TOP_SESSIONS)
        .map(|(title, bucket)| {
            let mut value = json!(bucket);
            value["title"] = json!(title);
            value
        })
        .collect();
    (values, other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(key: &str, calls: u64, cost: f64) -> UsageBucket {
        UsageBucket {
            key: key.into(),
            calls,
            input_tokens: calls * 100,
            output_tokens: calls * 10,
            cost,
            ..Default::default()
        }
    }

    #[test]
    fn roots_merge_per_session_and_unknown_go_to_other() {
        let session = uuid::Uuid::new_v4();
        let mut map = HashMap::new();
        map.insert("root-a".to_string(), (session, "排查 bug".to_string()));
        map.insert("root-b".to_string(), (session, "排查 bug".to_string()));
        let roots = [
            bucket("root-a", 2, 0.5),
            bucket("root-b", 1, 0.25),
            bucket("im-root", 3, 1.0),
        ];
        let (sessions, other) = by_session(&roots, &map);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["title"], "排查 bug");
        assert_eq!(sessions[0]["calls"], 3);
        assert_eq!(sessions[0]["cost"], 0.75);
        assert_eq!(other.calls, 3);
    }
}
