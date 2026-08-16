# 场景测试目录（Layer 2/3）

本目录承载声明式场景测试，设计见
`docs/design/2026-08-16-real-llm-scenario-testing-design.md`。
完整测试使用方式（四层模型、冒烟/场景/人工流程）见 `docs/testing.md`。

## 目录结构

```text
tests/scenarios/
├── *.toml               # 场景定义（纳入版本管理）
├── golden/              # 金标准快照（人工确认后纳入版本管理，更新需人工比对后显式覆盖）
├── review-pending/      # 人工待审队列（运行时产物，gitignore）
└── reports/             # 运行报告 Markdown（运行时产物，gitignore）
```

## 执行方式

```bash
# 真实 API 场景运行（手动、低频）
HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=sk-xxxx \
  cargo test --test real_llm_scenarios -- --ignored --nocapture

# 框架自检（mock executor，随 CI 常规运行）
cargo test --test real_llm_scenarios
```

结论规则：确定性断言（`tool_called` / `state_reached` / `response_matches`）
失败即测试失败；Judge 低置信、票数分裂或 `human_review` 断言写入待审队列，
不算测试失败。

## 场景字段与断言类型

### 场景字段（[scenario] 表）

| 字段 | 必填 | 说明 |
|------|------|------|
| `name` / `description` / `input` | 是 | 场景名、描述、第一轮输入 |
| `follow_ups` | 否 | 后续轮次输入列表；非空时任务按多轮会话运行（LLM 回复后等待续轮），全部注入后自动发送 `/finish` 收尾 |
| `compression_threshold_tokens` | 否 | 覆写 MemoryConfig 压缩阈值（默认 8000），用于低阈值触发摘要压缩 |
| `max_cost_usd` / `timeout_secs` | 否 | 软预算与整体 wall-clock 超时（含全部轮次） |

### 断言类型

在原有 5 类基础上新增：

| 类型 | 判断者 | 失败行为 |
|------|--------|---------|
| `summarization_triggered` | 代码 | 压缩触发次数 < `min_times`（默认 1）直接 fail |

多轮场景的 `response_matches` 检查最后一轮回复（非 `/finish` 的收尾文案）。

## Judge 独立 provider（可选）

Judge 模型应与被测模型不同源（避免自我偏好），通过以下环境变量组配置；
全部缺省时复用主 provider（报告中标注 same-provider 偏好风险）：

```bash
HARNESS_TEST_JUDGE_PROVIDER=openai        # openai / anthropic / deepseek / openai-compatible
HARNESS_TEST_JUDGE_MODEL=gpt-4.1-mini
HARNESS_TEST_JUDGE_API_KEY=sk-xxxx
HARNESS_TEST_JUDGE_API_BASE=https://...   # 仅 openai-compatible 必需
```

## 其他可调参数

- `HARNESS_TEST_SCENARIO_GAP_SECS`：场景间隔节流秒数（默认 2，防限流）
- `HARNESS_TEST_SCENARIO_POLL_MS`：轮询间隔毫秒数（默认 50）

## 待审队列

Judge 低置信、票数分裂、`human_review` 断言以及运行失败的场景样本会写入
`review-pending/<scenario>-<timestamp>.md`。人工标注 `pass` / `fail` / `partial`
后可将通过的样本沉淀为 `golden/<scenario>.md` 金标准。
