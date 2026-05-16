use std::sync::Arc;

use anyhow::Result;
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
        CreateChatCompletionRequestArgs,
    },
};

use crate::domain::{AgentExecutionRequest, AgentExecutor, ExecutionError, ExecutorFuture};

use super::provider::LlmProviderConfig;

#[derive(Clone)]
pub(crate) struct OpenAiExecutor {
    client: Arc<Client<OpenAIConfig>>,
    model: String,
}

impl OpenAiExecutor {
    /// 根据 provider 配置构造 OpenAI 或 OpenAI 兼容执行器。
    pub(crate) fn new(config: &LlmProviderConfig) -> Result<Self> {
        let mut client_config = OpenAIConfig::new().with_api_key(config.api_key.clone());

        if let Some(api_base) = config
            .api_base
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            client_config = client_config.with_api_base(api_base.clone());
        }

        if let Some(org_id) = config
            .org_id
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            client_config = client_config.with_org_id(org_id.clone());
        }

        if let Some(project_id) = config
            .project_id
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            client_config = client_config.with_project_id(project_id.clone());
        }

        Ok(Self {
            client: Arc::new(Client::with_config(client_config)),
            model: config.model.clone(),
        })
    }
}

impl AgentExecutor for OpenAiExecutor {
    /// 使用 OpenAI Chat Completions 执行一次请求。
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let client = Arc::clone(&self.client);
        let model = self.model.clone();

        Box::pin(async move {
            let mut messages = Vec::new();

            if let Some(system_prompt) = &request.system_prompt {
                let system_message = ChatCompletionRequestSystemMessageArgs::default()
                    .content(system_prompt.as_str())
                    .build()
                    .map_err(|error| ExecutionError::Unknown(error.to_string()))?;
                messages.push(system_message.into());
            }

            let user_message = ChatCompletionRequestUserMessageArgs::default()
                .content(request.prompt)
                .build()
                .map_err(|error| ExecutionError::Unknown(error.to_string()))?;
            messages.push(user_message.into());

            let completion_request = CreateChatCompletionRequestArgs::default()
                .model(model)
                .messages(messages)
                .build()
                .map_err(|error| ExecutionError::Unknown(error.to_string()))?;

            let response = client
                .chat()
                .create(completion_request)
                .await
                .map_err(classify_openai_error)?;

            response
                .choices
                .first()
                .and_then(|choice| choice.message.content.clone())
                .filter(|content| !content.trim().is_empty())
                .ok_or(ExecutionError::EmptyResponse)
        })
    }
}

/// 将 OpenAI SDK 错误转换为框架内部统一错误。
fn classify_openai_error(error: async_openai::error::OpenAIError) -> ExecutionError {
    let message = error.to_string();
    let lowered = message.to_lowercase();

    if lowered.contains("timeout") {
        ExecutionError::Timeout(message)
    } else if lowered.contains("rate limit") || lowered.contains("429") {
        ExecutionError::RateLimited {
            message,
            retry_after_secs: Some(5),
        }
    } else if lowered.contains("invalid_api_key")
        || lowered.contains("invalid api key")
        || lowered.contains("authentication")
        || lowered.contains("401")
    {
        ExecutionError::Authentication(message)
    } else if lowered.contains("quota") || lowered.contains("insufficient_quota") {
        ExecutionError::QuotaExhausted(message)
    } else if lowered.contains("cancel") {
        ExecutionError::UserCancelled(message)
    } else if lowered.contains("connect")
        || lowered.contains("transport")
        || lowered.contains("network")
    {
        ExecutionError::Transport(message)
    } else {
        ExecutionError::Unknown(message)
    }
}
