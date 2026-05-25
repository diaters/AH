# Harness TUI 设计文档

日期：2026-05-26
状态：待评审

## 1. 概述

为 AI Harness 添加终端 TUI 界面，基于 ratatui 框架，采用双栏布局 + 底部输入框。通过 `Frontend` trait 实现前端与 ECS 引擎的解耦，支持未来扩展 Telegram、Web 等多通道前端。

### 1.1 设计目标

- 提供沉浸式的终端交互体验：富文本对话、Agent 状态监控、任务可视化、审批交互
- 前端与引擎解耦：加新前端（Telegram、Web）只需实现 trait，ECS 侧零改动
- 消息路由：用户从哪个通道发起，响应就回到哪个通道；支持定向多目标发送和广播

### 1.2 非目标

- 不保留旧 CLI（stdin/stdout）入口，`main.rs` 直接替换为 TUI
- 不做 Web 前端（本次），但预留接口

## 2. 整体架构

```
┌──────────────────────┐                ┌─────────────────────────┐
│  TUI 前端 (ratatui)  │   channel      │  Bevy ECS 引擎          │
│                      │                │                         │
│  ┌────────────────┐  │  UserAction    │  ┌───────────────────┐  │
│  │ 双栏布局       │  │ ──────────────►│  │ input_ingress     │  │
│  │ 左: 对话+审批  │  │                │  │ dispatch          │  │
│  │ 右: 状态面板   │  │  EngineEvent   │  │ execution         │  │
│  │ 底: 输入框     │  │ ◄──────────────│  │ tool              │  │
│  │                │  │                │  │ response          │  │
│  └────────────────┘  │                │  │ frontend_output   │  │
│                      │                │  │ frontend_input    │  │
│  ratatui + crossterm │                │  └───────────────────┘  │
└──────────────────────┘                └─────────────────────────┘
```

核心原则：TUI 是 ECS 引擎的一个 `Frontend` 实现，通过 channel 通信。ECS 侧只依赖 `Frontend` trait，不依赖任何 UI 细节。

## 3. 协议设计

### 3.1 通道标识

```rust
/// 前端类型
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum FrontendKind {
    Tui,
    Telegram,
    Web,
}

/// 标识一个前端通道中的具体用户
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ChannelId {
    pub frontend: FrontendKind,
    /// 该前端内的用户标识（TUI 通常只有一个，Telegram 用 chat_id）
    pub user_id: String,
}
```

### 3.2 事件路由目标

```rust
/// 事件路由目标
#[derive(Debug, Clone)]
pub enum EventTarget {
    /// 广播：所有前端的所有用户
    Broadcast,
    /// 定向：发送给指定的一个或多个通道用户
    Directed(Vec<ChannelId>),
}
```

`Directed` 包含 1 个元素为单发，N 个为多发，`Broadcast` 为全发。

路由场景示例：

| 场景 | target 值 | 效果 |
|------|----------|------|
| TUI 用户发消息，Agent 回复 | `Directed([ChannelId{Tui,"default"}])` | 只发给 TUI |
| 系统通知 Agent 状态变化 | `Broadcast` | 所有前端所有用户 |
| 管理员同时在 TUI 和 Telegram | `Directed([ChannelId{Tui,"a"}, ChannelId{Telegram,"chat_123"}])` | 两个通道都收到 |
| 子任务结果汇总给发起者 | `Directed([origin_channel])` | 谁发起回谁 |

### 3.3 引擎 → 前端事件

```rust
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// 用户可见文本（Agent 回复、系统消息）
    Text {
        target: EventTarget,
        role: MessageRole,
        content: String,
    },
    /// 审批请求
    ApprovalRequest {
        target: EventTarget,
        request_id: Uuid,
        agent_name: String,
        tool_name: String,
        tool_input: serde_json::Value,
        options: Vec<ApprovalOption>,
    },
    /// 审批结果
    ApprovalResult {
        target: EventTarget,
        request_id: Uuid,
        decision: String,
    },
    /// Agent 状态变化
    AgentStatusChanged {
        target: EventTarget,
        agent_id: AgentId,
        name: String,
        status: AgentStatusKind,
    },
    /// Task 状态变化
    TaskStatusChanged {
        target: EventTarget,
        task_id: TaskId,
        name: String,
        status: TaskStatusKind,
        result: Option<String>,
    },
    /// 子任务批次进度
    BatchProgress {
        target: EventTarget,
        batch_id: Uuid,
        completed: usize,
        total: usize,
    },
}
```

默认路由规则：
- 用户触发的回复（Text、ApprovalRequest、ApprovalResult、TaskStatusChanged、BatchProgress）→ `Directed([origin_channel])`
- Agent 状态变化 → `Broadcast`
- 特殊场景（如管理员多通道监控）→ `Directed([多个 ChannelId])`

### 3.4 前端 → 引擎动作

```rust
#[derive(Debug, Clone)]
pub enum UserAction {
    /// 用户发送文本消息
    Text {
        channel: ChannelId,
        content: String,
    },
    /// 用户响应审批请求
    Confirmation {
        channel: ChannelId,
        request_id: Uuid,
        option_id: String,
    },
}
```

### 3.5 审批选项

```rust
#[derive(Debug, Clone)]
pub struct ApprovalOption {
    pub id: String,       // "allow_once", "allow_always", "deny"
    pub label: String,    // "Allow Once", "Always Allow", "Deny"
    pub description: String, // "仅本次允许", "永久允许此工具", "拒绝执行"
}
```

## 4. Frontend Trait

```rust
/// 前端 trait — 引擎只依赖这个接口
pub trait Frontend: Send + Sync + 'static {
    /// 该前端负责的 frontend 类型
    fn kind(&self) -> FrontendKind;

    /// 推送引擎事件，前端自行过滤：
    /// - EventTarget::Broadcast → 处理
    /// - EventTarget::Directed 中包含本前端的 ChannelId → 处理
    /// - 其他 → 忽略
    fn push_event(&self, event: EngineEvent);

    /// 拉取待处理的用户动作（ECS 每帧调用）
    fn poll_actions(&self) -> Vec<UserAction>;
}
```

### 4.1 TUI Frontend 实现

```rust
pub struct TuiFrontend {
    user_id: String,
    event_tx: Sender<EngineEvent>,
    action_rx: Receiver<UserAction>,
}

impl TuiFrontend {
    pub fn new(event_tx: Sender<EngineEvent>, action_rx: Receiver<UserAction>) -> Self {
        Self {
            user_id: "default".to_string(),
            event_tx,
            action_rx,
        }
    }

    fn my_channels(&self) -> Vec<ChannelId> {
        vec![ChannelId { frontend: FrontendKind::Tui, user_id: self.user_id.clone() }]
    }
}

impl Frontend for TuiFrontend {
    fn kind(&self) -> FrontendKind { FrontendKind::Tui }

    fn push_event(&self, event: EngineEvent) {
        let my_channels = self.my_channels();
        let for_me = match event.target() {
            EventTarget::Broadcast => true,
            EventTarget::Directed(targets) => {
                targets.iter().any(|t| my_channels.contains(t))
            }
        };
        if for_me {
            let _ = self.event_tx.send(event);
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        let mut actions = Vec::new();
        while let Ok(action) = self.action_rx.try_recv() {
            actions.push(action);
        }
        actions
    }
}
```

### 4.2 未来 Telegram Frontend 示例（不在本次实现范围）

```rust
struct TelegramFrontend {
    api: TelegramApi,
    pending: Mutex<Vec<UserAction>>,
}

impl Frontend for TelegramFrontend {
    fn kind(&self) -> FrontendKind { FrontendKind::Telegram }

    fn push_event(&self, event: &EngineEvent) {
        match event.target() {
            EventTarget::Broadcast => { /* 发给所有活跃 Telegram 用户 */ }
            EventTarget::Directed(targets) => {
                for target in targets {
                    if target.frontend == FrontendKind::Telegram {
                        self.send_to_user(&target.user_id, event);
                    }
                }
            }
        }
    }

    fn poll_actions(&self) -> Vec<UserAction> {
        let mut pending = self.pending.lock().unwrap();
        std::mem::take(&mut *pending)
    }
}
```

## 5. ECS 侧集成

### 5.1 FrontendRegistry

```rust
#[derive(Resource)]
pub struct FrontendRegistry {
    pub frontends: Vec<Box<dyn Frontend>>,
}
```

### 5.2 新增 System

#### frontend_output_system

将 ECS 状态变化转为 `EngineEvent` 推送给所有前端：

```rust
fn frontend_output_system(
    tasks: Query<&Task, Changed<Task>>,
    agents: Query<&Agent, Changed<Agent>>,
    // 审批相关事件读取
    registry: Res<FrontendRegistry>,
) {
    // Task 状态变化 → 定向发给任务发起者
    for task in &tasks {
        let target = EventTarget::Directed(vec![task.origin_channel.clone()]);
        let event = EngineEvent::TaskStatusChanged {
            target,
            task_id: task.id,
            name: task.input_summary.clone(),
            status: task.status.kind(),
            result: if task.status.is_done() { Some(task.result_summary.clone()) } else { None },
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }

    // Agent 状态变化 → 广播
    for agent in &agents {
        let event = EngineEvent::AgentStatusChanged {
            target: EventTarget::Broadcast,
            agent_id: agent.id,
            name: agent.profile.name.clone(),
            status: agent.status_kind(),
        };
        for frontend in &registry.frontends {
            frontend.push_event(event.clone());
        }
    }

    // 审批请求/结果 → 定向发给任务发起者
    // ...
}
```

#### frontend_input_system

从前端拉取用户动作，转换为 `ExternalInput` 注入 ECS：

```rust
fn frontend_input_system(
    registry: Res<FrontendRegistry>,
    mut input_writer: EventWriter<ExternalInput>,
) {
    for frontend in &registry.frontends {
        for action in frontend.poll_actions() {
            match action {
                UserAction::Text { channel, content } => {
                    input_writer.send(ExternalInput::TextWithChannel { channel, content });
                }
                UserAction::Confirmation { channel, request_id, option_id } => {
                    input_writer.send(ExternalInput::Confirmation { request_id, option: option_id });
                }
            }
        }
    }
}
```

### 5.3 Task 新增 origin_channel

```rust
#[derive(Component)]
pub struct Task {
    // ... 现有字段
    pub origin_channel: ChannelId,
}
```

`Task::from_user_input_ready` 签名增加 `channel: ChannelId` 参数。

### 5.4 ExternalInput 扩展

```rust
pub enum ExternalInput {
    TextWithChannel { channel: ChannelId, content: String },
    Confirmation { request_id: Uuid, option: String },
    Shutdown,
}
```

移除旧的 `Text` 变体，统一使用 `TextWithChannel`。

### 5.5 变更文件清单

| 文件 | 变更 |
|------|------|
| `src/domain/mod.rs` | 新增 `EngineEvent`、`EventTarget`、`ChannelId`、`FrontendKind`、`UserAction`、`ApprovalOption`、`Frontend` trait；`Task` 增加 `origin_channel`；`ExternalInput` 改为 `TextWithChannel` |
| `src/app/mod.rs` | 新增 `FrontendRegistry` Resource；注册 `frontend_output_system`、`frontend_input_system` |
| `src/systems/transform.rs` | `user_output_system` 产出 `EngineEvent`（替代或并行 `OutputMessage`） |
| `src/systems/tool.rs` | 审批流程产出 `EngineEvent::ApprovalRequest` / `ApprovalResult` |
| `src/main.rs` | 替换为 TUI 入口（ratatui 初始化 + 事件循环） |

## 6. TUI 前端设计

### 6.1 布局

```
┌─────────────────────────────────────────────────────────┐
│ Harness                                          [q]uit │
├──────────────────────────────────┬──────────────────────┤
│                                  │ Agents               │
│ You:                             │ ● brain       idle   │
│ 请创建3个子任务...                │ ● default-llm run..  │
│                                  │ ◆ 兔子计算    run..  │
│ default-llm:                     │ ◆ 苍蝇计算    wait   │
│ 已创建子任务批次...              │ ● summarizer  idle   │
│                                  ├──────────────────────┤
│ ┌─ ⚡ spawn_agent ────────────┐ │ Tasks                │
│ │ from default-llm-agent     │ │ ● Parent Task  Run   │
│ │ Create child "兔子繁衍计算" │ │   ├ 兔子 ✓          │
│ │                            │ │   ├ 苍蝇 ⏳          │
│ │ › Allow Once  仅本次允许   │ │   └ 分析 ⏸          │
│ │   Always Allow 永久允许    │ ├──────────────────────┤
│ │   Deny       拒绝执行      │ │ ⚡ Approvals    [1]  │
│ │                            │ │ 1 pending            │
│ │        ↑↓ 选择 · Enter 确认│ │                      │
│ └────────────────────────────┘ │                      │
├──────────────────────────────────┴──────────────────────┤
│ ❯ 输入消息...                                          │
└─────────────────────────────────────────────────────────┘
```

### 6.2 组件

| 组件 | 职责 |
|------|------|
| `ChatPanel` | 左栏主区域，渲染对话消息流和审批卡片 |
| `StatusPanel` | 右栏上部分，显示 Agent 列表 + Task 树 |
| `ApprovalBadge` | 右栏底部，审批队列指示（待处理数量徽章） |
| `InputBar` | 底部输入框 |
| `App` | 顶层布局和事件分发，持有以上组件及状态 |

### 6.3 交互模式

```rust
enum AppMode {
    /// 正常输入模式：输入文本消息
    Chat,
    /// 审批选择模式：↑↓ 选择审批选项，Enter 确认
    Approval {
        request_id: Uuid,
        selected_index: usize,
        options: Vec<ApprovalOption>,
    },
}
```

模式切换规则：
- 收到 `EngineEvent::ApprovalRequest` 且没有活跃审批 → 进入 `Approval` 模式
- 审批确认/拒绝 → 回到 `Chat` 模式（如还有排队审批则进入下一个）
- `Escape` → 取消审批选择回到 `Chat`（审批仍挂着，通过右栏徽章可见）

### 6.4 审批卡片状态

对话区中审批卡片有三种样式：

| 状态 | 样式 | 说明 |
|------|------|------|
| Active | 黄色边框，选项列表可 ↑↓ 选择 | 当前需要用户处理 |
| Queued | 灰色边框，文字提示"排队中" | 等待前面的审批处理完 |
| Done | 绿色折叠行 `✓ create_tasks 已批准 (Always)` | 已处理完毕 |

多个审批并发时，同一时间只有一个 Active，其余为 Queued。

### 6.5 Markdown 渲染

Agent 回复中的 Markdown 使用 `termimad` crate 渲染，支持：
- 标题、粗体、斜体
- 代码块（语法高亮）
- 表格
- 列表

### 6.6 App 状态

```rust
struct App {
    mode: AppMode,
    messages: Vec<ChatMessage>,
    agents: Vec<AgentState>,
    tasks: Vec<TaskState>,
    pending_approvals: Vec<PendingApproval>,
    input_buffer: String,
    cursor_position: usize,
    scroll_offset: u16,
}

enum ChatMessage {
    User(String),
    Agent { name: String, content: String },
    ApprovalCard(ApprovalCardState),
    ApprovalDone { tool_name: String, decision: String },
}

enum ApprovalCardState {
    Active {
        request_id: Uuid,
        agent_name: String,
        tool_name: String,
        tool_input: String,
        options: Vec<ApprovalOption>,
        selected_index: usize,
    },
    Queued {
        tool_name: String,
    },
}
```

### 6.7 主循环

```rust
fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    let _log_guard = init_tracing();

    let runtime = Arc::new(Runtime::new()?);
    let config = HarnessConfig::from_env()?;
    let executor = create_executor_from_config(&config.llm)?;

    // 创建 Frontend channel
    let (event_tx, event_rx) = unbounded::<EngineEvent>();
    let (action_tx, action_rx) = unbounded::<UserAction>();

    let tui_frontend = TuiFrontend::new(event_tx, action_rx);

    // 构建 ECS app
    let mut app = build_harness_app_with_frontend(config, runtime, executor, tui_frontend);

    // 启动 ratatui
    let mut terminal = ratatui::init();
    let mut app_state = App::new();

    loop {
        // 1. 处理 crossterm 键盘事件
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                app_state.handle_key_event(key, &action_tx);
            }
        }

        // 2. 从 channel 拉取 EngineEvent，更新 TUI 状态
        while let Ok(event) = event_rx.try_recv() {
            app_state.handle_engine_event(event);
        }

        // 3. 驱动 ECS
        app.update();
        if app.world().resource::<ShutdownState>().requested && app_is_idle(app.world_mut()) {
            break;
        }

        // 4. 渲染 TUI
        terminal.draw(|frame| app_state.render(frame))?;

        thread::sleep(Duration::from_millis(16));
    }

    ratatui::restore();
    Ok(())
}
```

### 6.8 快捷键

| 按键 | Chat 模式 | Approval 模式 |
|------|----------|---------------|
| `Enter` | 发送消息 | 确认当前选中选项 |
| `↑` / `↓` | 滚动对话区 | 切换审批选项 |
| `Esc` | — | 退出审批选择 |
| `q` / `Ctrl+C` | 退出程序 | 退出程序 |
| 字符键 | 输入到输入框 | — |

## 7. 依赖新增

| Crate | 用途 | 许可证 |
|-------|------|--------|
| `ratatui` | TUI 框架 | MIT |
| `crossterm` | 终端抽象（ratatui 依赖） | MIT |
| `termimad` | Markdown 渲染 | MIT |

均满足项目依赖原则（crates.io、MIT/Apache-2.0、纯 Rust）。

## 8. 测试策略

- `TestFrontend`：内存 channel 实现 `Frontend` trait，用于集成测试验证 `EngineEvent` 产出和路由
- `App` 状态单元测试：`handle_engine_event` 正确更新消息列表、Agent 状态、审批队列
- 审批流程集成测试：验证多审批并发的 Queued → Active → Done 状态流转
- 路由测试：验证 `EventTarget::Directed` 只发给目标前端，`Broadcast` 发给所有前端
