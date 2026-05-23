# AI Harness 项目规范

本文档定义了项目的核心原则、行为约束和技术边界。AI Agent 在参与本项目时必须遵守以下规范。

---

## 项目概述

AI Harness 是一个基于 ECS 架构的 AI harness 软件框架，使用 Rust + Bevy 构建。

__当前阶段__：MVP 主链路已实现，待进行真实 OpenAI 联调验证。

---

## 核心原则

### 文档先行

- 前期阶段以文档为核心，代码在文档完善后开始编写
- 架构设计必须先形成文档，经评审通过后方可实施
- 代码变更必须同步更新相关文档

### 规范驱动

- 所有工作遵循本规范定义的流程和约束
- 规范变更需要通过 ADR 记录决策过程
- 发现规范冲突时，优先上报讨论，不自作主张

### 用户确认

- 重大决策必须提交用户确认
- 设计文档完成自审后，展示给用户评审
- 用户确认是执行的前提

---

## 行为约束

### 必须做

- 遵循 Conventional Commits 提交规范
- 所有变更通过 PR 合并，禁止直接推送到 main
- 提交前进行自审，确保符合规范
- 文档和代码变更放在同一 commit
- 使用中文撰写文档，可夹杂英文单词

### 禁止做

- 禁止跳过评审流程直接实施重大变更
- 禁止强制推送到 main 分支
- 禁止引入不符合依赖原则的 crate
- 禁止在未评审的情况下合入 PR

---

## 技术边界

### 语言与框架

- 语言：Rust（遵循官方风格指南）
- 框架：Bevy（ECS 架构）
- 文档：Markdown（markdownlint 规则）

### 依赖原则

引入第三方依赖必须满足：

1. 来源：仅 crates.io，不使用 git 依赖
2. 许可证：MIT 或 Apache-2.0 兼容
3. 实现：优先纯 Rust 实现，保证跨平台兼容性
4. 必要性：按需引入，避免过度依赖

### 错误处理

- 库 crate：使用 `thiserror` 定义错误类型
- 应用/主程序：使用 `anyhow` 处理错误

### 日志规范

使用 `tracing` crate：

| 环境   | 默认级别 |
|--------|----------|
| 生产   | INFO     |
| 开发   | DEBUG    |

#### 日志级别使用

| 级别 | 用途 | 示例场景 |
|------|------|----------|
| `trace!` | 高频事件、周期性检查 | tick_clock_system、心跳检测 |
| `debug!` | 数据流转、状态转换、决策过程 | 任务创建、Agent 选择、响应处理 |
| `info!` | 重要业务事件、外部交互 | 任务完成、摘要触发（生产可见） |
| `warn!` | 异常但可恢复的情况 | Tool 执行拒绝、降级处理 |
| `error!` | 错误场景，必须附带完整现场信息 | 执行失败、认证错误 |

**高频日志使用 trace**：每帧都可能执行的日志使用 `trace!`，避免日志泛滥。例如：

- `tick_clock_system`：每帧更新时钟
- 心跳检测、健康检查等周期性任务
- 空轮询检查（无数据时）

#### 统一格式要求

所有日志必须使用结构化字段，格式如下：

```rust
debug!(
    event = "EventName",      // 必需：事件名称（PascalCase）
    field1 = value1,          // 业务字段
    field2 = value2,
    "human readable message"  // 简短描述
);
```

#### 必需字段

| 场景 | 必需字段 |
|------|----------|
| 所有日志 | `event` - 事件名称 |
| Task 相关 | `task_id` |
| Agent 相关 | `agent_id`, `agent_name` |
| 错误 | `error`, `error_type` |

#### 数据级日志要求

本项目采用数据级日志，记录完整数据流转：

1. **完整内容记录**: 记录完整的 prompt、响应内容、STM 条目等
2. **不截断不脱敏**: 调试需要完整上下文
3. **状态转换追踪**: 每次状态转换记录 from/to/reason

```rust
// 示例：任务状态转换日志
debug!(
    event = "TaskStatusTransition",
    task_id = %task.id,
    from_status = ?old_status,
    to_status = ?new_status,
    reason = "llm_response",
    response_content = %content,
    stm_entries = stm.entries.len(),
    "task status changed"
);
```

#### 错误现场规范

错误日志必须包含完整现场信息：

```rust
error!(
    // === 核心字段 ===
    task_id = %task.id,
    task_status = ?task.status,
    task_content = %task.content,
    retry_count = task.retry_count,
    last_error = ?task.last_error,

    // === STM 状态 ===
    stm_entries = stm_entries_count,
    stm_tokens = stm_tokens_count,
    stm_recent = ?recent_entries,

    // === 请求详情 ===
    agent_id = %agent_id,
    request_kind = ?request_kind,
    prompt_len = prompt_len,

    // === 错误本身 ===
    error = %error,
    error_type = std::any::type_name_of_val(&error),

    "execution error with full context"
);
```

#### 日志层级

按数据流转分层记录：

| 层级 | 系统 | 关注点 |
|------|------|--------|
| Ingress | input_ingress_system | 外部输入 |
| Signal | signal_ingest_system | 信号转换 |
| Command | command_parse_system | 命令解析 |
| Routing | user_input_routing_system | 路由决策 |
| Dispatch | task_dispatch_system | Agent 选择、prompt 构建 |
| Execution | agent_execution_system | 请求提交 |
| Response | llm_response_system | 响应处理、状态转换 |
| Memory | memory_compression_system | 记忆压缩 |
| Tool | tool_dispatch_system | 工具执行 |

#### 事件命名规范

事件名称使用 PascalCase，动词开头表示动作，名词开头表示状态：

| 模式 | 示例 |
|------|------|
| 动作完成 | `TaskCreated`, `AgentSelected`, `PromptBuilt` |
| 状态变化 | `TaskStatusTransition`, `StmEntryAdded` |
| 错误发生 | `TaskFailed`, `ToolExecutionFailed` |
| 触发事件 | `CompressionTriggered`, `SummarizationDispatched` |

---

## 流程规范

### 设计评审流程

```text
设计文档编写 → 自审 → 提交用户评审 → 用户确认 → 实施
```

评审标准：

- 与本规范一致性
- 内部逻辑无矛盾
- 技术方案合理

### 分支策略

采用 GitHub Flow：

1. 从 main 创建功能分支
2. 在功能分支上进行开发
3. 提交 PR，通过 CI 检查和人工审核
4. 合并到 main

### 提交信息格式

采用 Conventional Commits：

```text
<type>: <description>

[optional body]
```

常用类型：

- `feat`: 新功能
- `fix`: Bug 修复
- `docs`: 文档变更
- `refactor`: 重构
- `test`: 测试相关
- `chore`: 构建/工具变更

---

## 测试规范

- 单元测试：与代码同文件（`#[cfg(test)]`）
- 集成测试：放于 `tests/` 目录
- 前期不强求覆盖率，关键逻辑应有测试

---

## 文档规范

### 目录结构

```text
docs/
├── design/       # 设计文档
├── adr/          # 架构决策记录
└── api/          # API 文档（cargo doc 生成）
```

### ADR 格式

架构决策记录采用轻量级格式，参见 `docs/adr/ADR-000-template.md`。

### Markdown 规则

- 标题层级递增，不跳级
- 每篇文档仅一个 H1
- 代码块必须标注语言
- 列表符号统一用 `-`

---

## CI/CD 配置

### 检查项

__阶段一（文档）__：

- markdownlint

__阶段二（代码）__：

- markdownlint
- cargo fmt --check
- cargo clippy
- cargo test

### 触发条件

- Pull Request（任意分支）
- Push 到 main
- 手动触发

---

## 状态记录

本项目使用 Issue 和 PR 模板管理任务和变更，参见 `.github/` 目录。

---

## 规范更新

本规范随项目发展持续演进。更新流程：

1. 提出变更建议（Issue 或 PR）
2. 讨论并达成共识
3. 更新本文档
4. 记录 ADR（如涉及重大变更）
