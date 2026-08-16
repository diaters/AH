//! Layer 2 场景测试 runner（设计文档 §5、§6，`docs/design/2026-08-16-real-llm-scenario-testing-design.md`）。
//!
//! 双模式运行：
//!
//! - **Mock 模式**（非门控，进 CI）：`scenario_framework_mock_smoke_*` 用共享框架 +
//!   自检 mock executor 跑 `tests/scenarios/echo_report.toml`，验证
//!   "场景解析 → ECS 执行 → 断言引擎 → Judge 投票 → 报告/金标准产出" 全链路。
//!   框架正确性不依赖真实 API（设计 §11）。
//! - **Real 模式**（`#[ignore]` + `HARNESS_TEST_REAL_LLM` 双重门控）：真实 API 跑
//!   `tests/scenarios/*.toml`，产出报告、待审队列与金标准快照。
//!
//! 断言分级（设计 §6）：
//!
//! | 类型 | 判断者 | 失败行为 |
//! |------|--------|---------|
//! | `tool_called` / `state_reached` / `response_matches` | 代码 | 直接 fail |
//! | `llm_judge` | AI（采样投票） | 低置信/分裂 → 待审队列 |
//! | `human_review` | 人工 | 强制待审队列 |

mod common;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use common::mock_executor::{CannedExecutor, DEFAULT_BRAIN_DECISION_JSON, text_output};
use crossbeam_channel::unbounded;
use harness::prelude::*;
use harness::{
    Agent, AgentCapabilities, AgentExecutor, AgentKind, AgentProfile, AgentToolPermissions,
    ExecutorFuture, ToolPermission,
};
use harness::{
    AgentExecutionRequest, AgentRequestKind, ChannelId, DispatchHint, DispatchKind,
    DispatchStrategy, ExternalInput, FrontendKind, HarnessConfig, JudgeOutcome, JudgePromptData,
    JudgeRubric, JudgeVerdict, JudgeVote, LongTermMemory, MemoryConfig, PendingDispatch,
    ShortTermMemory, Task, TaskStatus, WaitingReason, WorkItem, WorkItemStatus, WorkItemType,
    build_harness_app, build_judge_user_prompt, judge_system_prompt, llm::ExecutorRegistry,
    parse_judge_verdict,
};
use serde::Deserialize;
use tokio::runtime::Runtime;
use uuid::Uuid;

// ============ 场景文件模型（TOML 声明式，设计 §5.1） ============

#[derive(Debug, Deserialize)]
struct ScenarioFile {
    scenario: ScenarioSpec,
    #[serde(default)]
    assertions: Vec<AssertionSpec>,
}

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

fn default_max_cost_usd() -> f32 {
    0.10
}

fn default_timeout_secs() -> u64 {
    120
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AssertionSpec {
    /// 工具被调用至少 min_times 次（代码断言）
    #[serde(rename = "tool_called")]
    ToolCalled {
        tool: String,
        #[serde(default = "default_one")]
        min_times: usize,
    },
    /// 执行单元到达指定状态（代码断言）。
    ///
    /// DirectDelegate 路径 Task 直接完成（无独立 WorkItem），因此
    /// "Completed" 同时接受 `WorkItemStatus::Completed` 与 `TaskStatus::Done`。
    #[serde(rename = "state_reached")]
    StateReached { workitem_status: String },
    /// 最终输出匹配正则（代码断言）
    #[serde(rename = "response_matches")]
    ResponseMatches {
        pattern: String,
        #[serde(default)]
        desc: Option<String>,
    },
    /// LLM-as-Judge 采样投票（AI 断言）
    #[serde(rename = "llm_judge")]
    LlmJudge {
        rubric: String,
        #[serde(default = "default_threshold")]
        threshold: f32,
        #[serde(default = "default_one")]
        samples: usize,
    },
    /// Summarization WorkItem 完成次数 >= min_times（代码断言，设计 §3.4）
    #[serde(rename = "summarization_triggered")]
    SummarizationTriggered {
        #[serde(default = "default_one")]
        min_times: usize,
    },
    /// 强制人工复核（人工断言）
    #[serde(rename = "human_review")]
    HumanReview {
        #[serde(default)]
        note: Option<String>,
    },
}

fn default_one() -> usize {
    1
}

fn default_threshold() -> f32 {
    0.7
}

impl AssertionSpec {
    /// 人读断言描述（用于报告）
    fn describe(&self) -> String {
        match self {
            Self::ToolCalled { tool, min_times } => format!("tool_called: {tool} × >= {min_times}"),
            Self::StateReached { workitem_status } => {
                format!("state_reached: {workitem_status}")
            }
            Self::ResponseMatches { pattern, desc } => {
                let d = desc.clone().unwrap_or_default();
                format!("response_matches: /{pattern}/ {d}")
            }
            Self::LlmJudge {
                rubric,
                threshold,
                samples,
            } => {
                format!("llm_judge (×{samples}, threshold {threshold}): {rubric}")
            }
            Self::SummarizationTriggered { min_times } => {
                format!("summarization_triggered: × >= {min_times}")
            }
            Self::HumanReview { note } => {
                format!("human_review: {}", note.clone().unwrap_or_default())
            }
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::ToolCalled { .. } => "tool_called",
            Self::StateReached { .. } => "state_reached",
            Self::ResponseMatches { .. } => "response_matches",
            Self::LlmJudge { .. } => "llm_judge",
            Self::SummarizationTriggered { .. } => "summarization_triggered",
            Self::HumanReview { .. } => "human_review",
        }
    }
}

// ============ 运行事实与断言结果 ============

/// 场景运行收集的事实（Judge 输入与代码断言依据）
#[derive(Debug, Default, Clone)]
struct RunTrace {
    /// Task 终态（`Done` / `Failed(..)` / 超时标记）
    task_status: Option<String>,
    /// 最终用户可见输出（Task::result_summary）
    final_output: Option<String>,
    /// 工具调用记录（tool_name, input）
    tool_calls: Vec<(String, String)>,
    /// WorkItem 终态集合（DirectDelegate 路径为空）
    workitem_statuses: Vec<String>,
    /// Summarization WorkItem 到达 Completed 的次数（summarization_triggered 断言依据）
    summarization_completed: usize,
    /// LLM 请求次数（被测链路，不含 Judge）
    llm_calls: usize,
    /// Judge 采样次数
    judge_calls: usize,
    /// 运行耗时（毫秒）
    elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq)]
enum AssertionOutcome {
    Pass,
    Fail(String),
    NeedsHuman(String),
}

impl AssertionOutcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail(_) => "FAIL",
            Self::NeedsHuman(_) => "NEEDS_HUMAN",
        }
    }
}

/// 单场景完整报告
#[derive(Debug, Clone)]
struct ScenarioReport {
    name: String,
    mode: String,
    /// 场景声明软预算（美元）；当前 LLM 输出无 usage 字段，仅展示不估算
    declared_budget_usd: f32,
    /// 全部断言均非 Fail
    all_passed: bool,
    /// 存在待人工裁决的断言
    needs_human: bool,
    results: Vec<(String, String, AssertionOutcome)>, // (kind, desc, outcome)
    judge_verdicts: Vec<JudgeVerdict>,
    trace: RunTrace,
    golden_note: String,
}

// ============ 场景自检 mock executor（本文件专用，不进共享库） ============

/// mock 模式专用执行器：
/// - `BrainDecision` → 标准决策 JSON（复用共享常量）
/// - `Evaluation`（Judge 请求）→ 高置信 canned verdict
/// - 其他 → 固定汇报文本（满足 echo_report 的确定性断言）
struct ScenarioSelfcheckExecutor {
    final_text: &'static str,
}

impl AgentExecutor for ScenarioSelfcheckExecutor {
    fn execute(&self, request: AgentExecutionRequest) -> ExecutorFuture {
        match request.request_kind {
            AgentRequestKind::BrainDecision => {
                Box::pin(async { Ok(text_output(DEFAULT_BRAIN_DECISION_JSON)) })
            }
            AgentRequestKind::Evaluation => Box::pin(async {
                // canned Judge verdict：高置信通过，验证投票链路
                Ok(text_output(
                    r#"{"scores":[{"name":"correctness","score":0.95,"rationale":"canned verdict（框架自检）"}],"pass":true,"reasoning":"框架自检 canned 裁决","confidence":0.95}"#,
                ))
            }),
            _ => Box::pin(async { Ok(text_output(self.final_text)) }),
        }
    }
}

// ============ Runner：构建 ECS app 并运行场景 ============

fn scenario_config() -> HarnessConfig {
    HarnessConfig {
        max_retries: 3,
        llm: harness::LlmProviderConfig {
            provider: harness::LlmProviderKind::OpenAi,
            model: "gpt-4.1-mini".to_string(),
            api_key: Some("scenario-test-key".to_string()),
            api_base: None,
        },
        brain: None,
        agents_config_path: "/nonexistent_agents.toml".to_string(),
        default_wait_tasks_timeout_secs: 300,
        max_tool_iterations: 8,
        shell_default_tail_lines: 200,
        shell_max_tail_lines: 500,
        shell_default_exec_timeout_secs: 120,
        shell_default_stop_timeout_secs: 10,
        tool_inflight_timeout_secs: 300,
        shell_max_buffer_bytes_per_stream: 64 * 1024,
        active_poll_ms: 16,
        idle_poll_ms: 150,
        channels: Default::default(),
        channels_config_path: None,
        triggers_config_path: None,
        providers_config_path: "/nonexistent_providers.toml".to_string(),
    }
}

fn scenario_channel() -> ChannelId {
    ChannelId {
        frontend: FrontendKind::Tui,
        user_id: "scenario".to_string(),
        thread_id: None,
    }
}

fn spawn_scenario_agent(app: &mut bevy_app::App) {
    let agent = Agent {
        id: Uuid::new_v4(),
        profile: AgentProfile {
            name: "default-llm-agent".to_string(),
            model: "gpt-4.1-mini".to_string(),
        },
        capabilities: AgentCapabilities {
            tags: vec!["llm".to_string(), "default".to_string()],
            description: "Scenario runner default agent".to_string(),
        },
        kind: AgentKind::Persistent,
        parent_id: None,
        bound_task_id: None,
        // 显式 Allow：场景 runner 无人值守，不能让 Confirm 权限的工具等待用户确认
        // （否则卡死在 Waiting(User)，见 scenario_tool_call_loop_reaches_done 注释）。
        // 同时 default_permission_explicit=true 让 implicit_confirm 回落不生效，
        // effective_permission 稳定返回 Allow。
        tool_permissions: AgentToolPermissions {
            default_permission: ToolPermission::Allow,
            default_permission_explicit: true,
            overrides: std::collections::HashMap::new(),
        },
        system_prompt: None,
    };
    let id = agent.id;
    let entity = app
        .world_mut()
        .spawn((agent, LongTermMemory::default()))
        .id();
    // 与中心 spawn_agent 封装一致：登记 AgentId → Entity。
    // async_tool_dispatch_system / tool_calling_orchestrator_system 均经 EntityIndex
    // 做 O(1) 解析；绕过封装直接 spawn 必须手动补登记，否则索引为空导致解析失败。
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .agents
        .insert(id, entity);
}

/// LLM 请求计数（Added 过滤器，对 mock/real 两种模式一致计数）
#[derive(Resource, Default)]
struct LlmRequestCount(usize);

fn count_llm_requests_system(
    requests: Query<
        &harness::AgentExecutionRequestMessage,
        Added<harness::AgentExecutionRequestMessage>,
    >,
    mut count: ResMut<LlmRequestCount>,
) {
    count.0 += requests.iter().count();
}

/// 场景稳定态：所有 Task 处于终态或 Waiting(User)（多轮等待续轮），
/// 且所有 WorkItem 到达终态（无 in-flight Summarization 等）。
///
/// 注意：若未来实现中 Summarization 完成后任务停留态导致本条件永不满足，
/// 可按实际状态机放宽 Task 侧条件；但 WorkItem 侧"全部终态"必须保留——
/// 否则 follow-up 会在压缩 in-flight 时注入，被 routing 判为无 Waiting(User)
/// 任务而开新 Task，丢失原任务上下文。
fn scenario_settled(app: &mut bevy_app::App) -> bool {
    let world = app.world_mut();
    let mut task_query = world.query::<&Task>();
    let tasks_settled = task_query.iter(world).all(|t| {
        t.status.is_terminal() || matches!(t.status, TaskStatus::Waiting(WaitingReason::User))
    });
    if !tasks_settled {
        return false;
    }
    let mut wi_query = world.query::<&WorkItem>();
    wi_query.iter(world).all(|wi| wi.is_terminal())
}

/// 轮询至稳定态；返回是否在整体超时前到达。
///
/// 注意：每次循环调用 `app.update()` 会推进 ECS 一帧（运行所有 system，
/// 可能产生副作用），调用方需知悉。
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

/// 运行场景：构建完整 ECS app + 注入 executor，轮询至 Task 终态或超时，收集运行事实。
fn execute_scenario(
    spec: &ScenarioSpec,
    executor: Arc<dyn AgentExecutor>,
    runtime: Arc<Runtime>,
) -> RunTrace {
    let start = Instant::now();
    let registry = ExecutorRegistry::from_single_executor(executor, "default");
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

    app.insert_resource(LlmRequestCount::default());
    // 注意：请求实体为帧内瞬态（Dispatch/Transform spawn、Execution 的
    // agent_execution_system 同帧 despawn），从测试侧无法用 `Added` 可靠计数，
    // 报告中的 "LLM 调用" 仅作参考，可能为 0（已知诊断限制，不影响断言）。
    app.add_systems(bevy_app::Update, count_llm_requests_system);

    app.update();
    spawn_scenario_agent(&mut app);

    // DirectDelegate 直派 default-llm-agent：场景聚焦"单轮输入 → 完整工具循环 → 完成"
    // 链路；Brain 调度策略属 Layer 0 既有覆盖（brain_dispatch_flow.rs）。
    let mut task = Task::from_user_input_ready(&spec.input, 3, scenario_channel());
    // 多轮场景置 multi_turn：LLM 文本回复后任务转 Waiting(User) 等续轮，
    // follow-up 才能经 routing continue_existing 挂回同一 Task（STM 保留）；
    // 单轮场景保持 false，文本回复后直接 Done（现有行为不变）。
    task.multi_turn = !spec.follow_ups.is_empty();
    let task_id = task.id;
    let task_entity = app
        .world_mut()
        .spawn((
            task,
            ShortTermMemory::default(),
            PendingDispatch {
                kind: DispatchKind::Task,
                hint: DispatchHint {
                    strategy: DispatchStrategy::DirectDelegate,
                    preferred_agent_name: Some("default-llm-agent".to_string()),
                    required_skill_id: None,
                    agent_spawn_spec: None,
                },
            },
        ))
        .id();
    // 关键：绕开 spawn_task 中心封装直接 spawn，必须手动登记 EntityIndex.tasks。
    // 否则 tool_calling_orchestrator_system 的 index.get_task() 解析失败（返回 None），
    // 会把 task_is_waiting 判为 false 而 skip，工具结果永不汇入 follow-up LLM，
    // 任务卡死在 Waiting(ToolExecution)（shell_stat_task 失败根因，见回归测试
    // scenario_tool_call_loop_reaches_done）。
    app.world_mut()
        .resource_mut::<harness::ecs::EntityIndex>()
        .tasks
        .insert(task_id, task_entity);

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
    // 边界：若此前 wait_until_settled 超时（WorkItem 未终态），/finish 不会注入，
    // 场景会卡在 Waiting(User) 直到整体超时（mock 测试不会触发，真实场景属预期超时风险）。
    if !spec.follow_ups.is_empty()
        && wait_until_settled(&mut app, &start, timeout, poll_ms)
        && input_tx
            .send(ExternalInput::TextWithChannel {
                channel: channel.clone(),
                content: "/finish".to_string(),
            })
            .is_err()
    {
        // 发送失败（receiver 已关闭），交由终态轮询统一判定超时
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

    // 收集运行事实
    let llm_calls = app.world().resource::<LlmRequestCount>().0;
    let mut trace = RunTrace {
        elapsed_ms: start.elapsed().as_millis(),
        llm_calls,
        ..Default::default()
    };
    let world = app.world_mut();
    let mut task_query = world.query::<&Task>();
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
    let mut wi_query = world.query::<&WorkItem>();
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
    // 工具调用记录来自 STM（tool_result_system 写入，稳定来源）
    let mut stm_query = world.query::<&ShortTermMemory>();
    for stm in stm_query.iter(world) {
        for entry in &stm.entries {
            for tc in &entry.metadata.tool_calls {
                trace
                    .tool_calls
                    .push((tc.tool_name.clone(), tc.input.clone()));
            }
        }
    }
    trace
}

// ============ Judge 集成（复用 AgentRequestKind::Evaluation 通道，设计 §6.2） ============

fn build_judge_request(data: &JudgePromptData) -> AgentExecutionRequest {
    AgentExecutionRequest {
        task_id: Uuid::nil(),
        agent_id: Uuid::nil(),
        request_kind: AgentRequestKind::Evaluation,
        prompt: build_judge_user_prompt(data),
        system_prompt: Some(judge_system_prompt()),
        tools: vec![],
        conversation: None,
        work_item_id: None,
        model_override: None,
    }
}

/// Judge prompt 数据从运行事实构建。
///
/// `tool_calls_summary` 由调用方构建并持有（`JudgePromptData` 仅借用）。
fn judge_data_from_trace<'a>(
    spec: &'a ScenarioSpec,
    trace: &'a RunTrace,
    tool_calls_summary: &'a [String],
    rubric: &'a JudgeRubric,
    extra: Option<&'a str>,
) -> JudgePromptData<'a> {
    JudgePromptData {
        scenario_name: &spec.name,
        scenario_description: &spec.description,
        user_input: &spec.input,
        agent_output: trace.final_output.as_deref().unwrap_or("（无输出）"),
        tool_calls_summary,
        rubric,
        extra_instructions: extra,
    }
}

/// 执行一次 Judge 采样（阻塞调用 executor）
fn run_judge_sample(
    executor: &Arc<dyn AgentExecutor>,
    runtime: &Runtime,
    data: &JudgePromptData,
) -> Result<JudgeVerdict, String> {
    let request = build_judge_request(data);
    let output = runtime
        .block_on(executor.execute(request))
        .map_err(|e| format!("judge executor error: {e}"))?;
    let content = match output.content {
        harness::OutputContent::Text(text) => text,
        other => return Err(format!("Judge 输出应为文本，实际为 {other:?}")),
    };
    parse_judge_verdict(&content)
}

// ============ 断言引擎（设计 §5.3、§6.4 降级链） ============

/// Judge 上下文：采样执行器 + runtime（mock 模式为同一自检 executor）
struct JudgeContext {
    executor: Arc<dyn AgentExecutor>,
    runtime: Arc<Runtime>,
}

fn check_assertions(
    spec: &ScenarioSpec,
    trace: &RunTrace,
    assertions: &[AssertionSpec],
    judge: Option<&JudgeContext>,
) -> (Vec<(String, String, AssertionOutcome)>, Vec<JudgeVerdict>) {
    let mut results = Vec::new();
    let mut verdicts = Vec::new();

    for assertion in assertions {
        let outcome = match assertion {
            AssertionSpec::ToolCalled { tool, min_times } => {
                let count = trace.tool_calls.iter().filter(|(n, _)| n == tool).count();
                if count >= *min_times {
                    AssertionOutcome::Pass
                } else {
                    AssertionOutcome::Fail(format!("{tool} 调用 {count} 次，期望 >= {min_times}"))
                }
            }
            AssertionSpec::StateReached { workitem_status } => {
                let reached = state_reached(trace, workitem_status);
                match reached {
                    true => AssertionOutcome::Pass,
                    false => AssertionOutcome::Fail(format!(
                        "未到达状态 {workitem_status}（task={:?}, workitems={:?}）",
                        trace.task_status, trace.workitem_statuses
                    )),
                }
            }
            AssertionSpec::ResponseMatches { pattern, .. } => {
                let output = trace.final_output.as_deref().unwrap_or("");
                match regex::Regex::new(pattern) {
                    Ok(re) if re.is_match(output) => AssertionOutcome::Pass,
                    Ok(_) => AssertionOutcome::Fail(format!("输出不匹配 /{pattern}/")),
                    Err(e) => AssertionOutcome::Fail(format!("非法正则 /{pattern}/: {e}")),
                }
            }
            AssertionSpec::LlmJudge {
                rubric,
                threshold,
                samples,
            } => {
                let judge_rubric = JudgeRubric {
                    dimensions: vec!["correctness".to_string()],
                    threshold: *threshold,
                    samples: *samples,
                };
                let Some(judge_ctx) = judge else {
                    // mock 冒烟子集之外未提供 Judge 执行器时降级待审，不伪造 AI 结论
                    results.push((
                        assertion.kind().to_string(),
                        assertion.describe(),
                        AssertionOutcome::NeedsHuman("无 Judge 执行器，降级待审".to_string()),
                    ));
                    continue;
                };
                let tool_calls_summary: Vec<String> = trace
                    .tool_calls
                    .iter()
                    .map(|(name, input)| format!("{name}({input})"))
                    .collect();
                let data = judge_data_from_trace(
                    spec,
                    trace,
                    &tool_calls_summary,
                    &judge_rubric,
                    Some(rubric),
                );
                let mut pass_votes = 0;
                let mut min_confidence = f32::MAX;
                for _ in 0..*samples {
                    match run_judge_sample(&judge_ctx.executor, &judge_ctx.runtime, &data) {
                        Ok(v) => {
                            if v.pass {
                                pass_votes += 1;
                            }
                            min_confidence = min_confidence.min(v.confidence);
                            verdicts.push(v);
                        }
                        Err(e) => {
                            results.push((
                                assertion.kind().to_string(),
                                assertion.describe(),
                                AssertionOutcome::NeedsHuman(format!("Judge 采样失败: {e}")),
                            ));
                            min_confidence = 0.0;
                            continue;
                        }
                    }
                }
                let vote = JudgeVote {
                    pass_votes,
                    total: *samples,
                    min_confidence,
                };
                match vote.outcome() {
                    JudgeOutcome::Pass => AssertionOutcome::Pass,
                    JudgeOutcome::Fail => {
                        AssertionOutcome::Fail(format!("Judge 全票否决（{pass_votes}/{samples}）"))
                    }
                    JudgeOutcome::NeedsHuman => AssertionOutcome::NeedsHuman(format!(
                        "Judge 低置信（min={min_confidence:.2}）或票数分裂（{pass_votes}/{samples}），降级待审"
                    )),
                }
            }
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
            AssertionSpec::HumanReview { .. } => {
                AssertionOutcome::NeedsHuman("human_review 断言，强制待审".to_string())
            }
        };
        results.push((assertion.kind().to_string(), assertion.describe(), outcome));
    }
    (results, verdicts)
}

/// 状态映射（见 AssertionSpec::StateReached 文档）：
/// DirectDelegate 路径 Task 直接完成，"Completed" 接受 Task Done 或 WorkItem Completed。
fn state_reached(trace: &RunTrace, status: &str) -> bool {
    let task_hit = match status {
        "Completed" => trace.task_status.as_deref() == Some("Done"),
        "Failed" => trace
            .task_status
            .as_deref()
            .is_some_and(|s| s.starts_with("Failed")),
        _ => false,
    };
    task_hit || trace.workitem_statuses.iter().any(|s| s == status)
}

// ============ 报告 / 待审队列 / 金标准产出（设计 §6.3） ============

fn render_report_markdown(report: &ScenarioReport) -> String {
    let mut md = String::new();
    md.push_str(&format!("# 场景报告：{}\n\n", report.name));
    md.push_str(&format!("- 模式：{}\n", report.mode));
    md.push_str(&format!(
        "- 声明预算：${:.2}（软预算；LLM 输出暂无 usage 字段，仅展示）\n",
        report.declared_budget_usd
    ));
    md.push_str(&format!("- 耗时：{} ms\n", report.trace.elapsed_ms));
    md.push_str(&format!(
        "- 结论：{}\n",
        if !report.all_passed {
            "FAIL"
        } else if report.needs_human {
            "NEEDS_HUMAN"
        } else {
            "PASS"
        }
    ));
    md.push_str(&format!("- 金标准：{}\n\n", report.golden_note));

    md.push_str("## 断言结果\n\n");
    md.push_str("| # | 类型 | 结果 | 详情 |\n|---|------|------|------|\n");
    for (idx, (kind, desc, outcome)) in report.results.iter().enumerate() {
        let detail = match outcome {
            AssertionOutcome::Pass => desc.clone(),
            AssertionOutcome::Fail(r) | AssertionOutcome::NeedsHuman(r) => format!("{desc} — {r}"),
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            idx + 1,
            kind,
            outcome.label(),
            detail.replace('|', "\\|")
        ));
    }

    md.push_str("\n## 运行事实\n\n");
    md.push_str(&format!(
        "- Task 终态：`{}`\n",
        report
            .trace
            .task_status
            .as_deref()
            .unwrap_or("未终止（超时）")
    ));
    md.push_str(&format!("- LLM 调用：{}\n", report.trace.llm_calls));
    md.push_str(&format!("- Judge 采样：{}\n", report.trace.judge_calls));
    if report.trace.tool_calls.is_empty() {
        md.push_str("- 工具调用：（无）\n");
    } else {
        md.push_str("- 工具调用：\n");
        for (name, input) in &report.trace.tool_calls {
            md.push_str(&format!("  - `{name}` `{}`\n", input.replace('\n', " ")));
        }
    }
    md.push_str("\n### 最终输出\n\n");
    md.push_str(&format!(
        "```text\n{}\n```\n",
        report.trace.final_output.as_deref().unwrap_or("（无输出）")
    ));

    if !report.judge_verdicts.is_empty() {
        md.push_str("\n## Judge 采样明细\n\n");
        for (idx, v) in report.judge_verdicts.iter().enumerate() {
            md.push_str(&format!(
                "- #{}: pass={} score={:.2} confidence={:.2} — {}\n",
                idx + 1,
                v.pass,
                v.overall_score(),
                v.confidence,
                v.reasoning.replace('\n', " ")
            ));
        }
    }
    md
}

/// 待审队列条目（NeedsHuman / Fail 均写入，人工标注 pass/fail/partial 后沉淀）
fn render_review_pending_markdown(report: &ScenarioReport) -> String {
    let mut md = String::new();
    md.push_str(&format!(
        "# 待审：{}（{}）\n\n",
        report.name,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));
    md.push_str("人工标注方式：在本文件末尾追加一行 `verdict: pass|fail|partial`，\n");
    md.push_str("标注后移动到 `tests/scenarios/golden/` 或记录退化。\n\n");
    md.push_str(&render_report_markdown(report));
    md
}

/// 金标准快照（结构级）：只锚定 Agent 使用的**工具集合**，最终输出文本不参与比对。
///
/// 设计依据（`docs/design/2026-08-16-real-llm-scenario-testing-design.md` §6.3A）：
/// 金标准做"结构差异"比对而非字节比对——真实 LLM 输出措辞必然波动，整文件全等
/// 只会产生噪音；文本正确性交由 `llm_judge` 语义判断。
///
/// 比对粒度为**去重后的工具集合**（无序、忽略调用次数）：只回答"Agent 是否用了
/// 预期的工具种类"。比"有序含重复序列"更稳健——真实 LLM 可能对同一工具调用多次
/// （如 shell_exec 重试），这不算行为回归；而"不用工具 → 用工具"或"换用别的工具"
/// 这类结构变化才会触发漂移。
///
/// 首次运行自动创建；后续运行仅比对工具集合，漂移时给出期望/实际集合，
/// 不自动更新（`--bless` 语义由人工显式覆盖实现）。
fn apply_golden(root: &Path, spec: &ScenarioSpec, trace: &RunTrace) -> String {
    let golden_dir = root.join("golden");
    let _ = std::fs::create_dir_all(&golden_dir);
    let path = golden_dir.join(format!("{}.md", spec.name));

    let tools = unique_tools(trace.tool_calls.iter().map(|(name, _)| name.clone()));
    let snapshot = format!(
        "# 金标准：{}\n\n## 工具序列\n\n{}",
        spec.name,
        if tools.is_empty() {
            "（无）\n".to_string()
        } else {
            tools
                .iter()
                .map(|t| format!("- {t}"))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n"
        }
    );

    match std::fs::read_to_string(&path) {
        Ok(old) => {
            let old_tools = unique_tools(parse_golden_tools(&old));
            if old_tools == tools {
                "与金标准一致".to_string()
            } else {
                format!(
                    "金标准漂移（工具集合变化）：期望 {:?} / 实际 {:?}；显式更新请覆盖 {}",
                    old_tools,
                    tools,
                    path.display()
                )
            }
        }
        _ => {
            let _ = std::fs::write(&path, &snapshot);
            "首次运行，金标准已创建".to_string()
        }
    }
}

/// 工具名去重并排序，得到集合语义（比对忽略调用顺序与次数）。
fn unique_tools(tools: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut set: Vec<String> = tools.into_iter().collect();
    set.sort();
    set.dedup();
    set
}

/// 从 golden 文件解析工具名：收集所有 `- xxx` 前缀行。
/// `（无）` 不以 `- ` 开头，解析为空集合，与"无工具"语义一致。
fn parse_golden_tools(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            line.strip_prefix("- ")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .collect()
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// 产出报告文件 + （如需）待审条目 + 金标准
fn write_report(root: &Path, report: &ScenarioReport) {
    let reports_dir = root.join("reports");
    let _ = std::fs::create_dir_all(&reports_dir);
    let report_path = reports_dir.join(format!("{}-{}.md", report.name, timestamp()));
    let _ = std::fs::write(&report_path, render_report_markdown(report));

    if report.needs_human || !report.all_passed {
        let pending_dir = root.join("review-pending");
        let _ = std::fs::create_dir_all(&pending_dir);
        let pending_path = pending_dir.join(format!("{}-{}.md", report.name, timestamp()));
        let _ = std::fs::write(&pending_path, render_review_pending_markdown(report));
    }
}

/// 场景 runner 主流程：执行 → 断言 → 报告
fn run_scenario(
    file: &ScenarioFile,
    executor: Arc<dyn AgentExecutor>,
    judge_executor: Arc<dyn AgentExecutor>,
    runtime: Arc<Runtime>,
    mode: &str,
    report_root: &Path,
) -> ScenarioReport {
    let mut trace = execute_scenario(&file.scenario, executor, runtime.clone());
    let judge_ctx = JudgeContext {
        executor: judge_executor,
        runtime,
    };
    let (results, judge_verdicts) =
        check_assertions(&file.scenario, &trace, &file.assertions, Some(&judge_ctx));
    trace.judge_calls = judge_verdicts.len();

    let all_passed = results
        .iter()
        .all(|(_, _, o)| !matches!(o, AssertionOutcome::Fail(_)));
    let needs_human = results
        .iter()
        .any(|(_, _, o)| matches!(o, AssertionOutcome::NeedsHuman(_)));

    let golden_note = apply_golden(report_root, &file.scenario, &trace);
    let report = ScenarioReport {
        name: file.scenario.name.clone(),
        mode: mode.to_string(),
        declared_budget_usd: file.scenario.max_cost_usd,
        all_passed,
        needs_human,
        results,
        judge_verdicts,
        trace,
        golden_note,
    };
    write_report(report_root, &report);
    report
}

// ============ 场景文件加载 ============

fn scenarios_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios")
}

fn load_scenario(name: &str) -> ScenarioFile {
    let path = scenarios_root().join(format!("{name}.toml"));
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取场景文件 {} 失败: {e}", path.display()));
    toml::from_str(&content).unwrap_or_else(|e| panic!("解析场景文件 {} 失败: {e}", path.display()))
}

fn list_scenario_files() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(scenarios_root()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names
}

// ============ 测试入口 ============

/// 门控：与 Layer 1 冒烟测试共用同一组开关（设计 §3 门控统一）
fn real_llm_enabled() -> bool {
    std::env::var("HARNESS_TEST_REAL_LLM").is_ok() && std::env::var("HARNESS_LLM_API_KEY").is_ok()
}

/// Mock 模式框架自检（进 CI，非门控）：验证场景框架全链路（设计 §11 PR3 验证）。
///
/// 使用 `ScenarioSelfcheckExecutor` 跑 echo_report 场景——其确定性断言在 mock 输出下
/// 全部可满足，Judge 走 canned 高置信 verdict，预期结论 PASS 且无需待审。
#[test]
fn scenario_framework_mock_smoke_echo_report() {
    let file = load_scenario("echo_report");
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(ScenarioSelfcheckExecutor {
        final_text: "mock 汇报：系统正常运行。",
    });
    let tmp = tempfile::tempdir().expect("tempdir");

    let report = run_scenario(
        &file,
        executor.clone(),
        executor,
        runtime,
        "mock（框架自检）",
        tmp.path(),
    );

    // Task 完成 + 输出含状态词 + canned Judge 高置信通过
    assert!(
        report.all_passed,
        "mock 自检不应有 FAIL 断言: {:?}",
        report.results
    );
    assert!(
        !report.needs_human,
        "canned Judge 高置信，不应有待审: {:?}",
        report.results
    );
    assert_eq!(report.trace.task_status.as_deref(), Some("Done"));
    assert!(
        report
            .trace
            .final_output
            .as_deref()
            .unwrap_or("")
            .contains("正常运行")
    );

    // 报告与金标准文件已产出
    let reports: Vec<_> = std::fs::read_dir(tmp.path().join("reports"))
        .expect("reports dir")
        .flatten()
        .collect();
    assert_eq!(reports.len(), 1, "应产出恰好一份报告");
    assert!(tmp.path().join("golden").join("echo_report.md").exists());
    // 无待审：review-pending 不应产出
    assert!(!tmp.path().join("review-pending").exists());
}

/// Mock 模式工具循环回归（shell_stat_task 失败根因的守护测试）。
///
/// 修复前 runner 直接 `world.spawn(Task)` 绕过 `spawn_task` 中心封装，EntityIndex
/// 未登记，`tool_calling_orchestrator_system` 的 `index.get_task()` 解析失败（None）
/// 把 `task_is_waiting` 判为 false 而 skip —— 工具结果永不汇入 follow-up LLM，
/// 任务卡死在 `Waiting(ToolExecution)`，最终输出为空（真实 API 运行报告复现）。
///
/// 修复：
/// 1. runner 手动登记 Task/Agent 到 EntityIndex（与中心封装等价）；
/// 2. 场景 agent 显式 Allow 权限——修复 1 后权限检查生效，若仍用默认 Confirm
///    权限，shell_exec 会卡在等待用户确认的 `Waiting(User)`。
///
/// 本测试用 `CannedExecutor`（首次返回 `shell_exec` 工具调用、follow-up 返回含
/// 数字的文本）跑 shell_stat_task 场景，断言任务到达 `Done` 且确定性断言无 FAIL。
#[test]
fn scenario_tool_call_loop_reaches_done() {
    let file = load_scenario("shell_stat_task");
    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let executor: Arc<dyn AgentExecutor> = Arc::new(CannedExecutor::new(vec![
        harness::AgentExecutionOutput {
            content: harness::OutputContent::ToolCalls(vec![harness::LlmToolCall {
                id: "call_shell_stat".to_string(),
                name: "shell_exec".to_string(),
                arguments: r#"{"command":"echo 279","timeout_secs":30}"#.to_string(),
            }]),
            reasoning_content: None,
        },
        text_output("统计完成：当前目录共有 279 个 .rs 文件。"),
    ]));
    let tmp = tempfile::tempdir().expect("tempdir");

    let report = run_scenario(
        &file,
        executor.clone(),
        executor,
        runtime,
        "mock（工具循环回归）",
        tmp.path(),
    );

    assert_eq!(
        report.trace.task_status.as_deref(),
        Some("Done"),
        "工具循环应到达 Done（EntityIndex 登记后 follow-up 正常续跑）: {:?}",
        report.trace.task_status
    );
    assert!(
        report
            .trace
            .final_output
            .as_deref()
            .unwrap_or("")
            .contains("279"),
        "最终输出应包含工具统计结果: {:?}",
        report.trace.final_output
    );
    assert!(
        report.all_passed,
        "确定性断言不应 FAIL（tool_called / state_reached / response_matches）: {:?}",
        report.results
    );
}

/// Mock 模式断言引擎单元验证：tool_called 失败、human_review 待审、正则不匹配失败。
#[test]
fn scenario_assertion_engine_branches() {
    let spec = ScenarioSpec {
        name: "selfcheck".into(),
        description: "断言引擎分支自检".into(),
        input: "x".into(),
        follow_ups: vec![],
        compression_threshold_tokens: None,
        max_cost_usd: 0.0,
        timeout_secs: 1,
    };
    let trace = RunTrace {
        task_status: Some("Done".into()),
        final_output: Some("系统正常运行".into()),
        tool_calls: vec![("shell_exec".into(), "{}".into())],
        workitem_statuses: vec![],
        summarization_completed: 0,
        llm_calls: 1,
        judge_calls: 0,
        elapsed_ms: 0,
    };
    let assertions = vec![
        AssertionSpec::ToolCalled {
            tool: "shell_exec".into(),
            min_times: 1,
        },
        AssertionSpec::ToolCalled {
            tool: "shell_read".into(),
            min_times: 1,
        }, // 失败分支
        AssertionSpec::StateReached {
            workitem_status: "Completed".into(),
        },
        AssertionSpec::ResponseMatches {
            pattern: "正常运行".into(),
            desc: None,
        },
        AssertionSpec::ResponseMatches {
            pattern: "\\d+".into(),
            desc: None,
        }, // 失败分支
        AssertionSpec::HumanReview { note: None }, // 待审分支
    ];
    let (results, _) = check_assertions(&spec, &trace, &assertions, None);
    let labels: Vec<&str> = results.iter().map(|(_, _, o)| o.label()).collect();
    assert_eq!(
        labels,
        vec!["PASS", "FAIL", "PASS", "PASS", "FAIL", "NEEDS_HUMAN"]
    );
}

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

/// Real 模式（#[ignore] + 环境变量双重门控）：真实 API 跑全部场景。
///
/// 执行方式（设计 §3）：
///
/// ```sh
/// HARNESS_TEST_REAL_LLM=1 HARNESS_LLM_API_KEY=... \
///   cargo test --test real_llm_scenarios -- --ignored --nocapture
/// ```
///
/// 结论规则：确定性断言 FAIL → 测试失败；Judge/人工待审不算失败（写入待审队列）。
/// 场景间串行 + 节流（默认 2s，`HARNESS_TEST_SCENARIO_GAP_SECS` 可调）。
#[test]
#[ignore = "需要真实 API（HARNESS_TEST_REAL_LLM=1 + HARNESS_LLM_API_KEY），见 tests/scenarios/README.md"]
fn real_llm_scenarios_run() {
    if !real_llm_enabled() {
        eprintln!("skip: 未设置 HARNESS_TEST_REAL_LLM / HARNESS_LLM_API_KEY，自动跳过真实场景运行");
        return;
    }
    // 与冒烟测试一致：安装 rustls ring CryptoProvider（生产入口 main.rs 同样处理）。
    // `install_default` 仅首次生效，重复调用返回 Err 可忽略。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let names = list_scenario_files();
    assert!(!names.is_empty(), "tests/scenarios/ 下应至少有一个场景文件");

    let runtime = Arc::new(Runtime::new().expect("runtime should be created"));
    let registry = ExecutorRegistry::from_env().expect("构建真实 ExecutorRegistry 失败");
    let executor = registry
        .get("default")
        .or_else(|| {
            registry.get(
                std::env::var("HARNESS_LLM_PROVIDER")
                    .unwrap_or_default()
                    .as_str(),
            )
        })
        .expect("registry 中无可用 executor（检查 HARNESS_LLM_PROVIDER）");

    // Judge 独立 provider（设计 §6.2：与被测模型不同源，避免自我偏好）；
    // 未配置时复用主 executor 并在报告中标注 same-provider。
    let judge_executor: Arc<dyn AgentExecutor> = judge_executor_from_env().unwrap_or_else(|| {
        eprintln!("warn: 未配置 HARNESS_TEST_JUDGE_* 环境变量，Judge 复用主 provider（same-provider 偏好风险）");
        executor.clone()
    });

    let gap_secs: u64 = std::env::var("HARNESS_TEST_SCENARIO_GAP_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let root = scenarios_root();
    let provider = std::env::var("HARNESS_LLM_PROVIDER").unwrap_or_else(|_| "openai".into());
    let mode = format!("real（provider={provider}）");

    for (idx, name) in names.iter().enumerate() {
        if idx > 0 {
            thread::sleep(Duration::from_secs(gap_secs));
        }
        let file = load_scenario(name);
        let report = run_scenario(
            &file,
            executor.clone(),
            judge_executor.clone(),
            runtime.clone(),
            &mode,
            &root,
        );
        println!(
            "[scenario] {} => {}（{} ms，断言 {} 项）",
            name,
            if !report.all_passed {
                "FAIL"
            } else if report.needs_human {
                "NEEDS_HUMAN"
            } else {
                "PASS"
            },
            report.trace.elapsed_ms,
            report.results.len()
        );
        assert!(
            report.all_passed,
            "场景 {name} 存在 FAIL 断言，详见 {}",
            root.join("reports").display()
        );
    }
}

/// Judge 独立 provider 构建（可选）：读取 `HARNESS_TEST_JUDGE_*` 环境变量组。
///
/// 任何一个变量设置即要求整组配置有效；全部缺省返回 None（复用主 executor）。
fn judge_executor_from_env() -> Option<Arc<dyn AgentExecutor>> {
    let provider = std::env::var("HARNESS_TEST_JUDGE_PROVIDER").ok()?;
    let model = std::env::var("HARNESS_TEST_JUDGE_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".into());
    let kind = match provider.to_lowercase().as_str() {
        "openai" => harness::LlmProviderKind::OpenAi,
        "anthropic" | "claude" => harness::LlmProviderKind::Anthropic,
        "deepseek" => harness::LlmProviderKind::DeepSeek,
        "openai-compatible" | "compatible" => harness::LlmProviderKind::OpenAiCompatible,
        other => panic!("未知的 HARNESS_TEST_JUDGE_PROVIDER: {other}"),
    };
    let config = harness::LlmProviderConfig {
        provider: kind,
        model,
        api_key: std::env::var("HARNESS_TEST_JUDGE_API_KEY").ok(),
        api_base: std::env::var("HARNESS_TEST_JUDGE_API_BASE").ok(),
    };
    let executor = harness::create_executor_from_config(&config)
        .expect("构建 Judge executor 失败（检查 HARNESS_TEST_JUDGE_* 配置）");
    Some(executor)
}
