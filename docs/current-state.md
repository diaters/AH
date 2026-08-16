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
- 结构化日志、CI、集成测试与回归测试已接入主流程
- Android aarch64 支持（rustls TLS 后端），可通过交叉编译在 Android 设备运行

#### 任务与执行模型

- `Task` 作为用户目标主实体
- `WorkItem` 作为内部执行单元
- `Evaluation` 与 `Summarization` 已迁移到 `WorkItem` 闭环
- `AgentExecutionRequest` 作为瞬时执行请求，不承担长期业务状态

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
  LLM 失败直接标记 Task 为 `Failed`，不 fallback
- skill 注入：对所有 Task 适用（max 1），仅限 Persistent Agent，通过 `TaskInjectedSkill`
  Component 注入
- WorkItem 创建器/派发器职责切分：summarization、evaluation、experience_collection、
  profile_generation、skill_update 系统仅创建 WorkItem + 附加 `PendingDispatch`，不再直接派发

#### 多模型与降级

- Per-Agent 多模型/多提供商差异化调度
- `ExecutorRegistry` 管理多个 provider executor
- `ModelChainState` Component 追踪降级状态
- 429（限流）/402（配额耗尽）错误自动降级到下一优先级
- 冷却期自动恢复到原优先级
- `providers.toml` 配置文件支持多 provider 实例
- 向后兼容：现有 `model` 字段和环境变量配置继续工作

#### 测试分层（真实 LLM 场景测试）

- 四层测试模型与三级正确性判断体系已确立（设计文档
  `docs/design/2026-08-16-real-llm-scenario-testing-design.md`）
- Layer 0：共享 mock executor 基础设施已收敛（`tests/common/mock_executor.rs`），
  genai 适配层纯函数已有无网络单元测试
- Layer 1：真实 LLM 冒烟测试已可用（`tests/real_llm_smoke.rs`），
  采用 `#[ignore]` + `HARNESS_TEST_REAL_LLM` 双重门控，永不进入 CI
- Layer 2/3：声明式场景测试框架已可用（`tests/real_llm_scenarios.rs` +
  `tests/scenarios/*.toml`）——TOML 场景定义五类断言
  （`tool_called` / `state_reached` / `response_matches` / `llm_judge` / `human_review`），
  产出 Markdown 报告、待审队列与金标准快照；框架自检（mock executor）随 CI
  常规运行，真实场景手动执行
- LLM-as-Judge 已成为 harness 一等能力：`JudgeVerdict` / `JudgeRubric` /
  `parse_judge_verdict`（`src/domain/evaluation.rs`）与 Judge prompt 构建
  （`src/llm/judge_prompt.rs`），复用 `AgentRequestKind::Evaluation` 请求通道；
  采样投票 + 低置信/分裂降级人工待审

#### 工具与会话

- 工具调用软限制：单轮用户输入内达到 `HARNESS_MAX_TOOL_ITERATIONS` 后返回合成 tool result，让 LLM 总结并询问用户；绝对硬上限（HARD_LIMIT_MULTIPLIER × max_iterations）时强制失败
- 工具权限、审批流程、结果回写与用户确认 UI 已可用
- `chat_with_agent` 工具：支持父任务与 Persistent Agent 多轮同步对话
- `ask_user` 工具：LLM 在工具调用循环中向用户提出开放文本问题，用户回复作为工具结果返回（声明式 Sync 工具，详见 [async-tool-bridge.md](async-tool-bridge.md#sync-工具分类)）
- `Space` 已收敛为最小共享资源边界，当前只保留 `SharedKnowledgeBase` 和
  `SpaceToolRegistry`
- shell 工具已收敛为六个意图化工具：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- shell 输出语义已收敛为"最新快照"，不再对 LLM 暴露伪增量游标协议
- 同一任务的多个工具确认请求现在按顺序逐个弹出；`allow_always` 授权的权限会立即复用，后续同工具请求直接执行
- 等待工具确认期间，文本输入 `1`/`2`/`3` 会被识别为确认选项（QQ 文本确认），其他文本会提示重试而不是创建新任务

#### 工具权限决策链路

- 工具权限决策采用三层回退：`agent.overrides` → `agent.default_permission`（显式配置时）→ `ToolDefinition.default_permission`
- `Agent::effective_permission(tool_name, registry)` 是权限查询单一入口
- `default_permission_explicit` 字段区分显式/隐式 Confirm，仅隐式 Confirm 回退到工具默认
- 子 Agent 权限继承：父 Confirm → 子 Confirm（不再降为 Allow）
- `EngineEvent::PermissionAudit` 审计事件覆盖 dispatch / async_dispatch / confirmation / approval 路径

#### 异步工具桥

- 异步工具桥已落地：`kind() == Async` 的工具请求经「dispatch 挂起 → tokio worker 异步执行
  → 通道回传 → ingest 落地」闭环，「不堵 ECS」从约定升级为构造。详见
  [async-tool-bridge.md](async-tool-bridge.md)
- 双轨期：`ToolActionKind` 缺省 `Sync`，异步工具 override 为 `Async`；dispatch 按 `kind()`
  分流，Sync 工具继续走旧路径，禁止新增 Sync 工具
- 六条架构不变量：统一异步、快照进效果出、compute/apply 切分、双账本单一修改入口、
  结果落地单点 + exactly-once、双超时分层
- 三条失联路径（worker panic / 业务超时 / 通道断开）经 sweeper claim 殊途同归到 ingest
  单点落地，exactly-once 由「挂起实体是否还在」唯一裁决
- 父任务终态触发 worker 取消：`cancel_monitor_system` 调 `CancellationToken.cancel()`，
  worker `select!` 监听后 kill 子进程并回送 cancelled error
- 通用效果提交：写工具返回声明式 `ToolEffect`，由 `commit_tool_effects_system`（exclusive
  system）经 `update_scheduler_state` 双资源入口原子落账，结果回送通道由 ingest 下一帧落地
- 动态定时任务管理三件套已上桥：
  - `list_scheduled_tasks`（纯读，pilot 工具）
  - `delete_scheduled_task`（写路径，idempotent 语义）
  - `schedule_task`（写路径，经 `ToolEffect::ScheduleTask` 落账）
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
- Telegram 通道消息撤回（`deleteMessage`）与输入状态指示器（`sendChatAction` with `typing`）
- QQ 通道接入（WebSocket Gateway、OAuth2、markdown/富媒体发送、审批文本回复匹配）
- QQ 文本回复 `1`/`2`/`3` 可直接作为工具确认选项，非选项文本会收到重试提示
- QQ 通道消息撤回 API（C2C / 群聊 DELETE 端点）
- QQ 通道输入状态指示器（typing indicator, POST /v2/users/{openid}/typing）
- QQ 通道交互回调（PUT /interactions/{id}）
- QQ 通道交互事件监听（INTERACTION_CREATE）与按钮点击闭环
- QQ 通道审批消息使用原生 InlineKeyboard 按钮交互（含 reject_with_feedback 两步流程）
- QQ 通道 send 方法返回 message_id（QqMessageResponse）
- `channel_send` 工具主动推送
- `origin_channel` 从入向消息透传到 `Task`
- IM 出向-自动回执：Agent 文本回复、`SystemOutputMessage`、任务失败提示等按 `output_channel`
  （通常即来源 `origin_channel`）自动推回来源 IM 通道，并附加任务短 ID 前缀（如 `[a1b2c3d4]`），
  便于同一会话中多任务并行时区分消息来源
- IM 任务状态展示：任务状态变更（如运行中 → 等待中）作为独立状态消息推送到 IM 通道，同样携带任务短 ID 前缀
- 跨通道隔离：用户输入仅路由到同一通道中等待用户的 Task；`/finish`、`/summarize`、`/btw` 等命令限定在发出通道生效；子任务继承父任务的 `origin_channel`

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
- `schedule_task` 任务的 `output_channel` 默认继承当前任务 `origin_channel`，显式指定时可覆盖到 `tui`/`telegram`/`qq`/`feishu`/`web`
- 动态 scheduled task 触发后，其 `output_channel` 同时作为审批通道，执行期需要用户确认的工具请求会路由到该 IM 用户；若对应 frontend 未注册，任务将明确失败并记录 `FrontendApprovalRouteInvalid`
- 动态 scheduled task 仅存内存，进程重启后丢失；一次性任务触发后自动从 registry 清理，并记录 `DynamicTaskRemoved` 结构化日志

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
- hook 脚本执行受 1 秒超时保护，按插件字母序顺序派发
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
  构造 `SkillRegistry`；SKILL.md frontmatter 支持 `version` 与 `self_updatable` 字段
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

### 待完善

- 父 Agent 审批仍是 MVP 自动通过实现，需要替换为真实 LLM 审查
- 插件 host API 部分 `WorldCommand` 变体（`CreateWorkItem`、`SetApprovalDecision`、`ExperienceSetPinned`）尚未实现回放
- 插件 `v1` 不追踪 `tool_deny` 的 per-plugin attribution，推迟到后续 host API 升级
- 历史设计文档仍有一部分使用旧阶段叙事，需要逐步补充状态标注
- 标准 provider 的实际兼容性说明仍需要更多运行验证和沉淀
- 飞书通道仅有占位模块，尚未接入实际 API
- Telegram 通道已支持收发媒体附件（图片、文档、语音等）与 Inline Keyboard 审批交互；QQ 通道已支持收发媒体附件与审批文本回复匹配；飞书仍为占位模块
- Telegram webhook 模式仍由轮询替代，尚未切换（注：信号触发系统的 webhook 服务器已基于 axum 实现，与 Telegram webhook 模式是不同功能）
- Brain LLM 选 Agent + skill 的链路已建立（`brain_dispatch_system` → `brain_decision_system` →
  `dispatch_system`），但实际 LLM 选错场景的集成测试仍需补充
- 异步工具桥双轨期待完善：`ToolContext<'a>` 借用上下文尚未完全退役；剩余 Sync 工具

### 已收敛或已废弃

- `Plan` 不再作为独立运行时模块存在，收敛为任务分解能力
- `Planning WorkItem` 已删除，不再作为未来预留项保留
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役
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
