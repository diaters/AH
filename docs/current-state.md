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

#### 任务与执行模型

- `Task` 作为用户目标主实体
- `WorkItem` 作为内部执行单元
- `Evaluation` 与 `Summarization` 已迁移到 `WorkItem` 闭环
- `AgentExecutionRequest` 作为瞬时执行请求，不承担长期业务状态

#### 协作与编排

- Brain 调度与多 Agent 配置加载已接入
- 任务分解通过 `create_tasks` + DAG 调度 + `wait_tasks` 实现
- 子任务结果可以回传父任务，支持继续执行

#### 工具与会话

- 工具权限、审批流程、结果回写与用户确认 UI 已可用
- `Space` 已收敛为最小共享资源边界，当前只保留 `SharedKnowledgeBase` 和
  `SpaceToolRegistry`
- shell 工具已收敛为六个意图化工具：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- shell 输出语义已收敛为”最新快照”，不再对 LLM 暴露伪增量游标协议

#### IM 通道

- 统一 `Channel` 抽象与 `ChannelManager`（含 listen 重启退避与 shutdown）
- Telegram 通道接入（长轮询、白名单、文本分块发送）
- QQ 通道接入（WebSocket Gateway、OAuth2、markdown/富媒体发送、审批文本回复匹配）
- `channel_send` 工具主动推送
- `origin_channel` 从入向消息透传到 `Task`
- IM 出向-自动回执：Agent 文本回复按 `origin_channel` 自动推回来源 IM 通道

#### 记忆治理

- 记忆系统已收敛为 `ShortTermMemory`、`LongTermMemory`、`SharedKnowledgeBase`
- `AgentExperience` 已删除，不再作为独立运行时概念保留
- `LongTermMemory` 采用 `Core + Relevant` 的受控注入策略，避免全量拼接 prompt
- 共享知识写入默认仅允许用户显式命令或主控审核链路，不允许普通 Agent 直写
- 长期记忆已具备基础衰退治理能力，会结合访问时间、重要度与复用次数更新分数
- 长期记忆已实现淘汰机制：`decay_score < 0.1` 且非 `pin` 非 `Critical` 的条目被移除并归档到 `<agent-name>/archive.jsonl`
- 长期记忆已实现 JSON 文件持久化（`MemoryStore` + `MemoryRepository` + `LongTermMemoryService` 写穿模型）
- Agent 启动时可从持久层恢复 `LongTermMemory`，子 Agent 贡献吸收后立即落盘

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

### 待完善

- 父 Agent 审批仍是 MVP 自动通过实现，需要替换为真实 LLM 审查
- 插件 host API 部分 `WorldCommand` 变体（`SpawnAgent`、`CreateWorkItem`、`SetApprovalDecision`、`ExperienceSetPinned`）尚未实现回放
- 插件 `v1` 不追踪 `tool_deny` 的 per-plugin attribution，推迟到后续 host API 升级
- 历史设计文档仍有一部分使用旧阶段叙事，需要逐步补充状态标注
- 标准 provider 的实际兼容性说明仍需要更多运行验证和沉淀
- 飞书通道仅有占位模块，尚未接入实际 API
- Telegram 通道已支持收发媒体附件（图片、文档、语音等）与 Inline Keyboard 审批交互；QQ 通道已支持收发媒体附件与审批文本回复匹配；飞书仍为占位模块
- Telegram webhook 模式仍由轮询替代，尚未切换

### 已收敛或已废弃

- `Plan` 不再作为独立运行时模块存在，收敛为任务分解能力
- `Planning WorkItem` 已删除，不再作为未来预留项保留
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役
- `ExperienceCollectionTracker` 与 task-scoped agent 保活逻辑已移除，经验收集改为独立 WorkItem

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
