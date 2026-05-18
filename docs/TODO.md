# 待办事项

本文档记录项目当前状态和待完成任务。

---

## 已完成

- [x] 项目规范讨论与确定
- [x] 创建项目规范文档（CLAUDE.md）
- [x] 创建 ADR 模板
- [x] 创建 PR 和 Issue 模板
- [x] 配置 CI 工作流
- [x] 初始化 Git 仓库
- [x] 首次提交

### 架构设计

- [x] 核心实体定义（Task / Message / Signal / Agent）
- [x] 核心 System 划分
- [x] 入口机制设计（外部线程 + channel）
- [x] 异步 LLM 集成方案（Tokio Runtime）
- [x] 错误处理策略（分层重试）
- [x] Agent 架构设计（Brain Agent + Factory 模式）
- [x] 实体字段细化（ID 类型、状态枚举）

### MVP 实现

- [x] 初始化 Rust 项目结构
- [x] 实现 SignalIngestSystem
- [x] 实现 UserMessageToTaskSystem
- [x] 实现 TaskDispatchSystem（产出 AgentExecutionRequest）
- [x] 实现 AgentExecutionSystem（异步 LLM 执行）
- [x] 实现 LlmResponseSystem
- [x] 实现 UserOutputSystem
- [x] 集成测试：单轮对话闭环
- [x] 设计文档评审与同步
- [x] 真实 LLM 联调验证（DeepSeek API）

### Phase 2: Brain Agent 调度

- [x] 新增 brain_dispatch_system
- [x] 新增 brain_decision_system
- [x] 定义 Brain prompt 模板与决策结果解析
- [x] Brain 配置（环境变量启用/关闭）
- [x] 集成测试：Brain 调度闭环
- [x] 真实 LLM 联调验证（Brain + DeepSeek API）

---

## 进行中

（无）

---

## 待办

### GitHub 仓库配置

- [x] 创建 GitHub 远程仓库
- [x] 推送代码到远程
- [x] 配置分支保护规则：
  - 禁止直接推送
  - 禁止强制推送
  - 禁止删除
  - 必须通过 PR 审核

### Phase 3: 多 Agent 支持

- [x] Agent 无状态化（移除 AgentStatus）
- [x] 新增 AgentKind（Persistent / TaskScoped）
- [x] TOML 配置文件加载持久性 Agent
- [x] 重写 AgentFactorySystem（配置加载 + 动态创建 + 销毁）
- [x] 任务型 Agent 动态创建（AgentSpawnRequestMessage）
- [x] 任务型 Task 终态自动销毁（TaskTerminatedMessage）
- [x] Agent tags 匹配逻辑
- [x] tags 子集权限继承校验
- [x] 集成测试

### Phase 4: 高级功能

- [x] Memory 实体设计（ShortTermMemory / LongTermMemory）
- [x] 多轮对话上下文管理
- [x] 记忆传承机制（子 Agent 向父 Agent 贡献）
- [x] 评估器 Agent 设计
- [x] 基于 token 的记忆压缩
- [x] 用户指令解析（/btw, /finish, /summarize）
- [x] Pending 状态支持多轮对话
- [x] Tool / ToolCall 实现（Phase 4.2 MVP 完成）

> 注：Session 概念已被 Phase 4.2 Space 设计取代，不再单独设计。
> 注：Planner 功能已被 Brain Agent + TaskScoped Agent 覆盖，不再单独设计。

#### Phase 4.2 Tool & Space 实现明细

- [x] Space Resource 骨架（SpaceKnowledge, SpacePreferences, SpaceToolRegistry, SpaceAgentRegistry, SpaceRuntimeContext）
- [x] ToolDefinition 与 ToolPermission
- [x] AgentRequestKind::ToolExecution
- [x] WaitingReason::Approval
- [x] Agent.tool_permissions 与 AgentExperience
- [x] agents.toml 扩展（tools 配置节）
- [x] Tool 执行消息（ToolExecutionRequestMessage, ToolExecutionResultMessage, ToolError）
- [x] Builtin Tool 执行器（tool_dispatch_system, tool_execution_system, tool_result_system）
- [x] ToolCall 记录到 ShortTermMemory
- [x] 审批消息（ApprovalRequestMessage, ApprovalResultMessage, ConfirmMode）
- [x] 审批流 System（approval_dispatch_system, approval_result_system）
- [x] Agent 演化 System（agent_evolution_system）
- [x] 用户确认 UI（选项式交互、永久权限、CLI channel）
- [x] 集成测试（Tool 执行流程、权限场景、确认流程）

---

## 备注

- 当前阶段：Phase 4.2 Tool & Space MVP 已完成
- 所有重大变更需要通过 PR 审核流程
- 架构设计文档：`docs/design/2026-05-10-core-flow-design.md`
- Phase 3 设计文档：`docs/design/2026-05-16-multi-agent-design.md`
- Phase 4.1 设计文档：`docs/design/2026-05-17-multi-turn-memory-design.md`
- Phase 4.2 设计文档：`docs/design/2026-05-17-tool-space-design.md`
