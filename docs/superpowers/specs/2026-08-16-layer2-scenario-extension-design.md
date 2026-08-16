# Layer 2 场景扩展：多轮上下文与摘要压缩

> __状态：当前有效__

| 属性 | 值 |
|------|-----|
| 创建日期 | 2026-08-16 |
| 实施状态 | 待实施 |
| 相关文档 | `docs/design/2026-08-16-real-llm-scenario-testing-design.md`、`tests/scenarios/README.md` |

## 1. 背景与目标

### 1.1 缺口

对照场景测试设计文档（§9 文件变更清单），首批场景承诺 3-5 个（shell 统计、多轮上下文、
摘要压缩），当前仅有 2 个（`echo_report`、`shell_stat_task`）。缺失的两个场景共同受阻于
框架能力：`ScenarioSpec.input` 是单个 String，runner 只 spawn 单 Task 单轮输入，无
follow-up 注入通道。

### 1.2 本批范围（已确认）

1. 框架扩展：多轮输入注入（follow_ups）+ 场景级压缩阈值覆写
2. 新增场景：`multi_turn_context`（多轮上下文保留）、`memory_compression`（摘要压缩触发）

范围收敛记录（讨论决策）：

- 多采样投票行使（samples=3）：__跳过__
- 金标准漂移分支 mock 单测：__不纳入本批__
- 任务分解 e2e、失败/重试路径、趋势报告、成本估算、Judge temperature：超出最小闭环，
  作为后续独立任务

### 1.3 目标

- 兑现场有设计承诺的首批场景集合
- 两个新场景均不改动生产代码：多轮注入复用 `ExternalInput` 通道，压缩触发覆写
  `MemoryConfig` Resource

## 2. 方案取舍

- __选定方案 A__：follow_ups 注入 + 场景级 `MemoryConfig` 覆写（详见第 3 节）
- 否决方案 B（仅加场景不动框架）：单条超长 input 触发压缩成本高且"多轮上下文"无法覆盖
- 否决方案 C（多 Task 会话语义）：覆盖跨 Task 记忆，但 `DefaultContributionPolicy` 默认
  Drop，LTM 无可验语义，YAGNI

## 3. 框架设计

### 3.1 TOML schema 扩展

```toml
[scenario]
name = "..."
input = "第一轮输入"
follow_ups = ["追问一", "追问二"]       # 新增，默认 []，现有场景零改动
compression_threshold_tokens = 300     # 新增，可选；设置时 runner 覆写 MemoryConfig
```

`ScenarioSpec` 保持 `deny_unknown_fields`，新增字段向后兼容。

### 3.2 多轮注入时序

```text
spawn 首轮 Task（现有流程）
  → 轮询至稳定态（所有 Task 终态 且 无 in-flight WorkItem）
  → 取下一条 follow_up，经保留的 input_tx 发送
    ExternalInput::TextWithChannel { channel: 同 scenario_channel(), content }
  → 轮询至稳定态 …… 直到 follow_ups 耗尽
  → 收集 trace
```

- runner 当前丢弃 `input_tx`（`let (_input_tx, input_rx) = unbounded()`），改为保留并持有
- follow-up 经 ingress 系统（`src/systems/ingress.rs:49`）转 `Signal::user_input_with_channel`，
  走生产信号路由链路；续轮挂到原 Task 还是开新 Task 由现有路由逻辑决定——
  这正是 `multi_turn_context` 场景要验证的真实行为
- trace 收集从"单 Task"改为覆盖全部 Task：`final_output` 取最后完成 Task 的
  `result_summary`
- `timeout_secs` 语义明确为整个场景（含所有轮）的 wall-clock 预算

### 3.3 压缩触发

场景声明 `compression_threshold_tokens` 时，runner 在 `build_harness_app` 后：

```rust
app.insert_resource(MemoryConfig {
    compression_threshold_tokens: spec.compression_threshold_tokens,
    ..Default::default()
});
```

`MemoryConfig` 默认值（`src/app/mod.rs:270`）：阈值 8000、保留最近 2 轮、摘要目标
1000 token。`preserve_recent_turns = 2` 意味着可压缩条目需轮数 > 2，压缩场景设计为
4-5 轮。

### 3.4 新断言类型 `summarization_triggered`

- 判断依据：`WorkItemType::Summarization` 的 WorkItem 到达过终态 Completed
- 可选 `min_times`（默认 1）
- 属确定性代码断言，失败直接 fail
- 为此 `RunTrace` 扩展：记录终态 WorkItem 的 `(work_type, status)`（当前仅记录
  status 字符串）
- 压缩后链路仍正常由 `state_reached: Completed` 兜底——压缩失败会阻塞任务完成，可观察

## 4. 场景定义

### 4.1 `multi_turn_context.toml`

```toml
[scenario]
name = "multi_turn_context"
description = "跨轮次上下文保留：第 1 轮告知事实，第 2 轮追问验证"
input = "请记住：本次会话的项目代号是 Falcon。回复确认即可。"
follow_ups = ["请问本次会话的项目代号是什么？"]
max_cost_usd = 0.10
timeout_secs = 120

[[assertions]]
type = "state_reached"
workitem_status = "Completed"

[[assertions]]
type = "response_matches"
pattern = "Falcon"
desc = "最终输出包含项目代号"

[[assertions]]
type = "llm_judge"
rubric = "第二轮回答是否基于第一轮告知的上下文（项目代号 Falcon），而非编造或遗忘"
threshold = 0.7
```

### 4.2 `memory_compression.toml`

```toml
[scenario]
name = "memory_compression"
description = "低阈值多轮触发摘要压缩，压缩后任务仍正常完成"
input = """介绍一下 Aurora 项目背景并逐条确认：Aurora 是内部数据平台重构项目，
2026 年 Q1 立项，目标是将离线批处理延迟从小时级降到分钟级；分三个里程碑：
3 月完成存储迁移（Iceberg），6 月完成计算引擎切换（Flink），9 月全量流量切灰度；
团队 12 人，后端 8 人、数据 3 人、PM 1 人；技术栈 Rust + Flink + Iceberg + Kafka；
年度预算 300 万。"""
follow_ups = [
    "详细展开存储迁移里程碑：Iceberg 表设计、分区策略、小文件治理方案，逐项说明",
    "详细展开计算引擎切换：Flink 作业拓扑、状态管理、 Exactly-Once 语义如何保证",
    "详细展开全量流量切灰度：灰度分层、回滚预案、监控告警指标，逐项说明",
    "基于以上所有讨论，用一段话完整总结 Aurora 项目的目标、三个里程碑与团队构成",
]
compression_threshold_tokens = 300
max_cost_usd = 0.20
timeout_secs = 180

[[assertions]]
type = "summarization_triggered"

[[assertions]]
type = "state_reached"
workitem_status = "Completed"

[[assertions]]
type = "llm_judge"
rubric = "压缩发生后最终回答是否仍连贯正确、未丢失关键上下文"
threshold = 0.7
```

场景内容设计原则：每轮事实性强、回答篇幅长（详细展开类问题），保证低阈值下
跨轮稳定累积可压缩条目；最后一轮要求总结，供 Judge 判断压缩后关键上下文是否保留。

## 5. 测试与验证

### 5.1 mock 自检（进 CI，非门控）

- `scenario_framework_mock_smoke_multi_turn`：`CannedExecutor` 每轮固定回复含
  "Falcon"，断言两轮全部 Done、final_output 正确——守护 runner 多轮注入循环本身
- `scenario_framework_mock_smoke_compression`：自检 executor 按 `request_kind` 分发
  （Summarization 请求返回 canned 摘要文本，其余返回长文本），低阈值下断言
  Summarization WorkItem Completed 且 Task Done
- 真实模式走既有 `#[ignore]` 入口，`list_scenario_files` 自动发现新场景

### 5.2 验证清单

- `cargo fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo test --all-features` 全绿
- 真实 API 手动运行一次，产出报告；人工确认两个新场景语义断言后沉淀金标准
  （工具集合粒度，沿用现有 `apply_golden` 机制）

### 5.3 文档同步

- `tests/scenarios/README.md`：登记 `follow_ups`、`compression_threshold_tokens` 字段与
  `summarization_triggered` 断言类型
- `docs/current-state.md`：能力状态登记首批场景集合补齐

## 6. 文件变更清单

| 文件 | 变更类型 | 说明 |
|------|---------|------|
| `tests/real_llm_scenarios.rs` | 修改 | follow_ups 注入、MemoryConfig 覆写、RunTrace 扩展、新断言、mock 自检 |
| `tests/scenarios/multi_turn_context.toml` | 新增 | 多轮上下文场景 |
| `tests/scenarios/memory_compression.toml` | 新增 | 摘要压缩场景 |
| `tests/scenarios/README.md` | 修改 | 新字段与新断言类型说明 |
| `docs/current-state.md` | 修改 | 能力状态登记 |

## 7. 边界与风险

- __续轮语义依赖路由__：follow-up 挂原 Task 还是新 Task 由路由决定；若实现时发现
  同 ChannelId 续轮无法保留上下文（新 Task 无 STM 继承），该发现本身即为场景价值，
  处理方式回到设计评审
- __压缩触发时机不确定__：`memory_compression_system` 每帧轮询，低阈值 + 多轮长文本
  可稳定触发；mock 自检验证框架侧链路
- __金标准沉淀__：新场景首次运行自动创建金标准（现有机制），人工确认后才算数
