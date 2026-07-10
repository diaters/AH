# Per-Agent 多模型/多提供商差异化调度与降级

> **状态：当前有效**

## 背景与动机

当前 AI Harness 使用全局单一 `ExecutorHandle` 执行所有 LLM 请求，`agents.toml` 中的 `model` 字段仅作为元数据传递给 Brain 调度 prompt，不控制实际 LLM 调用。这导致：

1. 所有 Agent 共享同一个模型，无法按任务特征差异化选型
2. 同一模型无法由多个提供商提供（如 OpenAI 直连 vs OpenRouter 代理）
3. 遭遇 429 限流或配额耗尽时无降级路径，整个系统阻塞

## 需求

- **Per-Agent 模型差异化**：不同 Agent 可使用不同模型
- **多提供商**：同一模型可由多个 provider 实例提供（如 `openai` + `openrouter`）
- **优先级降级**：Agent 声明有序模型链，第一优先级失败时按序降级
- **Per-Agent 独立降级**：每个 Agent 独立追踪自己的降级状态
- **冷却期自动恢复**：降级后启动冷却计时器，到期后重新尝试原优先级
- **向后兼容**：现有 `model` 字段和环境变量配置仍可正常工作

## 架构方案：Executor Registry

```
providers.toml → [ProviderEntry] → 每个 provider 一个 GenaiExecutor
                                    ↓
                            ExecutorRegistry (Resource)
                                    ↑
agents.toml → AgentEntry.models → ModelChain [(provider, model, priority), ...]
                                    ↓
                            ModelChainState (Component, per-Agent)
```

- 每个 provider 实例创建一个 `GenaiExecutor`，注册到全局 `ExecutorRegistry`
- 每个 Agent 持有 `ModelChainState` Component，包含有序模型链和降级状态
- 执行时由 `ExecutorRegistry` 根据 chain 当前优先级查找 executor，失败则降级并重试

## 配置层

### providers.toml

```toml
# 全局默认降级冷却期（秒）
default_fallback_cooldown_secs = 60

# 默认 provider（用于旧 model 字段生成单元素链）
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

设计决策：

- `api_key_env` 引用环境变量名，不硬编码密钥，与现有 `LlmProviderConfig::from_env` 模式一致
- 启动时校验 `api_key_env` 指向的环境变量是否存在，缺失则报错退出
- `api_base` 仅 `openai-compatible` 必填
- `name` 是 agents.toml 引用 provider 的唯一标识
- `default_fallback_cooldown_secs` 全局默认冷却期，可被 per-agent 覆盖
- `default_provider` 可选，用于旧 `model` 字段生成单元素链时确定 provider；若未设置则使用第一个 `[[provider]]`

### agents.toml（model 字段改造）

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
```

设计决策：

- `models` 是有序数组，第一个为最高优先级
- 每个 entry 必须指定 `provider`（引用 providers.toml 中的 name）和 `model`
- `fallback_cooldown_secs` 可选覆盖全局默认冷却期
- 向后兼容：若 agent 未配置 `models` 但有 `model` 字段，自动生成单元素链

## 领域类型

### 新增：`src/domain/model_chain.rs`

```rust
/// providers.toml 中的 provider 配置条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub name: String,
    pub kind: LlmProviderKind,
    pub api_key_env: String,
    pub api_base: Option<String>,
}

/// providers.toml 顶层结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    pub default_fallback_cooldown_secs: u64,
    /// 可选，用于旧 model 字段生成单元素链时确定 provider
    #[serde(default)]
    pub default_provider: Option<String>,
    pub provider: Vec<ProviderEntry>,
}

/// agents.toml 中 [[agent.models]] 的条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChainEntry {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub fallback_cooldown_secs: Option<u64>,
}

/// Agent 运行时的模型链状态（Bevy Component）
#[derive(Debug, Clone, Component)]
pub struct ModelChainState {
    /// 有序模型链（公开只读）
    pub chain: Vec<ModelChainEntry>,
    /// 当前生效的索引（0 = 最高优先级）
    pub active_index: usize,
    /// 降级冷却截止时刻（Instant），None 表示未降级
    pub cooldown_until: Option<Instant>,
    /// 全局默认冷却期
    pub default_cooldown_secs: u64,
}

impl ModelChainState {
    /// 从 chain 初始化，设置 active_index = 0
    pub fn new(chain: Vec<ModelChainEntry>, default_cooldown_secs: u64) -> Self;

    /// 当前生效的 ModelChainEntry
    pub fn current_entry(&self) -> &ModelChainEntry;

    /// 降级到下一优先级；返回 false 表示已耗尽
    pub fn step_fallback(&mut self, cooldown_secs: u64) -> bool;

    /// 若冷却期已过，重置 active_index = 0 并返回 true
    pub fn reset_if_cooldown_expired(&mut self, now: Instant) -> bool;

    /// 返回当前生效模型的 provider 名称
    pub fn current_provider(&self) -> &str;

    /// 返回当前生效模型的 model 名称
    pub fn current_model(&self) -> &str;
}
```

### 修改现有类型

- `AgentEntry`：新增 `models: Vec<ModelChainEntry>`，保留 `model: String` 做向后兼容解析
- `AgentProfile`：**保留** `model: String` 字段，语义变更为"当前生效模型"（由 `ModelChainState` 初始化或降级后同步更新）
- `AgentCapabilitySummary`：`model: String` 保持不变，仍展示当前生效模型；新增 `models: Vec<ModelChainEntry>` 展示完整链
- 子 Agent 继承逻辑（`maintenance.rs`）：spawn 子 Agent 时，若请求未指定 model chain，则从父 Agent 的 `ModelChainState` 克隆整条链，并初始化 `profile.model` 为链首模型

## Executor Registry

### 新增：`src/llm/registry.rs`

```rust
#[derive(Resource)]
pub struct ExecutorRegistry {
    executors: HashMap<String, Arc<dyn AgentExecutor>>,
    default_fallback_cooldown_secs: u64,
}

impl ExecutorRegistry {
    pub fn from_config(config: &ProvidersConfig) -> Result<Self>;
    pub fn get(&self, provider_name: &str) -> Option<Arc<dyn AgentExecutor>>;

    /// 带降级的执行
    pub async fn execute_with_fallback(
        &self,
        chain_state: &mut ModelChainState,
        request: AgentExecutionRequest,
    ) -> Result<AgentExecutionResult, ExecutionError>;
}
```

## 执行流程

### 重试与降级的顺序

现有 `ExecutionError::is_retryable()` 对 `RateLimited`/`Timeout`/`Transport`/`Unknown` 执行重试（`src/domain/error.rs:33-54`）。降级发生在**同一 model 的重试耗尽后**：

```
请求失败 → is_retryable()? → 重试（最多 max_retries 次）→ 仍失败 → is_fallback_eligible()? → 降级到下一优先级
```

### ECS 异步状态修改方案

`ModelChainState` 是 Bevy Component，不能在 async 闭包中持有可变引用。采用 **clone + 消息回写** 方案：

```rust
// src/systems/execution.rs

// 1. 在 system 中读取 ModelChainState，clone 其内容
let chain_snapshot = model_chain_state.map(|c| c.clone());

// 2. 将 clone 传入 async 任务
runtime.spawn(async move {
    let (result, updated_state) = executor_registry
        .execute_with_fallback(chain_snapshot, request)
        .await;

    // 3. 通过 channel 发送状态更新
    state_update_sender.send(ModelChainStateUpdate {
        agent_id,
        new_active_index: updated_state.active_index,
        cooldown_until: updated_state.cooldown_until,
    });

    result_sender.send(result);
});

// 4. 另一个 system 接收状态更新并写回 Component
fn model_chain_state_update_system(
    mut updates: EventReader<ModelChainStateUpdate>,
    mut states: Query<&mut ModelChainState>,
) {
    for update in updates.read() {
        if let Ok(mut state) = states.get_mut(update.agent_id) {
            state.active_index = update.new_active_index;
            state.cooldown_until = update.cooldown_until;

            // 同步更新 AgentProfile.model
            // ...
        }
    }
}
```

### 模型注入机制

`AgentExecutionRequest` 当前无 `model` 字段（`src/domain/execution.rs:83-95`），`GenaiExecutor` 在构造时绑定模型（`src/llm/genai.rs:82`）。支持同一 provider 内按请求切换模型：

**方案**：扩展 `AgentExecutionRequest`，新增 `model_override: Option<String>` 字段。`GenaiExecutor::execute()` 检查该字段，若存在则覆盖 `self.model` 调用。

```rust
// src/domain/execution.rs
pub struct AgentExecutionRequest {
    // ... 现有字段
    /// 覆盖 executor 的默认模型（用于多模型 provider）
    pub model_override: Option<String>,
}

// src/llm/genai.rs
impl AgentExecutor for GenaiExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        let model = request.model_override
            .as_ref()
            .unwrap_or(&self.model)
            .clone();
        // 使用 model 发送请求
    }
}
```

### 完整执行流程

```
1. agent_execution_system 遍历执行请求
2. 查询 Agent 的 ModelChainState Component
3. 若 ModelChainState 存在：
   a. 检查冷却期：若 cooldown_until 已过，重置 active_index = 0
   b. Clone ModelChainState 内容
   c. 调用 ExecutorRegistry::execute_with_fallback(chain_snapshot, request)
      - 从 chain[active_index] 取 (provider, model)
      - 查找 executor，构建 request（设置 model_override = Some(model)）
      - 执行，若成功则返回 (result, unchanged_state)
      - 若失败且重试耗尽：
        - 若错误 is_fallback_eligible():
          - step_fallback()
          - 若还有下一个优先级，重试
          - 若所有优先级耗尽，返回最后一个错误
      - 返回 (result, updated_state)
   d. 通过 ModelChainStateUpdate 消息写回状态
   e. 若 active_index 变化，同步更新 AgentProfile.model
4. 若 ModelChainState 不存在（向后兼容）：
   - 使用全局 ExecutorHandle（现有行为）
```

## 降级错误判定

仅以下错误触发降级：

| 错误 | HTTP 状态码 | ExecutionError 变体 | 说明 |
|------|-------------|---------------------|------|
| 限流 | 429 | `RateLimited { message, retry_after_secs }` | 现有类型，保留 `retry_after_secs` 用于日志 |
| 配额耗尽 | 402 | `QuotaExhausted(String)` | 现有类型 |

以下错误**不触发降级**：

| 错误 | HTTP 状态码 | ExecutionError 变体 | 原因 |
|------|-------------|---------------------|------|
| 认证失败 | 401 | `Authentication(String)` | 密钥错误，降级无效 |
| 权限不足 | 403 | `QuotaExhausted(String)` | IP/地区限制或权限问题，同一网络环境下降级无效 |
| 服务端错误 | 5xx | `Timeout`/`Unknown` | 临时故障，应重试而非降级 |
| 网络错误 | - | `Transport(String)` | 临时故障，应重试而非降级 |

`classify_genai_error()` 需修改 `src/llm/genai.rs:322`，将 `402 | 403` 拆分为：
- `402 => ExecutionError::QuotaExhausted(message)` — 触发降级
- `403 => ExecutionError::Authentication(message)` — 不降级（新增 `Authentication` 变体用于 403）

## 启动初始化与向后兼容

### 配置文件路径

| 配置文件 | 环境变量 | 默认值 |
|----------|----------|--------|
| agents.toml | `HARNESS_AGENTS_CONFIG` | `agents.toml` |
| providers.toml | `HARNESS_PROVIDERS_CONFIG` | `providers.toml` |

### 启动流程

```
main.rs:
  1. HarnessConfig::from_env()                    // 现有
  2. ProvidersConfig::load(env("HARNESS_PROVIDERS_CONFIG")) // 新增
  3. ExecutorRegistry::from_config(&providers)    // 替代 create_executor_from_config
  4. 加载 agents.toml → AgentConfig
  5. spawn_persistent_agent_from_entry 时：
     - 若 entry.models 非空 → spawn ModelChainState component，profile.model = models[0].model
     - 若 entry.models 为空但 entry.model 存在 → 生成单元素链（向后兼容，见下文）
     - 若两者都为空 → 不 spawn ModelChainState（使用全局 executor）
```

### 默认 Provider 规则

当旧 `model` 字段需要生成单元素链时，`provider` 的选择规则：

1. 若 `providers.toml` 中有 `default_provider = "xxx"` 字段，使用该值
2. 否则使用 `providers.toml` 中 `[[provider]]` 数组的第一个元素
3. 若 `providers.toml` 不存在，使用 `HARNESS_LLM_PROVIDER` 环境变量值

### 向后兼容策略

- `agents.toml` 旧的 `model = "gpt-4.1-mini"` 仍可解析。若 `models` 为空且 `model` 存在，自动生成 `models = [{ provider = <默认provider>, model = <model值> }]`
- 保留 `ExecutorHandle` Resource 作为 fallback（无 `ModelChainState` 的 Agent 走此路径）
- `HARNESS_LLM_PROVIDER` + `HARNESS_MODEL` 仍作为全局默认
- `AgentSpawnRequestMessage.model` 字段保留，语义变更为"指定单模型（生成单元素链）"；若未指定则从父 Agent 克隆整条链

### providers.toml 缺失时

- 若 `providers.toml` 不存在，从环境变量构建单 provider 配置，行为与当前完全一致
- 启动日志：`"providers.toml not found, using global env config as single provider"`

### 插件 Agent 兼容方案

插件 manifest 中的 `[[agents]]` 同样支持旧 `model` 字段。`collect_plugin_agent_entries()` 解析时，若 `model` 存在但 `models` 为空，按上述默认 provider 规则生成单元素链。

## 日志

- 降级事件：`info!(event = "ModelFallback", agent_id, from_provider, from_model, to_provider, to_model, cooldown_secs)`
- 冷却恢复事件：`info!(event = "ModelCooldownExpired", agent_id, provider, model)`
- 所有优先级耗尽：`warn!(event = "ModelChainExhausted", agent_id, provider, model, last_error)`
- LLM 请求开始（已有）：`info!(event = "LlmRequestStarted", task_id, agent_id, model, provider, tools_count)`

## 测试

### 单元测试

- `ModelChainState` 降级/冷却/恢复状态机转换
- `is_fallback_eligible()` 对各错误类型的判定
- `providers.toml` / `agents.toml` 解析 roundtrip
- 向后兼容：旧 `model` 字段自动生成单元素链

### 集成测试

- mock executor 返回 429 → 验证降级到第二优先级
- 冷却期过后 → 验证恢复到原优先级
- 所有优先级耗尽 → 验证返回最后一个错误
- 无 `ModelChainState` 的 Agent → 验证走全局 executor

## 影响范围

| 文件 | 变更类型 |
|------|----------|
| `src/domain/model_chain.rs` | 新增 |
| `src/domain/mod.rs` | 修改（AgentEntry、ProvidersConfig） |
| `src/domain/execution.rs` | 修改（AgentExecutionRequest 新增 model_override） |
| `src/domain/error.rs` | 修改（ExecutionError::Authentication 用于 403） |
| `src/llm/registry.rs` | 新增 |
| `src/llm/genai.rs` | 修改（classify_genai_error 拆分 402/403，execute 支持 model_override） |
| `src/llm/mod.rs` | 修改（pub use registry） |
| `src/systems/execution.rs` | 修改（使用 ExecutorRegistry，ModelChainStateUpdate 消息） |
| `src/systems/maintenance.rs` | 修改（spawn ModelChainState，更新 AgentProfile.model） |
| `src/systems/dispatch/brain_dispatch.rs` | 修改（读取 ModelChainState） |
| `src/llm/brain_prompt.rs` | 修改（读取 ModelChainState） |
| `src/contracts/dispatch.rs` | 修改（AgentCapabilitySummary 新增 models） |
| `src/domain/message.rs` | 修改（ModelChainStateUpdate 消息） |
| `src/app/mod.rs` | 修改（ExecutorRegistry Resource，HARNESS_PROVIDERS_CONFIG） |
| `src/main.rs` | 修改（启动流程） |
| `agents.toml` | 修改（models 链） |
| `providers.toml` | 新增 |
| `.env.example` | 修改（新增 provider 环境变量） |
| `docs/configuration.md` | 修改（新增 HARNESS_PROVIDERS_CONFIG，更新插件 agent 示例） |
| `docs/current-state.md` | 修改（新增 per-agent 多模型能力） |
| `README.md` | 修改（更新能力列表） |

## 文档同步要求

按 `AGENTS.md` 规范，本设计实施后需同步更新：

- `docs/current-state.md`：新增"Per-Agent 多模型/多提供商差异化调度与降级"能力
- `README.md`：更新能力列表，补充 providers.toml 配置说明
- `docs/configuration.md`：登记 `HARNESS_PROVIDERS_CONFIG` 环境变量，更新插件 Agent manifest 示例
