# 当前状态

本文档总结 AI Harness 当前已经落地的能力、关键架构结论、已知限制与推荐阅读顺序。

## 项目定位

AI Harness 是一个基于 Rust + Bevy ECS + TUI 的 AI harness 框架，当前重点是把
任务驱动执行、多 Agent 协作、工具调用、记忆治理与评估闭环收敛为一套可维护、
可验证、对 LLM 语义诚实的运行时主链路。

## 能力状态

### 已实现

#### 运行时主链路

- 用户输入经过 Frontend、Signal、Task、Dispatch、Execution、Writeback 形成闭环
- TUI 已作为当前主要交互入口
- 无终端运行形态：`--headless` 不初始化 TUI，只驱动运行时（审批经 IM 通道），启动时要求至少配置一个 IM 通道；
  SIGINT / SIGTERM 请求优雅退出，30 秒内未空闲或收到第二个信号时直接退出。TUI 与 headless 共用
  `harness::app::run_loop`，前端差异由 `LoopHooks` 提供
- 结构化日志、CI、集成测试与回归测试已接入主流程
- Android aarch64 支持（rustls TLS 后端），可通过交叉编译在 Android 设备运行

#### 任务与执行模型

- `Task` 作为用户目标主实体
- `WorkItem` 作为内部执行单元
- `Evaluation` 与 `Summarization` 已迁移到 `WorkItem` 闭环
- `AgentExecutionRequest` 作为瞬时执行请求，不承担长期业务状态
- 终态任务回收：`terminal_task_retention_system` 每 30 秒扫描，任务树内全部终态且超过
  `HARNESS_TERMINAL_TASK_RETENTION_SECS`（默认 1 小时）、无未终态 WorkItem 时整树销毁并清理终态派发去重集合；
  仍处于 `Waiting(User)` 的多轮任务不回收（需 `/finish` 或 `/clear`）

#### 协作与编排

- Brain 调度与多 Agent 配置加载已接入
- 任务分解通过 `create_tasks` + DAG 调度 + `wait_tasks` 实现
- 子任务结果可以回传父任务，支持继续执行
- Brain LLM 决策选择执行 Agent 与 0 或 1 个 skill，详见下方"派发架构"章节

#### 派发架构

- 统一派发入口：所有 Task/WorkItem 派发通过 `PendingDispatch` Component 附加在 Entity 上，
  由单一 `dispatch_system` 扫描处理
- `DispatchKind` 区分 `Task` 与 `WorkItem(WorkItemType)`，`DispatchHint` 携带策略
  （`BrainLlm` / `DirectDelegate`）、`preferred_agent_name`、`required_skill_id`、`agent_spawn_spec`
- Task 派发：TopLevelTask 与 SubTask 统一为 `DispatchKind::Task`，通过 `DispatchHint` 表达差异
- WorkItem 派发：按 `WorkItemType::required_tag()` 查找匹配的 Persistent Agent
- SubTask 派发前置：`subtask_dispatch_preparation_system` 负责 DAG 依赖检查、兄弟任务结果收集、
  `AgentSpawnSpec` 准备，完成后附加 `PendingDispatch`
- Brain LLM 决策：Brain Agent 选择执行 Agent + skill，产出 `PendingDispatch(DirectDelegate)`；
  LLM 失败、输出无法解析或选了不存在的 Agent 时默认标记 Task 为 `Failed`；显式配置
  `HARNESS_BRAIN_FALLBACK_AGENT` 后改为不注入 skill 直接派发给该 Agent（`BrainFallbackUsed`）
- skill 注入：对所有 Task 适用（max 1），仅限 Persistent Agent，通过 `TaskInjectedSkill`
  Component 注入；Brain 选中 skill 时按依赖闭包收窄注入（`build_injected_skills`），
  未选中或闭包为空时维持全量注入（G4 行为变更最小化）
- WorkItem 创建器/派发器职责切分：summarization、evaluation、experience_collection、
  profile_generation、skill_update 系统仅创建 WorkItem + 附加 `PendingDispatch`，不再直接派发

#### 多模型与降级

- Per-Agent 多模型/多提供商差异化调度
- `ExecutorRegistry` 管理多个 provider executor
- `ModelChainState` Component 追踪降级状态
- 429（限流）/402（配额耗尽）错误自动降级到下一优先级
- 冷却期自动恢复到原优先级
- 模型条目 `vision` 声明视觉能力，降级到非视觉模型时图片自动退回文本标记
- `providers.toml` 配置文件支持多 provider 实例
- 向后兼容：现有 `model` 字段和环境变量配置继续工作
- genai 适配层错误分类覆盖非流式 `exec_chat` 路径：HTTP 状态码从
  `webc::Error::ResponseFailedStatus` 中提取，401/403 → `Authentication`（不重试不降级）、
  429 → `RateLimited`（可重试可降级）、402 → `QuotaExhausted`（降级）

#### 测试分层（真实 LLM 场景测试）

- 四层测试模型与三级正确性判断体系已确立（设计文档
  `docs/design/2026-08-16-real-llm-scenario-testing-design.md`）
- Layer 0：共享 mock executor 基础设施已收敛（`tests/common/mock_executor.rs`），
  genai 适配层纯函数已有无网络单元测试
- Layer 1：真实 LLM 冒烟测试已可用（`tests/real_llm_smoke.rs`），覆盖纯文本往返、
  tool_calls 往返、工具名 sanitize 往返、OpenAiCompatible 自定义端点与错误分类；
  采用 `#[ignore]` + `HARNESS_TEST_REAL_LLM` 双重门控，永不进入 CI
- 错误分类测试（401/429）基于 wiremock 本地端点，确定性执行，随 CI 常规运行
- Layer 2/3：声明式场景测试框架已可用（`tests/real_llm_scenarios.rs` +
  `tests/scenarios/*.toml`）——TOML 场景定义六类断言
  （`tool_called` / `state_reached` / `response_matches` / `llm_judge` /
  `human_review` / `summarization_triggered`），支持多轮输入注入
  （`follow_ups`，经生产 routing 续轮链路挂回同一 Task）与场景级压缩阈值覆写
  （`compression_threshold_tokens`）；首批场景集合（echo_report、shell_stat_task、
  multi_turn_context、memory_compression）已齐备；产出 Markdown 报告、待审队列
  与金标准快照；框架自检（mock executor）随 CI 常规运行，真实场景手动执行
- LLM-as-Judge 已成为 harness 一等能力：`JudgeVerdict` / `JudgeRubric` /
  `parse_judge_verdict`（`src/domain/evaluation.rs`）与 Judge prompt 构建
  （`src/llm/judge_prompt.rs`），复用 `AgentRequestKind::Evaluation` 请求通道；
  采样投票 + 低置信/分裂降级人工待审

#### Agent loop 控制

设计见 [2026-10-28-loop-steering-and-background-notification-design.md](design/2026-10-28-loop-steering-and-background-notification-design.md)。

- 引导式插入（steer）：同通道存在活跃的顶层多轮任务时，用户输入投递到该任务的 `TaskInbox`，
  在下一个工具结果边界追加到对话尾部（同步写入 `ToolCallingState` 与 STM），不再新建任务；
  轮末仍有积压时自动续一轮；TUI 任务行显示排队数量；`/btw` 仍用于开启新话题
- 请求期上下文注入管道：`context_attachment_system` 在 `Execution` 首位统一处理任务级
  `LlmCompletion` 请求，负责回合代次打标、收件箱送达与新一轮进度清单回填
- `/stop` 中断（TUI 空输入时 `Esc`）：中止在飞 LLM 请求并递增 `TurnGeneration` 兜底作废结果，取消在飞异步工具，
  撤回排队/待确认工具请求，级联取消子任务，任务回到 `Waiting(User)` 并保留上下文
- 后台任务通知：`shell_start`（`notify_on_exit` 默认开启）与 `create_tasks` 批次登记为
  `BackgroundJob`（`JobOwner` 关系），完成后以 `<task-notification>` 投递收件箱；
  `wait_tasks` 显式等待时认领、不重复通知；空闲任务被自动拉起，连续自动拉起受
  `HARNESS_MAX_AUTO_WAKES` 约束
- 终态约束：任务自然完成时若仍有后台任务或未送达通知，进入 `Waiting(BackgroundJobs)` 暂存候选结果；
  `/finish`、`/clear`、失败时强制停止后台任务；终态任务收到的迟到 LLM 响应被丢弃
- `update_todos` 进度清单：纯展示数据（ADR-007），TUI 任务面板渲染，IM 仅在进度变化时推送

#### 工具与会话

- 工具调用软限制：单轮用户输入内达到 `HARNESS_MAX_TOOL_ITERATIONS` 后返回合成 tool result，让 LLM 总结并询问用户；绝对硬上限（HARD_LIMIT_MULTIPLIER × max_iterations）时强制失败
- 工具权限、审批流程、结果回写与用户确认 UI 已可用
- `chat_with_agent` 工具：支持父任务与 Persistent Agent 多轮同步对话
- `ask_user` 工具：LLM 在工具调用循环中向用户提出开放文本问题，用户回复作为工具结果返回（声明式 Sync 工具，详见 [async-tool-bridge.md](async-tool-bridge.md#sync-工具分类)）
- `view_image` 工具：校验本地图片并返回 `[IMAGE:path]` 标记，视觉模型在下一轮请求中看到真实图片
- `Space` 已收敛为最小共享资源边界，当前只保留 `SharedKnowledgeBase` 和
  `SpaceToolRegistry`
- shell 工具已收敛为六个意图化工具：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- shell 输出语义已收敛为"最新快照"，不再对 LLM 暴露伪增量游标协议
- shell 子进程以独立进程组运行：超时 / 取消 / `shell_stop` 终止整个进程组，后台孙进程不会成为孤儿；
  进程退出后输出读取线程只等待 500ms 宽限期，后台进程持有管道时不再阻塞 `shell_exec`
- 输出快照在行数上限之外再按 64 KiB 截断尾部；已结束的会话记录最多保留 128 条
- 同一任务的多个工具确认请求现在按顺序逐个弹出；`allow_always`（"本会话允许"）授权会在同一会话内立即复用，后续同工具请求直接执行
- 等待工具确认期间，文本输入 `1`/`2`/`3` 会被识别为确认选项（QQ 文本确认），其他文本会提示重试而不是创建新任务

#### 工具权限决策链路

- 工具权限决策采用三层回退：`agent.overrides` → `agent.default_permission`（显式配置时）→ `ToolDefinition.default_permission`
- `Agent::effective_permission(tool_name, registry)` 是权限查询单一入口
- `default_permission_explicit` 字段区分显式/隐式 Confirm，仅隐式 Confirm 回退到工具默认
- 子 Agent 权限继承：父 Confirm → 子 Confirm（不再降为 Allow）
- 运行期授权（`RuntimeGrants`）：用户选择"本会话允许"时按（Agent, 工具, 任务 `origin_channel`）记录，进程内有效、
  不持久化、不写入 `agent.overrides`；只把 Confirm 提升为 Allow，从不放宽 Deny。没有来源会话的任务
  （webhook 事件、定时任务）不复用任何运行期授权，选择该选项时按仅本次处理。权限查询入口为
  `resolve_tool_permission`（配置层三层回退 + 运行期授权）
- 父 Agent 自动审批（MVP）只认配置层权限，不读运行期授权，也不写入任何持续授权
- `EngineEvent::PermissionAudit` 审计事件覆盖 dispatch / async_dispatch / confirmation / approval 路径

#### 异步工具桥

- 异步工具桥已落地：`kind() == Async` 的工具请求经「dispatch 挂起 → tokio worker 异步执行
  → 通道回传 → ingest 落地」闭环，「不堵 ECS」从约定升级为构造。详见
  [async-tool-bridge.md](async-tool-bridge.md)
- 双轨期：`ToolActionKind` 缺省 `Sync`，异步工具 override 为 `Async`；dispatch 按 `kind()`
  分流，Sync 工具继续走旧路径，禁止新增 Sync 工具
- 六条架构不变量：统一异步、快照进效果出、compute/apply 切分、调度账本单一修改入口、
  结果落地单点 + exactly-once、双超时分层
- 三条失联路径（worker panic / 业务超时 / 通道断开）经 sweeper claim 殊途同归到 ingest
  单点落地，exactly-once 由「挂起实体是否还在」唯一裁决
- 父任务终态触发 worker 取消：`cancel_monitor_system` 调 `CancellationToken.cancel()`，
  worker `select!` 监听后 kill 子进程并回送 cancelled error
- 通用效果提交：写工具返回声明式 `ToolEffect`，由 `commit_tool_effects_system`（exclusive
  system）经 `update_scheduler_state` 统一入口落账，结果回送通道由 ingest 下一帧落地
- 动态定时任务管理四件套已上桥：
  - `list_scheduled_tasks`（纯读，pilot 工具）
  - `delete_scheduled_task`（写路径，按 kind 或 name 定位，idempotent 语义）
  - `schedule_task`（写路径，经 `ToolEffect::ScheduleTask` 落账）
  - `update_scheduled_task`（写路径，修改 / 暂停恢复 / 立即运行，经 `ToolEffect::UpdateSchedule` 落账）
- `shell_exec` 已迁移至异步桥，CancellationToken 取消路径打通
- Rhai 插件经 `spawn_blocking` 包裹上桥，插件 API 不变
- `OwnedToolContext.current_origin_channel` 由 dispatch 从 `Task.origin_channel` 注入，
  让 `schedule_task` 等需要继承通道的异步工具在 worker 内拿到真值
- 背压实验结论：保持无界 `mpsc::unbounded_channel`，不切换有界通道（详见
  [async-tool-bridge-pilot-report.md](async-tool-bridge-pilot-report.md)）

#### IM 通道

- 统一 `Channel` 抽象与 `ChannelManager`（含 listen 重启退避与 shutdown）
- Channel trait 统一抽象 `recall_message`/`send_typing`（QQ + Telegram 实现，默认 NotSupported/Ok）
- `ChannelOutboundMessage` 携带 `MessageKind`（LLMReply/TaskStatus/ApprovalRequest/System/Recall/Other）
- IM 通道状态消息治理：任务状态消息滚动撤回（发新撤旧，避免 2 分钟超时）、入向 ACK 用 typing 替代（C2C）、审批请求消息点击后撤回、LLM 回复到达时撤回最终态状态消息
- Telegram 通道接入（长轮询、白名单、文本分块发送）
- `/bind` 配对授权回写 `channels.toml` 统一经 `ChannelConfigs::update_file`：原子写入，文件无法解析时拒绝覆盖
- Telegram 通道消息撤回（`deleteMessage`）与输入状态指示器（`sendChatAction` with `typing`）
- QQ 通道接入（WebSocket Gateway、OAuth2、markdown/富媒体发送、审批文本回复匹配）
- QQ 文本回复 `1`/`2`/`3` 可直接作为工具确认选项，非选项文本会收到重试提示
- QQ 审批按钮受白名单约束；按钮过期或被同会话新审批覆盖后点击，会提示改用 `1`/`2`/`3` 文本回复
- QQ 通道消息撤回 API（C2C / 群聊 DELETE 端点）
- QQ 通道输入状态指示器（typing indicator, POST /v2/users/{openid}/typing）
- QQ 通道交互回调（PUT /interactions/{id}）
- QQ 通道交互事件监听（INTERACTION_CREATE）与按钮点击闭环
- QQ 通道审批消息使用原生 InlineKeyboard 按钮交互（含 reject_with_feedback 两步流程）
- QQ 通道 send 方法返回 message_id（QqMessageResponse）
- QQ 纯图片消息合并：移动端逐张发送的图片按发送者缓存，与下一条常规文字消息合并为一条输入
- 图片输入：`[IMAGE:path]` 标记在 LLM adapter 边界晚绑定为图片 ContentPart
  （设计：[2026-10-27-multimodal-image-input-design.md](design/2026-10-27-multimodal-image-input-design.md)）
- 图片信任边界：只有任务 `TrustedImages` 登记表中的目标会被晚绑定。登记来源为用户会话输入
  （新建任务、续轮、steer、`ask_user` 回答）与 `view_image` 成功结果（插件替换过的输出除外）；
  webhook / 定时任务内容、shell 输出等其他来源的标记保留为文本并附 `untrusted source` 说明
- `channel_send` 工具主动推送
- `origin_channel` 从入向消息透传到 `Task`
- IM 出向-自动回执：Agent 文本回复、`SystemOutputMessage`、任务失败提示等按 `output_channel`
  （通常即来源 `origin_channel`）自动推回来源 IM 通道，并附加任务短 ID 前缀（如 `[a1b2c3d4]`），
  便于同一会话中多任务并行时区分消息来源
- IM 任务状态展示：任务状态变更（如运行中 → 等待中）作为独立状态消息推送到 IM 通道，同样携带任务短 ID 前缀
- 跨通道隔离：用户输入仅路由到同一通道中等待用户或正在执行（steer）的 Task；`/finish`、`/stop`、`/summarize`、`/btw` 等命令限定在发出通道生效；子任务继承父任务的 `origin_channel`

#### 记忆治理

- 记忆系统已收敛为 `ShortTermMemory`、`LongTermMemory`、`SharedKnowledgeBase`
- `AgentExperience` 已删除，不再作为独立运行时概念保留
- `LongTermMemory` 采用 `Core + Relevant` 的受控注入策略，避免全量拼接 prompt
- 共享知识写入默认仅允许用户显式命令或主控审核链路，不允许普通 Agent 直写
- 长期记忆已具备基础衰退治理能力，会结合访问时间、重要度与复用次数更新分数
- 长期记忆已实现淘汰机制：`decay_score < 0.1` 且非 `pin` 非 `Critical` 的条目被移除并归档到 `<agent-name>/archive.jsonl`
- 长期记忆已实现 JSON 文件持久化（`MemoryStore` + `MemoryRepository` + `LongTermMemoryService` 写穿模型）
- Agent 启动时可从持久层恢复 `LongTermMemory`，子 Agent 贡献吸收后立即落盘
- `SharedKnowledgeBase` 已迁移到文件系统管理（`.harness/knowledge/*.md`），通过 `knowledge-manager` Agent 统一治理
- 知识库管理员 Agent（Persistent 类型）负责检索与维护知识库，复用 `chat_with_agent` + shell 工具

#### 信号触发系统

- 信号触发链路：外部 Signal → `TriggerTaskMessage` → `CreateTaskMessage` → Task，支持 Webhook 与 Timer 两类触发源
- `TaskRoutingPolicy`：控制触发任务的 `output_channel`、`approval_channel`、`approval_context`，实现触发源与任务路由的解耦
- `SignalTriggerRegistry`：映射触发 kind 到 `EventTaskRoute` 配置，支持运行时查询与热重载
- axum Webhook 服务器：监听 HTTP 请求，按路由配置匹配 kind 并注入信号
- cron Timer 调度器：按 cron 表达式周期触发信号，cron 表达式按系统本地时区解释，支持多路由并行调度
- prompt 模板插值：`{{body_json.field}}` 语法从 webhook payload 提取字段，生成任务提示
- `triggers.toml` 配置文件：声明 webhook 监听地址、路由、auth token 与 timer cron 表达式、路由
- `/reload-triggers` 命令：运行时热重载 `triggers.toml`，仅替换静态路由；动态 scheduled task 原样保留
- `schedule_task` 内置工具：Agent 可动态安排未来 AI 任务，支持 `once:<ISO>` 一次性触发与 `cron:<5字段>` 周期性触发
- `schedule_task` 与 `update_scheduled_task` 默认需要用户确认（会写入持久化、之后无人值守执行的任务），
  确认卡片展示完整参数（名称、调度、内容、输出通道）；`list_scheduled_tasks` / `delete_scheduled_task` 直接允许
- `schedule_task` 任务的 `output_channel` 默认继承当前任务 `origin_channel`，显式指定时可覆盖到 `tui`/`telegram`/`qq`/`web`
- 动态 scheduled task 触发后，其 `output_channel` 作为输出通道；若对应 frontend 未注册，任务将明确失败并记录 `FrontendApprovalRouteInvalid`
- 无人值守拒绝（设计 §7.3 的最小子集）：定时任务运行（`ScheduledRun` 标记的 Task 及其子任务）中权限结果为 Confirm 的工具
  不再发起审批，直接返回 `PermissionDenied` 合成结果并向输出通道推送提醒，避免任务挂起后被重叠规则持续跳过；
  任务级授权清单（`granted_tools`）仍待 P3
- 文本审批回复按任务的 `approval_channel` 匹配（缺省回退 `origin_channel`），webhook 事件任务也能用 `1`/`2`/`3` 回复审批
- 动态定时任务以 `ScheduleDefinition` 为唯一账本（`SchedulerState.dynamic_tasks`），带唯一名称、
  启停状态与 `last_fired_at`；设计见
  [无人值守定时任务与长期监控设计](design/2026-10-28-unattended-scheduled-monitoring-design.md)（P0 已实施）
- 持久化：定义变化时由 `schedule_persist_system` 原子写入 `.harness/schedules/schedules.json`，启动时恢复；
  文件损坏时改名为 `.corrupt-<时间戳>` 后按空启动，读取失败时本次会话不启用持久化
- 补跑：停机期间错过的运行，7 天内只补跑最近一次并向输出通道推送补跑说明，任务内容前标注原定时间；
  cron 由 `schedule_catch_up_startup_system` 在启动时判定，一次性任务在路由时判定，超出 7 天的一次性任务删除并通知
- 重叠规则：同一定时任务上一次运行（`ScheduledRun` 标记的 Task）未进入终态时，本次触发跳过并记录 `ScheduleSkipped`
- 暂停的定时任务不被 timer 调度，触发也会跳过；`update_scheduled_task` 的 `run_now` 可手动运行（不受暂停影响）
- 一次性任务触发时只记 `last_fired_at`（timer 不再调度已触发的一次性任务），运行进入终态后由
  `once_schedule_completion_system` 删除定义并记录 `DynamicTaskRemoved`；运行未完成就停机时，
  重启后该任务转为暂停并向输出通道提醒（`OnceScheduleInterrupted`），不自动重跑，
  可用 `run_now` 重新运行或删除

#### 插件系统

- 插件系统已实现完整的 Rhai 脚本扩展层，支持通过 `HARNESS_PLUGINS_DIR` 环境变量加载
- 插件清单格式为 `manifest.toml`，声明 `id`、`api_version`、`hooks`、`tools`、`skills`、`agents` 贡献
- 20 个 hook 点已全部接入，覆盖任务、工作项、Agent、工具、消息、记忆、知识、经验、审批全生命周期
- 前置 hook（`on_tool_called`）支持 `tool_deny` 拒绝能力，观察 hook（`on_tool_returned`）支持 `tool_set_result` 替换结果
- 插件工具通过 `RhaiToolExecutor` 注册为命名空间化工具（`plugin_id:tool_name`），支持 JSON Schema 输入校验
- 插件技能通过 `SkillLoader` 注入 `PluginSkillContributions`，命名空间化为 `plugin_id:skill_id`
- 插件 Agent 通过 `PluginAgentEntry` 合并到 `load_agents_system`，复用 Agent 启动链路
- 支持 `/plugins` 列出已加载插件、`/reload-plugins` 热重载、`/plugin_id:command` 调用插件命令
- 重载时自动清除旧插件的工具、技能、Agent 贡献，重新扫描磁盘并注册新贡献
- host API 提供 `WorldSnapshot`（只读快照）+ `WorldWriter`（写命令攒批回放）的隔离访问模型
- hook 脚本执行受 200 毫秒超时保护，按插件字母序顺序派发；一帧内累计耗时超过 50 毫秒时告警 `HookBudgetExceeded`
- 所有关键操作具备结构化审计日志（`PluginToolDeniedByHook`、`PluginToolResultSetByHook` 等）

#### 经验候选治理

- 经验治理已收敛为两层分层模型：非顶层 `TaskScoped Agent` 只产生、汇聚、向上贡献；顶层 `Persistent Agent` 做最终治理与落盘
- `ExperienceCandidate` 是经验治理唯一中间态，具备完整状态机：
  `Submitted / InInbox / Aggregated / Superseded / GovernancePending / NeedsUserApproval / Approved / Rejected / Persisted`
- 非顶层候选通过父任务 `ExperienceInbox` 上送，顶层候选进入 root 后触发 `ExperienceGovernanceRequestMessage`
- 经验类型简化为两类：`Knowledge`（可复用知识）和 `Skill`（可复用技能包，对齐 Agent Skills 规范）
- 顶层治理后三类最终去向全部可达：
  - `Knowledge` → 普通持久型 Agent 的 `LongTermMemory`
  - `Skill` → 用户确认后生成 Agent 私有 `Skill Package`（SKILL.md 目录结构，对齐 agentskills.io 规范）
  - `default Agent` 的 `Knowledge / Skill` → `IncubationProposal`
- `default Agent` 通过 `tags` 中的 `default` 识别，不直接沉淀私有长期身份资产
- `LongTermMemoryEntry` 已具备最小来源追溯字段：`source_candidate_id`、`source_task_id`、`agent_id`
- `IncubationProposal` 已扩展为正式治理输出结构，包含 `proposal_id`、`proposed_agent_profile`、按类型分列的候选 ID、`status`、`created_at`
- 写回失败时保留 `warn` 级审计日志，候选状态不推进到 `Persisted`
- Skill 候选提交时验证 `file_refs` 文件存在性，缺失文件拒绝提交
- 非顶层候选汇聚后，同类候选数 > 1 时触发 LLM 合并（`ExperienceConsolidationRequestMessage`），原始候选标记为 `Superseded`
- Skill Package 写回后，Agent 启动时通过 `SkillLoader` 扫描 `skills/` 目录，将 SKILL.md 内容注入系统提示
- `IncubationProposal` 执行时同时处理 `skill_candidate_ids`，将 Skill 写入新 Agent 的 Skill Package 目录

#### Skill 一等公民与自更新

- `SkillId`（`owner_agent_name` + `skill_name` 复合）+ `SkillEntry`（`name`、`description`、
  `instructions`、`version`、`self_updatable`）+ `SkillRegistry` Resource 作为 skill 一等公民基础
- `SkillLoader::build_registry()` 启动时扫描 `.harness/assets/agents/<owner>/skills/<name>/SKILL.md`
  构造 `SkillRegistry`；SKILL.md frontmatter 支持 `version`、`self_updatable` 与 `dependencies` 字段
- skill 依赖声明与按需注入（设计文档
  `docs/design/2026-08-17-skill-dependency-and-on-demand-injection-design.md`）：
  - frontmatter `dependencies: [skill-b]` 声明同 agent 名下 skill 的依赖，数据源为磁盘扫描
  - `resolve_skill_closure` 按拓扑序解析传递依赖闭包（DFS + 访问栈环检测，缺失跳过、环截断）
  - `validate_skill_dependencies` 严格校验依赖存在性 + 环，创建/更新写回前调用，
    失败时新 skill 候选置 `WritebackFailed`（不落盘）、更新走既有回滚路径
  - `build_injected_skills`：Brain 选中 skill 时注入其依赖闭包，未选中时维持全量注入
- 持久 Agent 直接吸收子任务经验而非向上转发（`route_persistent_agent_experience`）：
  - `Skill` kind → skill-updater WorkItem 路径
  - `Knowledge` kind → `WritebackPending`（直接写长期记忆）
  - 无注入 skill → 转发到顶层治理（`ExperienceGovernanceRequestMessage`）
  - 临时 Agent 维持原行为：候选进入父任务 `ExperienceInbox`
- `TaskExperiencePolicy` / `ExperienceKindFilter` Component 支持对候选类型做白名单/黑名单过滤
- 顶层治理 Skill 分支根据 `self_updatable` 路由：
  - `self_updatable = true` → `ExperienceWritebackDestination::SkillUpdate`，spawn
    `SkillUpdateRequestMessage`，候选保持 `GovernanceResolved`
  - `self_updatable = false` → 候选标记 `Discarded` + warn 日志
    （`SkillCandidateDiscardedNotSelfUpdatable`）。不强行降级 payload 形态，需要变更该 skill
    的应通过 `IncubationProposal` 提案新 skill（ADR-004 v6 D15）
  - `default Agent` 维持 `Skill → IncubationProposal` 路径不变
- skill-updater Agent 消费 `SkillUpdateRequestMessage`，构造 prompt 后 spawn `WorkItem`（类型为
  `WorkItemType::SkillUpdate`）+ `SkillUpdateContext` + `PendingDispatch`（dispatch 架构统一后由
  `dispatch_system` 派发）
- skill-updater 的 prompt 现在包含完整 SKILL.md 内容（frontmatter + 所有 section 标题），让 LLM
  看到真实结构而非幻觉 section 名
- `submit_skill_update` 工具：LLM 仅提交 `operations` + `rationale`；`skill_id` / `base_version` /
  `new_version` 由 orchestrator 从 `SkillUpdateContext` 服务端权威注入（避免 LLM 臆造 skill_id）。
  orchestrator 在 insert 完成消息前先做 dry-run 同步校验
- `SkillUpdateCompletedMessage` 由 orchestrator insert 到 WorkItem entity（与 `SkillUpdateContext`
  同 entity），不再 spawn 独立 entity
- `skill_update_completion_system` 通过同 entity Component 联合查询直接拿 context
  （`SkillUpdateContext` + `SkillUpdateCompletedMessage`），不再用 `work_item_id` Uuid 反查
- `skill_update_completion_system` 执行职责：
  - apply diff 到 SKILL.md（任一 section/subsection 未找到即整体失败）
  - 目录级快照备份到 `history/v{base}/`，写入新版本 frontmatter `version: base + 1`
  - 通过 `SkillLoader` 重建并替换 `SkillRegistry` Resource
  - 候选推进到 `Persisted`
- 失败时保留 SKILL.md 原内容不变，候选保持 `GovernanceResolved` 状态；LLM 返回 text/Err 时
  正确清理 `WorkItem` + `SkillUpdateContext` 并标记 `OnWorkItemFailed`

##### ADR-006：skill updater 多文件更新支持

- 参考 `docs/adr/ADR-006-skill-updater-multi-file-support.md`
- `SkillUpdateOperation` 扩展为 11 种 variant：8 种 section/subsection/frontmatter 操作新增
  `path: Option<String>` 字段（`None` → SKILL.md，`Some(p)` → sibling 文件），新增 3 种文件级操作
  `replace_file` / `create_file` / `delete_file`（禁止作用于 SKILL.md）
- `read_skill_file` 只读工具（仅 skill-updater Agent 可用）：读取 skill 目录下 sibling 文件内容，
  路径受 `validate_skill_file_path` 沙箱约束（词法 `..` 拒绝 + canonicalize 状态检查 + 后缀白名单）
- skill-updater WorkItem 为 `multi_turn = true`，LLM 可先 `read_skill_file` 感知子文件再
  `submit_skill_update`；prompt 注入 skill 目录文件树
- `skill_update_completion_system` 按操作是否含文件级/带 path 操作分流：纯 SKILL.md 走单文件
  apply（向后兼容），否则走 `apply_skill_operations_multi`（目录级顺序 apply）
- 备份从单文件 `history/v{base}.md` 升级为目录级快照 `history/v{base}/`（`backup_skill_dir`）；
  失败时 `restore_skill_dir` 整体回滚；`cleanup_skill_dir_history` 保留最新 3 代

##### v8 D19：update 端 / generation 端颗粒度对齐

- `SkillUpdateOperation` 扩展到 8 种 variant（ADR-004 v8）：
  - 二级标题级：`replace_section` / `add_section` / `remove_section` / `replace_frontmatter`
  - 三级标题级：`replace_subsection` / `add_subsection` / `remove_subsection`（在指定 `## section`
    范围内定位 `### subsection`）
  - 兜底：`replace_body`（整体替换 body，frontmatter 不变）
- `apply_skill_operations` 在 apply 完成后调用 `validate_skill_structure` 做 post-apply 校验：
  - 必须含至少 1 个 `##` 标题
  - 第一个 `##` section 不能为空
  - 校验失败回滚为 `ApplyError::StructureInvalid`，整体 apply 失败（D13 整体回滚语义）
- `find_section_range` 修复 `trim_start() ==` → `trim() ==`，标题前后空白均容忍；同名章节歧义
  时记 `warn!` 日志并取第一个匹配
- `persist_skill_package` 落盘前调用 `validate_skill_structure` 拒绝结构不合规的 instructions，
  frontmatter 显式写 3 字段（`name` + `description` + `self_updatable: true`）
- generation 端 prompt（`experience_collection_completion_system` / `submit_experience_candidate`
  工具描述）追加 SKILL.md 格式约束：至少 1 个 `##` 标题、推荐 section 名、可用 `### Subsection`、
  不要 `####`、`validate_skill_structure` 校验 + `WritebackFailed` 后果
- skill-updater prompt 列出全部 8 种 operation，标注优先级（subsection 级 > section 级 > replace_body），
  `replace_body` 加软约束警示（仅当其他 operation 无法表达时才使用）
- `candidate_payload_text` 输出显式 `[候选类型：Knowledge/Skill]` 前缀，与 prompt 中
  `candidate_kind_label` 一致，避免 LLM 在长候选中丢失类型语义

##### `/skill` 命令：Skill 创建

- `/skill <意图描述>` slash command：为活跃任务的 Agent 无中生有创建新 skill
  （`UserCommand::CreateSkill`）；空意图或无活跃任务时拒绝并提示 usage
- 执行由专用 `skill-creator` Agent 承担（声明在 `agents.toml.example`，tags `skill-creator`，
  工具白名单：`submit_skill` / `write_skill_file` / `read_skill_file`）
- 创作过程在沙盒目录 `.harness/assets/agents/<agent>/skills/.sandbox/<skill_name>/` 内进行，
  随任务终态或 `/clear` 自动清理；`SkillLoader` 扫描时过滤 `.sandbox` 目录
- `submit_skill` 提交后构造 `ExperienceCandidatePayload::Skill { is_new: true }`，经
  `SkillCreation` 目的地 + 用户确认流程，确认后由 `skill_creation_writeback_system`
  rename 原子写回正式 skill 目录并重建 `SkillRegistry`（同名冲突拒绝）

### 待完善

- 父 Agent 审批仍是 MVP 自动通过实现，需要替换为真实 LLM 审查
- 插件 host API 部分 `WorldCommand` 变体（`CreateWorkItem`、`SetApprovalDecision`、`ExperienceSetPinned`）尚未实现回放
- 插件 `v1` 不追踪 `tool_deny` 的 per-plugin attribution，推迟到后续 host API 升级
- 历史设计文档仍有一部分使用旧阶段叙事，需要逐步补充状态标注
- 标准 provider 的实际兼容性说明仍需要更多运行验证和沉淀
- 飞书通道尚未接入实际 API（`FrontendKind::Feishu` 变体与 `channels/lark.rs` 占位已移除，待实现时随真实通道一并加回）
- Telegram 通道已支持收发媒体附件（图片、文档、语音等）与 Inline Keyboard 审批交互；QQ 通道已支持收发媒体附件与审批文本回复匹配
- Telegram webhook 模式仍由轮询替代，尚未切换（注：信号触发系统的 webhook 服务器已基于 axum 实现，与 Telegram webhook 模式是不同功能）
- Brain LLM 选 Agent + skill 的链路已建立（`brain_dispatch_system` → `brain_decision_system` →
  `dispatch_system`），但实际 LLM 选错场景的集成测试仍需补充
- Agent loop 控制：真实 provider 对"tool 结果后追加 user 文本"的接受度用例已就绪（Layer 1 门控 +
  Layer 2 场景），OpenAI 兼容协议线上形态已由 wiremock 用例验证，其余 provider 待持有 Key 时人工执行
- 异步工具桥双轨期待完善：`ToolContext<'a>` 借用上下文尚未完全退役；剩余 Sync 工具

### 已收敛或已废弃

- `Plan` 不再作为独立运行时模块存在，收敛为任务分解能力
- `Planning WorkItem` 已删除，不再作为未来预留项保留
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役
- 动态定时任务双账本已合并：`DynamicScheduledTask`、`ScheduledTaskInfo`、`ScheduledTaskRegistry` 及其快照类型、
  `OwnedToolContext.registry` 均已删除，统一由 `ScheduleDefinition` 承载
- `schedule_task` 专用 commit 链路已退役：`schedule_task_commit_system`、
  `ScheduleTaskRequestMessage`、`ScheduleTaskCommitPending`、`ToolAction::ScheduleTask`
  变体均已删除，写路径统一经 `ToolEffect::ScheduleTask` + `commit_tool_effects_system`
  落账
- `ExperienceCollectionTracker` 与 task-scoped agent 保活逻辑已移除，经验收集改为独立 WorkItem
- `spawn_agent` Tool 已废弃并从 LLM 可调工具集中移除；子 Agent 创建统一收敛到
  `create_tasks` + Brain 调度内部生成的 `AgentSpawnRequestMessage`
- 插件 Host API 的 `spawn_agent` 函数与 `WorldCommand::SpawnAgent` 已移除
- 旧派发 system `task_dispatch_system`、`workitem_dispatch_system` 已删除，统一收敛到
  `dispatch_system`
- `agent_selection.rs` 已删除（tag 匹配逻辑收敛到 `dispatch_system` +
  `WorkItemType::required_tag()`）
- `WorkItem.tags` 字段已删除，由 `WorkItemType::required_tag()` 集中映射替代
- `contracts/dispatch.rs` 中未使用的 trait（`TagMatcher`、`AgentSelector`、`DispatchPolicy`、
  `TagBasedSelector`、`SummarizerSelectionPolicy` 等）已删除

## 当前架构结论

### Task 与 WorkItem 边界

- `Task` 代表用户真正想完成的事情
- `WorkItem` 代表为完成 `Task` 而派生的内部工作
- 控制状态如等待、审批、工具循环不等同于 `WorkItem`

### Plan 与 Evaluation 的收敛结论

- `Plan` 的职责已被任务分解链路覆盖，不再推进独立模块
- `Evaluation` 保留独立语义层，但执行复用统一 `WorkItem` 链路
- `Summarization` 与 `Evaluation` 都优先服务于运行时治理，而不是用户直接任务

### Shell 工具的收敛结论

- 阻塞执行走 `shell_exec`
- 异步会话走 `shell_start -> shell_read / shell_list -> shell_input / shell_stop`
- 输出读取统一为最新窗口快照
- 会话只允许由创建它的 `Task` 访问

### Space 边界的收敛结论

- `SharedKnowledgeBase` 负责承载用户显式写入及审核后共享的知识，当前仍为进程内存态
- `SpaceToolRegistry` 负责承载全局工具定义
- shell session 真源位于 `NativeProcessBackend`，不再作为 `Space` 资源建模

### 模块依赖方向与复杂度治理

- 单向分层依赖（下层不引用上层，同层括号内不互相引用）：
  `domain ← contracts ← {ecs, llm, channels, tui, infrastructure} ← {user_plugins, triggers} ← systems ← {plugins, app}`；
  `lib.rs` 不再含模块级 glob re-export，依赖方向经 `scripts/check_module_deps.sh` CI 断言固化。
- 知识域重组：`transform/llm_response.rs` 回归纯路由（1908→476 行），评估/摘要/画像结果处理归位各自领域模块，工具调用编排归位 `systems/tools/`，brain 决策三文件合并为 `systems/brain/`。
- 领域类型收紧：`TaskId`/`AgentId` 等改 newtype 杜绝互传；`Task`/`WorkItem` 状态字段
  `pub(crate)` + `mark_*` 转换；`TaskRoutingPolicy` 工厂构造；状态直接赋值经
  `scripts/check_status_assignment.sh` CI 断言。
- 登记性知识收口：`FrontendKind` 为通道名唯一权威（`channel_name`/`from_channel_name`），
  审批 `callback_data`、`agents.toml` 解析、skill 布局读写、QQ 配置写入均收口到单一模块/接口。
- 详细阶段任务与实施记录见
  [docs/design/2026-08-17-complexity-governance-design.md](design/2026-08-17-complexity-governance-design.md) §3 与 §8。

## 已知限制

### 审批链路限制

- 审批 UI、消息流和状态切换已具备
- 当前 `approval_dispatch_system` 仍使用自动通过逻辑，不是最终目标态

### 文档限制

- 文档索引入口为 `docs/README.md`，当前状态以本文档为准
- 历史设计文档已归档到 `docs/archive/design/`，仅供查阅演进脉络

### Provider 限制

- `openai`、`anthropic`、`deepseek`、`openai-compatible` 已接入统一执行器
- 标准 provider 更多依赖底层 `genai` 的默认接入方式，使用时需结合真实环境验证

## 推荐阅读顺序

1. `docs/README.md` — 文档索引入口
2. `AGENTS.md` — 项目规范
3. `README.md` — 项目简介
4. `docs/configuration.md` — 配置说明
5. `docs/TODO.md` — 待办事项
6. `docs/wiki/system-pipeline.md` — 系统管线流程与 System 注解
7. `docs/wiki/llm-context-assembly.md` — LLM 上下文组装机制与例子
8. `docs/design/2026-06-06-workitem-boundary-design.md` — Task 与 WorkItem 边界
9. `docs/design/2026-06-06-plan-evaluation-reassessment-design.md` — Plan 收敛与 Evaluation 重定位
10. `docs/design/2026-07-18-dispatch-architecture-unification-design.md` — 派发架构统一
11. `docs/design/README.md` — 设计文档索引
12. `docs/superpowers/README.md` — 当前活跃计划与规格
13. `docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md` — Skill 一等公民与经验治理改造
