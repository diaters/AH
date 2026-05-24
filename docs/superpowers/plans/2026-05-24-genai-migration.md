# genai Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace async-openai with genai 0.6 to support OpenAI, Anthropic, DeepSeek (reasoning + tool calls), and OpenAI-compatible providers through a unified multi-provider client.

**Architecture:** Keep the existing `AgentExecutor` trait and domain types unchanged. Create `GenaiExecutor` implementing `AgentExecutor` with internal bidirectional conversion between domain types and genai types. Extend `LlmProviderKind` with new provider variants. Use genai's auto-auth for standard providers and `ServiceTargetResolver` for OpenAI-compatible endpoints.

**Tech Stack:** Rust, genai 0.6, Bevy ECS, serde_json

---

## File Structure

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `Cargo.toml` | Replace `async-openai` with `genai = "0.6"` |
| Modify | `src/llm/provider.rs` | Extend `LlmProviderKind`, update `from_env` and `validate` |
| Create | `src/llm/genai.rs` | `GenaiExecutor` + conversion functions + error mapping |
| Delete | `src/llm/openai.rs` | Remove old `OpenAiExecutor` |
| Modify | `src/llm/factory.rs` | Create `GenaiExecutor` for all provider kinds |
| Modify | `src/llm/mod.rs` | `mod openai` → `mod genai` |

---

### Task 1: Update Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Replace async-openai with genai**

In `Cargo.toml`, remove the `async-openai` line and add `genai`:

```toml
# Remove this line:
# async-openai = { version = "0.38.0", features = ["chat-completion"] }

# Add this line:
genai = "0.6"
```

- [ ] **Step 2: Verify cargo resolves dependencies**

Run: `cargo check 2>&1 | head -20`

Expected: Compilation errors in `src/llm/openai.rs` and `src/llm/factory.rs` (unresolved import `async_openai`). Other files should be unaffected.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: replace async-openai with genai 0.6 in Cargo.toml"
```

---

### Task 2: Extend LlmProviderKind and update provider.rs

**Files:**
- Modify: `src/llm/provider.rs`

- [ ] **Step 1: Add new provider variants to LlmProviderKind**

Replace the `LlmProviderKind` enum in `src/llm/provider.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LlmProviderKind {
    OpenAi,
    Anthropic,
    DeepSeek,
    OpenAiCompatible,
}
```

- [ ] **Step 2: Update LlmProviderKind::parse to handle new variants**

Replace the `parse` method:

```rust
impl LlmProviderKind {
    /// 从环境变量中的 provider 字符串解析 provider 类型。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "deepseek" => Ok(Self::DeepSeek),
            "openai-compatible" | "openai_compatible" | "compatible" => Ok(Self::OpenAiCompatible),
            other => bail!(
                "unsupported HARNESS_LLM_PROVIDER value: {other}; expected openai, anthropic, deepseek, or openai-compatible"
            ),
        }
    }
}
```

- [ ] **Step 3: Remove org_id and project_id from LlmProviderConfig**

These fields are OpenAI-specific and not used by genai's auto-auth. Replace the struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmProviderConfig {
    pub provider: LlmProviderKind,
    pub model: String,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
}
```

Note: `api_key` is now `Option<String>` — for standard providers (OpenAI/Anthropic/DeepSeek), genai reads API keys from standard environment variables automatically, so explicit `api_key` is only needed for `openai-compatible`.

- [ ] **Step 4: Update from_env to support new providers**

Replace the `from_env` method:

```rust
impl LlmProviderConfig {
    /// 从环境变量加载 provider、模型和连接信息。
    pub fn from_env(default_model: &str) -> Result<Self> {
        let provider_raw =
            env::var("HARNESS_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
        let provider = LlmProviderKind::parse(&provider_raw)?;
        let model = env::var("HARNESS_MODEL").unwrap_or_else(|_| default_model.to_string());
        let api_key = read_first_env(&["HARNESS_LLM_API_KEY", "OPENAI_API_KEY"]);
        let api_base = read_first_env(&["HARNESS_LLM_API_BASE", "OPENAI_BASE_URL"]);

        let config = Self {
            provider,
            model,
            api_key,
            api_base,
        };

        config.validate()?;
        Ok(config)
    }
```

- [ ] **Step 5: Update validate for new config structure**

Replace the `validate` method:

```rust
    /// 校验 provider 配置是否满足启动条件。
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty() {
            bail!("HARNESS_MODEL must not be empty");
        }

        if matches!(self.provider, LlmProviderKind::OpenAiCompatible) {
            if self
                .api_base
                .as_deref()
                .is_none_or(|api_base| api_base.trim().is_empty())
            {
                bail!("HARNESS_LLM_API_BASE is required when HARNESS_LLM_PROVIDER=openai-compatible");
            }
            if self
                .api_key
                .as_deref()
                .is_none_or(|api_key| api_key.trim().is_empty())
            {
                bail!("HARNESS_LLM_API_KEY is required when HARNESS_LLM_PROVIDER=openai-compatible");
            }
        }

        Ok(())
    }
}
```

- [ ] **Step 6: Update unit tests**

Replace the test module in `src/llm/provider.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{LlmProviderConfig, LlmProviderKind};

    /// 验证 provider 字符串可以被稳定解析为内部枚举。
    #[test]
    fn parses_provider_aliases() {
        assert_eq!(
            LlmProviderKind::parse("openai").expect("openai should parse"),
            LlmProviderKind::OpenAi
        );
        assert_eq!(
            LlmProviderKind::parse("anthropic").expect("anthropic should parse"),
            LlmProviderKind::Anthropic
        );
        assert_eq!(
            LlmProviderKind::parse("claude").expect("claude should parse"),
            LlmProviderKind::Anthropic
        );
        assert_eq!(
            LlmProviderKind::parse("deepseek").expect("deepseek should parse"),
            LlmProviderKind::DeepSeek
        );
        assert_eq!(
            LlmProviderKind::parse("openai-compatible").expect("openai-compatible should parse"),
            LlmProviderKind::OpenAiCompatible
        );
        assert_eq!(
            LlmProviderKind::parse("compatible").expect("compatible should parse"),
            LlmProviderKind::OpenAiCompatible
        );
    }

    /// 验证不支持的 provider 字符串会被拒绝。
    #[test]
    fn rejects_unknown_provider() {
        let err = LlmProviderKind::parse("unknown").unwrap_err();
        assert!(err.to_string().contains("unsupported HARNESS_LLM_PROVIDER"));
    }

    /// 验证 OpenAI 兼容 provider 缺失 base URL 时会被拒绝。
    #[test]
    fn rejects_compatible_provider_without_api_base() {
        let config = LlmProviderConfig {
            provider: LlmProviderKind::OpenAiCompatible,
            model: "test-model".to_string(),
            api_key: Some("test-key".to_string()),
            api_base: None,
        };

        let error = config
            .validate()
            .expect_err("compatible provider without api base should fail");

        assert!(
            error
                .to_string()
                .contains("HARNESS_LLM_API_BASE is required"),
            "unexpected error: {error}"
        );
    }

    /// 验证 OpenAI 兼容 provider 缺失 api key 时会被拒绝。
    #[test]
    fn rejects_compatible_provider_without_api_key() {
        let config = LlmProviderConfig {
            provider: LlmProviderKind::OpenAiCompatible,
            model: "test-model".to_string(),
            api_key: None,
            api_base: Some("https://example.com/v1".to_string()),
        };

        let error = config
            .validate()
            .expect_err("compatible provider without api key should fail");

        assert!(
            error
                .to_string()
                .contains("HARNESS_LLM_API_KEY is required"),
            "unexpected error: {error}"
        );
    }

    /// 验证 OpenAI 兼容 provider 在完整配置下可以通过校验。
    #[test]
    fn accepts_compatible_provider_with_full_config() {
        let config = LlmProviderConfig {
            provider: LlmProviderKind::OpenAiCompatible,
            model: "test-model".to_string(),
            api_key: Some("test-key".to_string()),
            api_base: Some("https://example.com/v1".to_string()),
        };

        config
            .validate()
            .expect("compatible provider with full config should pass");
    }

    /// 验证标准 provider 无需显式 api_key 和 api_base 也能通过校验。
    #[test]
    fn accepts_standard_provider_without_explicit_config() {
        let config = LlmProviderConfig {
            provider: LlmProviderKind::OpenAi,
            model: "test-model".to_string(),
            api_key: None,
            api_base: None,
        };

        config
            .validate()
            .expect("standard provider without explicit config should pass");
    }
}
```

- [ ] **Step 7: Run tests to verify**

Run: `cargo test --lib llm::provider 2>&1`

Expected: All provider tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/llm/provider.rs
git commit -m "feat(llm): extend LlmProviderKind with Anthropic and DeepSeek variants"
```

---

### Task 3: Create GenaiExecutor

**Files:**
- Create: `src/llm/genai.rs`

- [ ] **Step 1: Write GenaiExecutor struct and constructor**

Create `src/llm/genai.rs` with the struct, constructor, and imports:

```rust
use anyhow::Result;
use genai::{
    Client,
    adapter::AdapterKind,
    chat::{
        ChatMessage, ChatRequest, ChatResponse, ContentPart, Tool, ToolCall, ToolResponse,
    },
    resolver::{AuthData, Endpoint, ServiceTargetResolver},
    ModelIden, ServiceTarget,
};
use tracing::debug;

use crate::domain::{
    AgentExecutionOutput, AgentExecutionRequest, AgentExecutor, ConversationMessage,
    ExecutionError, ExecutorFuture, LlmToolCall,
};

use super::provider::{LlmProviderConfig, LlmProviderKind};

pub(crate) struct GenaiExecutor {
    client: Client,
    model: String,
}

impl GenaiExecutor {
    pub(crate) fn new(config: &LlmProviderConfig) -> Result<Self> {
        debug!(model = %config.model, provider = ?config.provider, "creating genai executor");

        let client = match config.provider {
            LlmProviderKind::OpenAi
            | LlmProviderKind::Anthropic
            | LlmProviderKind::DeepSeek => {
                // genai 自动从标准环境变量读取 API key
                Client::default()
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

                let target_resolver =
                    ServiceTargetResolver::from_resolver_fn(move |service_target| {
                        let endpoint = Endpoint::from_base_url(&api_base)?;
                        let auth = AuthData::from_key(&api_key);
                        let model = ModelIden::new(
                            AdapterKind::OpenAI,
                            service_target.model.model_name,
                        );
                        Ok(ServiceTarget {
                            endpoint,
                            auth,
                            model,
                        })
                    });

                Client::builder()
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
```

- [ ] **Step 2: Write AgentExecutor impl for GenaiExecutor**

Append to `src/llm/genai.rs`:

```rust
impl AgentExecutor for GenaiExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let client = self.client.clone();
        let model = self.model.clone();

        Box::pin(async move {
            debug!(
                task_id = %request.task_id,
                agent_id = %request.agent_id,
                kind = ?request.request_kind,
                prompt_len = request.prompt.len(),
                has_system_prompt = request.system_prompt.is_some(),
                tools_count = request.tools.len(),
                has_conversation = request.conversation.is_some(),
                "sending request via genai"
            );

            let chat_request = build_chat_request(&request)?;

            let response = client
                .exec_chat(&model, chat_request, None)
                .await
                .map_err(|error| {
                    debug!(error = %error, "genai API error");
                    classify_genai_error(error)
                })?;

            parse_response(&request.task_id, response)
        })
    }
}
```

- [ ] **Step 3: Write build_chat_request function**

Append to `src/llm/genai.rs`:

```rust
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
```

- [ ] **Step 4: Write build_chat_messages function**

Append to `src/llm/genai.rs`:

```rust
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
                    // genai 的 ChatMessage::from(Vec<ToolCall>) 自动构建含 tool_calls 的 assistant 消息
                    let mut message = ChatMessage::from(genai_tool_calls);
                    if let Some(c) = content {
                        // 在 tool_calls 前插入文本内容
                        message.content.prepend(ContentPart::Text(c.clone()));
                    }
                    Ok(message)
                } else {
                    let content_str = content.as_deref().unwrap_or("");
                    Ok(ChatMessage::assistant(content_str))
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
```

- [ ] **Step 5: Write build_genai_tools function**

Append to `src/llm/genai.rs`:

```rust
fn build_genai_tools(tools: &[crate::domain::ToolDefinition]) -> Vec<Tool> {
    tools
        .iter()
        .map(|td| {
            let mut tool = Tool::new(td.name.as_str());
            if !td.description.is_empty() {
                tool = tool.with_description(td.description.as_str());
            }
            tool = tool.with_schema(td.parameters.schema.clone());
            tool
        })
        .collect()
}
```

- [ ] **Step 6: Write parse_response function**

Append to `src/llm/genai.rs`:

```rust
fn parse_response(
    task_id: &crate::domain::TaskId,
    response: ChatResponse,
) -> Result<AgentExecutionOutput, ExecutionError> {
    let tool_calls: Vec<&ToolCall> = response.content.tool_calls();

    if !tool_calls.is_empty() {
        let parsed_calls: Vec<LlmToolCall> = tool_calls
            .iter()
            .map(|tc| LlmToolCall {
                id: tc.call_id.clone(),
                name: tc.fn_name.clone(),
                arguments: tc.fn_arguments.to_string(),
            })
            .collect();

        debug!(
            task_id = %task_id,
            tool_call_count = parsed_calls.len(),
            tools = ?parsed_calls.iter().map(|c| &c.name).collect::<Vec<_>>(),
            "LLM requested tool calls"
        );
        return Ok(AgentExecutionOutput::ToolCalls(parsed_calls));
    }

    let content = response.first_text().map(|s| s.to_string());

    match &content {
        Some(c) => {
            debug!(task_id = %task_id, response_len = c.len(), "received genai response")
        }
        None => debug!(task_id = %task_id, "genai returned empty response"),
    }

    content
        .map(AgentExecutionOutput::Text)
        .ok_or(ExecutionError::EmptyResponse)
}
```

- [ ] **Step 7: Write classify_genai_error function**

Append to `src/llm/genai.rs`:

```rust
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
```

- [ ] **Step 8: Verify compilation**

Run: `cargo check 2>&1`

Note: At this point the file won't be wired into `mod.rs` yet, so compilation errors from `genai.rs` itself won't appear. We'll do a full check after Task 4.

- [ ] **Step 9: Commit**

```bash
git add src/llm/genai.rs
git commit -m "feat(llm): add GenaiExecutor with genai 0.6 integration"
```

---

### Task 4: Wire GenaiExecutor into module and factory

**Files:**
- Modify: `src/llm/mod.rs`
- Modify: `src/llm/factory.rs`
- Delete: `src/llm/openai.rs`

- [ ] **Step 1: Update mod.rs to use genai module**

Replace the content of `src/llm/mod.rs`:

```rust
mod brain_prompt;
mod factory;
mod genai;
mod provider;
mod summarization_prompt;

pub use brain_prompt::{brain_system_prompt, brain_user_prompt, parse_brain_decision};
pub use factory::create_executor_from_config;
pub use provider::{LlmProviderConfig, LlmProviderKind};
pub use summarization_prompt::{summarization_system_prompt, summarization_user_prompt};
```

- [ ] **Step 2: Update factory.rs to create GenaiExecutor**

Replace the content of `src/llm/factory.rs`:

```rust
use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

use crate::domain::AgentExecutor;

use super::{
    genai::GenaiExecutor,
    provider::{LlmProviderConfig, LlmProviderKind},
};

/// 基于 provider 配置创建可注入系统层的执行器实例。
pub fn create_executor_from_config(config: &LlmProviderConfig) -> Result<Arc<dyn AgentExecutor>> {
    match config.provider {
        LlmProviderKind::OpenAi
        | LlmProviderKind::Anthropic
        | LlmProviderKind::DeepSeek
        | LlmProviderKind::OpenAiCompatible => {
            debug!(provider = ?config.provider, model = %config.model, "creating executor from config");
            Ok(Arc::new(GenaiExecutor::new(config)?))
        }
    }
}
```

- [ ] **Step 3: Delete old openai.rs**

```bash
rm src/llm/openai.rs
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check 2>&1`

Expected: Clean compilation with no errors. All existing code referencing `create_executor_from_config`, `LlmProviderConfig`, `LlmProviderKind`, and `AgentExecutor` should still work.

- [ ] **Step 5: Run all tests**

Run: `cargo test 2>&1`

Expected: All unit tests pass. Integration tests using mock executors (not the real LLM) should pass unchanged.

- [ ] **Step 6: Commit**

```bash
git add src/llm/mod.rs src/llm/factory.rs
git rm src/llm/openai.rs
git commit -m "feat(llm): wire GenaiExecutor, remove OpenAiExecutor"
```

---

### Task 5: Fix compilation issues and run full test suite

**Files:**
- Modify: Any files that reference `LlmProviderConfig` fields changed in Task 2 (`org_id`, `project_id`, `api_key` type change)

- [ ] **Step 1: Find all references to removed/changed fields**

Run: `grep -rn "org_id\|project_id" /Users/diater/workspace/Harness/src/ 2>/dev/null`

Expected: No references remain (they were only in `provider.rs` and `openai.rs`).

- [ ] **Step 2: Find all references to `api_key` as a required String**

Run: `grep -rn "\.api_key" /Users/diater/workspace/Harness/src/ 2>/dev/null`

Check if any code assumes `api_key` is `String` rather than `Option<String>`. Fix any such references.

- [ ] **Step 3: Full compilation check**

Run: `cargo check 2>&1`

Expected: No errors.

- [ ] **Step 4: Run full test suite**

Run: `cargo test 2>&1`

Expected: All tests pass.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy 2>&1`

Expected: No warnings. Fix any warnings if they appear.

- [ ] **Step 6: Commit any fixes**

```bash
git add -A
git commit -m "fix: resolve compilation issues from genai migration"
```

(Only if there are fixes to commit. Skip if clean.)

---

### Task 6: Verify end-to-end with cargo test

**Files:**
- No file changes expected

- [ ] **Step 1: Run the full test suite with verbose output**

Run: `cargo test -- --nocapture 2>&1 | tail -30`

Expected: All tests pass. The test output shows provider tests and integration flow tests succeeding.

- [ ] **Step 2: Verify no async-openai references remain**

Run: `grep -rn "async.openai\|async_openai\|OpenAiExecutor" /Users/diater/workspace/Harness/src/ 2>/dev/null`

Expected: No output (no references remain).

- [ ] **Step 3: Verify genai is properly imported**

Run: `grep -rn "genai" /Users/diater/workspace/Harness/src/llm/ 2>/dev/null`

Expected: `genai.rs` and `factory.rs` contain `genai` references.

- [ ] **Step 4: Final commit (if any remaining changes)**

If there are uncommitted changes:

```bash
git add -A
git commit -m "chore: finalize genai migration"
```
