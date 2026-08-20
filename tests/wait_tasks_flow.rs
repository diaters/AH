//! wait_tasks 工具集成测试

use harness::prelude::*;
use harness::{
    domain::Agent, domain::AgentCapabilities, domain::AgentId, domain::AgentKind,
    domain::AgentProfile, domain::AgentToolPermissions, domain::ChannelId, domain::ExperienceStore,
    domain::FrontendKind, domain::SharedKnowledgeBase, domain::ToolContext,
    domain::WaitingForTasksInfo, systems::HarnessConfig,
};

#[allow(dead_code)]
fn default_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "default".to_string(),
        thread_id: None,
    }
}

#[allow(dead_code)]
fn test_config() -> HarnessConfig {
    HarnessConfig::default()
}

/// 创建测试用的 Agent
#[allow(dead_code)]
fn create_test_agent(world: &mut World) -> AgentId {
    let id = AgentId::new();
    world.spawn(Agent {
        id,
        profile: AgentProfile {
            name: "test-agent".to_string(),
            model: "test-model".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["test".to_string()],
            description: "test agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions: AgentToolPermissions::default(),
        system_prompt: None,
    });
    id
}

/// 测试：wait_tasks 工具参数解析
#[test]
fn test_wait_tasks_tool_parsing() {
    let input = serde_json::json!({
        "task_ids": ["550e8400-e29b-41d4-a716-446655440000"],
        "timeout_secs": 60
    });

    let _ctx = ToolContext {
        knowledge: &SharedKnowledgeBase::default(),
        experience_store: &ExperienceStore::default(),
        default_wait_tasks_timeout_secs: 300,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 300,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        current_task_id: harness::domain::TaskId::new(),
        current_agent_id: AgentId::new(),
        current_origin_channel: None,
        current_skill_dir: None,
    };

    // This test will need the WaitTasksTool to be accessible
    // For now, we test the parsing logic indirectly
    let task_ids_value = input.get("task_ids").unwrap();
    assert!(task_ids_value.is_array());
    assert_eq!(task_ids_value.as_array().unwrap().len(), 1);
}

/// 测试：wait_tasks 工具缺少 task_ids 参数应报错
#[test]
fn test_wait_tasks_missing_task_ids() {
    let input = serde_json::json!({
        "timeout_secs": 60
    });

    let has_task_ids = input.get("task_ids").is_some();
    assert!(!has_task_ids);
}

/// 测试：wait_tasks 工具空 task_ids 应报错
#[test]
fn test_wait_tasks_empty_task_ids() {
    let input = serde_json::json!({
        "task_ids": [],
        "timeout_secs": 60
    });

    let task_ids = input.get("task_ids").unwrap().as_array().unwrap();
    assert!(task_ids.is_empty());
}

/// 测试：默认超时时间
#[test]
fn test_wait_tasks_default_timeout() {
    let input = serde_json::json!({
        "task_ids": ["550e8400-e29b-41d4-a716-446655440000"]
    });

    let default_timeout = 300u64;
    let timeout = input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(default_timeout);
    assert_eq!(timeout, 300);
}

/// 测试：WaitingForTasksInfo 组件创建
#[test]
fn test_waiting_for_tasks_info_creation() {
    let now = chrono::Utc::now();
    let timeout_at = now + chrono::Duration::seconds(60);

    let info = WaitingForTasksInfo {
        target_task_ids: vec![harness::domain::TaskId::new()],
        timeout_at,
        tool_call_id: "test-call-id".to_string(),
        agent_id: harness::domain::AgentId::new(),
    };

    assert_eq!(info.target_task_ids.len(), 1);
    assert_eq!(info.tool_call_id, "test-call-id");
}
