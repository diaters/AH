# skill-creator Async 上下文注入与工具列表修正设计规约

> 当前有效 — 修复 skill-creator 在 async 工具路径下 `current_skill_dir` 硬编码 None 导致的完整失败链路

## 1. 问题

### 1.1 现象

任务 `55ef120b-feda-41b7-92d9-11546c761764`（意图"创建一个获取当天新闻的 skill"）在 4 轮 iteration 中全部失败，最终未能创建 skill：

| Iteration | 工具调用 | 错误 |
|-----------|---------|------|
| 1 | `write_skill_file` | `no skill directory in current context` |
| 2 | `read_skill_file` | `permission denied: requires tag 'skill-updater'` |
| 3 | `write_skill_file` | `no skill directory in current context`（参数与 iter 1 完全相同） |
| 4 | `submit_skill` | `SKILL.md not found in sandbox directory` |

### 1.2 根因

`write_skill_file` 是 `ToolActionKind::Async` 工具，走 `async_tool_dispatch_system` 路径。该 system 在构造 `OwnedToolContext` 时直接硬编码 `current_skill_dir: None`（`src/systems/tools/async_dispatch.rs:191`），未像 sync 路径（`src/systems/tools/dispatch.rs:255-282`）那样从 WorkItem entity 的 `SkillCreationContext.sandbox_dir` 解析。导致 worker 内 `ctx.current_skill_dir` 永远是 `None`，触发 `write_skill_file` 拒绝写入，进而 `submit_skill` 因 SKILL.md 不存在也失败。

### 1.3 次要问题

skill-creator 的 prompt 工具列表（`src/systems/experience/skill_creation.rs:143-156`）错误包含 `read_skill_file`。该工具的全局 `SpaceToolRegistry` 配置 `required_tag = "skill-updater"`（`src/systems/tools/mod.rs:578`），skill-creator agent 只有 `skill-creator` tag，被 `ToolTagDenied` 拒绝。ADR-006 §3.3 也明确 `read_skill_file` 是为 skill-updater 设计的"按需读取 sibling 文件"工具，skill-creator 创建场景不需要此能力（prompt 已含 SKILL.md 模板规范 + 现有 skill 列表）。

### 1.4 已知约束（不在本次修复范围）

skill-creator 工具能力声明存在**三个独立真相源**：

1. `WorkItem.input.context.tools`（由各 workitem_system 手动构造，决定 LLM 看到的工具列表）
2. 全局 `SpaceToolRegistry.required_tag`（决定运行时 tag 校验是否通过）
3. `agents.toml` 的 `[agent.tools]` 权限声明（决定 `effective_permission`，但被 `required_tag` 检查前置覆盖）

三者不一致时会出现"LLM 被广告引导调用工具，但运行时被拒绝"或"权限声明存在但永不生效"等腐化现象。本次修复仅治理 skill-creator 的三处 manifestation（3.3.1 prompt 工具列表 / 3.3.4 system_prompt 注释 / 3.3.5 `[agent.tools]` 权限），根治方向（workitem_system 从全局 registry 按 Agent tags 自动筛选工具，并废弃 `[agent.tools]` 对受限 tag 工具的权限声明）需单独立项，将在 ADR-006 追加约束记录。

## 2. 修复方案

### 2.1 整体策略

根因修复 + 测试加固，不重构"三源工具真相"架构问题。改动按职责拆分为 3 个 commit：

1. **fix(tools)**：提取 `resolve_current_skill_dir` 共享函数，注入 async 路径
2. **fix(experience)**：从三处移除 skill-creator 的 `read_skill_file` 暴露（prompt 工具列表 / prompt 模板 / `[agent.tools]` 权限声明）
3. **docs(adr)**：记录三源约束 + 同步过时文档（system_prompt 注释 / current-state.md / 设计文档）

### 2.2 不改动的部分

- `ToolExecutionRequestMessage` 数据结构（避免影响约 10 处构造点）
- sync 路径 `dispatch.rs` 的 `index_clock_loader` tuple 风格（已工作正常）
- `agents.toml.example` 的 skill-creator system_prompt 文本（仅追加注释标注不一致）
- skill-updater 流程

## 3. 详细设计

### 3.1 提取 `resolve_current_skill_dir` 共享函数

#### 3.1.1 新模块

新建 `src/systems/tools/skill_dir_resolver.rs`，导出共享函数与 `SkillContextParam`：

```rust
/// 从 WorkItem entity 解析当前 skill 目录。
///
/// 解析顺序：
/// 1. SkillCreationContext.sandbox_dir（skill-creator 路径，不依赖 skill_loader）
/// 2. SkillUpdateContext.skill_id → skill_loader.skill_md_path().parent()（skill-updater 路径）
///
/// 任一命中即返回；都不命中（非 skill 类 WorkItem）返回 None。
/// skill_loader 为 None 时，SkillUpdateContext 分支返回 None（测试世界无 loader）。
pub fn resolve_current_skill_dir<'w, 's>(
    work_item_entity: Option<Entity>,
    context_queries: &Query<'w, 's, (
        Entity,
        Option<&'w ProfileGenerationContext>,
        Option<&'w SkillUpdateContext>,
        Option<&'w SkillCreationContext>,
        &'w WorkItem,
    )>,
    skill_loader: Option<&SkillLoader>,
) -> Option<PathBuf>
```

**生命周期标注说明**（S2）：`Query<'w, 's, D>` 中 `D` 的引用生命周期必须与 `'w` 绑定，否则 Rust elision 规则无法正确推断，会导致编译失败或约束过于宽松。`SkillContextParam` 内 `Query<'w, 's, ...>` 已显式标注（3.2.2 节），此处保持一致。

#### 3.1.2 内部逻辑

移植 `dispatch.rs:255-282` 现有逻辑并适配 `Option<&SkillLoader>`：

1. `work_item_entity = None` → 返回 `None`
2. `context_queries.get(wi_entity)` 失败 → 返回 `None`
3. 取 `SkillCreationContext`（元组第 4 位），若 `Some` → 返回 `ctx.sandbox_dir.clone()`（不依赖 `skill_loader`）
4. 否则取 `SkillUpdateContext`（元组第 3 位），若 `Some`：
   - `skill_loader = None` → 返回 `None`
   - `skill_loader = Some(loader)` → `loader.skill_md_path(&ctx.skill_id).parent().map(|p| p.to_path_buf())`
5. 都无 → `None`

**行为变更说明**（S1）：

- sync 路径（`dispatch.rs`）原逻辑中 `skill_loader` 始终为 `Some`（来自 `index_clock_loader.2`），传入 `Some(&index_clock_loader.2)` 后 `SkillCreationContext` 分支行为完全一致。
- **`SkillUpdateContext` 分支有细微差异**：原 sync 逻辑（`dispatch.rs:274`）使用 `unwrap_or_default()` 兜底，当 `skill_md_path(...).parent()` 返回 `None` 时返回 `Some(PathBuf::default())`（空路径）；新逻辑改为 `parent().map(|p| p.to_path_buf())`，`parent()` 为 `None` 时返回 `None`。这是**有意的语义收紧**——空路径无意义，`None` 更诚实，避免下游工具误判"有目录"。实际不触发：`skill_md_path` 返回 `<base>/<agent>/skills/<name>/SKILL.md`（`loader.rs:64-70`），其 `parent()` 恒为 `Some`。
- async 路径原逻辑为硬编码 `None`，本次修复后传入 `skill_context.skill_loader.as_deref()`，在有 `SkillCreationContext` 时正确返回 `sandbox_dir`。

#### 3.1.3 调用点替换

- `src/systems/tools/dispatch.rs:255-282`：删除 27 行内联逻辑，替换为：

  ```rust
  let current_skill_dir = resolve_current_skill_dir(
      request.work_item_entity,
      &context_queries,
      Some(&index_clock_loader.2),
  );
  ```

- `src/systems/tools/async_dispatch.rs:191`：`current_skill_dir: None` 替换为：

  ```rust
  let current_skill_dir = resolve_current_skill_dir(
      request.work_item_entity,
      &skill_context.context_queries,
      skill_context.skill_loader.as_deref(),
  );
  // ... 后续构造 OwnedToolContext 时使用此变量
  ```

#### 3.1.4 模块归属决策

独立模块而非放在 `dispatch.rs` 内部，理由：

- `async_dispatch.rs` 是独立 system（其顶部注释说明刻意与 `tool_dispatch_system` 分离以避开参数上限），跨 system 共享逻辑必须独立模块
- 独立模块便于单元测试，避免在 16 参数 system 内嵌套测试

### 3.2 `SkillContextParam` SystemParam 封装

#### 3.2.1 参数上限约束

`async_tool_dispatch_system` 当前 16 个参数（`async_dispatch.rs:60-77`）已达 Bevy 单 system 上限。补充 `context_queries` 必须封装。

#### 3.2.2 封装方案

在 `skill_dir_resolver.rs` 同文件定义 `SkillContextParam`：

```rust
#[derive(SystemParam)]
pub struct SkillContextParam<'w, 's> {
    pub index: Res<'w, EntityIndex>,
    pub skill_loader: Option<Res<'w, SkillLoader>>,
    pub frontend_registry: Option<Res<'w, FrontendRegistry>>,
    pub context_queries: Query<
        'w, 's,
        (
            Entity,
            Option<&'static ProfileGenerationContext>,
            Option<&'static SkillUpdateContext>,
            Option<&'static SkillCreationContext>,
            &'static WorkItem,
        ),
    >,
}
```

**关键决策**：

- `skill_loader` 用 `Option<Res<SkillLoader>>` 而非 `Res<SkillLoader>`。async 路径当前**没有** `skill_loader` 参数（仅 sync 路径的 `index_clock_loader` tuple 内有），本次新增此依赖。用 `Option` 与 async 路径现有风格一致（`frontend_registry`、`backend` 等均为 `Option`），且对测试世界友好——测试世界若不装 `SkillLoader`，`resolve_current_skill_dir` 在 `SkillUpdateContext` 分支返回 `None`（skill-creator 路径走 `SkillCreationContext` 分支，不需要 `skill_loader`，不受影响）。
- `index` 保持 `Res<EntityIndex>`（非 Option），不迁移为 `Option`——async 路径当前 `index` 就是 `Res` 非 Option，测试世界也会装 `EntityIndex`（权限检查依赖它）。
- `frontend_registry` 保持 `Option<Res<FrontendRegistry>>`，与 async 路径现有定义一致。

#### 3.2.3 `resolve_current_skill_dir` 签名与 `SkillContextParam` 的对应

因 `skill_loader` 变为 `Option`，函数签名已在 3.1.1 节给出最终形态（含生命周期标注）。`SkillContextParam` 的字段类型与函数参数的对应关系：

| 函数参数 | `SkillContextParam` 字段 | 调用点传入 |
|---------|------------------------|-----------|
| `work_item_entity` | （来自 `request`） | `request.work_item_entity` |
| `context_queries` | `skill_context.context_queries` | `&skill_context.context_queries` |
| `skill_loader` | `skill_context.skill_loader: Option<Res<SkillLoader>>` | `skill_context.skill_loader.as_deref()` |

**sync 路径调用点**（`dispatch.rs:255-282`，skill_loader 始终存在）：

```rust
let current_skill_dir = resolve_current_skill_dir(
    request.work_item_entity,
    &context_queries,
    Some(&index_clock_loader.2),
);
```

**async 路径调用点**（`async_dispatch.rs:191`，skill_loader 可能为 None）：

```rust
let current_skill_dir = resolve_current_skill_dir(
    request.work_item_entity,
    &skill_context.context_queries,
    skill_context.skill_loader.as_deref(),
);
```

内部逻辑见 3.1.2 节，此处不重复。

#### 3.2.4 选 SystemParam 而非扩展 tuple 的理由

- tuple 索引在 4 个字段时不可读（`skill_context.2.skill_md_path(...)`）
- `SkillContextParam` 命名访问更清晰
- `skill_dir_resolver.rs` 内可一并导出 `resolve_current_skill_dir`，形成内聚模块

#### 3.2.5 sync 路径不迁移

`dispatch.rs` 继续用 `index_clock_loader` tuple，不迁移到 `SkillContextParam`：

- tuple 已工作正常，不属本次修复范围
- 强行统一会扩大改动面，违反"最小必要改动"
- 两套并行不冲突（`context_queries` 在 sync 路径已是独立参数）

#### 3.2.6 async_dispatch 签名调整

`async_tool_dispatch_system` 从 16 参数缩减为 15 参数：

- 移除：`index: Res<EntityIndex>`、`frontend_registry: Option<Res<FrontendRegistry>>`（2 个）
- 新增：`skill_context: SkillContextParam`（1 个）
- 净变化：16 - 2 + 1 = 15

`SkillContextParam` 内部包含 4 个字段（`index`、`skill_loader`、`frontend_registry`、`context_queries`），其中 `skill_loader` 是 async 路径**新增**的依赖（原 async 路径无此参数）。

函数体内对 `index`、`frontend_registry` 的原有访问需改为 `skill_context.index`、`skill_context.frontend_registry`。

### 3.3 移除 skill-creator 的 `read_skill_file` 暴露

#### 3.3.1 工具列表改动

`src/systems/experience/skill_creation.rs:143-156` 删除 `read_skill_file` 的 `make_tool_def(...)` 调用块（14 行）。改动后 `tools` vec 只剩 `submit_skill` + `write_skill_file` 两项。

#### 3.3.2 prompt 模板改动

`src/systems/experience/skill_creation.rs:93-96` 的"工作流程"section（prompt 字符串内的 `## 工作流程` 段，第 97 行起已是"## 注意事项"）改为：

```text
## 工作流程

1. 参考下方"SKILL.md 模板规范"和"现有 skill 列表"，构思新 skill 的结构
2. 使用 write_skill_file 创建 SKILL.md 文件（path 参数填 "SKILL.md"）
3. 如需辅助文件（脚本、模板等），继续使用 write_skill_file 创建
4. 调用 submit_skill 提交创建结果（name + description）
```

#### 3.3.3 现有 skill 列表语义强化

`src/systems/experience/skill_creation.rs:71` 的 prompt 字符串内"## 现有 skill 列表" header（注意：57-65 行是构造 `skills_listing` 的 Rust 代码，非 prompt 文本）改为：

```text
## 现有 skill 列表（仅用于避免重名，无需读取完整内容）

{skills_listing}
```

#### 3.3.4 `agents.toml.example` system_prompt 注释标注

`agents.toml.example:139-156` 的 skill-creator system_prompt 中提到"使用 read_skill_file 读取已有 skill"，但 system_prompt 是通用引导，实际工具列表由 `WorkItem.context.tools` 决定。决策：

- 不修改 system_prompt 文本（避免扩大改动面）
- 在 system_prompt 上方追加注释标注不一致
- 在 ADR-006 记录此为已知约束（3.4.1 节）

注释内容：

```toml
# NOTE: system_prompt 中提到的 read_skill_file 在当前实现中对 skill-creator 不可用
# （required_tag = "skill-updater"）。skill-creator 的 prompt 工具列表由
# skill_creation_workitem_system 动态构造，不含 read_skill_file。
# 此 system_prompt 表述为历史遗留，未来 skill-creator 若需读取 sibling 文件能力，
# 应新增 skill-creator 专用的 read 工具变体，而非复用 skill-updater 的工具。
# 参见 ADR-006 "三源工具真相" 约束记录。
```

#### 3.3.5 `agents.toml.example` 的 `[agent.tools]` 权限声明处理（N1）

`agents.toml.example:183-187` 中 skill-creator 的 `[agent.tools]` section 还声明了 `read_skill_file = "Allow"`：

```toml
[agent.tools]
default_permission = "Deny"
submit_skill = "Allow"
write_skill_file = "Allow"
read_skill_file = "Allow"   # ← 第 187 行，与设计意图冲突的死配置
```

**这是"三源工具真相"的第三处 manifestation**——除了 1.4 节已识别的 `WorkItem.input.context.tools`（prompt 工具列表）与全局 `SpaceToolRegistry.required_tag` 之外，`agents.toml` 的 `[agent.tools]` 权限声明是第三个独立真相源。

**实际运行影响**：无。`dispatch.rs:117-153` 的 `required_tag` 检查在权限检查之前执行，skill-creator 即使有 `Allow` 权限也会被 `ToolTagDenied` 先拒绝。这是与设计文档明确意图（"skill-creator 完全不该有 `read_skill_file` 能力"）冲突的死配置，属于 AGENTS.md 明令治理的"与当前设计冲突且未标注废止"的腐化点。

**决策**：删除该行，使 skill-creator 对 `read_skill_file` 的权限回落为 `default_permission = "Deny"`，三处（prompt 工具列表 / system_prompt / `[agent.tools]`）彻底一致。归入 commit 2（`fix(experience)`），与 prompt 工具列表移除同提交。

```toml
[agent.tools]
default_permission = "Deny"
submit_skill = "Allow"
write_skill_file = "Allow"
# read_skill_file 已移除：required_tag = "skill-updater" 会让此声明永不生效，
# 且 skill-creator 不应具备 sibling 文件读取能力。参见 ADR-006 "三源工具真相"。
```

### 3.4 文档同步

#### 3.4.1 ADR-006 追加约束

`docs/adr/ADR-006-skill-updater-multi-file-support.md` 的"## 风险"section 末尾追加：

```markdown
- __三源工具真相（已知约束，2026-08-15 发现）__：skill-creator 工具能力声明存在
  三个独立真相源，任一不一致即产生腐化：
  
  1. `WorkItem.input.context.tools`（由各 workitem_system 手动构造，决定 LLM
     看到的工具列表）
  2. 全局 `SpaceToolRegistry.required_tag`（决定运行时 tag 校验是否通过）
  3. `agents.toml` 的 `[agent.tools]` 权限声明（决定 `effective_permission`，
     但被 `required_tag` 检查前置覆盖）
  
  典型 manifestation：workitem_system 在 prompt 工具列表中加入了全局 registry
  限制 tag 的工具 → LLM 被广告引导调用 → 运行时被 `ToolTagDenied` 拒绝；
  或 `[agent.tools]` 声明了 `Allow` 但因 `required_tag` 不匹配而永不生效（死配置）。
  
  当前缓解：各 workitem_system 构造 prompt 工具列表时，需人工核对工具的
  `required_tag` 与目标 Agent 的 tags 是否匹配；`[agent.tools]` 不应为受限 tag
  工具声明权限。skill-creator 已在本次修复中从三处移除 `read_skill_file` 暴露
  （prompt 工具列表 / system_prompt 注释 / `[agent.tools]` 权限）。
  
  根治方向（单独立项）：workitem_system 应从全局 `SpaceToolRegistry` 按
  Agent tags 自动筛选可用工具，而非手动构造工具列表；`[agent.tools]` 应仅
  对无 `required_tag` 限制的工具声明权限，受限 tag 工具的权限由 tag 校验统一
  决定。此改造影响所有 WorkItem 类型（SkillCreation / SkillUpdate /
  ProfileGeneration / ExperienceCollection），需单独立项设计。
```

#### 3.4.2 `docs/current-state.md` 同步

`docs/current-state.md:280` 当前列出 skill-creator 工具白名单包含 `read_skill_file`，与现实不符。改为：

```text
- 执行由专用 `skill-creator` Agent 承担（声明在 `agents.toml.example`，tags `skill-creator`，
  工具白名单：`submit_skill` / `write_skill_file`）
```

#### 3.4.3 `docs/design/2026-08-10-skill-creation-command-design.md` 同步

该文档两处提及 `read_skill_file` 对 skill-creator 可用，与现实不符：

- 第 60 行：`从 SpaceToolRegistry 过滤工具（submit_skill + write_skill_file + read_skill_file）` → 移除 `+ read_skill_file`
- 第 73 行：`LLM 调用 read_skill_file 读取已有 skill` → 删除此行

设计文档顶部状态保持"当前有效"，不归档（仅同步过时表述）。

## 4. 测试策略

### 4.1 单元测试：`resolve_current_skill_dir`

位置：`src/systems/tools/skill_dir_resolver.rs` 内 `#[cfg(test)] mod tests`

复用 `src/systems/experience/skill_creation.rs:429-490` 的 minimal World + TempDir + SkillLoader 测试模式。

7 个用例：

| 用例 | 输入 | 期望输出 |
|------|------|---------|
| 无 work_item_entity | `None` | `None` |
| WorkItem 无任何 context | 只有 WorkItem Component | `None` |
| 仅 SkillCreationContext | sandbox_dir = `/tmp/x`，skill_loader = None | `Some(/tmp/x)`（不依赖 loader） |
| 仅 SkillUpdateContext + loader 存在 | skill_id = `agent/skill`，skill_loader = Some | `Some(<skills_dir>/agent/skill)`（**非空 PathBuf**，验证 S1 语义收紧） |
| 仅 SkillUpdateContext + loader 缺失 | skill_id = `agent/skill`，skill_loader = None | `None`（测试世界场景） |
| 两个 context 同时存在（防御） | 优先返回 SkillCreationContext | `Some(sandbox_dir)` |
| work_item_entity 指向不存在的 entity | 查询失败 | `None` |

**S1 回归防护**：用例 4 显式断言返回值为 `Some(<非空路径>)`，而非 `Some(PathBuf::default())`（空路径）。这覆盖了 sync 路径原 `unwrap_or_default()` 行为变更——若未来有人误改回 `unwrap_or_default()`，此用例不会失败（因 `parent()` 恒为 `Some`），但用例 4 的断言文档化了"非空路径"的预期，配合 3.1.2 节的行为变更说明形成完整防护。

### 4.2 单元测试：skill_creation 工具列表修正

位置：`src/systems/experience/skill_creation.rs` 内 `#[cfg(test)] mod tests`

新增 1 个用例，验证 prompt 工具列表不含 `read_skill_file`：

```rust
#[test]
fn workitem_system_does_not_include_read_skill_file() {
    // 构造 minimal World + SkillLoader + spawn request
    // 运行 skill_creation_workitem_system
    // 断言 WorkItem.input.context.tools 不含 "read_skill_file"
    // 断言仍含 "write_skill_file" 和 "submit_skill"
}
```

### 4.3 单元测试：async_dispatch 回归标记

位置：`src/systems/tools/async_dispatch.rs` 内 `#[cfg(test)] mod tests`

新增 1 个用例，防止未来误改回硬编码 None：

```rust
#[test]
fn async_dispatch_does_not_hardcode_current_skill_dir_none() {
    let src = include_str!("async_dispatch.rs");
    assert!(
        !src.contains("current_skill_dir: None"),
        "current_skill_dir must be resolved via resolve_current_skill_dir, not hardcoded None"
    );
}
```

源码字符串匹配是脆弱测试，但本次 bug 根因正是"硬编码 None"——对特定反模式有强威慑力，false positive 风险低。

**测试定位说明**（C4）：此测试**不验证行为**（行为已由 4.1 节 7 个用例覆盖），仅作为"防止硬编码回归"的额外防线。测试注释中应明确这一分工，避免未来读者误以为它是唯一的行为验证。

### 4.4 集成测试：skill-creator 端到端

位置：新建 `tests/skill_creation_async_integration.rs`

测试目标：覆盖完整链路，验证 async 路径 + prompt 修正 + writeback 协同工作。

#### 4.4.1 LLM mock 机制（S3）

现有 `tests/skill_update_integration.rs:48-59` 的 `NoOpExecutor` 仅返回 `OutputContent::Text("ok")`，不返回工具调用，无法直接驱动 skill-creator 的多轮 iteration。需要实现一个**可编程的 mock executor**，按调用顺序返回预设的工具调用序列：

```rust
struct ScriptedMockExecutor {
    /// 按调用顺序排列的预设输出队列
    /// 每次 execute() 消费一个元素
    script: Arc<Mutex<VecDeque<AgentExecutionOutput>>>,
}

impl AgentExecutor for ScriptedMockExecutor {
    fn execute(&self, _request: AgentExecutionRequest) -> harness::ExecutorFuture {
        let output = self.script.lock().unwrap().pop_front();
        Box::pin(async move {
            Ok(output.unwrap_or_else(|| AgentExecutionOutput {
                content: OutputContent::Text("done".to_string()),
                reasoning_content: None,
            }))
        })
    }
}
```

`AgentExecutionOutput.content` 用 `OutputContent::ToolCalls(Vec<ToolCall>)` 变体（若存在）返回工具调用；若 LLM 桥的工具调用解析路径尚不稳定，**fallback 方案**是绕过 LLM 桥，直接在测试中 spawn `ToolExecutionRequestMessage`：

```
方案 A（首选）：ScriptedMockExecutor 返回 OutputContent::ToolCalls
  → 复用现有 LLM 桥的工具调用解析路径
  → 验证完整链路（含 LLM 输出 → ToolExecutionRequestMessage 转换）

方案 B（fallback）：直接 spawn ToolExecutionRequestMessage
  → 绕过 LLM 桥，跳过 dispatch_system 的 Agent 派发
  → 验证 async_dispatch + commit + orchestrator + writeback 链路
  → 不验证 LLM 桥工具调用解析
```

实施时优先尝试方案 A。若 `OutputContent::ToolCalls` 变体不存在或 LLM 桥解析路径需要额外依赖（如 provider-specific 解析器），降级到方案 B，并在测试注释中说明降级原因。**决策点在实施时验证，不在规格中预先锁定**。

无论 A/B 方案，mock 均需驱动两轮 iteration：

1. 第一轮：返回 `write_skill_file(path="SKILL.md", content=...)` 工具调用
2. 第二轮：返回 `submit_skill(name="...", description="...")` 工具调用

#### 4.4.2 测试链路

```
SkillCreationRequestMessage
  → skill_creation_workitem_system
  → dispatch_system (派发到 skill-creator Agent)
  → ScriptedMockExecutor 第一轮（write_skill_file 工具调用）
  → async_tool_dispatch_system (write_skill_file 走 async)
  → commit_tool_effects_system (写入沙盒)
  → ScriptedMockExecutor 第二轮（submit_skill 工具调用）
  → tool_dispatch_system (submit_skill 走 sync)
  → orchestrator (处理 SubmitSkillCandidate)
  → skill_creation_writeback_system (rename + registry)
```

#### 4.4.3 测试用例

3 个测试用例：

1. **happy_path**：完整链路成功，验证：
   - sandbox_dir 下 SKILL.md 被写入
   - submit_skill 后 SKILL.md rename 到正式位置 `<agent>/skills/<skill_name>/SKILL.md`
   - SkillRegistry 注册成功
   - ExperienceCandidateStatus 为 `Persisted`

2. **write_skill_file_rejects_path_traversal**：第一轮 mock 返回 `write_skill_file(path="../escape.md")`，验证：
   - 返回 `ToolError::InvalidInput`，错误信息含 `..`
   - sandbox_dir 下无 `escape.md`
   - WorkItem 仍处于 Running 状态（可继续重试）

3. **submit_skill_without_skill_md_fails**：第一轮 mock 直接返回 `submit_skill`（跳过 write_skill_file），验证：
   - 返回 `ToolError::InvalidInput("SKILL.md not found in sandbox directory")`
   - 不执行 rename
   - sandbox_dir 仍存在

### 4.5 不测试的部分

- 真实 LLM 调用：集成测试用 mock 跳过 LLM
- skill-updater 流程：已有 `tests/skill_update_integration.rs` 覆盖
- `agents.toml.example` 的 system_prompt 一致性：作为已知约束记录

### 4.6 CI 验证

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## 5. 验证标准

### 5.1 修复后 LLM 行为预期

同一 prompt 输入（"创建一个获取当天新闻的 skill"），LLM 应在 2 轮 iteration 内完成：

1. Iteration 1：`write_skill_file(path="SKILL.md", content=...)` → 成功
2. Iteration 2：`submit_skill(name="daily-news", description=...)` → 成功

### 5.2 commit 结构

1. `fix(tools): extract resolve_current_skill_dir and inject into async_dispatch`
   - 包含：skill_dir_resolver.rs 新建、dispatch.rs 替换、async_dispatch.rs 替换、SkillContextParam、单元测试 4.1 + 4.3、集成测试 4.4

2. `fix(experience): remove read_skill_file from skill-creator across three sources`
   - 包含：skill_creation.rs 工具列表移除（3.3.1）、prompt 模板修正（3.3.2/3.3.3）、agents.toml.example 的 `[agent.tools]` 移除 `read_skill_file = "Allow"`（3.3.5）、单元测试 4.2

3. `docs(adr): record three-source tool truth constraint in ADR-006`
   - 包含：ADR-006 追加三源约束（3.4.1）、agents.toml.example system_prompt 注释标注（3.3.4）、current-state.md 同步（3.4.2）、设计文档同步（3.4.3）

## 6. 实施约束

### 6.1 依赖顺序

- commit 1 与 commit 2 相互独立，可并行实施
- commit 3 必须在 commit 1 + 2 完成后（文档描述需与现实一致）

### 6.2 风险点

- `async_tool_dispatch_system` 参数调整需仔细对照原 16 参数（`async_dispatch.rs:60-77`），确保 `index`、`frontend_registry` 的原有访问都正确迁移到 `skill_context.index`、`skill_context.frontend_registry`
- `skill_loader` 是 async 路径**新增**的依赖（原 async 路径无此参数）。生产世界装 `SkillLoader` 资源即可；若现有测试世界运行 `async_tool_dispatch_system` 但未装 `SkillLoader`，因 `SkillContextParam.skill_loader` 为 `Option`，不会 panic，但 `SkillUpdateContext` 分支会返回 `None`——需在集成测试中确认装齐
- **现有 `tests/async_dispatch_test.rs` 回归**：`setup_bridge_world()` 未装 `SkillLoader`（已确认）。封装 `SkillContextParam` 后，`skill_loader` 字段为 `None`，`resolve_current_skill_dir` 在无 `SkillCreationContext`/`SkillUpdateContext` 的测试场景返回 `None`，与原硬编码 `None` 行为一致。但需运行该测试套件确认无回归
- **sync 路径 `unwrap_or_default()` → `None` 的行为差异**（S1）：3.1.2 节已说明此为有意的语义收紧，实际不触发（`skill_md_path().parent()` 恒为 `Some`）。建议补一条 sync 路径 `SkillUpdateContext` 分支的单元测试，验证 `SkillUpdateContext + loader 存在` 场景返回 `Some(<skills_dir>/agent/skill)` 而非空路径
- **集成测试 mock 决策点**（S3）：4.4.1 节方案 A/B 的选择需在实施时验证 `OutputContent::ToolCalls` 变体是否存在。若降级到方案 B，集成测试不验证 LLM 桥工具调用解析路径，需在测试注释中说明
- 集成测试涉及完整 ECS world 构造，可能需要复用 `tests/skill_update_integration.rs` 的 helper 模式
- `dispatch.rs` 的 `index_clock_loader.2` 索引访问需确认是 `SkillLoader`（`dispatch.rs:60-65` tuple 定义已确认第 3 位是 `Res<SkillLoader>`）

## 7. 已知约束与不修复项

| 项 | 原因 | 缓解措施 |
|----|------|---------|
| 三源工具真相架构问题 | 影响所有 WorkItem 类型，需单独立项 | ADR-006 追加三源约束记录 |
| `agents.toml.example` system_prompt 文本不一致 | 通用引导，实际工具列表由 `WorkItem.context.tools` 决定 | 追加注释标注 |
| sync 路径未迁移到 `SkillContextParam` | 已工作正常，不属本次修复范围 | 两套并行不冲突 |
| `ToolExecutionRequestMessage` 未新增 `current_skill_dir` 字段 | 改动面与收益不成正比，违反"简化优先" | 共享函数方案已统一两路径行为 |
