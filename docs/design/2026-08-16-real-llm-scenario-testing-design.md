# 真实 LLM 场景测试与分层正确性判断设计

> __状态：当前有效__

| 属性 | 值 |
|------|-----|
| 创建日期 | 2026-08-16 |
| 实施状态 | 分阶段实施中（PR 路径见第 10 节） |
| 相关文档 | `2026-05-24-genai-migration-design.md`、`2026-06-06-plan-evaluation-reassessment-design.md` |

## 1. 背景与问题

### 1.1 现状

当前项目共约 1027 个测试（单元 685 + 集成 342），全部通过
`ExecutorRegistry::from_single_executor`（`src/llm/registry.rs:93`）注入 mock executor，
在 `AgentExecutor` trait 边界屏蔽真实 LLM 调用。CI（`.github/workflows/ci.yml`）只执行
`cargo test --all-features`，无 API key、无网络依赖，测试确定性强。

### 1.2 缺口

| 缺口 | 影响 |
|------|------|
| `src/llm/genai.rs` 适配层零测试（`build_chat_request`、`parse_response` 等） | genai 升级或 API 格式变化无法被测试捕获；无验证入口 |
| 无 `#[ignore]` 真实 LLM 冒烟测试 | 手动验证 provider 连通性只能靠运行完整 TUI 会话，无法快速回归 |
| 多 provider 降级链路无测试 | 生产走 `ExecutorRegistry::from_config`，测试全部走单 provider 注入，`per-agent-multi-model-fallback` 语义未覆盖 |
| Evaluation 决策下游分支未验证 | `EvaluationDecision`（Continue/Complete/Failed/OffTrack）解析后的运行时行为分支缺少集成覆盖 |
| Mock executor 重复定义 | `EchoExecutor` 等基础 mock 在约 20 个测试文件中重复实现，行为可能漂移 |
| 真实场景正确性无判断手段 | "Agent 是否真的完成了任务"只能靠人工开 TUI 观察，无结构化判断与沉淀机制 |

### 1.3 核心矛盾

mock 测试验证的是__编排逻辑__（状态机、派发、工具循环、通道路由），它无法回答两类问题：

- __连通性问题__：真实 provider 的请求格式、响应解析、错误分类是否正确。
- __语义问题__：真实 LLM 输出是否正确完成了任务（摘要是否准确、决策是否合理）。

第二类问题本质上__无法用确定性代码完全判断__，需要引入 AI 判断与人工判断的分层机制。

## 2. 设计目标与非目标

### 目标

- 为 genai 适配层提供真实 API 的快速回归入口（分钟级、单 provider、低成本）。
- 提供声明式场景测试框架：一条命令跑完整 ECS 链路 + 真实 LLM，覆盖端到端行为。
- 建立"代码判断 → AI 判断 → 人工判断"三级正确性体系，明确各级的适用边界与降级规则。
- Judge 复用现有 Evaluation 体系成为 harness 一等能力（数据结构、解析、请求通道），
  未来可同时服务于运行时自评估与测试评估。
- 真实 API 测试永不进入 CI：`#[ignore]` + 环境变量双重门控，成本与 flaky 风险受控。

### 非目标

- 不在 CI 中集成真实 API（成本与稳定性不可接受）。
- 不做压测、性能基准、并发容量验证。
- 不做自动 prompt 优化闭环（Judge 只产出判断，不反写 prompt）。
- 不覆盖 QQ/Telegram 真实通道联调（已有 wiremock 覆盖协议层，真实联调属运维验证）。
- 不在本设计中重构 Evaluation 运行时下游分支（`OffTrackPolicy` 行为覆盖作为独立任务）。

## 3. 总体架构：四层测试模型

```text
Layer 3  评估闭环     AI Judge + 人工审批队列（语义正确性）
Layer 2  场景测试     真实 API + 完整 ECS 链路（端到端行为）
Layer 1  冒烟测试     真实 API + 单点验证（适配层连通性）
Layer 0  单元/集成    Mock executor（现有 1027 个，CI 必跑，确定性）
```

| 层级 | 执行方式 | 依赖 | 判断方式 |
|------|---------|------|---------|
| Layer 0 | `cargo test`（CI 必跑） | 无外部依赖 | 确定性断言 |
| Layer 1 | `cargo test -- --ignored`（手动） | `HARNESS_TEST_REAL_LLM=1` + API key | 结构性断言 |
| Layer 2 | `cargo test --test real_llm_scenarios -- --ignored`（手动/低频） | 同上 + 场景文件 | 混合断言（第 6 节） |
| Layer 3 | 场景运行产出的待审队列（人工低频处理） | Layer 2 产出 | AI 判断 + 人工标注 |

设计原则：

- __逐层收敛__：Layer 1 失败无需跑 Layer 2；Layer 2 的确定性断言失败无需看 Judge 结果。
- __判断成本递增__：代码断言零成本，AI 斤断低成本（每场景数次 LLM 调用），人工判断高成本
  （只处理前两级无法裁决的样本）。
- __门控统一__：所有真实 API 测试共用同一组环境变量开关，未设置时测试体自动 skip 并提示，
  不产生失败。

## 4. Layer 1：真实 LLM 冒烟测试

### 4.1 定位

最小化验证 genai 适配层的连通性与往返正确性，每个 provider kind（OpenAi、Anthropic、
DeepSeek、OpenAiCompatible）一组测试，目标是"分钟级、单次调用、结构性断言"。

### 4.2 门控

```rust
// tests/real_llm_smoke.rs
fn real_llm_enabled() -> bool {
    std::env::var("HARNESS_TEST_REAL_LLM").is_ok()
        && std::env::var("HARNESS_LLM_API_KEY").is_ok()
}
```

- `HARNESS_TEST_REAL_LLM`：显式开关，防止误配置导致 CI 意外调用真实 API。
- `HARNESS_LLM_API_KEY`：复用现有生产环境变量（`src/llm/provider.rs:45`）。
- provider 矩阵通过 `HARNESS_TEST_PROVIDER` 选择（默认 `openai`），一次只测一个 provider，
  控制成本。

### 4.3 覆盖点与断言

| 测试 | 断言（全部为结构性断言，不判断语义） |
|------|------|
| 纯文本往返 | 响应在超时预算内返回非空文本 |
| tool_calls 往返 | 请求携带工具定义，响应解析出 `LlmToolCall`，name 与注册名一致，参数是合法 JSON |
| 工具名 sanitize 往返 | 含命名空间的工具名（如 `shell:exec`）经 sanitize/unsanitize 后与原始一致 |
| `OpenAiCompatible` 自定义端点 | `ServiceTargetResolver` 注入的 base_url 生效，请求可达 |
| 错误分类 | 用无效 key 触发 401/403，断言归类为 `Authentication`（不重试）；可选触发 429 断言 `RateLimited` |

### 4.4 与单元测试的分工

`build_chat_request`、`build_genai_tools`、`parse_response`、`build_chat_messages` 属于纯
函数，补__无网络的单元测试__（放 `src/llm/genai.rs` 的 `#[cfg(test)]`），覆盖消息转换、
tool_calls 解析、`reasoning_content` 透传等分支。冒烟测试只负责"真实端点 + 真实响应格式"
这层单元测试无法覆盖的部分。

## 5. Layer 2：场景测试框架

### 5.1 场景定义（TOML 声明式）

与项目现有 `agents.toml` / `providers.toml` 配置风格一致：

```toml
# tests/scenarios/shell_stat_task.toml
[scenario]
name = "shell_stat_task"
description = "统计当前目录下 .rs 文件数量并汇报"
input = "统计当前目录下 .rs 文件数量并汇报"
max_cost_usd = 0.10
timeout_secs = 120

[[assertions]]
type = "tool_called"
tool = "shell_exec"
min_times = 1

[[assertions]]
type = "state_reached"
workitem_status = "Completed"

[[assertions]]
type = "response_matches"
pattern = '\\d+'
desc = "结果包含数字"

[[assertions]]
type = "llm_judge"
rubric = "回答是否正确完成了统计任务，数字是否合理（1-500 之间）"
threshold = 0.7

[[assertions]]
type = "human_review"
note = "汇报口吻是否符合中文助手风格"
```

字段说明：

- `max_cost_usd`：软预算。runner 按 token 用量估算成本，超预算时打印警告并在报告中标记，
  不中断执行（避免估算误差导致误杀）。
- `timeout_secs`：wall-clock 超时，超时即 fail 而非 hang。

### 5.2 场景 runner

新增测试 binary `tests/real_llm_scenarios.rs`（整体 `#[ignore]` 门控）：

- 读取 `tests/scenarios/*.toml`，逐场景构建完整 ECS app（`build_harness_app`）+
  真实 `ExecutorRegistry`（`from_env` 或 `from_config`）。
- 注入 `TestToolResults` / `MockFrontend` 等现有捕获基础设施，收集：
  工具调用序列、WorkItem 状态转换、最终用户可见输出、token 用量。
- 依次执行断言列表，产出结构化报告（JSON + Markdown）到 `tests/scenarios/reports/`。
- 场景间串行执行，间隔节流（可配置），避免 provider 限流。

### 5.3 断言类型

| 类型 | 判断者 | 确定性 | 失败行为 |
|------|--------|--------|---------|
| `tool_called` / `state_reached` / `response_matches` | 代码 | 确定 | 直接 fail |
| `llm_judge` | AI | 概率 | 低于阈值或低置信 → 进入待审队列 |
| `human_review` | 人工 | 确定 | 强制进入待审队列 |

## 6. Layer 3：三级正确性判断体系

核心原则：__代码能判的归代码，代码判不了的归 AI，AI 判不稳的归人。__

### 6.1 第 1 级：结构性断言（代码判断）

适用：格式合规（JSON schema、markdown 结构）、状态机转换序列、工具调用事实、
确定性子串/正则匹配、预算与超时约束。凡此范畴绝不交给 AI。

### 6.2 第 2 级：LLM-as-Judge（AI 判断，复用 Evaluation 体系）

__请求通道__：复用 `AgentRequestKind::Evaluation`（`src/domain/execution.rs:17`）与
`AgentExecutionRequest`，Judge 本质是对测试输出的评估请求，通过 `system_prompt`
注入 Judge 语境。不新增枚举变体，不触碰既有 match 分支（简化优先）。

__数据结构__（新增于 `src/domain/evaluation.rs`，与 `EvaluationResult` 并列）：

```rust
/// Judge 维度
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeDimension {
    pub name: String,        // 如 "correctness" / "completeness"
    pub score: f32,          // 0.0 - 1.0
    pub rationale: String,
}

/// Judge 裁决结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    pub scores: Vec<JudgeDimension>,
    pub pass: bool,
    pub reasoning: String,
    pub confidence: f32,     // 低于阈值时降级人工待审
}

/// Judge rubric 配置
#[derive(Debug, Clone)]
pub struct JudgeRubric {
    pub dimensions: Vec<String>,  // 评估维度名列表
    pub threshold: f32,           // 综合分通过阈值
    pub samples: usize,           // 采样次数，默认 3
}

/// 解析 Judge 输出（与 parse_evaluation_result 同构的鲁棒解析）
pub fn parse_judge_verdict(content: &str) -> Result<JudgeVerdict, String>;
```

__非确定性治理__：

- Judge 模型与被测模型__必须不同源__（场景文件中独立声明 judge provider），避免自我偏好。
- 每个断言采样 `samples` 次（默认 3），多数投票决定 pass/fail。
- 票数分裂（如 2:1）或任一次 `confidence < 0.8` → 自动降级为人工待审，不强行裁决。
- Judge 请求 `temperature = 0`。
- 复用 `parse_evaluation_result` 的 markdown code block 提取模式，容忍 LLM 输出包裹格式。

__Prompt 构建__：新增 `build_judge_prompt`（放 `src/llm/` 下与
`summarization_prompt.rs` 并列的 `judge_prompt.rs`），输入为场景描述 + 用户输入 +
Agent 最终输出 + 工具调用摘要，输出要求 JSON 格式的 `JudgeVerdict`。

### 6.3 第 3 级：人工判断（human-in-the-loop）

__A. 金标准快照（golden set）__

- 场景首次运行通过（代码断言 + Judge + 人工确认）后，输出存为
  `tests/scenarios/golden/<scenario>.md`（含输入、输出、工具序列、Judge 裁决）。
- 后续运行自动 diff：结构差异（工具序列变化）报告 + 语义差异交 Judge 复核。
- 金标准更新必须显式 `--bless`（类似 snapshot test），防止静默漂移。

__B. 待审队列（review queue）__

- Judge 低置信、票数分裂、`human_review` 断言、以及运行失败的场景样本，写入
  `tests/scenarios/review-pending/<scenario>-<timestamp>.md`。
- 人工标注 `pass` / `fail` / `partial` 后归档为金标准或记录退化。
- runner 汇总产出趋势报告：哪些场景质量退化、哪些场景长期稳定，为场景集增删提供依据。

### 6.4 判断降级链

```text
代码断言（确定） → 通过/失败
Judge（采样投票）→ 高置信通过/失败 → 金标准比对
                  → 低置信/分裂 → 人工待审队列 → 标注沉淀为金标准
```

## 7. 非确定性与成本治理

| 风险 | 对策 |
|------|------|
| LLM 输出不稳定 | 断言用"包含/匹配/判断"而非"相等"；Judge 采样投票；temperature = 0 |
| 网络抖动 | 超时预算 + 有限重试（复用 `error_handling_flow.rs` 已验证的退避语义） |
| 成本失控 | 场景声明 `max_cost_usd` 软预算；runner 每次运行打印 token 消耗与估算成本汇总 |
| 慢响应 | `timeout_secs` wall-clock 超时，超时即 fail 不 hang |
| provider 限流 | 场景间串行 + 间隔节流 |
| 误进 CI | 双重门控：`#[ignore]` + `HARNESS_TEST_REAL_LLM` 环境变量，CI 配置零改动 |

## 8. Mock 基础设施收敛（伴生修复）

当前 `EchoExecutor` 等基础 mock 在约 20 个测试文件重复定义。随本设计一并收敛：

- 新增 `tests/common/mock_executor.rs`，收纳高频 mock：`EchoExecutor`、`MockExecutor`
  （固定文本）、`CannedExecutor`（预设序列）、`CapturingExecutor`（请求捕获）、
  `KindAwareMockExecutor`（按 `request_kind` 分发，统一覆盖 Brain/Summarization/Evaluation
  场景）。
- 各测试文件删除本地定义改用共享版本，行为以共享实现为准。
- 收敛原则：只收敛__跨文件重复__的定义，单场景专用 mock（如 `InfiniteToolCallExecutor`）
  留在原文件。

## 9. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `src/llm/genai.rs` | 修改 | 补适配层无网络单元测试（消息转换、tool_calls 解析、sanitize 往返） |
| `src/domain/evaluation.rs` | 修改 | 新增 Judge 数据结构与 `parse_judge_verdict` |
| `src/llm/judge_prompt.rs` | 新增 | `build_judge_prompt`（场景 + 输出 + 工具摘要 → Judge prompt） |
| `tests/common/mock_executor.rs` | 新增 | 共享 mock executor 集 |
| `tests/common/mod.rs` | 修改 | 导出 `mock_executor` 子模块 |
| `tests/real_llm_smoke.rs` | 新增 | Layer 1 冒烟测试（`#[ignore]` 门控） |
| `tests/real_llm_scenarios.rs` | 新增 | Layer 2 场景 runner（`#[ignore]` 门控） |
| `tests/scenarios/*.toml` | 新增 | 首批场景文件（3-5 个：shell 统计、多轮上下文、摘要压缩） |
| `tests/scenarios/golden/` | 新增（目录） | 金标准快照 |
| `tests/scenarios/review-pending/` | 新增（目录） | 人工待审队列（gitignore 运行时产物，保留示例说明） |
| `tests/scenarios/reports/` | 新增（目录，gitignore） | 运行报告产出 |
| `docs/current-state.md` | 修改 | 登记测试分层能力 |
| `docs/configuration.md` | 修改 | 登记 `HARNESS_TEST_REAL_LLM` 等测试环境变量 |
| `.gitignore` | 修改 | 忽略 `tests/scenarios/reports/`、`tests/scenarios/review-pending/` 运行时产物 |

## 10. 落地路径

按依赖顺序拆为 3 个 PR：

1. __PR 1：测试基础设施收敛与适配层单元测试__
   - `tests/common/mock_executor.rs` 共享 mock + 各文件迁移。
   - `genai.rs` 纯单元测试（无网络，进 CI）。
   - 风险最低，先行合入。
2. __PR 2：Layer 1 冒烟测试__
   - `tests/real_llm_smoke.rs` + 门控 + provider 矩阵。
   - 文档同步（`configuration.md`、`current-state.md`）。
3. __PR 3：Layer 2/3 场景框架与 Judge__
   - Judge 数据结构与 prompt 构建（含单元测试：`parse_judge_verdict` 鲁棒性、
     `build_judge_prompt` 内容）。
   - 场景 runner + 断言引擎 + 报告产出。
   - 首批场景文件 + 金标准目录 + 待审队列。

## 11. 验证方案

- __PR 1__：现有 342 个集成测试全绿（mock 收敛不改变行为）；新增 genai 单元测试进 CI。
- __PR 2__：无环境变量时测试自动 skip 且不 fail；配置真实 key 手动执行
  `cargo test --test real_llm_smoke -- --ignored` 全部通过；四个 provider kind 至少
  各验证一次。
- __PR 3__：场景框架自身用 mock executor 跑通（框架正确性不依赖真实 API，可进 CI 的
  非门控冒烟子集）；真实场景手动执行产出报告；`parse_judge_verdict` 单元测试覆盖
  纯 JSON、markdown 包裹、非法输入。
- __文档验证__：`markdownlint` 与 `cargo fmt` / `clippy` / `test` 全部通过。

## 12. 风险与边界

- __Judge 判断质量依赖 prompt 与模型__：金标准集合是校准手段；rubric 措辞随场景演进，
  初期接受"低置信转人工"比例偏高，通过待审标注逐步收敛。
- __真实 API 测试天然非确定__：本设计不追求其稳定复现，定位是"手动回归 + 趋势监控"，
  不作为合并门禁。
- __成本__：单次全场景运行预算控制在美元级（首批 3-5 个场景、Judge 采样 3 次），
  报告中显式汇总；后续场景增长时按需提高采样或调整频率。
- __Evaluation 下游分支覆盖缺口__（1.2 节第 4 行）不纳入本设计，作为独立任务跟进。
- __`build_harness_app` 真实配置路径测试__（真实 `agents.toml` 解析）属配置加载范畴，
  同样作为独立任务，不与真实 API 测试耦合。
