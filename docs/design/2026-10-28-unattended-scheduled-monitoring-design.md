# 无人值守定时任务与长期监控设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 当前有效（P0 已实施） |
| 创建日期 | 2026-10-28 |
| 前置变更 | PR #108：同一时刻到期的多个 cron 全部触发 |
| 实施记录 | P0：`docs/superpowers/plans/2026-10-28-schedule-p0-persistence.md` |
| 相关文档 | `2026-10-28-loop-steering-and-background-notification-design.md`、`docs/async-tool-bridge.md` |

---

## 1. 背景

### 1.1 驱动场景

用户在 IM 中说："帮我监控 xx 论坛的 xx 帖子，收集跟 xx 相关的内容，有新内容马上提醒我，
每天 8 点汇报进度。"

这类需求有四个特征：

- __持续时间长__：以天、周计，跨越进程重启。
- __绝大多数时候无事发生__：轮询 100 次可能只有 1 次有新内容。
- __无人值守__：触发时用户不在线，不能指望用户临场审批。
- __沉默必须可信__：用户收不到消息时，需要能确信是"没有新内容"，而不是"监控已经挂了"。

### 1.2 现状缺口

本节结论均已对照代码核实。

| 缺口 | 现状 | 位置 |
|------|------|------|
| 调度不持久 | `schedule_task` 创建的动态任务只存在内存，进程重启后丢失 | `src/triggers/scheduled_task.rs` |
| 每次运行没有状态 | 触发时新建独立 Task，输入只有 `content` 文本，与发起任务无关联 | `ScheduledTaskInfo::build_task_input` |
| 每次触发都叫醒 LLM | 没有"先确定性检查、有变化才调用 LLM"的环节 | `trigger_task_routing_system` |
| 运行时必定出声 | `schedule_task` 必定带 `output_channel`，最终回复与任务状态消息都推回 IM | `build_output_channel`、`frontend_output.rs` |
| 无人值守被权限卡住 | `shell_exec` / `shell_start` 默认 Confirm，审批请求推到 IM 等人处理 | `src/systems/tools/mod.rs` |
| 授权粒度过粗 | `allow_always` 写入 `agent.overrides`，授权对该 Agent 的__所有__任务生效 | `src/systems/tools/confirmation.rs` |
| 无法判断健康 | 不记录每次运行的结果，沉默与故障无法区分 | — |
| 管理能力不足 | 只能列出、删除，不能暂停、修改 | `list_scheduled_tasks`、`delete_scheduled_task` |
| 同刻多 cron 只触发一个 | 已由 PR #108 修复 | `src/triggers/timer_scheduler.rs` |
| 没有网页抓取 | 联网只能靠 `shell_exec` + `curl`，没有正文提取，输出受 64KB 缓冲限制 | — |

### 1.3 参考设计对照

| 能力 | Claude Code | Codex | 本设计 |
|------|-------------|-------|--------|
| 持久化 | 桌面端本地定时任务、云端定时任务持久保存；会话内 `/loop` 7 天过期 | 自动化持久保存 | 定时任务落盘，重启后恢复 |
| 补跑 | 7 天内只补跑最近一次，弹通知 | 未见明确说明 | 同 Claude：只补跑最近一次并通知 |
| 运行上下文 | 每次新开会话，提示词存为 `SKILL.md` | 独立自动化每次全新；线程自动化回到同一对话 | 每次全新运行，靠状态目录与 harness 注入的上下文块延续 |
| 事件驱动 | Monitor 工具：后台脚本每行输出推回会话 | — | 预筛探针（probe）：代码判断有无变化 |
| 权限 | 每个任务单独设权限模式；首次运行时 always allow，可撤销 | 沙箱模式：只读 / 工作区可写 / 完全访问 | 定时任务级授权清单，无人值守时未授权一律拒绝 |
| 重叠 | 上一次还在跑时跳过并记录原因 | — | 同 Claude：跳过并记录 |
| 无事可报 | — | 结构化输出 `nothing_to_report` 时不通知 | 默认静默，出声必须是显式工具调用；失败必响 |

### 1.4 目标

- 定时任务持久化，重启后恢复，并有明确的补跑规则。
- 引入预筛探针：由确定性的代码判断"有没有变化"，没有变化时不调用 LLM。
- 无人值守运行默认静默，提醒必须是显式的工具调用；失败与长期无成功必定告警。
- 运行结果由 harness 记账，日报可以带上可信的健康信息。
- 定时任务级授权：创建时用户确认一次，之后的无人值守运行不再打扰。
- 与现有 Task、收件箱、异步工具桥、权限链路复用同一套机制，不另起子系统。

### 1.5 非目标

- 不做调度抖动（jitter）：本项目是单机单用户，没有大量会话同刻请求 API 的问题。
- 不做唤醒原任务继续运行的 `continue` 模式（评审决定推迟，见 §11）。
- 不做"后台脚本每行输出即通知"的流式监控：论坛类场景由探针轮询覆盖，流式监控另行立项（见 §11）。
- 不做云端运行：进程不在时定时任务不触发，靠补跑规则兜底。
- 不处理需要登录、JS 渲染或反爬的网站：`web_fetch` 只做无状态 GET（见 §9）。

---

## 2. 总体设计

### 2.1 核心思路

不新增"监控"子系统，而是把现有的动态定时任务升级为一等的持久对象，在触发链路上增加三个环节：

1. __探针__：触发后先由 harness 执行确定性命令，判断有无变化；没有变化就到此为止。
2. __运行__：有变化时才创建 Task，并注入 harness 生成的运行上下文。
3. __记账__：每次触发（无论是否调用 LLM）都写入运行账本，账本驱动失败告警与日报健康信息。

"监控论坛"在本设计中表达为两条定时任务：

| 定时任务 | 调度 | 探针 | 通知策略 | 作用 |
|----------|------|------|----------|------|
| `forum-watch` | `cron:*/10 * * * *` | 抓帖子并与已看过的 ID 比对 | `explicit` | 有新帖时让 LLM 判断相关性，相关才提醒 |
| `forum-daily` | `cron:0 8 * * *` | 无 | `reply` | 汇报进度；`report_of = forum-watch`，注入其健康信息 |

### 2.2 数据流

```text
timer_scheduler ──► ExternalInput::Timer{kind}
                         │
                         ▼
            trigger_task_routing_system（改造）
       ┌─────────┬───────┴────────┬──────────────┐
       │ 已暂停   │ 上次仍在运行    │ 有探针         │ 无探针
       ▼         ▼                ▼               │
     记账      记账 Skipped    ProbeRequest ──►   │
     (Paused)  (Overlap)       probe_runner（tokio）
                                   │
                                   ▼
                         ExternalInput::ProbeResult
                                   │
                    ┌──────────────┼──────────────┐
                    │ 无变化        │ 出错          │ 有变化
                    ▼              ▼              ▼
                 记账 NoChange   记账 ProbeError  创建 Task ◄──┘
                 （静默）        （失败计数）      + ScheduledRun
                                                  + 运行上下文注入
                                                  + 定时任务级授权
                                                     │
                                                     ▼ 任务终态
                                              记账 Notified / Silent / Failed
                                                     │
                                                     ▼
                                        失败计数达到阈值 → 告警（必响）
```

### 2.3 核心概念

| 概念 | 形态 | 职责 |
|------|------|------|
| `ScheduleDefinition` | 持久化数据（`ScheduleStore`） | 定时任务的全部声明：调度、提示词、探针、通知策略、授权等 |
| `ScheduleHealth` | 持久化数据（随定义保存） | 上次触发、上次成功、连续失败次数、是否处于告警中 |
| `RunLedger` | 追加写的 `runs.jsonl` | 每次触发的结果记录，harness 独占写入 |
| `ScheduledRun` | Task 实体上的 Component | 标记该任务是哪个定时任务的一次运行，携带通知策略与授权 |
| `ProbeRequest` / `ProbeResult` | 通道消息 | 探针执行的请求与结果，沿用 `ExternalInput` 入口 |

---

## 3. 定时任务模型与持久化

### 3.1 数据模型

```rust
pub struct ScheduleDefinition {
    pub id: ScheduleId,                    // Uuid；kind 仍为 "scheduled:<id>"
    pub name: String,                      // 用户可读名称，唯一，用于管理命令
    pub content: String,                   // 每次运行的任务提示词
    pub schedule: ScheduleExpr,            // 保留原始表达式字符串，便于落盘与展示
    pub output_channel: Option<ChannelId>,
    pub probe: Option<ProbeSpec>,
    pub notify: NotifyPolicy,              // Reply | Explicit
    pub granted_tools: Vec<String>,        // 定时任务级授权清单（§7）
    pub report_of: Option<ScheduleId>,     // 日报类：注入被观察定时任务的健康信息
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub created_from_task: Option<TaskId>,
}

pub struct ScheduleExpr { pub raw: String, pub spec: ScheduleSpec } // raw 如 "cron:*/10 * * * *"

pub struct ProbeSpec { pub command: String, pub timeout_secs: u64 }

pub struct ScheduleHealth {
    pub last_fired_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub alerting: bool,
}
```

- `ScheduleDefinition` 取代现有 `DynamicScheduledTask` + `ScheduledTaskInfo` 两份数据，
  `SchedulerState.dynamic_tasks` 与 `ScheduledTaskRegistry` 改为从同一份定义投影，消除双账本。
- `ScheduleSpec::Cron` 内的 `cron::Schedule` 不可序列化，因此保留 `raw` 字符串，加载时重新解析。
- 静态路由（`triggers.toml`）维持现状，不并入 `ScheduleStore`：静态路由是部署配置，动态定时任务是运行时数据。

### 3.2 存储

```text
.harness/schedules/
├── schedules.json          # 全部 ScheduleDefinition + ScheduleHealth
└── <schedule-id>/
    ├── runs.jsonl          # RunLedger，harness 独占写入
    └── state/              # 工作目录：探针与 LLM 存放业务状态（如已看过的帖子 ID）
```

- `schedules.json` 采用"写临时文件 + rename"原子写，与 `infrastructure/memory/json_file_store.rs` 一致。
- 写入时机：定义变更（创建 / 修改 / 暂停 / 删除）与 `ScheduleHealth` 变更时立即写穿。
- `ScheduleStore` 放在 `infrastructure/`，ECS 侧经 Resource 持有，对外只暴露一组写方法：
  - 定义变更来自工具调用，经 `ToolEffect` 由 `commit_tool_effects_system` 落账；
  - `ScheduleHealth` 变更来自运行记账系统（§10.1）；
  - 两条路径都经 `ScheduleStore` 的写方法写穿并刷新投影，保持"单一修改入口"不变量。
- 删除定时任务时把 `<schedule-id>/` 移到 `.harness/schedules/.trash/`，不直接删除用户数据。

### 3.3 启动恢复与补跑

启动时从 `schedules.json` 加载全部 `enabled` 的定时任务，然后逐个判断是否错过运行：

- __cron__：计算 `last_fired_at` 之后、当前时刻之前的最近一次应触发时间。若存在且距今不超过
  7 天，__只补跑这一次__，更早的全部丢弃；超过 7 天则不补跑，只记账 `Missed`。
- __once__：触发时间已过且距今不超过 7 天则补跑一次，之后删除；超过 7 天删除并记账 `Missed`。
- 补跑的运行在注入的运行上下文中标注 `trigger="catch_up"` 与原定时间，提示词可据此调整行为
  （例如原定 8 点的日报在 23 点补跑）。
- 每次补跑向 `output_channel` 推送一条系统消息说明补跑原因（与 Claude 桌面端一致）。

### 3.4 重叠规则

同一定时任务的上一次运行仍未进入终态时，本次触发__跳过__，记账 `SkippedOverlap`，不排队、不并发。

- 判定方式：查询是否存在带 `ScheduledRun { schedule_id }` 且未终态的 Task。
- 探针执行中同样算作"仍在运行"，避免慢探针被反复叠加。
- 跳过不计入失败次数，但计入健康信息，连续跳过说明调度间隔小于实际耗时，日报中可以看到。

---

## 4. 预筛探针

### 4.1 契约

探针是一条由 harness（而不是 LLM）执行的 shell 命令，工作目录为该定时任务的 `state/`：

| 退出码 | 含义 | harness 行为 |
|--------|------|--------------|
| `0` | 有变化 | stdout（截断到 16KB）作为载荷，创建 Task |
| `1` | 无变化 | 记账 `NoChange`，静默结束，不调用 LLM |
| 其他 / 超时 / 启动失败 | 探针出错 | 记账 `ProbeError`，计入连续失败（§6.3） |

- 去重由探针脚本基于 `state/` 中的文件负责（例如 `seen_ids.json`），这是确定性逻辑，不依赖 LLM 记忆。
- 探针脚本通常由 LLM 在创建定时任务时编写并落盘到 `state/`，`ProbeSpec.command` 只引用它，
  便于用户审阅。

### 4.2 执行

- 新增 `probe_runner` tokio worker，接收 `ProbeRequest { schedule_id, command, cwd, timeout }`，
  复用 `SessionBackend::exec_with_cancel`（与 `shell_exec` 同一实现，超时与取消语义一致），结果以
  `ExternalInput::ProbeResult` 回到 ECS，经既有 ingress 入口落地。
- 这与 webhook、timer 进入 ECS 的方式一致，不新增通道类型，也不阻塞 ECS 帧。
- 探针超时默认 60 秒，可在 `ProbeSpec.timeout_secs` 中覆盖。

### 4.3 安全

探针在无人值守时以用户权限执行任意命令，因此：

- 创建或修改带探针的定时任务时，`schedule_task` 的权限从 Allow 提升为 __Confirm__，
  确认消息完整展示探针命令与脚本路径（§7.2）。
- 探针命令变更视为新的授权请求，必须重新确认。

---

## 5. 运行上下文

每次运行创建全新 Task，`content` 之后追加 harness 生成的上下文块（Durable，写入对话首条）：

```text
<schedule-context name="forum-watch" trigger="scheduled" fired_at="2026-10-29T08:10:00+08:00">
state_dir: .harness/schedules/<id>/state
notify: explicit（只有调用 channel_send 才会通知用户）
recent_runs: 最近 24 小时 144 次触发，140 次无变化，3 次已通知，1 次探针出错
probe_output:
…探针 stdout…
</schedule-context>
```

- `recent_runs` 由 harness 从 `RunLedger` 统计，不由 LLM 复述，保证可信。
- 带 `report_of` 的定时任务（日报）额外注入被观察定时任务的健康信息：上次成功时间、
  区间内触发 / 通知 / 失败次数、当前是否处于告警中，并可读取其 `state/`。
- 业务状态（收集到的内容、进度）存放在 `state/` 文件中，由探针与 LLM 共同维护；
  harness 只保证目录存在、路径注入、跨运行保留。

------|---------|-----------|
| 跨重启 | 是 | 否 |
| 上下文 | 每次全新，靠 `state/` 与运行上下文块延续 | 宿主任务的完整对话 |
| 上下文增长 | 不增长 | 随运行次数增长，依赖短期记忆压缩 |
| 典型场景 | 长期监控、日报 | 短期跟进 |

`schedule_task` 的工具描述明确说明两者差异，默认 `fresh`。

---

## 6. 通知策略：沉默必须可信

### 6.1 原则

沉默必须是 harness 确认"一切正常且没有新内容"的结果，而不是由 LLM 声明。任何不确定的情况都应该出声。

### 6.2 `NotifyPolicy`

| 策略 | 最终回复 | 任务状态消息 | 提醒用户的方式 |
|------|----------|--------------|----------------|
| `reply` | 自动推送到 `output_channel`（现有行为） | 不推送 | 最终回复本身 |
| `explicit` | __不推送__，只写入账本与 TUI | 不推送 | 只有调用 `channel_send` 才会送达 |

- 两种策略下，定时运行的任务状态消息（运行中、等待中等）都不推送到 IM，避免无人值守时刷屏；
  `frontend_output.rs` 在推送前检查 `ScheduledRun`。
- `explicit` 下出声是一次工具调用，有 schema、可审计、不需要匹配文本。`channel_send`
  未指定目标时已回退到任务的路由通道（`src/systems/tools/channel_send_dispatch.rs:18`），无需新工具。
- 运行结束时，harness 根据该运行是否成功调用过 `channel_send` 记账 `Notified` 或 `Silent`。

### 6.3 失败必响

以下情况不受 `NotifyPolicy` 约束，一律向 `output_channel` 推送系统消息：

- 运行的 Task 进入 `Failed`。
- 无人值守运行中工具因未授权被拒绝（§7.3）。
- 连续失败（`ProbeError` 或运行 `Failed`）达到 `HARNESS_SCHEDULE_FAILURE_ALERT_THRESHOLD`
  （默认 3）次：推送一次告警并置 `alerting = true`，之后不再重复推送；下一次成功时推送一次恢复消息并清除。
- 补跑（§3.3）。

### 6.4 心跳

日报类定时任务通过 `report_of` 拿到被观察定时任务的健康信息，提示词要求在汇报中包含
"上次成功抓取时间、区间内运行次数、失败次数"。由此形成两层保障：

- 监控挂了：日报中能看到"上次成功"停留在很久以前，或直接收到失败告警。
- 日报本身没来：用户自己就能察觉。

---

## 7. 定时任务级授权

### 7.1 权限链路

在现有三层回退之前增加一层任务级授权：

```text
TaskPermissionGrant（任务级） → agent.overrides → agent.default_permission → ToolDefinition.default_permission
```

- `TaskPermissionGrant { tools: Vec<String> }` 是 Task 实体上的 Component，由 `ScheduledRun`
  创建任务时从 `granted_tools` 复制而来。
- 只能放宽为 Allow，不能用来收紧；`agent.overrides` 中显式的 Deny 仍然优先（Deny 不可被授权覆盖）。
- `Agent::effective_permission` 增加任务级参数，仍是权限查询的单一入口；`PermissionAudit`
  事件记录来源 `TaskGrant`。

### 7.2 授权的获取

- 创建定时任务时，`schedule_task` 的参数 `granted_tools` 声明本定时任务需要的工具。
- 声明了 `granted_tools` 或 `probe` 时，`schedule_task` 走 Confirm 流程，确认消息展示：
  调度、提示词摘要、探针命令、授权工具清单。用户确认一次即生效。
- 授权随定时任务持久化，`update_scheduled_task` 修改授权清单或探针时重新确认。
- 现有 `allow_always`（写入 `agent.overrides`，对该 Agent 全部任务生效）保持不变，不在本设计范围内调整。

### 7.3 无人值守的未授权调用

定时运行（Task 上存在 `ScheduledRun`）中，权限结果为 Confirm 的工具__直接拒绝__，
不发起审批请求：

- 返回合成的工具结果："该工具未在定时任务授权清单中，无人值守运行不能请求审批"，让 LLM 调整做法。
- 记账 `PermissionDenied`，并按 §6.3 推送一条提醒，告诉用户可以修改授权清单。
- 这取代现状中"审批请求推到 IM、任务挂起等人"的行为，避免半夜打扰，也避免任务无限期挂起。

---

## 8. 管理

| 操作 | 方式 | 说明 |
|------|------|------|
| 创建 | `schedule_task`（扩展参数） | 新增 `name`、`context`、`probe`、`notify`、`granted_tools`、`report_of` |
| 列出 | `list_scheduled_tasks`（扩展输出） | 增加启用状态、下次触发、上次成功、连续失败、是否告警 |
| 修改 / 暂停 / 恢复 | 新增 `update_scheduled_task` | 按 `name` 或 id 修改调度、提示词、探针、通知策略、授权、`enabled` |
| 立即运行 | `update_scheduled_task` 的 `run_now` | 记账 `trigger="manual"` |
| 删除 | `delete_scheduled_task` | 工作目录移入 `.trash/` |
| 查看历史 | `/schedules <name>` 命令 | TUI 与 IM 展示最近 N 条账本记录 |

- `fresh` 定时任务独立于创建它的任务：创建任务 `/finish` 后定时任务继续运行，
  `schedule_task` 的工具结果需明确告知用户这一点以及如何暂停、删除。
- `/reload-triggers` 仍只替换静态路由，不影响 `ScheduleStore`。

---

## 9. `web_fetch` 工具

监控场景的第一步是抓网页，现有 `shell_exec` + `curl` 有三个问题：原始 HTML 浪费 token、
输出受 64KB 缓冲与 200 行截断限制、需要 shell 权限。新增只读异步工具：

| 参数 | 说明 |
|------|------|
| `url` | 仅 `http` / `https` |
| `mode` | `text`（默认，提取正文）或 `raw` |
| `max_bytes` | 返回内容上限，默认 32KB |

- `kind() == Async`，走异步工具桥；只发 GET，不带 Cookie；拒绝解析到回环、私有网段的地址
  （含重定向目标），防止 SSRF。
- 默认权限 __Confirm__：GET 也能通过 URL 泄露数据，不能默认放开；监控类定时任务经 `granted_tools`
  授权后无人值守可用。
- HTTP 客户端复用已有的 `reqwest`；HTML 转正文需要新依赖，候选 `html2text`（MIT，纯 Rust），
  引入前按依赖原则单独评估（评审结论见 §12）。
- 探针脚本仍可以用 `curl` 自行抓取，`web_fetch` 主要服务于有变化后 LLM 阅读详情页。

---

## 10. ECS 融洽性

### 10.1 系统与调度位置

| 系统 | Set | 新增 / 改造 | 职责 |
|------|-----|-------------|------|
| `schedule_store_load_system` | `Startup` | 新增 | 加载 `ScheduleStore`，投影到 `SchedulerState`，执行补跑判定 |
| `trigger_task_routing_system` | `Transform` | 改造 | 暂停 / 重叠判定、发起探针或直接创建任务 |
| `probe_result_system` | `Transform` | 新增 | 消费 `ProbeResult`，记账或创建任务 |
| `schedule_run_finalize_system` | `Maintenance` | 新增 | 任务终态时记账、更新 `ScheduleHealth`、触发告警 |
| `frontend_output_system` | `Output` | 改造 | 按 `ScheduledRun` 过滤最终回复与状态消息 |
| `commit_tool_effects_system` | — | 改造 | 新增 `ToolEffect::UpdateSchedule` / `DeleteSchedule` |

### 10.2 不变量

- __定义单一真源__：`ScheduleStore` 是动态定时任务的唯一真源，`SchedulerState` 与
  `ScheduledTaskRegistry` 只是投影，只经 `update_scheduler_state` 入口刷新。
- __账本单点写入__：`RunLedger` 只由 `probe_result_system`、`trigger_task_routing_system`
  （跳过类记录）与 `schedule_run_finalize_system` 写入，LLM 与探针都不能写。
- __一次触发一条记录__：每次触发恰好产生一条账本记录，最终结果在终态时补全，便于统计。

### 10.3 发现的冲突与处理

| 冲突 | 处理 |
|------|------|
| `DynamicScheduledTask` 与 `ScheduledTaskInfo` 已是两份数据 | 合并为 `ScheduleDefinition`，属于腐化治理 |
| `schedule_task` 默认 Allow，带探针后可无人值守执行任意命令 | 带探针或授权清单时提升为 Confirm |
| 定时运行的任务状态消息推送到 IM，与"默认静默"冲突 | 定时运行不推送状态消息 |
| 无人值守时审批请求无人处理，任务无限期挂起 | 未授权的 Confirm 工具直接拒绝并提醒 |

---

## 11. 后续（不在本次范围）

- __流式监控__：`shell_start` 支持"每行输出即通知"，并放宽 `AutoWakeBudget` 对长期后台任务的限制。
  适用于本地日志、构建输出等会主动产生事件的来源。论坛类场景由探针轮询覆盖，因此推迟。
- __`continue` 模式__：触发时经 `TaskInbox` 唤醒原任务继续运行，保留完整对话，适用于"盯着 PR 直到合并"
  这类短期跟进。评审决定推迟：`fresh` 加状态目录已覆盖监控场景。若后续实施，需随宿主任务消亡、不持久化，
  且定时唤醒不计入 `AutoWakeBudget`。
- __授权粒度收窄__：评估把 `allow_always` 从 Agent 级收窄为任务级或会话级。
- __需要登录的网站__：Cookie 管理与浏览器自动化。

---

## 12. 评审结论

2026-10-28 用户评审确认：

1. __`continue` 模式__：本次不做，移入 §11 后续。
2. __`web_fetch`__：作为 P5 单独一个 PR 实施；引入 `html2text` 前按依赖原则单独评估。
3. __无人值守时未授权的工具__：直接拒绝并提醒用户（§7.3），不保留"推送审批、超时后拒绝"。
4. __阈值__：失败告警 `HARNESS_SCHEDULE_FAILURE_ALERT_THRESHOLD` 默认 3 次，补跑窗口 7 天。

---

## 13. 测试计划

| 层级 | 用例 |
|------|------|
| 单元 | `ScheduleExpr` 序列化往返；补跑判定（错过 1 次、多次、超过 7 天、once 过期）；重叠判定；告警状态机（达到阈值、不重复、恢复） |
| 单元 | `effective_permission` 四层回退：任务级授权生效、显式 Deny 不可被覆盖、无授权时回退 |
| 集成 | 定时任务落盘后重启恢复；探针退出码 0 / 1 / 其他 / 超时四条路径；`explicit` 下未调用 `channel_send` 不推送、调用后推送 |
| 集成 | 无人值守运行中未授权工具被拒绝并告警 |
| 场景（Layer 2） | 用本地 HTTP 服务模拟论坛：新增相关帖子时收到提醒，无关帖子不提醒，服务停掉后收到失败告警，日报包含健康信息 |

---

## 14. 实施分期

| 阶段 | 内容 | 依赖 |
|------|------|------|
| P0 | `ScheduleDefinition` 合并双账本、`ScheduleStore` 持久化、启动恢复、补跑、重叠规则、`update_scheduled_task` | PR #108 |
| P1 | `RunLedger`、`ScheduledRun`、`NotifyPolicy`、失败告警、`report_of` 健康信息注入 | P0 |
| P2 | 预筛探针（`probe_runner`、`ProbeResult` 入口、Confirm 提升） | P1 |
| P3 | 任务级授权与无人值守拒绝策略（无人值守拒绝已提前实施；`granted_tools` 待做） | P1 |
| P4 | （`continue` 模式，已推迟，见 §11） | — |
| P5 | `web_fetch` 工具（先评估 `html2text` 依赖） | 无 |

每个阶段独立成 PR，同步更新 `docs/current-state.md`、`docs/configuration.md` 与 `.env.example`。
