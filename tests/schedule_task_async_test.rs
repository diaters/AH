//! Task 14: schedule_task 工具迁移上异步桥的单元测试。
//!
//! 验证 `ScheduleTaskTool` 上桥后：
//! - `kind() == Async`
//! - sync `execute(...)` 防御性返回 `InternalState` 错误（参考
//!   `list_experience_candidates.rs` 与 `delete.rs` 模式）
//! - `run_async(input, ctx)` 返回 `ToolWorkerOutput::Effect(ToolEffect::ScheduleTask { ... })`，
//!   字段（id / kind / content / schedule / output_channel）与原 sync 实现等价
//!
//! 注：用独立 `tokio::runtime::Runtime::block_on` 跑 `run_async`——工具本体单元
//! 测试，不进 ECS、不依赖 `AsyncRuntime` 资源；`#[test]` 而非 `#[tokio::test]`
//! 仍遵守（runtime 嵌套 panic 规避）。

use chrono::Local;
use harness::domain::{
    BuiltinTool, ChannelId, FrontendKind, OwnedToolContext, ToolActionKind, ToolError,
    ToolWorkerOutput,
};
use harness::systems::tools::builtin::ScheduleTaskTool;
use harness::triggers::ScheduleSpec;

fn future_local_iso() -> String {
    let future = Local::now() + chrono::Duration::days(1);
    future.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn future_rfc3339() -> String {
    let future = Local::now() + chrono::Duration::days(1);
    future.to_rfc3339()
}

fn inherited_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Telegram,
        user_id: "inherited-tg".to_string(),
        thread_id: None,
    }
}

fn ctx_with_channel(channel: Option<ChannelId>) -> OwnedToolContext {
    let mut ctx = OwnedToolContext::empty_for_test(300);
    ctx.current_origin_channel = channel;
    ctx
}

#[test]
fn schedule_task_kind_is_async() {
    assert_eq!(ScheduleTaskTool.kind(), ToolActionKind::Async);
}

#[test]
fn schedule_task_execute_is_async_only_defense() {
    let result = ScheduleTaskTool.execute(
        &serde_json::json!({}),
        &harness::domain::ToolContext {
            knowledge: Box::leak(Box::new(harness::domain::SharedKnowledgeBase::default())),
            experience_store: Box::leak(Box::new(harness::domain::ExperienceStore::default())),
            default_wait_tasks_timeout_secs: 300,
            shell_default_tail_lines: 50,
            shell_max_tail_lines: 500,
            shell_default_exec_timeout_secs: 60,
            shell_default_stop_timeout_secs: 5,
            tool_inflight_timeout_secs: 300,
            current_task_id: uuid::Uuid::new_v4(),
            current_agent_id: uuid::Uuid::new_v4(),
            current_origin_channel: None,
        },
    );
    assert!(matches!(result, Err(ToolError::InternalState(_))));
}

#[test]
fn run_async_returns_schedule_task_effect_with_inherited_channel() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "send report",
        "schedule": format!("once:{}", future_local_iso()),
    });
    let inherited = inherited_channel();
    let output = rt
        .block_on(tool.run_async(input, ctx_with_channel(Some(inherited.clone()))))
        .unwrap();
    match output {
        ToolWorkerOutput::Effect(harness::domain::ToolEffect::ScheduleTask {
            content,
            kind,
            schedule,
            output_channel,
            ..
        }) => {
            assert_eq!(content, "send report");
            assert!(kind.starts_with("scheduled:"));
            assert!(matches!(schedule, ScheduleSpec::Once(_)));
            assert_eq!(output_channel, Some(inherited));
        }
        other => panic!("expected ScheduleTask effect, got {:?}", other),
    }
}

#[test]
fn run_async_accepts_rfc3339_once_schedule() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "x",
        "schedule": format!("once:{}", future_rfc3339()),
        "output_channel": "qq",
        "target": "qq-user"
    });
    let output = rt
        .block_on(tool.run_async(input, ctx_with_channel(None)))
        .unwrap();
    match output {
        ToolWorkerOutput::Effect(harness::domain::ToolEffect::ScheduleTask {
            output_channel,
            schedule,
            ..
        }) => {
            assert!(matches!(schedule, ScheduleSpec::Once(_)));
            let channel = output_channel.expect("output_channel should be set");
            assert_eq!(channel.frontend, FrontendKind::QQ);
            assert_eq!(channel.user_id, "qq-user");
        }
        other => panic!("expected ScheduleTask effect, got {:?}", other),
    }
}

#[test]
fn run_async_accepts_cron_schedule() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "daily standup",
        "schedule": "cron:0 9 * * 1-5",
        "output_channel": "tui",
        "target": "user-1"
    });
    let output = rt
        .block_on(tool.run_async(input, ctx_with_channel(None)))
        .unwrap();
    match output {
        ToolWorkerOutput::Effect(harness::domain::ToolEffect::ScheduleTask {
            content,
            schedule,
            ..
        }) => {
            assert_eq!(content, "daily standup");
            assert!(matches!(schedule, ScheduleSpec::Cron(_)));
        }
        other => panic!("expected ScheduleTask effect, got {:?}", other),
    }
}

#[test]
fn run_async_rejects_missing_content() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "schedule": format!("once:{}", future_local_iso()),
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_rejects_missing_schedule() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({"content": "do something"});
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_rejects_invalid_schedule_prefix() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "do something",
        "schedule": "every 5 minutes"
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_rejects_once_in_past() {
    let past = Local::now() - chrono::Duration::days(1);
    let past_str = past.format("%Y-%m-%dT%H:%M:%S").to_string();
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "do something",
        "schedule": format!("once:{}", past_str),
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_rejects_explicit_output_channel_without_target() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "do something",
        "schedule": format!("once:{}", future_local_iso()),
        "output_channel": "tui",
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_rejects_unknown_output_channel() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "do something",
        "schedule": format!("once:{}", future_local_iso()),
        "output_channel": "irc",
        "target": "u1"
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_rejects_no_output_channel_and_no_inherited() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "do something",
        "schedule": format!("once:{}", future_local_iso()),
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_explicit_output_channel_overrides_inherited() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "do something",
        "schedule": format!("once:{}", future_local_iso()),
        "output_channel": "qq",
        "target": "qq-user"
    });
    let output = rt
        .block_on(tool.run_async(input, ctx_with_channel(Some(inherited_channel()))))
        .unwrap();
    match output {
        ToolWorkerOutput::Effect(harness::domain::ToolEffect::ScheduleTask {
            output_channel,
            ..
        }) => {
            let channel = output_channel.expect("output_channel should be set");
            assert_eq!(channel.frontend, FrontendKind::QQ);
            assert_eq!(channel.user_id, "qq-user");
        }
        other => panic!("expected ScheduleTask effect, got {:?}", other),
    }
}

#[test]
fn run_async_kind_field_starts_with_scheduled_prefix() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "x",
        "schedule": format!("once:{}", future_local_iso()),
        "output_channel": "tui",
        "target": "u"
    });
    let output = rt
        .block_on(tool.run_async(input, ctx_with_channel(None)))
        .unwrap();
    match output {
        ToolWorkerOutput::Effect(harness::domain::ToolEffect::ScheduleTask {
            id, kind, ..
        }) => {
            assert!(kind.starts_with("scheduled:"));
            let suffix = kind.strip_prefix("scheduled:").unwrap();
            let parsed = uuid::Uuid::parse_str(suffix).expect("kind suffix should be a UUID");
            assert_eq!(parsed, id);
        }
        other => panic!("expected ScheduleTask effect, got {:?}", other),
    }
}

#[test]
fn run_async_rejects_invalid_cron() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let input = serde_json::json!({
        "content": "x",
        "schedule": "cron:not-a-cron",
        "output_channel": "tui",
        "target": "u"
    });
    let result = rt.block_on(tool.run_async(input, ctx_with_channel(None)));
    assert!(matches!(result, Err(ToolError::InvalidInput(_))));
}

#[test]
fn run_async_all_frontend_kinds_accepted() {
    let tool = ScheduleTaskTool;
    let rt = tokio::runtime::Runtime::new().unwrap();
    for (name, expected) in [
        ("tui", FrontendKind::Tui),
        ("telegram", FrontendKind::Telegram),
        ("web", FrontendKind::Web),
        ("qq", FrontendKind::QQ),
        ("feishu", FrontendKind::Feishu),
    ] {
        let input = serde_json::json!({
            "content": "x",
            "schedule": format!("once:{}", future_local_iso()),
            "output_channel": name,
            "target": "u"
        });
        let output = rt
            .block_on(tool.run_async(input, ctx_with_channel(None)))
            .unwrap_or_else(|e| panic!("frontend {} should be accepted, got err {:?}", name, e));
        match output {
            ToolWorkerOutput::Effect(harness::domain::ToolEffect::ScheduleTask {
                output_channel,
                ..
            }) => {
                let channel = output_channel.expect("output_channel should be set");
                assert_eq!(channel.frontend, expected, "frontend {} mismatch", name);
            }
            other => panic!("expected ScheduleTask effect, got {:?}", other),
        }
    }
}
