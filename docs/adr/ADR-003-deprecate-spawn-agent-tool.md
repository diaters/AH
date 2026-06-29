# ADR-003: 废弃 `spawn_agent` Tool

## 状态

Accepted

## 生效范围

本决策关联以下文档与模块：

- `docs/current-state.md`（任务分解能力描述）
- `docs/framework-architecture-analysis.md`（路径 D：子任务创建与等待）
- `src/systems/tools/builtin/spawn_agent.rs`
- `src/systems/tools/orchestrator.rs` 中 `spawn_spawn_agent_messages`
- `src/systems/maintenance.rs` 中 `agent_factory_system`
- `src/domain/message.rs` 中 `AgentSpawnRequestMessage`

## 背景

项目早期通过 `spawn_agent` Tool 探索“Agent 自主创建子 Agent”的能力，相关设计规格为
`docs/archive/superpowers/specs/2026-05-23-agent-spawn-subagent-design.md`，该规格已于
2026-06-10 归档。

随着任务分解链路收敛，`create_tasks` + DAG 调度 + `wait_tasks` 已成为当前框架的主链路。在此链路中：

1. 子任务以独立的 `Task` 实体存在，具备 `parent_task_id` 与 `batch_id`；
2. Brain 调度器基于 Agent `tags` 为子任务匹配执行 Agent；
3. `agent_factory_system` 统一创建 `TaskScoped` Agent 执行子任务；
4. `SubTaskBatchState` 管理批次状态与依赖结果回传。

相较之下，`spawn_agent` Tool 仍存在以下问题：

- __绕过了 Brain 调度与 DAG 管理__：直接在当前 `Task` 上生成 `AgentSpawnRequestMessage`，没有独立的子任务实体；
- __与原任务语义冲突__：创建子 Agent 后会修改原任务的 `delegate`，但父 Agent 的 Tool 调用循环尚未结束，职责边界模糊；
- __没有结果回收机制__：不生成 `SubTaskBatchState`，也无法与 `wait_tasks` 协作；
- __缺少当前框架的上下文透传__：例如 `origin_channel` 继承、`SubTaskConfig` 依赖信息等；
- __使用场景被 `create_tasks` 完全覆盖__：任何需要“创建子 Agent 执行子任务”的场景，都应该通过 `create_tasks` 走标准子任务链路。

目前 `spawn_agent` 仅在孤立测试中被直接调用，实际工作流中已边缘化。

## 决策

1. __废弃 `spawn_agent` Tool 的对外暴露能力__：从 `SpaceToolRegistry` 默认注册中移除
   `spawn_agent`，不再作为 LLM 可调用的内置工具。
2. __保留内部 `AgentSpawnRequestMessage` 与 `agent_factory_system`__：`create_tasks` 链路仍依赖
   该消息创建 `TaskScoped` Agent，此内部机制继续保留并维护。
3. __保留 `AgentKind::TaskScoped`__：`TaskScoped` Agent 仍是 `create_tasks` 子任务的执行载体，与本次废弃无关。
4. __移除 `src/systems/tools/builtin/spawn_agent.rs` 及 orchestrator 中对应的处理分支__，或将其标记为
   内部保留但不对 LLM 暴露。
5. __同步更新测试__：移除仅用于验证 `spawn_agent` Tool 调用路径的孤立测试，或将它们改写为验证
   `create_tasks` 链路创建 `TaskScoped` Agent 的等价测试。
6. __同步更新文档__：
   - 在 `docs/current-state.md` 的“已收敛或已废弃”一节补充说明；
   - 在 `docs/framework-architecture-analysis.md` 中移除或标注 `spawn_agent` 相关路径；
   - 更新 `docs/README.md` 与 `docs/design/README.md` 索引。

## 后果

### 正面

- 消除 `spawn_agent` 与 `create_tasks` 之间的语义重叠，统一任务分解入口；
- 减少 LLM 可选工具数量，降低误用风险；
- 所有子 Agent 创建都经过 Brain 调度与批次管理，便于审计、依赖控制和结果回收；
- 简化测试与文档维护面。

### 负面

- 如果未来确实需要“Agent 在运行中直接孵化一个 Agent 来接替当前任务”的能力，需要重新设计，不能直接复用当前
  `spawn_agent`；
- 需要一次性的代码清理和文档同步工作；
- 插件 Host API 中的 `WorldCommand::SpawnAgent` 变体需要同步评估是否一并移除或保留为内部命令。

## 实施要点

1. 移除 `src/systems/tools/builtin/spawn_agent.rs`；
2. 移除 `src/systems/tools/mod.rs` 中对 `spawn_agent` 的注册；
3. 移除 `src/systems/tools/orchestrator.rs` 中 `Ok(ToolAction::SpawnAgent { ... })` 分支及
   `spawn_spawn_agent_messages` 函数；
4. 移除 `tests/tool_execution_flow.rs` 中专门测试 `spawn_agent` Tool 的用例，或迁移为
   `create_tasks` 链路测试；
5. 检查 `src/user_plugins/host_api/entity_write.rs` 等插件相关代码中是否仍引用 `spawn_agent`，并同步处理；
6. 更新 `docs/current-state.md`、`docs/framework-architecture-analysis.md` 及相关索引。
