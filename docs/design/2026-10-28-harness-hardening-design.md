# Harness 加固：授权范围、信任边界与运行形态设计

## 文档信息

| 属性 | 值 |
|------|-----|
| 状态 | 待评审 |
| 创建日期 | 2026-10-28 |
| 来源 | 2026-10-28 harness 脆弱性审查（18 项缺陷） |
| 已实施部分 | 分支 `fix/harness-hardening`：webhook 认证、QQ 按钮白名单、无人值守拒绝、LLM 超时、shell 进程组、终态任务回收、hook 沙箱上限、配置回写、腐化治理 |
| 相关文档 | `2026-10-28-unattended-scheduled-monitoring-design.md`、`docs/current-state.md` |

---

## 1. 背景

脆弱性审查共发现 18 项缺陷。其中修复路径明确、不改变系统边界的 12 项已在
`fix/harness-hardening` 分支实施。本文档覆盖剩余 6 项和 2 个治理问题。它们会改变
授权语义、工具默认权限、信任边界或运行形态，按项目规范须先评审再实施。

每一节给出现状、问题、推荐方案和备选方案，评审时逐节确认即可。

## 2. 永久授权的作用范围（审查 #4）

### 2.1 现状

- 用户选择"永久允许"后，`Agent::grant_permission` 向 `agent.overrides` 写入
  `tool → Allow`。粒度是"Agent × 工具名"，不区分参数、任务来源和通道。
- 子任务请求 Confirm 工具时，只要父 Agent 对该工具为 Allow，就走父 Agent 审批，
  而父 Agent 审批目前是 MVP 自动通过。
- 授权只存在于内存，重启后丢失，"永久"名不副实。

### 2.2 问题

在一个 QQ 群里批准一次 `shell_exec` 后，同一 Agent 在 TUI、Telegram、webhook
事件任务和定时任务中都不再询问；经父 Agent 自动审批，这份授权还会扩散到所有子 Agent。
webhook 内容和 shell 输出都可能带有提示注入，这条链路让注入能直接拿到 shell 权限。

### 2.3 推荐方案

- 运行期授权改为按"审批通道"划分：`RuntimeGrants { (agent_id, tool, approval_channel) }`
  替代直接写 `overrides`。只有审批通道相同的任务能复用授权；没有审批通道的任务
  （webhook 事件、定时任务）不继承任何运行期授权。
- 父 Agent 自动审批只认配置层权限（`agents.toml` 中的 Allow），不认运行期授权，
  直到父 Agent 审批换成真实的 LLM 审查。
- 选项文案由"永久允许"改为"本次运行内对该会话允许"，与实际生命周期一致。
- 不做持久化。持久授权需要单独设计撤销入口（如 `/grants` 命令），不在本次范围内。

### 2.4 备选方案

- 保持 Agent 级授权，只在审批卡片上明确提示"将对该 Agent 的所有会话生效"。改动最小，
  但没有解决跨通道扩散。

## 3. `schedule_task` 的默认权限（审查 #5）

### 3.1 现状

`schedule_task` 与 `update_scheduled_task` 的默认权限都是 Allow。LLM 可以在不经确认的
情况下创建持久化的周期任务，这些任务之后会在无人值守时反复执行。

### 3.2 推荐方案

- `schedule_task`、`update_scheduled_task` 默认改为 Confirm；`list_scheduled_tasks`、
  `delete_scheduled_task` 保持 Allow（删除只会减少执行面）。
- 确认卡片展示调度表达式、提示词摘要和输出通道。
- 与无人值守设计 §7.2 一致：届时 `granted_tools` / `probe` 的确认合并到同一张卡片。

### 3.3 影响

用户每次让 Agent 建定时任务时多一次确认；定时任务内部再建定时任务会被无人值守拒绝。

## 4. 图片标记的信任边界（审查 #6）

### 4.1 现状

`[IMAGE:path]` 标记在 LLM 适配层晚绑定：扫描所有 user / tool 消息，把其中的本地路径读出并
上传给 provider。shell 输出、webhook 内容、IM 文本只要包含这个标记就会触发，只校验扩展名，
不限制目录，绕过了 `view_image` 的权限检查。

### 4.2 推荐方案

- 新增任务级 `AttachedImages` 登记表（Task Component），记录可信来源产生的图片：
  通道入向附件（下载到 workspace 附件目录的文件）和 `view_image` 工具的成功结果。
- 适配层只绑定登记表中的本地路径；未登记的本地路径保留原文本标记，并追加
  `[image not attached: untrusted source]` 说明。
- HTTP(S) URL 只在 user 消息中绑定，tool 结果中的 URL 不绑定（避免 provider 侧 SSRF）。

### 4.3 备选方案

- 只把本地路径限制在 workspace 目录内。实现简单，但 workspace 内的敏感图片仍可被注入读出。

## 5. 一次性定时任务的执行语义（审查 #10）

### 5.1 现状

一次性任务在触发瞬间就从 `SchedulerState` 删除并落盘，早于运行结束。运行失败或进程崩溃时，
任务直接丢失，属于"最多一次"语义，用户得不到任何提示。

### 5.2 推荐方案

- 触发时只记 `last_fired_at`，定义保留；运行进入终态后再删除（复用 P1 的 `RunLedger`
  终态记账入口）。
- 重启时如发现"已触发但未完成"的一次性任务，不自动重跑（避免重复副作用），而是向输出通道
  推送"上次运行被中断"的提醒，并把定义转为暂停状态，由用户决定 `run_now` 或删除。

### 5.3 依赖

依赖无人值守设计的 P1（`RunLedger`），建议与 P1 一起实施。

## 6. Brain 单点（审查 #11）

### 6.1 现状

Brain LLM 失败（包括模型链耗尽）时，任务直接 Failed、不降级，这是"派发架构"一节明确记录的
决策。Brain 模型不可用时，所有新任务都会失败。

### 6.2 推荐方案

- 新增配置 `HARNESS_BRAIN_FALLBACK_AGENT`（默认不设置，保持现有行为）。设置后，Brain 失败
  时以 `DirectDelegate` 派发给该 Agent，不注入 skill，并记录 `BrainFallbackUsed` 事件。
- 选择配置开关而不是默认降级：保留"失败要显式"的既有决策，同时给需要可用性的部署一个出口。

## 7. 运行形态：无 TUI 模式（审查 #12）

### 7.1 现状

ECS 只由 `main.rs` 中的 TUI 主循环驱动。IM 通道、定时器、webhook 都依赖一个开着的终端；
在服务器上做无人值守监控只能套 `tmux`。插件 hook 在 ECS 线程同步等待（每个最长 1 秒），
hook 一多 TUI 就会卡顿。

### 7.2 推荐方案

- 新增 `--headless` 启动参数：不初始化 ratatui / crossterm，不注册 TUI Frontend，主循环
  只执行 `app.update()` 与空闲休眠；SIGINT / SIGTERM 触发 `ExternalInput::Shutdown`，
  走现有优雅退出路径。
- 主循环抽成 `run_loop(app, hooks)`，TUI 与 headless 共用，避免两份循环逻辑。
- headless 下没有本地审批入口：审批只能经 IM 通道完成；未配置任何 IM 通道时启动报错。
- hook 同步等待的问题单独处理：hook 超时从 1 秒降到 200 毫秒，并统计每帧 hook 耗时，
  超过阈值时打 `HookBudgetExceeded` 警告。

## 8. 治理问题

### 8.1 规范真相源缺失

`CLAUDE.md` 声明 `AGENTS.md` 是项目规范的唯一真相源，但仓库里没有 `AGENTS.md`，
且 `.gitignore` 忽略了它；实际受版本控制的是 `CLAUDE.md`。

推荐：从 `.gitignore` 移除 `AGENTS.md`，以当前 `CLAUDE.md` 的规范部分创建 `AGENTS.md`
并纳入版本控制，`CLAUDE.md` 保持镜像。这是规范变更，需按"规范更新"流程确认。

### 8.2 `Cargo.lock` 未纳入版本控制

本项目是二进制应用（含 Android 交叉编译），却忽略了 `Cargo.lock`，CI 与本地构建无法复现，
0.x 版本依赖（如 `genai`）的小版本升级可能直接破坏构建。

推荐：从 `.gitignore` 移除 `Cargo.lock` 并提交。

## 9. 审查中发现、尚未处理的次要问题

- 仍处于 `Waiting(User)` 的多轮任务永不回收（终态回收只处理终态任务树），长期运行的 IM
  会话会积累。可选方案：空闲超过 N 小时自动 `/finish`。
- `shell_start` 长期会话每读到一行输出就把整个 1 MiB 缓冲区重新拼接生成快照，输出密集的
  进程会占用大量 CPU。可改为按需生成快照。
- 群聊中 `ChannelId` 以群为粒度：白名单内的任意群成员都能回复其他成员任务的审批、发
  `/finish` 等命令。是否需要按发起人绑定审批，待产品层面确认。

## 10. 评审要点

| 节 | 需要确认的决策 |
|----|----------------|
| §2 | 运行期授权按审批通道划分；父 Agent 自动审批只认配置层权限 |
| §3 | `schedule_task` / `update_scheduled_task` 默认改为 Confirm |
| §4 | 图片晚绑定只认任务级可信登记表 |
| §5 | 一次性任务终态后再删除，中断后转暂停并提醒（随 P1 实施） |
| §6 | 新增 `HARNESS_BRAIN_FALLBACK_AGENT` 配置开关 |
| §7 | 新增 `--headless` 运行形态 |
| §8 | 纳入 `AGENTS.md` 与 `Cargo.lock` 的版本控制 |
