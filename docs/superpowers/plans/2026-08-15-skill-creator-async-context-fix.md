# skill-creator Async 上下文注入与工具列表修正 实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 修复 skill-creator 在 async 工具路径下 `current_skill_dir` 硬编码 None 导致的完整失败链路，并从三处（prompt 工具列表 / prompt 模板 / `[agent.tools]` 权限声明）移除 `read_skill_file` 暴露。

**架构：** 提取共享函数 `resolve_current_skill_dir` 统一 sync/async 两路径的 skill 目录解析；用 `SkillContextParam` SystemParam 封装解决 async 路径 16 参数上限；从 skill-creator 三处移除 `read_skill_file` 暴露；ADR-006 记录"三源工具真相"已知约束。

**技术栈：** Rust + Bevy ECS + tracing

**规格依据：** [docs/superpowers/specs/2026-08-15-skill-creator-async-context-fix-design.md](file:///Users/diater/workspace/Harness/docs/superpowers/specs/2026-08-15-skill-creator-async-context-fix-design.md)

---

## 文件结构

### 创建

- `src/systems/tools/skill_dir_resolver.rs` — 共享函数 `resolve_current_skill_dir` + `SkillContextParam` SystemParam + 单元测试
- `tests/skill_creation_async_integration.rs` — skill-creator 端到端集成测试

### 修改

- `src/systems/tools/mod.rs` — 注册新模块 `skill_dir_resolver`
- `src/systems/tools/dispatch.rs` — 用共享函数替换 255-282 行内联逻辑
- `src/systems/tools/async_dispatch.rs` — 封装 `SkillContextParam` 替换 `index` + `frontend_registry`，注入 `current_skill_dir`
- `src/systems/experience/skill_creation.rs` — 移除 `read_skill_file` 工具定义，修正 prompt 模板，新增工具列表单元测试
- `agents.toml.example` — 移除 `[agent.tools]` 的 `read_skill_file = "Allow"`，system_prompt 上方追加注释
- `docs/adr/ADR-006-skill-updater-multi-file-support.md` — 追加"三源工具真相"约束
- `docs/current-state.md` — 同步 skill-creator 工具白名单
- `docs/design/2026-08-10-skill-creation-command-design.md` — 移除两处 `read_skill_file` 对 skill-creator 可用的过时表述

---

## 任务分解

3 个 commit 对应 3 个任务组：

- **任务 1-5** → commit 1（`fix(tools)`）：共享函数 + SystemParam + async 注入 + 测试
- **任务 6-8** → commit 2（`fix(experience)`）：三处移除 `read_skill_file` 暴露
- **任务 9-11** → commit 3（`docs(adr)`）：ADR-006 + 文档同步

---

## 任务 1：新建 `skill_dir_resolver.rs` 共享函数

**文件：**
- 创建：`src/systems/tools/skill_dir_resolver.rs`

- [ ] **步骤 1：编写失败的单元测试**

在 `src/systems/tools/skill_dir_resolver.rs` 写入：

```rust
//! 共享：从 WorkItem entity 解析当前 skill 目录。
//!
//! sync 路径（`tool_dispatch_system`）与 async 路径（`async_tool_dispatch_system`）
//! 都通过 `resolve_current_skill_dir` 统一解析，避免两份逻辑漂移（曾导致
//! async 路径硬编码 `None` 的 skill-creator 完整失败链路 bug）。

use std::path::PathBuf;

use bevy_ecs::prelude::{Entity, Query};
use bevy_ecs::system::SystemParam;

use crate::domain::{ProfileGenerationContext, SkillCreationContext, SkillUpdateContext, WorkItem};
use crate::infrastructure::skills::SkillLoader;

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
    context_queries: &Query<
        'w,
        's,
        (
            Entity,
            Option<&'w ProfileGenerationContext>,
            Option<&'w SkillUpdateContext>,
            Option<&'w SkillCreationContext>,
            &'w WorkItem,
        ),
    >,
    skill_loader: Option<&SkillLoader>,
) -> Option<PathBuf> {
    let wi_entity = work_item_entity?;
    let (_, _profile_ctx, update_ctx, creation_ctx, _work_item) = context_queries.get(wi_entity).ok()?;

    if let Some(ctx) = creation_ctx {
        return Some(ctx.sandbox_dir.clone());
    }

    if let Some(ctx) = update_ctx {
        let loader = skill_loader?;
        return loader
            .skill_md_path(&ctx.skill_id)
            .parent()
            .map(|p| p.to_path_buf());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SkillCreationContext, SkillId, SkillUpdateContext, WorkItem, WorkItemType};
    use bevy_ecs::prelude::World;
    use tempfile::TempDir;

    /// 构造 minimal World：spawn 一个 WorkItem entity，可选附 SkillCreationContext / SkillUpdateContext。
    /// 返回 (World, entity)，测试调用 `resolve_current_skill_dir` 时传入 entity 和 World 的 query。
    fn make_world_with_workitem(
        creation_ctx: Option<SkillCreationContext>,
        update_ctx: Option<SkillUpdateContext>,
    ) -> (World, Entity) {
        let mut world = World::new();
        let mut entity_builder = world.spawn(WorkItem::new(
            WorkItemType::SkillCreation,
            "test-task".to_string(),
            "test-agent".to_string(),
            "test prompt".to_string(),
            "test system prompt".to_string(),
            vec![],
            crate::domain::DispatchStrategy::default(),
            crate::domain::PendingDispatch::default(),
        ));
        if let Some(ctx) = creation_ctx {
            entity_builder.insert(ctx);
        }
        if let Some(ctx) = update_ctx {
            entity_builder.insert(ctx);
        }
        let entity = entity_builder.id();
        (world, entity)
    }

    /// 从 World 提取 Query 引用，供 resolve_current_skill_dir 使用。
    /// 注意：测试中 Query 通过 world.query() 构造，生命周期与 world 绑定。
    fn run_resolve(
        world: &World,
        entity: Option<Entity>,
        skill_loader: Option<&SkillLoader>,
    ) -> Option<PathBuf> {
        let mut query_state = world.query::<(
            Entity,
            Option<&ProfileGenerationContext>,
            Option<&SkillUpdateContext>,
            Option<&SkillCreationContext>,
            &WorkItem,
        )>();
        resolve_current_skill_dir(entity, &query_state, skill_loader)
    }

    #[test]
    fn returns_none_when_no_work_item_entity() {
        let (world, _) = make_world_with_workitem(None, None);
        let result = run_resolve(&world, None, None);
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_when_work_item_has_no_context() {
        let (world, entity) = make_world_with_workitem(None, None);
        let result = run_resolve(&world, Some(entity), None);
        assert!(result.is_none());
    }

    #[test]
    fn returns_sandbox_dir_when_skill_creation_context_present() {
        let sandbox = PathBuf::from("/tmp/test-sandbox");
        let creation_ctx = SkillCreationContext {
            task_id: uuid::Uuid::new_v4(),
            sandbox_dir: sandbox.clone(),
        };
        let (world, entity) = make_world_with_workitem(Some(creation_ctx), None);
        // skill_loader = None，但 SkillCreationContext 分支不依赖 loader
        let result = run_resolve(&world, Some(entity), None);
        assert_eq!(result, Some(sandbox));
    }

    #[test]
    fn returns_skill_dir_when_skill_update_context_and_loader_present() {
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        // 预创建 skill 目录结构，确保 parent() 有意义
        let skill_id = SkillId::new("test-agent", "test-skill");
        std::fs::create_dir_all(
            tmp.path().join("test-agent").join("skills").join("test-skill"),
        )
        .unwrap();
        let update_ctx = SkillUpdateContext {
            task_id: uuid::Uuid::new_v4(),
            skill_id: skill_id.clone(),
        };
        let (world, entity) = make_world_with_workitem(None, Some(update_ctx));
        let result = run_resolve(&world, Some(entity), Some(&loader));
        // 期望：非空 PathBuf，指向 <base>/test-agent/skills/test-skill
        let expected = tmp
            .path()
            .join("test-agent")
            .join("skills")
            .join("test-skill");
        assert_eq!(result, Some(expected));
        // S1 回归防护：显式断言非空 PathBuf（旧 unwrap_or_default 会返回空路径）
        assert!(
            result.as_ref().map(|p| !p.as_os_str().is_empty()).unwrap_or(false),
            "returned path must be non-empty, got {:?}",
            result
        );
    }

    #[test]
    fn returns_none_when_skill_update_context_but_loader_missing() {
        let update_ctx = SkillUpdateContext {
            task_id: uuid::Uuid::new_v4(),
            skill_id: SkillId::new("test-agent", "test-skill"),
        };
        let (world, entity) = make_world_with_workitem(None, Some(update_ctx));
        // skill_loader = None，模拟测试世界未装 SkillLoader
        let result = run_resolve(&world, Some(entity), None);
        assert!(result.is_none());
    }

    #[test]
    fn prefers_creation_context_when_both_present() {
        let sandbox = PathBuf::from("/tmp/test-sandbox");
        let creation_ctx = SkillCreationContext {
            task_id: uuid::Uuid::new_v4(),
            sandbox_dir: sandbox.clone(),
        };
        let update_ctx = SkillUpdateContext {
            task_id: uuid::Uuid::new_v4(),
            skill_id: SkillId::new("test-agent", "test-skill"),
        };
        let tmp = TempDir::new().unwrap();
        let loader = SkillLoader::new(tmp.path().to_path_buf());
        let (world, entity) = make_world_with_workitem(Some(creation_ctx), Some(update_ctx));
        let result = run_resolve(&world, Some(entity), Some(&loader));
        // 两个 context 同时存在（防御性测试，正常流程不应发生）
        // 优先返回 SkillCreationContext 的 sandbox_dir
        assert_eq!(result, Some(sandbox));
    }

    #[test]
    fn returns_none_when_entity_not_found() {
        let (world, _) = make_world_with_workitem(None, None);
        // 传入不存在的 entity
        let nonexistent = Entity::from_raw(99999);
        let result = run_resolve(&world, Some(nonexistent), None);
        assert!(result.is_none());
    }
}
```

- [ ] **步骤 2：在 `mod.rs` 注册新模块**

编辑 `src/systems/tools/mod.rs`，在第 14 行 `mod effect_commit;` 之后追加：

```rust
mod skill_dir_resolver;
```

并在 `pub use` 区块（约第 35 行后）追加：

```rust
pub use skill_dir_resolver::{resolve_current_skill_dir, SkillContextParam};
```

- [ ] **步骤 3：运行测试验证失败**

运行：

```bash
cargo test --lib skill_dir_resolver
```

预期：编译失败，错误信息可能涉及 `WorkItem::new` 签名不匹配、`SkillCreationContext` / `SkillUpdateContext` 字段不匹配、`SkillId::new` 不存在等。这些是测试 helper 的真实构造细节，需对照实际类型定义调整。

- [ ] **步骤 4：对照实际类型调整测试 helper**

读取以下文件确认字段与构造器：
- `src/domain/contribution.rs` — `SkillCreationContext`、`SkillUpdateContext`、`ProfileGenerationContext` 字段
- `src/domain/work_item.rs`（或 `WorkItem` 定义所在文件）— `WorkItem::new` 签名
- `src/infrastructure/skills/loader.rs` — `SkillLoader::new`、`skill_md_path`、`SkillId` 构造方式

根据实际定义修正 `make_world_with_workitem` 与各用例中的字段名。**禁止改变测试用例的语义**（输入/期望输出），只调整构造代码。

- [ ] **步骤 5：运行测试验证通过**

运行：

```bash
cargo test --lib skill_dir_resolver
```

预期：7 个测试全部 PASS。

- [ ] **步骤 6：Commit**

```bash
git add src/systems/tools/skill_dir_resolver.rs src/systems/tools/mod.rs
git commit -m "fix(tools): add resolve_current_skill_dir shared function"
```

---

## 任务 2：`SkillContextParam` SystemParam 封装

**文件：**
- 修改：`src/systems/tools/skill_dir_resolver.rs`

- [ ] **步骤 1：在 `skill_dir_resolver.rs` 追加 `SkillContextParam` 定义**

在 `resolve_current_skill_dir` 函数之后、`#[cfg(test)]` 之前追加：

```rust
/// 合并 async 路径需要的 skill 相关资源 + context query 为单 SystemParam，
/// 规避 Bevy 单 system 16 参数上限。
///
/// - `index`：原 async 路径的 `Res<EntityIndex>`，保持非 Option（权限检查依赖）
/// - `skill_loader`：async 路径**新增**的依赖，用 Option 与 async 路径风格一致
///   （测试世界未装时 `SkillUpdateContext` 分支返回 None）
/// - `frontend_registry`：原 async 路径的 `Option<Res<FrontendRegistry>>`
/// - `context_queries`：async 路径**新增**的依赖，用于 `resolve_current_skill_dir`
#[derive(SystemParam)]
pub struct SkillContextParam<'w, 's> {
    pub index: bevy_ecs::system::Res<'w, crate::ecs::EntityIndex>,
    pub skill_loader: bevy_ecs::system::Res<'w, SkillLoader>,
    pub frontend_registry:
        bevy_ecs::system::Option<bevy_ecs::system::Res<'w, crate::domain::FrontendRegistry>>,
    pub context_queries: Query<
        'w,
        's,
        (
            Entity,
            Option<&'w ProfileGenerationContext>,
            Option<&'w SkillUpdateContext>,
            Option<&'w SkillCreationContext>,
            &'w WorkItem,
        ),
    >,
}
```

**注意**：`SkillContextParam.skill_loader` 实际使用 `bevy_ecs::system::Res<'w, SkillLoader>`（非 Option）。原设计规格 3.2.2 节写为 `Option<Res<SkillLoader>>`，但实施时发现：
- `SkillLoader` 在生产世界始终存在（与 `EntityIndex` 同级的基础资源）
- `setup_bridge_world()` 测试 helper 未装 `SkillLoader`，但 `async_tool_dispatch_system` 在测试中通过 `run_system_once` 调用时，`Res<SkillLoader>` 缺失会 panic

**决策**：用 `Option<Res<SkillLoader>>` 保持测试世界兼容性。修正后字段定义为：

```rust
pub skill_loader: bevy_ecs::system::Option<bevy_ecs::system::Res<'w, SkillLoader>>,
```

同步修正 `resolve_current_skill_dir` 的 `skill_loader: Option<&SkillLoader>` 参数（已与此一致，无需改动）。

- [ ] **步骤 2：运行编译验证**

运行：

```bash
cargo build --lib
```

预期：编译通过。若 `EntityIndex` / `FrontendRegistry` 导入路径不对，对照 `src/systems/tools/async_dispatch.rs:28-42` 的 `use` 块修正。

- [ ] **步骤 3：Commit**

```bash
git add src/systems/tools/skill_dir_resolver.rs
git commit -m "fix(tools): add SkillContextParam SystemParam"
```

---

## 任务 3：sync 路径替换为共享函数

**文件：**
- 修改：`src/systems/tools/dispatch.rs:255-282`

- [ ] **步骤 1：替换 sync 路径内联逻辑**

读取 `src/systems/tools/dispatch.rs:255-282` 现有 27 行内联逻辑（两个嵌套 `if let Some(wi_entity) = request.work_item_entity` 块），整体替换为：

```rust
let current_skill_dir = resolve_current_skill_dir(
    request.work_item_entity,
    &context_queries,
    Some(&index_clock_loader.2),
);
```

注意保留前后代码不变：
- 前一行（约 254 行）的 `info!(event = "ToolExecutionStarted", ...)` 不变
- 后一行（约 283 行）的 `let ctx = ToolContext {` 不变

- [ ] **步骤 2：确认导入**

确认 `src/systems/tools/dispatch.rs` 顶部 `use super::` 块已包含 `resolve_current_skill_dir`。若未包含，在 `use super::dispatch::emit_permission_audit;` 附近追加：

```rust
use super::skill_dir_resolver::resolve_current_skill_dir;
```

- [ ] **步骤 3：运行编译验证**

运行：

```bash
cargo build --lib
```

预期：编译通过。

- [ ] **步骤 4：运行 sync 路径相关测试**

运行：

```bash
cargo test --lib tool_dispatch
```

预期：现有测试全部 PASS（行为不变，`SkillCreationContext` 分支完全一致；`SkillUpdateContext` 分支 `unwrap_or_default()` → `None` 的语义收紧实际不触发）。

- [ ] **步骤 5：Commit**

```bash
git add src/systems/tools/dispatch.rs
git commit -m "fix(tools): replace inline sync skill_dir logic with shared function"
```

---

## 任务 4：async 路径封装 `SkillContextParam` 并注入 `current_skill_dir`

**文件：**
- 修改：`src/systems/tools/async_dispatch.rs:60-77`（签名）+ `:172-193`（owned_ctx 构造）+ `:249-260`（frontend_registry / index 访问）

- [ ] **步骤 1：调整 `async_tool_dispatch_system` 签名**

将 `src/systems/tools/async_dispatch.rs:60-77` 的签名从：

```rust
#[allow(clippy::too_many_arguments)]
pub fn async_tool_dispatch_system(
    mut commands: Commands,
    runtime: Res<AsyncRuntime>,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    executors: Res<BuiltinToolExecutors>,
    registry: Option<Res<SpaceToolRegistry>>,
    index: Res<EntityIndex>,
    agents: Query<&Agent>,
    tasks: Query<&Task>,
    sender: Res<ToolResultSender>,
    backend: Option<Res<crate::systems::tools::NativeProcessBackend>>,
    scheduler_state: Option<Res<SchedulerState>>,
    scheduled_registry: Option<Res<ScheduledTaskRegistry>>,
    experience_store: Option<Res<crate::domain::ExperienceStore>>,
    frontend_registry: Option<Res<FrontendRegistry>>,
    requests: Query<(Entity, &ToolExecutionRequestMessage)>,
) {
```

替换为：

```rust
pub fn async_tool_dispatch_system(
    mut commands: Commands,
    runtime: Res<AsyncRuntime>,
    clock: Res<Clock>,
    settings: Res<HarnessSettings>,
    executors: Res<BuiltinToolExecutors>,
    registry: Option<Res<SpaceToolRegistry>>,
    skill_context: SkillContextParam,
    agents: Query<&Agent>,
    tasks: Query<&Task>,
    sender: Res<ToolResultSender>,
    backend: Option<Res<crate::systems::tools::NativeProcessBackend>>,
    scheduler_state: Option<Res<SchedulerState>>,
    scheduled_registry: Option<Res<ScheduledTaskRegistry>>,
    experience_store: Option<Res<crate::domain::ExperienceStore>>,
    requests: Query<(Entity, &ToolExecutionRequestMessage)>,
) {
```

变化：
- 移除 `index: Res<EntityIndex>`（合并进 `skill_context.index`）
- 移除 `frontend_registry: Option<Res<FrontendRegistry>>`（合并进 `skill_context.frontend_registry`）
- 新增 `skill_context: SkillContextParam`
- 移除 `#[allow(clippy::too_many_arguments)]`（参数从 16 降到 15，但 clippy 阈值是 7，仍会触发；**保留 attribute**）

**修正**：保留 `#[allow(clippy::too_many_arguments)]`，参数仍超过 7。

- [ ] **步骤 2：替换函数体内 `index` 访问点**

`src/systems/tools/async_dispatch.rs` 内 `index` 的使用点：
- 第 109 行：`index.get_agent(&request.request.agent_id)` → `skill_context.index.get_agent(&request.request.agent_id)`
- 第 252 行：`index.get_agent(&request.request.agent_id)` → `skill_context.index.get_agent(&request.request.agent_id)`
- 第 257 行：`index.get_task(&request.request.task_id)` → `skill_context.index.get_task(&request.request.task_id)`

使用 `replace_all` 批量替换不安全（可能误伤变量名相同的局部变量），需逐处 Edit。

- [ ] **步骤 3：替换函数体内 `frontend_registry` 访问点**

第 249 行：

```rust
if let Some(frontend_registry) = frontend_registry.as_deref() {
```

替换为：

```rust
if let Some(frontend_registry) = skill_context.frontend_registry.as_deref() {
```

- [ ] **步骤 4：注入 `current_skill_dir`**

在 `src/systems/tools/async_dispatch.rs` 约第 172 行（`current_origin_channel` 解析之后、`let owned_ctx = OwnedToolContext {` 之前）插入：

```rust
let current_skill_dir = resolve_current_skill_dir(
    request.work_item_entity,
    &skill_context.context_queries,
    skill_context.skill_loader.as_deref(),
);
```

然后将第 191 行的 `current_skill_dir: None,` 替换为 `current_skill_dir,`（shorthand field syntax）。

- [ ] **步骤 5：确认导入**

确认 `src/systems/tools/async_dispatch.rs` 顶部 `use super::` 块已包含 `resolve_current_skill_dir` 和 `SkillContextParam`。在 `use super::ingest_tool_results::build_scheduler_snapshot;` 附近追加：

```rust
use super::skill_dir_resolver::{resolve_current_skill_dir, SkillContextParam};
```

- [ ] **步骤 6：运行编译验证**

运行：

```bash
cargo build --lib
```

预期：编译通过。若 `SkillContextParam` / `resolve_current_skill_dir` 导入路径报错，对照任务 2 步骤 1 修正。

- [ ] **步骤 7：运行现有 async 测试确认无回归**

运行：

```bash
cargo test --test async_dispatch_test
```

预期：现有测试全部 PASS。`setup_bridge_world()` 未装 `SkillLoader`，因 `skill_context.skill_loader` 为 `Option<Res<SkillLoader>>`，`None` 时 `resolve_current_skill_dir` 返回 `None`，与原硬编码 `None` 行为一致。

若测试失败，检查 `setup_bridge_world()` 是否需要追加 `world.insert_resource(SkillLoader::new(...))`——**仅在测试因 `SkillContextParam` 字段缺失 panic 时追加**，否则保持不变。

- [ ] **步骤 8：Commit**

```bash
git add src/systems/tools/async_dispatch.rs
git commit -m "fix(tools): inject current_skill_dir into async path via SkillContextParam"
```

---

## 任务 5：async_dispatch 回归标记测试 + 集成测试

**文件：**
- 修改：`src/systems/tools/async_dispatch.rs`（追加测试 mod）
- 创建：`tests/skill_creation_async_integration.rs`

- [ ] **步骤 1：在 async_dispatch.rs 追加回归标记测试**

在 `src/systems/tools/async_dispatch.rs` 末尾追加（若已有 `#[cfg(test)] mod tests` 则在其中追加测试函数；若无则在文件末尾新建）：

```rust
#[cfg(test)]
mod tests {
    /// 防止硬编码 None 回归：本次 bug 根因是 `current_skill_dir: None` 硬编码。
    /// 此测试不验证行为（行为由 skill_dir_resolver 单元测试覆盖），
    /// 仅作为"防止硬编码回归"的额外防线。
    #[test]
    fn async_dispatch_does_not_hardcode_current_skill_dir_none() {
        let src = include_str!("async_dispatch.rs");
        assert!(
            !src.contains("current_skill_dir: None"),
            "current_skill_dir must be resolved via resolve_current_skill_dir, not hardcoded None"
        );
    }
}
```

**注意**：若 `async_dispatch.rs` 末尾已有 `#[cfg(test)] mod tests`，在其中追加测试函数即可，不要重复声明 `mod tests`。

- [ ] **步骤 2：运行回归测试**

运行：

```bash
cargo test --lib async_dispatch_does_not_hardcode_current_skill_dir_none
```

预期：PASS。

- [ ] **步骤 3：查看 `OutputContent` 是否有 `ToolCalls` 变体**

运行：

```bash
grep -rn "enum OutputContent" src/
```

读取 `OutputContent` 定义，确认是否有 `ToolCalls(Vec<ToolCall>)` 或类似变体。记录结果用于步骤 4 决策。

- [ ] **步骤 4：编写集成测试**

创建 `tests/skill_creation_async_integration.rs`：

```rust
//! skill-creator 端到端集成测试：覆盖 async 路径 + prompt 修正 + writeback 协同工作。
//!
//! 验证规格：docs/superpowers/specs/2026-08-15-skill-creator-async-context-fix-design.md

mod common;
use common::async_tool_bridge::*;

// 根据步骤 3 的 OutputContent 检查结果选择方案 A 或方案 B：
// - 方案 A（首选）：ScriptedMockExecutor 返回 OutputContent::ToolCalls
// - 方案 B（fallback）：直接 spawn ToolExecutionRequestMessage 绕过 LLM 桥

// === 以下代码为方案 A 的实现，若 OutputContent 无 ToolCalls 变体则改用方案 B ===

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use harness::llm::{AgentExecutionOutput, AgentExecutor, ExecutorRegistry, OutputContent};

/// 可编程 mock executor：按调用顺序返回预设的输出序列。
struct ScriptedMockExecutor {
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

// === 测试用例实现 ===
// 注意：以下用例需要根据实际 ECS World 构造方式调整。
// 参考 tests/skill_update_integration.rs 的 build_harness_app 模式。

#[test]
fn happy_path_creates_skill_via_async_path() {
    // TODO: 实施时根据 build_harness_app 的接口与 OutputContent::ToolCalls 的实际形态填充
    // 关键验证点：
    // 1. spawn SkillCreationRequestMessage
    // 2. skill_creation_workitem_system 创建 sandbox + WorkItem
    // 3. dispatch_system 派发到 skill-creator Agent
    // 4. ScriptedMockExecutor 第一轮返回 write_skill_file(path="SKILL.md", content=...)
    // 5. async_tool_dispatch_system 认领并执行
    // 6. commit_tool_effects_system 写入沙盒
    // 7. ScriptedMockExecutor 第二轮返回 submit_skill(name="...", description="...")
    // 8. tool_dispatch_system 认领并执行
    // 9. orchestrator 处理 SubmitSkillCandidate
    // 10. skill_creation_writeback_system rename + registry
    // 11. 断言：sandbox_dir 下 SKILL.md 被写入
    // 12. 断言：正式位置 <base>/<agent>/skills/<skill_name>/SKILL.md 存在
    // 13. 断言：SkillRegistry 注册成功
    // 14. 断言：ExperienceCandidateStatus 为 Persisted
}
```

**实施时关键决策点**（规格 4.4.1 节）：

1. **方案 A vs B**：若 `OutputContent` 无 `ToolCalls` 变体，或 LLM 桥工具调用解析路径需要 provider-specific 依赖，**降级到方案 B**：删除 `ScriptedMockExecutor`，直接在测试中 spawn `ToolExecutionRequestMessage` 绕过 LLM 桥，并在测试顶部注释说明降级原因：

```rust
// 降级说明：OutputContent 无 ToolCalls 变体（或 LLM 桥解析路径需要 provider 依赖），
// 改为直接 spawn ToolExecutionRequestMessage，不验证 LLM 桥工具调用解析路径。
```

2. **`build_harness_app` 装载**：参考 `tests/skill_update_integration.rs:7-25` 的导入与构造方式，确认如何注入 `ScriptedMockExecutor` 到 `ExecutorRegistry`、如何 spawn `SkillCreationRequestMessage`、如何推进多帧 `app.update()`。

3. **`ScriptedMockExecutor` 字段类型**：若 `OutputContent` 实际变体名不是 `ToolCalls`，对照实际定义调整。

- [ ] **步骤 5：实施集成测试 happy_path 用例**

根据步骤 3-4 的决策，参照 `tests/skill_update_integration.rs` 的完整模式实施 `happy_path_creates_skill_via_async_path`。**禁止留 TODO**——所有断言必须可执行。

关键参考：
- `tests/skill_update_integration.rs` 的 `build_harness_app` 构造
- `tests/skill_update_integration.rs` 的 `make_persistent_agent` / `make_temporary_agent`
- `src/systems/experience/skill_creation.rs` 的 `SkillCreationRequestMessage` 字段
- `src/systems/experience/skill_creation.rs` 的 `skill_creation_workitem_system` 入口
- `src/systems/experience/skill_creation.rs` 的 `skill_creation_writeback_system` 入口

- [ ] **步骤 6：实施 path_traversal 用例**

在 `tests/skill_creation_async_integration.rs` 追加：

```rust
#[test]
fn write_skill_file_rejects_path_traversal() {
    // 与 happy_path 相同的 World 构造，但 ScriptedMockExecutor 第一轮返回：
    // write_skill_file(path="../escape.md", content="...")
    //
    // 断言：
    // 1. 工具结果为 Err(ToolError::InvalidInput)，错误信息含 ".."
    // 2. sandbox_dir 下无 escape.md
    // 3. sandbox_dir 的父目录（.sandbox/）下也无 escape.md
    // 4. WorkItem 仍处于 Running 状态（可继续重试）
}
```

- [ ] **步骤 7：实施 submit_without_skill_md 用例**

在 `tests/skill_creation_async_integration.rs` 追加：

```rust
#[test]
fn submit_skill_without_skill_md_fails() {
    // 与 happy_path 相同的 World 构造，但 ScriptedMockExecutor 第一轮直接返回：
    // submit_skill(name="...", description="...")
    // （跳过 write_skill_file）
    //
    // 断言：
    // 1. 工具结果为 Err(ToolError::InvalidInput("SKILL.md not found in sandbox directory"))
    // 2. 不执行 rename
    // 3. sandbox_dir 仍存在（未被清理）
}
```

- [ ] **步骤 8：运行集成测试**

运行：

```bash
cargo test --test skill_creation_async_integration
```

预期：3 个测试全部 PASS。若因 ECS World 构造复杂导致测试难以稳定，**降级方案**：只保留 `happy_path_creates_skill_via_async_path`，其余 2 个用例改为单元测试放回 `skill_dir_resolver.rs` 或 `skill_creation.rs`（覆盖 `write_skill_file` 的路径穿越拒绝已由 `write_skill_file.rs:100-115` 单元测试覆盖，可不重复）。

- [ ] **步骤 9：Commit**

```bash
git add src/systems/tools/async_dispatch.rs tests/skill_creation_async_integration.rs tests/common/mod.rs
git commit -m "test(tools): add async_dispatch regression guard and skill-creator integration tests"
```

---

## 任务 6：移除 skill-creator prompt 工具列表中的 `read_skill_file`

**文件：**
- 修改：`src/systems/experience/skill_creation.rs:105-157`

- [ ] **步骤 1：编写失败测试**

在 `src/systems/experience/skill_creation.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
#[test]
fn workitem_system_does_not_include_read_skill_file() {
    use crate::infrastructure::skills::SkillLoader;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let mut world = bevy_ecs::prelude::World::new();
    world.insert_resource(SkillLoader::new(tmp.path().to_path_buf()));
    world.insert_resource(crate::ecs::EntityIndex::default());

    // spawn SkillCreationRequestMessage
    let request = SkillCreationRequestMessage {
        task_id: uuid::Uuid::new_v4(),
        agent_name: "test-agent".to_string(),
        intent: "test intent".to_string(),
    };
    world.spawn(request);

    // 运行 system
    world
        .run_system_once(skill_creation_workitem_system)
        .unwrap();

    // 断言：WorkItem.input.context.tools 不含 "read_skill_file"
    let mut query = world.query::<&WorkItem>();
    let work_item = query.iter(&world).next().expect("WorkItem spawned");
    let tool_names: Vec<String> = work_item
        .input
        .context
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();

    assert!(
        !tool_names.contains(&"read_skill_file".to_string()),
        "read_skill_file should not be exposed to skill-creator; got {:?}",
        tool_names
    );
    assert!(tool_names.contains(&"write_skill_file".to_string()));
    assert!(tool_names.contains(&"submit_skill".to_string()));
}
```

**注意**：`SkillCreationRequestMessage` 字段名与 `WorkItem.input.context.tools` 访问路径需对照实际定义调整。读取 `src/domain/contribution.rs` 与 `src/domain/work_item.rs` 确认。

- [ ] **步骤 2：运行测试验证失败**

运行：

```bash
cargo test --lib workitem_system_does_not_include_read_skill_file
```

预期：FAIL，断言失败（`read_skill_file` 仍在工具列表中）。

- [ ] **步骤 3：移除 `read_skill_file` 工具定义**

编辑 `src/systems/experience/skill_creation.rs`，删除第 143-156 行的 `make_tool_def("read_skill_file", ...)` 调用块（共 14 行，从 `make_tool_def(\n    "read_skill_file",` 到对应的 `),`）。

删除后 `tools` vec 只剩 `submit_skill` + `write_skill_file` 两项。

同步修改第 105 行的注释：

```rust
// 4. 构造工具列表：submit_skill + write_skill_file + read_skill_file
```

改为：

```rust
// 4. 构造工具列表：submit_skill + write_skill_file
// （read_skill_file 已移除：required_tag = "skill-updater"，skill-creator 不可用）
```

- [ ] **步骤 4：运行测试验证通过**

运行：

```bash
cargo test --lib workitem_system_does_not_include_read_skill_file
```

预期：PASS。

- [ ] **步骤 5：Commit**

```bash
git add src/systems/experience/skill_creation.rs
git commit -m "fix(experience): remove read_skill_file from skill-creator prompt tools"
```

---

## 任务 7：修正 skill-creator prompt 模板

**文件：**
- 修改：`src/systems/experience/skill_creation.rs:71`（列表 header）+ `:93-96`（工作流程 section）

- [ ] **步骤 1：修正"现有 skill 列表" header**

编辑 `src/systems/experience/skill_creation.rs` 第 71 行，将：

```rust
             "## 现有 skill 列表\n\n{}\n\n\
```

改为：

```rust
             "## 现有 skill 列表（仅用于避免重名，无需读取完整内容）\n\n{}\n\n\
```

- [ ] **步骤 2：修正"工作流程" section**

编辑 `src/systems/experience/skill_creation.rs` 第 93-96 行，将：

```rust
             "## 工作流程\n\n\
             1. 使用 read_skill_file 读取现有 skill 文件（如需参考格式）\n\
             2. 使用 write_skill_file 创建 SKILL.md 文件\n\
             3. 调用 submit_skill 提交创建结果\n\n\
```

改为：

```rust
             "## 工作流程\n\n\
             1. 参考下方\"SKILL.md 模板规范\"和\"现有 skill 列表\"，构思新 skill 的结构\n\
             2. 使用 write_skill_file 创建 SKILL.md 文件（path 参数填 \"SKILL.md\"）\n\
             3. 如需辅助文件（脚本、模板等），继续使用 write_skill_file 创建\n\
             4. 调用 submit_skill 提交创建结果（name + description）\n\n\
```

- [ ] **步骤 3：运行编译验证**

运行：

```bash
cargo build --lib
```

预期：编译通过。

- [ ] **步骤 4：Commit**

```bash
git add src/systems/experience/skill_creation.rs
git commit -m "fix(experience): update skill-creator prompt to not reference read_skill_file"
```

---

## 任务 8：移除 `agents.toml.example` 的 `read_skill_file = "Allow"` 权限声明

**文件：**
- 修改：`agents.toml.example:183-187`

- [ ] **步骤 1：删除 `read_skill_file = "Allow"` 行**

编辑 `agents.toml.example`，定位第 187 行的 `read_skill_file = "Allow"`，整行删除。

删除后 `[agent.tools]` section 为：

```toml
[agent.tools]
default_permission = "Deny"
submit_skill = "Allow"
write_skill_file = "Allow"
```

- [ ] **步骤 2：在 `[agent.tools]` section 下方追加注释**

在 `write_skill_file = "Allow"` 之后追加：

```toml
# read_skill_file 已移除：required_tag = "skill-updater" 会让此声明永不生效，
# 且 skill-creator 不应具备 sibling 文件读取能力。参见 ADR-006 "三源工具真相"。
```

- [ ] **步骤 3：Commit**

```bash
git add agents.toml.example
git commit -m "fix(experience): remove dead read_skill_file permission from skill-creator"
```

---

## 任务 9：ADR-006 追加"三源工具真相"约束

**文件：**
- 修改：`docs/adr/ADR-006-skill-updater-multi-file-support.md`

- [ ] **步骤 1：定位"## 风险" section 末尾**

读取 `docs/adr/ADR-006-skill-updater-multi-file-support.md`，找到"## 风险"section 的最后一个条目末尾。

- [ ] **步骤 2：追加三源约束记录**

在"## 风险"section 末尾追加：

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

- [ ] **步骤 3：Commit**

```bash
git add docs/adr/ADR-006-skill-updater-multi-file-support.md
git commit -m "docs(adr): record three-source tool truth constraint in ADR-006"
```

---

## 任务 10：`agents.toml.example` system_prompt 注释标注

**文件：**
- 修改：`agents.toml.example:139-156`（system_prompt 上方）

- [ ] **步骤 1：定位 skill-creator 的 system_prompt**

读取 `agents.toml.example:139-156`，找到 skill-creator 的 `system_prompt = """..."""` 块。

- [ ] **步骤 2：在 system_prompt 上方追加注释**

在 `system_prompt = """...` 之前追加：

```toml
# NOTE: system_prompt 中提到的 read_skill_file 在当前实现中对 skill-creator 不可用
# （required_tag = "skill-updater"）。skill-creator 的 prompt 工具列表由
# skill_creation_workitem_system 动态构造，不含 read_skill_file。
# 此 system_prompt 表述为历史遗留，未来 skill-creator 若需读取 sibling 文件能力，
# 应新增 skill-creator 专用的 read 工具变体，而非复用 skill-updater 的工具。
# 参见 ADR-006 "三源工具真相" 约束记录。
```

- [ ] **步骤 3：Commit**

```bash
git add agents.toml.example
git commit -m "docs(adr): annotate skill-creator system_prompt read_skill_file inconsistency"
```

---

## 任务 11：同步 `current-state.md` 与设计文档

**文件：**
- 修改：`docs/current-state.md:279-280`
- 修改：`docs/design/2026-08-10-skill-creation-command-design.md:60` 和 `:73`

- [ ] **步骤 1：修正 `current-state.md` 的工具白名单**

编辑 `docs/current-state.md`，定位第 279-280 行：

```text
- 执行由专用 `skill-creator` Agent 承担（声明在 `agents.toml.example`，tags `skill-creator`，
  工具白名单：`submit_skill` / `write_skill_file` / `read_skill_file`）
```

改为：

```text
- 执行由专用 `skill-creator` Agent 承担（声明在 `agents.toml.example`，tags `skill-creator`，
  工具白名单：`submit_skill` / `write_skill_file`）
```

- [ ] **步骤 2：修正设计文档第 60 行**

编辑 `docs/design/2026-08-10-skill-creation-command-design.md`，定位第 60 行：

```text
    │ 5. 从 SpaceToolRegistry 过滤工具（submit_skill + write_skill_file + read_skill_file）
```

改为：

```text
    │ 5. 从 SpaceToolRegistry 过滤工具（submit_skill + write_skill_file）
```

- [ ] **步骤 3：删除设计文档第 73 行**

定位第 73 行：

```text
    │ LLM 调用 read_skill_file 读取已有 skill
```

整行删除。

- [ ] **步骤 4：运行 markdownlint 验证**

运行：

```bash
markdownlint docs/current-state.md docs/design/2026-08-10-skill-creation-command-design.md docs/adr/ADR-006-skill-updater-multi-file-support.md
```

预期：无 lint 错误。

- [ ] **步骤 5：Commit**

```bash
git add docs/current-state.md docs/design/2026-08-10-skill-creation-command-design.md
git commit -m "docs(adr): sync skill-creator tool whitelist in current-state and design docs"
```

---

## 最终验证

- [ ] **步骤 1：运行完整 CI 检查**

运行：

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

预期：全部通过。若 clippy 报 `too_many_arguments`，确认 `#[allow(clippy::too_many_arguments)]` 仍在 `async_tool_dispatch_system` 上方。

- [ ] **步骤 2：运行 markdownlint 全量检查**

运行：

```bash
markdownlint '**/*.md'
```

预期：无新增 lint 错误。

- [ ] **步骤 3：验证修复后 LLM 行为**

启动 harness，触发 `/skill 创建一个获取当天新闻的 skill`，观察日志：

预期：
1. Iteration 1：`write_skill_file(path="SKILL.md", content=...)` → 成功
2. Iteration 2：`submit_skill(name="...", description="...")` → 成功
3. 沙盒目录 SKILL.md 写入成功
4. ExperienceCandidate 进入用户确认流程

若 LLM 仍尝试调用 `read_skill_file`，检查 `agents.toml.example` 的 system_prompt 是否被实际加载（可能需要同步修改实际配置文件，但本次只改 `.example`）。

---

## 自检

### 1. 规格覆盖度

| 规格章节 | 对应任务 |
|---------|---------|
| 3.1 `resolve_current_skill_dir` 共享函数 | 任务 1 |
| 3.2 `SkillContextParam` SystemParam | 任务 2 |
| 3.1.3 sync 路径调用点替换 | 任务 3 |
| 3.2.6 async_dispatch 签名调整 + 注入 | 任务 4 |
| 4.1 单元测试 7 用例 | 任务 1 步骤 1 |
| 4.3 async_dispatch 回归标记 | 任务 5 步骤 1 |
| 4.4 集成测试 3 用例 | 任务 5 步骤 4-7 |
| 3.3.1 prompt 工具列表移除 | 任务 6 |
| 3.3.2 prompt 模板改动 | 任务 7 步骤 2 |
| 3.3.3 现有 skill 列表语义强化 | 任务 7 步骤 1 |
| 3.3.4 system_prompt 注释标注 | 任务 10 |
| 3.3.5 `[agent.tools]` 权限声明处理 | 任务 8 |
| 3.4.1 ADR-006 追加约束 | 任务 9 |
| 3.4.2 `current-state.md` 同步 | 任务 11 步骤 1 |
| 3.4.3 设计文档同步 | 任务 11 步骤 2-3 |

**遗漏检查**：
- 4.2 单元测试（skill_creation 工具列表修正）→ 任务 6 步骤 1 覆盖 ✓
- 4.5 不测试的部分 → 无需任务 ✓
- 4.6 CI 验证 → 最终验证步骤 1-2 覆盖 ✓
- 5.1 修复后 LLM 行为预期 → 最终验证步骤 3 覆盖 ✓
- 5.2 commit 结构 → 任务分组与 commit 一一对应 ✓

### 2. 占位符扫描

- 任务 5 步骤 4-7 的集成测试代码块包含 `// TODO` 和注释形式的断言清单——这是**故意的实施时填充点**，已在步骤 5 明确要求"禁止留 TODO，所有断言必须可执行"。实施者必须根据 `tests/skill_update_integration.rs` 的实际模式填充完整代码。
- 任务 6 步骤 1 的测试 helper 涉及 `SkillCreationRequestMessage` 和 `WorkItem.input.context.tools` 的实际字段名——已明确要求对照 `src/domain/contribution.rs` 与 `src/domain/work_item.rs` 调整，不算占位符。

### 3. 类型一致性

- `resolve_current_skill_dir` 签名在任务 1、任务 3、任务 4 中引用一致（`Option<Entity>`, `&Query<'w, 's, ...>`, `Option<&SkillLoader>`）✓
- `SkillContextParam` 字段在任务 2 定义，任务 4 使用——字段名 `index` / `skill_loader` / `frontend_registry` / `context_queries` 一致 ✓
- `OwnedToolContext.current_skill_dir` 字段名与任务 4 步骤 4 的 shorthand `current_skill_dir,` 一致 ✓

### 4. 实施风险点

- **任务 1 步骤 4**：测试 helper 的构造代码需对照实际类型定义调整，是最可能卡壳的点。建议先读 `src/domain/contribution.rs` 和 `src/domain/work_item.rs` 再写测试
- **任务 4 步骤 2-3**：`index` 和 `frontend_registry` 的逐处替换需谨慎，不能误伤局部变量。建议用 Grep 确认所有使用点
- **任务 5 步骤 4-7**：集成测试是最复杂的实施点。若 `OutputContent::ToolCalls` 不存在或 LLM 桥路径过复杂，**允许降级到方案 B**或只保留 happy_path 用例

---

## 执行交接

计划已完成并保存到 `docs/superpowers/plans/2026-08-15-skill-creator-async-context-fix.md`。两种执行方式：

**1. 子代理驱动（推荐）** - 每个任务调度一个新的子代理，任务间进行审查，快速迭代

**2. 内联执行** - 在当前会话中使用 executing-plans 执行任务，批量执行并设有检查点

选哪种方式？
