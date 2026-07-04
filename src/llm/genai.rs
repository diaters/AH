use anyhow::{Context, Result};
use genai::{
    Client, ModelIden, ServiceTarget,
    adapter::AdapterKind,
    chat::{ChatMessage, ChatRequest, ChatResponse, ContentPart, Tool, ToolCall, ToolResponse},
    resolver::{AuthData, Endpoint, ServiceTargetResolver},
};
use reqwest_013;
use tokio::time::Instant;
use tracing::debug;

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
        let model = self.model.clone();

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

            parse_response(&request.task_id, response)
        })
    }
}

fn build_chat_request(request: &AgentExecutionRequest) -> Result<ChatRequest, ExecutionError> {
    let mut chat_req = if let Some(conversation) = &request.conversation {
        let messages = build_chat_messages(conversation)?;
        ChatRequest::new(messages)
    } else {
        ChatRequest::new(vec![ChatMessage::user(&request.prompt)])
    };

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

        genai::Error::HttpError { status, .. } => {
            let message = error.to_string();
            match status.as_u16() {
                401 => ExecutionError::Authentication(message),
                429 => ExecutionError::RateLimited {
                    message,
                    retry_after_secs: Some(5),
                },
                402 | 403 => ExecutionError::QuotaExhausted(message),
                408 | 504 => ExecutionError::Timeout(message),
                _ => ExecutionError::Unknown(message),
            }
        }

        genai::Error::WebAdapterCall { .. }
        | genai::Error::WebModelCall { .. }
        | genai::Error::WebStream { .. } => ExecutionError::Transport(error.to_string()),

        genai::Error::NoChatResponse { .. } => ExecutionError::EmptyResponse,

        _ => ExecutionError::Unknown(error.to_string()),
    }
}
