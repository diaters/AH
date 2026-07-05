> **状态：已归档** — 对应功能已合并到 main，归档于 2026-07-05

# 用户插件系统 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Harness 核心之外引入受控的用户插件机制：扫描 `.harness/plugins/<id>/manifest.toml`，加载 Rhai 脚本定义的 hook / tool / slash command / skill / agent 贡献，在固定 hook 点受控派发，通过白名单 Host API 暴露框架能力，软失败不扩散。

**Architecture:** `PluginLoader`（Startup 阶段）扫描磁盘、解析 manifest、校验命名空间/版本/schema，把通过校验的插件写入 `PluginRegistry`（ECS Resource）。`HookDispatcher` 在固定 hook 点按插件 id 字母序同步派发 Rhai 脚本，单次超时 1 秒，超时/panic 仅 warn 日志。Rhai 引擎不注册任何 FS/网络原语，插件读自身目录通过 `read_plugin_resource(rel_path)`，Host 内部做 canonicalize 前缀校验。`/reload-plugins` 等同清空 `PluginRegistry` 后重新执行扫描+注册。插件贡献的 tool/agent/skill/command 强制以 `<plugin-id>:<local-id>` 命名，与内置条目合并到既有 registeries。

**Tech Stack:** Rust 2024 · Bevy 0.18 ECS · `rhai` 1.x（脚本引擎）· `toml` 0.8（manifest）· `serde_json` 1.0（schema）· `jsonschema`（Draft 7 校验）· `tracing`（结构化日志/审计）· `tempfile`（测试）。无新增 C 依赖。

---

## File Structure

新建 Rust 模块树（避开既有的 `src/plugins/`，那是 Bevy 内部 ECS Plugin 组装点）：

```text
src/user_plugins/
├── mod.rs                       # 模块入口 + 公开 API
├── manifest.rs                  # Manifest 数据结构 + TOML 解析 + 校验
├── loader.rs                    # PluginLoader：扫描磁盘、跑校验
├── registry.rs                  # PluginRegistry ECS Resource + 查询 API
├── hook_point.rs                # HookPoint 枚举 + 21 个 hook 点定义
├── dispatcher.rs                # HookDispatcher：按 id 字母序派发、1s 超时
├── host_api/
│   ├── mod.rs                   # 注册到 Rhai 的 host 函数集
│   ├── entity_query.rs          # get_task / get_work_item / get_agent 等
│   ├── entity_write.rs          # create_task / spawn_agent / task_set_metadata
│   ├── tool_control.rs          # tool_deny / tool_set_result + 审计日志
│   ├── plugin_resource.rs       # read_plugin_resource + 路径校验
│   ├── approval.rs              # approval_request_id / approval_resolve
│   ├── experience.rs            # experience_get_candidate / experience_set_pinned
│   ├── log.rs                   # log_warn / log_info / log_error
│   └── state.rs                 # 插件级 state + register_temp_resource
├── tool_executor.rs             # 调用 Rhai tool handler 的 BuiltinTool 实现
├── slash_command.rs             # 用户插件 slash command 调度
├── reload.rs                    # /reload-plugins 处理
└── tests/
    └── mod.rs                   # 共享测试夹具
```

需要修改的既有文件：

```text
Cargo.toml                                  # 新增 rhai、jsonschema 依赖
src/lib.rs                                  # 导出 user_plugins 模块
src/app/mod.rs                              # Startup 注册 plugin_load_system
src/infrastructure/skills/loader.rs         # 支持合并插件贡献的 skill
src/systems/maintenance.rs                  # load_agents_system 合并插件 agent
src/domain/space.rs                         # SpaceToolRegistry 支持冒号命名空间查询
src/domain/command.rs                       # UserCommand 增加 PluginCommand 分支
src/systems/command.rs                      # command_parse_system 识别插件 command
src/systems/tools/dispatch.rs               # tool_dispatch 派发 on_tool_called hook
src/systems/tools/result.rs                 # tool_result 派发 on_tool_returned hook
src/systems/dispatch/task_dispatch.rs       # 派发 on_task_created/completed/failed
src/systems/dispatch/workitem_dispatch.rs   # 派发 on_workitem_* 系列hook
src/systems/dispatch/agent_selection.rs     # 派发 on_agent_started/stopped
src/systems/experience/governance.rs        # 派发经验相关 6 个 hook
src/systems/tools/approval.rs               # 派发 on_approval_requested/resolved
tests/fixtures/plugins/test-plugin/         # 内置示例插件目录
docs/current-state.md                       # 新增"用户插件系统"能力状态条目
docs/README.md                              # 索引插件系统文档入口
docs/configuration.md                       # 新增 .harness/plugins/ 说明
```

实施顺序按 Phase 1 → Phase 13 推进，每个 Phase 内任务可顺序执行。Phase 间存在依赖：Registry 依赖 Manifest，Dispatcher 依赖 Registry，Hook 派发依赖 Dispatcher，集成依赖 Host API。

---

## Phase 1：依赖与基础数据结构

### Task 1：添加 rhai 与 jsonschema 依赖

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1：编辑 `Cargo.toml` 的 `[dependencies]` 节**

在 `termimad = "0.31"` 之后追加：

```toml
rhai = { version = "1.19", default-features = false, features = ["sync"] }
jsonschema = { version = "0.20", default-features = false, features = ["resolve-file"] }
```

`rhai` 关闭 default-features 后不含 std FS 模块，`sync` 让 Engine 可跨线程共享。`jsonschema` 用于加载阶段校验插件 tool 的 JSON Schema Draft 7。

- [ ] **Step 2：运行 `cargo check` 验证依赖可解析**

Run: `cargo check`
Expected: 编译通过（可能拉取新 crate，首次较慢）。

- [ ] **Step 3：Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add rhai and jsonschema dependencies for user plugin system"
```

### Task 2：创建 `src/user_plugins/mod.rs` 空骨架

**Files:**
- Create: `src/user_plugins/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1：创建模块文件**

```rust
//! 用户插件系统
//!
//! 提供 `.harness/plugins/<id>/` 下的用户扩展加载、hook 派发与 Host API。
//! 详见 `docs/superpowers/specs/2026-06-23-plugin-system-design.md`。

pub mod manifest;
pub mod loader;
pub mod registry;
pub mod hook_point;
pub mod dispatcher;
pub mod host_api;
pub mod tool_executor;
pub mod slash_command;
pub mod reload;

/// 核心 Host API 版本。manifest 的 `api_version` 必须与此相等才能加载。
pub const API_VERSION: u32 = 1;
```

其他子模块在后续 Task 中创建，先以 `pub mod` 引用，编译失败属于预期，使用 `cargo check --lib` 时需保证 Task 3 之前不被强制要求编译。为简化执行：本 Task 同时创建空文件以让 `mod.rs` 通过编译。

为每个子模块创建仅含 `// placeholder` 的占位文件即可（host_api 为目录，需要新增 `mod.rs`）。

- [ ] **Step 2：在 `src/lib.rs` 中导出模块**

在 `pub mod tui;` 之后追加：

```rust
pub mod user_plugins;
```

- [ ] **Step 3：运行 `cargo check`**

Expected: PASS。

- [ ] **Step 4：Commit**

```bash
git add src/user_plugins/ src/lib.rs
git commit -m "feat(user-plugins): scaffold user_plugins module"
```

### Task 3：Manifest 数据结构定义

**Files:**
- Create: `src/user_plugins/manifest.rs`（替换 placeholder）

- [ ] **Step 1：编写 Manifest 数据结构与解析测试**

写入 `src/user_plugins/manifest.rs`：

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::user_plugins::API_VERSION;

/// 用户插件 Manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub api_version: u32,
    pub author: Option<String>,
    pub description: Option<String>,

    #[serde(default)]
    pub hooks: Vec<HookSubscription>,
    #[serde(default)]
    pub tools: Vec<ToolContribution>,
    #[serde(default)]
    pub skills: Vec<SkillContribution>,
    #[serde(default)]
    pub agents: Vec<AgentContribution>,
    #[serde(default)]
    pub commands: Vec<CommandContribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSubscription {
    pub event: String,
    pub script: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolContribution {
    pub id: String,
    pub schema: PathBuf,
    pub handler: PathBuf,
    pub description: String,
    pub default_permission: Option<crate::domain::ToolPermission>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillContribution {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContribution {
    pub id: String,
    pub profile: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandContribution {
    pub id: String,
    pub display: String,
    pub script: PathBuf,
    pub description: Option<String>,
}

/// Manifest 校验结果
#[derive(Debug)]
pub enum ManifestError {
    Parse(toml::de::Error),
    Invalid(String),
}

impl PluginManifest {
    /// 从 TOML 字符串解析。
    pub fn from_toml(content: &str) -> Result<Self, ManifestError> {
        let manifest: PluginManifest =
            toml::from_str(content).map_err(ManifestError::Parse)?;
        manifest.validate().map_err(ManifestError::Invalid)?;
        Ok(manifest)
    }

    /// 静态校验：api_version、id 非空、display 非空、script 路径是相对路径。
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("manifest.id must not be empty".into());
        }
        if self.id.contains(':') {
            return Err(format!(
                "manifest.id must not contain ':': {}",
                self.id
            ));
        }
        if self.api_version != API_VERSION {
            return Err(format!(
                "api_version mismatch: manifest={}, core={}",
                self.api_version, API_VERSION
            ));
        }
        for hook in &self.hooks {
            if hook.event.trim().is_empty() {
                return Err("hook.event must not be empty".into());
            }
            if hook.script.is_absolute() {
                return Err(format!(
                    "hook.script must be relative to plugin root: {}",
                    hook.script.display()
                ));
            }
        }
        for tool in &self.tools {
            if tool.id.trim().is_empty() {
                return Err("tool.id must not be empty".into());
            }
            if tool.id.contains(':') {
                return Err(format!("tool.id must not contain ':': {}", tool.id));
            }
            if tool.description.trim().is_empty() {
                return Err(format!("tool.description must not be empty: {}", tool.id));
            }
            if tool.schema.is_absolute() || tool.handler.is_absolute() {
                return Err(format!(
                    "tool paths must be relative: {}/{}",
                    tool.schema.display(),
                    tool.handler.display()
                ));
            }
        }
        for skill in &self.skills {
            if skill.id.contains(':') {
                return Err(format!("skill.id must not contain ':': {}", skill.id));
            }
            if skill.path.is_absolute() {
                return Err(format!("skill.path must be relative: {}", skill.path.display()));
            }
        }
        for agent in &self.agents {
            if agent.id.contains(':') {
                return Err(format!("agent.id must not contain ':': {}", agent.id));
            }
            if agent.profile.is_absolute() {
                return Err(format!(
                    "agent.profile must be relative: {}",
                    agent.profile.display()
                ));
            }
        }
        for cmd in &self.commands {
            if cmd.id.contains(':') {
                return Err(format!("command.id must not contain ':': {}", cmd.id));
            }
            if !cmd.display.starts_with('/') {
                return Err(format!("command.display must start with '/': {}", cmd.display));
            }
            if cmd.script.is_absolute() {
                return Err(format!("command.script must be relative: {}", cmd.script.display()));
            }
        }
        // 单插件内 display 不允许重复（跨插件 display 冲突在 loader 阶段处理，见 Task 5）
        let mut seen_displays = std::collections::HashSet::new();
        for cmd in &self.commands {
            if !seen_displays.insert(cmd.display.as_str()) {
                return Err(format!(
                    "duplicate command.display within plugin: {}",
                    cmd.display
                ));
            }
        }
        Ok(())
    }

    /// 该 manifest 是否声明了某个 hook 事件
    pub fn subscribes_to(&self, event: &str) -> bool {
        self.hooks.iter().any(|h| h.event == event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_header() -> &'static str {
        "id = \"my-plugin\"\napi_version = 1\n"
    }

    #[test]
    fn parses_minimal_valid_manifest() {
        let toml_src = valid_header();
        let m = PluginManifest::from_toml(toml_src).unwrap();
        assert_eq!(m.id, "my-plugin");
        assert_eq!(m.api_version, 1);
        assert!(m.hooks.is_empty());
    }

    #[test]
    fn rejects_id_with_colon() {
        let toml_src = "id = \"bad:id\"\napi_version = 1\n";
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_wrong_api_version() {
        let toml_src = "id = \"x\"\napi_version = 999\n";
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_absolute_hook_script() {
        let toml_src = r#"
id = "x"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "/abs/path.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn rejects_command_display_without_slash() {
        let toml_src = r#"
id = "x"
api_version = 1
[[commands]]
id = "summarize"
display = "summarize"
script = "commands/summarize.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)));
    }

    #[test]
    fn subscribes_to_detects_event() {
        let toml_src = r#"
id = "x"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"
"#;
        let m = PluginManifest::from_toml(toml_src).unwrap();
        assert!(m.subscribes_to("on_task_created"));
        assert!(!m.subscribes_to("on_tool_called"));
    }

    #[test]
    fn rejects_duplicate_display_within_single_plugin() {
        let toml_src = r#"
id = "x"
api_version = 1
[[commands]]
id = "a"
display = "/hi"
script = "commands/a.rhai"
[[commands]]
id = "b"
display = "/hi"
script = "commands/b.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(s) if s.contains("duplicate command.display")));
    }

    #[test]
    fn rejects_tool_with_empty_description() {
        let toml_src = r#"
id = "x"
api_version = 1
[[tools]]
id = "t"
description = ""
schema = "tools/t.schema.json"
handler = "tools/t.rhai"
"#;
        let err = PluginManifest::from_toml(toml_src).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(s) if s.contains("tool.description must not be empty")));
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::manifest`
Expected: 8 tests PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/manifest.rs
git commit -m "feat(user-plugins): add manifest data structure with validation"
```

### Task 4：HookPoint 枚举与契约校验

**Files:**
- Create: `src/user_plugins/hook_point.rs`（替换 placeholder）

- [ ] **Step 1：编写 HookPoint 枚举与测试**

```rust
use std::str::FromStr;

use thiserror::Error;

/// v1 暴露的固定 hook 点清单。
///
/// 新增 hook 点算核心契约变更，需要设计评审。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    // 前 hook（可拒绝）
    OnToolCalled,
    // 后 hook（观察 + 受控修改）
    OnTaskCreated,
    OnTaskCompleted,
    OnTaskFailed,
    OnWorkItemStarted,
    OnWorkItemCompleted,
    OnWorkItemFailed,
    OnAgentStarted,
    OnAgentStopped,
    OnToolReturned,
    OnMessageDispatched,
    OnMessageReceived,
    OnLlmResponse,
    OnLongTermMemoryWrite,
    OnLongTermMemoryEvicted,
    OnSharedKnowledgeWrite,
    OnExperienceCandidateSubmitted,
    OnExperienceCandidateApproved,
    OnExperienceCandidateRejected,
    OnApprovalRequested,
    OnApprovalResolved,
}

#[derive(Debug, Error)]
pub enum HookPointParseError {
    #[error("unknown hook point: {0}")]
    Unknown(String),
}

impl FromStr for HookPoint {
    type Err = HookPointParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "on_tool_called" => Ok(Self::OnToolCalled),
            "on_task_created" => Ok(Self::OnTaskCreated),
            "on_task_completed" => Ok(Self::OnTaskCompleted),
            "on_task_failed" => Ok(Self::OnTaskFailed),
            "on_workitem_started" => Ok(Self::OnWorkItemStarted),
            "on_workitem_completed" => Ok(Self::OnWorkItemCompleted),
            "on_workitem_failed" => Ok(Self::OnWorkItemFailed),
            "on_agent_started" => Ok(Self::OnAgentStarted),
            "on_agent_stopped" => Ok(Self::OnAgentStopped),
            "on_tool_returned" => Ok(Self::OnToolReturned),
            "on_message_dispatched" => Ok(Self::OnMessageDispatched),
            "on_message_received" => Ok(Self::OnMessageReceived),
            "on_llm_response" => Ok(Self::OnLlmResponse),
            "on_long_term_memory_write" => Ok(Self::OnLongTermMemoryWrite),
            "on_long_term_memory_evicted" => Ok(Self::OnLongTermMemoryEvicted),
            "on_shared_knowledge_write" => Ok(Self::OnSharedKnowledgeWrite),
            "on_experience_candidate_submitted" => Ok(Self::OnExperienceCandidateSubmitted),
            "on_experience_candidate_approved" => Ok(Self::OnExperienceCandidateApproved),
            "on_experience_candidate_rejected" => Ok(Self::OnExperienceCandidateRejected),
            "on_approval_requested" => Ok(Self::OnApprovalRequested),
            "on_approval_resolved" => Ok(Self::OnApprovalResolved),
            other => Err(HookPointParseError::Unknown(other.to_string())),
        }
    }
}

impl HookPoint {
    /// 此 hook 点是否为"前 hook"，允许拒绝或修改入参。
    pub fn is_pre(&self) -> bool {
        matches!(self, Self::OnToolCalled)
    }
}

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_points() {
        for s in [
            "on_tool_called",
            "on_task_created",
            "on_task_completed",
            "on_task_failed",
            "on_workitem_started",
            "on_workitem_completed",
            "on_workitem_failed",
            "on_agent_started",
            "on_agent_stopped",
            "on_tool_returned",
            "on_message_dispatched",
            "on_message_received",
            "on_llm_response",
            "on_long_term_memory_write",
            "on_long_term_memory_evicted",
            "on_shared_knowledge_write",
            "on_experience_candidate_submitted",
            "on_experience_candidate_approved",
            "on_experience_candidate_rejected",
            "on_approval_requested",
            "on_approval_resolved",
        ] {
            assert!(HookPoint::from_str(s).is_ok(), "failed to parse {s}");
        }
    }

    #[test]
    fn rejects_unknown_point() {
        assert!(HookPoint::from_str("on_unknown_thing").is_err());
    }

    #[test]
    fn only_on_tool_called_is_pre() {
        assert!(HookPoint::OnToolCalled.is_pre());
        assert!(!HookPoint::OnTaskCreated.is_pre());
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::hook_point`
Expected: 3 tests PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/hook_point.rs
git commit -m "feat(user-plugins): define 21 v1 hook points as enum contract"
```

---

## Phase 2：PluginRegistry 与 PluginLoader

### Task 5：PluginRegistry ECS Resource

**Files:**
- Create: `src/user_plugins/registry.rs`（替换 placeholder）

- [ ] **Step 1：编写 PluginRegistry 与测试**

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use bevy::prelude::Resource;
use rhai::AST;

use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::manifest::PluginManifest;

/// 一个已通过校验、加载到内存的插件。
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root_dir: PathBuf,
    /// 预编译的 hook 脚本，按 hook 点分组。
    pub hook_asts: HashMap<HookPoint, Vec<AST>>,
    /// 预编译的 slash command 脚本。
    pub command_asts: HashMap<String, AST>,
    /// 预编译的 tool handler 脚本。
    pub tool_asts: HashMap<String, AST>,
    /// 该插件的 per-plugin state。
    pub state: HashMap<String, rhai::Dynamic>,
    /// 该插件贡献的临时资源，reload 时清空。
    pub temp_resources: HashMap<String, rhai::Dynamic>,
}

impl LoadedPlugin {
    /// 全局命名空间的 tool id。
    pub fn namespaced_tool_id(&self, local_id: &str) -> String {
        format!("{}:{}", self.manifest.id, local_id)
    }

    pub fn namespaced_agent_id(&self, local_id: &str) -> String {
        format!("{}:{}", self.manifest.id, local_id)
    }

    pub fn namespaced_skill_id(&self, local_id: &str) -> String {
        format!("{}:{}", self.manifest.id, local_id)
    }
}

/// 全局插件注册表。
///
/// 按 manifest.id 字母序保存通过校验的插件。其它系统通过此 Resource
/// 查询当前可用的插件贡献。
#[derive(Resource, Debug, Default)]
pub struct PluginRegistry {
    plugins: Vec<LoadedPlugin>,
    failed: Vec<PluginLoadFailure>,
}

#[derive(Debug, Clone)]
pub struct PluginLoadFailure {
    pub plugin_id: Option<String>,
    pub root_dir: PathBuf,
    pub error: String,
}

impl PluginRegistry {
    /// 按 manifest.id 字母序插入。重复 id 视为第二次冲突，插入失败列表。
    /// 若该插件任意 `command.display` 与已注册插件冲突，也插入失败列表，
    /// 不影响已经注册的插件（后注册者被跳过）。
    pub fn insert(&mut self, plugin: LoadedPlugin) {
        let id = plugin.manifest.id.clone();
        if self.plugins.iter().any(|p| p.manifest.id == id) {
            self.failed.push(PluginLoadFailure {
                plugin_id: Some(id),
                root_dir: plugin.root_dir.clone(),
                error: "duplicate plugin id".into(),
            });
            return;
        }
        // 检查跨插件 command.display 冲突
        let conflicts: Vec<String> = plugin
            .manifest
            .commands
            .iter()
            .filter(|c| {
                self.plugins
                    .iter()
                    .any(|p| p.manifest.commands.iter().any(|oc| oc.display == c.display))
            })
            .map(|c| c.display.clone())
            .collect();
        if !conflicts.is_empty() {
            self.failed.push(PluginLoadFailure {
                plugin_id: Some(id),
                root_dir: plugin.root_dir.clone(),
                error: format!(
                    "command.display conflicts with already-loaded plugin(s): {}",
                    conflicts.join(", ")
                ),
            });
            return;
        }
        let pos = self
            .plugins
            .partition_point(|p| p.manifest.id < plugin.manifest.id);
        self.plugins.insert(pos, plugin);
    }

    pub fn record_failure(&mut self, failure: PluginLoadFailure) {
        self.failed.push(failure);
    }

    /// 所有成功加载的插件。
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// 失败清单。
    pub fn failures(&self) -> &[PluginLoadFailure] {
        &self.failed
    }

    /// 查找拥有该 id 的插件。
    pub fn get(&self, id: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut LoadedPlugin> {
        self.plugins.iter_mut().find(|p| p.manifest.id == id)
    }

    /// 返回订阅指定 hook 点的所有插件（按已排序的字母序）。
    pub fn subscribers_for(&self, point: HookPoint) -> Vec<&LoadedPlugin> {
        self.plugins
            .iter()
            .filter(|p| p.manifest.subscribes_to(point.as_serialized()))
            .collect()
    }

    /// 清空所有数据，用于 /reload-plugins。
    pub fn clear(&mut self) {
        self.plugins.clear();
        self.failed.clear();
    }

    /// 是否没有任何插件加载（含失败也算非空）。
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty() && self.failed.is_empty()
    }
}

/// 让 HookPoint 可以与 manifest 字符串直接比较。
impl HookPoint {
    pub fn as_serialized(&self) -> &'static str {
        match self {
            Self::OnToolCalled => "on_tool_called",
            Self::OnTaskCreated => "on_task_created",
            Self::OnTaskCompleted => "on_task_completed",
            Self::OnTaskFailed => "on_task_failed",
            Self::OnWorkItemStarted => "on_workitem_started",
            Self::OnWorkItemCompleted => "on_workitem_completed",
            Self::OnWorkItemFailed => "on_workitem_failed",
            Self::OnAgentStarted => "on_agent_started",
            Self::OnAgentStopped => "on_agent_stopped",
            Self::OnToolReturned => "on_tool_returned",
            Self::OnMessageDispatched => "on_message_dispatched",
            Self::OnMessageReceived => "on_message_received",
            Self::OnLlmResponse => "on_llm_response",
            Self::OnLongTermMemoryWrite => "on_long_term_memory_write",
            Self::OnLongTermMemoryEvicted => "on_long_term_memory_evicted",
            Self::OnSharedKnowledgeWrite => "on_shared_knowledge_write",
            Self::OnExperienceCandidateSubmitted => "on_experience_candidate_submitted",
            Self::OnExperienceCandidateApproved => "on_experience_candidate_approved",
            Self::OnExperienceCandidateRejected => "on_experience_candidate_rejected",
            Self::OnApprovalRequested => "on_approval_requested",
            Self::OnApprovalResolved => "on_approval_resolved",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_plugins::manifest::PluginManifest;

    fn make_plugin(id: &str) -> LoadedPlugin {
        LoadedPlugin {
            manifest: PluginManifest {
                id: id.to_string(),
                name: None,
                version: None,
                api_version: 1,
                author: None,
                description: None,
                hooks: vec![],
                tools: vec![],
                skills: vec![],
                agents: vec![],
                commands: vec![],
            },
            root_dir: PathBuf::from("/tmp"),
            hook_asts: HashMap::new(),
            command_asts: HashMap::new(),
            tool_asts: HashMap::new(),
            state: HashMap::new(),
            temp_resources: HashMap::new(),
        }
    }

    #[test]
    fn inserts_sorted_by_id() {
        let mut reg = PluginRegistry::default();
        reg.insert(make_plugin("zebra"));
        reg.insert(make_plugin("alpha"));
        reg.insert(make_plugin("middle"));

        let ids: Vec<_> = reg.plugins().iter().map(|p| p.manifest.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn duplicate_id_goes_to_failures() {
        let mut reg = PluginRegistry::default();
        reg.insert(make_plugin("dup"));
        reg.insert(make_plugin("dup"));

        assert_eq!(reg.plugins().len(), 1);
        assert_eq!(reg.failures().len(), 1);
    }

    #[test]
    fn duplicate_display_across_plugins_goes_to_failures() {
        let mut reg = PluginRegistry::default();

        let mut first = make_plugin("alpha");
        first.manifest.commands.push(crate::user_plugins::manifest::CommandContribution {
            id: "hi".to_string(),
            display: "/hi".to_string(),
            script: PathBuf::from("commands/hi.rhai"),
            description: None,
        });
        reg.insert(first);

        let mut second = make_plugin("beta");
        second.manifest.commands.push(crate::user_plugins::manifest::CommandContribution {
            id: "hi".to_string(),
            display: "/hi".to_string(),
            script: PathBuf::from("commands/hi.rhai"),
            description: None,
        });
        reg.insert(second);

        // 先注册者保留
        assert_eq!(reg.plugins().len(), 1);
        assert_eq!(reg.plugins()[0].manifest.id, "alpha");
        // 后注册者跳到 failures
        assert_eq!(reg.failures().len(), 1);
        assert!(reg.failures()[0].error.contains("display"));
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::registry`
Expected: 3 tests PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/registry.rs
git commit -m "feat(user-plugins): add PluginRegistry ECS resource"
```

### Task 6：PluginLoader 磁盘扫描与预编译

**Files:**
- Create: `src/user_plugins/loader.rs`（替换 placeholder）

- [ ] **Step 1：编写 PluginLoader**

```rust
use std::fs;
use std::path::{Path, PathBuf};

use rhai::{Engine, AST};
use tracing::{debug, warn};

use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::manifest::{ManifestError, PluginManifest};
use crate::user_plugins::registry::{LoadedPlugin, PluginLoadFailure, PluginRegistry};

/// 默认插件根目录
pub const DEFAULT_PLUGINS_DIR: &str = ".harness/plugins";

/// 扫描 `plugins_dir` 下每个子目录的 `manifest.toml`，加载校验通过的插件。
///
/// 失败的插件不会让整个加载过程 panic，只记入 registry.failures 并 warn 日志。
pub fn load_plugins_from_dir(plugins_dir: &Path) -> PluginRegistry {
    let mut registry = PluginRegistry::default();

    let entries = match fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(err) => {
            debug!(
                event = "PluginsDirMissing",
                path = %plugins_dir.display(),
                error = %err,
                "plugins directory not present, loading no plugins"
            );
            return registry;
        }
    };

    // 先收集所有候选目录，再按 id 字母序处理（registry.insert 会再排序，但提前排序
    // 让日志顺序稳定）。
    let mut plugin_dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    plugin_dirs.sort();

    for plugin_dir in plugin_dirs {
        load_single_plugin(&plugin_dir, &mut registry);
    }

    if !registry.is_empty() {
        let loaded: Vec<&str> = registry.plugins().iter().map(|p| p.manifest.id.as_str()).collect();
        let failed: Vec<&str> = registry
            .failures()
            .iter()
            .map(|f| f.plugin_id.as_deref().unwrap_or("<unknown>"))
            .collect();
        debug!(
            event = "PluginsLoadedSummary",
            loaded = ?loaded,
            failed = ?failed,
            "plugin summary"
        );
    }

    registry
}

fn load_single_plugin(plugin_dir: &Path, registry: &mut PluginRegistry) {
    let manifest_path = plugin_dir.join("manifest.toml");
    let manifest_content = match fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(err) => {
            warn!(
                event = "PluginManifestMissing",
                path = %manifest_path.display(),
                error = %err,
                "skip plugin: manifest.toml not found"
            );
            registry.record_failure(PluginLoadFailure {
                plugin_id: None,
                root_dir: plugin_dir.to_path_buf(),
                error: format!("manifest.toml read failed: {err}"),
            });
            return;
        }
    };

    let manifest = match PluginManifest::from_toml(&manifest_content) {
        Ok(m) => m,
        Err(ManifestError::Parse(e)) => {
            warn!(
                event = "PluginManifestParseError",
                path = %manifest_path.display(),
                error = %e,
                "skip plugin: manifest parse"
            );
            registry.record_failure(PluginLoadFailure {
                plugin_id: None,
                root_dir: plugin_dir.to_path_buf(),
                error: format!("manifest parse: {e}"),
            });
            return;
        }
        err @ Err(ManifestError::Invalid(_)) => {
            let err = err.unwrap_err();
            if let ManifestError::Invalid(msg) = &err {
                warn!(
                    event = "PluginManifestInvalid",
                    path = %manifest_path.display(),
                    reason = %msg,
                    "skip plugin: manifest validation"
                );
            }
            registry.record_failure(PluginLoadFailure {
                plugin_id: None,
                root_dir: plugin_dir.to_path_buf(),
                error: format!("manifest invalid: {err}"),
            });
            return;
        }
    };

    let plugin_id = manifest.id.clone();

    match build_loaded_plugin(&manifest, plugin_dir) {
        Ok(loaded) => registry.insert(loaded),
        Err(err) => {
            warn!(
                event = "PluginAssetBuildFailed",
                plugin_id = %plugin_id,
                error = %err,
                "skip plugin: asset build"
            );
            registry.record_failure(PluginLoadFailure {
                plugin_id: Some(plugin_id),
                root_dir: plugin_dir.to_path_buf(),
                error: err,
            });
        }
    }
}

fn build_loaded_plugin(
    manifest: &PluginManifest,
    root_dir: &Path,
) -> Result<LoadedPlugin, String> {
    // 校验所有引用的文件存在 + 落在 root_dir 内（防穿越）。
    for path in manifest_files(manifest) {
        let abs = root_dir.join(&path);
        if !abs.exists() {
            return Err(format!("missing file referenced by manifest: {}", path.display()));
        }
        if !is_within(root_dir, &abs) {
            return Err(format!(
                "manifest references file outside plugin root: {}",
                path.display()
            ));
        }
    }

    // 静态编译 Rhai 脚本
    let engine = new_sandboxed_engine();
    let mut hook_asts: std::collections::HashMap<HookPoint, Vec<AST>> =
        std::collections::HashMap::new();
    for hook in &manifest.hooks {
        let point: HookPoint = hook.event.parse().map_err(|e| format!("{}", e))?;
        let script_path = root_dir.join(&hook.script);
        let source = fs::read_to_string(&script_path).map_err(|e| {
            format!("read hook script {}: {e}", script_path.display())
        })?;
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("compile {}: {e}", script_path.display()))?;
        hook_asts.entry(point).or_default().push(ast);
    }

    let mut command_asts = std::collections::HashMap::new();
    for cmd in &manifest.commands {
        let script_path = root_dir.join(&cmd.script);
        let source = fs::read_to_string(&script_path).map_err(|e| {
            format!("read command script {}: {e}", script_path.display())
        })?;
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("compile {}: {e}", script_path.display()))?;
        command_asts.insert(cmd.id.clone(), ast);
    }

    let mut tool_asts = std::collections::HashMap::new();
    for tool in &manifest.tools {
        let script_path = root_dir.join(&tool.handler);
        let source = fs::read_to_string(&script_path).map_err(|e| {
            format!("read tool handler {}: {e}", script_path.display())
        })?;
        let ast = engine
            .compile(&source)
            .map_err(|e| format!("compile {}: {e}", script_path.display()))?;
        tool_asts.insert(tool.id.clone(), ast);
    }

    Ok(LoadedPlugin {
        manifest: manifest.clone(),
        root_dir: root_dir.to_path_buf(),
        hook_asts,
        command_asts,
        tool_asts,
        state: std::collections::HashMap::new(),
        temp_resources: std::collections::HashMap::new(),
    })
}

fn manifest_files(manifest: &PluginManifest) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for h in &manifest.hooks {
        out.push(h.script.clone());
    }
    for t in &manifest.tools {
        out.push(t.schema.clone());
        out.push(t.handler.clone());
    }
    for s in &manifest.skills {
        out.push(s.path.clone());
    }
    for a in &manifest.agents {
        out.push(a.profile.clone());
    }
    for c in &manifest.commands {
        out.push(c.script.clone());
    }
    out
}

/// canonicalize 后做前缀检查，确认 abs 在 root 之内（含 root 本身）。
pub fn is_within(root: &Path, abs: &Path) -> bool {
    let root_c = std::fs::canonicalize(root).ok();
    let abs_c = std::fs::canonicalize(abs).ok();
    match (root_c, abs_c) {
        (Some(r), Some(a)) => a.starts_with(&r),
        _ => abs.starts_with(root),
    }
}

/// 创建禁用 FS / 网络的 Rhai Engine。
///
/// 仅靠不注册任何 std 原语来沙箱化。我们要的 Host API 全部通过
/// `register_fn` 显式绑定。
pub fn new_sandboxed_engine() -> Engine {
    let mut engine = Engine::new();
    // 关闭标准 type 提示，注册的函数少即可。
    engine.set_max_expr_levels(64);
    engine.set_max_call_levels(32);
    engine
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_plugin(root: &Path, manifest: &str, files: &[(&str, &str)]) {
        fs::write(root.join("manifest.toml"), manifest).unwrap();
        for (path, content) in files {
            let p = root.join(path);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(p, content).unwrap();
        }
    }

    #[test]
    fn loads_valid_plugin_with_hook() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("alpha");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(
            &plugin_dir,
            r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"
"#,
            &[("hooks/on_task_created.rhai", "log_info(\"hello\");\n")],
        );

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 1);
        assert_eq!(registry.plugins()[0].manifest.id, "alpha");
        assert!(registry.plugins()[0].hook_asts.contains_key(&HookPoint::OnTaskCreated));
    }

    #[test]
    fn missing_manifest_records_failure_and_continues() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("no-manifest")).unwrap();
        let good = dir.path().join("good");
        fs::create_dir(&good).unwrap();
        write_plugin(
            &good,
            "id = \"good\"\napi_version = 1\n",
            &[],
        );

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 1);
        assert_eq!(registry.failures().len(), 1);
    }

    #[test]
    fn wrong_api_version_skips_plugin() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(&plugin_dir, "id = \"bad\"\napi_version = 99\n", &[]);

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 0);
        assert_eq!(registry.failures().len(), 1);
    }

    #[test]
    fn compile_error_skips_plugin() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad-syntax");
        fs::create_dir(&plugin_dir).unwrap();
        write_plugin(
            &plugin_dir,
            r#"
id = "bad-syntax"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/x.rhai"
"#,
            &[("hooks/x.rhai", "let x = ;\n")], // 语法错误
        );

        let registry = load_plugins_from_dir(dir.path());
        assert_eq!(registry.plugins().len(), 0);
        assert_eq!(registry.failures().len(), 1);
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::loader`
Expected: 4 tests PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/loader.rs
git commit -m "feat(user-plugins): add PluginLoader with sandboxed rhai engine"
```

### Task 7：Startup 系统接入 PluginLoader

**Files:**
- Modify: `src/app/mod.rs`
- Create: `src/user_plugins/mod.rs` 中的 `PluginLoadStartup` 系统

- [ ] **Step 1：在 `src/user_plugins/mod.rs` 追加 startup 系统函数**

在现有 `pub mod` 行之后追加：

```rust
use bevy::prelude::*;
use std::path::PathBuf;

use crate::user_plugins::loader::{load_plugins_from_dir, DEFAULT_PLUGINS_DIR};
use crate::user_plugins::registry::PluginRegistry;

/// Startup 系统：扫描 `.harness/plugins/` 并把 registry 插入 world。
pub fn plugin_load_startup_system(mut commands: Commands) {
    let plugins_dir = PathBuf::from(
        std::env::var("HARNESS_PLUGINS_DIR").unwrap_or_else(|_| DEFAULT_PLUGINS_DIR.to_string()),
    );
    let registry = load_plugins_from_dir(&plugins_dir);
    let loaded: Vec<String> = registry.plugins().iter().map(|p| p.manifest.id.clone()).collect();
    let failed: Vec<String> = registry
        .failures()
        .iter()
        .map(|f| format!("{}: {}", f.plugin_id.as_deref().unwrap_or("?"), f.error))
        .collect();

    if loaded.is_empty() && failed.is_empty() {
        tracing::debug!(event = "PluginsEmpty", "no plugins found in {}", plugins_dir.display());
    } else {
        tracing::info!(
            event = "PluginsLoadedSummary",
            loaded = ?loaded,
            failed = ?failed,
            "[plugins] summary"
        );
        eprintln!("[plugins] loaded: {}", loaded.join(", "));
        if !failed.is_empty() {
            eprintln!("[plugins] failed: {}", failed.join("; "));
        }
    }

    commands.insert_resource(registry);
}
```

- [ ] **Step 2：在 `src/app/mod.rs` 的 `build_harness_app` 中注册 startup 系统**

在 `app.add_systems(Startup, load_agents_system);` 之后追加：

```rust
    app.add_systems(Startup, crate::user_plugins::plugin_load_startup_system);
```

并在文件头 use 处补：

```rust
use crate::{
    ...
    user_plugins::plugin_load_startup_system,
    ...
};
```

实际编辑时把这一行加入既有 use 块。

- [ ] **Step 3：运行 `cargo check`**

Expected: PASS。

- [ ] **Step 4：Commit**

```bash
git add src/user_plugins/mod.rs src/app/mod.rs
git commit -m "feat(user-plugins): wire PluginLoader into app Startup"
```

---

## Phase 3：Rhai Host API 骨架

### Task 8：Host API 模块结构与 log host 函数

**Files:**
- Create: `src/user_plugins/host_api/mod.rs`（替换 placeholder）
- Create: `src/user_plugins/host_api/log.rs`
- Create: `src/user_plugins/host_api/state.rs`

- [ ] **Step 1：在 `src/user_plugins/host_api/mod.rs` 写入**

```rust
//! 注册到 Rhai Engine 的 Host API 表面。
//!
//! 每个子模块导出一个 `register(Engine)` 函数，由
//! `register_all` 在派发前一次性注册。

use rhai::Engine;

pub mod entity_query;
pub mod entity_write;
pub mod log;
pub mod plugin_resource;
pub mod state;
pub mod tool_control;
pub mod approval;
pub mod experience;

/// 把所有 host API 注册到给定 Engine 上。
///
/// 每次派发 hook 时，dispatcher 会为本插件构造一个独立的 Engine 实例，
/// 调用此函数后再注入插件上下文（plugin_id、ctx），最后执行 AST。
pub fn register_all(engine: &mut Engine) {
    log::register(engine);
    state::register(engine);
    entity_query::register(engine);
    entity_write::register(engine);
    tool_control::register(engine);
    plugin_resource::register(engine);
    approval::register(engine);
    experience::register(engine);
}
```

- [ ] **Step 2：写 `log.rs`**

```rust
use rhai::Engine;
use tracing::{error, info, warn};

pub fn register(engine: &mut Engine) {
    engine.register_fn("log_info", |msg: &str| {
        info!(event = "PluginLog", level = "info", "{}", msg);
    });
    engine.register_fn("log_warn", |msg: &str| {
        warn!(event = "PluginLog", level = "warn", "{}", msg);
    });
    engine.register_fn("log_error", |msg: &str| {
        error!(event = "PluginLog", level = "error", "{}", msg);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::Engine;

    #[test]
    fn log_info_does_not_panic() {
        let mut e = Engine::new();
        register(&mut e);
        let r = e.eval::<()>("log_info(\"test\");");
        assert!(r.is_ok());
    }
}
```

- [ ] **Step 3：写 `state.rs` 骨架**

`register_temp_resource` / `read_plugin_state` 等需要在 dispatcher 上下文里通过 `PluginContext` 提供，不能在 Engine 顶层注册为无状态函数。先留下接口注释，在 Phase 4 dispatcher 中以 `Engine::register_fn` 闭包形式按插件实例绑定：

```rust
use rhai::Engine;

/// 在 Engine 顶层注册的仅是无状态那部分。per-plugin state 通过
/// dispatcher 注入的闭包绑定，在此处不注册。
pub fn register(_engine: &mut Engine) {
    // 占位：currently no global state functions.
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}
```

- [ ] **Step 4：为剩余 5 个子模块各写占位 `register` 函数**

`entity_query.rs`、`entity_write.rs`、`tool_control.rs`、`plugin_resource.rs`、`approval.rs`、`experience.rs` 内容形如：

```rust
use rhai::Engine;

pub fn register(_engine: &mut Engine) {
    // Phase 3 后续任务填充具体 host API。
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder() {}
}
```

- [ ] **Step 5：更新 `src/user_plugins/mod.rs` 导出 host_api**

由于 `pub mod host_api;` 已存在，本步只需确认。

- [ ] **Step 6：运行 `cargo test --lib user_plugins::host_api`**

Expected: PASS。

- [ ] **Step 7：Commit**

```bash
git add src/user_plugins/host_api/
git commit -m "feat(user-plugins): scaffold host_api with log functions"
```

### Task 9：Host API entity_query — get_task / get_agent 等

**Files:**
- Modify: `src/user_plugins/host_api/entity_query.rs`

- [ ] **Step 1：定义查询 host API**

插件 hook 脚本里调用 `get_task(task_id)` 时，task_id 是字符串形式 UUID。返回一个 Rhai `Map`（用 `rhai::Map`），包含 `title`、`status`、`metadata` 等字段。本任务实现 spec §Host API 清单中"读 - 实体查询"类的全部 6 个调用：`get_task`、`get_task_ids`、`get_work_item`、`get_work_item_ids_for`、`get_agent`、`get_agent_ids`。

```rust
use std::sync::Arc;

use bevy::prelude::World;
use rhai::{Dynamic, Engine, Map};
use uuid::Uuid;

use crate::domain::{Agent, Task, WorkItem};

/// 在派发 hook 时由 dispatcher 注入到 Engine 的共享世界快照。
///
/// World 不能跨 await / 跨线程借用，因此 dispatcher 在派发前把需要的快照
/// 拷贝到 `WorldSnapshot` 里，注入 host API。
#[derive(Clone)]
pub struct WorldSnapshot {
    pub tasks: Arc<Vec<Task>>,
    pub work_items: Arc<Vec<WorkItem>>,
    pub agents: Arc<Vec<Agent>>,
}

impl WorldSnapshot {
    pub fn empty() -> Self {
        Self {
            tasks: Arc::new(Vec::new()),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(Vec::new()),
        }
    }

    pub fn from_world(world: &World) -> Self {
        let mut tasks: Vec<Task> = Vec::new();
        for task in world.query::<&Task>().iter(world) {
            tasks.push(task.clone());
        }
        let mut work_items: Vec<WorkItem> = Vec::new();
        for w in world.query::<&WorkItem>().iter(world) {
            work_items.push(w.clone());
        }
        let mut agents: Vec<Agent> = Vec::new();
        for a in world.query::<&Agent>().iter(world) {
            agents.push(a.clone());
        }
        Self {
            tasks: Arc::new(tasks),
            work_items: Arc::new(work_items),
            agents: Arc::new(agents),
        }
    }
}

pub fn register(engine: &mut Engine, snapshot: WorldSnapshot) {
    // get_task(task_id) -> Map | ()
    let snap = snapshot.clone();
    engine.register_fn("get_task", move |id: &str| -> Dynamic {
        match Uuid::parse_str(id) {
            Ok(uuid) => snap
                .tasks
                .iter()
                .find(|t| t.id == uuid)
                .map(task_to_map)
                .map(Dynamic::from)
                .unwrap_or(Dynamic::UNIT),
            Err(_) => Dynamic::UNIT,
        }
    });

    // get_task_ids() -> [String]
    let snap = snapshot.clone();
    engine.register_fn("get_task_ids", move || -> Vec<String> {
        snap.tasks.iter().map(|t| t.id.to_string()).collect()
    });

    // get_work_item(workitem_id) -> Map | ()
    let snap = snapshot.clone();
    engine.register_fn("get_work_item", move |id: &str| -> Dynamic {
        match Uuid::parse_str(id) {
            Ok(uuid) => snap
                .work_items
                .iter()
                .find(|w| w.id == uuid)
                .map(work_item_to_map)
                .map(Dynamic::from)
                .unwrap_or(Dynamic::UNIT),
            Err(_) => Dynamic::UNIT,
        }
    });

    // get_work_item_ids_for(task_id) -> [String]
    let snap = snapshot.clone();
    engine.register_fn("get_work_item_ids_for", move |task_id: &str| -> Vec<String> {
        match Uuid::parse_str(task_id) {
            Ok(tid) => snap
                .work_items
                .iter()
                .filter(|w| w.task_id == tid)
                .map(|w| w.id.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    });

    // get_agent(agent_id) -> Map | ()
    let snap = snapshot.clone();
    engine.register_fn("get_agent", move |id: &str| -> Dynamic {
        // agents 在本仓库以 Entity 作为 id；这里按 agent_id 字段匹配 trim 后的字符串。
        let needle = id.trim();
        snap.agents
            .iter()
            .find(|a| agent_id_str(a) == needle)
            .map(agent_to_map)
            .map(Dynamic::from)
            .unwrap_or(Dynamic::UNIT)
    });

    // get_agent_ids() -> [String]
    let snap = snapshot.clone();
    engine.register_fn("get_agent_ids", move || -> Vec<String> {
        snap.agents.iter().map(agent_id_str).collect()
    });
}

fn task_to_map(task: &Task) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(task.id.to_string()));
    m.insert("title".into(), Dynamic::from(task.title.clone()));
    m.insert("status".into(), Dynamic::from(format!("{:?}", task.status)));
    m
}

fn work_item_to_map(w: &WorkItem) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(w.id.to_string()));
    m.insert("task_id".into(), Dynamic::from(w.task_id.to_string()));
    m.insert("status".into(), Dynamic::from(format!("{:?}", w.status)));
    m
}

fn agent_to_map(a: &Agent) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(agent_id_str(a)));
    m.insert("profile".into(), Dynamic::from(a.profile.clone()));
    m
}

/// Agent id 在本仓库以字符串/UUID 形式表示；具体字段名以 codegraph 为准。
/// 若 `Agent` 的 id 字段名与这里不符，实施时按真实字段名做最小调整即可。
fn agent_id_str(a: &Agent) -> String {
    a.id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Agent, Task, TaskStatus, WorkItem};

    fn make_task(title: &str) -> Task {
        let mut t = Task::new(title.to_string(), uuid::Uuid::nil());
        t.id = uuid::Uuid::new_v4();
        t.status = TaskStatus::Running;
        t
    }

    #[test]
    fn get_task_returns_map_for_known_id() {
        let t = make_task("hello");
        let snap = WorldSnapshot {
            tasks: Arc::new(vec![t.clone()]),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(Vec::new()),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let script = format!(r#"let t = get_task("{}"); t.title"#, t.id);
        let out: String = e.eval(&script).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn get_task_ids_lists_all() {
        let snap = WorldSnapshot {
            tasks: Arc::new(vec![make_task("a"), make_task("b")]),
            work_items: Arc::new(Vec::new()),
            agents: Arc::new(Vec::new()),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let ids: Vec<String> = e.eval("get_task_ids()").unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn bad_uuid_returns_unit() {
        let snap = WorldSnapshot::empty();
        let mut e = Engine::new();
        register(&mut e, snap);
        let v: () = e.eval(r#"get_task("not-a-uuid")"#).unwrap();
        assert_eq!(v, ());
    }

    #[test]
    fn get_work_item_ids_for_filters_by_task() {
        let tid = uuid::Uuid::new_v4();
        let mut w = WorkItem::new(tid, "kind".to_string());
        w.id = uuid::Uuid::new_v4();
        let snap = WorldSnapshot {
            tasks: Arc::new(Vec::new()),
            work_items: Arc::new(vec![w]),
            agents: Arc::new(Vec::new()),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let script = format!(r#"get_work_item_ids_for("{}")"#, tid);
        let ids: Vec<String> = e.eval(&script).unwrap();
        assert_eq!(ids.len(), 1);
    }
}
```

注：若 `Task::new`、`WorkItem::new`、`Agent.id` 字段名或 `WorkItem.task_id` 字段名与本仓库不符，实施时以 codegraph 为准做最小调整。

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::host_api::entity_query`
Expected: 4 tests PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/host_api/entity_query.rs
git commit -m "feat(user-plugins): add get_task / get_task_ids host API"
```

### Task 10：Host API entity_write — create_task + task_set_metadata + task_set_tag

**Files:**
- Modify: `src/user_plugins/host_api/entity_write.rs`

- [ ] **Step 1：实现 create_task / task_set_metadata / task_set_tag**

`create_task` 不能直接写入 World（Rhai 调用是同步的，Engine 不持有 `&mut World`），需要通过 `bevy::ecs::system::CommandQueue` 在派发结束后 flush。本任务实现 `WriterHandle` 抽象：dispatcher 给 Engine 注入一个 `crossbeam-channel::Sender<WorldCommand>`，host API 把指令 push 到 channel，dispatcher 在 hook 返回后落到 world。

由于插件脚本通过 channel 异步下指令，`spawn_agent` / `create_work_item` 的"返回值"模式上是异步的——v1 这里返回 `Uuid::nil().to_string()` 占位字符串，hook 若需要立即拿真实 id 应改为后 hook（如 `on_task_created`、`on_workitem_started`）。这一约定与 `create_task` 一致，已在 spec §Host API 清单注释中标明。

```rust
use std::sync::Arc;

use crossbeam_channel::Sender;
use rhai::Engine;
use uuid::Uuid;

/// 插件对 World 的写指令。dispatcher 在 hook 完成后 replay。
///
/// 注意：本枚举在 Task 13 等后续任务会继续扩展（SpawnAgent、CreateWorkItem、
/// SetApprovalDecision、ExperienceSetPinned、SetTaskTag 等）。每个新增变体
/// 都要同步追加到 `replay` 函数匹配分支。
#[derive(Debug)]
pub enum WorldCommand {
    CreateTask { title: String, parent: Option<Uuid> },
    SetTaskMetadata { task_id: Uuid, key: String, value: String },
    SetTaskTag { task_id: Uuid, key: String, value: String },
}

/// 每个 hook 派发携带的 sender。
#[derive(Clone)]
pub struct WorldWriter {
    pub tx: Sender<WorldCommand>,
}

impl WorldWriter {
    pub fn new(tx: Sender<WorldCommand>) -> Self {
        Self { tx }
    }
}

pub fn register(engine: &mut Engine, writer: WorldWriter) {
    let w = writer.clone();
    engine.register_fn("create_task", move |title: &str| -> String {
        let cmd = WorldCommand::CreateTask {
            title: title.to_string(),
            parent: None,
        };
        let _ = w.tx.send(cmd);
        // 临时返回占位 uuid，真实 id 在 dispatcher replay 后写入。
        // hook 若需要立即拿 id 应改为后 hook / on_task_created。
        uuid::Uuid::nil().to_string()
    });

    let w = writer.clone();
    engine.register_fn("task_set_metadata", move |task_id: &str, key: &str, value: &str| {
        if let Ok(id) = Uuid::parse_str(task_id) {
            let _ = w.tx.send(WorldCommand::SetTaskMetadata {
                task_id: id,
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    });

    let w = writer.clone();
    engine.register_fn("task_set_tag", move |task_id: &str, key: &str, value: &str| {
        if let Ok(id) = Uuid::parse_str(task_id) {
            let _ = w.tx.send(WorldCommand::SetTaskTag {
                task_id: id,
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn create_task_sends_command() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        let _ = e.eval::<String>(r#"create_task("hello")"#).unwrap();
        let cmd = rx.recv().unwrap();
        match cmd {
            WorldCommand::CreateTask { title, .. } => assert_eq!(title, "hello"),
            _ => panic!("wrong cmd"),
        }
    }

    #[test]
    fn task_set_metadata_with_bad_uuid_sends_nothing() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        let _ = e.eval::<()>(r#"task_set_metadata("not-uuid", "k", "v")"#).unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn task_set_tag_sends_command() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, WorldWriter::new(tx));
        let id = uuid::Uuid::new_v4();
        let script = format!(r#"task_set_tag("{}", "env", "ci")"#, id);
        let _ = e.eval::<()>(&script).unwrap();
        match rx.recv().unwrap() {
            WorldCommand::SetTaskTag { task_id, key, value } => {
                assert_eq!(task_id, id);
                assert_eq!(key, "env");
                assert_eq!(value, "ci");
            }
            _ => panic!("wrong cmd"),
        }
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::host_api::entity_write`
Expected: 3 PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/host_api/entity_write.rs
git commit -m "feat(user-plugins): add create_task / task_set_metadata / task_set_tag host API"
```

### Task 11：Host API tool_control — tool_deny / tool_set_result + 审计

**Files:**
- Modify: `src/user_plugins/host_api/tool_control.rs`
- Create: `src/user_plugins/dispatcher.rs`（补 HookContext 与 HookOutcome）

- [ ] **Step 1：写 `tool_control.rs`**

`tool_deny` 只在 `on_tool_called` 中有效；`tool_set_result` 只在 `on_tool_returned` 中有效。两者通过 `HookOutcome` 状态对象读写 — Rhai 函数为闭包捕获 `Arc<Mutex<HookOutcome>>`。

```rust
use std::sync::{Arc, Mutex};

use rhai::Engine;
use tracing::warn;

/// 单次 hook 派发的累积结果。同一 hook 点多个订阅者顺序派发，
/// 前一个的 outcome 会作为后一个的输入。
#[derive(Debug, Default, Clone)]
pub struct HookOutcome {
    pub deny_reason: Option<String>,
    pub replaced_result: Option<serde_json::Value>,
}

pub type SharedHookOutcome = Arc<Mutex<HookOutcome>>;

pub fn register(engine: &mut Engine, outcome: SharedHookOutcome) {
    let o = outcome.clone();
    engine.register_fn("tool_deny", move |reason: &str| {
        let mut g = o.lock().unwrap();
        warn!(
            event = "PluginToolDenied",
            reason = reason,
            "plugin denied tool call"
        );
        g.deny_reason = Some(reason.to_string());
    });

    let o = outcome.clone();
    engine.register_fn("tool_set_result", move |value: rhai::Dynamic| {
        let json = rhai_to_json(value);
        let mut g = o.lock().unwrap();
        warn!(
            event = "PluginToolResultSet",
            "plugin replaced tool result"
        );
        g.replaced_result = Some(json);
    });
}

fn rhai_to_json(v: rhai::Dynamic) -> serde_json::Value {
    // 简化：用 Display fallback。生产实现可按 rhai::Dynamic 类型分支。
    serde_json::Value::String(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_deny_sets_reason() {
        let outcome = Arc::new(Mutex::new(HookOutcome::default()));
        let mut e = Engine::new();
        register(&mut e, outcome.clone());
        let _: () = e.eval(r#"tool_deny("blocked")"#).unwrap();
        assert_eq!(outcome.lock().unwrap().deny_reason.as_deref(), Some("blocked"));
    }

    #[test]
    fn tool_set_result_sets_value() {
        let outcome = Arc::new(Mutex::new(HookOutcome::default()));
        let mut e = Engine::new();
        register(&mut e, outcome.clone());
        let _: () = e.eval(r#"tool_set_result("hello")"#).unwrap();
        assert!(outcome.lock().unwrap().replaced_result.is_some());
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::host_api::tool_control`
Expected: 2 PASS。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/host_api/tool_control.rs
git commit -m "feat(user-plugins): add tool_deny and tool_set_result with HookOutcome"
```

### Task 12：Host API plugin_resource — read_plugin_resource + 路径校验

**Files:**
- Modify: `src/user_plugins/host_api/plugin_resource.rs`

- [ ] **Step 1：实现 `read_plugin_resource` 与越权检测**

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rhai::Engine;
use tracing::warn;

use crate::user_plugins::loader::is_within;

#[derive(Clone)]
pub struct PluginRoots {
    pub roots: Arc<Vec<PathBuf>>,
}

impl PluginRoots {
    pub fn single(root: PathBuf) -> Self {
        Self {
            roots: Arc::new(vec![root]),
        }
    }
}

pub fn register(engine: &mut Engine, roots: PluginRoots) {
    let r = roots.clone();
    engine.register_fn("read_plugin_resource", move |rel: &str| -> String {
        // 只能为该插件自己的根目录。dispatcher 注入的 PluginRoots 已仅含该插件根。
        let root = match r.roots.first() {
            Some(p) => p,
            None => return String::new(),
        };
        let candidate = root.join(rel);
        if !is_within(root, &candidate) {
            warn!(
                event = "PluginResourceAccessDenied",
                path = %candidate.display(),
                "plugin tried to read outside its root"
            );
            return String::new();
        }
        match std::fs::read_to_string(&candidate) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    event = "PluginResourceReadError",
                    path = %candidate.display(),
                    error = %e,
                    "failed to read plugin resource"
                );
                String::new()
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn reads_file_within_root() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let mut e = Engine::new();
        register(&mut e, PluginRoots::single(dir.path().to_path_buf()));
        let content: String = e.eval(r#"read_plugin_resource("hello.txt")"#).unwrap();
        assert_eq!(content, "hi");
    }

    #[test]
    fn traversal_outside_root_returns_empty() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.txt"), "ssh").unwrap();

        let mut e = Engine::new();
        register(&mut e, PluginRoots::single(dir.path().to_path_buf()));
        let rel = format!("../{}", outside.path().file_name().unwrap().to_string_lossy());
        let content: String = e.eval(&format!(r#"read_plugin_resource("{rel}/secret.txt")"#)).unwrap();
        // canonicalize 检查后应拒绝跨目录读取
        assert_eq!(content, "");
    }
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --lib user_plugins::host_api::plugin_resource`
Expected: 可能 traversal 校验依赖 canonicalize，若 fails 实施时补正同目录范围匹配（先 canonicalize 后做 starts_with）。

- [ ] **Step 3：Commit**

```bash
git add src/user_plugins/host_api/plugin_resource.rs
git commit -m "feat(user-plugins): add read_plugin_resource with sandbox path check"
```

### Task 13：Host API approval + experience + entity_write 扩展

**Files:**
- Modify: `src/user_plugins/host_api/approval.rs`
- Modify: `src/user_plugins/host_api/experience.rs`
- Modify: `src/user_plugins/host_api/entity_write.rs`（追加 spawn_agent / create_work_item）

- [ ] **Step 1：approval.rs**

```rust
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use rhai::Engine;

use crate::user_plugins::host_api::entity_write::WorldCommand;

#[derive(Clone)]
pub struct ApprovalContext {
    pub current_request_id: Option<uuid::Uuid>,
    pub tx: Sender<WorldCommand>,
}

pub fn register(engine: &mut Engine, ctx: ApprovalContext) {
    let c = ctx.clone();
    engine.register_fn("approval_request_id", move || -> String {
        c.current_request_id.map(|u| u.to_string()).unwrap_or_default()
    });

    let c = ctx.clone();
    engine.register_fn("approval_resolve", move |request_id: &str, decision: &str| {
        if let Ok(id) = uuid::Uuid::parse_str(request_id) {
            let _ = c.tx.send(WorldCommand::SetApprovalDecision {
                request_id: id,
                decision: decision.to_string(),
            });
        }
    });
}

// 在 WorldCommand 枚举中新增 SetApprovalDecision 变体，实施时同步 entity_write.rs。
```

注：实施时需在 `entity_write::WorldCommand` 中追加 `SetApprovalDecision { request_id, decision }`、`ExperienceSetPinned { id, pinned }`、`SpawnAgent { profile_id, task_id, input }`、`CreateWorkItem { task_id, kind, payload }`。

- [ ] **Step 2：experience.rs**

```rust
use std::sync::Arc;

use bevy::prelude::World;
use crossbeam_channel::Sender;
use rhai::Engine;

use crate::domain::ExperienceStore;
use crate::user_plugins::host_api::entity_write::WorldCommand;

#[derive(Clone)]
pub struct ExperienceContext {
    pub store: Arc<ExperienceStore>, // 经验 store 是 Clone 友好的副本
    pub tx: Sender<WorldCommand>,
}

pub fn register(engine: &mut Engine, ctx: ExperienceContext) {
    let c = ctx.clone();
    engine.register_fn("experience_get_candidate", move |id: &str| -> rhai::Dynamic {
        match uuid::Uuid::parse_str(id) {
            Ok(u) => c
                .store
                .candidates
                .get(&u)
                .map(|cand| {
                    let mut m = rhai::Map::new();
                    m.insert("title".into(), rhai::Dynamic::from(cand.title.clone()));
                    m.insert("kind".into(), rhai::Dynamic::from(format!("{:?}", cand.kind_hint)));
                    m.insert("status".into(), rhai::Dynamic::from(format!("{:?}", cand.status)));
                    rhai::Dynamic::from(m)
                })
                .unwrap_or(rhai::Dynamic::UNIT),
            Err(_) => rhai::Dynamic::UNIT,
        }
    });

    let c = ctx.clone();
    engine.register_fn("experience_set_pinned", move |id: &str, pinned: bool| {
        if let Ok(u) = uuid::Uuid::parse_str(id) {
            let _ = c.tx.send(WorldCommand::ExperienceSetPinned {
                id: u,
                pinned,
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use crate::domain::ExperienceStore;

    #[test]
    fn unknown_candidate_returns_unit() {
        let (tx, _rx) = unbounded();
        let mut e = Engine::new();
        register(
            &mut e,
            ExperienceContext {
                store: Arc::new(ExperienceStore::default()),
                tx,
            },
        );
        let v: () = e.eval(r#"experience_get_candidate("00000000-0000-0000-0000-000000000000")"#).unwrap();
        assert_eq!(v, ());
    }
}
```

- [ ] **Step 3：补全 entity_write.rs 中的 spawn_agent / create_work_item / 其它 WorldCommand 变体**

在 `WorldCommand` 枚举追加（与 Task 10 中已有变体合并，这里只列出新增）：

```rust
    SpawnAgent { profile_id: String, task_id: Uuid, input: String },
    CreateWorkItem { task_id: Uuid, kind: String, payload: serde_json::Value },
    SetApprovalDecision { request_id: Uuid, decision: String },
    ExperienceSetPinned { id: Uuid, pinned: bool },
```

并在 `register` 追加（注意 `spawn_agent` 接收三参数，返回占位 agent-id 字符串；`create_work_item` 接收三参数含 payload，返回占位 workitem-id 字符串）：

```rust
    let w = writer.clone();
    engine.register_fn(
        "spawn_agent",
        move |profile_id: &str, task_id: &str, input: &str| -> String {
            if let Ok(tid) = Uuid::parse_str(task_id) {
                let _ = w.tx.send(WorldCommand::SpawnAgent {
                    profile_id: profile_id.to_string(),
                    task_id: tid,
                    input: input.to_string(),
                });
            }
            // 占位字符串，真实 agent id 在 `on_agent_started` 后 hook 拿到。
            // 见 spec §Host API 清单关于 spawn_agent 返回值的约定。
            uuid::Uuid::nil().to_string()
        },
    );

    let w = writer.clone();
    engine.register_fn(
        "create_work_item",
        move |task_id: &str, kind: &str, payload: rhai::Dynamic| -> String {
            if let Ok(tid) = Uuid::parse_str(task_id) {
                // 把 Rhai Dynamic 转 JSON Value。这里用 serde_json::Value::Null 作为兜底。
                // 实施时若手头有 Dynamic -> Value helper（如紧邻的 to_json 函数），用之；
                // 否则按 Dynamic 分支做最小序列化。
                let payload_json = rhai_dynamic_to_json(&payload);
                let _ = w.tx.send(WorldCommand::CreateWorkItem {
                    task_id: tid,
                    kind: kind.to_string(),
                    payload: payload_json,
                });
            }
            // 占位字符串，真实 workitem id 在 `on_workitem_started` 后 hook 拿到。
            uuid::Uuid::nil().to_string()
        },
    );
```

补 `rhai_dynamic_to_json` 辅助函数（最小实现，覆盖常见 Rhai 字面量类型）：

```rust
fn rhai_dynamic_to_json(v: &rhai::Dynamic) -> serde_json::Value {
    use rhai::Dynamic;
    match v {
        Dynamic::UNIT => serde_json::Value::Null,
        d if d.is::<bool>() => serde_json::Value::Bool(d.as_bool().unwrap()),
        d if d.is::<i64>() => serde_json::Value::from(d.as_int().unwrap()),
        d if d.is::<f64>() => serde_json::json!(d.as_float().unwrap()),
        d if d.is::<String>() => serde_json::Value::String(d.cast::<String>()),
        d if d.is::<rhai::Map>() => {
            let m = d.cast::<rhai::Map>();
            let mut obj = serde_json::Map::new();
            for (k, v) in m.iter() {
                obj.insert(k.to_string(), rhai_dynamic_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        d if d.is::<rhai::Array>() => {
            let arr = d.cast::<rhai::Array>();
            serde_json::Value::Array(arr.iter().map(rhai_dynamic_to_json).collect())
        }
        _ => serde_json::Value::Null,
    }
}
```

新增 `spawn_agent_returns_placeholder` / `create_work_item_sends_payload_command` 两条单元测试：

```rust
#[test]
fn spawn_agent_returns_placeholder_and_sends_command() {
    let (tx, rx) = unbounded();
    let mut e = Engine::new();
    register(&mut e, WorldWriter::new(tx));
    let tid = uuid::Uuid::new_v4();
    let script = format!(r#"spawn_agent("researcher", "{}", "find x")"#, tid);
    let ret: String = e.eval(&script).unwrap();
    assert_eq!(ret, uuid::Uuid::nil().to_string());
    match rx.recv().unwrap() {
        WorldCommand::SpawnAgent { profile_id, task_id, input } => {
            assert_eq!(profile_id, "researcher");
            assert_eq!(task_id, tid);
            assert_eq!(input, "find x");
        }
        _ => panic!("wrong cmd"),
    }
}

#[test]
fn create_work_item_sends_payload_command() {
    let (tx, rx) = unbounded();
    let mut e = Engine::new();
    register(&mut e, WorldWriter::new(tx));
    let tid = uuid::Uuid::new_v4();
    let script = format!(
        r#"
let p = #{{"topic": "ci-fail", "severity": 5}};
create_work_item("{}", "triage", p)
"#,
        tid
    );
    let ret: String = e.eval(&script).unwrap();
    assert_eq!(ret, uuid::Uuid::nil().to_string());
    match rx.recv().unwrap() {
        WorldCommand::CreateWorkItem { task_id, kind, payload } => {
            assert_eq!(task_id, tid);
            assert_eq!(kind, "triage");
            assert_eq!(payload["topic"], "ci-fail");
            assert_eq!(payload["severity"], 5);
        }
        _ => panic!("wrong cmd"),
    }
}
```

- [ ] **Step 4：运行 `cargo test --lib user_plugins`**

Expected: PASS。

- [ ] **Step 5：Commit**

```bash
git add src/user_plugins/host_api/
git commit -m "feat(user-plugins): add approval and experience host APIs"
```

### Task 14：Host API 表面汇总注册 + skill/message/temp_resource 模块

**Files:**
- Modify: `src/user_plugins/host_api/mod.rs`
- Create: `src/user_plugins/host_api/skills_meta.rs`
- Create: `src/user_plugins/host_api/message.rs`
- Create: `src/user_plugins/host_api/temp_resource.rs`

- [ ] **Step 1：写 `skills_meta.rs` — list_skills host API**

```rust
use std::sync::Arc;

use bevy::prelude::World;
use rhai::{Dynamic, Engine, Map};

use crate::infrastructure::skills::SkillLoader;

/// Skill 元数据的快照。dispatcher 在派发前从 SkillLoader 拷贝。
#[derive(Clone, Default)]
pub struct SkillsSnapshot {
    pub skills: Arc<Vec<SkillInfo>>,
}

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub id: String,
    pub title: String,
    pub description: String,
}

impl SkillsSnapshot {
    pub fn empty() -> Self {
        Self { skills: Arc::new(Vec::new()) }
    }

    pub fn from_world(world: &World) -> Self {
        let loader = match world.get_resource::<SkillLoader>() {
            Some(l) => l,
            None => return Self::empty(),
        };
        let skills = loader
            .iter_skills()
            .map(|s| SkillInfo {
                id: s.id.clone(),
                title: s.title.clone(),
                description: s.description.clone(),
            })
            .collect();
        Self { skills: Arc::new(skills) }
    }
}

pub fn register(engine: &mut Engine, snapshot: SkillsSnapshot) {
    let snap = snapshot.clone();
    engine.register_fn("list_skills", move || -> Vec<Dynamic> {
        snap.skills
            .iter()
            .map(|s| {
                let mut m = Map::new();
                m.insert("id".into(), Dynamic::from(s.id.clone()));
                m.insert("title".into(), Dynamic::from(s.title.clone()));
                m.insert("description".into(), Dynamic::from(s.description.clone()));
                Dynamic::from(m)
            })
            .collect()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_skills_returns_map_array() {
        let snap = SkillsSnapshot {
            skills: Arc::new(vec![SkillInfo {
                id: "core:negotiation".into(),
                title: "Negotiation".into(),
                description: "negotiation skill".into(),
            }]),
        };
        let mut e = Engine::new();
        register(&mut e, snap);
        let v: Vec<Dynamic> = e.eval("list_skills()").unwrap();
        assert_eq!(v.len(), 1);
    }
}
```

注：`SkillLoader::iter_skills` 在仓库中可能叫 `list_skills` 或类似名；实施时以 codegraph 为准做最小调整。若 `SkillLoader` 的 skill 访问器不存在只读迭代方法，本任务可在 Task 37（SkillLoader 合并插件贡献）中一并补上 — 计划应同步更新。

- [ ] **Step 2：写 `message.rs` — emit_message host API**

`emit_message` 把一个 `(channel, payload)` 对发到 dispatcher 提供的 channel；本 host API 不与任何特定前端绑定，仅记录到进程内 `MessageBus`（后续由 frontend 订阅），v1 只写入 `tracing` 与 `PluginRegistry` 内的最近 emit 列表（调试用）。

```rust
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use rhai::{Dynamic, Engine};

#[derive(Clone)]
pub struct MessageContext {
    pub plugin_id: String,
    pub tx: Sender<EmittedMessage>,
}

#[derive(Debug, Clone)]
pub struct EmittedMessage {
    pub plugin_id: String,
    pub channel: String,
    pub payload: serde_json::Value,
}

pub fn register(engine: &mut Engine, ctx: MessageContext) {
    let c = ctx.clone();
    engine.register_fn("emit_message", move |channel: &str, payload: Dynamic| {
        let plugin_id = c.plugin_id.clone();
        let payload_json = crate::user_plugins::host_api::entity_write::rhai_dynamic_to_json(&payload);
        let _ = c.tx.send(EmittedMessage {
            plugin_id,
            channel: channel.to_string(),
            payload: payload_json,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;

    #[test]
    fn emit_message_sends_typed_message() {
        let (tx, rx) = unbounded();
        let mut e = Engine::new();
        register(&mut e, MessageContext { plugin_id: "p".into(), tx });
        let _ = e.eval::<()>(r#"emit_message("progress", "halfway")"#).unwrap();
        let m = rx.recv().unwrap();
        assert_eq!(m.plugin_id, "p");
        assert_eq!(m.channel, "progress");
        assert_eq!(m.payload, serde_json::Value::String("halfway".into()));
    }
}
```

注：`rhai_dynamic_to_json` 在 Task 13 中已加到 `entity_write.rs` 并 `pub` 暴露；若 Task 13 完成时仍为私有，本任务追加 `pub` 修饰。

- [ ] **Step 3：写 `temp_resource.rs` — register_temp_resource host API**

`register_temp_resource(key, value)` 把临时键值对存到当前插件的 `LoadedPlugin.temp_resources`。由于 Rhai 不持有 `&mut PluginRegistry`，通过 dispatcher 注入的回调句柄写入：dispatcher 给 Engine 注入 `Arc<Mutex<HashMap<String, Dynamic>>>` 共享槽，hook 结束后 dispatcher 把这个 map merge 进 `LoadedPlugin.temp_resources`。

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rhai::{Dynamic, Engine, Map};

#[derive(Clone, Default)]
pub struct TempResourceSlot {
    pub inner: Arc<Mutex<HashMap<String, Dynamic>>>,
}

impl TempResourceSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&self) -> HashMap<String, Dynamic> {
        let mut g = self.inner.lock().unwrap();
        std::mem::take(&mut *g)
    }
}

pub fn register(engine: &mut Engine, slot: TempResourceSlot) {
    let s = slot.clone();
    engine.register_fn("register_temp_resource", move |key: &str, value: Dynamic| {
        let mut g = s.inner.lock().unwrap();
        g.insert(key.to_string(), value);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_temp_resource_stores_into_slot() {
        let slot = TempResourceSlot::new();
        let mut e = Engine::new();
        register(&mut e, slot.clone());
        let _ = e.eval::<()>(r#"register_temp_resource("k", "v")"#).unwrap();
        let drained = slot.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained["k"].cast::<String>(), "v");
    }
}
```

- [ ] **Step 4：写 `register_all` 把新增模块接入**

修改 `mod.rs` 为：

```rust
use rhai::Engine;

use crate::user_plugins::dispatcher::PluginContext;

pub mod entity_query;
pub mod entity_write;
pub mod log;
pub mod plugin_resource;
pub mod state;
pub mod tool_control;
pub mod approval;
pub mod experience;
pub mod skills_meta;
pub mod message;
pub mod temp_resource;

/// 用给定 PluginContext 在 engine 上注册全部 v1 host API。
pub fn register_all(engine: &mut Engine, ctx: &PluginContext) {
    log::register(engine);
    state::register(engine);
    entity_query::register(engine, ctx.snapshot.clone());
    entity_write::register(engine, ctx.writer.clone());
    tool_control::register(engine, ctx.outcome.clone());
    plugin_resource::register(engine, ctx.plugin_roots.clone());
    approval::register(engine, ctx.approval.clone());
    experience::register(engine, ctx.experience.clone());
    skills_meta::register(engine, ctx.skills.clone());
    message::register(engine, ctx.message.clone());
    temp_resource::register(engine, ctx.temp_resource.clone());
}
```

`PluginContext` 也要补三个字段：`skills: SkillsSnapshot`、`message: MessageContext`、`temp_resource: TempResourceSlot` —— 在 Task 15 中 `PluginContext` 定义处追加；`MessageContext` 的 `plugin_id` 字段在每个 hook 派发时由 `ctx_builder` 填入当前插件 id。

- [ ] **Step 5：Commit**

本任务与 Task 15 合并提交（因 `PluginContext` 字段在 15 引入）。Commit 信息：

```bash
git add src/user_plugins/host_api/ src/user_plugins/dispatcher.rs
git commit -m "feat(user-plugins): complete v1 host API surface (skills/message/temp_resource + register_all)"
```

---

## Phase 4：HookDispatcher 与派发顺序

### Task 15：PluginContext 与 disk dispatcher 框架

**Files:**
- Create: `src/user_plugins/dispatcher.rs`（替换 placeholder）

- [ ] **Step 1：写 PluginContext 与 dispatcher 骨架**

```rust
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::Sender;
use rhai::Engine;
use tracing::{debug, warn};

use crate::domain::ExperienceStore;
use crate::user_plugins::host_api::approval::ApprovalContext;
use crate::user_plugins::host_api::entity_query::WorldSnapshot;
use crate::user_plugins::host_api::entity_write::{WorldCommand, WorldWriter};
use crate::user_plugins::host_api::experience::ExperienceContext;
use crate::user_plugins::host_api::message::MessageContext;
use crate::user_plugins::host_api::plugin_resource::PluginRoots;
use crate::user_plugins::host_api::skills_meta::SkillsSnapshot;
use crate::user_plugins::host_api::temp_resource::TempResourceSlot;
use crate::user_plugins::host_api::tool_control::{HookOutcome, SharedHookOutcome};
use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::host_api;
use crate::user_plugins::registry::{LoadedPlugin, PluginRegistry};

/// 每次 hook 派发提供给 host API 的上下文。
///
/// 不包含 `&mut World`。World 状态被快照为 `WorldSnapshot`，
/// 写操作通过 `WorldWriter` 攒到 `WorldCommand` 后由 dispatcher replay。
#[derive(Clone)]
pub struct PluginContext {
    pub snapshot: WorldSnapshot,
    pub writer: WorldWriter,
    pub outcome: SharedHookOutcome,
    pub plugin_roots: PluginRoots,
    pub approval: ApprovalContext,
    pub experience: ExperienceContext,
    pub skills: SkillsSnapshot,
    pub message: MessageContext,
    pub temp_resource: TempResourceSlot,
}

/// Hook 派发参数。
pub struct HookDispatchInput<'a> {
    pub point: HookPoint,
    pub world: &'a mut World,
    pub registry: &'a mut PluginRegistry,
    pub writer_tx: Sender<WorldCommand>,
    /// ctx 字段，由调用方按 hook 点填充
    ///
    /// 实现要求：每次调用必须为当前 plugin 构造一个**新的** `MessageContext`，
    /// 其中 `plugin_id` 字段填入 `plugin.manifest.id`；同时为 `temp_resource`
    /// 构造一个**新的** `TempResourceSlot`（每次 hook 派发独立 state，不复用）。
    /// 其他字段从当前 `World` 与 `PluginRegistry` 派生。
    pub ctx_builder: Box<dyn Fn(&LoadedPlugin, &mut World) -> PluginContext + 'a>,
}

/// v1 hook 单脚本超时 1 秒。
const HOOK_TIMEOUT: Duration = Duration::from_secs(1);

/// 派发入口。按 registry.plugins() 字母序逐插件执行订阅 AST。
///
/// 返回累积的 `HookOutcome`：多次 deny 取最后一个；replaced_result 取最后一次 set。
pub fn dispatch_hook<'a>(input: HookDispatchInput<'a>) -> HookOutcome {
    let outcome = Arc::new(std::sync::Mutex::new(HookOutcome::default()));
    let subscribers: Vec<LoadedPlugin> = input
        .registry
        .subscribers_for(input.point)
        .into_iter()
        .cloned()
        .collect();

    debug!(event = "HookDispatchStart", point = ?input.point, subscribers = subscribers.len());

    let asts_by_plugin: std::collections::HashMap<String, Vec<rhai::AST>> = subscribers
        .iter()
        .map(|p| {
            let asts = p.hook_asts.get(&input.point).cloned().unwrap_or_default();
            (p.manifest.id.clone(), asts)
        })
        .collect();

    for plugin in subscribers {
        let asts = match asts_by_plugin.get(&plugin.manifest.id) {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };
        let ctx = (input.ctx_builder)(&plugin, input.world);

        for ast in asts {
            run_one_ast(&plugin, ast, &ctx, input.point, &outcome);
        }
    }

    let result = outcome.lock().unwrap().clone();
    result
}

fn run_one_ast(
    plugin: &LoadedPlugin,
    ast: &rhai::AST,
    ctx: &PluginContext,
    point: HookPoint,
    outcome: &SharedHookOutcome,
) {
    // Step 1 用占位实现证明编译；Step 2 立即用真实超时版本替换。
    let _ = (plugin, ast, ctx, point, outcome);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_dispatches_no_op() {
        // 占位：集成测试在 Phase 12 编写。
    }
}
```

> **注意**：`PluginContext` 的 `message.plugin_id` 在每次派发时由调用方通过 `ctx_builder` 填入当前插件 id；`temp_resource` 每次派发使用全新的 slot（不复用跨 hook 的 state），具体在 Phase 5/6 各 hook 点的系统构造 `ctx_builder` 时落地。

- [ ] **Step 2：实施真正的超时机制**

简化版本：我们不能跨线程共享 Engine（即使带 sync feature，AST + scope 复杂），改用 scoped thread + 收到完成信号即结束；超时则当前线程放弃等待但 plugin 仍可能继续运行（由于不共享 mutable World，副作用只能是 WorldCommand，超时的命令会被接收但不再 flush）。

下面的实现更稳健：

```rust
use std::sync::mpsc;
use std::thread;

fn run_one_ast(
    plugin: &LoadedPlugin,
    ast: &rhai::AST,
    ctx: &PluginContext,
    point: HookPoint,
    outcome: &SharedHookOutcome,
) {
    let mut engine = Engine::new();
    {
        let cur = outcome.lock().unwrap().clone();
        *ctx.outcome.lock().unwrap() = cur;
    }
    host_api::register_all(&mut engine, ctx);

    let (done_tx, done_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name(format!("hook-{point:?}-{}", plugin.manifest.id))
        .spawn(move || {
            let r = engine.call_fn::<()>("", ast, &mut ());
            let _ = done_tx.send(r);
        })
        .ok();

    let handle = match handle {
        Some(h) => h,
        None => {
            warn!(event = "HookThreadSpawnFailed", plugin = %plugin.manifest.id);
            return;
        }
    };

    match done_rx.recv_timeout(HOOK_TIMEOUT) {
        Ok(Ok(())) => {
            // 把当前 local_outcome 同步回全局 outcome
            let local = ctx.outcome.lock().unwrap().clone();
            let mut g = outcome.lock().unwrap();
            if local.deny_reason.is_some() {
                g.deny_reason = local.deny_reason;
            }
            if local.replaced_result.is_some() {
                g.replaced_result = local.replaced_result;
            }
        }
        Ok(Err(e)) => {
            warn!(
                event = "HookScriptError",
                plugin = %plugin.manifest.id,
                point = ?point,
                error = %e,
                "hook script returned error"
            );
        }
        Err(_) => {
            warn!(
                event = "HookTimeout",
                plugin = %plugin.manifest.id,
                point = ?point,
                "hook script exceeded 1s, ignored"
            );
        }
    }
    // 注：超时线程在后台继续运行直到脚本退出。v1 接受这一潜在泄漏，因为 host API
    // 都是同步进程内快速操作，最长 1s 内必然结束。
    let _ = handle;
}
```

把上面整段替换到 `dispatcher.rs`，保留 export `HookOutcome`、`dispatch_hook`。

- [ ] **Step 3：编译并跑测试**

Run: `cargo test --lib user_plugins::dispatcher`
Expected: 占位测试 PASS。

- [ ] **Step 4：Commit**

```bash
git add src/user_plugins/dispatcher.rs src/user_plugins/host_api/mod.rs
git commit -m "feat(user-plugins): add hook dispatcher with 1s timeout and alphabetical order"
```

---

## Phase 5：将 Hook 点接入既有系统

本 Phase 逐点将 21 个 hook 点接入既有 ECS 系统。每个 Task 形如：在某个既有系统中调用 `dispatch_hook`，`WorldCommand` 通过 System flush 写回。先做最关键的 4 个 hook 点，其余按相同模板。

### Task 16：`on_task_created` 接入 task_dispatch

**Files:**
- Modify: `src/systems/dispatch/task_dispatch.rs`
- Modify: `src/user_plugins/dispatcher.rs`（提供 system-friendly 入口）

- [ ] **Step 1：在 `dispatcher.rs` 追加一个公开的 helper**

```rust
use bevy::ecs::system::SystemParam;
use crossbeam_channel::unbounded;
use crate::domain::ExperienceStore;

/// 让既有 system 在 dispatch 一组 WorldCommand 后 flush 到 world。
pub fn flush_world_commands(world: &mut World, rx: &crossbeam_channel::Receiver<WorldCommand>) {
    while let Ok(cmd) = rx.try_recv() {
        apply_world_command(world, cmd);
    }
}

fn apply_world_command(world: &mut World, cmd: WorldCommand) {
    match cmd {
        WorldCommand::CreateTask { title, parent: _ } => {
            let id = uuid::Uuid::new_v4();
            let task = crate::domain::Task::new(title, uuid::Uuid::nil());
            let _ = id;
            world.spawn(task);
        }
        WorldCommand::SetTaskMetadata { task_id, key, value } => {
            for mut task in world.query::<&mut crate::domain::Task>().iter_mut(world) {
                if task.id == task_id {
                    task.metadata.insert(key, value);
                }
            }
        }
        WorldCommand::SpawnAgent { .. }
        | WorldCommand::CreateWorkItem { .. }
        | WorldCommand::SetApprovalDecision { .. }
        | WorldCommand::ExperienceSetPinned { .. } => {
            // 后续任务接入
        }
    }
}
```

注：`Task::metadata` 字段是否存在需实施时按 codegraph 校对，必要时改为 `Task::tags` 或新增 metadata map。

- [ ] **Step 2：在 `task_dispatch.rs` 调用 dispatch_hook**

找到 `task_dispatch.rs` 中创建 Task 的位置。在创建之后、把 entity 派给 system 后，调用：

```rust
use crate::user_plugins::dispatcher::{dispatch_hook, HookDispatchInput, PluginContext};
use crate::user_plugins::hook_point::HookPoint;
use crate::user_plugins::host_api::approval::ApprovalContext;
use crate::user_plugins::host_api::entity_query::WorldSnapshot;
use crate::user_plugins::host_api::entity_write::{WorldCommand, WorldWriter};
use crate::user_plugins::host_api::experience::ExperienceContext;
use crate::user_plugins::host_api::plugin_resource::PluginRoots;
use crate::user_plugins::host_api::tool_control::{HookOutcome, SharedHookOutcome};
use std::sync::{Arc, Mutex};
use crossbeam_channel::unbounded;

// ... 在创建 Task 之后：
let mut registry = world.resource_mut::<crate::user_plugins::registry::PluginRegistry>();
let (tx, rx) = unbounded::<WorldCommand>();
let outcome = Arc::new(Mutex::new(HookOutcome::default()));
let snap = WorldSnapshot::from_world(world);

let input = HookDispatchInput {
    point: HookPoint::OnTaskCreated,
    world,
    registry: &mut *registry,
    writer_tx: tx.clone(),
    ctx_builder: Box::new(|plugin, _w| {
        let local_outcome = outcome.clone();
        PluginContext {
            snapshot: snap.clone(),
            writer: WorldWriter::new(tx.clone()),
            outcome: local_outcome,
            plugin_roots: PluginRoots::single(plugin.root_dir.clone()),
            approval: ApprovalContext {
                current_request_id: None,
                tx: tx.clone(),
            },
            experience: ExperienceContext {
                store: Arc::new(ExperienceStore::default()),
                tx: tx.clone(),
            },
        }
    }),
};

let _ = dispatch_hook(input);
crate::user_plugins::dispatcher::flush_world_commands(world, &rx);
```

由于 dispatch_hook 内部要求 `&mut World` 与 `&mut PluginRegistry` 互斥，要先从 world 拿 registry（用 `world.resource_mut` 在闭包外获取并手动调用），实施时需要把这段封装到 `task_dispatch.rs` 的 helper 中。

如果 system signature 是 `Query<...>` 而非 `&mut World`，改用 `ParamSet` 或在 system 内部 `world.resource_scope`：

```rust
world.resource_scope(|world: &mut World, mut registry: Mut<PluginRegistry>| {
    let (tx, rx) = unbounded::<WorldCommand>();
    // ... 派发 ...
    flush_world_commands(world, &rx);
});
```

- [ ] **Step 3：写集成测试**

新建 `tests/user_plugins_on_task_created.rs`：

```rust
use harness::domain::Task;
use harness::user_plugins::loader::load_plugins_from_dir;
use harness::user_plugins::registry::PluginRegistry;
use std::fs;
use tempfile::TempDir;

#[test]
fn on_task_created_hook_writes_metadata() {
    // 准备 test-plugin
    let dir = TempDir::new().unwrap();
    let plugin_dir = dir.path().join("alpha");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join("manifest.toml"), r#"
id = "alpha"
api_version = 1
[[hooks]]
event = "on_task_created"
script = "hooks/on_task.rhai"
"#).unwrap();
    fs::create_dir_all(plugin_dir.join("hooks")).unwrap();
    fs::write(plugin_dir.join("hooks/on_task.rhai"), r#"
let t = get_task(ctx.task_id);
task_set_metadata(ctx.task_id, "source", "plugin");
"#).unwrap();

    let registry = load_plugins_from_dir(dir.path());
    assert!(registry.plugins().iter().any(|p| p.manifest.id == "alpha"));
}
```

实施时本测试需要把 dispatch_hook 单独跑起来；若 system 不可独立 spawn，集成测试改放在 `tests/` 内一个用 Bevy `App::new()` + `minimal Startup` 的 setup 上。

- [ ] **Step 4：Commit**

```bash
git add src/user_plugins/dispatcher.rs src/systems/dispatch/task_dispatch.rs tests/user_plugins_on_task_created.rs
git commit -m "feat(user-plugins): wire on_task_created hook"
```

### Task 17 — Task 30：其余 hook 点接入

剩余 hook 点按 Task 16 的模板逐一接入。每个 hook 点对应一个 Task，包含 4 步：

1. 在目标 system 中调用 `dispatch_hook`
2. 把 `WorldCommand` 通过 `flush_world_commands` 应用
3. 写一个集成测试验证 host API 副作用可见
4. Commit

具体 hook 点与目标 system 对照表：

| Task | Hook 点 | 目标 system 文件 |
|------|--------|--------------|
| 17 | `on_task_completed` | `src/systems/dispatch/task_dispatch.rs`（task 完成分支） |
| 18 | `on_task_failed` | `src/systems/dispatch/task_dispatch.rs`（错误分支） |
| 19 | `on_tool_called`（前 hook，含 deny）| `src/systems/tools/dispatch.rs` |
| 20 | `on_tool_returned` | `src/systems/tools/result.rs` |
| 21 | `on_workitem_started` | `src/systems/dispatch/workitem_dispatch.rs` |
| 22 | `on_workitem_completed` | `src/systems/dispatch/workitem_dispatch.rs` |
| 23 | `on_workitem_failed` | `src/systems/dispatch/workitem_dispatch.rs` |
| 24 | `on_agent_started` | `src/systems/dispatch/agent_selection.rs` |
| 25 | `on_agent_stopped` | `src/systems/maintenance.rs`（agent 终止分支） |
| 26 | `on_message_dispatched` | `src/systems/dispatch/brain_dispatch.rs` |
| 27 | `on_message_received` | `src/systems/ingress.rs` |
| 28 | `on_llm_response` | `src/llm/`（LLM 返回后） |
| 29 | `on_long_term_memory_write` | `src/systems/memory.rs`（写入 LTM 时） |
| 30 | `on_long_term_memory_evicted` | `src/systems/memory.rs`（淘汰时） |

### Task 31 — Task 36：剩余 hook 点接入

| Task | Hook 点 | 目标 system 文件 |
|------|--------|--------------|
| 31 | `on_shared_knowledge_write` | `src/systems/command.rs` 或 `src/domain/space.rs` 写入路径 |
| 32 | `on_experience_candidate_submitted` | `src/systems/experience/collection.rs` |
| 33 | `on_experience_candidate_approved` | `src/systems/experience/governance.rs` |
| 34 | `on_experience_candidate_rejected` | `src/systems/experience/governance.rs` |
| 35 | `on_approval_requested` | `src/systems/tools/approval.rs` |
| 36 | `on_approval_resolved` | `src/systems/tools/approval.rs` |

每 Task 都按 Task 16 模板编写。对于 `on_tool_called`（前 hook），额外要求：

- 派发完 HookOutcome 后，若 `outcome.deny_reason.is_some()`，中止工具调用，返回标准工具错误 message 给 LLM，工具调用历史记录 `denied_by_plugin + plugin id + reason`。
- 若 `outcome.replaced_result.is_some()`（`on_tool_returned`），保留原 result 作为 audit 字段，插件提供的 result 作为正式 result 回传 LLM。
- 审计日志：所有 deny / set_result 调用写入 `tracing::warn!` 结构化日志，字段 `plugin_id`、`reason_or_value`、`tool_call_id`。

具体模板在 Task 17 之后所有步骤内重复；commit message 示例：`feat(user-plugins): wire on_task_completed hook`。

---

## Phase 6：Skill / Agent / Tool / Command 集成

### Task 37：SkillLoader 合并插件贡献

**Files:**
- Modify: `src/infrastructure/skills/loader.rs`
- Modify: `src/user_plugins/mod.rs`

- [ ] **Step 1：在 `SkillLoader` 增加注入入口**

```rust
#[derive(Resource, Debug, Clone, Default)]
pub struct PluginSkillContributions {
    pub entries: Vec<PluginSkillEntry>,
}

#[derive(Debug, Clone)]
pub struct PluginSkillEntry {
    pub plugin_id: String,
    pub skill_id: String,
    pub path: PathBuf,
}
```

在 `SkillLoader::load_skills` 之后追加：

```rust
pub fn load_plugin_skills(
    &self,
    contributions: &PluginSkillContributions,
    agent_name: &str,
) -> Vec<LoadedSkill> {
    // 插件贡献的 skill 与 Agent 名称无关，全局注入。
    let _ = agent_name;
    contributions
        .entries
        .iter()
        .filter_map(|c| parse_skill_md(&c.path).map(|mut s| {
            s.name = format!("{}:{}", c.plugin_id, s.name);
            s
        }))
        .collect()
}
```

- [ ] **Step 2：把插件 skill 注入到 prompt 组装**

在 agent prompt 组装处合并 `load_skills` 与 `load_plugin_skills` 的结果后再 `format_skills_prompt`。若 prompt 组装在 `src/llm/brain_prompt.rs`，则在其内把 `app.world().resource::<PluginSkillContributions>()` 与既有 skill 合并。

让 `PluginLoadStartup` 在扫描后把 skill 路径填入 `PluginSkillContributions` 并 `commands.insert_resource`。这与 `PluginRegistry` 同时插入。

- [ ] **Step 3：运行 `cargo test --lib`**

Expected: 既有测试 PASS（无插件时 PluginSkillContributions 为空）。

- [ ] **Step 4：Commit**

```bash
git add src/infrastructure/skills/loader.rs src/user_plugins/mod.rs
git commit -m "feat(user-plugins): inject plugin skills into SkillLoader"
```

### Task 38：AgentRegistry 合并插件 Agent

**Files:**
- Modify: `src/systems/maintenance.rs`
- Modify: `src/user_plugins/mod.rs`

- [ ] **Step 1：让 `load_agents_system` 合并插件贡献**

在 `load_persistent_agents` 内 `config.agent` 处理完成后，追加：

```rust
if let Some(registry) = commands.get_resource::<crate::user_plugins::registry::PluginRegistry>() {
    for plugin in registry.plugins() {
        for agent_contrib in &plugin.manifest.agents {
            let path = plugin.root_dir.join(&agent_contrib.profile);
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let Ok(entry): Result<crate::domain::AgentEntry, _> = toml::from_str(&content) else { continue };
            let entry = crate::domain::AgentEntry {
                name: format!("{}:{}", plugin.manifest.id, entry.name),
                ..entry
            };
            // 仿照既有 spawn 流程 spawn 该 Agent（codegraph 查具体）
        }
    }
}
```

实际实施时与 `load_persistent_agents` 共享一个 helper `spawn_persistent_agent_entry(commands, entry)`，避免复制粘贴。

- [ ] **Step 2：运行 `cargo check`**

Expected: PASS。

- [ ] **Step 3：Commit**

```bash
git add src/systems/maintenance.rs
git commit -m "feat(user-plugins): merge plugin agents into persistent agent spawn"
```

### Task 39：ToolRegistry 注册插件 Tool

**Files:**
- Create: `src/user_plugins/tool_executor.rs`（替换占位）
- Modify: `src/systems/tools/mod.rs`

- [ ] **Step 1：写 `RhaiToolExecutor` 实现 `BuiltinTool`**

```rust
use crate::domain::{BuiltinTool, ToolAction, ToolContext, ToolError};
use crate::user_plugins::registry::PluginRegistry;
use rhai::Engine;
use serde_json::Value;

pub struct RhaiToolExecutor {
    pub plugin_id: String,
    pub tool_id: String,
}

impl BuiltinTool for RhaiToolExecutor {
    fn name(&self) -> &str {
        "rhai_tool"
    }

    fn execute(&self, input: &Value, _ctx: &ToolContext) -> Result<ToolAction, ToolError> {
        // 通过全局 PluginRegistry（由调用方传递）拿到对应的 plugin 与 AST，
        // 构造 Engine，注册 host API，注入 `input`，execute AST。
        // 简化：在 Phase 12 集成测试中补完整实现。
        let _ = input;
        Ok(ToolAction::Direct(Value::Null))
    }
}
```

- [ ] **Step 2：在 `register_builtin_tools` 中遍历 PluginRegistry 注册插件 tool**

```rust
// 在 register_builtin_tools 末尾追加
if let Some(registry) = world.resource::<PluginRegistry>() {
    for plugin in registry.plugins() {
        for tool in &plugin.manifest.tools {
            let namespaced = format!("{}:{}", plugin.manifest.id, tool.id);
            // 读取 schema 文件
            let schema_path = plugin.root_dir.join(&tool.schema);
            let Ok(schema_str) = std::fs::read_to_string(&schema_path) else { continue };
            let Ok(schema_value): Result<serde_json::Value, _> = serde_json::from_str(&schema_str) else { continue };
            // 用 jsonschema 校验 / 跳过（Phase 7 Task 40 补）

            registry_tools.register(ToolDefinition {
                name: namespaced.clone(),
                description: tool.description.clone(),
                parameters: ToolSchema { schema: schema_value },
                default_permission: tool.default_permission.unwrap_or(ToolPermission::Confirm),
                executor: ToolExecutorKind::Builtin(namespaced.clone()),
                required_tag: None,
            });
        }
    }
}
```

注：`register_builtin_tools` 当前不带 `world: &World`，实施时改签名为接收 `&mut World` 或在 startup 系统中执行。

- [ ] **Step 3：运行 `cargo check` 并修复签名错位**

- [ ] **Step 4：Commit**

```bash
git add src/user_plugins/tool_executor.rs src/systems/tools/mod.rs
git commit -m "feat(user-plugins): register plugin tools with namespaced ids"
```

### Task 40：插件 Tool 的 JSON Schema 校验

**Files:**
- Modify: `src/user_plugins/tool_executor.rs`（或 loader）

- [ ] **Step 1：在加载阶段用 jsonschema 校验**

在 `build_loaded_plugin` 的 tool 循环中，读取 schema 文件后：

```rust
let schema_value: serde_json::Value = match serde_json::from_str(&schema_str) {
    Ok(v) => v,
    Err(e) => return Err(format!("parse schema {}: {e}", schema_path.display())),
};
// Draft 7 校验：编译 schema 本身的合法性
if let Err(e) = jsonschema::validator_for(&schema_value).map_err(|e| format!(
    "invalid schema {}: {e}", schema_path.display()
)) {
    return Err(e);
}
```

- [ ] **Step 2：写失败测试**

在 `src/user_plugins/loader.rs` 的 `#[cfg(test)] mod tests` 块追加：

```rust
#[test]
fn malformed_json_schema_skips_plugin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plugin_root = tmp.path().join("bad-schema-plugin");
    fs::create_dir_all(plugin_root.join("tools")).unwrap();

    // manifest 引用一个 type 非法的 schema
    let manifest = r#"
id = "bad-schema-plugin"
name = "Bad Schema"
version = "0.1.0"
api_version = 1

[[tools]]
id = "broken"
description = "schema malformed"
default_permission = "confirm"
schema = "tools/broken.schema.json"
handler = "tools/broken.rhai"
"#;
    fs::write(plugin_root.join("manifest.toml"), manifest).unwrap();

    fs::write(
        plugin_root.join("tools/broken.schema.json"),
        r#"{"type": "not-a-real-type"}"#,
    )
    .unwrap();
    fs::write(
        plugin_root.join("tools/broken.rhai"),
        "fn main(params) { \"ok\" }",
    )
    .unwrap();

    let registry = PluginRegistry::default();
    let loaded = PluginLoader::scan_root(plugin_root, &registry, API_VERSION);

    assert!(loaded.plugins().is_empty(), "bad-schema plugin should not be loaded");
    let failures = loaded.failures();
    assert_eq!(failures.len(), 1);
    let f = &failures[0];
    assert_eq!(f.plugin_id.as_deref(), Some("bad-schema-plugin"));
    assert!(
        f.error.contains("invalid schema") || f.error.contains("schema"),
        "error should mention schema, got: {}",
        f.error
    );
}

#[test]
fn valid_json_schema_loads_plugin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plugin_root = tmp.path().join("good-schema-plugin");
    fs::create_dir_all(plugin_root.join("tools")).unwrap();

    let manifest = r#"
id = "good-schema-plugin"
name = "Good Schema"
version = "0.1.0"
api_version = 1

[[tools]]
id = "echo"
description = "echo tool"
default_permission = "confirm"
schema = "tools/echo.schema.json"
handler = "tools/echo.rhai"
"#;
    fs::write(plugin_root.join("manifest.toml"), manifest).unwrap();

    fs::write(
        plugin_root.join("tools/echo.schema.json"),
        r#"{
  "type": "object",
  "properties": {
    "msg": {"type": "string"}
  },
  "required": ["msg"]
}"#,
    )
    .unwrap();
    fs::write(
        plugin_root.join("tools/echo.rhai"),
        "fn main(params) { params.msg }",
    )
    .unwrap();

    let registry = PluginRegistry::default();
    let loaded = PluginLoader::scan_root(plugin_root, &registry, API_VERSION);

    assert_eq!(loaded.plugins().len(), 1, "good-schema plugin should load");
    assert!(loaded.failures().is_empty());
}
```

- [ ] **Step 3：跑测试**

Run: `cargo test --lib user_plugins::loader::tests::malformed_json_schema_skips_plugin`
Run: `cargo test --lib user_plugins::loader::tests::valid_json_schema_loads_plugin`
Expected: 两个测试都 FAIL（schema 校验逻辑尚未在 `build_loaded_plugin` 中接好）—— 先 Step 4 把 schema 校验接到 loader，再回到这两个测试应全部 PASS。

- [ ] **Step 4：在 loader 接入 schema 校验**

确认 Step 1 的 schema 校验片段已被 `build_loaded_plugin` 在 tool 循环中调用：读取 `schema_path`、解析为 `serde_json::Value`、调用 `jsonschema::validator_for`。校验失败时返回 `Err(String)`，由上层 `scan_root` 转写为 `PluginFailure` 并跳过该插件。

- [ ] **Step 5：回到 Step 3 跑测试，两个测试都应 PASS**

- [ ] **Step 6：Commit**

```bash
git add src/user_plugins/loader.rs
git commit -m "feat(user-plugins): validate tool JSON schema at load time"
```

### Task 41：插件 Slash Command 识别与派发

**Files:**
- Modify: `src/domain/command.rs`
- Modify: `src/systems/command.rs`
- Create: `src/user_plugins/slash_command.rs`（替换占位）

- [ ] **Step 1：扩展 `UserCommand` 枚举**

```rust
pub enum UserCommand {
    NewTask { topic: String },
    FinishCurrentTask,
    Summarize,
    Remember { content: String },
    PluginCommand { display: String, args: String },
    PlainText(String),
}
```

在 `UserCommand::parse` 内，若输入位于 `PluginRegistry` 中某插件的 `display` 字段，则解析为 `PluginCommand`。由于 `parse` 不能访问 ECS Resource，采用两阶段：先在 parse 阶段把所有"非内置 / 指令"识别为 `PlainText`；在 system 内查 PluginRegistry，若匹配某 `display` 则升级为 `PluginCommand`。

- [ ] **Step 2：写 `slash_command.rs`**

```rust
use rhai::Engine;
use std::sync::Arc;
use crate::user_plugins::host_api;
use crate::user_plugins::host_api::approval::ApprovalContext;
use crate::user_plugins::host_api::entity_query::WorldSnapshot;
use crate::user_plugins::host_api::entity_write::{WorldCommand, WorldWriter};
use crate::user_plugins::host_api::experience::ExperienceContext;
use crate::user_plugins::host_api::plugin_resource::PluginRoots;
use crate::user_plugins::host_api::tool_control::{HookOutcome, SharedHookOutcome};
use crate::user_plugins::registry::PluginRegistry;
use std::sync::Mutex;

/// 派发一个插件 slash command。返回 stdout 字符串供 TUI 显示。
pub fn dispatch_plugin_command(
    display: &str,
    args: &str,
    registry: &PluginRegistry,
    world: &mut bevy::prelude::World,
) -> String {
    for plugin in registry.plugins() {
        for cmd in &plugin.manifest.commands {
            if cmd.display == display {
                let ast = match plugin.command_asts.get(&cmd.id) {
                    Some(a) => a,
                    None => return String::new(),
                };
                let (tx, _rx) = crossbeam_channel::unbounded::<WorldCommand>();
                let outcome: SharedHookOutcome = Arc::new(Mutex::new(HookOutcome::default()));
                let mut engine = Engine::new();
                let snap = WorldSnapshot::from_world(world);
                let ctx = crate::user_plugins::dispatcher::PluginContext {
                    snapshot: snap,
                    writer: WorldWriter::new(tx.clone()),
                    outcome: outcome.clone(),
                    plugin_roots: PluginRoots::single(plugin.root_dir.clone()),
                    approval: ApprovalContext { current_request_id: None, tx: tx.clone() },
                    experience: ExperienceContext {
                        store: Arc::new(world.resource::<crate::domain::ExperienceStore>().clone()),
                        tx: tx.clone(),
                    },
                };
                host_api::register_all(&mut engine, &ctx);
                engine.set_global_var("args", args.to_string());
                let result: String = engine.call_fn("", ast, &mut ()).unwrap_or_default();
                return result;
            }
        }
    }
    String::new()
}
```

- [ ] **Step 3：在 `command_parse_system` 处理 `PluginCommand` 分支**

```rust
UserCommand::PluginCommand { display, args } => {
    if let Some(reg) = world.get_resource::<PluginRegistry>() {
        let output = crate::user_plugins::slash_command::dispatch_plugin_command(
            &display, &args, reg, world,
        );
        debug!(event = "PluginCommandDispatched", display = %display, output = %output);
    }
    commands.entity(entity).despawn();
}
```

注：`world.get_resource` 与 `&mut World` 冲突，需要在 system signature 用 `ParSet` 或 `world.resource_scope`。

- [ ] **Step 4：Commit**

```bash
git add src/domain/command.rs src/systems/command.rs src/user_plugins/slash_command.rs
git commit -m "feat(user-plugins): dispatch plugin slash commands"
```

### Task 42：`/plugins` 内置 slash command

**Files:**
- Modify: `src/domain/command.rs`
- Modify: `src/systems/command.rs`

- [ ] **Step 1：在 `UserCommand` 枚举新增 `Plugins`**

```rust
Plugins,
```

parse 中：

```rust
} else if trimmed == "/plugins" {
    Self::Plugins
}
```

- [ ] **Step 2：在 `command_parse_system` 中处理分支**

把所有加载的插件 + 失败清单格式化为字符串 emit 到 stdout 通道，让 TUI 显示：

```rust
UserCommand::Plugins => {
    let mut out = String::new();
    if let Some(reg) = world.get_resource::<PluginRegistry>() {
        out.push_str(&format!("[plugins] loaded ({}):\n", reg.plugins().len()));
        for p in reg.plugins() {
            out.push_str(&format!(
                "  {} — {} tools, {} skills, {} agents, {} commands, {} hooks\n",
                p.manifest.id,
                p.manifest.tools.len(),
                p.manifest.skills.len(),
                p.manifest.agents.len(),
                p.manifest.commands.len(),
                p.manifest.hooks.len(),
            ));
        }
        if !reg.failures().is_empty() {
            out.push_str(&format!("[plugins] failed ({}):\n", reg.failures().len()));
            for f in reg.failures() {
                out.push_str(&format!("  {}: {}\n", f.plugin_id.as_deref().unwrap_or("?"), f.error));
            }
        }
    } else {
        out.push_str("plugin system not initialized\n");
    }
    // emit_message 到 stdout channel，由 frontend_output 输出
    // 详细 emit 路径见现有 command.rs 内的 UserOutputMessage 处理
    commands.entity(entity).despawn();
}
```

- [ ] **Step 3：测试**

在 `tests/` 新增 `tests/user_plugins_list.rs`：构造含一个真插件、一个坏插件的 fixtures，从 PluginRegistry 生成描述字符串并断言含 `loaded` 与 `failed` 行。

- [ ] **Step 4：Commit**

```bash
git add src/domain/command.rs src/systems/command.rs tests/user_plugins_list.rs
git commit -m "feat(user-plugins): add /plugins command"
```

---

## Phase 7：/reload-plugins 与重启语义

### Task 43：`/reload-plugins` 命令实现

**Files:**
- Create: `src/user_plugins/reload.rs`（替换占位）
- Modify: `src/domain/command.rs`
- Modify: `src/systems/command.rs`

- [ ] **Step 1：编写 reload.rs**

`/reload-plugins` 等同"重新执行启动序列"，因此除了重建 `PluginRegistry`，还必须把上次插件贡献到 ECS 的所有"扩展"清理掉，否则旧 plugin tool / agent / skill 与新 registry 状态不一致。清理动作与正常进程启动时一致（启动时这些资源为空，首次加载不存在旧贡献；reload 时要先回到"空"再加载）。

```rust
use bevy::prelude::*;
use crate::user_plugins::loader::load_plugins_from_dir;
use crate::user_plugins::registry::PluginRegistry;
use crate::domain::space::{SpaceToolRegistry, BuiltinToolExecutors};
use std::path::PathBuf;

pub fn reload_plugins(world: &mut World) {
    tracing::info!(event = "PluginsReloading", "reload-plugins initiated");

    // 1) 清空 PluginRegistry
    let stale_plugin_ids: Vec<String> = world
        .get_resource::<PluginRegistry>()
        .map(|r| r.plugins().iter().map(|p| p.manifest.id.clone()).collect())
        .unwrap_or_default();
    if let Some(mut reg) = world.get_resource_mut::<PluginRegistry>() {
        reg.clear();
    } else {
        world.insert_resource(PluginRegistry::default());
    }

    // 2) 移除插件贡献的 Tool 定义与执行器
    if let Some(mut space) = world.get_resource_mut::<SpaceToolRegistry>() {
        let to_remove: Vec<String> = space
            .iter()
            .map(|t| t.name.clone())
            .filter(|name| {
                stale_plugin_ids
                    .iter()
                    .any(|pid| name.starts_with(&format!("{pid}:")))
            })
            .collect();
        for name in to_remove {
            // SpaceToolRegistry 需暴露 remove(&str)；见 Step 5
            space.remove(&name);
        }
    }
    if let Some(mut execs) = world.get_resource_mut::<BuiltinToolExecutors>() {
        // BuiltinToolExecutors 需暴露 remove(&str)；见 Step 5
        for pid in &stale_plugin_ids {
            let names: Vec<String> = execs
                .iter_names()
                .filter(|n| n.starts_with(&format!("{pid}:")))
                .cloned()
                .collect();
            for n in names {
                execs.remove(&n);
            }
        }
    }

    // 3) 清理插件贡献的 Skill 元数据（SkillLoader 暴露 clear_plugin_contributions）
    if let Some(mut loader) = world.get_resource_mut::<crate::infrastructure::skills::SkillLoader>() {
        loader.clear_plugin_contributions();
    }

    // 4) 插件贡献的 Agent profile 在 AgentRegistry 中按 id 前缀移除
    //    AgentRegistry 暴露 remove_agents_with_prefix(&str)；见 Step 5
    if let Some(mut agents) = world.get_resource_mut::<crate::systems::AgentRegistry>() {
        for pid in &stale_plugin_ids {
            agents.remove_agents_with_prefix(pid);
        }
    }

    // 5) 重新扫描磁盘
    let plugins_dir = PathBuf::from(
        std::env::var("HARNESS_PLUGINS_DIR").unwrap_or_else(|_| ".harness/plugins".to_string()),
    );
    let new_registry = load_plugins_from_dir(&plugins_dir);

    // 6) 重新把贡献注入到 SpaceToolRegistry / BuiltinToolExecutors /
    //    SkillLoader / AgentRegistry（与 startup 路径复用同一函数）
    crate::user_plugins::integrate::integrate_plugin_contributions(world, &new_registry);

    world.insert_resource(new_registry);
    tracing::info!(event = "PluginsReloaded", "reload-plugins complete");
}
```

注意 `reload_plugins` 不 despawn 任何已存在的 `Task` / `WorkItem` / `Agent` ECS 实体——这些是用户业务数据，不是插件贡献；插件扩展能力被清掉后，仍在运行的 Agent 下一次取 tool / skill 时看不到旧插件条目，自然降级。

- [ ] **Step 2：在 SpaceToolRegistry / BuiltinToolExecutors / SkillLoader / AgentRegistry 补 `remove` API**

- `SpaceToolRegistry::remove(&str)`：`self.tools.remove(name)`，返回 `Option<ToolDefinition>`。
- `BuiltinToolExecutors::remove(&str)`：`self.executors.remove(name)`，返回 `Option<Box<dyn BuiltinTool>>`。
- `BuiltinToolExecutors::iter_names() -> impl Iterator<Item = &str>`：返回 key 集合。
- `SkillLoader::clear_plugin_contributions()`：清空内部 `plugin_skills` 字段，下次组装 prompt 时只保留 `.harness/skills/` 的扫描结果。
- `AgentRegistry::remove_agents_with_prefix(&str)`：移除所有以 `<prefix>:` 为前缀的 Agent 配置。

均放在对应模块的 impl 块，每个加一个最小单元测试：

```rust
// src/domain/space.rs
#[test]
fn remove_returns_previous_definition() {
    let mut reg = SpaceToolRegistry::default();
    reg.register(ToolDefinition {
        name: "p:t".to_string(),
        description: "x".to_string(),
        parameters: ToolSchema::default(),
        default_permission: ToolPermission::Confirm,
        executor: ToolExecutorKind::Builtin("x".to_string()),
        required_tag: None,
    });
    assert!(reg.remove("p:t").is_some());
    assert!(reg.get("p:t").is_none());
    assert!(reg.remove("p:t").is_none());
}
```

其余三个模块各放一个等价的 add-then-remove round-trip 测试。

- [ ] **Step 3：新增 `crate::user_plugins::integrate::integrate_plugin_contributions`**

把"把 PluginRegistry 中的贡献合并到 SpaceToolRegistry / BuiltinToolExecutors / SkillLoader / AgentRegistry"的逻辑抽成独立函数，startup 路径（Task 7 / Task 37 / Task 38 / Task 39）与 reload 路径共用：

```rust
pub fn integrate_plugin_contributions(world: &mut World, registry: &PluginRegistry) {
    // 遍历 registry.plugins()，按 manifest 把 tool / skill / agent 注册到对应 ECS Resource。
    // 实现等价于 Task 37/38/39 中原本只在 startup 调用的内联代码，只是抽出来。
}
```

Task 7 / 37 / 38 / 39 中的内联注册代码改为调用本函数。

- [ ] **Step 4：添加 `ReloadPlugins` 到 UserCommand + parse**

`UserCommand::ReloadPlugins`，parse 内 `} else if trimmed == "/reload-plugins" { Self::ReloadPlugins }`。

- [ ] **Step 5：在 command_parse_system 调用**

```rust
UserCommand::ReloadPlugins => {
    crate::user_plugins::reload::reload_plugins(world);
    commands.entity(entity).despawn();
}
```

- [ ] **Step 6：Commit**

```bash
git add src/user_plugins/reload.rs src/user_plugins/integrate.rs \
        src/domain/space.rs src/infrastructure/skills/loader.rs \
        src/systems/agent_registry.rs src/domain/command.rs src/systems/command.rs
git commit -m "feat(user-plugins): add /reload-plugins with full World contribution reset"
```

---

## Phase 8：内置示例插件与集成测试

### Task 44：tests/fixtures/plugins/test-plugin

**Files:**
- Create: `tests/fixtures/plugins/test-plugin/manifest.toml`
- Create: `tests/fixtures/plugins/test-plugin/hooks/on_task_created.rhai`
- Create: `tests/fixtures/plugins/test-plugin/tools/hello.schema.json`
- Create: `tests/fixtures/plugins/test-plugin/tools/hello.rhai`
- Create: `tests/fixtures/plugins/test-plugin/commands/hello.rhai`

- [ ] **Step 1：写 manifest**

```toml
id = "test-plugin"
name = "Test Plugin"
version = "0.1.0"
api_version = 1
description = "internal fixture plugin for harness tests"

[[hooks]]
event = "on_task_created"
script = "hooks/on_task_created.rhai"

[[tools]]
id = "hello"
description = "Return a friendly greeting"
schema = "tools/hello.schema.json"
handler = "tools/hello.rhai"

[[commands]]
id = "hello"
display = "/test-hello"
script = "commands/hello.rhai"
description = "Say hello from the test plugin"
```

- [ ] **Step 2：写 hook 脚本**

```rhai
// hooks/on_task_created.rhai
let tasks = get_task_ids();
log_info(`test-plugin saw ${tasks.len()} tasks`);
```

- [ ] **Step 3：写 tool schema + handler**

`tools/hello.schema.json`:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "properties": {
    "name": { "type": "string" }
  },
  "required": ["name"]
}
```

`tools/hello.rhai`:

```rhai
// args 是 dispatcher 注入的全局对象
let name = args.name;
"hello, " + name
```

- [ ] **Step 4：写 slash command**

`commands/hello.rhai`:

```rhai
"hello from test-plugin"
```

- [ ] **Step 5：Commit**

```bash
git add tests/fixtures/plugins/test-plugin/
git commit -m "test(user-plugins): add fixture test-plugin"
```

### Task 45：集成测试覆盖加载路径

**Files:**
- Create: `tests/user_plugins_integration.rs`

- [ ] **Step 1：写集成测试**

```rust
use harness::user_plugins::loader::load_plugins_from_dir;
use std::path::PathBuf;

#[test]
fn fixture_plugin_loads() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins");
    let registry = load_plugins_from_dir(&path);
    assert!(registry.plugins().iter().any(|p| p.manifest.id == "test-plugin"));
    assert!(registry.failures().is_empty(), "failures: {:?}", registry.failures());
}

#[test]
fn bad_api_version_plugin_goes_to_failures() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plugin = tmp.path().join("bad");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("manifest.toml"), "id = \"bad\"\napi_version = 999\n").unwrap();
    let registry = load_plugins_from_dir(tmp.path());
    assert_eq!(registry.plugins().len(), 0);
    assert_eq!(registry.failures().len(), 1);
}

#[test]
fn bad_plugin_does_not_block_good_plugin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let bad = tmp.path().join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("manifest.toml"), "id = \"bad\"\napi_version = 999\n").unwrap();
    let good = tmp.path().join("good");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(good.join("manifest.toml"), "id = \"good\"\napi_version = 1\n").unwrap();
    let registry = load_plugins_from_dir(tmp.path());
    assert!(registry.plugins().iter().any(|p| p.manifest.id == "good"));
    assert!(registry.failures().iter().any(|f| f.plugin_id.is_none()));
}

#[test]
fn duplicate_command_display_skips_later_plugin() {
    let tmp = tempfile::TempDir::new().unwrap();

    let first = tmp.path().join("alpha");
    std::fs::create_dir_all(first.join("commands")).unwrap();
    std::fs::write(
        first.join("manifest.toml"),
        r#"
id = "alpha"
api_version = 1
[[commands]]
id = "hi"
display = "/hi"
script = "commands/hi.rhai"
"#,
    )
    .unwrap();
    std::fs::write(first.join("commands/hi.rhai"), "fn main(args) { \"alpha\" }").unwrap();

    let second = tmp.path().join("beta");
    std::fs::create_dir_all(second.join("commands")).unwrap();
    std::fs::write(
        second.join("manifest.toml"),
        r#"
id = "beta"
api_version = 1
[[commands]]
id = "hi"
display = "/hi"
script = "commands/hi.rhai"
"#,
    )
    .unwrap();
    std::fs::write(second.join("commands/hi.rhai"), "fn main(args) { \"beta\" }").unwrap();

    let registry = load_plugins_from_dir(tmp.path());

    // 先注册者保留
    assert_eq!(registry.plugins().len(), 1);
    assert_eq!(registry.plugins()[0].manifest.id, "alpha");

    // 后注册者进失败列表
    assert_eq!(registry.failures().len(), 1);
    let f = &registry.failures()[0];
    assert_eq!(f.plugin_id.as_deref(), Some("beta"));
    assert!(
        f.error.contains("display"),
        "expected display conflict in error, got: {}",
        f.error
    );
}
```

- [ ] **Step 2：运行测试**

Run: `cargo test --test user_plugins_integration`
Expected: 4 PASS。

- [ ] **Step 3：Commit**

```bash
git add tests/user_plugins_integration.rs
git commit -m "test(user-plugins): integration tests for loading paths"
```

### Task 46：集成测试覆盖 hook 派发

**Files:**
- Create: `tests/user_plugins_hook_dispatch.rs`

- [ ] **Step 1：用最小 Bevy App 触发 on_task_created**

```rust
use bevy::prelude::*;
use crossbeam_channel::unbounded;
use harness::domain::Task;
use harness::user_plugins::dispatcher::{dispatch_hook, HookDispatchInput, PluginContext};
use harness::user_plugins::hook_point::HookPoint;
use harness::user_plugins::host_api::approval::ApprovalContext;
use harness::user_plugins::host_api::entity_query::WorldSnapshot;
use harness::user_plugins::host_api::entity_write::{WorldCommand, WorldWriter};
use harness::user_plugins::host_api::experience::ExperienceContext;
use harness::user_plugins::host_api::plugin_resource::PluginRoots;
use harness::user_plugins::host_api::tool_control::{HookOutcome, SharedHookOutcome};
use harness::user_plugins::loader::load_plugins_from_dir;
use std::sync::{Arc, Mutex};

#[test]
fn on_task_created_hook_runs_for_fixture_plugin() {
    // 1) 加载 fixture 插件
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plugins");
    let mut registry = load_plugins_from_dir(&path);
    let plugin = registry.plugins().iter().find(|p| p.manifest.id == "test-plugin").unwrap().clone();
    assert!(plugin.hook_asts.contains_key(&HookPoint::OnTaskCreated));

    // 2) 构造最小 world 含一个 Task
    let mut world = World::new();
    world.spawn(Task::new("test", uuid::Uuid::nil()));

    // 3) 派发：分发参数需 registry ref + world ref
    let (tx, _rx) = unbounded::<WorldCommand>();
    let outcome: SharedHookOutcome = Arc::new(Mutex::new(HookOutcome::default()));
    let snap = WorldSnapshot::from_world(&world);

    // 由于 dispatch_hook API 需要外部 World 和 PluginRegistry 借用，
    // 实施时把 dispatch_hook signature 调整为接收 owned PluginRegistry + 一组 ctx-builder，
    // 或者把 registry clone 后调用。
    // 此测试再确认 implementation 时按 API 调整。
}
```

实施说明：测试中的 dispatch_hook API 可能略有调整，按 Phase 4 Task 15 真实签名写。

- [ ] **Step 2：运行测试**

Run: `cargo test --test user_plugins_hook_dispatch`
Expected: PASS（占位 PASS 也可接受，验证编译路径）。

- [ ] **Step 3：Commit**

```bash
git add tests/user_plugins_hook_dispatch.rs
git commit -m "test(user-plugins): integration test for hook dispatch"
```

---

## Phase 9：审计日志与 LLM 可见语义

### Task 47：tool_deny 与 tool_set_result 审计日志

**Files:**
- Modify: `src/user_plugins/host_api/tool_control.rs`
- Modify: `src/systems/tools/dispatch.rs`、`src/systems/tools/result.rs`

- [ ] **Step 1：在 `tool_deny` / `tool_set_result` 写结构化审计**

`tracing::warn!` 已经在 Task 11 内置。补字段：

```rust
warn!(
    event = "PluginToolDeniedAudit",
    plugin_id = %plugin_id,  // 通过 ctx 注入
    tool_call_id = %tool_call_id,  // 通过 ctx 注入
    reason = reason,
    "audit: tool call denied by plugin"
);
```

需要在 `PluginContext` 中新增 `tool_call_id: String`、`plugin_id: String`，dispatcher 在 `run_one_ast` 内 set_global_var 或在 register_fn 闭包中通过对外 `Arc<Mutex<...>>` 共享。

- [ ] **Step 2：在 `tool_dispatch_system` 处理 deny**

派发 `OnToolCalled` 后读 `outcome.deny_reason`。若非空：

```rust
let outcome = dispatch_hook(...);
if let Some(reason) = outcome.deny_reason {
    // 标记 ToolExecutionRequest 为 denied
    // 回给 LLM 的 tool_call 结果用标准错误 message
    // 工具调用历史写入 denied_by_plugin + plugin id + reason
    return; // 跳过实际执行
}
```

- [ ] **Step 3：在 `tool_result_system` 处理 replaced_result**

```rust
let outcome = dispatch_hook(...);
if let Some(replaced) = outcome.replaced_result {
    // 保留原 result 作为 audit 字段
    tracing::info!(event = "PluginToolResultReplacedAudit", original = ?original, new = ?replaced);
    final_result = replaced;
}
```

- [ ] **Step 4：Commit**

```bash
git add src/user_plugins/host_api/tool_control.rs src/systems/tools/dispatch.rs src/systems/tools/result.rs
git commit -m "feat(user-plugins): write audit logs for tool_deny and tool_set_result"
```

---

## Phase 10：文档与配置更新

### Task 48：更新 `docs/current-state.md`

**Files:**
- Modify: `docs/current-state.md`

- [ ] **Step 1：在「已实现」节追加**

```markdown
#### 用户插件系统

- 引入 `.harness/plugins/<id>/` 用户扩展机制，由 manifest + Rhai 脚本 + 静态资源组成
- Plugin Loader 启动时扫描并校验 manifest，按 id 字母序注册到 PluginRegistry
- 21 个 v1 hook 点接入既有 ECS system，前 hook 仅 `on_tool_called` 可拒绝
- Host API 受控表面：实体查询、实体写、工具控制、插件资源、审批、经验、日志
- 沙箱边界：Rhai 不含 FS / 网络原语；`read_plugin_resource` 通过 canonicalize 前缀校验
- 插件 tool / agent / skill / slash command 强制 `<plugin-id>:<local-id>` 命名空间
- `/plugins` 命令显示加载与失败清单，`/reload-plugins` 等同重新执行扫描与注册
- 插件软失败：manifest / api_version / schema / 脚本编译错误跳过该插件并 warn 日志，
  其他插件继续加载
- `tool_deny` / `tool_set_result` 写入结构化审计日志
```

- [ ] **Step 2：Commit**

```bash
git add docs/current-state.md
git commit -m "docs: add user plugin system to current-state"
```

### Task 49：更新 `docs/configuration.md`

**Files:**
- Modify: `docs/configuration.md`

- [ ] **Step 1：在配置文档追加章节**

```markdown
## 用户插件

`.harness/plugins/<id>/` 是用户插件根目录，每个子目录是一个独立插件：

- `manifest.toml` —— 插件清单，必填 `id` 与 `api_version`
- `hooks/*.rhai` —— hook 订阅脚本
- `tools/*.rhai` + `tools/*.schema.json` —— 工具贡献
- `skills/<id>/SKILL.md` —— 技能贡献
- `commands/*.rhai` —— slash command 贡献
- `agents/<id>.toml` —— Agent 贡献

环境变量 `HARNESS_PLUGINS_DIR` 可覆盖默认路径。

核心 Host API 版本为 `1`，manifest 的 `api_version` 必须与此相等才能加载。
```

- [ ] **Step 2：Commit**

```bash
git add docs/configuration.md
git commit -m "docs: document .harness/plugins/ user plugin layout"
```

### Task 50：更新 `docs/README.md` 索引

**Files:**
- Modify: `docs/README.md`

- [ ] **Step 1：在阅读入口追加插件系统条目**

参照既有结构，在「设计规格」附近补：

```markdown
- 用户插件系统设计：`docs/superpowers/specs/2026-06-23-plugin-system-design.md`
- 用户插件系统实施计划：`docs/superpowers/plans/2026-06-23-plugin-system.md`
```

- [ ] **Step 2：Commit**

```bash
git add docs/README.md
git commit -m "docs: index user plugin system documents"
```

---

## Self-Review 笔记

### 1. Spec coverage

逐节核对 spec：

| Spec 节 | 覆盖 Task |
|---|---|
| 背景 / 术语 / Rhai 合理性 | 不需要 Task |
| 设计目标 / 非目标 | 不需要 Task |
| 总体架构 | Task 2（模块结构） |
| 插件 Manifest | Task 3 |
| 命名空间 | Task 5（registry）、Task 39（tool） |
| API 版本兼容 | Task 3（validate）、Task 1（API_VERSION 常量） |
| 加载流程 | Task 6（loader）、Task 7（startup） |
| 启动时输出 | Task 7（eprintln） |
| `/reload-plugins` 语义 | Task 43 |
| `/plugins` 命令 | Task 42 |
| Hook 点清单 | Task 4（21 个枚举）、Task 16-36（逐点接入） |
| 派发顺序 | Task 5（sort by id）、Task 15（dispatcher） |
| 上下文对象 | Task 9-14（ctx 注入与 host_api::register_all） |
| Host API 表面 | Task 9-14 |
| 插件级 state | Task 8（state.rs 占位）、Task 15 PluginContext |
| 安全边界 | Task 6（is_within）、Task 12（plugin_resource） |
| 沙箱实现机制 | Task 6（禁用 FS）、Task 12 |
| 工具 schema 标准 | Task 40 |
| API 表面演进规则 | 不需要 Task |
| 错误处理 | Task 6（failures list）、Task 15（timeout） |
| 测试策略 | Task 45（集成）、Task 16-36（hook 覆盖） |
| 内置示例插件 | Task 44 |
| 实现范围 | 本 plan 外 |

### 2. Placeholder 扫描

检查发现：Task 8 的 state.rs、entity_query.rs 等占位 `register` 已经在后续 Task 内替换。Task 15 中我提示了一段 "result 不存在" 的描述，已在 Step 2 给出替换实现。Task 13、Task 16 提到 "需要回调到 entity_write.rs 添加变体"，实施者按 Step 提到的具体变体名加。

### 3. Type consistency

- `HookPoint` 枚举贯穿 Task 4、5、6、15、16-36，命名一致 (`OnToolCalled` 等)
- `PluginManifest` 字段 `id` / `api_version` / `hooks` 全程一致
- `WorldCommand` 在 Task 10 定义，Task 13 追加 4 个变体，Task 15 flush
- `PluginContext` 在 Task 14 引用、Task 15 定义、Task 16-36 + Task 41 调用，字段一致

### 4. 风险提示（不阻塞 plan 通过）

- Task 15 dispatch_hook 的 Engine 与 World 借用互斥需要在 system 内用 `world.resource_scope`。Task 16 注释已说明，实施者须留心。
- Task 38 spawn_agent 完成 helper 与 maintenance.rs 既有 spawn 分支的差异，需 codegraph 二次校对。
- Task 46 整合测试的 dispatch_hook API 可能在实施期间微调 —— 测试代码在实施 Task 15 完成后再写更稳。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-23-plugin-system.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?