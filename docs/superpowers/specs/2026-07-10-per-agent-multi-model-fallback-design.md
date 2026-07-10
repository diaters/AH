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
    /// 有序模型链
    chain: Vec<ModelChainEntry>,
    /// 当前生效的索引（0 = 最高优先级）
    active_index: usize,
    /// 降级冷却截止时刻（Instant），None 表示未降级
    cooldown_until: Option<Instant>,
    /// 全局默认冷却期
    default_cooldown_secs: u64,
}
```

### 修改现有类型

- `AgentEntry`：新增 `models: Vec<ModelChainEntry>`，保留 `model: String` 做向后兼容解析
- `AgentProfile`：移除 `model: String` 字段，模型信息改由 `ModelChainState` Component 承载
- `AgentSummary`：`model: String` → `models: Vec<ModelChainEntry>`
- 子 Agent 继承逻辑（`maintenance.rs`）：spawn 子 Agent 时，若请求未指定 model chain，则从父 Agent 的 `ModelChainState` 克隆整条链

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

```
1. agent_execution_system 遍历执行请求
2. 查询 Agent 的 ModelChainState Component
3. 若 ModelChainState 存在：
   a. 检查冷却期：若 cooldown_until 已过，重置 active_index = 0
   b. 调用 ExecutorRegistry::execute_with_fallback()
      - 从 chain[active_index] 取 (provider, model)
      - 查找 executor，构建 request（注入该 model）
      - 执行，若成功则返回
      - 若失败且错误可降级（429/402/403）：
        - active_index += 1
        - 设置 cooldown_until = now + cooldown_secs
        - 若还有下一个优先级，重试
        - 若所有优先级耗尽，返回最后一个错误
4. 若 ModelChainState 不存在（向后兼容）：
   - 使用全局 ExecutorHandle（现有行为）
```

## 降级错误判定

仅以下错误触发降级：

| 错误 | HTTP 状态码 | ExecutionError 变体 |
|------|-------------|---------------------|
| 限流 | 429 | `RateLimited(String)` |
| 配额耗尽 | 402 | `QuotaExceeded(String)` |
| 认证失败 | 403 | `Authentication(String)`（已有） |

5xx 服务端错误、网络错误不触发降级（应走重试而非降级）。

`classify_genai_error()` 需扩展，将 genai 的 HTTP 429/402 映射到新变体。

## 启动初始化与向后兼容

### 启动流程

```
main.rs:
  1. HarnessConfig::from_env()                    // 现有
  2. ProvidersConfig::load("providers.toml")      // 新增
  3. ExecutorRegistry::from_config(&providers)    // 替代 create_executor_from_config
  4. 加载 agents.toml → AgentConfig
  5. spawn_persistent_agent_from_entry 时：
     - 若 entry.models 非空 → spawn ModelChainState component
     - 若 entry.models 为空但 entry.model 存在 → 生成单元素链（向后兼容）
     - 若两者都为空 → 不 spawn ModelChainState（使用全局 executor）
```

### 向后兼容策略

- `agents.toml` 旧的 `model = "gpt-4.1-mini"` 仍可解析。若 `models` 为空且 `model` 存在，自动生成 `models = [{ provider = <全局默认provider>, model = <model值> }]`
- 保留 `ExecutorHandle` Resource 作为 fallback
- `HARNESS_LLM_PROVIDER` + `HARNESS_MODEL` 仍作为全局默认

### providers.toml 缺失时

- 若 `providers.toml` 不存在，从环境变量构建单 provider 配置，行为与当前完全一致
- 启动日志：`"providers.toml not found, using global env config as single provider"`

## 日志

- 降级事件：`info!(event = "ModelFallback", agent_id, from_provider, from_model, to_provider, to_model, cooldown_secs)`
- 冷却恢复事件：`info!(event = "ModelCooldownExpired", agent_id, provider, model)`
- 所有优先级耗尽：`warn!(event = "ModelChainExhausted", agent_id, last_error)`

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
| `src/domain/mod.rs` | 修改（AgentEntry、AgentProfile） |
| `src/llm/registry.rs` | 新增 |
| `src/llm/genai.rs` | 修改（classify_genai_error 扩展） |
| `src/llm/provider.rs` | 修改（ExecutionError 新变体） |
| `src/llm/mod.rs` | 修改（pub use registry） |
| `src/systems/execution.rs` | 修改（使用 ExecutorRegistry） |
| `src/systems/maintenance.rs` | 修改（spawn ModelChainState） |
| `src/systems/dispatch/brain_dispatch.rs` | 修改（读取 ModelChainState） |
| `src/llm/brain_prompt.rs` | 修改（读取 ModelChainState） |
| `src/contracts/dispatch.rs` | 修改（AgentSummary.models） |
| `src/app/mod.rs` | 修改（ExecutorRegistry Resource） |
| `src/main.rs` | 修改（启动流程） |
| `agents.toml` | 修改（models 链） |
| `providers.toml` | 新增 |
| `.env.example` | 修改（新增 provider 环境变量） |
