# Layer 2 场景扩展（多轮上下文与摘要压缩）实现计划

> __面向 AI 代理的工作者：__ 必需子技能：使用 superpowers:subagent-driven-development（推荐）
> 或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

__目标：__ 扩展 Layer 2 场景 runner 支持多轮输入注入与场景级压缩阈值覆写，
新增 `multi_turn_context` 与 `memory_compression` 两个场景及配套 mock 自检。

__架构：__ 全部改动落在测试侧（`tests/real_llm_scenarios.rs` + 场景 TOML），复用生产链路：
follow-up 经 `ExternalInput::TextWithChannel` → ingress → routing 续轮；压缩经覆写
`MemoryConfig` Resource 触发 `memory_compression_system`。不改动任何生产代码。

__技术栈：__ Rust、Bevy ECS、TOML（serde 反序列化）、crossbeam-channel。

__规格：__ `docs/superpowers/specs/2026-08-16-layer2-scenario-extension-design.md`

---

## 实现前已确认机制（零上下文工程师必读）

以下机制已在源码中逐条验证，计划所有设计决策基于这些事实。引用行号为当前 `main` 分支状态。

1. __multi_turn 任务生命周期__：`Task::from_user_input`（生产路径）构造
   `multi_turn: true` 的任务，LLM 文本回复后任务转 `Waiting(WaitingReason::User)`
   而非 Done，且回复内容写入 `task.input_summary`
   （`src/systems/transform/llm_response.rs:1089-1106`）。
2. __follow-up 续轮路由__：`routing_system` 查找同 `origin_channel` 且处于
   `Waiting(User)` 的任务，产出 `ContinueTaskMessage` 续轮——追加用户输入到 STM、
   任务转 Ready、复用上一轮 delegate（`src/systems/routing.rs:130-157`、
   `continue_task_system`）。若不存在 `Waiting(User)` 任务，
   则创建__全新 Task（全新 STM，上下文丢失）__（`src/systems/routing.rs:158-173`）。
3. __现有 runner 是单轮语义__：`Task::from_user_input_ready` 构造
   `multi_turn: false`（`src/domain/task.rs:252`），文本回复后直接 Done，
   `result_summary` 为回复内容——这就是现有 mock 测试能到 Done 的原因。
4. __/finish 收尾__：`/finish` 命令（同 channel）→ `FinishTaskMessage` →
   `mark_done("finished by user")` → Done，但会把 `result_summary`
   __覆盖__为 `"finished by user"`（`src/systems/command.rs:93-113`、
   `src/domain/task.rs:344`）。因此多轮场景的最终输出必须取 `task.input_summary`，
   不能取 `result_summary`。
5. __压缩触发__：`memory_compression_system` 只跳过终态任务和
   `Waiting(Summarization)` 任务，`Waiting(User)` __不跳过__
   （`src/systems/memory.rs:22-31`）；条件为
   `short_term.estimated_tokens > config.compression_threshold_tokens`。
   `MemoryConfig` 默认阈值 8000、保留最近 2 轮（`src/app/mod.rs:270-277`）。
6. __Summarization 请求通道__：Summarization WorkItem 派发的 LLM 请求
   `request_kind = AgentRequestKind::Summarization`
   （`src/systems/dispatch/dispatch_system.rs:259-262`）——mock executor
   按此分发 canned 摘要。
7. __测试侧类型可达性__：`harness::MemoryConfig`、`harness::ExternalInput`、
   `harness::TaskStatus`、`harness::WaitingReason`、`harness::WorkItemType`、
   `harness::WorkItemStatus` 均经 `src/lib.rs` 的 `pub use app::*` /
   `pub use domain::*` 从 crate 根导出，集成测试直接 `use` 即可。
8. __已知边界（实现时观察）__：Summarization WorkItem 完成后任务回填的目标状态
   （回 Running 还是回 `Waiting(User)`）决定稳定态轮询是否会卡住——任务 5 的
   mock 测试会揭示真实流转，若卡住按实际状态机调整稳定态条件
   （见任务 3 步骤 4 的注释）。
9. __实施后确认的源码事实（任务 5 实现中验证，取代第 8 条的猜测）__：
   - Summarization WorkItem 派发按 `work_type.required_tag()`（= "summarization"）
     查找 Persistent Agent（`src/systems/dispatch/dispatch_system.rs` L224-248），
     缺失即 `work_item.fail()`；场景 runner 的单 agent 必须补 `"summarization"`
     tag（生产由独立 summarizer agent 承担）。
   - 生产 `handle_summarization_work_item_result`（`src/systems/transform/
     llm_response.rs` L507-661）完成 Summarization 时 __从不调用 `complete()`__
     （保持 Running）且末尾直接 despawn WorkItem——因此 Summarization 从不会
     处于 Completed 终态，`RunTrace.summarization_completed` 无法用终态集合统计，
     任务 5 改用 `Added<WorkItem>` + `work_type == Summarization` 计数"压缩触发"。
   - 稳定态条件 __无需放宽__：修复 agent tag 后，Summarization 完成将任务回填
     `Waiting(User)`，`scenario_settled` 正常达成。

## 文件结构

| 文件 | 变更 | 职责 |
|------|------|------|
| `tests/real_llm_scenarios.rs` | 修改 | schema 扩展、RunTrace 扩展、新断言、多轮注入循环、MemoryConfig 覆写、3 个新 mock 测试 |
| `tests/scenarios/multi_turn_context.toml` | 新增 | 多轮上下文场景定义 |
| `tests/scenarios/memory_compression.toml` | 新增 | 摘要压缩场景定义 |
| `tests/scenarios/README.md` | 修改 | 登记新字段与新断言类型 |
| `docs/current-state.md` | 修改 | 能力状态登记 |

工作分支：`fix/real-llm-scenario-runner`（规格已在此分支）。全程不修改 `src/` 下任何文件。

---

### 任务 1：TOML schema 扩展

__文件：__

- 修改：`tests/real_llm_scenarios.rs`（`ScenarioSpec` 定义，当前 L56-66）
- 测试：同文件新增解析测试

- [x] __步骤 1：编写失败的测试__

在 `tests/real_llm_scenarios.rs` 测试区（`scenario_assertion_engine_branches` 之后）新增：

```rust
/// 新字段解析：follow_ups 与 compression_threshold_tokens。
#[test]
fn scenario_spec_parses_follow_ups_and_threshold() {
    let content = r#"
[scenario]
name = "x"
description = "d"
input = "第一轮"
follow_ups = ["第二轮", "第三轮"]
compression_threshold_tokens = 300

[[assertions]]
type = "state_reached"
workitem_status = "Completed"
"#;
    let file: ScenarioFile = toml::from_str(content).expect("解析应成功");
    assert_eq!(file.scenario.follow_ups.len(), 2);
    assert_eq!(file.scenario.compression_threshold_tokens, Some(300));
}

/// 向后兼容：旧场景文件（无新字段）解析后 follow_ups 为空、阈值为 None。
#[test]
fn scenario_spec_defaults_backward_compatible() {
    let content = r#"
[scenario]
name = "x"
description = "d"
input = "第一轮"
"#;
    let file: ScenarioFile = toml::from_str(content).expect("解析应成功");
    assert!(file.scenario.follow_ups.is_empty());
    assert_eq!(file.scenario.compression_threshold_tokens, None);
}
```

- [x] __步骤 2：运行测试验证失败__

```bash
cargo test --test real_llm_scenarios scenario_spec_parses
```

预期：编译失败，报 `error[E0609]: no field 'follow_ups' on type 'ScenarioSpec'`（`deny_unknown_fields` 同时拦截 TOML 字段）。

- [x] __步骤 3：扩展 ScenarioSpec__

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioSpec {
    name: String,
    description: String,
    input: String,
    /// 后续轮次输入（多轮场景）：每条经生产 ingress → routing 续轮链路注入，
    /// 挂回同一 Task（STM 保留）；空列表 = 单轮场景（现有行为不变）。
    #[serde(default)]
    follow_ups: Vec<String>,
    /// 场景级压缩阈值覆写：Some 时 runner 覆写 MemoryConfig，
    /// 让多轮长文本稳定触发 memory_compression_system。
    #[serde(default)]
    compression_threshold_tokens: Option<u32>,
    #[serde(default = "default_max_cost_usd")]
    max_cost_usd: f32,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}
```

同时给文件内现有 `ScenarioSpec` 字面量构造补上 `follow_ups: vec![]` 与
`compression_threshold_tokens: None`——本任务只需修
`scenario_assertion_engine_branches` 一处（编译器会指出）。

- [x] __步骤 4：运行测试验证通过__

```bash
cargo test --test real_llm_scenarios scenario_spec
```

预期：2 个新测试 PASS，`scenario_assertion_engine_branches` 编译修复后仍 PASS。

- [x] __步骤 5：Commit__

```bash
git add tests/real_llm_scenarios.rs
git commit -m "test: 场景 schema 支持 follow_ups 与压缩阈值字段"
```

---

### 任务 2：RunTrace 扩展与 summarization_triggered 断言

__文件：__

- 修改：`tests/real_llm_scenarios.rs`（`RunTrace` L163-179、`AssertionSpec` L76-114、`check_assertions` L511-623）
- 测试：同文件新增断言分支测试

- [x] __步骤 1：编写失败的测试__

```rust
/// summarization_triggered 断言分支：达标 PASS、未达标 FAIL。
#[test]
fn scenario_assertion_summarization_triggered_branches() {
    let spec = ScenarioSpec {
        name: "selfcheck".into(),
        description: "d".into(),
        input: "x".into(),
        follow_ups: vec![],
        compression_threshold_tokens: None,
        max_cost_usd: 0.0,
        timeout_secs: 1,
    };
    let mut trace = RunTrace {
        task_status: Some("Done".into()),
        final_output: Some("汇总完成".into()),
        tool_calls: vec![],
        workitem_statuses: vec!["Completed".into()],
        summarization_completed: 1,
        llm_calls: 1,
        judge_calls: 0,
        elapsed_ms: 0,
    };
    let assertions = vec![
        AssertionSpec::SummarizationTriggered { min_times: 1 },
        AssertionSpec::SummarizationTriggered { min_times: 2 },
    ];
    let (results, _) = check_assertions(&spec, &trace, &assertions, None);
    let labels: Vec<&str> = results.iter().map(|(_, _, o)| o.label()).collect();
    assert_eq!(labels, vec!["PASS", "FAIL"]);

    trace.summarization_completed = 0;
    let (results, _) = check_assertions(&spec, &trace, &assertions, None);
    let labels: Vec<&str> = results.iter().map(|(_, _, o)| o.label()).collect();
    assert_eq!(labels, vec!["FAIL", "FAIL"]);
}
```

注意：`RunTrace` 加字段后，现有 `scenario_assertion_engine_branches` 测试的字面量构造也要补 `summarization_completed: 0`。

- [x] __步骤 2：运行测试验证失败__

```bash
cargo test --test real_llm_scenarios scenario_assertion_summarization
```

预期：编译失败，`no field 'summarization_completed'` / `no variant SummarizationTriggered`。

- [x] __步骤 3：实现__

`RunTrace` 增加字段：

```rust
/// Summarization WorkItem 到达 Completed 的次数（summarization_triggered 断言依据）
summarization_completed: usize,
```

`AssertionSpec` 增加变体：

```rust
/// Summarization WorkItem 完成次数 >= min_times（代码断言，设计 §3.4）
#[serde(rename = "summarization_triggered")]
SummarizationTriggered {
    #[serde(default = "default_one")]
    min_times: usize,
},
```

`describe()` 增加分支：

```rust
Self::SummarizationTriggered { min_times } => {
    format!("summarization_triggered: × >= {min_times}")
}
```

`kind()` 增加分支：

```rust
Self::SummarizationTriggered { .. } => "summarization_triggered",
```

`check_assertions` 的 match 增加分支（放在 `HumanReview` 之前）：

```rust
AssertionSpec::SummarizationTriggered { min_times } => {
    let count = trace.summarization_completed;
    if count >= *min_times {
        AssertionOutcome::Pass
    } else {
        AssertionOutcome::Fail(format!(
            "Summarization 完成 {count} 次，期望 >= {min_times}"
        ))
    }
}
```

- [x] __步骤 4：运行测试验证通过__

```bash
cargo test --test real_llm_scenarios scenario_assertion
```

预期：全部 PASS。

- [x] __步骤 5：Commit__

```bash
git add tests/real_llm_scenarios.rs
git commit -m "test: 新增 summarization_triggered 确定性断言"
```

---

### 任务 3：多轮注入循环、压缩阈值覆写与 trace 收集

__文件：__

- 修改：`tests/real_llm_scenarios.rs`（`execute_scenario` L334-447、use 列表 L22-42）
- 测试：本任务改造运行时骨架，行为验证在任务 4/5 的场景 mock 测试（跨任务依赖，本任务以编译 + 现有 3 个 mock 测试不回归为准）

- [x] __步骤 1：扩展 use 导入__

在现有 `use harness::{...}` 列表（第二个 use 块）中追加：

```rust
ExternalInput, MemoryConfig, TaskStatus, WaitingReason, WorkItemStatus, WorkItemType,
```

（均在 crate 根导出，见"实现前已确认机制"第 7 条。）

- [x] __步骤 2：保留 input_tx 并覆写 MemoryConfig__

`execute_scenario` 开头（L341）：

```rust
// 改动前：let (_input_tx, input_rx) = unbounded();
let (input_tx, input_rx) = unbounded();
let mut app = build_harness_app(
    scenario_config(),
    runtime,
    registry,
    input_rx,
    vec![],
    harness::channels::ChannelManager::empty().0,
);

// 场景级压缩阈值覆写（仅测试侧注入，不触生产代码）
if let Some(threshold) = spec.compression_threshold_tokens {
    app.insert_resource(MemoryConfig {
        compression_threshold_tokens: threshold,
        ..Default::default()
    });
}
```

- [x] __步骤 3：Task 构造置 multi_turn__

```rust
// 改动前：let task = Task::from_user_input_ready(&spec.input, 3, scenario_channel());
let mut task = Task::from_user_input_ready(&spec.input, 3, scenario_channel());
// 多轮场景置 multi_turn：LLM 文本回复后任务转 Waiting(User) 等续轮，
// follow-up 才能经 routing continue_existing 挂回同一 Task（STM 保留）；
// 单轮场景保持 false，文本回复后直接 Done（现有行为不变）。
task.multi_turn = !spec.follow_ups.is_empty();
```

- [x] __步骤 4：新增稳定态轮询函数__

在 `execute_scenario` 之前新增两个函数：

```rust
/// 场景稳定态：所有 Task 处于终态或 Waiting(User)（多轮等待续轮），
/// 且所有 WorkItem 到达终态（无 in-flight Summarization 等）。
///
/// 注意：若实现中发现 Summarization 完成后任务停留态导致该条件永不满足
/// （见"实现前已确认机制"第 8 条），按实际状态机放宽 Task 侧条件，
/// WorkItem 侧"全部终态"必须保留（防 follow-up 在压缩 in-flight 时注入
/// 被 routing 判为无 Waiting(User) 任务而开新 Task 丢上下文）。
fn scenario_settled(app: &mut bevy_app::App) -> bool {
    let world = app.world_mut();
    let mut task_query = world.query::<&Task>();
    let tasks_settled = task_query.iter(world).all(|t| {
        t.status.is_terminal()
            || matches!(t.status, TaskStatus::Waiting(WaitingReason::User))
    });
    if !tasks_settled {
        return false;
    }
    let mut wi_query = world.query::<&WorkItem>();
    wi_query.iter(world).all(|wi| wi.is_terminal())
}

/// 轮询至稳定态；返回是否在整体超时前到达。
fn wait_until_settled(
    app: &mut bevy_app::App,
    start: &Instant,
    timeout: Duration,
    poll_ms: u64,
) -> bool {
    loop {
        app.update();
        if scenario_settled(app) {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(poll_ms));
    }
}
```

- [x] __步骤 5：替换主轮询循环为多轮注入循环__

将现有 L396-412 的单一终态轮询循环替换为：

```rust
let poll_ms: u64 = std::env::var("HARNESS_TEST_SCENARIO_POLL_MS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(50);
let timeout = Duration::from_secs(spec.timeout_secs);
let channel = scenario_channel();

// 多轮场景：逐条 follow-up 经生产 ingress → routing 续轮链路注入。
// 每条前置等待稳定态，确保上一轮回复完成且无 in-flight Summarization。
for follow_up in &spec.follow_ups {
    if !wait_until_settled(&mut app, &start, timeout, poll_ms) {
        break; // 超时，交由终态轮询统一判定
    }
    if input_tx
        .send(ExternalInput::TextWithChannel {
            channel: channel.clone(),
            content: follow_up.clone(),
        })
        .is_err()
    {
        break;
    }
}

// 多轮任务停在 Waiting(User) 不会自行 Done：
// 注入 /finish 命令走生产命令链路收尾（command → FinishTaskMessage → Done）。
if !spec.follow_ups.is_empty() && wait_until_settled(&mut app, &start, timeout, poll_ms) {
    let _ = input_tx.send(ExternalInput::TextWithChannel {
        channel: channel.clone(),
        content: "/finish".to_string(),
    });
}

// 终态等待：所有 Task 到达 terminal 或整体超时
loop {
    app.update();
    let all_terminal = {
        let world = app.world_mut();
        let mut query = world.query::<&Task>();
        !query.iter(world).any(|t| !t.status.is_terminal())
    };
    if all_terminal {
        break;
    }
    if start.elapsed() > timeout {
        break;
    }
    thread::sleep(Duration::from_millis(poll_ms));
}
```

- [x] __步骤 6：trace 收集改造__

Task 收集（现 L422-428）改为：

```rust
for task in task_query.iter(world) {
    if trace.task_status.is_none() {
        trace.task_status = Some(format!("{:?}", task.status));
        // 多轮：/finish 的 mark_done 会把 result_summary 覆盖为
        // "finished by user"，最后一轮真实回复在 input_summary
        // （llm_response multi_turn 分支写入）；单轮：result_summary 即回复。
        trace.final_output = Some(if task.multi_turn {
            task.input_summary.clone()
        } else {
            task.result_summary.clone()
        });
    }
}
```

WorkItem 收集（现 L429-434）改为：

```rust
for wi in wi_query.iter(world) {
    if wi.is_terminal() {
        trace.workitem_statuses.push(format!("{:?}", wi.status));
        if wi.work_type == WorkItemType::Summarization
            && matches!(wi.status, WorkItemStatus::Completed)
        {
            trace.summarization_completed += 1;
        }
    }
}
```

- [x] __步骤 7：验证现有测试不回归__

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --test real_llm_scenarios
```

预期：现有 3 个 mock 测试（`scenario_framework_mock_smoke_echo_report`、
`scenario_tool_call_loop_reaches_done`、`scenario_assertion_engine_branches`）
以及任务 1/2 新测试全 PASS（单轮路径 `follow_ups` 为空，行为不变）。

- [x] __步骤 8：Commit__

```bash
git add tests/real_llm_scenarios.rs
git commit -m "test: runner 支持多轮注入循环与场景级压缩阈值覆写"
```

---

### 任务 4：multi_turn_context 场景与 mock 自检

__文件：__

- 创建：`tests/scenarios/multi_turn_context.toml`
- 修改：`tests/real_llm_scenarios.rs`（新增 mock 测试）

- [x] __步骤 1：创建场景文件__

```toml
# 场景：多轮上下文保留（设计 §4.1）
# 第 1 轮告知事实 → 任务转 Waiting(User) → 第 2 轮追问验证上下文保留
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
samples = 1
```

- [x] __步骤 2：编写失败的 mock 测试__

```rust
/// Mock 模式多轮注入回归：守护 runner 的 follow-up 注入走生产 routing 续轮链路
/// （Waiting(User) → ContinueTaskMessage → 同 Task STM 追加）与 /finish 收尾。
/// 回归锚点：若续轮被 routing 判为"无等待任务"而开新 Task，新 Task STM 为空，
/// 最终输出不含 Falcon，本测试失败。
#[test]
fn scenario_framework_mock_smoke_multi_turn() {
    let file = load_scenario("multi_turn_context");
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    // 每轮一次 LlmCompletion：首轮确认、第 2 轮回答代号；多余项为保险
    let executor: Arc<dyn AgentExecutor> = Arc::new(CannedExecutor::new(vec![
        text_output("好的，已记住：项目代号 Falcon。"),
        text_output("本次会话的项目代号是 Falcon。"),
        text_output("（多余保险项）Falcon。"),
    ]));
    // Judge 走独立 selfcheck executor（Evaluation kind → canned 高置信 verdict）
    let judge: Arc<dyn AgentExecutor> = Arc::new(ScenarioSelfcheckExecutor {
        final_text: "unused",
    });
    let tmp = tempfile::tempdir().expect("tempdir");

    let report = run_scenario(&file, executor, judge, runtime, "mock（多轮回归）", tmp.path());

    // /finish 收尾后任务 Done；最终输出取 input_summary（最后一轮真实回复）
    assert_eq!(
        report.trace.task_status.as_deref(),
        Some("Done"),
        "多轮任务应经 /finish 到达 Done: {:?}",
        report.trace.task_status
    );
    assert!(
        report
            .trace
            .final_output
            .as_deref()
            .unwrap_or("")
            .contains("Falcon"),
        "最终输出应包含最后一轮回复内容: {:?}",
        report.trace.final_output
    );
    assert!(report.all_passed, "断言不应 FAIL: {:?}", report.results);
    assert!(!report.needs_human, "不应有待审: {:?}", report.results);
}
```

注：`ScenarioSelfcheckExecutor.final_text` 类型是 `&'static str`，传 `"unused"` 即可（Evaluation 分支不走它）。

- [x] __步骤 3：运行测试验证行为__

```bash
cargo test --test real_llm_scenarios scenario_framework_mock_smoke_multi_turn -- --nocapture
```

预期：PASS。若失败，按失败点排查：

- 卡到超时且 `task_status = "Waiting(User)"`：检查 `/finish` 注入是否执行（稳定态是否达成）；
- `final_output` 为 `"finished by user"`：说明取了 `result_summary`（任务 3 步骤 6 改造遗漏）；
- 新 Task 被创建（`task_status` 非预期）：routing 未续轮，检查 follow-up 与首轮
  `origin_channel` 是否一致（必须同用 `scenario_channel()`）。

- [x] __步骤 4：Commit__

```bash
git add tests/scenarios/multi_turn_context.toml tests/real_llm_scenarios.rs
git commit -m "test: multi_turn_context 场景与多轮注入 mock 自检"
```

---

### 任务 5：memory_compression 场景与 mock 自检

__文件：__

- 创建：`tests/scenarios/memory_compression.toml`
- 修改：`tests/real_llm_scenarios.rs`（新增压缩自检 executor 与 mock 测试）

- [x] __步骤 1：创建场景文件__

```toml
# 场景：摘要压缩触发（设计 §4.2）
# 低阈值 + 多轮长文本：每轮回复累积 STM 超阈值，触发 memory_compression_system；
# 压缩后任务仍正常完成，最终总结轮验证关键上下文保留。
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
    "详细展开计算引擎切换：Flink 作业拓扑、状态管理、Exactly-Once 语义如何保证",
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
type = "response_matches"
pattern = "Aurora"
desc = "最终总结包含项目名"

[[assertions]]
type = "llm_judge"
rubric = "压缩发生后最终回答是否仍连贯正确、未丢失关键上下文（目标、里程碑、团队构成）"
threshold = 0.7
samples = 1
```

- [x] __步骤 2：编写压缩自检 executor 与失败的 mock 测试__

executor（放在 `ScenarioSelfcheckExecutor` 定义之后）：

```rust
/// 压缩场景自检 executor（本文件专用）：
/// - `Summarization`（压缩摘要请求）→ canned 摘要文本
/// - 其他（各轮对话）→ 长文本回复，多轮累积远超低压缩阈值
struct CompressionSelfcheckExecutor;

impl AgentExecutor for CompressionSelfcheckExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        match request.request_kind {
            AgentRequestKind::Summarization => Box::pin(async {
                Ok(text_output(
                    "压缩摘要：Aurora 为内部数据平台重构项目，2026 Q1 立项，\
                     目标离线批处理降至分钟级；里程碑为 3 月 Iceberg 存储迁移、\
                     6 月 Flink 计算引擎切换、9 月全量灰度；团队 12 人。",
                ))
            }),
            _ => Box::pin(async {
                Ok(text_output(
                    "Aurora 项目详述：存储迁移采用 Iceberg 表格式，按天分区，\
                     配合小文件合并任务治理；计算引擎切换使用 Flink SQL 双跑验证，\
                     状态后端 RocksDB，通过两阶段提交保证 Exactly-Once；灰度按\
                     1%、5%、20%、50%、100% 五层放量，每层观察核心延迟与丢失率\
                     指标，异常即回滚至旧链路。团队构成与预算维持既定规划。",
                ))
            }),
        }
    }
}
```

测试：

```rust
/// Mock 模式压缩链路自检：低阈值 + 多轮长文本触发
/// memory_compression_system → Summarization WorkItem 完成，任务经 /finish 到 Done。
#[test]
fn scenario_framework_mock_smoke_compression() {
    let file = load_scenario("memory_compression");
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(CompressionSelfcheckExecutor);
    let judge: Arc<dyn AgentExecutor> = Arc::new(ScenarioSelfcheckExecutor {
        final_text: "unused",
    });
    let tmp = tempfile::tempdir().expect("tempdir");

    let report = run_scenario(&file, executor, judge, runtime, "mock（压缩自检）", tmp.path());

    assert!(
        report.trace.summarization_completed >= 1,
        "应至少完成一次 Summarization: workitems={:?}",
        report.trace.workitem_statuses
    );
    assert_eq!(
        report.trace.task_status.as_deref(),
        Some("Done"),
        "压缩后任务仍应完成: {:?}",
        report.trace.task_status
    );
    assert!(report.all_passed, "断言不应 FAIL: {:?}", report.results);
}
```

- [x] __步骤 3：运行测试验证行为__

```bash
cargo test --test real_llm_scenarios scenario_framework_mock_smoke_compression -- --nocapture
```

预期：PASS。若失败，按失败点排查（对应"实现前已确认机制"第 9 条实际确认的源码事实）：

- 任务卡死/超时且 Summarization 大量 Failed：场景 agent 缺 `"summarization"` tag
  （dispatch_system 按 required_tag 找 agent，缺失即 fail）——给
  `spawn_scenario_agent` 补该 tag（生产由独立 summarizer agent 承担）。
- `summarization_triggers = 0`：终态集合统计恒为 0（生产 Summarization WorkItem
  完成即 despawn 且从不置 Completed）——改用 `Added<WorkItem>` +
  `work_type == Summarization` 计数"压缩触发"，计数系统注册在所有生产系统之后。
- follow-up 后新 Task 被创建：压缩 in-flight 期间注入了 follow-up，
  检查稳定态的 WorkItem 条件（`scenario_settled` 的 WorkItem"全部终态"必须保留）。

- [x] __步骤 4：Commit__

```bash
git add tests/scenarios/memory_compression.toml tests/real_llm_scenarios.rs
git commit -m "test: memory_compression 场景与压缩链路 mock 自检"
```

---

### 任务 6：文档同步与全量验证

__文件：__

- 修改：`tests/scenarios/README.md`
- 修改：`docs/current-state.md`

- [x] __步骤 1：README 登记新字段与新断言__

在 `tests/scenarios/README.md` 的"执行方式"章节之后新增：

```markdown
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
| `summarization_triggered` | 代码 | Summarization WorkItem 完成次数 < `min_times`（默认 1）直接 fail |

多轮场景的 `response_matches` 检查最后一轮回复（非 `/finish` 的收尾文案）。
```

- [x] __步骤 2：current-state.md 登记__

修改 `docs/current-state.md` L76-80 的 Layer 2/3 条目（`#### 测试分层（真实 LLM 场景测试）` 小节内）：

改动前（L76-80）：

```markdown
- Layer 2/3：声明式场景测试框架已可用（`tests/real_llm_scenarios.rs` +
  `tests/scenarios/*.toml`）——TOML 场景定义五类断言
  （`tool_called` / `state_reached` / `response_matches` / `llm_judge` / `human_review`），
  产出 Markdown 报告、待审队列与金标准快照；框架自检（mock executor）随 CI
  常规运行，真实场景手动执行
```

改动后：

```markdown
- Layer 2/3：声明式场景测试框架已可用（`tests/real_llm_scenarios.rs` +
  `tests/scenarios/*.toml`）——TOML 场景定义六类断言
  （`tool_called` / `state_reached` / `response_matches` / `llm_judge` /
  `human_review` / `summarization_triggered`），支持多轮输入注入
  （`follow_ups`，经生产 routing 续轮链路挂回同一 Task）与场景级压缩阈值覆写
  （`compression_threshold_tokens`）；首批场景集合（echo_report、shell_stat_task、
  multi_turn_context、memory_compression）已齐备；产出 Markdown 报告、待审队列
  与金标准快照；框架自检（mock executor）随 CI 常规运行，真实场景手动执行
```

- [x] __步骤 3：全量验证__

```bash
npx markdownlint-cli2
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

预期：全部通过（77 个测试 binary、约 1200+ 测试）。

- [x] __步骤 4：Commit__

```bash
git add tests/scenarios/README.md docs/current-state.md
git commit -m "docs: 登记场景多轮注入与压缩阈值能力"
```

---

## 验收清单（对照规格）

| 规格条目 | 对应任务 |
|---------|---------|
| §3.1 TOML schema（follow_ups / compression_threshold_tokens） | 任务 1 |
| §3.2 多轮注入时序（稳定态 → 注入 → /finish 收尾） | 任务 3 |
| §3.3 压缩阈值覆写（仅测试侧） | 任务 3 步骤 2 |
| §3.4 summarization_triggered 断言 + RunTrace 扩展 | 任务 2 + 任务 3 步骤 6 |
| §4.1 multi_turn_context 场景 | 任务 4 |
| §4.2 memory_compression 场景 | 任务 5 |
| §5.1 两个 mock 自检进 CI | 任务 4/5 |
| §5.3 文档同步 | 任务 6 |
| §7 边界（续轮语义、压缩时机） | 已在"实现前已确认机制"落地，mock 测试锚定 |
