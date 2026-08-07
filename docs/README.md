# 文档索引

本文件是 AI Harness 文档体系的__统一入口__。任何读者都应先读本节，再按需深入。

## 真相源分层（先读这段）

判断"某篇文档是否仍是有效真相源"，看其文件顶部状态标注：

- __`当前有效`__ — 可作为当前架构与实现的依据。
- __`历史背景`__ — 早期思路，部分已被取代，仅供了解演进脉络，不可作依据。
- __`已归档`__ — 已移入 `docs/archive/`，方案被后续设计完全取代，仅增不改。

分层结论：

| 层级 | 范围 | 是否当前真相源 |
| --- | --- | --- |
| __唯一真相源__ | `docs/current-state.md` | 是 — 能力、架构结论与限制的最终依据 |
| __当前有效设计文档__ | `docs/design/`（逐篇标注）、`docs/wiki/`、`docs/adr/`、根目录带 `状态：当前有效` 的架构/指南文档 | 是 |
| __活跃规格与计划__ | `docs/superpowers/` 下 `specs/`、`plans/`（标注 `当前有效`/`活跃`） | 是 — 功能合并 main 后应归档 |
| __历史 / 已归档__ | `docs/archive/`、标注 `历史背景`/`已归档` 的文档 | 否 — 仅供查阅 |

> 治理规则见 `AGENTS.md` 的「文档结构治理」与「文档生命周期」。当 `design/` 文档与 `current-state.md` 矛盾时，以 `current-state.md` 为准并归档前者。

## 快速入口

| 文档 | 状态 | 定位 |
| --- | --- | --- |
| `current-state.md` | 唯一真相源 | 当前能力、架构结论、已知限制、后续方向 |
| `async-tool-bridge.md` | 当前有效 | 工具异步桥接机制与架构结论 |
| `async-tool-bridge-pilot-report.md` | 当前有效 | 异步工具桥接试点验证报告 |
| `plugin-development.md` | 当前有效 | 插件系统开发参考（Host API 与 hook 点） |
| `AI-Harness-Data-Flow-Guide.md` | 当前有效 | 数据流转概述，实施依据见对应规格 |
| `configuration.md` | 当前有效 | 配置项参考 |
| `logs.md` | 当前有效 | 结构化日志规范 |
| `TODO.md` | 动态 | 进行中与待办清单 |
| `framework-architecture-analysis.md` | 历史背景 | 早期模块布局分析，已被 `current-state.md`、`wiki/system-pipeline.md` 取代 |
| `design/README.md` | 索引 | `design/` 全量状态表（含每篇有效性） |
| `superpowers/README.md` | 索引 | `superpowers/` 活跃计划与规格清单 |
| `wiki/README.md` | 索引 | 系统知识库（pipeline、LLM 上下文组装） |

## 架构决策记录（ADR）

所有 ADR 均"追加不修改"，已决议即锁定。

| 文件 | 标题 | 当前状态 |
| --- | --- | --- |
| `adr/ADR-000-template.md` | 架构决策记录模板 | 参考模板 |
| `adr/ADR-001-brain-agent-scheduling.md` | Brain Agent 调度机制 | 已决议 |
| `adr/ADR-002-agent-controlled-evolution.md` | Agent 受控演化模型 | 已决议 |
| `adr/ADR-003-deprecate-spawn-agent-tool.md` | 废弃 `spawn_agent` Tool | 已决议（对应工具已退役，见 `current-state.md` 已废弃项） |
| `adr/ADR-004-skill-first-class-and-experience-governance-reform.md` | Skill 一等公民与经验治理改造 | 已决议（进行中，见 superpowers 计划） |
| `adr/ADR-005-ecs-relation-modeling.md` | 实体关系改用 ECS 原生建模（EntityIndex + ChildOf） | 已决议（最新） |
| `adr/ADR-006-skill-updater-multi-file-support.md` | skill updater 多文件更新支持 | 已决议（最新） |

## 设计文档（design/）

> 完整状态表见 `design/README.md`。当前共 9 篇：__7 篇当前有效、2 篇历史背景__。

| 文档 | 标注 | 主题 |
| --- | --- | --- |
| `design/brain-agent-scheduling.md` | 当前有效 | Brain Agent 调度 |
| `design/cognitive-load-reduction.md` | 当前有效 | 认知负荷削减 |
| `design/decision-loop.md` | 当前有效 | 决策循环 |
| `design/evaluation.md` | 当前有效 | 评估闭环 |
| `design/task-decomposition.md` | 当前有效 | 任务分解 |
| `design/tool-architecture.md` | 当前有效 | 工具架构 |
| `design/user-journey.md` | 当前有效 | 用户旅程 |
| `design/llm-response-format.md` | 历史背景 | LLM 响应格式（早期方案） |
| `design/tool-architecture-early.md` | 历史背景 | 早期工具架构（已被 `tool-architecture.md` 取代） |

## 活跃规格与计划（superpowers/）

> 完整清单见 `superpowers/README.md`。当前：__13 个活跃计划、12 个规格__（其中 11 篇标注 `当前有效`，1 篇草案，1 篇待补状态）。

注意：标记为 `当前有效` 的规格若对应功能已合并 main（对照 `current-state.md` 的"已实现"章节），应按生命周期规则在 7 天内归档到 `docs/archive/superpowers/`。索引只反映文档自身声明状态，是否归档以实际合入为准。

## 系统知识库（wiki/）

| 文档 | 主题 |
| --- | --- |
| `wiki/system-pipeline.md` | 系统管线各阶段职责与数据流（权威管线文档） |
| `wiki/llm-context-assembly.md` | LLM 上下文组装机制 |

## 归档（archive/）

`archive/` 存放被取代的设计与已完成的计划/规格，仅增不改。结构：

- `archive/design/` — 已被取代的设计文档
- `archive/superpowers/` — 已完成的计划与过期规格

归档文档在文件顶部标注 `> __状态：已归档__` 并指向 `current-state.md`。

## 文档地图

```text
docs/
├── README.md                  # 统一索引（本文件）
├── current-state.md           # 唯一真相源：当前能力状态
├── configuration.md           # 配置项参考
├── logs.md                    # 结构化日志规范
├── TODO.md                    # 进行中与待办
├── AGENTS.md 镜像说明见仓库根 CLAUDE.md
├── async-tool-bridge.md               # 当前有效
├── async-tool-bridge-pilot-report.md  # 当前有效
├── plugin-development.md               # 当前有效
├── AI-Harness-Data-Flow-Guide.md       # 当前有效
├── framework-architecture-analysis.md  # 历史背景
├── adr/                       # 架构决策记录（追加不修改）
├── design/                    # 当前有效设计文档（含状态表）
├── superpowers/               # 活跃计划与规格（插件生成）
├── wiki/                      # 系统知识库
└── archive/                   # 已归档历史文档
```

## 插件系统

`plugins/` 目录以 Rhai 脚本承载 Channel/Provider 适配与业务逻辑，受 `docs/plugin-development.md` 约束。
Channel/Provider 的实际配置接入见 `configuration.md` 与仓库根的 `*.toml.example` 模板。

## 目录职责

见 `AGENTS.md` 的「文档结构治理」：本文件（统一索引）、`current-state.md`（状态真相源）、`design/`（架构结论）、`superpowers/`（活跃规格）、`archive/`（归档）。

## 维护规则

- 对外能力变化 → 同步 `current-state.md` 与 README。
- 配置项变化 → 同步 `configuration.md` 与 `.env.example`。
- 重大设计决策 → 更新 `design/` 或 `adr/`，并在本索引登记。
- 文档过期/矛盾/缺失 → 作为独立任务及时修正，不口头跳过。
- 文档状态流转：活跃 → 完成/取代 → 归档（顶部加 `状态：已归档` 标注）。

## 推荐阅读顺序

1. 本文件「真相源分层」与「快速入口」
2. `current-state.md`（当前能力全貌）
3. `adr/`（关键架构决策）
4. `design/`（逐能力设计，按 `design/README.md` 状态表选择 `当前有效` 篇目）
5. `superpowers/`（进行中的计划与规格，了解近期方向）
6. `wiki/`（管线与上下文组装细节）
