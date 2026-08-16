# 测试指南

> __状态：当前有效__
>
> 本指南汇总真实 LLM 测试（Layer 1-3）与日常测试（Layer 0）的使用方式。
> 设计依据：`docs/design/2026-08-16-real-llm-scenario-testing-design.md`。

## 四层测试模型

| 层级 | 测试 | 执行方式 | 依赖 | 判断方式 |
|------|------|---------|------|---------|
| Layer 0 | 单元/集成（mock executor） | `cargo test --all-features`（CI 必跑） | 无 | 确定性断言 |
| Layer 1 | 冒烟（`tests/real_llm_smoke.rs`） | `cargo test --test real_llm_smoke -- --ignored` | 真实 API | 结构性断言 |
| Layer 2 | 场景（`real_llm_scenarios.rs`） | `cargo test --test real_llm_scenarios -- --ignored` | 真实 API + 场景文件 | 混合断言 |
| Layer 3 | 人工审批队列 | 处理 `review-pending/` 产出 | Layer 2 产出 | AI 判断 + 人工标注 |

真实 API 测试（Layer 1/2）统一采用 `#[ignore]` + `HARNESS_TEST_REAL_LLM`
环境变量双重门控，永不进入 CI；未设置环境变量时自动 skip 不失败。

## Layer 0：日常测试（进 CI）

```bash
cargo test --all-features
```

- 无需 API key、无网络依赖，确定性。
- 覆盖：编排逻辑（状态机、派发、工具循环、通道路由）、genai 适配层纯函数、
  Judge 解析与 prompt 构建、场景框架自检（mock executor）。
- 单独跑场景框架自检：

```bash
cargo test --test real_llm_scenarios
```

## Layer 1：冒烟测试（真实 API 连通性）

最小化验证 genai 适配层连通性与往返正确性，每个 provider 一组测试，
目标是"分钟级、单次调用、结构性断言"。

```bash
HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=sk-xxxx \
HARNESS_TEST_PROVIDER=openai \
  cargo test --test real_llm_smoke -- --ignored --nocapture
```

- `HARNESS_TEST_PROVIDER`：本次测试的 provider，一次只测一个
  （`openai` / `anthropic` / `deepseek` / `openai-compatible`）。
- 覆盖点：纯文本往返、tool_calls 往返、工具名 sanitize 往返、
  OpenAiCompatible 自定义端点、错误分类（401/429 走 wiremock，确定性进 CI）。

## Layer 2：场景测试（真实 API + 完整 ECS 链路）

声明式场景：`tests/scenarios/*.toml` 定义输入、断言与预算，runner 构建完整
ECS 应用跑端到端链路。

```bash
HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=sk-xxxx \
  cargo test --test real_llm_scenarios -- --ignored --nocapture
```

- 自动发现并执行 `tests/scenarios/*.toml` 全部场景，场景间串行 + 节流。
- 断言分级：

| 断言类型 | 判断者 | 失败行为 |
|---------|--------|---------|
| `tool_called` / `state_reached` / `response_matches` | 代码 | 直接失败（测试红） |
| `llm_judge` | AI（采样投票） | 低置信/票数分裂 → 待审队列，不算失败 |
| `human_review` | 人工 | 强制待审队列，不算失败 |

### Judge 独立 provider（推荐）

Judge 模型应与被测模型不同源，避免自我偏好：

```bash
HARNESS_TEST_JUDGE_PROVIDER=openai        # openai / anthropic / deepseek / openai-compatible
HARNESS_TEST_JUDGE_MODEL=gpt-4.1-mini
HARNESS_TEST_JUDGE_API_KEY=sk-xxxx
HARNESS_TEST_JUDGE_API_BASE=https://...   # 仅 openai-compatible 必需
```

全部缺省时复用主 provider（报告标注 same-provider 偏好风险）。

### 可调参数

- `HARNESS_TEST_SCENARIO_GAP_SECS`：场景间隔节流秒数（默认 2，防限流）。
- `HARNESS_TEST_SCENARIO_POLL_MS`：场景轮询间隔毫秒数（默认 50）。

场景目录细节见 `tests/scenarios/README.md`。

## Layer 3：人工判断（human-in-the-loop）

### 待审队列

Judge 低置信、票数分裂、`human_review` 断言及运行失败的样本写入
`tests/scenarios/review-pending/<scenario>-<timestamp>.md`。处理方式：

1. 阅读样本（输入、输出、工具序列、Judge 裁决）。
2. 在文件末尾追加 `verdict: pass|fail|partial`。
3. `pass` 样本沉淀为金标准；`fail` / `partial` 记录退化，必要时调整场景或 rubric。

### 金标准快照

- `tests/scenarios/golden/<scenario>.md` 首次运行自动创建（纳入版本管理）。
- 后续运行 diff 结构差异：工具序列变化报告 + 语义差异交 Judge 复核。
- 金标准更新需人工比对后显式覆盖，防止静默漂移。

## 运行时产物

| 目录 | 内容 | 版本管理 |
|------|------|---------|
| `tests/scenarios/reports/` | 每次运行报告（Markdown） | gitignore |
| `tests/scenarios/review-pending/` | 人工待审样本 | gitignore |
| `tests/scenarios/golden/` | 金标准快照 | 纳入版本管理 |
| `tests/scenarios/*.toml` | 场景定义 | 纳入版本管理 |

## 边界与注意

- 真实 API 测试天然非确定，定位是"手动回归 + 趋势监控"，不作为合并门禁。
- 成本：单次全场景运行预算控制在美元级（场景声明 `max_cost_usd` 软预算，
  报告展示；LLM 输出暂无 usage 字段，不精确估算）。
- 网络抖动/限流：依赖场景超时预算（`timeout_secs`）与串行节流兜底。
