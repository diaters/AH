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
- `Space` 已收敛为最小共享资源边界，当前只保留 `SpaceKnowledge` 和
  `SpaceToolRegistry`
- shell 工具已收敛为六个意图化工具：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- shell 输出语义已收敛为“最新快照”，不再对 LLM 暴露伪增量游标协议

### 待完善

- 父 Agent 审批仍是 MVP 自动通过实现，需要替换为真实 LLM 审查
- 历史设计文档仍有一部分使用旧阶段叙事，需要逐步补充状态标注
- 标准 provider 的实际兼容性说明仍需要更多运行验证和沉淀

### 已收敛或已废弃

- `Plan` 不再作为独立运行时模块存在，收敛为任务分解能力
- `Planning WorkItem` 已删除，不再作为未来预留项保留
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役

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

- `SpaceKnowledge` 负责承载用户显式写入的共享知识，当前仍为进程内存态
- `SpaceToolRegistry` 负责承载全局工具定义
- shell session 真源位于 `NativeProcessBackend`，不再作为 `Space` 资源建模

## 已知限制

### 审批链路限制

- 审批 UI、消息流和状态切换已具备
- 当前 `approval_dispatch_system` 仍使用自动通过逻辑，不是最终目标态

### 文档限制

- `README.md`、`AGENTS.md`、`docs/current-state.md` 是当前面向使用者的主要入口
- `docs/design/` 中部分旧文档仍可用于理解历史演进，但不一定代表当前实现

### Provider 限制

- `openai`、`anthropic`、`deepseek`、`openai-compatible` 已接入统一执行器
- 标准 provider 更多依赖底层 `genai` 的默认接入方式，使用时需结合真实环境验证

## 推荐阅读顺序

1. `AGENTS.md`
2. `README.md`
3. `docs/configuration.md`
4. `docs/TODO.md`
5. `docs/design/2026-06-06-workitem-boundary-design.md`
6. `docs/design/2026-06-06-plan-evaluation-reassessment-design.md`
7. `docs/design/README.md`
8. `docs/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`
9. `docs/superpowers/specs/2026-06-09-space-module-convergence-design.md`
