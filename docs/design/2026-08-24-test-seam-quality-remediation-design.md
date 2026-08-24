# 测试接缝质量修正方案：从实现耦合回归到可观察端口验证

> __状态：待评审__
> 产出背景：2026-08-24 基于 [docs/analysis/test-seam-quality-low-value-inventory.md](../analysis/test-seam-quality-low-value-inventory.md)
> （低质量测试清单）评审输入，对其 `file:line` 证据逐条核对，并制定分批修正方案。
> 本方案为评审级设计，评审通过后再分批转 writing-plans 实施计划落地；本方案不改动任何产品代码。

路径约定：文中 `systems/...`、`domain/...` 等简写均以 `src/` 为根（如
`transform/task_lifecycle.rs` 即 `src/systems/transform/task_lifecycle.rs`）。

## 1. 背景与目标

清单将测试套件按「证伪力」分为三类低质信号——同义反复、实现耦合、占位/空断言——并按
P0/P1/P2 给出优先级。该清单自标「待评审」，且声称的 `file:line` 证据未经验证。本方案完成
两件事：

1. __核对证据__：逐条比对清单引用的代码位置，确认其判断是否成立（§2）。
2. __制定修正方案__：在「基建先行」策略下，给出分批治理的阶段计划、改法、验收与风险（§4）。

__整体目标__：让测试套件从「锁定内部结构」回归到「锁定可观察行为」，使重构内部结构时
测试不无条件碎裂，且恒绿断言清零。__非目标__：不改产品逻辑；不追求一次清零全部耦合，
按阶段独立 PR 推进。

## 2. 清单证据核对结论

核对方式：直接读取清单引用的 `file:line`，与清单描述逐一比对。结论分三类。

### 2.1 已确认：证据属实、判断成立（核心修复源）

| # | 证据位置 | 清单判断 | 核对 |
| --- | --- | --- | --- |
| E1 | `tests/experience_layered_governance_flow.rs:275-280` | 手工赋值再断言同值 | 属实 |
| E2 | `tests/tool_execution_flow.rs:481-487` | 或放水恒真 | 属实 |
| E3 | `tests/dispatch_phase2.rs` / `tests/dispatch_phase3.rs` | 占位零 `assert` | 属实 |
| E4 | `tests/brain_dispatch_flow.rs:227-300` | 负向用例零 `assert` | 属实 |
| E5 | `tests/multi_agent_flow.rs:232-243` | 复制子集逻辑 | 升级，见§2.2 |
| E6 | `src/domain/work_item.rs:430-437` | 构造器 echo | 属实 |
| E7 | `src/domain/work_item.rs:554-587` | 纯 getter 7 连 | 调整，见§2.2 |

核对细节：

- __E1__：`candidate.status = WritebackFailed` 后 `assert_eq!(candidate.status, WritebackFailed)`，零系统变换。
- __E2__：`has_tool_record || !memory.entries.is_empty()`，后者近恒真，工具未记录仍全绿。
- __E3__：两文件各 9/12 行，仅 `placeholder()` 注释占位。
- __E4__：`mvp_flow_unchanged_when_brain_disabled` 末尾仅 8 帧 `sleep` 循环，无断言。
- __E6__：`WorkItem::execution(...)` 后断言 `status == Pending`，对构造器赋值的回声。

### 2.2 证据属实，但处置判断需修正（本方案对清单的实质修正）

__E5 `tags_subset_validation_rejects_invalid_spawn`__：清单判断为「复制被测逻辑」的同义反复，
修复方向写为「调用真实 agent spawn / tags 校验系统」。但核对发现更严重的问题——

- 归档设计 [archive/design/2026-05-16-multi-agent-design.md](../archive/design/2026-05-16-multi-agent-design.md)
  明确记载「权限继承已从 tags 子集改为 tools 权限过滤」；
- codegraph 全量搜索未命中任何 tags 子集校验函数；spawn 路径走
  `spawn_agent` 工具 + `AgentToolPermissions`（`src/systems/tools/builtin/spawn_agent.rs:67`），
  走 tools 权限而非 tags 子集。

由此判断：`tags_subset_validation` 极可能在测试一条__已被废弃的规则__（幽灵规则测试）。
比同义反反复严重——它在锁定一个不再存在的业务约束，且约束逻辑由测试自身重写。
__修正处置__：实施前最终确认规则现状（§4 P0-E 给出确认步骤）；若已废弃则删除测试，
而非「改成调用真实 tags 校验系统」（后者前提不成立）。

__E7 `required_tag_*` 7 连__：清单归为「放水断言」，列入删除候选。本方案判断调整——

- 这些 `required_tag()` 返回的 tag 字符串是对外配置契约（被 plugin/skill 匹配等消费），
  锁定其值有防回归价值，证伪力低但非零；
- 真问题是 7 个独立测试函数的__噪声__，而非「无证明力」。

__修正处置__：不删除，转表驱动压缩为 1 个参数化测试（§4 P4），保留契约覆盖、降噪声。

### 2.3 现状摸底：可观察端口的基建缺口

清单 P1 修复方向是「改为按可观察端口断言」，但未摸底现有 test seam 的复用情况。核对发现
基建缺口是 P1 治理的真正前置：

| 现状 | 问题 |
| --- | --- |
| `MockFrontend` 重复定义 5 处 | 同签名桩被复制，无统一 seam |
| 异步轮询 helper 分散 | 固定 `sleep` 轮询多处，已有雏形散落 |
| 公开注入端口已存在 | 真 seam 已就位，`MockFrontend` 可经此注入 |

证据明细：

- in-source 副本：`frontend_output.rs:329`、`tools/orchestrator.rs:2553`、
  `transform/task_lifecycle.rs:1062`（3 处，同 `kind()`/`push_event()` 签名）；
- tests/ 副本：`signal_event_trigger_flow.rs:16`、`evaluation_workitem_flow.rs:30`；
- 异步 helper：`wait_for_tool_result`（`tests/common/async_tool_bridge.rs:77`）、
  `wait_for_tool_results`（`tests/shell_tool_flow.rs:76`）；
- 注入端口：`build_harness_app`（`src/app/mod.rs:279`）接受
  `frontends: Vec<Box<dyn Frontend>>`。

结论：P1 治理前应先收敛 test seam（§4 P1），否则逐测试改造会反复复制桩代码，重复劳动。

## 3. 治理原则

- __证伪力优先__：每条修复后须能回答「若被测行为回归，此测试会红吗」；不能红即未修复。
- __可观察端口__：断言对象依次取——终态 `status` → `MockFrontend` 事件 → 真实进程产物 →
  异步通道结果 → 真实文件；避免断言内部组件字段 / 内部消息 / 手写索引。
- __基建先行__：P2 迁移前先收敛 `tests/common/` 公共 seam，避免在迁移中复制桩。
- __行为不变__：测试重写不改产品逻辑；迁移耦合测试时用 git 对比保证覆盖的行为不变。
- __规范闸门__：每阶段独立分支 + PR + CI（`fmt` / `clippy` / `test`），遵循 GitHub Flow。

## 4. 阶段计划

阶段依赖：P0 独立可立即执行；P1 → P2 顺序（P2 消耗 P1 基建）；P3、P4 独立可穿插。

### P0：删除占位 + 修复恒真/放水（独立、立即）

__目标__：以最低成本清除清单§一/§三/§四的恒绿与空断言，恢复证伪力。每项可独立 PR。

__P0-A 占位__（`tests/dispatch_phase2.rs`、`tests/dispatch_phase3.rs`）：

- __改法__：dispatch 迁移已完成（`dispatch_system.rs:224`），占位为脚手架遗留。
  探查确认 `async_dispatch_test.rs`（测 `async_tool_dispatch_system`）与
  `workitem_dispatch_flow.rs`（测 `workitem_dispatch_system`）均不覆盖
  phase2/3 意图（`dispatch_system` 扫描 `PendingDispatch` / brain 产出
  `DirectDelegate`）；故处置=补真，经 `build_harness_app` 验证 `PendingDispatch`
  被真实派发。
- __验收__：占位移除；新增测试经真实 app 路径验证 `PendingDispatch` 派发。

__P0-B 幂真__（`tests/experience_layered_governance_flow.rs:267-281`）：

- __改法__：删除手工 `candidate.status = WritebackFailed`；改为触发真实
  writeback 失败路径让系统置 `status`。探查确认 `WritebackFailed` 由真实
  system 设置（`profile_update.rs:242`、`skill_creation.rs:238+` 等，文件
  写入/同名冲突/rename 失败时）；knowledge 类候选的写回失败 seam 为
  实施首步定位项。
- __验收__：注释掉系统 writeback 失败迁移逻辑后，此测试应红。

__P0-C 放水__（`tests/tool_execution_flow.rs:481-487`）：

- __改法__：拆为独立断言：`has_tool_record` 必须为真且校验 `tool_name`/
  `tool_calls` 输入匹配；删除 `|| !memory.entries.is_empty()` 放水侧。
- __验收__：工具调用未记录时测试应红。

__P0-D 零 assert__（`tests/brain_dispatch_flow.rs:227-300`）：

- __改法__：补真实负向断言：`brain = None` 时不应产出
  `PendingDispatch(DirectDelegate)`/`AwaitingBrainDecision`，且任务仍被
  default agent 处理（断言终态 `TaskStatus::Done` 或对应 STM 产物）。
- __验收__：若 brain 误启用产出 dispatch，测试应红。

__P0-E 幽灵规则__（`tests/multi_agent_flow.rs:232-243`）：

- __改法__：已确认废弃——`rg` 全量未命中「子 agent tags ⊆ 父 agent tags」
  校验规则；`orchestrator.rs:1304` 是「按 tags 匹配找 persistent agent」
  （`agent_tags` 全部被 `a.capabilities.tags` 包含）的不同语义；归档
  `2026-05-16-multi-agent-design.md` 记载「权限继承已从 tags 子集改为 tools
  权限过滤」。该测试锁定不存在的规则，删除即无覆盖损失。
- __验收__：删除幽灵测试；无覆盖损失（规则不存在于产品代码）。

__P0-F 构造器 echo__（`src/domain/work_item.rs:430-437`）：

- __改法__：弱化 echo：保留 `work_type` 断言（构造器契约），删除
  `status == Pending` + `is_pending()` 重复 echo，改为依赖既有
  `work_item_state_transitions`（`:439+`）覆盖迁移；或并入 P4。
- __验收__：删除 echo 后迁移测试仍覆盖 `Pending→Assigned`。

__风险__：P0-B 需定位 writeback 失败 system 的触发 seam（见 `experience`
相关 system）；P0-D 需确认 default agent 在 brain 禁用下的处理路径。
均属测试侧改造，不改产品逻辑。

### P1：基建先行——收敛 test seam（为 P2 铺路）

__目标__：在 `tests/common/` 收敛可观察端口桩与异步 helper，消除 5 处 `MockFrontend` 副本，
为 P2 迁移提供统一断言面。

__任务清单__：

- __统一 `MockFrontend`__ → 新建 `tests/common/mock_frontend.rs`（或扩 `mod.rs`）：
  - 收敛 `push_event` 录 `EngineEvent` 的能力，提供 `assert_event_contains(&self, ...)` /
    `events_of_type::<T>()` 等断言 helper；
  - 删除 `frontend_output.rs`、`orchestrator.rs`、`task_lifecycle.rs` 三处 in-source 副本与
    `signal_event_trigger_flow.rs`、`evaluation_workitem_flow.rs` 两处 tests/ 副本，统一 `use`。
- __收敛异步轮询__ → 扩 `tests/common/async_tool_bridge.rs`：
  - 统一 `wait_for_tool_result` / `wait_for_tool_results`，新增 `wait_for_event` /
    `wait_for_condition(timeout, predicate)` 取代固定 `sleep` 轮询；
  - `tests/shell_tool_flow.rs:76` 的本地 `wait_for_tool_results` 改为引用公共版。
- __可观察端口断言 helper__ → `tests/common/` 提供按「终态 status / 真实副作用 / 消息产物」
  断言的封装，供 P2 各批迁移直接调用。

__验收__：

- `MockFrontend` 定义数 `= 1`（`rg -c "struct MockFrontend"` 仅命中 `tests/common/`）；
- 新 helper 被至少 1 个 P2 迁移测试引用；
- 现有 in-source system 单测（`frontend_output`/`orchestrator`/`task_lifecycle`）全绿，
  证明收敛未破坏既有覆盖。

__风险__：收敛 in-source `MockFrontend`（`src/systems/*` 内 `#[cfg(test)]`）需保证 system
单测仍能构造桩；若 in-source 桩与 tests/ 桩行为有细微差异，需对齐而非强删——以测试是否
仍绿为准，差异点在 PR 中逐个裁决。

### P2：逐批迁移实现耦合测试到可观察端口

__目标__：把清单§二「实现耦合」类测试从「断言内部组件字段 / 手写索引 /
内部消息」迁移到「可观察端口」断言，使重构内部结构时测试不无条件碎裂。
__依赖 P1 基建__。量最大，分批 PR。

__P2.1 systems 层 in-source 组件字段断言__：

- __范围__：`routing.rs`、`transform/task_lifecycle.rs`、`tools/orchestrator.rs`、
  `experience/skill_update.rs`、`memory.rs`。
- __改法__：`World.spawn → 跑系统 → query 内部字段` 改为跑系统后断言
  `MockFrontend` 事件 / 终态 `status` / 真实副作用。
- __验收__：模拟内部重构（改名/搬字段）测试不碎。

__P2.2 手工构造内部 Message/索引驱动__：

- __范围__：`tests/o2_permission_inheritance.rs`、`tests/tool_execution_*`、
  `tests/brain_*`、`tests/workitem_dispatch_flow.rs`。
- __改法__：通过 `ingress → dispatch` 公开端口注入输入（真实用户输入 /
  `build_harness_app`），而非 `spawn(PendingDispatch/内部 Message)`；
  断言真实 confirm 行为 / 真实副作用。
- __验收__：改为同步调用 executor 时测试不误报。

__P2.3 时序耦合固定 `sleep` 轮询__：

- __范围__：`tests/experience_layered_governance_*`、
  `tests/sequential_tool_confirmation.rs`、`tests/tool_execution_flow.rs` 等。
- __改法__：固定 `sleep(20ms)×N` 改用 P1 `wait_for_*` 超时轮询
  （已有 `async_bridge_e2e_test.rs` 范式）。
- __验收__：慢机 CI 不 flaky；超时即明确失败。

__保留项__（清单§二.4 已识别为真缝）：`user_plugins_tool_returned` 的
`tool_set_result("replaced")`、`user_plugins_tool_called` 的 `tool_deny`
拒绝语义——断言真实副作用，不在本批迁移。

__风险__：重写测试有「漏测原行为」风险。每批迁移须 git 对比旧断言覆盖的
行为集合，保证新断言覆盖等价或更宽；行为不变由产品代码不动 + CI 全绿兜底。

### P3：插件 marker 自证族补真实副作用

__目标__：把清单§二.4「marker 自证族」（`on_message_dispatched`、`on_llm_response`、
`on_ltm*`、`on_workitem_hooks`、`on_agent_hooks`、`on_task_completed` 等）从「marker 移除 +
entity 存活」的自证，改为消费者观察。

__改法__：让至少一个 hook 脚本执行真实可观测副作用（写文件、发 harness 外 channel 消息、
写共享 store），测试断言该副作用内容（文件内容 / 消息体 / store 值），而非 marker 状态。

__验收__：若 companion 派发在「失配也移除 marker」次序下先移除 marker，测试仍能因副作用
缺失而红（堵住清单§二.4 所述漏洞）。

__风险__：需引入可观测副作用通道（如测试专用 hook 脚本写临时文件）；与 plugin 沙箱边界
需对齐，不得为测试旁路沙箱。

### P4：work_item in-source 测试整理

__目标__：整理 `src/domain/work_item.rs` 的 in-source 测试噪声（构造器 echo + getter 7 连）。

__改法__：

- 构造器 echo（`:430-437`）：按 P0-F 弱化；
- `required_tag_*` 7 连（`:554-587`）：转表驱动压缩为 1 个参数化测试——

```rust
#[test]
fn required_tag_matches_contract() {
    let cases: [(WorkItemType, &str); 7] = [
        (WorkItemType::Evaluation, "evaluation"),
        (WorkItemType::Summarization, "summarization"),
        (WorkItemType::ExperienceCollection, "collect"),
        (WorkItemType::SkillUpdate, "skill-updater"),
        (WorkItemType::ProfileGeneration, "profile"),
        (WorkItemType::Execution, "execution"),
        (WorkItemType::SkillCreation, "skill-creator"),
    ];
    for (work_type, expected) in cases {
        assert_eq!(work_type.required_tag(), expected);
    }
}
```

__验收__：契约覆盖不降（7 条 tag 全覆盖），测试函数数 `7 → 1`；变更任一 tag 常量测试应红。

## 5. 阶段依赖与排期

| 阶段 | 依赖 | 估时档位 | PR 粒度 |
| --- | --- | --- | --- |
| P0 | 无 | 小（每项独立） | 每项一 PR 或合并为 1 PR |
| P1 | 无（可与 P0 并行） | 中（基建收敛） | 1 PR |
| P2 | P1 | 大（分批） | P2.1 / P2.2 / P2.3 各 1+ PR |
| P3 | 无（可穿插） | 中 | 1 PR |
| P4 | 无（可穿插） | 小 | 1 PR |

建议顺序：P0 立即 → P1 基建 → P2.1/2.2/2.3 逐批 → P3、P4 穿插。

## 6. 风险与回退

- __P0 风险__：低。每项独立、测试侧改造，逐项可回退。
- __P1 风险__：收敛 in-source `MockFrontend` 可能触及 system 单测对桩行为的隐式依赖。回退：
  保留 in-source 副本仅作 `#[cfg(test)]` 私有，tests/ 副本先收敛；以 CI 全绿为闸门。
- __P2 风险__：重写测试漏测原行为。缓解：git 对比旧断言行为集合 + 产品代码不动兜底；
  每批 PR 限定单一范围，便于评审。
- __P3 风险__：引入副作用通道可能与 plugin 沙箱边界冲突。缓解：副作用通道限定为测试专用
  hook 脚本写临时文件，不旁路沙箱；沙箱边界由产品代码维护。
- __跨阶段__：所有阶段不改产品代码，行为不变由 CI（fmt/clippy/test）兜底。

## 7. 验收标准（整体）

- 占位 / 空 `assert` 清零（`dispatch_phase2/3`、`mvp_flow_unchanged_when_brain_disabled`）；
- 恒真 / 放水断言清零（E1、E2 等）；
- `MockFrontend` 副本数 `= 1`；
- 固定 `sleep` 轮询清零（改 `wait_for_*`）；
- `required_tag` 测试函数数 `7 → 1`；
- CI 全绿（`markdownlint` / `cargo fmt --check` / `cargo clippy -D warnings` / `cargo test`）。

## 8. 索引同步与附带发现的文档腐化

### 8.1 本方案落位后的索引同步（本方案范围内）

- `docs/design/README.md` 状态表追加本方案一行（状态「待评审」）；
- `docs/README.md`「测试质量分析（analysis/）」小节追加指向本方案的链接。

### 8.2 清单文档状态流转

评审通过后，`docs/analysis/test-seam-quality-low-value-inventory.md` 顶部状态由「待评审」更新为
「已转方案」，指向本方案；实施完成后按生命周期归档。

### 8.3 附带发现的文档腐化（独立任务，不在本方案内实施）

核对期间发现以下文档腐化点，依 AGENTS.md「文档维护是独立任务义务」建议作为独立任务修正，
不在本方案范围内夹带：

1. `docs/README.md`「设计文档（design/）」章节文件名体系过期：所列
   `brain-agent-scheduling.md`、`cognitive-load-reduction.md`、`decision-loop.md` 等短名与
   `docs/design/` 实际日期前缀命名（如 `2026-05-14-brain-agent-design.md`）不符；
2. `docs/README.md`「当前共 10 篇」计数过期，与 `docs/design/` 实际文档数
   （14 篇：13 篇日期前缀 + `im-channel-adapters.md`）不符；
3. `docs/design/README.md` 状态表漏登 `2026-08-10-skill-creation-command-design.md`
   与 `2026-08-17-skill-dependency-and-on-demand-injection-design.md` 两篇。

---

本方案定位为评审输入；评审通过后各阶段转 writing-plans 实施计划落地。
