# 低质量测试清单与原因（Seam 与证伪力评估）

> __状态：待评审__
>
> 本清单是测试套件质量扫描的产物，目的是明确哪些测试属于「低质量 / 低证伪力」，等价说明
> 「为什么它们低质」，并给出「后续如何调整」的入口，供评审后形成修复任务。不代表已实施任何修改。

## 评估口径

三类「低证伪力」信号任意之一即可归入本清单：

| 类别 | 定义 | 后果 |
|---|:---|:---|
| __同义反复（tautology）__ | 不调用被测系统，或断言「自己刚写入的值 / mock 刚返回的值 / 由被测同一逻辑算出的期望」，零变换 | 恒绿，退化为对自身代码的自证，测不出真正回归 |
| __实现耦合__（implementation coupling） | 断言锁定内部组件字段 / 内部消息 / 内部 store 方法 / 手写索引 / 内部时序 | 重构内部结构即无条件碎裂，失败点与「行为是否正确」无关 |
| __占位 / 空断言__（no-op） | 无 `assert`，或「任一为真即过」的放水断言 | 跑过 = 无证明力，纯噪声拖累维护 |

文件覆盖：采样了 `src/` 全部 120 个 `#[cfg(test)]` 模块与 `tests/` 全部 76 个集成文件，其中 40+
个文件由后台子代理逐行复读并给出 `file:line` 证据。量化参考：895 个单元测试 + 359 个集成测试；
「对外缝真 seam」约 35–40%，「实现耦合」约 40–45%，「同义反复 / 占位 / 放水」合计约 15–20%。

---

## 一、同义反复 / 恒真断言（应当前重写的核心源）

### 1. `tests/experience_layered_governance_flow.rs:275-277` 幂真断言

```rust
candidate.status = harness::domain::ExperienceCandidateStatus::WritebackFailed;
// ...随后
assert_eq!(candidate.status, ExperienceCandidateStatus::WritebackFailed);
```

- __原因__：测试先亲手把 `status` 赋成 `WritebackFailed`，再断言 `status == WritebackFailed`。
  这个过程中没有任何系统代码参与——若去掉赋值那行，测试根本不发生变换。即便日后删掉「writeback 失败」
  状态迁移逻辑或改判，此测试依然绿。
- __同类__：该文件前 6 个案例（`45-281`）均是「直接调 store/service 方法 + 断言返回值」，其实都是写在
  集成文件里的 store 自测，未经过任何 system/app。
- __调整方向__：删除手工赋值，改为让真实的 writeback 系统在失败路径下把候选置为 `WritebackFailed` 再断言；
  即测到状态机转换，而非赋值后的回声。

### 2. `tests/multi_agent_flow.rs:232-243` 复制被测逻辑于测试内

```rust
let is_subset = child_tags.iter().all(|tag| parent_tags.contains(tag));
assert!(is_subset); // ...再断言 valid_child_tags 是子集
```

- __原因__：这行 `all(|tag| parent_tags.contains(tag))` 就是产品代码 `is-subset` 规则一字不差的重新实现，
  且整函数根本未调用任何 harness / 被测系统，断言的是「测试内这行代码对它自己返回 true」。
  一旦业务规则变化（如改白名单而非子集校验），此测试静默继续通过。
- __调整方向__：调用真实的 agent spawn / tags 校验系统（经 `build_harness_app` 建任务），断言其对外行为
  （如拒绝 + 错误类型），而非在测试里重搭子集判断后再 assert。

### 3. `tests/tool_execution_flow.rs:481-487` “或”放水恒真

```rust
let has_tool_record = ...;
assert!(has_tool_record || !memory.entries.is_empty(), "tool call should be recorded");
```

- __原因__：`||` 两侧只要任意一侧为真即过；`memory.entries`（当前任务 STM）几乎必然非空，因此该断言等于
  「有任意一条 entry 就通过」——工具调用根本未被记录，测试仍全绿。
- __修复__：单独断言工具调用记录存在（含 `tool_name` / `input` 与输入匹配），不依赖「非空」放水。

### 4. 断言「刚 spawn 放入的值」零变换

`multi_agent` 相关测试手工 `world.spawn(Agent)` 放入一个 TaskScoped 子代理后，直接断言「TaskScoped 数量 == 1」。

- __原因__：这个 `1` 由测试亲手 `spawn` 放进去，中间无任何系统变换，断言对其同构、全绿零证明力。
- __同类__：`src/domain/work_item.rs:431-437` 中构造器 set 的状态（`Pending`）被直接断言回，
  属「构造器 echo」而非独立推导。

### 5. `brain` 负向用例「零断言」

`mvp_flow_unchanged_when_brain_disabled`（`tests/brain_dispatch_*`）整函数末尾无 `assert`。

- __原因__：mock executor 恒真返回默认文本 + Brain mock 硬编码 JSON，测试对「Brain 是否决策 / 选哪个 agent /
  技能解析」全未断；负向用例期望行为不变却无任何断言，跑完即绿，属噪声。

---

## 二、实现耦合 —— 重构内部结构即碎

### 1. ECS 内部组件字段断言（systems 层，量最大耦合源）

`src/systems/routing.rs`、`src/systems/transform/task_lifecycle.rs`、`src/systems/tools/orchestrator.rs`、
`src/systems/experience/skill_update.rs`、`src/systems/memory.rs` 等的 in-source 单元测试，
几乎全部是「`World.spawn(...)` → 跑系统 → `world.query::<&Component>()` → 断言内部组件字段」。

- __原因__：断言的是 `&Task` / `&Agent` 组件单元格的内部字段（如 `child_tasks[].origin_channel`、`decay_score`），
  而非「可被观察的事件 / 真实副作用 / 终态状态机」。
- __后果__：重构若把状态从 `Component` 字段搬走、改 SpinLock、改用 Query 直接解析或改名，测试当场整段重写，
  失败点与「系统正确与否」无关。这是治理资源的池底。

### 2. 手工构造内部 Message / 索引以驱动

- `tests/o2_permission_inheritance.rs`：手工构造父 `Agent` 后手动同步 `EntityIndex.agents/tasks`、spawn `Task`、
  手工 `insert` 索引，再断言 `child_agent.tool_permissions.overrides["shell_exec"]==Confirm`——断言内部 `Agent`
  组件字段（而非观察子代理真实 confirm 行为）。
- `tests/tool_execution_*`、`tests/brain_*`：直接 `spawn(ToolExecutionRequestMessage / PendingDispatch(...))`，
  即通过内部 ECS 消息 / `PendingDispatch` 组件注入输入，而非真实用户输入 → `ingress` → dispatch 公开端口。
- `tests/workitem_dispatch_flow.rs:228-233`：断言「没有 `AgentExecutionRequestMessage`」作为成功信号——
  若实现改为同步调用 executor（不再发消息），测试误报。

### 3. 时序耦合 —— 同帧 order 锁定与固定 sleep 轮询

- `tests/experience_layered_governance_*`：通过注释「`approval_result` 与 `writeback` 在同一
  Execution 集内顺序执行」并配合同帧断言（如 `:506`、`:630` 的 same-frame 断言），
  把实现内部的帧内时序绑定进来。一旦调度重构使二者分帧或换集，`assert_eq!(..., Executed, " same-frame ")` 直接碎。
- `tests/sequential_tool_confirmation.rs`、`tests/tool_execution_flow.rs` 等以固定
  `std::thread::sleep` 轮询（约 20ms 间隔 × N 帧）等待异步 worker 推进，属「时序依赖而非契约依赖」，
  慢机 CI 存在 flaky 隐患；`tests/async_*` 群中 `async_bridge_e2e_test.rs` 的关键 race 验证
  已改用 `wait_for_tool_result` 超时轮询规避 vacuous-pass（见该文件注释），其余固定 sleep 处仍属本类隐患。

### 4. 插件侧 marker 自证

- 大多数 `tests/user_plugins_*`（`on_message_dispatched`、`on_llm_response`、`on_ltm*`、`on_workitem_hooks`、
  `on_agent_hooks`、`on_task_completed` 等）：手工 `spawn` 一个 `*_HookPending` marker → 推几帧 → 只断言
  「marker 被移除 + entity 存活」+ 不 panic，脚本体几乎 no-op（`get_task_ids()` + `log_info`）。
- __漏洞__：若 companion 派发在「失配也移除 marker」的次序下先移除 marker，测试照样全绿，
  而 nook 本身（参数传递、回调效果落地）毫无验证。
- __真缝可保留__：`user_plugins_tool_returned` 的 `tool_set_result("replaced")` 断言、`user_plugins_tool_called` 的 `tool_deny` 拒绝语义——这两族断言真实副作用。

---

## 三、占位 / 空断言（删除或补真）

- `tests/dispatch_phase2.rs`（9 行）与 `tests/dispatch_phase3.rs`（12 行）：`placeholder()` 只有注释
  「实际用例在 3/4 填」，断言数为 0，占位即无证据。
- `tests/brain_dispatch_flow.rs:227-300`：`mvp_flow_unchanged_when_brain_disabled` 负向用例整函数零 `assert`。

---

## 四、放水断言汇总

| 模式 | 实例 | 行号 |
|---|---|:---|
| `X \|\|` 放水（任一真即过） | tool_call_is_recorded | `tests/tool_execution_flow.rs:481-487` |
| 断「请求消息被清理」而非真实副作用 | allow/deny 成功仅验 `pending_requests.is_empty()` | `tests/tool_execution_flow.rs` 多处 |
| 断言「刚 spawn 计数 == 1」零变换 | 多 agent 生命周期 | 见上文一.4 |
| 纯 `getter` 回常量 7 连 | required_tag_* | `src/domain/work_item.rs:554-587` |
| mock 短路整条逻辑 + 只断 len/status | brain_dispatch | `tests/brain_dispatch_*` |

---

## 五、后续调整优先级建议（供评审）

1. __P0 立即删除占位 / 空 assert__：`dispatch_phase2/3` 删「占位」或补真 `assert`。
2. __P0 修复恒真 / 放水__：`WritebackFailed` 幂真、`\|\|` 放水，成本低、恢复证伪力，优先级最高。
3. __P1 去实现耦合__：把对内部组件字段 / 手写索引 / 内部消息的断言改为按「可观察端口」——终态 `status`、
   前端 `MockFrontend` 事件、真实进程产物、异步通道结果、真实文件——分批治理，量最大。
4. __P2 插件 marker 自证族__：让至少一个 hook 脚本执行真实可观测副作用（写文件、发消息、写 harness 外 channel），
   测试断言该副作用内容，把「self-flag」变成「消费者观察」。

---

本清单定位为评审输入；P0 / P1 的修复为变更代码，需另走「设计 → 评审 → 实施」流程，本次未改动产品逻辑。
相关索引见 `docs/README.md`。
