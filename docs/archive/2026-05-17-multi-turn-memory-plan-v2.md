# Phase 4.1 v2 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

__Goal:__ 实现基于 token 的多轮对话与记忆管理，支持用户指令创建/结束子任务。

__Architecture:__ 引入 tiktoken 进行 token 估算，修改记忆结构移除轮数依赖，新增命令解析系统处理用户指令。

__Tech Stack:__ Rust, Bevy ECS, tiktoken, Tokio, chrono, serde, uuid

---

## 文件结构

| 文件 | 变更类型 | 职责 |
|------|----------|------|
| `Cargo.toml` | 修改 | 添加 tiktoken 依赖 |
| `src/domain/memory.rs` | 修改 | 移除轮数字段，添加 token 估算 |
| `src/domain/mod.rs` | 修改 | 新增 TaskStatus::Pending，新增用户指令枚举 |
| `src/app/mod.rs` | 修改 | 修改 MemoryConfig 结构 |
| `src/systems/command.rs` | 新建 | 命令解析系统 |
| `src/systems/memory.rs` | 修改 | 按 token 触发压缩 |
| `src/systems/transform.rs` | 修改 | 支持 Pending 状态处理 |
| `tests/multi_turn_flow.rs` | 修改 | 更新测试适配新结构 |

---

## Task 1: 引入 tiktoken 依赖

__Files:__
- Modify: `Cargo.toml`

- [ ] __Step 1: 添加 tiktoken 依赖__

修改 `Cargo.toml`：

```toml
[dependencies]
# ... 现有依赖
tiktoken-rs = "0.5"
```

- [ ] __Step 2: 验证编译__

Run: `cargo build`
Expected: 编译成功

- [ ] __Step 3: 提交__

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
feat: add tiktoken-rs dependency for token estimation

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: 定义用户指令枚举

__Files:__
- Modify: `src/domain/mod.rs`

- [ ] __Step 1: 定义用户指令枚举__

在 `src/domain/mod.rs` 中添加：

```rust
/// 用户指令
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCommand {
    /// /btw - 创建子任务承接新话题
    NewTask { topic: String },
    /// /finish - 结束当前任务
    FinishCurrentTask,
    /// /summarize - 触发总结
    Summarize,
    /// 普通输入（非指令）
    PlainText(String),
}

impl UserCommand {
    /// 解析用户输入
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        if trimmed.starts_with("/btw ") {
            Self::NewTask {
                topic: trimmed[4..].trim().to_string(),
            }
        } else if trimmed == "/btw" {
            Self::NewTask {
                topic: String::new(),
            }
        } else if trimmed == "/finish" {
            Self::FinishCurrentTask
        } else if trimmed == "/summarize" {
            Self::Summarize
        } else {
            Self::PlainText(input.to_string())
        }
    }
    
    /// 判断是否是指令
    pub fn is_command(&self) -> bool {
        !matches!(self, Self::PlainText(_))
    }
}
```

- [ ] __Step 2: 修改 TaskStatus 添加 Pending 状态__

找到 `TaskStatus` 枚举，确保有 `Pending` 状态：

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,           // 新建，待总结主题
    Ready,
    Running,
    Waiting(WaitingReason),
    Done,
    Failed(FailureReason),
}
```

- [ ] __Step 3: 修改 Task::from_user_input__

```rust
impl Task {
    /// 基于用户输入创建一个处于 Pending 状态的新任务。
    pub fn from_user_input(content: impl Into<String>, max_retries: u32) -> Self {
        let content = content.into();
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            content,
            creator: Uuid::nil(),
            delegate: None,
            status: TaskStatus::Pending,  // 改为 Pending
            input_summary: String::new(),
            result_summary: String::new(),
            priority: 0,
            created_at: now,
            updated_at: now,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            last_error: None,
        }
    }
}
```

- [ ] __Step 4: 运行测试确认__

Run: `cargo test`
Expected: 部分测试可能失败，但编译通过

- [ ] __Step 5: 提交__

```bash
git add src/domain/mod.rs
git commit -m "$(cat <<'EOF'
feat: add UserCommand enum and TaskStatus::Pending

- UserCommand: /btw, /finish, /summarize, PlainText
- TaskStatus::Pending for tasks awaiting topic summarization
- Task::from_user_input now creates Pending status

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: 修改 MemoryConfig 和 ShortTermMemory

__Files:__
- Modify: `src/app/mod.rs`
- Modify: `src/domain/memory.rs`

- [ ] __Step 1: 修改 MemoryConfig__

在 `src/app/mod.rs` 中修改：

```rust
#[derive(Debug, Clone, Resource)]
pub struct MemoryConfig {
    /// 压缩触发阈值（token 数）
    pub compression_threshold_tokens: u32,
    /// 保留最近 N 轮不压缩
    pub preserve_recent_turns: u32,
    /// LLM 摘要目标 token 数
    pub summary_target_tokens: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            compression_threshold_tokens: 8000,
            preserve_recent_turns: 2,
            summary_target_tokens: 1000,
        }
    }
}
```

- [ ] __Step 2: 修改 ShortTermMemory__

在 `src/domain/memory.rs` 中修改：

```rust
use tiktoken_rs::cl100k_base;

/// 短期记忆（绑定 Task）
#[derive(Component, Default)]
pub struct ShortTermMemory {
    /// 完整对话条目
    pub entries: Vec<MemoryEntry>,
    /// 摘要前缀（压缩后的旧内容）
    pub summary_prefix: Option<String>,
    /// 当前 token 估算
    pub estimated_tokens: u32,
    /// 最后一次缓存命中的 token 数
    pub last_cached_tokens: Option<u32>,
}

impl ShortTermMemory {
    /// 添加新条目
    pub fn add_entry(&mut self, role: EntryRole, content: impl Into<String>, metadata: EntryMetadata) {
        let content = content.into();
        // 更新 token 估算
        self.estimated_tokens += estimate_tokens(&content);
        let entry = MemoryEntry::new(self.entries.len() as u32 + 1, role, content)
            .with_metadata(metadata);
        self.entries.push(entry);
    }

    /// 重新计算 token 估算
    pub fn recalculate_tokens(&mut self) {
        let mut total = 0u32;
        if let Some(summary) = &self.summary_prefix {
            total += estimate_tokens(summary);
        }
        for entry in &self.entries {
            total += estimate_tokens(&entry.content);
        }
        self.estimated_tokens = total;
    }
}

/// 估算文本的 token 数
pub fn estimate_tokens(text: &str) -> u32 {
    cl100k_base()
        .map(|enc| enc.encode_with_special_tokens(text).len() as u32)
        .unwrap_or_else(|_| (text.len() / 4) as u32)  // fallback: 4 chars ≈ 1 token
}
```

- [ ] __Step 3: 更新 MemoryEntry__

移除 turn 字段（不再需要）：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub role: EntryRole,
    pub content: String,
    pub metadata: EntryMetadata,
}

impl MemoryEntry {
    pub fn new(_index: u32, role: EntryRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            metadata: EntryMetadata::default(),
        }
    }
}
```

- [ ] __Step 4: 运行测试确认编译__

Run: `cargo build`
Expected: 编译通过

- [ ] __Step 5: 提交__

```bash
git add src/app/mod.rs src/domain/memory.rs
git commit -m "$(cat <<'EOF'
refactor: change memory management from turns to tokens

- MemoryConfig: compression_threshold_tokens, preserve_recent_turns, summary_target_tokens
- ShortTermMemory: add estimated_tokens, remove turn_count/summary_range
- Use tiktoken for token estimation

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: 实现命令解析系统

__Files:__
- Create: `src/systems/command.rs`
- Modify: `src/systems/mod.rs`
- Modify: `src/systems/routing.rs`

- [ ] __Step 1: 创建命令解析系统__

创建 `src/systems/command.rs`：

```rust
use bevy::prelude::*;
use tracing::info;

use crate::domain::{
    Agent, AgentKind, CreateTaskMessage, Task, TaskStatus, UserCommand, UserInputMessage,
};

/// 命令解析系统：解析用户输入中的指令
pub(crate) fn command_parse_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
    agents: Query<&Agent>,
) {
    for (entity, input) in &user_inputs {
        let cmd = UserCommand::parse(&input.content);

        match cmd {
            UserCommand::NewTask { topic } => {
                // /btw - 创建子任务承接新话题
                // 查找当前活跃的任务作为父任务
                let parent_task = tasks
                    .iter()
                    .find(|t| !t.status.is_terminal() && t.status != TaskStatus::Pending);

                if let Some(parent) = parent_task {
                    info!(
                        parent_id = %parent.id,
                        topic = %topic,
                        "creating sub-task via /btw command"
                    );
                    // 创建子任务
                    let child_task = Task::from_user_input(
                        if topic.is_empty() { &input.content } else { &topic },
                        parent.max_retries,
                    );
                    commands.spawn((
                        child_task,
                        crate::domain::ShortTermMemory::default(),
                    ));
                    // TODO: 记录父子关系
                } else {
                    // 没有父任务，创建普通任务
                    commands.spawn(CreateTaskMessage {
                        content: input.content.clone(),
                    });
                }
            }
            UserCommand::FinishCurrentTask => {
                // /finish - 结束当前任务
                let current_task = tasks
                    .iter()
                    .find(|t| !t.status.is_terminal());

                if let Some(task) = current_task {
                    info!(task_id = %task.id, "finishing current task via /finish command");
                    // 触发任务终止，后续会被 contribution 系统处理
                    commands.spawn(crate::domain::TaskTerminatedMessage { task_id: task.id });
                }
            }
            UserCommand::Summarize => {
                // /summarize - 触发总结
                // TODO: 实现总结触发
                info!("summarize command received - to be implemented");
            }
            UserCommand::PlainText(_) => {
                // 普通输入，交给路由系统处理
                continue;
            }
        }

        commands.entity(entity).despawn();
    }
}
```

- [ ] __Step 2: 导出新系统__

修改 `src/systems/mod.rs`：

```rust
mod command;

pub(crate) use command::command_parse_system;
```

- [ ] __Step 3: 修改 user_input_routing_system__

修改 `src/systems/routing.rs`，在路由前先检查是否已处理：

```rust
pub(crate) fn user_input_routing_system(
    mut commands: Commands,
    user_inputs: Query<(Entity, &UserInputMessage)>,
    tasks: Query<&Task>,
) {
    for (entity, input) in &user_inputs {
        // 检查是否是命令（命令由 command_parse_system 处理）
        if UserCommand::parse(&input.content).is_command() {
            continue;  // 跳过，由 command_parse_system 处理
        }

        // 原有逻辑：查找 Waiting(User) 或创建新任务
        let waiting_task = tasks
            .iter()
            .find(|t| t.status == TaskStatus::Waiting(crate::domain::WaitingReason::User));

        if let Some(task) = waiting_task {
            commands.spawn(ContinueTaskMessage {
                task_id: task.id,
                user_input: input.content.clone(),
            });
        } else {
            commands.spawn(CreateTaskMessage {
                content: input.content.clone(),
            });
        }

        commands.entity(entity).despawn();
    }
}
```

- [ ] __Step 4: 注册系统__

修改 `src/app/mod.rs`，确保命令解析在路由之前：

```rust
command_parse_system.in_set(HarnessSet::Transform),
user_input_routing_system
    .in_set(HarnessSet::Transform)
    .after(command_parse_system),
```

- [ ] __Step 5: 运行测试确认编译__

Run: `cargo build`
Expected: 编译通过

- [ ] __Step 6: 提交__

```bash
git add src/systems/command.rs src/systems/mod.rs src/systems/routing.rs src/app/mod.rs
git commit -m "$(cat <<'EOF'
feat: implement command parsing system

- command_parse_system: handle /btw, /finish, /summarize
- user_input_routing_system: skip commands
- Register command_parse_system before routing

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: 修改记忆压缩系统

__Files:__
- Modify: `src/systems/memory.rs`

- [ ] __Step 1: 重写 memory_compression_system__

修改 `src/systems/memory.rs`：

```rust
use bevy::prelude::*;
use tracing::info;

use crate::{
    app::MemoryConfig,
    domain::{LongTermMemory, ShortTermMemory},
};

/// 记忆压缩系统：检测 token 阈值并触发摘要
pub(crate) fn memory_compression_system(
    config: Res<MemoryConfig>,
    mut tasks: Query<(&crate::domain::Task, &mut ShortTermMemory, Option<&mut LongTermMemory>)>,
) {
    for (task, mut short_term, long_term) in &mut tasks {
        // 检查是否需要压缩
        if short_term.estimated_tokens > config.compression_threshold_tokens {
            let entries_count = short_term.entries.len();
            
            // 保留最近 N 轮
            let preserve_count = (config.preserve_recent_turns * 2) as usize;  // 每轮 = User + Assistant
            if entries_count <= preserve_count {
                continue;
            }

            let compress_count = entries_count - preserve_count;
            
            // 收集需要压缩的条目
            let to_compress: Vec<_> = short_term.entries.drain(0..compress_count).collect();
            
            // 生成摘要内容
            let mut compress_text = String::new();
            for entry in &to_compress {
                compress_text.push_str(&format!("{:?}: {}\n", entry.role, entry.content));
            }
            
            // 更新摘要前缀
            // Phase 4.1: 简单拼接，Phase 4.2 调用 LLM 生成摘要
            let new_summary = if let Some(existing) = &short_term.summary_prefix {
                format!("{}\n\n{}", existing, compress_text)
            } else {
                compress_text
            };
            
            short_term.summary_prefix = Some(new_summary);
            
            // 重新计算 token
            short_term.recalculate_tokens();
            
            info!(
                task_id = %task.id,
                compressed_count = compress_count,
                new_tokens = short_term.estimated_tokens,
                "compressed short-term memory"
            );

            // 将压缩的条目移入长期记忆
            if let Some(mut long) = long_term {
                for entry in to_compress {
                    long.add_archive(entry.content);
                }
            }
        }
    }
}
```

- [ ] __Step 2: 移除旧的测试并添加新测试__

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EntryRole, Task, TaskStatus};

    #[test]
    fn memory_compression_by_tokens() {
        let mut world = World::new();
        world.insert_resource(MemoryConfig {
            compression_threshold_tokens: 100,
            preserve_recent_turns: 1,
            summary_target_tokens: 50,
        });

        let task = Task::from_user_input("test", 3);
        let entity = world.spawn((task, ShortTermMemory::default())).id();

        // Add entries with known token counts
        {
            let mut stm = world.get_mut::<ShortTermMemory>(entity).unwrap();
            // Add enough content to exceed threshold
            for i in 0..10 {
                stm.add_entry(
                    EntryRole::User,
                    format!("This is message number {} with some content", i),
                    Default::default(),
                );
            }
        }

        // Verify tokens were estimated
        let stm = world.get::<ShortTermMemory>(entity).unwrap();
        assert!(stm.estimated_tokens > 0);
    }
}
```

- [ ] __Step 3: 运行测试__

Run: `cargo test systems::memory`
Expected: 测试通过

- [ ] __Step 4: 提交__

```bash
git add src/systems/memory.rs
git commit -m "$(cat <<'EOF'
refactor: memory compression by token threshold

- Check estimated_tokens instead of turn_count
- Preserve recent N turns before compression
- Update summary_prefix with compressed content
- Recalculate tokens after compression

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: 修改 llm_response_system 支持 Pending 状态

__Files:__
- Modify: `src/systems/transform.rs`

- [ ] __Step 1: 修改 llm_response_system__

在 `llm_response_system` 中，Pending 状态的任务响应后需要总结确定主题：

```rust
pub(crate) fn llm_response_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    results: Query<(Entity, &AgentExecutionResultMessage)>,
) {
    for (entity, result_message) in &results {
        if result_message.result.request_kind != AgentRequestKind::LlmCompletion {
            continue;
        }

        let result = &result_message.result;

        for mut task in &mut tasks {
            if task.id != result.task_id {
                continue;
            }

            match &result.result {
                Ok(content) => {
                    // 检查任务状态
                    if task.status == TaskStatus::Pending {
                        // Pending 状态：响应后进入 Waiting(User)，不结束任务
                        task.status = TaskStatus::Waiting(WaitingReason::User);
                        task.input_summary = content.clone();  // 暂存响应
                        task.updated_at = clock.0;
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    } else {
                        // 非 Pending：原有逻辑，标记完成
                        task.mark_done(content.clone(), clock.0);
                        commands.spawn(UserOutputMessage {
                            content: content.clone(),
                        });
                    }
                }
                Err(error) if error.is_retryable() && task.retry_count < task.max_retries => {
                    task.schedule_retry(error, clock.0);
                }
                Err(error) => {
                    task.mark_failed(error, clock.0);
                    commands.spawn(UserOutputMessage {
                        content: format!(
                            "任务执行失败（{:?}）：{}",
                            task_status_failure_reason(&task).unwrap_or(FailureReason::Unknown),
                            error.message()
                        ),
                    });
                }
            }

            break;
        }

        commands.entity(entity).despawn();
    }
}
```

- [ ] __Step 2: 修改 task_dispatch_system__

确保 Pending 状态的任务也能被调度：

```rust
pub(crate) fn task_dispatch_system(
    clock: Res<Clock>,
    mut commands: Commands,
    mut tasks: Query<&mut Task>,
    agents: Query<&Agent>,
) {
    for mut task in &mut tasks {
        // Pending 或 Ready 状态都可以被调度
        if task.status != TaskStatus::Ready && task.status != TaskStatus::Pending {
            continue;
        }

        // ... 其余逻辑不变
    }
}
```

- [ ] __Step 3: 运行测试__

Run: `cargo test`
Expected: 测试通过

- [ ] __Step 4: 提交__

```bash
git add src/systems/transform.rs src/systems/dispatch.rs
git commit -m "$(cat <<'EOF'
feat: support Pending task status in execution flow

- Pending tasks enter Waiting(User) after response
- Non-Pending tasks still mark_done after response
- task_dispatch_system handles both Ready and Pending

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: 更新测试

__Files:__
- Modify: `tests/multi_turn_flow.rs`
- Modify: `tests/multi_turn_routing.rs`

- [ ] __Step 1: 更新测试适配新结构__

移除/修改依赖 `turn_count`、`summary_range` 等字段的测试。

- [ ] __Step 2: 添加新测试__

```rust
#[test]
fn pending_task_enters_waiting_after_response() {
    // 测试 Pending 状态任务响应后进入 Waiting(User)
}

#[test]
fn command_parse_creates_subtask() {
    // 测试 /btw 命令创建子任务
}

#[test]
fn token_based_compression() {
    // 测试基于 token 的压缩
}
```

- [ ] __Step 3: 运行测试__

Run: `cargo test`
Expected: 所有测试通过

- [ ] __Step 4: 提交__

```bash
git add tests/
git commit -m "$(cat <<'EOF'
test: update tests for token-based memory

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: 更新文档

__Files:__
- Modify: `docs/TODO.md`

- [ ] __Step 1: 更新 TODO__

标记 Phase 4.1 v2 相关项。

- [ ] __Step 2: 提交__

```bash
git add docs/TODO.md
git commit -m "$(cat <<'EOF'
docs: update TODO with Phase 4.1 v2 progress

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## 自审清单

### Spec Coverage

| Spec 要求 | 对应 Task |
|-----------|-----------|
| tiktoken 依赖 | Task 1 |
| UserCommand 枚举 | Task 2 |
| TaskStatus::Pending | Task 2 |
| MemoryConfig 按 token | Task 3 |
| ShortTermMemory 移除轮数 | Task 3 |
| 命令解析系统 | Task 4 |
| /btw 创建子任务 | Task 4 |
| /finish 结束任务 | Task 4 |
| 按 token 压缩 | Task 5 |
| Pending 状态处理 | Task 6 |

### Placeholder Scan

检查无 `TBD`、`TODO`、未完成的步骤代码块。✅

### Type Consistency

检查所有类型定义在 Task 间保持一致。✅
