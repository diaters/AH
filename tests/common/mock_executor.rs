//! 共享 mock executor 集。
//!
//! 收敛集成测试中跨文件重复的 `AgentExecutor` mock 实现，行为以本模块为准。
//! 设计依据：`docs/design/2026-08-16-real-llm-scenario-testing-design.md` 第 8 节。
//!
//! 收敛原则：只承载跨文件重复的定义；单场景专用 mock（如 `InfiniteToolCallExecutor`、
//! `SummarizationMockExecutor`）保留在各测试文件本地。

use std::collections::VecDeque;
use std::sync::Mutex;

use harness::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, AgentRequestKind, ExecutorFuture,
    OutputContent,
};

/// 构造纯文本输出（`reasoning_content` 为 None）。
pub fn text_output(text: &str) -> AgentExecutionOutput {
    AgentExecutionOutput {
        content: OutputContent::Text(text.to_string()),
        reasoning_content: None,
    }
}

/// 标准 BrainDecision JSON 决策（选择 default-llm-agent）。
///
/// TopLevelTask 经 `user_message_to_task_system` 创建时附加 `PendingDispatch(BrainLlm)`，
/// 走 BrainLlm 派发路径的测试都需要 BrainDecision 请求返回该 JSON。
pub const DEFAULT_BRAIN_DECISION_JSON: &str =
    r#"{"agent_name":"default-llm-agent","skill_name":null}"#;

/// 固定返回 `"echo"` 文本。
pub struct EchoExecutor;

impl AgentExecutor for EchoExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async { Ok(text_output("echo")) })
    }
}

/// 回显 prompt：返回 `echo: {prompt}`。
pub struct PromptEchoExecutor;

impl AgentExecutor for PromptEchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let echoed = format!("echo: {}", request.prompt);
        Box::pin(async move { Ok(text_output(&echoed)) })
    }
}

/// 固定返回 `"mock response"` 文本。
pub struct MockExecutor;

impl AgentExecutor for MockExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async { Ok(text_output("mock response")) })
    }
}

/// 固定返回 `"ok"` 文本。
pub struct NoOpExecutor;

impl AgentExecutor for NoOpExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async { Ok(text_output("ok")) })
    }
}

/// panic 守卫：不应被执行，一旦被调用即 panic。
///
/// 用于验证"executor 不应运行"路径（如审批禁用时事件任务直接失败）。
pub struct PanickingExecutor;

impl AgentExecutor for PanickingExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> ExecutorFuture {
        Box::pin(async { panic!("executor should not run in this test") })
    }
}

/// Brain 决策感知的回显执行器。
///
/// `BrainDecision` 请求返回 [`DEFAULT_BRAIN_DECISION_JSON`]，其余请求返回
/// `fallback_text`。适用于带 Brain 派发路径的多轮/多场景测试。
pub struct BrainAwareEchoExecutor {
    fallback_text: &'static str,
}

impl BrainAwareEchoExecutor {
    pub fn new(fallback_text: &'static str) -> Self {
        Self { fallback_text }
    }
}

impl AgentExecutor for BrainAwareEchoExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        if request.request_kind == AgentRequestKind::BrainDecision {
            return Box::pin(async { Ok(text_output(DEFAULT_BRAIN_DECISION_JSON)) });
        }
        let fallback = self.fallback_text;
        Box::pin(async move { Ok(text_output(fallback)) })
    }
}

/// 按顺序返回预设 LLM 输出的执行器，用于端到端测试。
///
/// - `BrainDecision` 请求返回标准 JSON 决策（见 [`DEFAULT_BRAIN_DECISION_JSON`]）。
/// - 治理型 WorkItem / 非普通 LLM 请求直接返回 `"ok"` 占位文本，避免干扰主流程。
/// - 普通请求按预设序列 `pop_front` 返回；序列耗尽后返回 `"done"`。
pub struct CannedExecutor {
    responses: Mutex<VecDeque<AgentExecutionOutput>>,
}

impl CannedExecutor {
    pub fn new(responses: Vec<AgentExecutionOutput>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }

    /// 运行时替换预设响应序列。
    pub fn set_responses(&self, responses: Vec<AgentExecutionOutput>) {
        *self.responses.lock().unwrap() = responses.into();
    }
}

impl AgentExecutor for CannedExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        if request.request_kind == AgentRequestKind::BrainDecision {
            return Box::pin(async { Ok(text_output(DEFAULT_BRAIN_DECISION_JSON)) });
        }
        if request.work_item_id.is_some() || request.request_kind != AgentRequestKind::LlmCompletion
        {
            return Box::pin(async { Ok(text_output("ok")) });
        }

        let response = self.responses.lock().unwrap().pop_front();
        Box::pin(async { Ok(response.unwrap_or_else(|| text_output("done"))) })
    }
}
