# 子任务卡在 Brain 决策阶段不推进

> 状态：待修复
> 发现于：`logs/harness_2026-08-09_16-41-16.jsonl`

## 概述

在 `create_tasks` 派生的子任务执行链路中，子任务提交 Brain 决策请求（LLM 选择 agent/skill）后，LLM 响应已返回，但 `brain_decision_system` 未产生任何决策结果，任务永久停留在 `Waiting(Agent)` 状态，既不 dispatch 也不进入可见的失败终态，表现为"卡在 brain"。

同一个会话内，顶层任务的 Brain 决策均能正常 resolve 并 dispatch，唯独该子任务卡死。

## 卡住的任务

- 子任务：`302acd67-bdfe-419c-880a-8e12d9be307d`（name：`fetch-remote-logs`）
- 父任务：`a450aa9c-bf6c-4cdc-b4a7-c15e2402a36f`（name：`p2p-discovery`）
- 进入时机：`08:50:09` 由父任务通过 `create_tasks` 创建，状态 `Waiting(Agent)`
- 卡死时刻：`08:50:20` LLM 响应返回后，至日志结束 `08:51:31` 仍 `Waiting(Agent)`

## 现象与证据链（来自日志）

1. 子任务被 dispatch 到 Brain，请求类型 `BrainDecision`，模型 `Kimi-K2.6`，`tools_count=0`：

```901:901:logs/harness_2026-08-09_16-41-16.jsonl
// event=BrainLlmRequestBuilt task_id=302acd67... agent_id=brain model=Kimi-K2.6 tools_count=0 candidate_agents=14 prompt_len=6368
```

2. `08:50:20` LLM 响应返回，**仅 97 字符且全部为 reasoning 内容，正文无可解析的结构化决策**：

```909:911:logs/harness_2026-08-09_16-41-16.jsonl
// event=LlmRequestCompleted task_id=302acd67... response_len=97
// event=received genai response task_id=302acd67... response_len=97 has_reasoning=true
```

3. 响应返回后仅派发了 `on_llm_response` hook（订阅者为 0），之后再无任何 Brain 决策处理事件：

```912:913:logs/harness_2026-08-09_16-41-16.jsonl
// event=HookDispatchStart hook=OnLlmResponse subscribers=0
// event=LlmResponseHookDispatched hook=OnLlmResponse
```

4. 直至日志结束（`08:51:31`），该任务：
   - 无 `BrainDecisionResolved`（未决策成功）
   - 无 `TaskStatusChanged`（状态仍为 `Waiting`，未变更）
   - 无失败/重试用 `debug!`/`warn!` 日志
   - 无 `AgentDispatched`/子 agent 执行

5. 对照同一日志中两次成功的 Brain 决策（顶层任务 `7da4a314...` 于 `08:49:43` 请求、`08:49:46` resolve；父任务 `a450aa9c...` 于 `08:50:09` 请求、`08:50:11` resolve），均正常出现 `BrainDecisionResolved` 并随后 dispatch。

## 已定位的事实

- `AgentExecutionResultMessage` 实体已成功入 World（`on_llm_response` hook 已派发，证明消息存在且带 `LlmResponseHookPending`）。
- `brain_decision_system`（`src/systems/transform/brain_decision.rs`）在收到该消息后应调用 `parse_brain_skill_selection` 解析 LLM 输出。
- 子任务的 LLM 响应 `response_len=97` 且 `has_reasoning=true`，**响应文本几乎全为 reasoning，未包含可解析的 `{"agent_name":...}` JSON**，解析失败。
- `brain_decision_system` 的解析失败分支（`src/systems/transform/brain_decision.rs` 约 `159-167` 行）仅静默设置 `task.last_error` 与 `task.status=Failed`，**不打任何日志且未触发状态变更推送**，导致任务既不 dispatch 也无法在 TUI 中观察到失败，永久停在 `Waiting(Agent)`。
- 日志未记录 LLM 响应正文（仅 `response_len` 与 `has_reasoning`），无法直接核验 97 字符内容，但"仅 reasoning、无 JSON"可由上述字段推断。

## 影响范围

- 任何 `create_tasks` 派生的子任务，若在 Brain 决策环节命中"模型仅返回 reasoning、无结构化决策 JSON"的响应，都会同样静默卡死，无法自愈、无法在 TUI 观察，只能依赖进程结束或人工介入。
- 父 Agent 的 `wait_tasks` 会因此一直等待该子任务，可能连带阻塞父任务收敛。
- 由于失败分支无日志，问题在日志中不可观测，定位成本高。
