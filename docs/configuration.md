# 配置说明

本文档说明 Harness 当前 MVP 阶段的运行配置，重点覆盖 LLM provider 相关环境变量与推荐用法。

## 配置来源

当前主程序通过环境变量加载运行配置，入口位于 [main.rs](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/src/main.rs) 和 [app/mod.rs](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/src/app/mod.rs)。

配置加载顺序如下：

- 主程序调用 `HarnessConfig::from_env()`
- `HarnessConfig` 内部委托 `LlmProviderConfig::from_env()`
- `LlmProviderConfig` 负责解析 provider、模型与鉴权信息

## 环境变量

### 通用变量

| 变量名 | 必填 | 说明 |
|------|------|------|
| `HARNESS_LLM_PROVIDER` | 否 | provider 类型，默认 `openai` |
| `HARNESS_MODEL` | 否 | 模型名称，默认 `gpt-4.1-mini` |
| `HARNESS_LLM_API_KEY` | 条件必填 | 首选 API Key |
| `HARNESS_LLM_API_BASE` | 条件必填 | OpenAI 兼容接口的基础地址 |
| `HARNESS_LLM_ORG_ID` | 否 | 可选组织 ID |
| `HARNESS_LLM_PROJECT_ID` | 否 | 可选项目 ID |

### 兼容回退变量

为了兼容 OpenAI 默认环境变量命名，当前也支持以下回退读取：

- `OPENAI_API_KEY`
- `OPENAI_BASE_URL`
- `OPENAI_ORG_ID`
- `OPENAI_PROJECT_ID`

优先级规则：

- API Key：优先读 `HARNESS_LLM_API_KEY`，否则回退 `OPENAI_API_KEY`
- API Base：优先读 `HARNESS_LLM_API_BASE`，否则回退 `OPENAI_BASE_URL`
- Org ID：优先读 `HARNESS_LLM_ORG_ID`，否则回退 `OPENAI_ORG_ID`
- Project ID：优先读 `HARNESS_LLM_PROJECT_ID`，否则回退 `OPENAI_PROJECT_ID`

## Provider 取值

当前支持以下 provider：

| 取值 | 含义 |
|------|------|
| `openai` | 标准 OpenAI 接口 |
| `openai-compatible` | OpenAI 兼容协议接口 |
| `compatible` | `openai-compatible` 的别名 |

## 配置约束

当前代码会执行以下校验，见 [llm/mod.rs](file:///Users/diater/Library/Mobile%20Documents/com~apple~CloudDocs/Obsidian/diater/Harness/src/llm/mod.rs)：

- `HARNESS_MODEL` 不能为空
- API Key 不能为空
- 当 `HARNESS_LLM_PROVIDER=openai-compatible` 时，必须提供 `HARNESS_LLM_API_BASE`

## 配置示例

### OpenAI

```bash
export HARNESS_LLM_PROVIDER=openai
export HARNESS_MODEL=gpt-4.1-mini
export HARNESS_LLM_API_KEY=sk-xxxx
```

### OpenAI Compatible

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://example.com/v1
```

## 本地开发建议

- 复制 `.env.example` 生成本地私有环境文件
- 不要提交真实 API Key 到仓库
- 共享示例配置时，仅更新 `.env.example`
- 若接入新的兼容 provider，优先保持 OpenAI 协议兼容，避免过早为单家厂商定制分支逻辑
