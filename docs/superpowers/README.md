# Superpowers 文档

本目录由 superpowers 插件自动生成，包含实施计划（`plans/`）和设计规格（`specs/`）。

## 文档状态

### 活跃计划

| 文件 | 主题 | 状态 |
|------|------|------|
| `plans/2026-06-06-workitem-unified-execution.md` | WorkItem 统一执行 | 活跃 |
| `plans/2026-06-07-continue-existing-delegate.md` | 委托任务续接 | 活跃 |
| `plans/2026-06-07-shell-tool-phase1.md` | Shell 工具第一阶段 | 活跃 |
| `plans/2026-06-09-space-module-convergence.md` | Space 模块收敛 | 活跃 |
| `plans/2026-06-10-memory-convergence-implementation.md` | 记忆系统收敛 | 活跃 |

### 活跃规格

| 文件 | 主题 | 状态 |
|------|------|------|
| `specs/2026-06-07-continue-existing-delegate-design.md` | 委托任务续接设计 | 活跃 |
| `specs/2026-06-09-space-module-convergence-design.md` | Space 模块收敛设计 | 活跃 |
| `specs/2026-06-10-memory-convergence-design.md` | 记忆系统收敛设计 | 活跃 |

## 生命周期规则

- 计划执行完毕或规格被代码实现后，应在 **7 天内** 移动到 `docs/archive/superpowers/`
- 归档时在文件顶部添加 `> **状态：已归档**` 标注
- 归档后的文档只增不改，不做内容修订
- 历史计划和规格参见 [docs/archive/superpowers/](../archive/superpowers/)

## 与插件的兼容性

superpowers 插件默认将新文件写入以下路径，无需手动调整：

- 设计规格：`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`
- 实施计划：`docs/superpowers/plans/YYYY-MM-DD-<feature-name>.md`
