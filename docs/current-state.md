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
- Brain 派发子任务时按 `SubTaskConfig.child_agent_name` 选定执行 Agent，并通过 LLM 在该 Agent
  的 `owner_skills` 中选 0 或 1 个 skill 注入子任务（仅暴露 `name` + `description` 给 LLM），
  选中后 spawn `TaskInjectedSkill` Component；LLM 选错或 owner_skills 为空时 fallback 到无 skill 路径

#### 多模型与降级

- Per-Agent 多模型/多提供商差异化调度
- `ExecutorRegistry` 管理多个 provider executor
- `ModelChainState` Component 追踪降级状态
- 429（限流）/402（配额耗尽）错误自动降级到下一优先级
- 冷却期自动恢复到原优先级
- `providers.toml` 配置文件支持多 provider 实例
- 向后兼容：现有 `model` 字段和环境变量配置继续工作

#### 工具与会话

- 工具调用软限制：单轮用户输入内达到 `HARNESS_MAX_TOOL_ITERATIONS` 后返回合成 tool result，让 LLM 总结并询问用户；绝对硬上限（HARD_LIMIT_MULTIPLIER × max_iterations）时强制失败
- 工具权限、审批流程、结果回写与用户确认 UI 已可用
- `chat_with_agent` 工具：支持父任务与 Persistent Agent 多轮同步对话
- `Space` 已收敛为最小共享资源边界，当前只保留 `SharedKnowledgeBase` 和
  `SpaceToolRegistry`
- shell 工具已收敛为六个意图化工具：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- shell 输出语义已收敛为"最新快照"，不再对 LLM 暴露伪增量游标协议
- 同一任务的多个工具确认请求现在按顺序逐个弹出；`allow_always` 授权的权限会立即复用，后续同工具请求直接执行
- 等待工具确认期间，文本输入 `1`/`2`/`3` 会被识别为确认选项（QQ 文本确认），其他文本会提示重试而不是创建新任务

#### IM 通道

- 统一 `Channel` 抽象与 `ChannelManager`（含 listen 重启退避与 shutdown）
- Telegram 通道接入（长轮询、白名单、文本分块发送）
- QQ 通道接入（WebSocket Gateway、OAuth2、markdown/富媒体发送、审批文本回复匹配）
- QQ 文本回复 `1`/`2`/`3` 可直接作为工具确认选项，非选项文本会收到重试提示
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
  - `self_updatable = false` → 降级 `kind_hint` 为 `Knowledge`
  - `default Agent` 维持 `Skill → IncubationProposal` 路径不变
- skill-updater Agent 消费 `SkillUpdateRequestMessage`，构造 prompt 后 spawn `WorkItem`（类型为
  `WorkItemType::SkillUpdate`）+ `SkillUpdateContext` + `AgentExecutionRequestMessage`
- `submit_skill_update` 工具：LLM 提交结构化 diff 操作（`replace_section` / `add_section` /
  `remove_section` / `replace_frontmatter`），orchestrator 解析后 spawn
  `SkillUpdateCompletedMessage`
- `skill_update_completion_system` 消费 `SkillUpdateCompletedMessage`：
  - apply diff 到 SKILL.md（任一 section 未找到即整体失败）
  - 备份旧版本到 `history/v{base}.md`，写入新版本 frontmatter `version: base + 1`
  - 通过 `SkillLoader` 重建并替换 `SkillRegistry` Resource
  - 候选推进到 `Persisted`
- 失败时保留 SKILL.md 原内容不变，候选保持 `GovernanceResolved` 状态；LLM 返回 text/Err 时
  正确清理 `WorkItem` + `SkillUpdateContext` 并标记 `OnWorkItemFailed`

### 待完善

- 父 Agent 审批仍是 MVP 自动通过实现，需要替换为真实 LLM 审查
- 插件 host API 部分 `WorldCommand` 变体（`CreateWorkItem`、`SetApprovalDecision`、`ExperienceSetPinned`）尚未实现回放
- 插件 `v1` 不追踪 `tool_deny` 的 per-plugin attribution，推迟到后续 host API 升级
- 历史设计文档仍有一部分使用旧阶段叙事，需要逐步补充状态标注
- 标准 provider 的实际兼容性说明仍需要更多运行验证和沉淀
- 飞书通道仅有占位模块，尚未接入实际 API
- Telegram 通道已支持收发媒体附件（图片、文档、语音等）与 Inline Keyboard 审批交互；QQ 通道已支持收发媒体附件与审批文本回复匹配；飞书仍为占位模块
- Telegram webhook 模式仍由轮询替代，尚未切换（注：信号触发系统的 webhook 服务器已基于 axum 实现，与 Telegram webhook 模式是不同功能）
- Brain 中 `select_agent_for_sub_task_with_skill` 仍为占位实现，未接入真实 LLM 选 skill 调用，
  当前仅在 owner_skills 为空时 fallback；接入 LLM 后需要补充 LLM 选错场景的集成测试
- 治理层将 `kind_hint` 从 `Skill` 降级为 `Knowledge` 时未同步转换候选 payload，导致 writeback 路径失败
  （候选最终为 `WritebackFailed` 而非 `Persisted`）；需要补 payload 适配层或直接重新构造 Knowledge 候选
- ADR-004 §4.1 与实现存在语义偏差：`apply_skill_operations` 在 section 未找到时返回 `Err` 并整体回滚，
  与 ADR 描述的"跳过未找到 section 并继续"不一致；偏差决策待补（更新 ADR 或修正实现）
- ADR-004 §2.3 错误类型设计待定：`parse_brain_skill_selection` 当前使用 `String` 错误，未使用 typed error（如 `thiserror` 定义的 `BrainSkillSelectionError`）

### 已收敛或已废弃

- `Plan` 不再作为独立运行时模块存在，收敛为任务分解能力
- `Planning WorkItem` 已删除，不再作为未来预留项保留
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役
- `ExperienceCollectionTracker` 与 task-scoped agent 保活逻辑已移除，经验收集改为独立 WorkItem
- `spawn_agent` Tool 已废弃并从 LLM 可调工具集中移除；子 Agent 创建统一收敛到
  `create_tasks` + Brain 调度内部生成的 `AgentSpawnRequestMessage`
- 插件 Host API 的 `spawn_agent` 函数与 `WorldCommand::SpawnAgent` 已移除

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
10. `docs/design/README.md` — 设计文档索引
11. `docs/superpowers/README.md` — 当前活跃计划与规格
12. `docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md` — Skill 一等公民与经验治理改造
