use anyhow::{Context, Result};
use genai::{
    Client, ModelIden, ServiceTarget,
    adapter::AdapterKind,
    chat::{ChatMessage, ChatRequest, ChatResponse, ContentPart, Tool, ToolCall, ToolResponse},
    resolver::{AuthData, Endpoint, ServiceTargetResolver},
};
use reqwest_013;
use tokio::time::Instant;
use tracing::{debug, info};

use crate::domain::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ConversationMessage,
    ExecutionError, ExecutorFuture, LlmToolCall, OutputContent,
};

use super::provider::{LlmProviderConfig, LlmProviderKind};

pub(crate) struct GenaiExecutor {
    client: Client,
    model: String,
}

fn create_reqwest_client() -> Result<reqwest_013::Client> {
    let roots = webpki_roots::TLS_SERVER_ROOTS.iter().cloned();
    let root_store = rustls::RootCertStore::from_iter(roots);
    let rustls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    reqwest_013::Client::builder()
        .tls_backend_preconfigured(rustls_config)
        .build()
        .context("failed to build reqwest client with webpki roots")
}

impl GenaiExecutor {
    pub(crate) fn new(config: &LlmProviderConfig) -> Result<Self> {
        debug!(model = %config.model, provider = ?config.provider, "creating genai executor");

        let client = match config.provider {
            LlmProviderKind::OpenAi | LlmProviderKind::Anthropic | LlmProviderKind::DeepSeek => {
                Client::builder()
                    .with_reqwest(create_reqwest_client()?)
                    .build()
            }
            LlmProviderKind::OpenAiCompatible => {
                let api_base = config
                    .api_base
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("api_base is required for openai-compatible"))?
                    .to_string();
                let api_key = config
                    .api_key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("api_key is required for openai-compatible"))?
                    .to_string();

                let target_resolver = ServiceTargetResolver::from_resolver_fn(
                    move |service_target: ServiceTarget| {
                        let endpoint = Endpoint::from_owned(api_base.as_str());
                        let auth = AuthData::from_single(api_key.as_str());
                        let model =
                            ModelIden::new(AdapterKind::OpenAI, service_target.model.model_name);
                        Ok(ServiceTarget {
                            endpoint,
                            auth,
                            model,
                        })
                    },
                );

                Client::builder()
                    .with_reqwest(create_reqwest_client()?)
                    .with_service_target_resolver(target_resolver)
                    .build()
            }
        };

        Ok(Self {
            client,
            model: config.model.clone(),
        })
    }
}

impl AgentExecutor for GenaiExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let client = self.client.clone();
        // 支持 model_override 覆盖默认模型
        let model = request
            .model_override
            .as_ref()
            .unwrap_or(&self.model)
            .clone();

        Box::pin(async move {
            debug!(
                event = "LlmRequestStart",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                model = %model,
                kind = ?request.request_kind,
                prompt_len = request.prompt.len(),
                has_system_prompt = request.system_prompt.is_some(),
                tools_count = request.tools.len(),
                has_conversation = request.conversation.is_some(),
                "sending request via genai"
            );

            // info! — 审计摘要
            info!(
                event = "LlmRequestStarted",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                model = %model,
                tools_count = request.tools.len(),
                "LLM 请求开始：model={}, tools={}",
                model,
                request.tools.len()
            );

            let chat_request = build_chat_request(&request)?;
            let start = Instant::now();

            let response = client
                .exec_chat(&model, chat_request, None)
                .await
                .map_err(|error| {
                    debug!(error = %error, "genai API error");
                    classify_genai_error(error)
                })?;

            let duration_ms = start.elapsed().as_millis();
            debug!(
                event = "LlmRequestCompleted",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                model = %model,
                duration_ms = duration_ms,
                response_len = response.first_text().map(|c| c.len()).unwrap_or(0),
                "genai request completed"
            );

            info!(
                event = "LlmRequestCompleted",
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                model = %model,
                duration_ms = duration_ms,
                response_len = response.first_text().map(|c| c.len()).unwrap_or(0),
                "LLM 调用完成：{}ms，响应 {} 字符",
                duration_ms,
                response.first_text().map(|c| c.len()).unwrap_or(0)
            );

            parse_response(&request.task_id, response)
        })
    }
}

fn build_chat_request(request: &AgentExecutionRequest) -> Result<ChatRequest, ExecutionError> {
    // 组合模式：conversation（历史对话）+ prompt（当前用户消息）
    // 两者独立可选，拼接为最终消息列表。
    // 这避免了旧逻辑中 Some(vec![]) 吞掉 prompt 的 bug。
    let mut messages = Vec::new();

    // 追加历史对话（如果有）
    if let Some(conversation) = &request.conversation {
        messages.extend(build_chat_messages(conversation)?);
    }

    // 追加当前用户消息（如果有）
    if !request.prompt.is_empty() {
        messages.push(ChatMessage::user(&request.prompt));
    }

    let mut chat_req = ChatRequest::new(messages);

    if let Some(system_prompt) = &request.system_prompt {
        chat_req = chat_req.with_system(system_prompt.as_str());
    }

    if !request.tools.is_empty() {
        let tools = build_genai_tools(&request.tools);
        chat_req = chat_req.with_tools(tools);
    }

    Ok(chat_req)
}

fn build_chat_messages(
    conversation: &[ConversationMessage],
) -> Result<Vec<ChatMessage>, ExecutionError> {
    conversation
        .iter()
        .map(|msg| match msg {
            ConversationMessage::System { content } => Ok(ChatMessage::system(content.as_str())),
            ConversationMessage::User { content } => Ok(ChatMessage::user(content.as_str())),
            ConversationMessage::Assistant {
                content,
                tool_calls,
                reasoning_content,
            } => {
                if !tool_calls.is_empty() {
                    let genai_tool_calls: Vec<ToolCall> = tool_calls
                        .iter()
                        .map(|tc| ToolCall {
                            call_id: tc.id.clone(),
                            fn_name: tc.name.clone(),
                            fn_arguments: serde_json::from_str(&tc.arguments)
                                .unwrap_or(serde_json::Value::String(tc.arguments.clone())),
                            thought_signatures: None,
                        })
                        .collect();
                    let mut message = ChatMessage::from(genai_tool_calls);
                    if let Some(c) = content {
                        message.content.prepend(ContentPart::Text(c.clone()));
                    }
                    message = message.with_reasoning_content(reasoning_content.clone());
                    Ok(message)
                } else {
                    let content_str = content.as_deref().unwrap_or("");
                    let message = ChatMessage::assistant(content_str)
                        .with_reasoning_content(reasoning_content.clone());
                    Ok(message)
                }
            }
            ConversationMessage::Tool {
                tool_call_id,
                content,
            } => {
                let tool_response = ToolResponse::new(tool_call_id.as_str(), content.as_str());
                Ok(ChatMessage::tool(tool_response))
            }
        })
        .collect()
}

fn build_genai_tools(tools: &[crate::domain::ToolDefinition]) -> Vec<Tool> {
    tools
        .iter()
        .map(|td| {
            // OpenAI 兼容 API 要求 function.name 匹配 ^[a-zA-Z0-9_-]+$，
            // 插件命名空间使用冒号（如 harness-demo:echo），需替换为双下划线。
            let safe_name = sanitize_tool_name(&td.name);
            let mut tool = Tool::new(safe_name.as_str());
            if !td.description.is_empty() {
                tool = tool.with_description(td.description.as_str());
            }
            tool = tool.with_schema(td.parameters.schema.clone());
            tool
        })
        .collect()
}

/// 将工具名中的冒号替换为双下划线，以符合 OpenAI API 的 function.name 格式要求。
fn sanitize_tool_name(name: &str) -> String {
    name.replace(':', "__")
}

/// 将 LLM 返回的工具名还原为内部命名空间格式（双下划线 → 冒号）。
fn unsanitize_tool_name(name: &str) -> String {
    name.replace("__", ":")
}

fn parse_response(
    task_id: &crate::domain::TaskId,
    response: ChatResponse,
) -> Result<AgentExecutionOutput, ExecutionError> {
    let reasoning_content = response.reasoning_content.clone();
    let tool_calls: Vec<&ToolCall> = response.content.tool_calls();

    if !tool_calls.is_empty() {
        let parsed_calls: Vec<LlmToolCall> = tool_calls
            .iter()
            .map(|tc| LlmToolCall {
                id: tc.call_id.clone(),
                name: unsanitize_tool_name(&tc.fn_name),
                arguments: tc.fn_arguments.to_string(),
            })
            .collect();

        debug!(
            task_id = %task_id,
            tool_call_count = parsed_calls.len(),
            tools = ?parsed_calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            has_reasoning = reasoning_content.is_some(),
            "LLM requested tool calls"
        );

        info!(
            event = "LlmToolCallsRequested",
            task_id = %task_id,
            tool_names = ?parsed_calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "LLM 请求调用工具：{:?}",
            parsed_calls.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        return Ok(AgentExecutionOutput {
            content: OutputContent::ToolCalls(parsed_calls),
            reasoning_content,
        });
    }

    let content = response.first_text().map(|s| s.to_string());

    match &content {
        Some(c) => {
            debug!(task_id = %task_id, response_len = c.len(), has_reasoning = reasoning_content.is_some(), "received genai response")
        }
        None => debug!(task_id = %task_id, "genai returned empty response"),
    }

    content
        .map(|c| AgentExecutionOutput {
            content: OutputContent::Text(c),
            reasoning_content,
        })
        .ok_or(ExecutionError::EmptyResponse)
}

fn classify_genai_error(error: genai::Error) -> ExecutionError {
    match &error {
        genai::Error::RequiresApiKey { .. }
        | genai::Error::NoAuthResolver { .. }
        | genai::Error::NoAuthData { .. } => ExecutionError::Authentication(error.to_string()),

        genai::Error::HttpError { status, .. } => classify_http_status(*status, error.to_string()),

        // 非流式 exec_chat 的 HTTP 错误路径：状态码包装在 webc::Error::ResponseFailedStatus 中。
        // 不提取状态码会导致 401/403 被误判为 Transport（可重试）、429 丢失降级资格。
        genai::Error::WebAdapterCall { webc_error, .. }
        | genai::Error::WebModelCall { webc_error, .. } => match webc_error {
            genai::webc::Error::ResponseFailedStatus { status, .. } => {
                classify_http_status(*status, error.to_string())
            }
            _ => ExecutionError::Transport(error.to_string()),
        },

        genai::Error::WebStream { .. } => ExecutionError::Transport(error.to_string()),

        genai::Error::NoChatResponse { .. } => ExecutionError::EmptyResponse,

        _ => ExecutionError::Unknown(error.to_string()),
    }
}

/// 将 HTTP 状态码映射为稳定的 ExecutionError 分类。
fn classify_http_status(status: reqwest_013::StatusCode, message: String) -> ExecutionError {
    match status.as_u16() {
        401 => ExecutionError::Authentication(message), // 不降级
        402 => ExecutionError::QuotaExhausted(message), // 降级
        403 => ExecutionError::Authentication(message), // 不降级
        429 => ExecutionError::RateLimited {
            message,
            retry_after_secs: Some(5),
        },
        408 | 504 => ExecutionError::Timeout(message),
        _ => ExecutionError::Unknown(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ConversationMessage, LlmToolCall, ToolDefinition, ToolExecutorKind, ToolPermission,
        ToolSchema,
    };
    use genai::chat::{ChatRole, MessageContent, Usage};

    fn sample_tool(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: ToolSchema {
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    },
                    "required": ["command"]
                }),
            },
            default_permission: ToolPermission::Allow,
            executor: ToolExecutorKind::Builtin(name.to_string()),
            required_tag: None,
        }
    }

    fn sample_request() -> AgentExecutionRequest {
        AgentExecutionRequest {
            task_id: uuid::Uuid::new_v4(),
            agent_id: uuid::Uuid::new_v4(),
            request_kind: crate::domain::AgentRequestKind::LlmCompletion,
            prompt: "hello".to_string(),
            system_prompt: None,
            tools: vec![],
            conversation: None,
            work_item_id: None,
            model_override: None,
        }
    }

    fn make_response(content: MessageContent, reasoning: Option<String>) -> ChatResponse {
        ChatResponse {
            content,
            reasoning_content: reasoning,
            model_iden: ModelIden::new(AdapterKind::OpenAI, "test-model"),
            provider_model_iden: ModelIden::new(AdapterKind::OpenAI, "test-model"),
            stop_reason: None,
            usage: Usage::default(),
            captured_raw_body: None,
            response_id: None,
        }
    }

    // ---- 工具名 sanitize ----

    #[test]
    fn sanitize_tool_name_replaces_colons() {
        assert_eq!(sanitize_tool_name("shell:exec"), "shell__exec");
        assert_eq!(sanitize_tool_name("a:b:c"), "a__b__c");
    }

    #[test]
    fn sanitize_keeps_plain_names() {
        assert_eq!(sanitize_tool_name("shell_exec"), "shell_exec");
        assert_eq!(sanitize_tool_name("shell"), "shell");
    }

    #[test]
    fn sanitize_unsanitize_roundtrip() {
        let original = "plugin:tool_name";
        let roundtrip = unsanitize_tool_name(&sanitize_tool_name(original));
        assert_eq!(roundtrip, original);
    }

    // ---- build_genai_tools ----

    #[test]
    fn build_genai_tools_sanitizes_names() {
        let tools = build_genai_tools(&[sample_tool("shell:exec", "run a command")]);
        assert_eq!(tools.len(), 1);
        assert!(matches!(
            &tools[0].name,
            genai::chat::ToolName::Custom(n) if n == "shell__exec"
        ));
    }

    #[test]
    fn build_genai_tools_sets_description_and_schema() {
        let tools = build_genai_tools(&[sample_tool("shell_exec", "run a command")]);
        assert_eq!(
            tools[0].description.as_deref(),
            Some("run a command"),
            "非空描述应传递给 LLM"
        );
        assert_eq!(
            tools[0].schema,
            Some(sample_tool("shell_exec", "").parameters.schema)
        );
    }

    #[test]
    fn build_genai_tools_empty_description_is_omitted() {
        let tools = build_genai_tools(&[sample_tool("shell_exec", "")]);
        assert_eq!(tools[0].description, None);
    }

    // ---- build_chat_request ----

    #[test]
    fn build_chat_request_prompt_only() {
        let request = sample_request();
        let chat_req = build_chat_request(&request).unwrap();
        assert_eq!(chat_req.messages.len(), 1);
        assert_eq!(chat_req.messages[0].role, ChatRole::User);
        assert_eq!(chat_req.messages[0].content.first_text(), Some("hello"));
    }

    #[test]
    fn build_chat_request_empty_prompt_without_conversation_yields_no_messages() {
        // 组合模式：prompt 与 conversation 独立可选，空 prompt 不产生空 user 消息
        let mut request = sample_request();
        request.prompt = String::new();
        let chat_req = build_chat_request(&request).unwrap();
        assert!(chat_req.messages.is_empty());
    }

    #[test]
    fn build_chat_request_appends_prompt_after_conversation() {
        let mut request = sample_request();
        request.conversation = Some(vec![ConversationMessage::User {
            content: "earlier".to_string(),
        }]);
        let chat_req = build_chat_request(&request).unwrap();
        assert_eq!(chat_req.messages.len(), 2);
        assert_eq!(chat_req.messages[0].content.first_text(), Some("earlier"));
        assert_eq!(chat_req.messages[1].content.first_text(), Some("hello"));
    }

    #[test]
    fn build_chat_request_sets_system_prompt() {
        let mut request = sample_request();
        request.system_prompt = Some("you are a helper".to_string());
        let chat_req = build_chat_request(&request).unwrap();
        assert_eq!(chat_req.system.as_deref(), Some("you are a helper"));
    }

    #[test]
    fn build_chat_request_attaches_tools() {
        let mut request = sample_request();
        request.tools = vec![sample_tool("shell:exec", "run a command")];
        let chat_req = build_chat_request(&request).unwrap();
        let tools = chat_req.tools.expect("tools should be attached");
        assert_eq!(tools.len(), 1);
    }

    #[test]
    fn build_chat_request_without_tools_has_no_tools() {
        let request = sample_request();
        let chat_req = build_chat_request(&request).unwrap();
        assert!(chat_req.tools.is_none());
    }

    // ---- build_chat_messages ----

    #[test]
    fn build_chat_messages_maps_roles() {
        let conversation = vec![
            ConversationMessage::System {
                content: "sys".to_string(),
            },
            ConversationMessage::User {
                content: "hi".to_string(),
            },
            ConversationMessage::Assistant {
                content: Some("hello".to_string()),
                tool_calls: vec![],
                reasoning_content: None,
            },
        ];
        let messages = build_chat_messages(&conversation).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, ChatRole::System);
        assert_eq!(messages[1].role, ChatRole::User);
        assert_eq!(messages[2].role, ChatRole::Assistant);
        assert_eq!(messages[2].content.first_text(), Some("hello"));
    }

    #[test]
    fn build_chat_messages_assistant_tool_calls_parses_json_arguments() {
        let conversation = vec![ConversationMessage::Assistant {
            content: Some("I will run ls".to_string()),
            tool_calls: vec![LlmToolCall {
                id: "call_1".to_string(),
                name: "shell:exec".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            }],
            reasoning_content: Some("thinking".to_string()),
        }];
        let messages = build_chat_messages(&conversation).unwrap();
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.role, ChatRole::Assistant);
        // 工具调用名称保持内部命名空间格式（sanitize 仅在 build_genai_tools 中进行）
        let tool_calls = message.content.tool_calls();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].fn_name, "shell:exec");
        // arguments 从 JSON 字符串解析为 Value
        assert_eq!(
            tool_calls[0].fn_arguments,
            serde_json::json!({"command": "ls"})
        );
        // 文本内容与 reasoning 均保留（reasoning 以 ContentPart 形式存在）
        assert_eq!(message.content.first_text(), Some("I will run ls"));
        assert_eq!(message.content.first_reasoning_content(), Some("thinking"));
    }

    #[test]
    fn build_chat_messages_assistant_invalid_json_arguments_falls_back_to_string() {
        let conversation = vec![ConversationMessage::Assistant {
            content: None,
            tool_calls: vec![LlmToolCall {
                id: "call_1".to_string(),
                name: "shell:exec".to_string(),
                arguments: "not json".to_string(),
            }],
            reasoning_content: None,
        }];
        let messages = build_chat_messages(&conversation).unwrap();
        let tool_calls = messages[0].content.tool_calls();
        assert_eq!(
            tool_calls[0].fn_arguments,
            serde_json::Value::String("not json".to_string())
        );
    }

    #[test]
    fn build_chat_messages_tool_response_maps_to_tool_role() {
        let conversation = vec![ConversationMessage::Tool {
            tool_call_id: "call_1".to_string(),
            content: "ls output".to_string(),
        }];
        let messages = build_chat_messages(&conversation).unwrap();
        assert_eq!(messages[0].role, ChatRole::Tool);
        // 内容以 ToolResponse part 承载
        let tool_responses = messages[0].content.tool_responses();
        assert_eq!(tool_responses.len(), 1);
        assert_eq!(tool_responses[0].call_id, "call_1");
        assert_eq!(tool_responses[0].content, "ls output");
    }

    // ---- parse_response ----

    #[test]
    fn parse_response_returns_text_output() {
        let response = make_response(ChatMessage::assistant("answer").content, None);
        let output = parse_response(&uuid::Uuid::new_v4(), response).unwrap();
        assert_eq!(output.content, OutputContent::Text("answer".to_string()));
        assert!(output.reasoning_content.is_none());
    }

    #[test]
    fn parse_response_preserves_reasoning_content() {
        let response = make_response(
            ChatMessage::assistant("answer").content,
            Some("chain of thought".to_string()),
        );
        let output = parse_response(&uuid::Uuid::new_v4(), response).unwrap();
        assert_eq!(
            output.reasoning_content.as_deref(),
            Some("chain of thought")
        );
    }

    #[test]
    fn parse_response_empty_returns_empty_response_error() {
        let response = make_response(MessageContent::default(), None);
        let result = parse_response(&uuid::Uuid::new_v4(), response);
        assert!(matches!(result, Err(ExecutionError::EmptyResponse)));
    }

    #[test]
    fn parse_response_tool_calls_unsanitizes_names() {
        // LLM 返回的是 sanitize 后的工具名（双下划线），解析时应还原为命名空间格式
        let tool_call = ToolCall {
            call_id: "call_1".to_string(),
            fn_name: "shell__exec".to_string(),
            fn_arguments: serde_json::json!({"command": "ls"}),
            thought_signatures: None,
        };
        let response = make_response(MessageContent::from(vec![tool_call]), None);
        let output = parse_response(&uuid::Uuid::new_v4(), response).unwrap();
        match output.content {
            OutputContent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "shell:exec");
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].arguments, r#"{"command":"ls"}"#);
            }
            other => panic!("expected ToolCalls, got {:?}", other),
        }
    }

    #[test]
    fn classify_403_as_authentication() {
        // 403 应映射为 Authentication，不触发降级
        let error = genai::Error::HttpError {
            status: reqwest_013::StatusCode::FORBIDDEN,
            canonical_reason: "Forbidden".to_string(),
            body: String::new(),
        };
        let classified = classify_genai_error(error);
        assert!(matches!(classified, ExecutionError::Authentication(_)));
    }

    #[test]
    fn classify_402_as_quota_exhausted() {
        // 402 应映射为 QuotaExhausted，触发降级
        let error = genai::Error::HttpError {
            status: reqwest_013::StatusCode::PAYMENT_REQUIRED,
            canonical_reason: "Payment Required".to_string(),
            body: String::new(),
        };
        let classified = classify_genai_error(error);
        assert!(matches!(classified, ExecutionError::QuotaExhausted(_)));
        assert!(classified.is_fallback_eligible());
    }

    /// 构造非流式 exec_chat 路径的 WebModelCall 错误（状态码包装在 webc::Error 中）。
    fn web_model_call_error(status: reqwest_013::StatusCode) -> genai::Error {
        genai::Error::WebModelCall {
            model_iden: ModelIden::new(AdapterKind::OpenAI, "test-model"),
            webc_error: genai::webc::Error::ResponseFailedStatus {
                status,
                body: String::new(),
                headers: Box::default(),
            },
        }
    }

    #[test]
    fn classify_web_model_call_401_as_authentication() {
        // 非流式路径 401 应提取状态码归类为 Authentication，而非 Transport
        let error = web_model_call_error(reqwest_013::StatusCode::UNAUTHORIZED);
        let classified = classify_genai_error(error);
        assert!(matches!(classified, ExecutionError::Authentication(_)));
        assert!(!classified.is_retryable());
        assert!(!classified.is_fallback_eligible());
    }

    #[test]
    fn classify_web_model_call_429_as_rate_limited() {
        // 非流式路径 429 应提取状态码归类为 RateLimited，保留降级资格
        let error = web_model_call_error(reqwest_013::StatusCode::TOO_MANY_REQUESTS);
        let classified = classify_genai_error(error);
        assert!(matches!(
            classified,
            ExecutionError::RateLimited {
                retry_after_secs: Some(_),
                ..
            }
        ));
        assert!(classified.is_retryable());
        assert!(classified.is_fallback_eligible());
    }

    #[test]
    fn classify_web_model_call_500_as_unknown() {
        let error = web_model_call_error(reqwest_013::StatusCode::INTERNAL_SERVER_ERROR);
        let classified = classify_genai_error(error);
        assert!(matches!(classified, ExecutionError::Unknown(_)));
    }
}
