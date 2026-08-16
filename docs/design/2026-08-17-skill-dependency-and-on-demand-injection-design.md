# Skill 依赖声明与按需注入设计

> __状态：当前有效（已实现）__
>
> 实现于分支 `feat/skill-dependency-injection`（T1~T6）。本文档为设计依据，实际行为以
> `docs/current-state.md` 与代码为准。

## 背景

2026-08-16 的日志 `logs/harness_2026-08-16_23-40-14.jsonl` 显示了一次典型场景：
任务（获取当天新闻）由 Brain 派发给 `browser-operator`，注入 `browser-automation`
skill 后用 playwright-cli 完成抓取；随后用户通过 `/skill` 命令创建了新 skill
`daily-news`。

新 skill 落盘后（`.harness/assets/agents/browser-operator/skills/daily-news/`），
与原 skill 的联动仅体现在辅助文件 `sources.md` 中的一句文本引用：
"若页面为动态渲染（JS 加载），可配合 browser-automation skill 抓取"。

### 当前机制事实

经代码调研确认（关键位置见下文）：

1. __注入粒度是 agent 名下全量__。`dispatch_system.rs` 的 DirectDelegate 路径调用
   `skill_loader.load_skills(&agent.profile.name)` 加载该 agent 名下所有 skill，
   全部注入 system_prompt。Brain 输出的 `skill_name` 只写入 `TaskInjectedSkill`
   标记，不收窄注入范围。
2. __skill 之间无依赖建模__。`SkillEntry` / `LoadedSkill` / frontmatter 解析均无
   `dependencies` 类字段。
3. __Brain prompt 已注入 skills 列表__（`brain_llm_builder.rs` 的
   `build_agent_descriptions` + `brain_dispatch.rs` 的
   `brain_user_prompt_from_descriptions`），按需注入的前置条件已具备。
   `brain_decision.rs` 中"Brain prompt 当前未包含 skills 列表"的注释已过期。
4. __skill-updater 的 frontmatter 白名单__（`diff.rs`
   `FRONTMATTER_WHITELIST = ["name", "description", "self_updatable"]`）不包含
   依赖类字段；`parse_skill_md` 也不解析此类字段。

### 问题

- __P1 联动是隐式的__：`sources.md` 的引用是悬空文本。一旦未来注入策略收窄、
  或 skill 被复制到其他 agent 名下，引用即断裂，新 skill 可能因缺少原 skill 的
  操作知识而无法完成任务。
- __P2 全量注入导致上下文膨胀__：agent 名下每多一个 skill，每次派发都全量注入。
  `browser-automation` 的 SKILL.md 有 763 行；skill 数量增长后 system_prompt 将
  持续膨胀。
- __P3 创建时无依赖约束__：skill-creator 的 prompt 只提供现有 skill 的
  `name + description`，不要求新 skill 声明对现有 skill 的依赖，也不校验引用
  的存在性。

## 目标

- G1：skill 依赖显式建模——frontmatter 声明 `dependencies`，运行期可解析。
- G2：注入策略从"agent 名下全量"演进为"Brain 选中 skill + 依赖闭包"，
  同时解决 P1 与 P2。
- G3：创建与更新流程校验依赖（存在性、环），保证落盘的依赖声明可信。
- G4：行为变更最小化——Brain 未选中 skill 时维持现状全量注入。

## 非目标

- 不支持跨 agent 依赖（依赖项必须在同一 agent 名下）。当前无真实用例，
  避免引入跨 agent 注入的权限与语义复杂度。
- 不改变 `system_prompt` 覆盖语义（skills 非空时覆盖 agent.system_prompt 的
  既有行为保持不变）。
- 不放开 `read_skill_file` 的运行期限制（见"边界 B3"）。
- 不解决 skill 创建时的"经验提炼"问题（skill-creator 看不到任务执行轨迹），
  该问题独立立项。

## 设计

### D1 依赖字段格式

frontmatter 新增可选字段 `dependencies`，值为同 agent 名下的 skill 名列表：

```yaml
---
name: daily-news
description: 获取当天最新新闻，支持按分类、关键词或来源筛选，汇总实时资讯动态。
version: 1
self_updatable: false
dependencies: [browser-automation]
---
```

- 缺省视为空列表（无依赖），完全向后兼容。
- 解析规则：仅接受数组形式，元素为合法 skill 名字符串；格式非法时 warn 并
  视为空列表（与 `version` / `self_updatable` 的容错风格一致）。
- 写回与更新时校验：每个依赖名必须能被 `SkillRegistry.list_by_owner` 在同
  agent 名下找到，否则拒绝。

### D2 数据结构扩展

- `LoadedSkill`（`loader.rs`）增加 `dependencies: Vec<String>`。
- `SkillEntry`（`registry.rs`）增加 `dependencies: Vec<String>`。
- `parse_skill_md`（`loader.rs`）解析 `dependencies` 字段。
- `build_registry` 传递该字段至 `SkillEntry`。
- `SkillSummary`（Brain 可见视图）__不__增加该字段：Brain 只负责选中主
  skill，依赖由系统自动解析注入，Brain 无需感知。

### D3 依赖闭包解析

新增纯函数（建议放 `loader.rs`）：

```rust
/// 解析 skill 的传递依赖闭包，按拓扑序返回（依赖在前，选中 skill 最后）。
/// - 依赖缺失：跳过并 warn，不失败
/// - 循环依赖：环上边截断并 warn
pub fn resolve_skill_closure(
    loaded: &[LoadedSkill],
    skill_name: &str,
) -> Vec<&LoadedSkill>
```

数据源说明：入参为 `load_skills(agent_name)` 的磁盘扫描结果，而非
`SkillRegistry`——与 dispatch 注入的既有数据源保持一致，避免"手工放置
但未经 registry 注册的 skill"被误判为缺失。

实现为 DFS + 访问栈环检测。环检测策略：进入已在栈中的节点时忽略该边并
warn，继续解析其余分支。缺失依赖同理：warn 后跳过。

运行期（注入时）对缺失与环采取__容错降级__（warn + 跳过），不让任务 Failed；
创建/更新时（写回前）采取__严格校验__（拒绝），见 D5 / D6。

### D4 注入策略改造（dispatch_system.rs）

DirectDelegate 路径的现状逻辑：

```rust
let mut skills = skill_loader.load_skills(&agent.profile.name);
skills.extend(skill_loader.load_plugin_skills(&plugin_skills, &agent.profile.name));
let skills_prompt = SkillLoader::format_skills_prompt(&skills);
```

改造为：

- `hint.required_skill_id` 为 `Some`（Brain 选中了 skill）：
  注入 `resolve_skill_closure(...)` 的结果（依赖拓扑序在前，选中 skill 最后），
  再 extend 插件 skill（插件 skill 维持全量，见边界 B2）。
- `hint.required_skill_id` 为 `None`：维持现状全量注入（G4，行为不变）。

注入格式不变（`format_skills_prompt`：name / description / Skill 目录 /
instructions），依赖 skill 与选中 skill 使用同一格式，按拓扑序排列。
不额外添加"依赖/主"标注，保持 prompt 简洁。

### D5 创建流程改造（skill_creation.rs）

- __prompt 扩展__：在"现有 skill 列表"section 之后增加依赖声明要求——
  若新 skill 的执行依赖现有 skill 提供的能力（工具用法、操作流程等），
  必须在 frontmatter 中声明 `dependencies`；并给出示例。
- __写回校验__：`skill_creation_writeback_system` 在 rename 落盘前校验
  候选 SKILL.md 的 `dependencies`：
  - 每个依赖名存在于同 agent 名下（数据源为 `load_skills(agent_name)`
    磁盘扫描，与 D3 一致），否则候选置 `WritebackFailed`（既有写回失败状态），
    错误信息写明缺失的依赖名。
  - 环校验：新 skill 的依赖均为已存在 skill，且写回要求目标目录不存在，
    理论无环；仍做防御性闭包解析，异常时按 `WritebackFailed` 处理。

### D6 更新流程改造（skill_update.rs / diff.rs）

- `FRONTMATTER_WHITELIST` 增加 `"dependencies"`：允许 skill-updater 通过
  `replace_frontmatter` 调整依赖声明（skill 演进中依赖可能变化）。
- updater prompt 增加依赖字段说明：语义、同名约束、修改后将触发校验。
- `skill_update_completion_system` 在应用 operations 成功、刷新 registry 前
  校验新的 `dependencies`（存在性 + 环，数据源为 `load_skills` 磁盘扫描，
  与 D3 一致）；校验失败走既有回滚路径（`restore_skill_dir`）。

### D7 腐化治理（顺带修正）

`brain_decision.rs` 第 37 行与 110-112 行的"Brain prompt 当前未包含 skills
列表"注释与实际代码矛盾，本次一并修正。注释中提到的
`BrainSelectedSkillNotOwned` 降级是否收紧为 Failed，不在本设计范围内，
保持现状。

### D8 存量 skill 迁移

`daily-news` 的 `sources.md` 已含对 `browser-automation` 的文本引用，作为
迁移示例：手动为其 frontmatter 补充 `dependencies: [browser-automation]`。
其余存量 skill 无依赖引用，无需处理。

## 涉及变更清单

| 文件 | 变更 |
|---|---|
| `src/infrastructure/skills/loader.rs` | `parse_skill_md` 解析 dependencies；`LoadedSkill` 加字段；新增闭包解析函数 |
| `src/infrastructure/skills/registry.rs` | `SkillEntry` 加字段；`upsert` 传递 |
| `src/infrastructure/skills/diff.rs` | `FRONTMATTER_WHITELIST` 加 dependencies |
| `src/systems/dispatch/dispatch_system.rs` | DirectDelegate 注入按 `required_skill_id` 收窄为依赖闭包 |
| `src/systems/experience/skill_creation.rs` | prompt 要求声明依赖；写回校验 |
| `src/systems/experience/skill_update.rs` | prompt 说明依赖字段；completion 校验 + 失败回滚 |
| `src/systems/transform/brain_decision.rs` | 修正过期注释 |
| `.harness/assets/agents/browser-operator/skills/daily-news/SKILL.md` | 补 dependencies frontmatter（迁移示例） |
| `docs/current-state.md` | 能力状态更新 |
| `docs/design/2026-08-10-skill-creation-command-design.md` | 补充依赖声明要求或加注索引 |

## 测试计划

- 单元测试（loader）：
  - `dependencies` 解析：正常列表 / 缺省 / 非法格式容错。
  - `resolve_skill_closure`：单级依赖 / 多级传递依赖 / 环截断 / 缺失跳过 /
    拓扑序正确性。
- 集成测试（dispatch）：Brain 选中 skill 时注入内容为闭包集合（断言
  system_prompt 含依赖与选中 skill、不含无关 skill）；未选中时全量（回归）。
- 集成测试（skill_creation）：候选声明不存在的依赖 → 写回 Failed 且错误
  信息含依赖名；声明合法依赖 → 写回成功且 registry 中可见。
- 集成测试（skill_update）：`replace_frontmatter` 修改 dependencies 成功
  路径；修改为不存在依赖 → 回滚。

## 风险与边界

- __B1 Brain 误选的放大__：按需注入后，若 Brain 选错 skill，注入内容不再
  包含"备胎" skill，任务失败更直接。缓解：Brain prompt 已含完整 skill
  列表；`BrainSelectedSkillNotOwned` 时降级为 None 走全量注入，天然兜底。
- __B2 插件 skill 维持全量__：插件 skill 数量少且由集成方控制，暂不参与
  依赖闭包；如未来需要，依赖声明同样适用。
- __B3 运行期读取辅助文件__：`read_skill_file` 的 `required_tag: "skill"`
  与 `current_skill_dir`（运行期为 None）双重限制普通执行 Agent 调用，
  按需注入后依赖 skill 的辅助文件（如 `browser-automation/playwright-tests.md`）
  仍需通过 `shell_exec` 读取（skill prompt 已注入 Skill 目录路径）。本设计
  不改变该行为；若后续希望统一按需读取入口，独立立项扩展
  `read_skill_file` 的运行期支持。
- __B4 全量注入仍在的路径__：Brain 未选中 skill（null）时维持全量，P2 的
  膨胀问题在该路径依旧存在；待 Brain 选择准确率验证后可再评估是否全面
  收窄（如改为注入 name+description 索引）。
- __B5 依赖 skill 的 instructions 变更__：依赖 skill 被 skill-updater 更新
  后，注入内容随 `load_skills` 实时读取而变化，无需额外机制。

## 已确认决策

- Q1：Brain 未选中 skill 时__维持全量注入__（D4/G4，行为变更最小）。
- Q2：写回校验失败时候选置 __WritebackFailed__（D5，语义诚实，避免落盘不可信
  声明；沿用既有写回失败状态枚举）。
