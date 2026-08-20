//! Task 13：`list_experience_candidates` 迁移上异步桥的 golden 对照测试。
//!
//! 验证：
//! - `kind()` override 为 `Async`
//! - `execute()` 返回 `InternalState` 错误（async-only 快速失败）
//! - `run_async()` 输出与迁移前 sync 路径的 golden JSON 完全一致：
//!   - 空收件箱返回 `{"count":0,"items":[]}`
//!   - Knowledge 类短 content 完整输出
//!   - Knowledge 类长 content（>200 字符）截断 + "…"
//!   - Skill 类 summary 取 description
//! - 缺 `experience_candidates` 快照返回 `InternalState` 错误
//! - 缺 `current_task_id` 返回 `InternalState` 错误
//!
//! golden JSON 直接硬编码，避免依赖即将下线的 `execute()`。

use std::sync::Arc;

use harness::domain::{
    BuiltinTool, ExperienceCandidate, ExperienceStore, OwnedToolContext, SharedKnowledgeBase,
    ToolActionKind, ToolContext, ToolError, ToolWorkerOutput,
};
use harness::systems::tools::builtin::ListExperienceCandidatesTool;

/// 构造一个 `OwnedToolContext`，把给定候选列表挂到 `experience_candidates` 快照。
fn ctx_with_candidates(candidates: Vec<ExperienceCandidate>) -> OwnedToolContext {
    OwnedToolContext {
        experience_candidates: Some(Arc::new(candidates)),
        current_task_id: Some(harness::domain::TaskId::nil()),
        tool_inflight_timeout_secs: 300,
        ..Default::default()
    }
}

/// 用 `ExperienceStore::queue_for_parent` 把候选入箱，再 `list_for_task` 拉出来，
/// 让状态流（Submitted → InInbox）与生产运行时一致。dispatch 抓快照时也是走
/// `list_for_task`，因此测试用同一路径构造快照最具代表性。
fn snapshot_from_store(
    task_id: harness::domain::TaskId,
    agent_id: harness::domain::AgentId,
    candidates: Vec<ExperienceCandidate>,
) -> Vec<ExperienceCandidate> {
    let mut store = ExperienceStore::default();
    for c in candidates {
        store.queue_for_parent(task_id, agent_id, c);
    }
    store.list_for_task(task_id).into_iter().cloned().collect()
}

#[test]
fn kind_is_async() {
    let tool = ListExperienceCandidatesTool;
    assert_eq!(tool.kind(), ToolActionKind::Async);
}

#[test]
fn execute_returns_internal_state_error() {
    let tool = ListExperienceCandidatesTool;
    let knowledge = SharedKnowledgeBase::default();
    let store = ExperienceStore::default();
    let ctx = ToolContext {
        knowledge: &knowledge,
        experience_store: &store,
        default_wait_tasks_timeout_secs: 300,
        shell_default_tail_lines: 50,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 60,
        shell_default_stop_timeout_secs: 5,
        tool_inflight_timeout_secs: 300,
        current_task_id: harness::domain::TaskId::nil(),
        current_agent_id: harness::domain::AgentId::nil(),
        current_origin_channel: None,
        current_skill_dir: None,
    };
    let result = tool.execute(&serde_json::json!({}), &ctx);
    assert!(
        matches!(result, Err(ToolError::InternalState(_))),
        "expected InternalState error, got {:?}",
        result
    );
}

#[test]
fn run_async_empty_inbox_returns_empty_array() {
    let tool = ListExperienceCandidatesTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), ctx_with_candidates(vec![])))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value, got {:?}", output)
    };
    assert_eq!(v["count"], 0);
    assert_eq!(v["items"].as_array().unwrap().len(), 0);
}

#[test]
fn run_async_knowledge_short_content_matches_golden() {
    let task_id = harness::domain::TaskId::nil();
    let agent_id = harness::domain::AgentId::nil();
    let candidate_id = uuid::Uuid::nil();
    let candidate = ExperienceCandidate::knowledge(
        candidate_id,
        task_id,
        agent_id,
        "shell timeout".to_string(),
        "shell_stop 默认等待退出".to_string(),
    );
    let candidates = snapshot_from_store(task_id, agent_id, vec![candidate]);
    let tool = ListExperienceCandidatesTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), ctx_with_candidates(candidates)))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value, got {:?}", output)
    };

    let expected = serde_json::json!({
        "count": 1,
        "items": [
            {
                "candidate_id": candidate_id,
                "title": "shell timeout",
                "kind": "Knowledge",
                "status": "InInbox",
                "summary": "shell_stop 默认等待退出",
            }
        ]
    });
    assert_eq!(v, expected);
}

#[test]
fn run_async_knowledge_long_content_truncates_to_200_chars() {
    let task_id = harness::domain::TaskId::nil();
    let agent_id = harness::domain::AgentId::nil();
    let candidate_id = uuid::Uuid::nil();
    // 250 个字符，触发 >200 截断逻辑
    let long_content = "a".repeat(250);
    let candidate = ExperienceCandidate::knowledge(
        candidate_id,
        task_id,
        agent_id,
        "long knowledge".to_string(),
        long_content.clone(),
    );
    let candidates = snapshot_from_store(task_id, agent_id, vec![candidate]);
    let tool = ListExperienceCandidatesTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), ctx_with_candidates(candidates)))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value, got {:?}", output)
    };

    let expected_summary = format!("{}…", "a".repeat(200));
    let item = &v["items"][0];
    assert_eq!(item["kind"], "Knowledge");
    assert_eq!(item["summary"], expected_summary);
    // 截断后 summary 字符数 = 200 + 1（"…"）
    assert_eq!(item["summary"].as_str().unwrap().chars().count(), 201);
}

#[test]
fn run_async_skill_summary_uses_description() {
    let task_id = harness::domain::TaskId::nil();
    let agent_id = harness::domain::AgentId::nil();
    let candidate_id = uuid::Uuid::nil();
    let candidate = ExperienceCandidate::skill(
        candidate_id,
        task_id,
        agent_id,
        "skill title".to_string(),
        "skill_name".to_string(),
        "skill description text".to_string(),
        "instructions body".to_string(),
        vec![],
    );
    let candidates = snapshot_from_store(task_id, agent_id, vec![candidate]);
    let tool = ListExperienceCandidatesTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let output = rt
        .block_on(tool.run_async(serde_json::json!({}), ctx_with_candidates(candidates)))
        .unwrap();
    let ToolWorkerOutput::Value(v) = output else {
        panic!("expected Value, got {:?}", output)
    };

    let expected = serde_json::json!({
        "count": 1,
        "items": [
            {
                "candidate_id": candidate_id,
                "title": "skill title",
                "kind": "Skill",
                "status": "InInbox",
                "summary": "skill description text",
            }
        ]
    });
    assert_eq!(v, expected);
}

#[test]
fn run_async_missing_snapshot_returns_internal_state_error() {
    let tool = ListExperienceCandidatesTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // 缺 experience_candidates（None）
    let ctx = OwnedToolContext {
        current_task_id: Some(harness::domain::TaskId::nil()),
        ..Default::default()
    };
    let result = rt.block_on(tool.run_async(serde_json::json!({}), ctx));
    assert!(matches!(result, Err(ToolError::InternalState(_))));
}

#[test]
fn run_async_missing_current_task_id_returns_internal_state_error() {
    let tool = ListExperienceCandidatesTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // 有快照但缺 current_task_id
    let ctx = OwnedToolContext {
        experience_candidates: Some(Arc::new(vec![])),
        ..Default::default()
    };
    let result = rt.block_on(tool.run_async(serde_json::json!({}), ctx));
    assert!(matches!(result, Err(ToolError::InternalState(_))));
}
