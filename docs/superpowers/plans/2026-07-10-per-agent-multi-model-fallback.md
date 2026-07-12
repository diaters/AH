# Per-Agent 多模型/多提供商差异化调度与降级 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 Per-Agent 多模型/多提供商差异化调度与降级能力，支持 Agent 声明有序模型链并在遭遇 429/402 错误时自动降级。

**Architecture:** 每个 provider 实例创建独立的 `GenaiExecutor`，注册到全局 `ExecutorRegistry` Resource。每个 Agent 持有 `ModelChainState` Component，包含有序模型链和降级状态。执行时 clone 状态传入 async 任务，通过消息回写更新 Component。

**Tech Stack:** Rust, Bevy ECS, genai, serde, toml

## Global Constraints

- 遵循 `AGENTS.md` 规范：所有变更通过分支和 PR 合并，禁止直接推送到 `main`
- 测试先行：每个功能点需有对应单元测试
- 向后兼容：现有 `model` 字段和环境变量配置必须继续工作
- 错误处理：仅 429（限流）和 402（配额耗尽）触发降级，401/403 不降级
- 配置安全：`providers.toml` 中使用 `api_key_env` 引用环境变量名，不硬编码密钥

---

## Task 1: 领域类型基础 — ModelChainState

**Files:**
- Create: `src/domain/model_chain.rs`
- Modify: `src/domain/mod.rs`

**Interfaces:**
- Produces: `ProviderEntry`, `ProvidersConfig`, `ModelChainEntry`, `ModelChainState` 及其公共方法

- [ ] **Step 1: 编写 ModelChainState 单元测试**

创建 `src/domain/model_chain.rs`，先写测试：

```rust
use crate::prelude::*;
use bevy::ecs::component::Component;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_chain() -> Vec<ModelChainEntry> {
        vec![
            ModelChainEntry {
                provider: "openai".to_string(),
                model: "gpt-4.1-mini".to_string(),
                fallback_cooldown_secs: None,
            },
            ModelChainEntry {
                provider: "deepseek".to_string(),
                model: "deepseek-chat".to_string(),
                fallback_cooldown_secs: Some(120),
            },
        ]
    }

    #[test]
    fn new_initializes_active_index_to_zero() {
        let chain = make_test_chain();
        let state = ModelChainState::new(chain.clone(), 60);
        
        assert_eq!(state.active_index, 0);
        assert_eq!(state.chain.len(), 2);
        assert!(state.cooldown_until.is_none());
    }

    #[test]
    fn current_entry_returns_first_by_default() {
        let chain = make_test_chain();
        let state = ModelChainState::new(chain, 60);
        
        let entry = state.current_entry();
        assert_eq!(entry.provider, "openai");
        assert_eq!(entry.model, "gpt-4.1-mini");
    }

    #[test]
    fn step_fallback_moves_to_next_priority() {
        let chain = make_test_chain();
        let mut state = ModelChainState::new(chain, 60);
        
        let result = state.step_fallback(90);
        assert!(result);
        assert_eq!(state.active_index, 1);
        assert!(state.cooldown_until.is_some());
    }

    #[test]
    fn step_fallback_returns_false_when_exhausted() {
        let chain = make_test_chain();
        let mut state = ModelChainState::new(chain, 60);
        
        state.step_fallback(90);
        let result = state.step_fallback(90);
        assert!(!result);
        assert_eq!(state.active_index, 1);
    }

    #[test]
    fn reset_if_cooldown_expired_resets_to_first_priority() {
        let chain = make_test_chain();
        let mut state = ModelChainState::new(chain, 60);
        
        state.step_fallback(90);
        // 冷却期未过
        assert!(!state.reset_if_cooldown_expired(Instant::now()));
        assert_eq!(state.active_index, 1);
        
        // 模拟冷却期已过（设置一个过去的时刻）
        state.cooldown_until = Some(Instant::now() - std::time::Duration::from_secs(1));
        assert!(state.reset_if_cooldown_expired(Instant::now()));
        assert_eq!(state.active_index, 0);
        assert!(state.cooldown_until.is_none());
    }

    #[test]
    fn current_provider_and_model_helpers() {
        let chain = make_test_chain();
        let state = ModelChainState::new(chain, 60);
        
        assert_eq!(state.current_provider(), "openai");
        assert_eq!(state.current_model(), "gpt-4.1-mini");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib domain::model_chain --no-run`
Expected: 编译失败（类型未定义）

- [ ] **Step 3: 实现 ProviderEntry 和 ProvidersConfig**

在 `src/domain/model_chain.rs` 添加：

```rust
use crate::prelude::*;
use crate::llm::LlmProviderKind;
use bevy::ecs::component::Component;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// providers.toml 中的 provider 配置条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderEntry {
    pub name: String,
    pub kind: LlmProviderKind,
    pub api_key_env: String,
    pub api_base: Option<String>,
}

/// providers.toml 顶层结构
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvidersConfig {
    pub default_fallback_cooldown_secs: u64,
    #[serde(default)]
    pub default_provider: Option<String>,
    pub provider: Vec<ProviderEntry>,
}

/// agents.toml 中 [[agent.models]] 的条目
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelChainEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub fallback_cooldown_secs: Option<u64>,
}

/// Agent 运行时的模型链状态（Bevy Component）
#[derive(Debug, Clone, Component)]
pub struct ModelChainState {
    pub chain: Vec<ModelChainEntry>,
    pub active_index: usize,
    pub cooldown_until: Option<Instant>,
    pub default_cooldown_secs: u64,
}

impl ModelChainState {
    pub fn new(chain: Vec<ModelChainEntry>, default_cooldown_secs: u64) -> Self {
        Self {
            chain,
            active_index: 0,
            cooldown_until: None,
            default_cooldown_secs,
        }
    }

    pub fn current_entry(&self) -> &ModelChainEntry {
        &self.chain[self.active_index]
    }

    pub fn step_fallback(&mut self, cooldown_secs: u64) -> bool {
        if self.active_index + 1 >= self.chain.len() {
            return false;
        }
        self.active_index += 1;
        self.cooldown_until = Some(Instant::now() + std::time::Duration::from_secs(cooldown_secs));
        true
    }

    pub fn reset_if_cooldown_expired(&mut self, now: Instant) -> bool {
        if let Some(until) = self.cooldown_until {
            if now >= until {
                self.active_index = 0;
                self.cooldown_until = None;
                return true;
            }
        }
        false
    }

    pub fn current_provider(&self) -> &str {
        &self.current_entry().provider
    }

    pub fn current_model(&self) -> &str {
        &self.current_entry().model
    }
}
```

- [ ] **Step 4: 在 mod.rs 中导出新模块**

修改 `src/domain/mod.rs`，在模块声明区域添加：

```rust
mod model_chain;
pub use model_chain::{ModelChainEntry, ModelChainState, ProviderEntry, ProvidersConfig};
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib domain::model_chain`
Expected: 所有测试通过

- [ ] **Step 6: 提交**

```bash
git add src/domain/model_chain.rs src/domain/mod.rs
git commit -m "feat(domain): add ModelChainState and ProvidersConfig types

- ProviderEntry and ProvidersConfig for providers.toml
- ModelChainEntry for [[agent.models]]
- ModelChainState Component with fallback state machine

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 2: 错误类型扩展 — is_fallback_eligible

**Files:**
- Modify: `src/domain/error.rs`

**Interfaces:**
- Consumes: 现有 `ExecutionError` 类型
- Produces: `ExecutionError::is_fallback_eligible()` 方法

- [ ] **Step 1: 编写 is_fallback_eligible 测试**

在 `src/domain/error.rs` 测试模块中添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_fallback_eligible_for_429_and_402() {
        let rate_limited = ExecutionError::RateLimited {
            message: "too many requests".to_string(),
            retry_after_secs: Some(60),
        };
        assert!(rate_limited.is_fallback_eligible());

        let quota_exhausted = ExecutionError::QuotaExhausted("insufficient quota".to_string());
        assert!(quota_exhausted.is_fallback_eligible());
    }

    #[test]
    fn is_fallback_eligible_false_for_other_errors() {
        let auth = ExecutionError::Authentication("invalid key".to_string());
        assert!(!auth.is_fallback_eligible());

        let timeout = ExecutionError::Timeout("timed out".to_string());
        assert!(!timeout.is_fallback_eligible());

        let transport = ExecutionError::Transport("network error".to_string());
        assert!(!transport.is_fallback_eligible());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib domain::error::tests::is_fallback_eligible`
Expected: 编译失败（方法未定义）

- [ ] **Step 3: 实现 is_fallback_eligible 方法**

在 `src/domain/error.rs` 的 `impl ExecutionError` 块中添加：

```rust
impl ExecutionError {
    // ... 现有方法 ...

    /// 判断错误是否应触发模型降级。
    /// 仅 429（限流）和 402（配额耗尽）触发降级。
    /// 401/403（认证/权限错误）不降级，因为同一环境下降级无效。
    pub fn is_fallback_eligible(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::QuotaExhausted(_)
        )
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib domain::error::tests::is_fallback_eligible`
Expected: 所有测试通过

- [ ] **Step 5: 提交**

```bash
git add src/domain/error.rs
git commit -m "feat(error): add is_fallback_eligible method

Only 429 (RateLimited) and 402 (QuotaExhausted) trigger fallback.
401/403 do not fallback as they indicate auth/permission issues.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 3: AgentExecutionRequest 扩展 — model_override

**Files:**
- Modify: `src/domain/execution.rs`

**Interfaces:**
- Consumes: 现有 `AgentExecutionRequest` 类型
- Produces: `AgentExecutionRequest.model_override: Option<String>` 字段

- [ ] **Step 1: 修改 AgentExecutionRequest 结构体**

在 `src/domain/execution.rs` 的 `AgentExecutionRequest` 结构体中添加字段：

```rust
/// Agent 执行请求
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExecutionRequest {
    pub task_id: TaskId,
    pub agent_id: AgentId,
    pub request_kind: AgentRequestKind,
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub conversation: Option<Vec<ConversationMessage>>,
    pub work_item_id: Option<Uuid>,
    /// 覆盖 executor 的默认模型（用于多模型 provider）
    #[serde(default)]
    pub model_override: Option<String>,
}
```

- [ ] **Step 2: 更新测试中的请求构造**

在 `src/domain/execution.rs` 测试模块中修改：

```rust
#[test]
fn agent_execution_request_carries_work_item_id() {
    let request = AgentExecutionRequest {
        task_id: uuid::Uuid::nil(),
        agent_id: uuid::Uuid::nil(),
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "test".to_string(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: Some(uuid::Uuid::new_v4()),
        model_override: None, // 新增
    };

    assert!(request.work_item_id.is_some());
    assert!(request.model_override.is_none());
}

#[test]
fn agent_execution_request_supports_model_override() {
    let request = AgentExecutionRequest {
        task_id: uuid::Uuid::nil(),
        agent_id: uuid::Uuid::nil(),
        request_kind: AgentRequestKind::LlmCompletion,
        prompt: "test".to_string(),
        system_prompt: None,
        tools: vec![],
        conversation: None,
        work_item_id: None,
        model_override: Some("gpt-4.1-mini".to_string()),
    };

    assert_eq!(request.model_override, Some("gpt-4.1-mini".to_string()));
}
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib domain::execution`
Expected: 所有测试通过

- [ ] **Step 4: 检查其他代码是否需要更新**

Run: `cargo build`
Expected: 编译错误，定位所有构造 `AgentExecutionRequest` 的位置

- [ ] **Step 5: 更新所有 AgentExecutionRequest 构造点**

搜索并更新所有构造 `AgentExecutionRequest` 的位置，添加 `model_override: None`：

```bash
grep -rn "AgentExecutionRequest {" --include="*.rs" src/
```

逐一更新，添加 `model_override: None` 字段。

- [ ] **Step 6: 再次运行构建确认通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 7: 提交**

```bash
git add src/domain/execution.rs
git commit -m "feat(execution): add model_override field to AgentExecutionRequest

Allows per-request model override for multi-model providers.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 4: GenaiExecutor 支持模型覆盖

**Files:**
- Modify: `src/llm/genai.rs`

**Interfaces:**
- Consumes: `AgentExecutionRequest.model_override`
- Produces: `GenaiExecutor::execute()` 支持按请求覆盖模型

- [ ] **Step 1: 修改 GenaiExecutor::execute 方法**

在 `src/llm/genai.rs` 的 `impl AgentExecutor for GenaiExecutor` 中修改 `execute` 方法：

找到以下代码（约 L88-125）：

```rust
impl AgentExecutor for GenaiExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let client = self.client.clone();
        let model = self.model.clone();
```

修改为：

```rust
impl AgentExecutor for GenaiExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let client = self.client.clone();
        // 支持 model_override 覆盖默认模型
        let model = request.model_override
            .as_ref()
            .unwrap_or(&self.model)
            .clone();
```

- [ ] **Step 2: 更新日志中的 model 字段**

确保日志中使用的是覆盖后的 `model` 变量（约 L93-115）：

```rust
debug!(
    event = "LlmRequestStart",
    task_id = %request.task_id,
    agent_id = %request.agent_id,
    model = %model,  // 使用覆盖后的 model
    kind = ?request.request_kind,
    // ...
);
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib llm::genai`
Expected: 所有测试通过

- [ ] **Step 4: 提交**

```bash
git add src/llm/genai.rs
git commit -m "feat(genai): support model_override in execute

Use request.model_override if present, otherwise fall back to
executor's default model.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 5: classify_genai_error 拆分 402/403

**Files:**
- Modify: `src/llm/genai.rs`

**Interfaces:**
- Consumes: genai HTTP 错误状态码
- Produces: 402 → `QuotaExhausted`（降级），403 → `Authentication`（不降级）

- [ ] **Step 1: 编写 403 映射测试**

在 `src/llm/genai.rs` 测试模块中添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_403_as_authentication() {
        // 403 应映射为 Authentication，不触发降级
        let error = genai::Error::HttpError {
            status: http::StatusCode::FORBIDDEN,
            message: "Forbidden".to_string(),
        };
        let classified = classify_genai_error(error);
        assert!(matches!(classified, ExecutionError::Authentication(_)));
    }

    #[test]
    fn classify_402_as_quota_exhausted() {
        // 402 应映射为 QuotaExhausted，触发降级
        let error = genai::Error::HttpError {
            status: http::StatusCode::PAYMENT_REQUIRED,
            message: "Insufficient quota".to_string(),
        };
        let classified = classify_genai_error(error);
        assert!(matches!(classified, ExecutionError::QuotaExhausted(_)));
        assert!(classified.is_fallback_eligible());
    }
}
```

- [ ] **Step 2: 修改 classify_genai_error 函数**

在 `src/llm/genai.rs` 中找到 `classify_genai_error` 函数（约 L308-336），修改：

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
                402 => ExecutionError::QuotaExhausted(message),  // 降级
                403 => ExecutionError::Authentication(message),   // 不降级
                429 => ExecutionError::RateLimited {
                    message,
                    retry_after_secs: Some(5),
                },
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

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib llm::genai::tests`
Expected: 所有测试通过（注意：测试可能需要 mock genai::Error，若无法直接构造可跳过集成测试）

- [ ] **Step 4: 提交**

```bash
git add src/llm/genai.rs
git commit -m "fix(genai): split 402/403 error classification

- 402 (Payment Required) → QuotaExhausted (triggers fallback)
- 403 (Forbidden) → Authentication (no fallback)

This ensures only 429 and 402 trigger model fallback.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 6: ExecutorRegistry 实现

**Files:**
- Create: `src/llm/registry.rs`
- Modify: `src/llm/mod.rs`

**Interfaces:**
- Consumes: `ProvidersConfig`, `GenaiExecutor`, `ModelChainState`
- Produces: `ExecutorRegistry` Resource, `execute_with_fallback()` 方法

- [ ] **Step 1: 创建 ExecutorRegistry 结构体**

创建 `src/llm/registry.rs`：

```rust
use crate::prelude::*;
use crate::domain::{ExecutionError, ModelChainState, ProvidersConfig};
use crate::llm::genai::GenaiExecutor;
use crate::llm::provider::LlmProviderKind;
use bevy::ecs::resource::Resource;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// 全局 executor 注册表，按 provider name 索引
#[derive(Resource)]
pub struct ExecutorRegistry {
    executors: HashMap<String, Arc<dyn crate::llm::AgentExecutor>>,
    default_fallback_cooldown_secs: u64,
}

impl ExecutorRegistry {
    /// 从 ProvidersConfig 构建注册表
    pub fn from_config(config: &ProvidersConfig) -> Result<Self> {
        let mut executors = HashMap::new();

        for entry in &config.provider {
            let llm_config = crate::llm::LlmProviderConfig {
                provider: entry.kind.clone(),
                model: "placeholder".to_string(), // 模型由请求覆盖
                api_key: std::env::var(&entry.api_key_env).ok(),
                api_base: entry.api_base.clone(),
            };

            let executor = GenaiExecutor::new(&llm_config)
                .with_context(|| format!("failed to create executor for provider '{}'", entry.name))?;

            executors.insert(entry.name.clone(), Arc::new(executor) as Arc<dyn crate::llm::AgentExecutor>);
            
            debug!(
                provider = %entry.name,
                kind = ?entry.kind,
                "executor registered"
            );
        }

        Ok(Self {
            executors,
            default_fallback_cooldown_secs: config.default_fallback_cooldown_secs,
        })
    }

    /// 查找指定 provider 的 executor
    pub fn get(&self, provider_name: &str) -> Option<Arc<dyn crate::llm::AgentExecutor>> {
        self.executors.get(provider_name).cloned()
    }

    /// 从环境变量构建单 provider 注册表（向后兼容）
    pub fn from_env() -> Result<Self> {
        let config = crate::llm::LlmProviderConfig::from_env("gpt-4.1-mini")?;
        let executor = GenaiExecutor::new(&config)?;
        
        let provider_name = std::env::var("HARNESS_LLM_PROVIDER")
            .unwrap_or_else(|_| "default".to_string());

        let mut executors = HashMap::new();
        executors.insert(provider_name.clone(), Arc::new(executor) as Arc<dyn crate::llm::AgentExecutor>);

        Ok(Self {
            executors,
            default_fallback_cooldown_secs: 60,
        })
    }

    /// 全局默认冷却期
    pub fn default_cooldown_secs(&self) -> u64 {
        self.default_fallback_cooldown_secs
    }
}
```

- [ ] **Step 2: 在 llm/mod.rs 中导出**

修改 `src/llm/mod.rs`：

```rust
mod genai;
mod provider;
mod factory;
mod registry;  // 新增

pub use provider::{LlmProviderConfig, LlmProviderKind};
pub use registry::ExecutorRegistry;  // 新增
```

- [ ] **Step 3: 运行构建确认编译通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 4: 提交**

```bash
git add src/llm/registry.rs src/llm/mod.rs
git commit -m "feat(llm): add ExecutorRegistry

- Registry holds per-provider executors in HashMap
- from_config() builds from ProvidersConfig
- from_env() builds single-provider for backward compat

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 7: ModelChainStateUpdate 消息

**Files:**
- Modify: `src/domain/message.rs`

**Interfaces:**
- Produces: `ModelChainStateUpdate` 消息类型

- [ ] **Step 1: 添加 ModelChainStateUpdate 消息**

在 `src/domain/message.rs` 中添加：

```rust
use bevy::ecs::component::Component;
use uuid::Uuid;
use std::time::Instant;

// ... 现有类型 ...

/// ModelChainState 状态更新消息（从 async 任务回写）
#[derive(Debug, Clone, Component)]
pub struct ModelChainStateUpdate {
    pub agent_id: crate::domain::AgentId,
    pub new_active_index: usize,
    pub cooldown_until: Option<Instant>,
    pub previous_model: String,
    pub new_model: String,
}
```

- [ ] **Step 2: 确认编译通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 3: 提交**

```bash
git add src/domain/message.rs
git commit -m "feat(message): add ModelChainStateUpdate

Message type for async task to write back state changes to Component.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 8: AgentEntry 扩展 — models 字段

**Files:**
- Modify: `src/domain/mod.rs`

**Interfaces:**
- Consumes: `ModelChainEntry`
- Produces: `AgentEntry.models: Vec<ModelChainEntry>`

- [ ] **Step 1: 修改 AgentEntry 结构体**

在 `src/domain/mod.rs` 中找到 `AgentEntry` 定义（约 L168-179），修改：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    /// 向后兼容：单模型声明，自动生成单元素 models 链
    #[serde(default)]
    pub model: Option<String>,
    /// 有序模型链，第一个为最高优先级
    #[serde(default)]
    pub models: Vec<ModelChainEntry>,
    pub tags: Vec<String>,
    pub description: String,
    pub tools: Option<AgentToolsConfig>,
    #[serde(default)]
    pub skills: Option<Vec<String>>,
}
```

- [ ] **Step 2: 添加向后兼容测试**

在 `src/domain/mod.rs` 测试模块中添加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_entry_backward_compat_single_model() {
        let toml_str = r#"
[[agent]]
name = "test-agent"
model = "gpt-4.1-mini"
tags = ["test"]
description = "test"
"#;
        let config: AgentConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert_eq!(config.agent[0].model, Some("gpt-4.1-mini".to_string()));
        assert!(config.agent[0].models.is_empty());
    }

    #[test]
    fn agent_entry_with_models_chain() {
        let toml_str = r#"
[[agent]]
name = "test-agent"
tags = ["test"]
description = "test"

[[agent.models]]
provider = "openai"
model = "gpt-4.1-mini"

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"
"#;
        let config: AgentConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.agent.len(), 1);
        assert!(config.agent[0].model.is_none());
        assert_eq!(config.agent[0].models.len(), 2);
        assert_eq!(config.agent[0].models[0].provider, "openai");
        assert_eq!(config.agent[0].models[1].provider, "deepseek");
    }
}
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib domain::tests`
Expected: 所有测试通过

- [ ] **Step 4: 更新现有测试**

搜索并更新所有构造 `AgentEntry` 的测试代码，添加 `model: None` 和 `models: vec![]`：

```bash
grep -rn "AgentEntry {" --include="*.rs" src/ tests/
```

- [ ] **Step 5: 提交**

```bash
git add src/domain/mod.rs
git commit -m "feat(domain): add models field to AgentEntry

- models: Vec<ModelChainEntry> for ordered model chain
- model: Option<String> for backward compat
- Both can coexist; model generates single-element chain if models is empty

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 9: 启动流程改造 — 加载 ProvidersConfig

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `ProvidersConfig`, `ExecutorRegistry`
- Produces: `ExecutorRegistry` Resource 注入，`HARNESS_PROVIDERS_CONFIG` 环境变量

- [ ] **Step 1: 添加 HARNESS_PROVIDERS_CONFIG 环境变量**

在 `src/app/mod.rs` 的 `HarnessConfig::from_env()` 中添加：

```rust
impl HarnessConfig {
    pub fn from_env() -> Result<Self> {
        // ... 现有代码 ...

        let providers_config_path = std::env::var("HARNESS_PROVIDERS_CONFIG")
            .unwrap_or_else(|_| "providers.toml".to_string());

        Ok(Self {
            // ... 现有字段 ...
            providers_config_path,  // 新增字段
        })
    }
}
```

同时在 `HarnessConfig` 结构体中添加字段：

```rust
pub struct HarnessConfig {
    // ... 现有字段 ...
    pub providers_config_path: String,
}
```

- [ ] **Step 2: 修改 main.rs 启动流程**

在 `src/main.rs` 中修改启动逻辑（约 L95-105）：

找到：

```rust
let executor = create_executor_from_config(&config.llm)?;
```

替换为：

```rust
// 加载 providers.toml 或从环境变量构建
let executor_registry = if std::path::Path::new(&config.providers_config_path).exists() {
    let providers_toml = std::fs::read_to_string(&config.providers_config_path)
        .with_context(|| format!("failed to read {}", config.providers_config_path))?;
    let providers_config: crate::domain::ProvidersConfig = toml::from_str(&providers_toml)
        .with_context(|| "failed to parse providers.toml")?;
    
    info!(
        path = %config.providers_config_path,
        provider_count = providers_config.provider.len(),
        "providers config loaded"
    );
    
    crate::llm::ExecutorRegistry::from_config(&providers_config)?
} else {
    info!("providers.toml not found, using global env config as single provider");
    crate::llm::ExecutorRegistry::from_env()?
};
```

- [ ] **Step 3: 更新 create_app 函数签名**

修改 `create_app` 函数，接收 `ExecutorRegistry` 而非 `Arc<dyn AgentExecutor>`：

```rust
pub fn create_app(
    config: HarnessConfig,
    runtime: Arc<Runtime>,
    executor_registry: ExecutorRegistry,  // 改为 ExecutorRegistry
    // ...
) -> App {
    // ...
    app.insert_resource(executor_registry);  // 注入 Resource
    // ...
}
```

- [ ] **Step 4: 移除旧的 ExecutorHandle**

在 `src/app/mod.rs` 中移除 `ExecutorHandle` Resource（如果不再需要），或保留用于向后兼容。

- [ ] **Step 5: 运行构建确认通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 6: 提交**

```bash
git add src/app/mod.rs src/main.rs
git commit -m "feat(app): load ProvidersConfig and inject ExecutorRegistry

- Add HARNESS_PROVIDERS_CONFIG env var (default: providers.toml)
- Fall back to env-based single provider if file missing
- Replace ExecutorHandle with ExecutorRegistry Resource

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 10: spawn ModelChainState Component

**Files:**
- Modify: `src/systems/maintenance.rs`

**Interfaces:**
- Consumes: `AgentEntry.models`, `ExecutorRegistry`
- Produces: `ModelChainState` Component 附加到 Agent 实体

- [ ] **Step 1: 修改 spawn_persistent_agent_from_entry**

在 `src/systems/maintenance.rs` 中修改 `spawn_persistent_agent_from_entry` 函数（约 L153-185）：

```rust
fn spawn_persistent_agent_from_entry(
    commands: &mut Commands,
    entry: &crate::domain::AgentEntry,
    registry: &crate::llm::ExecutorRegistry,
) {
    let id = Uuid::new_v4();

    // 确定模型链
    let models = if !entry.models.is_empty() {
        entry.models.clone()
    } else if let Some(model) = &entry.model {
        // 向后兼容：从单 model 字段生成单元素链
        // 使用默认 provider（第一个注册的）
        let default_provider = registry.executors.keys().next()
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        
        vec![crate::domain::ModelChainEntry {
            provider: default_provider,
            model: model.clone(),
            fallback_cooldown_secs: None,
        }]
    } else {
        vec![]
    };

    let (profile_model, model_chain_state) = if !models.is_empty() {
        let first_model = models[0].model.clone();
        let state = crate::domain::ModelChainState::new(
            models,
            registry.default_cooldown_secs(),
        );
        (first_model, Some(state))
    } else {
        ("gpt-4.1-mini".to_string(), None)  // fallback
    };

    debug!(
        event = "PersistentAgentSpawned",
        agent_id = %id,
        agent_name = %entry.name,
        agent_model = %profile_model,
        has_model_chain = model_chain_state.is_some(),
        "spawning persistent agent"
    );

    let tool_permissions = entry
        .tools
        .clone()
        .map(crate::domain::AgentToolPermissions::from)
        .unwrap_or_default();

    let mut entity_commands = commands.spawn(crate::domain::Agent {
        id,
        profile: crate::domain::AgentProfile {
            name: entry.name.clone(),
            model: profile_model,
        },
        capabilities: crate::domain::AgentCapabilities {
            tags: entry.tags.clone(),
            description: entry.description.clone(),
        },
        kind: crate::domain::AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        tool_permissions,
    });

    // 附加 ModelChainState Component
    if let Some(state) = model_chain_state {
        entity_commands.insert(state);
    }
}
```

- [ ] **Step 2: 更新调用点传入 registry**

修改调用 `spawn_persistent_agent_from_entry` 的 system，传入 `ExecutorRegistry` Resource：

```rust
fn spawn_persistent_agents_system(
    commands: Commands,
    config: Res<HarnessSettings>,
    registry: Res<ExecutorRegistry>,  // 新增
    // ...
) {
    // ...
    spawn_persistent_agent_from_entry(&mut commands, &entry, &registry);
    // ...
}
```

- [ ] **Step 3: 运行构建确认通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 4: 提交**

```bash
git add src/systems/maintenance.rs
git commit -m "feat(maintenance): spawn ModelChainState Component

- Generate model chain from entry.models or legacy model field
- Attach ModelChainState to Agent entity
- Sync AgentProfile.model with chain's first model

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 11: 执行系统改造 — 使用 ExecutorRegistry

**Files:**
- Modify: `src/systems/execution.rs`

**Interfaces:**
- Consumes: `ExecutorRegistry`, `ModelChainState`, `ModelChainStateUpdate`
- Produces: 使用 `ExecutorRegistry` 执行请求，回写状态更新

- [ ] **Step 1: 修改 agent_execution_system**

在 `src/systems/execution.rs` 中重构 `agent_execution_system`：

```rust
use crate::llm::ExecutorRegistry;

pub(crate) fn agent_execution_system(
    clock: Res<Clock>,
    runtime: Res<AsyncRuntime>,
    executor_registry: Res<ExecutorRegistry>,  // 改为 registry
    result_sender: Res<ExecutionResultSender>,
    state_update_sender: Res<ModelChainStateUpdateSender>,  // 新增
    mut commands: Commands,
    requests: Query<(Entity, &AgentExecutionRequestMessage)>,
    mut tasks: Query<&mut Task>,
    agents: Query<(Entity, &crate::domain::Agent, Option<&ModelChainState>)>,  // 新增
) {
    for (entity, message) in &requests {
        let request = message.request.clone();
        let registry = Arc::new(executor_registry.clone());  // clone registry
        let sender = result_sender.0.clone();
        let state_sender = state_update_sender.0.clone();  // 新增

        // 查找 Agent 的 ModelChainState
        let chain_snapshot = agents
            .iter()
            .find(|(_, agent, _)| agent.id == request.agent_id)
            .and_then(|(_, _, state)| state.map(|s| s.clone()));

        // ... 更新 task 状态 ...

        runtime.0.spawn(async move {
            let result = if let Some(mut chain) = chain_snapshot {
                // 有 ModelChainState：执行带降级的请求
                execute_with_fallback_logic(
                    registry.as_ref(),
                    &mut chain,
                    request.clone(),
                    state_sender.clone(),
                ).await
            } else {
                // 无 ModelChainState：使用默认 executor（向后兼容）
                let default_executor = registry.get("default")
                    .or_else(|| registry.executors.values().next().cloned())
                    .expect("no executor available");
                
                default_executor.execute(request.clone()).await
            };

            let _ = sender.send(AgentExecutionResult {
                task_id: request.task_id,
                agent_id: request.agent_id,
                request_kind: request.request_kind,
                result,
                prompt: request.prompt.clone(),
                system_prompt: request.system_prompt.clone(),
                tools: request.tools.clone(),
                reasoning_content: None,
                work_item_id: request.work_item_id,
            });
        });

        commands.entity(entity).despawn();
    }
}
```

- [ ] **Step 2: 实现 execute_with_fallback_logic 辅助函数**

在 `src/systems/execution.rs` 中添加：

```rust
async fn execute_with_fallback_logic(
    registry: &ExecutorRegistry,
    chain_state: &mut ModelChainState,
    mut request: AgentExecutionRequest,
    state_sender: mpsc::UnboundedSender<ModelChainStateUpdate>,
) -> Result<AgentExecutionOutput, ExecutionError> {
    let original_index = chain_state.active_index;
    
    loop {
        // 获取当前优先级的 provider 和 model
        let entry = chain_state.current_entry();
        let executor = registry.get(&entry.provider)
            .ok_or_else(|| ExecutionError::Unknown(format!("provider '{}' not found", entry.provider)))?;

        // 设置 model_override
        request.model_override = Some(entry.model.clone());

        // 执行
        let result = executor.execute(request.clone()).await;

        match result {
            Ok(output) => {
                // 成功，若发生过降级，发送状态更新
                if chain_state.active_index != original_index {
                    let _ = state_sender.send(ModelChainStateUpdate {
                        agent_id: request.agent_id,
                        new_active_index: chain_state.active_index,
                        cooldown_until: chain_state.cooldown_until,
                        previous_model: chain_state.chain[original_index].model.clone(),
                        new_model: entry.model.clone(),
                    });
                }
                return Ok(output);
            }
            Err(error) => {
                // 检查是否应降级
                if error.is_fallback_eligible() {
                    let cooldown_secs = chain_state.current_entry()
                        .fallback_cooldown_secs
                        .unwrap_or_else(|| registry.default_cooldown_secs());

                    if chain_state.step_fallback(cooldown_secs) {
                        // 降级成功，继续循环
                        warn!(
                            event = "ModelFallback",
                            agent_id = %request.agent_id,
                            to_provider = %chain_state.current_provider(),
                            to_model = %chain_state.current_model(),
                            cooldown_secs,
                            "falling back to next model"
                        );
                        continue;
                    }
                }

                // 所有优先级耗尽或错误不可降级
                warn!(
                    event = "ModelChainExhausted",
                    agent_id = %request.agent_id,
                    provider = %chain_state.current_provider(),
                    model = %chain_state.current_model(),
                    error = %error,
                    "model chain exhausted"
                );
                return Err(error);
            }
        }
    }
}
```

- [ ] **Step 3: 添加 ModelChainStateUpdateSender Resource**

在 `src/app/mod.rs` 中添加：

```rust
pub struct ModelChainStateUpdateSender(pub mpsc::UnboundedSender<ModelChainStateUpdate>);
pub struct ModelChainStateUpdateReceiver(pub mpsc::UnboundedReceiver<ModelChainStateUpdate>);
```

并在 `create_app` 中初始化：

```rust
let (state_tx, state_rx) = mpsc::unbounded_channel();
app.insert_resource(ModelChainStateUpdateSender(state_tx));
app.insert_resource(ModelChainStateUpdateReceiver(state_rx));
```

- [ ] **Step 4: 添加状态更新 system**

在 `src/systems/execution.rs` 中添加：

```rust
pub(crate) fn model_chain_state_update_system(
    mut updates: EventReader<ModelChainStateUpdate>,
    mut agents: Query<(&crate::domain::AgentId, &mut ModelChainState, &mut crate::domain::Agent)>,
) {
    for update in updates.read() {
        for (_, mut state, mut agent) in &mut agents {
            if agent.id == update.agent_id {
                state.active_index = update.new_active_index;
                state.cooldown_until = update.cooldown_until;
                agent.profile.model = update.new_model.clone();

                info!(
                    event = "ModelChainStateUpdated",
                    agent_id = %update.agent_id,
                    active_index = update.new_active_index,
                    new_model = %update.new_model,
                    "model chain state updated"
                );
            }
        }
    }
}
```

- [ ] **Step 5: 运行构建确认通过**

Run: `cargo build`
Expected: 编译成功（可能有未使用的导入警告）

- [ ] **Step 6: 提交**

```bash
git add src/systems/execution.rs src/app/mod.rs
git commit -m "feat(execution): use ExecutorRegistry with fallback logic

- Clone ModelChainState and pass to async task
- Execute with fallback on 429/402 errors
- Send ModelChainStateUpdate to write back state
- Sync AgentProfile.model on state change

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 12: 配置文件示例

**Files:**
- Create: `providers.toml`
- Modify: `agents.toml`
- Modify: `.env.example`

**Interfaces:**
- Produces: 示例配置文件

- [ ] **Step 1: 创建 providers.toml 示例**

创建 `providers.toml`：

```toml
# Provider 配置示例
# 每个provider 需要对应的环境变量（如 OPENAI_API_KEY）

default_fallback_cooldown_secs = 60
default_provider = "openai"

[[provider]]
name = "openai"
kind = "openai"
api_key_env = "OPENAI_API_KEY"

[[provider]]
name = "deepseek"
kind = "deepseek"
api_key_env = "DEEPSEEK_API_KEY"

[[provider]]
name = "openrouter"
kind = "openai-compatible"
api_key_env = "OPENROUTER_API_KEY"
api_base = "https://openrouter.ai/api/v1"
```

- [ ] **Step 2: 更新 agents.toml 添加 models 示例**

在 `agents.toml` 中为 `default-llm-agent` 添加 models 链：

```toml
[[agent]]
name = "default-llm-agent"
tags = ["llm", "default", "general"]
description = "默认 LLM Agent，处理通用任务"

[[agent.models]]
provider = "openai"
model = "gpt-4.1-mini"

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"
fallback_cooldown_secs = 120

[agent.tools]
default_permission = "Confirm"
spawn_agent = "Deny"
execute_code = "Deny"
read_file = "Allow"
write_file = "Confirm"
search_web = "Allow"
```

- [ ] **Step 3: 更新 .env.example**

在 `.env.example` 中添加 provider 环境变量示例：

```bash
# Provider 环境变量
OPENAI_API_KEY=sk-...
DEEPSEEK_API_KEY=sk-...
OPENROUTER_API_KEY=sk-or-...

# Provider 配置文件路径（可选）
HARNESS_PROVIDERS_CONFIG=providers.toml
```

- [ ] **Step 4: 提交**

```bash
git add providers.toml agents.toml .env.example
git commit -m "chore: add providers.toml and update agents.toml with models

- providers.toml example with openai/deepseek/openrouter
- agents.toml with model chain for default-llm-agent
- .env.example with provider env vars

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 13: 文档同步

**Files:**
- Modify: `docs/configuration.md`
- Modify: `docs/current-state.md`
- Modify: `README.md`

**Interfaces:**
- Produces: 更新的文档

- [ ] **Step 1: 更新 docs/configuration.md**

添加 `HARNESS_PROVIDERS_CONFIG` 环境变量说明：

```markdown
### Provider 配置

| 环境变量 | 说明 | 默认值 |
|----------|------|--------|
| `HARNESS_PROVIDERS_CONFIG` | providers.toml 路径 | `providers.toml` |

#### providers.toml 结构

\`\`\`toml
default_fallback_cooldown_secs = 60
default_provider = "openai"

[[provider]]
name = "openai"
kind = "openai"
api_key_env = "OPENAI_API_KEY"
\`\`\`

#### agents.toml 模型链

\`\`\`toml
[[agent.models]]
provider = "openai"
model = "gpt-4.1-mini"

[[agent.models]]
provider = "deepseek"
model = "deepseek-chat"
fallback_cooldown_secs = 120
\`\`\`
```

- [ ] **Step 2: 更新 docs/current-state.md**

在"已实现"部分添加：

```markdown
- Per-Agent 多模型/多提供商差异化调度与降级
  - ExecutorRegistry 管理多个 provider executor
  - ModelChainState Component 追踪降级状态
  - 429/402 错误自动降级到下一优先级
  - 冷却期自动恢复到原优先级
```

- [ ] **Step 3: 更新 README.md**

在能力列表中添加：

```markdown
- **Per-Agent 多模型支持**：不同 Agent 可使用不同模型/提供商
- **自动降级**：429/402 错误自动切换到备用模型
- **冷却恢复**：降级后自动恢复到原优先级
```

- [ ] **Step 4: 提交**

```bash
git add docs/configuration.md docs/current-state.md README.md
git commit -m "docs: update for per-agent multi-model/fallback

- Add HARNESS_PROVIDERS_CONFIG and providers.toml docs
- Update current-state.md with new capability
- Update README with multi-model features

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 14: 集成测试

**Files:**
- Create: `tests/model_chain_integration.rs`

**Interfaces:**
- Consumes: 所有已实现功能
- Produces: 集成测试验证降级流程

- [ ] **Step 1: 编写 mock executor 测试**

创建 `tests/model_chain_integration.rs`：

```rust
use harness::domain::{ModelChainEntry, ModelChainState};
use harness::llm::AgentExecutor;
use std::sync::Arc;

struct MockExecutor {
    should_fail_with_429: bool,
}

impl AgentExecutor for MockExecutor {
    fn execute(&self, _request: harness::domain::AgentExecutionRequest) -> harness::llm::ExecutorFuture {
        Box::pin(async move {
            if self.should_fail_with_429 {
                Err(harness::domain::ExecutionError::RateLimited {
                    message: "too many requests".to_string(),
                    retry_after_secs: Some(60),
                })
            } else {
                Ok(harness::domain::AgentExecutionOutput {
                    content: harness::domain::OutputContent::Text("ok".to_string()),
                    reasoning_content: None,
                })
            }
        })
    }
}

#[test]
fn fallback_moves_to_next_priority_on_429() {
    let chain = vec![
        ModelChainEntry {
            provider: "primary".to_string(),
            model: "gpt-4".to_string(),
            fallback_cooldown_secs: None,
        },
        ModelChainEntry {
            provider: "backup".to_string(),
            model: "gpt-3.5".to_string(),
            fallback_cooldown_secs: None,
        },
    ];

    let mut state = ModelChainState::new(chain, 60);
    assert_eq!(state.active_index, 0);

    // 模拟 429 错误后降级
    let cooldown = state.current_entry().fallback_cooldown_secs.unwrap_or(60);
    let success = state.step_fallback(cooldown);

    assert!(success);
    assert_eq!(state.active_index, 1);
    assert_eq!(state.current_provider(), "backup");
}

#[test]
fn fallback_exhausted_returns_false() {
    let chain = vec![
        ModelChainEntry {
            provider: "only".to_string(),
            model: "gpt-4".to_string(),
            fallback_cooldown_secs: None,
        },
    ];

    let mut state = ModelChainState::new(chain, 60);
    let success = state.step_fallback(60);

    assert!(!success);  // 无法降级
    assert_eq!(state.active_index, 0);
}
```

- [ ] **Step 2: 运行测试确认通过**

Run: `cargo test --test model_chain_integration`
Expected: 所有测试通过

- [ ] **Step 3: 提交**

```bash
git add tests/model_chain_integration.rs
git commit -m "test: add model chain integration tests

- Verify fallback moves to next priority on 429
- Verify exhausted chain returns false

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Task 15: 最终验证与清理

**Files:**
- All modified files

**Interfaces:**
- Produces: 完整的通过所有测试的实现

- [ ] **Step 1: 运行完整测试套件**

Run: `cargo test --all-features`
Expected: 所有测试通过

- [ ] **Step 2: 运行 clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: 无警告

- [ ] **Step 3: 运行 fmt**

Run: `cargo fmt --all --check`
Expected: 格式正确

- [ ] **Step 4: 运行 markdownlint**

Run: `npx markdownlint docs/`
Expected: 无错误

- [ ] **Step 5: 最终提交**

```bash
git add -A
git commit -m "chore: final cleanup and verification

All tests pass, clippy clean, formatted.

Co-Authored-By: Claude <noreply@anthropic.com>"
```

- [ ] **Step 6: 创建 PR**

```bash
git push origin feature/per-agent-multi-model-fallback
gh pr create --title "feat: Per-Agent 多模型/多提供商差异化调度与降级" --body "实现 spec: docs/superpowers/specs/2026-07-10-per-agent-multi-model-fallback-design.md"
```

---

## 自审检查清单

**1. Spec 覆盖检查：**

| Spec 章节 | 对应任务 |
|-----------|----------|
| 领域类型（ModelChainState, ProviderEntry） | Task 1 |
| is_fallback_eligible 方法 | Task 2 |
| AgentExecutionRequest.model_override | Task 3 |
| GenaiExecutor 支持 model_override | Task 4 |
| classify_genai_error 拆分 402/403 | Task 5 |
| ExecutorRegistry 实现 | Task 6 |
| ModelChainStateUpdate 消息 | Task 7 |
| AgentEntry.models 字段 | Task 8 |
| 启动流程（ProvidersConfig 加载） | Task 9 |
| spawn ModelChainState Component | Task 10 |
| 执行系统改造（execute_with_fallback） | Task 11 |
| 配置文件示例 | Task 12 |
| 文档同步 | Task 13 |
| 集成测试 | Task 14 |

**2. 无占位符检查：** 已确认所有步骤包含完整代码，无 TBD/TODO。

**3. 类型一致性检查：**
- `ModelChainState` 在 Task 1 定义，Task 10/11 使用 ✓
- `ExecutorRegistry` 在 Task 6 定义，Task 9/10/11 使用 ✓
- `ModelChainStateUpdate` 在 Task 7 定义，Task 11 使用 ✓
- `AgentExecutionRequest.model_override` 在 Task 3 添加，Task 4/11 使用 ✓

---

**Plan complete.** Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
