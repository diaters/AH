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

---

## 进行中

- [ ] 架构设计文档评审

---

## 待办

### MVP 实现（极简）

- [ ] 确定输出机制（stdout / 日志 / 其他）
- [ ] 确定 LLM SDK 选型
- [ ] 确定 Bevy 版本
- [ ] 初始化 Rust 项目结构
- [ ] 实现 SignalIngestSystem
- [ ] 实现 UserMessageToTaskSystem
- [ ] 实现 TaskDispatchSystem（直接 LLM 调用）
- [ ] 实现 LlmResponseSystem
- [ ] 实现 UserOutputSystem
- [ ] 集成测试：单轮对话闭环

### GitHub 仓库配置

- [ ] 创建 GitHub 远程仓库
- [ ] 推送代码到远程
- [ ] 配置分支保护规则：
  - 禁止直接推送
  - 禁止强制推送
  - 禁止删除
  - 必须通过 PR 审核

### Phase 2: Brain Agent 调度

- [ ] 新增 BrainDispatchSystem
- [ ] 新增 BrainDecisionSystem
- [ ] 定义 Brain prompt 模板
- [ ] 定义 Brain 决策结果解析
- [ ] 改造 TaskDispatchSystem 接收 Brain 决策

### Phase 3: 多 Agent 支持

- [ ] 新增 AgentFactorySystem
- [ ] Agent 配置文件加载
- [ ] 任务型 Agent 创建/销毁逻辑
- [ ] Agent 能力匹配逻辑

### Phase 4: 高级功能

- [ ] Memory 实体设计
- [ ] Tool / ToolCall 设计
- [ ] Session 概念设计
- [ ] Planner 模块设计
- [ ] 多轮对话上下文管理

---

## 备注

- 当前阶段：文档先行，代码在文档完善后开始编写
- 所有重大变更需要通过 PR 审核流程
- 架构设计文档：`docs/design/2026-05-10-core-flow-design.md`
