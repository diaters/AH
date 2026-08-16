//! Layer 1 真实 LLM 冒烟测试。
//!
//! 定位（设计文档 `docs/design/2026-08-16-real-llm-scenario-testing-design.md` §4）：
//! 最小化验证 genai 适配层的连通性与往返正确性，全部为结构性断言，不判断语义。
//!
//! 双重门控，真实 API 测试永不进入 CI：
//! - `#[ignore]`：默认不随 `cargo test` 执行
//! - 环境变量：`HARNESS_TEST_REAL_LLM=1` + `HARNESS_LLM_API_KEY`
//!
//! provider 矩阵通过 `HARNESS_TEST_PROVIDER` 选择（默认 `openai`），一次只测一个 provider。
//! 模型通过 `HARNESS_MODEL` 覆盖（默认 `gpt-4.1-mini`）。
//!
//! 执行方式：
//!
//! ```text
//! HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=sk-... \
//!   cargo test --test real_llm_smoke -- --ignored --nocapture
//! ```
//!
//! 错误分类测试使用 wiremock 本地端点，确定性执行，不依赖真实 API，随 CI 常规运行。

use std::sync::Arc;
use std::time::Duration;

use harness::domain::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, AgentRequestKind, ExecutionError,
    OutputContent, ToolDefinition, ToolExecutorKind, ToolPermission, ToolSchema,
};
use harness::llm::{LlmProviderConfig, LlmProviderKind, create_executor_from_config};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 单次真实请求的 wall-clock 超时预算（设计文档 §4.1：分钟级）。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

fn real_llm_enabled() -> bool {
    std::env::var("HARNESS_TEST_REAL_LLM").is_ok() && std::env::var("HARNESS_LLM_API_KEY").is_ok()
}

fn skip_notice() {
    eprintln!(
        "SKIP: 真实 LLM 冒烟测试需要 HARNESS_TEST_REAL_LLM=1 与 HARNESS_LLM_API_KEY，\
         详见 docs/design/2026-08-16-real-llm-scenario-testing-design.md §4.2"
    );
}

/// 从测试环境变量构建 provider 配置。
fn smoke_provider_config() -> LlmProviderConfig {
    let provider_raw =
        std::env::var("HARNESS_TEST_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    let provider = LlmProviderKind::parse(&provider_raw).expect("HARNESS_TEST_PROVIDER 应可解析");
    let model = std::env::var("HARNESS_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());
    let api_key = std::env::var("HARNESS_LLM_API_KEY").ok();
    let api_base = std::env::var("HARNESS_LLM_API_BASE").ok();

    LlmProviderConfig {
        provider,
        model,
        api_key,
        api_base,
    }
}

/// 标准 provider 的 key 由 genai 从原生环境变量读取（如 `OPENAI_API_KEY`）。
/// 若用户只设置了 `HARNESS_LLM_API_KEY`，测试进程内桥接到原生变量，保持单一 key 入口；
/// 已设置的原生变量优先，不会被覆盖。
fn bridge_api_key_if_needed(config: &LlmProviderConfig) {
    let Ok(harness_key) = std::env::var("HARNESS_LLM_API_KEY") else {
        return;
    };
    let native_var = match config.provider {
        LlmProviderKind::OpenAi => Some("OPENAI_API_KEY"),
        LlmProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
        LlmProviderKind::DeepSeek => Some("DEEPSEEK_API_KEY"),
        // OpenAiCompatible 的 key 经 ServiceTargetResolver 直接注入，无需原生变量
        LlmProviderKind::OpenAiCompatible => None,
    };
    if let Some(var) = native_var
        && std::env::var(var).is_err()
    {
        // 测试进程内的桥接；Rust 2024 中 set_var 为 unsafe
        unsafe { std::env::set_var(var, harness_key) };
    }
}

fn smoke_executor() -> Arc<dyn AgentExecutor> {
    ensure_crypto_provider();
    let config = smoke_provider_config();
    config.validate().expect("provider 配置校验应通过");
    bridge_api_key_if_needed(&config);
    create_executor_from_config(&config).expect("应能创建真实 executor")
}

/// 测试进程内安装 rustls ring CryptoProvider（生产入口 `main.rs` 同样处理）。
/// `install_default` 仅首次生效，重复调用返回 Err 可忽略。
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn sample_tool(name: &str, description: &str, param: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters: ToolSchema {
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    param: {"type": "string", "description": "value for the parameter"}
                },
                "required": [param]
            }),
        },
        default_permission: ToolPermission::Allow,
        executor: ToolExecutorKind::Builtin(name.to_string()),
        required_tag: None,
    }
}

fn smoke_request(prompt: &str, tools: Vec<ToolDefinition>) -> AgentExecutionRequest {
    AgentExecutionRequest {
        task_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: prompt.to_string(),
        system_prompt: None,
        tools,
        conversation: None,
        work_item_id: None,
        model_override: None,
    }
}

/// 在超时预算内执行请求，超时归类为 `ExecutionError::Timeout` 而非 hang。
async fn execute_with_budget(
    executor: &Arc<dyn AgentExecutor>,
    request: AgentExecutionRequest,
) -> Result<AgentExecutionOutput, ExecutionError> {
    match tokio::time::timeout(REQUEST_TIMEOUT, executor.execute(request)).await {
        Ok(result) => result,
        Err(_) => Err(ExecutionError::Timeout(
            "smoke test budget exceeded".to_string(),
        )),
    }
}

fn tool_names(calls: &[harness::domain::LlmToolCall]) -> Vec<&str> {
    calls.iter().map(|c| c.name.as_str()).collect()
}

// ============================================================
// 真实 API 冒烟测试（#[ignore] + 环境变量双重门控）
// ============================================================

/// 纯文本往返：响应在超时预算内返回非空文本（§4.3 第 1 行）。
#[tokio::test]
#[ignore = "需要真实 API：HARNESS_TEST_REAL_LLM=1 + HARNESS_LLM_API_KEY"]
async fn text_roundtrip_returns_nonempty_text_within_budget() {
    if !real_llm_enabled() {
        skip_notice();
        return;
    }

    let executor = smoke_executor();
    let request = smoke_request("Reply with the single word: OK", vec![]);

    let output = execute_with_budget(&executor, request)
        .await
        .expect("纯文本请求应成功");

    match output.content {
        OutputContent::Text(text) => {
            assert!(!text.trim().is_empty(), "响应文本不应为空");
        }
        other => panic!("预期文本响应，实际: {other:?}"),
    }
}

/// tool_calls 往返：请求携带工具定义，响应解析出 LlmToolCall，
/// name 与注册名一致，参数是合法 JSON（§4.3 第 2 行）。
#[tokio::test]
#[ignore = "需要真实 API：HARNESS_TEST_REAL_LLM=1 + HARNESS_LLM_API_KEY"]
async fn tool_call_roundtrip_parses_llm_tool_call() {
    if !real_llm_enabled() {
        skip_notice();
        return;
    }

    let executor = smoke_executor();
    let tool = sample_tool("get_weather", "Get the current weather for a city", "city");
    let request = smoke_request(
        "What is the weather in Paris right now? You must call the get_weather tool.",
        vec![tool],
    );

    let output = execute_with_budget(&executor, request)
        .await
        .expect("携带工具的请求应成功");

    match output.content {
        OutputContent::ToolCalls(calls) => {
            let call = calls
                .iter()
                .find(|c| c.name == "get_weather")
                .unwrap_or_else(|| {
                    panic!("应调用 get_weather，实际调用: {:?}", tool_names(&calls))
                });

            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|err| {
                    panic!("工具参数应为合法 JSON: {err}, 原始: {}", call.arguments)
                });
            assert!(
                args.get("city").is_some(),
                "schema required 参数 city 应出现在参数中，原始: {args}"
            );
        }
        OutputContent::Text(text) => {
            panic!("预期工具调用，模型返回了文本: {text}")
        }
    }
}

/// 工具名 sanitize 往返：含命名空间的工具名（如 `demo:echo`）经
/// sanitize（发给 LLM）→ unsanitize（解析响应）后与原始一致（§4.3 第 3 行）。
#[tokio::test]
#[ignore = "需要真实 API：HARNESS_TEST_REAL_LLM=1 + HARNESS_LLM_API_KEY"]
async fn sanitized_tool_name_roundtrip_preserves_namespace() {
    if !real_llm_enabled() {
        skip_notice();
        return;
    }

    let executor = smoke_executor();
    let tool = sample_tool("demo:echo", "Echo the given message back", "message");
    // LLM 在工具列表中看到的是 sanitize 后的名字 demo__echo
    let request = smoke_request(
        "Call the demo__echo tool with message set to \"hi\".",
        vec![tool],
    );

    let output = execute_with_budget(&executor, request)
        .await
        .expect("携带命名空间工具的请求应成功");

    match output.content {
        OutputContent::ToolCalls(calls) => {
            let call = calls
                .iter()
                .find(|c| c.name == "demo:echo")
                .unwrap_or_else(|| {
                    panic!(
                        "sanitize/unsanitize 往返后应还原为 demo:echo，实际调用: {:?}",
                        tool_names(&calls)
                    )
                });

            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|err| {
                    panic!("工具参数应为合法 JSON: {err}, 原始: {}", call.arguments)
                });
            assert!(
                args.get("message").is_some(),
                "schema required 参数 message 应出现在参数中，原始: {args}"
            );
        }
        OutputContent::Text(text) => {
            panic!("预期工具调用，模型返回了文本: {text}")
        }
    }
}

/// OpenAiCompatible 自定义端点：ServiceTargetResolver 注入的 base_url 生效，
/// 请求可达（§4.3 第 4 行）。仅当 HARNESS_TEST_PROVIDER=openai-compatible 时执行。
#[tokio::test]
#[ignore = "需要真实 API：HARNESS_TEST_PROVIDER=openai-compatible 及 key/base 配置"]
async fn openai_compatible_custom_endpoint_roundtrip() {
    if !real_llm_enabled() {
        skip_notice();
        return;
    }

    let config = smoke_provider_config();
    if config.provider != LlmProviderKind::OpenAiCompatible {
        eprintln!(
            "SKIP: 该测试仅在 HARNESS_TEST_PROVIDER=openai-compatible 时执行，当前: {:?}",
            config.provider
        );
        return;
    }

    let executor = smoke_executor();
    let request = smoke_request("Reply with the single word: OK", vec![]);

    let output = execute_with_budget(&executor, request)
        .await
        .expect("自定义端点请求应可达");

    match output.content {
        OutputContent::Text(text) => {
            assert!(!text.trim().is_empty(), "响应文本不应为空");
        }
        other => panic!("预期文本响应，实际: {other:?}"),
    }
}

// ============================================================
// 错误分类测试（wiremock 本地端点，确定性，随 CI 常规运行）
// ============================================================

fn compatible_config_with_base(api_base: String) -> LlmProviderConfig {
    LlmProviderConfig {
        provider: LlmProviderKind::OpenAiCompatible,
        model: "test-model".to_string(),
        api_key: Some("invalid-key".to_string()),
        api_base: Some(api_base),
    }
}

fn compatible_executor_with_base(api_base: String) -> Arc<dyn AgentExecutor> {
    ensure_crypto_provider();
    create_executor_from_config(&compatible_config_with_base(api_base)).expect("应能创建 executor")
}

/// 无效 key 触发 401：错误归类为 `Authentication`，不进入重试（§4.3 第 5 行）。
#[tokio::test]
async fn http_401_classified_as_authentication_and_not_retryable() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_string(r#"{"error": {"message": "Invalid API key provided"}}"#),
        )
        .mount(&mock_server)
        .await;

    let executor = compatible_executor_with_base(mock_server.uri());
    let request = smoke_request("hi", vec![]);

    let error = executor
        .execute(request)
        .await
        .expect_err("401 响应应返回错误");

    assert!(
        matches!(error, ExecutionError::Authentication(_)),
        "401 应归类为 Authentication，实际: {error:?}"
    );
    assert!(!error.is_retryable(), "Authentication 不应进入统一重试流程");
}

/// 429 触发限流：错误归类为 `RateLimited`，允许重试（§4.3 第 5 行可选项）。
#[tokio::test]
async fn http_429_classified_as_rate_limited_and_retryable() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_string(r#"{"error": {"message": "Rate limit reached"}}"#),
        )
        .mount(&mock_server)
        .await;

    let executor = compatible_executor_with_base(mock_server.uri());
    let request = smoke_request("hi", vec![]);

    let error = executor
        .execute(request)
        .await
        .expect_err("429 响应应返回错误");

    assert!(
        matches!(error, ExecutionError::RateLimited { .. }),
        "429 应归类为 RateLimited，实际: {error:?}"
    );
    assert!(error.is_retryable(), "RateLimited 应允许统一重试");
}
