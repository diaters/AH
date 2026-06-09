# 用 genai 替换 async-openai 设计文档

> __状态说明（2026-06-09）__
> 分类：当前有效（决策背景）。
> 作用：说明为什么当前 LLM 接入使用 `genai`，以及 provider 能力如何收敛。
> 说明：本文主要是迁移决策记录；具体运行配置以 `docs/configuration.md` 为准。

## 背景

当前项目使用 `async-openai` crate 作为 LLM 客户端，存在以下问题：

1. 不支持 Anthropic（Claude 系列）
2. DeepSeek 推理模式下 Tool 调用存在兼容性问题
3. 扩展新 provider 需要大量适配代码

`genai` crate 提供统一的多 provider 抽象，内置 OpenAI、Anthropic、DeepSeek、Gemini 等 adapter，且支持 OpenAI 兼容 API 的自定义 endpoint。

## 决策

### 核心决策

1. __替换 `async-openai` → `genai 0.6`__：使用 genai 作为唯一的 LLM 客户端
2. __保留 `AgentExecutor` trait__：最小化 trait 抽象（方案 A），不引入额外的 LLM trait 层
3. __保留领域类型__：`ConversationMessage`、`LlmToolCall`、`AgentExecutionOutput` 等不变，`GenaiExecutor` 内部做双向转换
4. __环境变量驱动配置__：`HARNESS_LLM_PROVIDER` 扩展为 `openai` / `anthropic` / `deepseek` / `openai-compatible`

### 理由

- genai 本身已是多 provider 统一抽象层，再自建 trait 属于双重抽象（YAGNI）
- 现有领域类型已是 provider-agnostic，无需重新设计
- 保留 `AgentExecutor` trait 使得上层系统零修改
- 换 crate 代价可控：只需写新的 `impl AgentExecutor`，约 150-200 行转换代码

## 设计详情

### 1. 依赖与 Provider 配置

__Cargo.toml 变更__：

- 移除 `async-openai`
- 添加 `genai = "0.6"`

__LlmProviderKind 扩展__：

```rust
pub enum LlmProviderKind {
    OpenAi,
    Anthropic,
    DeepSeek,
    OpenAiCompatible,
}
```

__环境变量映射__：

| `HARNESS_LLM_PROVIDER` | genai AdapterKind | 认证环境变量 |
|---|---|---|
| `openai` | `openai` | `OPENAI_API_KEY` |
| `anthropic` | `anthropic` | `ANTHROPIC_API_KEY` |
| `deepseek` | `deepseek` | `DEEPSEEK_API_KEY` |
| `openai-compatible` | `openai` + 自定义 endpoint | 自定义 |

genai 的 `Client::default()` 自动从标准环境变量读取 API key。对于 `openai-compatible` 场景，通过 `ServiceTargetResolver` 配置自定义 endpoint：

```rust
let target_resolver = ServiceTargetResolver::from_resolver_fn(|service_target| {
    let endpoint = Endpoint::from_static(&api_base);
    let auth = AuthData::from_key(&api_key);
    let model = ModelIden::new(AdapterKind::OpenAI, service_target.model.model_name);
    Ok(ServiceTarget { endpoint, auth, model })
});
let client = Client::builder()
    .with_service_target_resolver(target_resolver)
    .build();
```

`LlmProviderConfig` 保留 `api_base`、`api_key` 字段用于 `openai-compatible` 场景，OpenAI/Anthropic/DeepSeek 走 genai 自动认证。

### 2. GenaiExecutor 实现

替换 `OpenAiExecutor` → `GenaiExecutor`，实现 `AgentExecutor` trait。

__结构体__：

```rust
pub(crate) struct GenaiExecutor {
    client: genai::Client,
    model: String,
}
```

genai 的 `Client` 内部已是 `Arc` 包装，`Clone` 零成本，无需再包 `Arc`。

__execute 核心流程__：

```text
AgentExecutionRequest
    │
    ├─ conversation? → build_chat_messages(conversation)
    │                  → ChatRequest::new(messages)
    │
    ├─ system_prompt? → chat_req.with_system(sp)
    │
    ├─ tools? → build_genai_tools(tools)
    │           → chat_req.with_tools(genai_tools)
    │
    ▼
client.exec_chat(&model, chat_req, None)
    │
    ▼
ChatResponse
    │
    ├─ has tool_calls? → AgentExecutionOutput::ToolCalls(→ LlmToolCall)
    │
    ├─ has text? → AgentExecutionOutput::Text(content)
    │
    └─ empty → ExecutionError::EmptyResponse
```

__转换函数__：

| 函数 | 方向 | 说明 |
|------|------|------|
| `build_chat_request` | `AgentExecutionRequest` → `ChatRequest` | 组装 system、messages、tools |
| `build_chat_messages` | `Vec<ConversationMessage>` → `Vec<ChatMessage>` | 多轮对话转换 |
| `build_genai_tools` | `Vec<ToolDefinition>` → `Vec<Tool>` | Tool 定义转换 |
| `parse_response` | `ChatResponse` → `Result<AgentExecutionOutput>` | 响应解析 |
| `classify_genai_error` | `genai::Error` → `ExecutionError` | 错误分类 |

__LlmToolCall 转换注意__：领域类型的 `arguments` 是 `String`，genai 的 `ToolCall.fn_arguments` 是 `serde_json::Value`，转换时用 `fn_arguments.to_string()`。

### 3. 错误处理

保留现有 `ExecutionError` 不变。`classify_genai_error` 利用 genai 的枚举型 `Error` 精确映射：

| genai Error | ExecutionError |
|---|---|
| `RequiresApiKey` / `NoAuthResolver` / `NoAuthData` | `Authentication` |
| `HttpError { status: 401 }` | `Authentication` |
| `HttpError { status: 429 }` | `RateLimited` |
| `HttpError { status: 402/403 }` 含 quota | `QuotaExhausted` |
| `HttpError { status: 408/504 }` | `Timeout` |
| `WebAdapterCall` / `WebModelCall` / `WebStream` | `Transport` |
| `NoChatResponse` | `EmptyResponse` |
| 其余 | `Unknown` |

相比之前 `async-openai` 的字符串匹配，基于枚举变体和 HTTP 状态码的映射更精确可靠。

### 4. 文件变更

| 操作 | 文件 | 说明 |
|------|------|------|
| 修改 | `Cargo.toml` | `async-openai` → `genai = "0.6"` |
| 修改 | `src/llm/mod.rs` | `mod openai` → `mod genai` |
| 重写 | `src/llm/openai.rs` → `src/llm/genai.rs` | `OpenAiExecutor` → `GenaiExecutor` |
| 修改 | `src/llm/provider.rs` | `LlmProviderKind` 新增变体，`from_env` 适配 |
| 修改 | `src/llm/factory.rs` | 统一创建 `GenaiExecutor` |
| 修改 | `tests/*.rs` | 移除对 `async-openai` 类型的依赖（如有） |

__不变更__：

- `src/domain/mod.rs` — `AgentExecutor` trait、领域类型不动
- `src/systems/*.rs` — 上层系统零修改
- `src/llm/brain_prompt.rs`、`src/llm/summarization_prompt.rs` — 纯文本生成，不涉及 LLM 类型

集成测试通过 `create_executor_from_config` 获取 `Arc<dyn AgentExecutor>`，只依赖 trait 接口和领域类型，不需要改动测试逻辑。
