# Harness

Harness 是一个基于 Rust + Bevy ECS 的 AI Harness 框架，聚焦于任务驱动执行、
多 Agent 协作、工具调用、记忆治理与评估闭环，并提供基于 TUI 的本地交互入口。

## 能力状态

### 已实现

- 按需 ECS 驱动：输入、执行结果和截止时间唤醒；等待期间阻塞，TUI 按可见变化刷新
- TUI 驱动的本地交互与事件循环
- 原生客户端：HTTPS/WSS、配对鉴权、可靠同步、任务控制、审批问答、附件、自动化与 MeoW 通知；
  [HarmonyOS API 26 工作台](docs/client-channel.md) 已提供原生工程，真机验证与后续管理能力仍待完成
- `Task` 作为用户目标载体，`WorkItem` 作为内部执行单元
- 多 Agent 按职责分工：用户任务直接交给主 Agent（`HARNESS_MAIN_AGENT`，默认 `default-llm-agent`），
  摘要员、evaluator、经验管理员与子任务执行为主 Agent 服务
- `create_tasks` + `wait_tasks` 驱动的任务分解能力
- 每个完整聊天通道只保留一个未结束的用户顶层任务，运行中消息引导该任务；
  `/stop` 保留会话，`/finish` 清理后释放名额；`/btw` 只在空通道创建新主题；内部子任务与输出型定时任务可并行
- Agent loop 控制：任务运行中的输入以引导式插入（steer）在下一个工具结果边界送达，
  不打断执行；`/stop`（TUI 空输入时 `Esc`）中断当前回合并保留上下文；后台 shell 会话
  退出与子任务批次完成以 `<task-notification>` 通知所属任务，空闲任务自动重新拉起；
  `update_todos` 进度清单在 TUI / IM 展示
- 模型文字可与工具调用同时返回：即时输出进度并保留对话上下文，不提前结束任务；
  steer 回执展示真实任务等待状态、在飞工具及运行时长
- `chat_with_agent` 工具：支持父任务与 Persistent Agent 多轮同步对话
- Summarization 与 Evaluation 收敛到 `WorkItem` 闭环
- 工具权限、审批 UI、顺序确认与工具执行链路
- 群聊审批绑定任务发起人（可配置 `approvers` 代审）；多轮会话空闲超时自动结束
- 精简后的 shell 工具集：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`；
  一次性 exec 返回前清理同组后台后代，`command_succeeded` 独立标识命令成功；
  运行中的 exec 与会话都登记真实 Task / Agent 归属；
  Unix 生产入口由独立守护器实际启动命令，宿主正常退出或异常死亡后按实例回收受管进程组
- 记忆治理：ShortTermMemory、LongTermMemory 两层收敛，支持衰退淘汰与 JSON 文件持久化
- 试错经验库：经验文件存于 `.harness/experience/`，由用户在客户端发起“总结经验”，
  经验管理员（`experience-librarian`）负责检索与撰写，`search_experience` 与开局预查逐字回填经验原文
- Skill：Agent 名下 `.harness/assets/agents/<agent>/skills/*/SKILL.md` 派发时注入系统提示
- 插件系统：Rhai 脚本扩展，17 个 hook 点，支持贡献工具、技能和 Agent
- 定时任务：Webhook / cron 信号触发；`schedule_task` 动态定时任务持久化保存，重启后恢复，
  错过的运行在 7 天内补跑最近一次，支持暂停、修改与立即运行；运行账本、真实通知回执、
  `reply` / `explicit` 策略、连续失败告警与恢复、`report_of` 健康上下文已实现
- IM 通道：Telegram（长轮询、白名单、媒体附件）+ QQ（WebSocket Gateway、OAuth2、
  富媒体），跨通道隔离，出向-自动回执
- 图片输入：模型声明 `vision = true` 后，IM 入站图片与 `view_image` 工具结果以真实图片
  发给模型；QQ 移动端逐张发送的图片与下一条文字合并为一条输入
- 结构化日志、CI、单元测试与集成测试
- Android aarch64 支持（rustls TLS 后端）

- 持续监控：`shell_start(notify_on_output=true)` 将 stdout 完整行直接推送到任务通道，
  平台回执送回原 Task；支持有界重试、背压停止与自动唤醒窗口恢复。
- 后台汇报与监控生命周期分开处理：仅汇报失败且仍有受管作业时保留作业并明确提示；
  对话恢复保留真实工具协议，重复或迟到重试不会重新派发在飞任务。

### 待完善

- 父 Agent 审批仍为 MVP 自动通过实现，尚未接入真实 LLM 审查
- 部分历史设计文档仍需持续整理状态标注
- 更多真实场景下的 provider 兼容性与复杂任务策略验证
- 飞书通道仅有占位模块，尚未接入实际 API

### 已收敛

- `Plan` 已收敛为任务分解能力，不再作为独立运行时模块存在
- `Evaluation` 保留独立语义层，但执行链路统一走 `WorkItem`
- `spawn_agent` 工具已废弃并从 LLM 可调工具集中移除
- `AgentExperience` 已删除，不再作为独立运行时概念保留
- Brain 路由、自动经验收集/治理/孵化、`/remember` 共享知识库与 `/skill` 技能生成已移除
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役

## 核心架构

```text
用户输入
  -> Frontend / Signal
  -> Task
  -> Dispatch（主 Agent / 服务角色）
  -> Agent Execution
  -> Tool Calls / WorkItems
  -> Response / Writeback
  -> TUI Output
```

当前代码主目录：

```text
src/
├── app/         # 应用配置、资源与装配
├── contracts/   # 契约与 trait 定义
├── domain/      # 核心实体、消息、状态模型
├── llm/         # provider 配置与执行器实现
├── plugins/     # 模块装配
├── systems/     # ECS systems
│   ├── dispatch/
│   ├── tools/
│   └── transform/
├── tui/         # TUI 前端
├── lib.rs
└── main.rs
```

## 快速开始

### 1. 准备环境变量

复制示例配置：

```bash
cp .env.example .env.local
```

根据所用 provider 补充对应变量。

### 2. OpenAI 兼容接口示例

```bash
export HARNESS_LLM_PROVIDER=openai-compatible
export HARNESS_MODEL=deepseek-chat
export HARNESS_LLM_API_KEY=sk-xxxx
export HARNESS_LLM_API_BASE=https://example.com/v1
cargo run
```

### 3. 标准 provider 示例

支持的 provider 值：

- `openai`
- `anthropic`
- `deepseek`
- `openai-compatible`

其中 `openai-compatible` 需要显式提供 `HARNESS_LLM_API_KEY` 和
`HARNESS_LLM_API_BASE`。标准 provider 使用 `genai` 默认接入方式。

## 常用命令

### 启动程序

```bash
cargo run
```

无终端运行（服务器上的无人值守监控）：不初始化 TUI，只驱动运行时，审批经 IM 通道完成，
因此必须配置至少一个 IM 通道（`HARNESS_CHANNELS_CONFIG`）。SIGINT / SIGTERM 触发优雅退出，
30 秒内未空闲或收到第二个信号时直接退出。

```bash
cargo run -- --headless
```

### 编译检查

```bash
cargo check
```

### 运行测试

```bash
cargo test --all-features
```

### 运行格式与静态检查

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

## 关键配置

常用环境变量包括：

- `HARNESS_LLM_PROVIDER`
- `HARNESS_MODEL`
- `HARNESS_LLM_API_KEY`
- `HARNESS_LLM_API_BASE`
- `HARNESS_BRAIN_ENABLED`
- `HARNESS_MAX_RETRIES`
- `HARNESS_MAX_TOOL_ITERATIONS`
- `HARNESS_DEFAULT_WAIT_TASKS_TIMEOUT_SECS`
- `HARNESS_MAX_AUTO_WAKES`
- `HARNESS_AGENTS_CONFIG`
- `HARNESS_LOG_DIR`

Shell 相关运行参数见 `docs/configuration.md`。

## 重要文档

- 项目规范：[`AGENTS.md`](AGENTS.md)
- 当前状态：[`docs/current-state.md`](docs/current-state.md)
- 配置说明：[`docs/configuration.md`](docs/configuration.md)
- 待办与风险：[`docs/TODO.md`](docs/TODO.md)
- 设计文档索引：[`docs/design/README.md`](docs/design/README.md)
- WorkItem 边界设计：
  [`docs/design/2026-06-06-workitem-boundary-design.md`](docs/design/2026-06-06-workitem-boundary-design.md)
- Plan / Evaluation 重评估：
  [`docs/design/2026-06-06-plan-evaluation-reassessment-design.md`](docs/design/2026-06-06-plan-evaluation-reassessment-design.md)
- Skill 一等公民与经验治理改造（ADR-004）：
  [`docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md`](docs/adr/ADR-004-skill-first-class-and-experience-governance-reform.md)
- Shell 工具精简设计：
  [`docs/archive/superpowers/specs/2026-06-08-shell-tool-simplification-design.md`](docs/archive/superpowers/specs/2026-06-08-shell-tool-simplification-design.md)
