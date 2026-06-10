# 文档索引

本文件是 AI Harness 文档体系的统一入口。

## 快速入口

| 文档 | 定位 |
|------|------|
| [current-state.md](current-state.md) | 当前能力、架构结论与已知限制的唯一真相源 |
| [TODO.md](TODO.md) | 当前待办事项与近期关注方向 |
| [configuration.md](configuration.md) | 配置项说明 |
| [logs.md](logs.md) | 结构化日志规范 |

## 文档地图

```text
docs/
├── README.md              ← 本文件：统一索引
├── current-state.md       ← 当前状态唯一真相源
├── TODO.md                ← 待办事项
├── configuration.md       ← 配置说明
├── logs.md                ← 日志规范（从 AGENTS.md 拆出）
├── adr/                   ← 架构决策记录（ADR），决策后追加，不修改
├── design/                ← 当前有效的设计文档，过期即归档
├── superpowers/           ← 活跃的实施计划与设计规格（由 superpowers 插件生成）
└── archive/               ← 历史文档归档，只增不改
    ├── design/            ← 已被取代的设计文档
    └── superpowers/       ← 已完成的计划和过期规格
```

## 目录职责与维护规则

| 目录 | 定位 | 维护规则 |
|------|------|----------|
| `adr/` | 架构决策记录 | 决策后追加，不修改已有记录 |
| `design/` | 当前有效的设计文档 | 与代码保持一致，与 `current-state.md` 矛盾时应归档 |
| `superpowers/` | 活跃的计划与规格 | 计划执行完毕后 7 天内归档 |
| `archive/` | 历史文档 | 只增不改，保留设计演进脉络 |

## 推荐阅读顺序

1. [current-state.md](current-state.md) — 当前全貌
2. [adr/](adr/) — 关键架构决策及其理由
3. [design/](design/) — 当前有效的设计细节
4. [superpowers/](superpowers/) — 正在进行的工作

## 文档状态标注

设计文档使用以下状态标注，通常位于文件顶部：

- **当前有效** — 与代码和 `current-state.md` 一致，可作为设计依据
- **历史背景** — 描述早期设计思路，部分内容已被取代，仅供参考
- **已归档** — 描述的方案已被后续设计完全取代，已移入 `archive/`
