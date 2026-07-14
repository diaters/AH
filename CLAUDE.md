# AI Harness 项目规范

本文档定义 AI Harness 项目的核心原则、行为约束、技术边界与文档更新要求。
所有 AI Agent 与人工协作者在本仓库内工作时都必须遵守本规范。

## 文档定位

- `AGENTS.md` 是项目规范的唯一真相源。
- `CLAUDE.md` 是 `AGENTS.md` 的镜像副本，不单独承载额外规则。
- 若两者内容不一致，以 `AGENTS.md` 为准，并应尽快重新同步 `CLAUDE.md`。

## 项目概述

AI Harness 是一个基于 Rust + Bevy ECS + TUI 的 AI harness 框架，用于探索任务调度、
多 Agent 协作、工具调用、记忆治理与评估闭环。

## 当前能力状态

### 已实现

- TUI 主循环与 ECS 运行时主链路
- Task 驱动的用户目标建模
- WorkItem 驱动的内部执行单元建模
- Brain 调度、多 Agent 配置加载与任务分发
- `create_tasks` + `wait_tasks` 驱动的任务分解能力
- Summarization 与 Evaluation 收敛到 `WorkItem` 执行闭环
- 工具权限、审批 UI 与工具执行主链路
- 面向 LLM 的精简 shell 工具集：
  `shell_exec`、`shell_start`、`shell_read`、`shell_list`、`shell_input`、`shell_stop`
- `tracing` 结构化日志、CI、集成测试与回归测试
- QQ 通道接入（WebSocket Gateway、OAuth2、markdown/富媒体发送、审批文本回复匹配）

### 待继续完善

- 父 Agent 审批仍为 MVP 自动通过实现，尚未替换为真实 LLM 审查
- 部分历史设计文档仍保留阶段性语境，需要持续做状态标注和索引整理
- Provider 兼容性与复杂任务策略需要结合真实场景继续验证

### 已废弃或已收敛

- `Plan` 不再作为独立运行时模块推进，收敛为任务分解能力
- `Planning WorkItem` 不再作为预留抽象
- 旧 shell 工具 `shell_status`、`shell_read_output`、`shell_wait`、
  `shell_send_signal` 已退役

## 核心原则

### 文档先行

- 重大架构调整必须先有设计文档，再进入实施。
- 代码变更涉及能力边界、配置、工具面或工作流时，必须同步更新相关文档。
- 文档中的“当前状态”使用“能力状态”表达，不继续依赖过时的阶段编号叙事。

### 规范驱动

- 所有工作遵循本规范、ADR 和已确认的设计文档。
- 发现规范冲突时，优先上报讨论，不得私自选择对自己有利的一侧。
- 新增规范若会影响现有协作方式，应通过文档评审确认。

### 用户确认

- 重大设计决策必须提交用户确认。
- 设计文档完成自审后，应先展示给用户评审，再进入实施。
- 未经确认，不得擅自推进会改变系统边界的方案。

### 简化优先

- 优先采用精简、组合式设计，避免为了“抽象完整”引入不必要层级。
- 对 LLM 暴露的工具、状态与参数应保持语义诚实，避免伪精细控制面。
- 新抽象必须证明其在当前代码库中有真实复用价值。

## 行为约束

### 必须做

- 遵循 Conventional Commits。
- 通过分支和 PR 合并代码，禁止直接推送到 `main`。
- 提交前完成自审，确认代码、测试、文档与规范一致。
- 同一变更涉及的代码与文档应尽量放在同一提交中。
- 使用中文撰写项目文档，可夹杂必要英文术语。
- 发现文档过期、矛盾或缺失时，应作为独立任务及时修正，不做口头说明后跳过。

### 禁止做

- 跳过设计评审直接实施重大变更
- 强制推送到 `main`
- 引入不符合依赖原则的 crate
- 在未评审的情况下合入 PR
- 保留与当前实现明显冲突但未标注“废止”或“历史”的文档描述

## 技术边界

### 语言与框架

- 语言：Rust，遵循官方风格指南
- 架构：Bevy ECS
- 前端：`ratatui` + `crossterm`
- LLM 接入：`genai`
- 文档：Markdown，遵循 `markdownlint`

### 依赖原则

引入第三方依赖必须同时满足以下条件：

1. 来源仅限 crates.io，不使用 git 依赖
2. 许可证与 MIT 或 Apache-2.0 兼容
3. 优先纯 Rust 实现，保证跨平台兼容性
4. 以当前真实需求为前提，避免过度依赖

### 错误处理

- 库 crate 使用 `thiserror` 定义稳定错误类型
- 应用和主程序使用 `anyhow` 聚合错误
- 错误路径应保留足够上下文，便于定位任务、Agent、工具与状态

## 日志规范

参见 [docs/logs.md](docs/logs.md)。

## 流程规范

### 设计评审流程

```text
设计文档编写 -> 自审 -> 提交用户评审 -> 用户确认 -> 实施
```

评审标准：

- 与本规范一致
- 逻辑自洽
- 技术路径合理
- 文档中的“当前状态”与实际代码一致

### 分支策略

采用 GitHub Flow：

1. 从 `main` 创建功能分支
2. 在功能分支上开发
3. 提交 PR，经过 CI 与人工审核
4. 审核通过后合并到 `main`

### 提交信息格式

```text
<type>: <description>

[optional body]
```

常用类型：

- `feat`
- `fix`
- `docs`
- `refactor`
- `test`
- `chore`

## 测试规范

- 单元测试与实现文件放在一起，使用 `#[cfg(test)]`
- 集成测试放在 `tests/` 目录
- 关键执行链路、工具行为、边界条件应有测试覆盖
- 文档变更若涉及命令、配置或工具面，应至少做一次对应验证

## 文档规范

### 目录职责

```text
docs/
├── README.md           # 统一索引与阅读指南
├── adr/                # 架构决策记录（ADR）
├── design/             # 当前有效的设计文档
├── superpowers/        # 活跃的计划与规格（由 superpowers 插件生成）
│   ├── plans/          # 实施计划
│   └── specs/          # 设计规格
├── archive/            # 历史文档归档
│   ├── design/         # 已被取代的设计文档
│   └── superpowers/    # 已完成的计划和过期规格
├── configuration.md
├── current-state.md
├── logs.md             # 结构化日志规范
└── TODO.md
```

统一索引入口为 `docs/README.md`。

### 文档生命周期

文档状态流转：活跃 → 完成/取代 → 归档。

- `docs/design/` 文档与 `docs/current-state.md` 出现明显矛盾时，应归档到 `docs/archive/design/`
- `docs/superpowers/` 计划执行完毕后，应在 7 天内归档到 `docs/archive/superpowers/`
- 归档文档在文件顶部添加 `> **状态：已归档**` 标注，说明归档原因并指向 `current-state.md`
- 归档后的文档只增不改
- 文档维护是独立任务义务：发现过期、矛盾或缺失的文档应主动创建修正任务，不得以口头说明替代实际修正

### 文档状态标注

设计文档应在文件顶部使用以下状态之一：

- __当前有效__ — 与代码和 `current-state.md` 一致，可作为设计依据
- __历史背景__ — 描述早期设计思路，部分内容已被取代，仅供参考
- __已归档__ — 已移入 `archive/`，描述的方案已被后续设计完全取代

### 写作要求

- 每篇文档只保留一个 H1
- 标题层级递增，不跳级
- 代码块必须标注语言
- 列表统一使用 `-`
- 面向当前实现的文档优先使用”已实现 / 待完善 / 已废弃”表达

### 文档同步要求

以下变更必须同步更新相应文档：

- 项目规范变化：更新 `AGENTS.md`，再同步 `CLAUDE.md`
- 对外能力变化：更新 `README.md` 与 `docs/current-state.md`
- 配置项变化：更新 `docs/configuration.md` 与 `.env.example`
- 重大设计决策变化：更新 `docs/design/` 或 `docs/adr/`
- 文档归档后：更新对应的 README 索引

## CI/CD 配置

### 检查项

- `markdownlint`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`

### 触发条件

- Pull Request
- Push 到 `main`
- 手动触发

## 状态记录

- 任务与变更入口使用 `.github/` 下的 Issue 与 PR 模板
- 当前能力、限制与后续方向以 `docs/current-state.md` 和 `docs/TODO.md` 为准

## 规范更新

本规范随项目演进持续更新，流程如下：

1. 提出变更建议
2. 讨论并形成结论
3. 更新 `AGENTS.md`
4. 同步镜像 `CLAUDE.md`
5. 如涉及重大架构调整，补充 ADR 或设计文档

<!-- superpowers-zh:begin (do not edit between these markers) -->
# Superpowers-ZH 中文增强版

本项目已安装 superpowers-zh 技能框架（20 个 skills）。

## 核心规则

1. **收到任务时，先检查是否有匹配的 skill** — 哪怕只有 1% 的可能性也要检查
2. **设计先于编码** — 收到功能需求时，先用 brainstorming skill 做需求分析
3. **测试先于实现** — 写代码前先写测试（TDD）
4. **验证先于完成** — 声称完成前必须运行验证命令

## 可用 Skills

Skills 位于 `.claude/skills/` 目录，每个 skill 有独立的 `SKILL.md` 文件。

- **brainstorming**: 在任何创造性工作之前必须使用此技能——创建功能、构建组件、添加功能或修改行为。在实现之前先探索用户意图、需求和设计。
- **chinese-code-review**: 中文 review 沟通参考——话术模板、分级标注（必须修复/建议修改/仅供参考）、国内团队常见反模式应对。仅在用户显式 /chinese-code-review 时调用，不要根据上下文自动触发。
- **chinese-commit-conventions**: 中文 commit 与 changelog 配置参考——Conventional Commits 中文适配、commitlint/husky/commitizen 中文模板、conventional-changelog 中文配置。仅在用户显式 /chinese-commit-conventions 时调用，不要根据上下文自动触发。
- **chinese-documentation**: 中文文档排版参考——中英文空格、全半角标点、术语保留、链接格式、中文文案排版指北约定。仅在用户显式 /chinese-documentation 时调用，不要根据上下文自动触发。
- **chinese-git-workflow**: 国内 Git 平台配置参考——Gitee、Coding.net、极狐 GitLab、CNB 的 SSH/HTTPS/凭据/CI 接入差异与镜像同步配置。仅在用户显式 /chinese-git-workflow 时调用，不要根据上下文自动触发。
- **dispatching-parallel-agents**: 当面对 2 个以上可以独立进行、无共享状态或顺序依赖的任务时使用
- **executing-plans**: 当你有一份书面实现计划需要在单独的会话中执行，并设有审查检查点时使用
- **finishing-a-development-branch**: 当实现完成、所有测试通过、需要决定如何集成工作时使用——通过提供合并、PR 或清理等结构化选项来引导开发工作的收尾
- **mcp-builder**: MCP 服务器构建方法论 — 系统化构建生产级 MCP 工具，让 AI 助手连接外部能力
- **receiving-code-review**: 收到代码审查反馈后、实施建议之前使用，尤其当反馈不明确或技术上有疑问时——需要技术严谨性和验证，而非敷衍附和或盲目执行
- **requesting-code-review**: 完成任务、实现重要功能或合并前使用，用于验证工作成果是否符合要求
- **subagent-driven-development**: 当在当前会话中执行包含独立任务的实现计划时使用
- **systematic-debugging**: 遇到任何 bug、测试失败或异常行为时使用，在提出修复方案之前执行
- **test-driven-development**: 在实现任何功能或修复 bug 时使用，在编写实现代码之前
- **using-git-worktrees**: 当需要开始与当前工作区隔离的功能开发，或在执行实现计划之前使用——通过原生工具或 git worktree 回退机制确保隔离工作区存在
- **using-superpowers**: 在开始任何对话时使用——确立如何查找和使用技能，要求在任何响应（包括澄清性问题）之前调用 Skill 工具
- **verification-before-completion**: 在宣称工作完成、已修复或测试通过之前使用，在提交或创建 PR 之前——必须运行验证命令并确认输出后才能声称成功；始终用证据支撑断言
- **workflow-runner**: 在 Claude Code / OpenClaw / Cursor 中直接运行 agency-orchestrator YAML 工作流——无需 API key，使用当前会话的 LLM 作为执行引擎。当用户提供 .yaml 工作流文件或要求多角色协作完成任务时触发。
- **writing-plans**: 当你有规格说明或需求用于多步骤任务时使用，在动手写代码之前
- **writing-skills**: 当创建新技能、编辑现有技能或在部署前验证技能是否有效时使用

## 如何使用

当任务匹配某个 skill 时，使用 `Skill` 工具加载对应 skill 并严格遵循其流程。绝不要用 Read 工具读取 SKILL.md 文件。

如果你认为哪怕只有 1% 的可能性某个 skill 适用于你正在做的事情，你必须调用该 skill 检查。
<!-- superpowers-zh:end -->
