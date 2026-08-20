//! 共享 mock executor 的行为测试。
//!
//! 独立成测试文件（而非 `#[cfg(test)]` 内嵌在 `tests/common/` 中），避免模块被
//! 多个测试 binary 以 `mod common;` 引入时重复执行同一批测试。

mod common;

use common::mock_executor::{
    BrainAwareEchoExecutor, CannedExecutor, DEFAULT_BRAIN_DECISION_JSON, EchoExecutor,
    MockExecutor, NoOpExecutor, PanickingExecutor, PromptEchoExecutor, text_output,
};
use harness::{
    domain::AgentExecutionOutput, domain::AgentExecutionRequest, domain::AgentExecutor,
    domain::AgentRequestKind, domain::OutputContent,
};

fn sample_request(kind: AgentRequestKind) -> AgentExecutionRequest {
    AgentExecutionRequest {
        task_id: harness::domain::TaskId::new(),
        agent_id: harness::domain::AgentId::new(),
        request_kind: kind,
        prompt: "hello".to_string(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
        model_override: None,
    }
}

fn run(executor: &dyn AgentExecutor, request: AgentExecutionRequest) -> AgentExecutionOutput {
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(executor.execute(request))
        .expect("executor output")
}

#[test]
fn echo_executor_returns_fixed_text() {
    let out = run(
        &EchoExecutor,
        sample_request(AgentRequestKind::LlmCompletion),
    );
    assert_eq!(out.content, OutputContent::Text("echo".to_string()));
}

#[test]
fn prompt_echo_executor_echoes_prompt() {
    let out = run(
        &PromptEchoExecutor,
        sample_request(AgentRequestKind::LlmCompletion),
    );
    assert_eq!(out.content, OutputContent::Text("echo: hello".to_string()));
}

#[test]
fn mock_executor_returns_fixed_text() {
    let out = run(
        &MockExecutor,
        sample_request(AgentRequestKind::LlmCompletion),
    );
    assert_eq!(
        out.content,
        OutputContent::Text("mock response".to_string())
    );
}

#[test]
fn no_op_executor_returns_ok() {
    let out = run(
        &NoOpExecutor,
        sample_request(AgentRequestKind::LlmCompletion),
    );
    assert_eq!(out.content, OutputContent::Text("ok".to_string()));
}

#[test]
fn brain_aware_executor_returns_json_for_brain_decision() {
    let executor = BrainAwareEchoExecutor::new("echo reply");
    let out = run(&executor, sample_request(AgentRequestKind::BrainDecision));
    assert_eq!(
        out.content,
        OutputContent::Text(DEFAULT_BRAIN_DECISION_JSON.to_string())
    );
}

#[test]
fn brain_aware_executor_returns_fallback_for_other_kinds() {
    let executor = BrainAwareEchoExecutor::new("echo reply");
    let out = run(&executor, sample_request(AgentRequestKind::LlmCompletion));
    assert_eq!(out.content, OutputContent::Text("echo reply".to_string()));
}

#[test]
fn canned_executor_returns_preset_sequence_then_done() {
    let executor = CannedExecutor::new(vec![text_output("first"), text_output("second")]);
    let first = run(&executor, sample_request(AgentRequestKind::LlmCompletion));
    let second = run(&executor, sample_request(AgentRequestKind::LlmCompletion));
    let exhausted = run(&executor, sample_request(AgentRequestKind::LlmCompletion));
    assert_eq!(first.content, OutputContent::Text("first".to_string()));
    assert_eq!(second.content, OutputContent::Text("second".to_string()));
    assert_eq!(exhausted.content, OutputContent::Text("done".to_string()));
}

#[test]
fn canned_executor_workitem_request_returns_ok() {
    let executor = CannedExecutor::new(vec![text_output("first")]);
    let mut request = sample_request(AgentRequestKind::LlmCompletion);
    request.work_item_id = Some(uuid::Uuid::new_v4());
    let out = run(&executor, request);
    assert_eq!(out.content, OutputContent::Text("ok".to_string()));
    // 预设序列未被消费
    let next = run(&executor, sample_request(AgentRequestKind::LlmCompletion));
    assert_eq!(next.content, OutputContent::Text("first".to_string()));
}

#[test]
fn canned_executor_set_responses_replaces_sequence() {
    let executor = CannedExecutor::new(vec![text_output("old")]);
    executor.set_responses(vec![text_output("new")]);
    let out = run(&executor, sample_request(AgentRequestKind::LlmCompletion));
    assert_eq!(out.content, OutputContent::Text("new".to_string()));
}

#[test]
#[should_panic(expected = "executor should not run in this test")]
fn panicking_executor_panics() {
    let _ = run(
        &PanickingExecutor,
        sample_request(AgentRequestKind::LlmCompletion),
    );
}
