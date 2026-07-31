use crossbeam_channel::Sender;
use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};
use uuid::Uuid;

use crate::domain::{
    AgentStatusKind, ApprovalOption, ChannelId, EngineEvent, FrontendKind, MessageRole,
    TaskStatusKind, UserAction,
};

use super::chat::{ApprovalCardState, ChatMessage, ChatPanel};
use super::input::InputBar;
use super::status::StatusPanel;

/// 交互模式
#[derive(Debug)]
pub enum AppMode {
    /// 正常输入模式
    Chat,
    /// 审批选择模式
    Approval {
        request_id: Uuid,
        selected_index: usize,
        options: Vec<ApprovalOption>,
    },
    /// 拒绝并反馈输入模式：用户选择 "reject_with_feedback" 后输入反馈文本
    Feedback {
        request_id: Uuid,
        feedback_buffer: String,
        cursor_position: usize,
    },
}

/// Agent 前端状态
#[derive(Debug, Clone)]
pub struct AgentState {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: AgentStatusKind,
}

/// Task 前端状态
#[derive(Debug, Clone)]
pub struct TaskState {
    pub id: uuid::Uuid,
    pub name: String,
    pub status: TaskStatusKind,
    pub result: Option<String>,
    pub parent_id: Option<uuid::Uuid>,
    pub subtask_count: u32,
    pub completed_count: u32,
    /// 任务来源的前端通道
    pub origin_channel: Option<ChannelId>,
    /// 任务进入终态的时刻
    pub completed_at: Option<std::time::Instant>,
    /// 被指派 agent 的名称，无 delegate 时为 None
    pub agent_name: Option<String>,
    /// 等待原因，仅当 status 为 Waiting 时有意义
    pub waiting_reason: Option<crate::domain::WaitingReasonKind>,
}

/// 待处理审批
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: Uuid,
    pub agent_name: String,
    pub tool_name: String,
    pub tool_input: String,
    pub options: Vec<ApprovalOption>,
}

/// TUI App 顶层状态
pub struct App {
    pub mode: AppMode,
    pub messages: Vec<ChatMessage>,
    pub agents: Vec<AgentState>,
    pub tasks: Vec<TaskState>,
    pub pending_approvals: Vec<PendingApproval>,
    pub input_buffer: String,
    pub cursor_position: usize,
    pub scroll_offset: u16,
    pub should_quit: bool,
    action_tx: Sender<UserAction>,
}

impl App {
    pub fn new(action_tx: Sender<UserAction>) -> Self {
        Self {
            mode: AppMode::Chat,
            messages: Vec::new(),
            agents: Vec::new(),
            tasks: Vec::new(),
            pending_approvals: Vec::new(),
            input_buffer: String::new(),
            cursor_position: 0,
            scroll_offset: 0,
            should_quit: false,
            action_tx,
        }
    }

    /// 处理键盘事件
    pub fn handle_key_event(&mut self, key: KeyEvent) {
        match &self.mode {
            AppMode::Chat => self.handle_chat_key(key),
            AppMode::Approval {
                request_id,
                selected_index,
                options,
            } => {
                self.handle_approval_key(key, *request_id, *selected_index, options.clone());
            }
            AppMode::Feedback {
                request_id,
                feedback_buffer,
                cursor_position,
            } => {
                self.handle_feedback_key(
                    key,
                    *request_id,
                    feedback_buffer.clone(),
                    *cursor_position,
                );
            }
        }
    }

    /// 处理粘贴事件（IME 输入提交的中文等多字节文本）
    pub fn handle_paste(&mut self, text: &str) {
        match &mut self.mode {
            AppMode::Chat => {
                self.input_buffer.insert_str(self.byte_index(), text);
                self.cursor_position += text.chars().count();
            }
            AppMode::Feedback {
                feedback_buffer,
                cursor_position,
                ..
            } => {
                let bi = feedback_byte_index(feedback_buffer, *cursor_position);
                feedback_buffer.insert_str(bi, text);
                *cursor_position += text.chars().count();
            }
            _ => {}
        }
    }

    /// 处理鼠标事件（滚轮翻页）
    pub fn handle_mouse_event(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp if self.scroll_offset > 0 => {
                self.scroll_offset -= 1;
            }
            MouseEventKind::ScrollDown => {
                self.scroll_offset += 1;
            }
            _ => {}
        }
    }

    /// 将消息列表中匹配的 Queued 审批卡片提升为 Active
    fn promote_queued_card(&mut self, pending: &PendingApproval) {
        for msg in &mut self.messages {
            if let ChatMessage::ApprovalCard(ApprovalCardState::Queued { tool_name }) = msg
                && tool_name == &pending.tool_name
            {
                *msg = ChatMessage::ApprovalCard(ApprovalCardState::Active {
                    request_id: pending.request_id,
                    agent_name: pending.agent_name.clone(),
                    tool_name: pending.tool_name.clone(),
                    tool_input: pending.tool_input.clone(),
                    options: pending.options.clone(),
                    selected_index: 0,
                });
                break;
            }
        }
    }

    /// 将 char 索引转为 byte 索引
    fn byte_index(&self) -> usize {
        self.input_buffer
            .char_indices()
            .nth(self.cursor_position)
            .map(|(i, _)| i)
            .unwrap_or(self.input_buffer.len())
    }

    /// 处理 Feedback 模式按键
    fn handle_feedback_key(
        &mut self,
        key: KeyEvent,
        request_id: Uuid,
        mut feedback_buffer: String,
        mut cursor_position: usize,
    ) {
        match key.code {
            KeyCode::Char('q')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                let feedback = feedback_buffer.clone();
                self.send_feedback_confirmation(request_id, feedback);
            }
            KeyCode::Esc => {
                // 取消反馈，回到审批选项列表
                self.restore_approval_mode(request_id);
            }
            KeyCode::Char(c) => {
                let bi = feedback_byte_index(&feedback_buffer, cursor_position);
                feedback_buffer.insert(bi, c);
                cursor_position += 1;
                self.mode = AppMode::Feedback {
                    request_id,
                    feedback_buffer,
                    cursor_position,
                };
            }
            KeyCode::Backspace if cursor_position > 0 => {
                cursor_position -= 1;
                let bi = feedback_byte_index(&feedback_buffer, cursor_position);
                if let Some((char_byte_len, _)) = feedback_buffer[bi..].char_indices().nth(1) {
                    feedback_buffer.drain(bi..bi + char_byte_len);
                } else {
                    feedback_buffer.drain(bi..);
                }
                self.mode = AppMode::Feedback {
                    request_id,
                    feedback_buffer,
                    cursor_position,
                };
            }
            KeyCode::Left if cursor_position > 0 => {
                cursor_position -= 1;
                self.mode = AppMode::Feedback {
                    request_id,
                    feedback_buffer,
                    cursor_position,
                };
            }
            KeyCode::Right if cursor_position < feedback_buffer.chars().count() => {
                cursor_position += 1;
                self.mode = AppMode::Feedback {
                    request_id,
                    feedback_buffer,
                    cursor_position,
                };
            }
            _ => {}
        }
    }

    /// 发送 reject_with_feedback 确认
    fn send_feedback_confirmation(&mut self, request_id: Uuid, feedback: String) {
        let channel = ChannelId {
            frontend: FrontendKind::Tui,
            user_id: "default".to_string(),
            thread_id: None,
        };
        let _ = self.action_tx.send(UserAction::Confirmation {
            channel,
            request_id,
            option_id: "reject_with_feedback".to_string(),
            feedback: Some(feedback),
        });

        // 更新消息列表中的审批卡片状态
        for msg in &mut self.messages {
            if let ChatMessage::ApprovalCard(state) = msg
                && state.is_active_for(request_id)
            {
                state.mark_done("拒绝并反馈".to_string());
            }
        }

        // 移除已处理的审批
        self.pending_approvals
            .retain(|a| a.request_id != request_id);

        // 切换到下一个审批或回到 Chat 模式
        let next = self.pending_approvals.first().cloned();
        if let Some(next) = next {
            self.mode = AppMode::Approval {
                request_id: next.request_id,
                selected_index: 0,
                options: next.options.clone(),
            };
            self.promote_queued_card(&next);
        } else {
            self.mode = AppMode::Chat;
        }
    }

    /// 取消反馈，回到审批选项列表
    fn restore_approval_mode(&mut self, request_id: Uuid) {
        if let Some(pending) = self
            .pending_approvals
            .iter()
            .find(|a| a.request_id == request_id)
            .cloned()
        {
            self.mode = AppMode::Approval {
                request_id,
                selected_index: 0,
                options: pending.options,
            };
        } else {
            self.mode = AppMode::Chat;
        }
    }

    /// 更新所有主任务的子任务进度
    fn update_all_subtask_progress(&mut self) {
        use std::collections::HashMap;

        // Build parent -> children map in O(n)
        let mut parent_to_children: HashMap<Uuid, Vec<_>> = HashMap::new();
        for task in &self.tasks {
            if let Some(parent_id) = task.parent_id {
                parent_to_children
                    .entry(parent_id)
                    .or_default()
                    .push((task.id, task.status));
            }
        }

        // Update all main tasks in O(n)
        for task in self.tasks.iter_mut() {
            if task.parent_id.is_none() {
                let children = parent_to_children
                    .get(&task.id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                task.subtask_count = children.len() as u32;
                task.completed_count = children
                    .iter()
                    .filter(|(_, status)| {
                        matches!(status, TaskStatusKind::Done | TaskStatusKind::Failed)
                    })
                    .count() as u32;
            }
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) {
        use crossterm::event::KeyModifiers;
        match key.code {
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Tab if !self.pending_approvals.is_empty() => {
                let first = self.pending_approvals.first().cloned();
                if let Some(pending) = first {
                    self.mode = AppMode::Approval {
                        request_id: pending.request_id,
                        selected_index: 0,
                        options: pending.options.clone(),
                    };
                    self.promote_queued_card(&pending);
                }
            }
            KeyCode::Enter => {
                let content = self.input_buffer.clone();
                if !content.is_empty() {
                    let channel = ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                        thread_id: None,
                    };
                    let _ = self.action_tx.send(UserAction::Text {
                        channel,
                        content: content.clone(),
                    });
                    self.messages.push(ChatMessage::User(content));
                    self.input_buffer.clear();
                    self.cursor_position = 0;
                }
            }
            KeyCode::Char(c) => {
                self.input_buffer.insert(self.byte_index(), c);
                self.cursor_position += 1;
            }
            KeyCode::Backspace if self.cursor_position > 0 => {
                self.cursor_position -= 1;
                let bi = self.byte_index();
                // 删除 cursor 位置处的一整个字符
                if let Some((char_byte_len, _)) = self.input_buffer[bi..].char_indices().nth(1) {
                    self.input_buffer.drain(bi..bi + char_byte_len);
                } else {
                    // 删除最后一个字符
                    self.input_buffer.drain(bi..);
                }
            }
            KeyCode::Delete if self.cursor_position < self.input_buffer.chars().count() => {
                let bi = self.byte_index();
                // 找到下一个字符的 byte 边界
                if let Some((next_byte, _)) = self.input_buffer[bi..].char_indices().nth(1) {
                    self.input_buffer.drain(bi..bi + next_byte);
                } else if bi < self.input_buffer.len() {
                    // 删除最后一个字符
                    self.input_buffer.drain(bi..);
                }
            }
            KeyCode::Left if self.cursor_position > 0 => {
                self.cursor_position -= 1;
            }
            KeyCode::Right if self.cursor_position < self.input_buffer.chars().count() => {
                self.cursor_position += 1;
            }
            KeyCode::Up if self.scroll_offset > 0 => {
                self.scroll_offset -= 1;
            }
            KeyCode::Down => {
                self.scroll_offset += 1;
            }
            _ => {}
        }
    }

    fn handle_approval_key(
        &mut self,
        key: KeyEvent,
        request_id: Uuid,
        selected_index: usize,
        options: Vec<ApprovalOption>,
    ) {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Up if selected_index > 0 => {
                let new_index = selected_index - 1;
                self.mode = AppMode::Approval {
                    request_id,
                    selected_index: new_index,
                    options,
                };
                // 同步更新对应 ApprovalCard 的 selected_index
                for msg in &mut self.messages {
                    if let ChatMessage::ApprovalCard(ApprovalCardState::Active {
                        request_id: rid,
                        selected_index: si,
                        ..
                    }) = msg
                        && *rid == request_id
                    {
                        *si = new_index;
                    }
                }
            }
            KeyCode::Down if selected_index < options.len() - 1 => {
                let new_index = selected_index + 1;
                self.mode = AppMode::Approval {
                    request_id,
                    selected_index: new_index,
                    options,
                };
                // 同步更新对应 ApprovalCard 的 selected_index
                for msg in &mut self.messages {
                    if let ChatMessage::ApprovalCard(ApprovalCardState::Active {
                        request_id: rid,
                        selected_index: si,
                        ..
                    }) = msg
                        && *rid == request_id
                    {
                        *si = new_index;
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(option) = options.get(selected_index) {
                    // 检测 reject_with_feedback 选项：切换到 Feedback 模式而非直接发送
                    if option.id == "reject_with_feedback" {
                        self.mode = AppMode::Feedback {
                            request_id,
                            feedback_buffer: String::new(),
                            cursor_position: 0,
                        };
                        return;
                    }

                    let channel = ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                        thread_id: None,
                    };
                    let _ = self.action_tx.send(UserAction::Confirmation {
                        channel,
                        request_id,
                        option_id: option.id.clone(),
                        feedback: None,
                    });

                    // 更新消息列表中的审批卡片状态
                    for msg in &mut self.messages {
                        if let ChatMessage::ApprovalCard(state) = msg
                            && state.is_active_for(request_id)
                        {
                            state.mark_done(option.label.clone());
                        }
                    }

                    // 移除已处理的审批
                    self.pending_approvals
                        .retain(|a| a.request_id != request_id);

                    // 切换到下一个审批或回到 Chat 模式
                    let next = self.pending_approvals.first().cloned();
                    if let Some(next) = next {
                        self.mode = AppMode::Approval {
                            request_id: next.request_id,
                            selected_index: 0,
                            options: next.options.clone(),
                        };
                        self.promote_queued_card(&next);
                    } else {
                        self.mode = AppMode::Chat;
                    }
                }
            }
            KeyCode::Esc => {
                // 将当前审批移到队列末尾，激活下一个
                if self.pending_approvals.len() > 1 {
                    let current_id = request_id;
                    // 把当前审批移到末尾
                    if let Some(idx) = self
                        .pending_approvals
                        .iter()
                        .position(|a| a.request_id == current_id)
                    {
                        let deferred = self.pending_approvals.remove(idx);
                        self.pending_approvals.push(deferred);
                    }
                    let next = self.pending_approvals.first().cloned();
                    if let Some(next) = next {
                        self.mode = AppMode::Approval {
                            request_id: next.request_id,
                            selected_index: 0,
                            options: next.options.clone(),
                        };
                        self.promote_queued_card(&next);
                    }
                } else {
                    self.mode = AppMode::Chat;
                }
            }
            _ => {}
        }
    }

    /// 处理引擎事件，更新 TUI 状态
    pub fn handle_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Text { role, content, .. } => {
                let msg = match role {
                    MessageRole::User => ChatMessage::User(content),
                    MessageRole::Agent => ChatMessage::Agent {
                        name: "Agent".to_string(),
                        content,
                    },
                    MessageRole::System => ChatMessage::System(content),
                };
                self.messages.push(msg);
            }
            EngineEvent::ApprovalRequest {
                request_id,
                agent_name,
                tool_name,
                tool_input,
                options,
                ..
            } => {
                let tool_input_str = serde_json::to_string_pretty(&tool_input)
                    .unwrap_or_else(|_| tool_input.to_string());

                let pending = PendingApproval {
                    request_id,
                    agent_name: agent_name.clone(),
                    tool_name: tool_name.clone(),
                    tool_input: tool_input_str.clone(),
                    options: options.clone(),
                };

                let is_first =
                    matches!(self.mode, AppMode::Chat) && self.pending_approvals.is_empty();

                self.pending_approvals.push(pending);

                if is_first {
                    self.mode = AppMode::Approval {
                        request_id,
                        selected_index: 0,
                        options,
                    };
                    self.messages
                        .push(ChatMessage::ApprovalCard(ApprovalCardState::Active {
                            request_id,
                            agent_name,
                            tool_name,
                            tool_input: tool_input_str,
                            options: self
                                .pending_approvals
                                .last()
                                .map(|p| p.options.clone())
                                .unwrap_or_default(),
                            selected_index: 0,
                        }));
                } else {
                    self.messages
                        .push(ChatMessage::ApprovalCard(ApprovalCardState::Queued {
                            tool_name,
                        }));
                }
            }
            EngineEvent::ApprovalResult {
                request_id,
                decision,
                ..
            } => {
                for msg in &mut self.messages {
                    if let ChatMessage::ApprovalCard(state) = msg
                        && state.is_active_for(request_id)
                    {
                        state.mark_done(decision.clone());
                    }
                }
                self.pending_approvals
                    .retain(|a| a.request_id != request_id);
            }
            EngineEvent::AgentStatusChanged {
                agent_id,
                name,
                status,
                ..
            } => {
                if let Some(agent) = self.agents.iter_mut().find(|a| a.id == agent_id) {
                    agent.status = status;
                } else {
                    self.agents.push(AgentState {
                        id: agent_id,
                        name,
                        status,
                    });
                }
            }
            EngineEvent::TaskStatusChanged {
                task_id,
                name,
                status,
                result,
                parent_id,
                origin_channel,
                agent_name,
                waiting_reason,
                ..
            } => {
                let completed_at =
                    if matches!(status, TaskStatusKind::Done | TaskStatusKind::Failed) {
                        Some(std::time::Instant::now())
                    } else {
                        None
                    };
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = status;
                    task.result = result;
                    task.parent_id = parent_id;
                    if task.origin_channel.is_none() {
                        task.origin_channel = origin_channel;
                    }
                    if completed_at.is_some() {
                        task.completed_at = completed_at;
                    }
                    task.agent_name = agent_name;
                    task.waiting_reason = waiting_reason;
                } else {
                    self.tasks.push(TaskState {
                        id: task_id,
                        name,
                        status,
                        result,
                        parent_id,
                        subtask_count: 0,
                        completed_count: 0,
                        origin_channel,
                        completed_at,
                        agent_name,
                        waiting_reason,
                    });
                }

                // 更新子任务进度
                self.update_all_subtask_progress();
            }
            EngineEvent::BatchProgress { .. } => {}
            EngineEvent::ToolCallStarted {
                task_id,
                agent_name,
                tool_name,
                tool_input_summary,
                ..
            } => {
                let short_id = task_id
                    .to_string()
                    .split('-')
                    .next()
                    .unwrap_or("????")
                    .to_string();
                let content = if tool_input_summary.is_empty() {
                    format!("[{}] 🔧 {} 调用 {}", short_id, agent_name, tool_name)
                } else {
                    format!(
                        "[{}] 🔧 {} 调用 {}: {}",
                        short_id, agent_name, tool_name, tool_input_summary
                    )
                };
                self.messages.push(ChatMessage::System(content));
            }
        }
    }

    /// 清理已超过 5 秒的终态任务及其子任务
    pub fn cleanup_completed_tasks(&mut self) {
        const CLEANUP_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
        let now = std::time::Instant::now();

        // 找出需要清理的主任务 ID
        let expired_main_ids: Vec<Uuid> = self
            .tasks
            .iter()
            .filter(|t| {
                t.parent_id.is_none()
                    && t.completed_at
                        .is_some_and(|at| now.duration_since(at) > CLEANUP_DELAY)
            })
            .map(|t| t.id)
            .collect();

        if expired_main_ids.is_empty() {
            return;
        }

        // 移除过期主任务及其子任务
        self.tasks.retain(|t| {
            !expired_main_ids.contains(&t.id)
                && !t
                    .parent_id
                    .is_some_and(|pid| expired_main_ids.contains(&pid))
        });
    }

    /// 渲染 TUI
    pub fn render(&mut self, frame: &mut Frame) {
        self.cleanup_completed_tasks();
        let area = frame.area();

        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(area);

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(48)])
            .split(main_layout[0]);

        ChatPanel::render(self, frame, content_layout[0]);
        StatusPanel::render(self, frame, content_layout[1]);
        InputBar::render(self, frame, main_layout[1]);
    }
}

/// 将 char 索引转为 byte 索引（Feedback 模式专用，不依赖 App 实例）
fn feedback_byte_index(buffer: &str, cursor_position: usize) -> usize {
    buffer
        .char_indices()
        .nth(cursor_position)
        .map(|(i, _)| i)
        .unwrap_or(buffer.len())
}

#[cfg(test)]
mod tests {
    use crossbeam_channel::unbounded;
    use crossterm::event::{KeyCode, KeyEvent};
    use uuid::Uuid;

    use crate::domain::{
        AgentStatusKind, ApprovalOption, ChannelId, EngineEvent, EventTarget, FrontendKind,
        MessageRole, TaskStatusKind, WaitingReasonKind,
    };
    use crate::tui::chat::{ApprovalCardState, ChatMessage};

    use super::{App, AppMode};

    fn test_app() -> App {
        let (action_tx, _) = unbounded();
        App::new(action_tx)
    }

    #[test]
    fn handle_text_event_adds_agent_message() {
        let mut app = test_app();
        app.handle_engine_event(EngineEvent::Text {
            target: EventTarget::Broadcast,
            role: MessageRole::Agent,
            content: "hello world".to_string(),
            task_id: None,
        });
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(
            &app.messages[0],
            ChatMessage::Agent { content, .. } if content == "hello world"
        ));
    }

    #[test]
    fn handle_approval_request_enters_approval_mode() {
        let mut app = test_app();
        let request_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id,
            agent_name: "test-agent".to_string(),
            tool_name: "create_tasks".to_string(),
            tool_input: serde_json::json!({"tasks": []}),
            options: vec![ApprovalOption {
                id: "allow_once".to_string(),
                label: "Allow Once".to_string(),
                description: "仅本次允许".to_string(),
            }],
            approval_context: None,
        });
        assert!(matches!(app.mode, AppMode::Approval { .. }));
        assert_eq!(app.pending_approvals.len(), 1);
    }

    #[test]
    fn handle_agent_status_adds_agent() {
        let mut app = test_app();
        let agent_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::AgentStatusChanged {
            target: EventTarget::Broadcast,
            agent_id,
            name: "brain".to_string(),
            status: AgentStatusKind::Idle,
        });
        assert_eq!(app.agents.len(), 1);
        assert_eq!(app.agents[0].name, "brain");
    }

    #[test]
    fn handle_task_status_adds_task() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "test task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].name, "test task");
        assert_eq!(app.tasks[0].parent_id, None);
        assert_eq!(app.tasks[0].subtask_count, 0);
        assert_eq!(app.tasks[0].completed_count, 0);
    }

    #[test]
    fn subtask_progress_calculated_correctly() {
        let mut app = test_app();
        let main_id = Uuid::new_v4();
        let sub1_id = Uuid::new_v4();
        let sub2_id = Uuid::new_v4();

        // 添加主任务
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: main_id,
            name: "main task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 添加子任务 1（已完成）
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: sub1_id,
            name: "subtask 1".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: Some(main_id),
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 添加子任务 2（运行中）
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: sub2_id,
            name: "subtask 2".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: Some(main_id),
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 验证主任务进度
        let main_task = app.tasks.iter().find(|t| t.id == main_id).unwrap();
        assert_eq!(main_task.subtask_count, 2);
        assert_eq!(main_task.completed_count, 1);
    }

    #[test]
    fn second_approval_goes_to_queued() {
        let mut app = test_app();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id: id1,
            agent_name: "agent1".to_string(),
            tool_name: "tool1".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![ApprovalOption {
                id: "allow_once".to_string(),
                label: "Allow Once".to_string(),
                description: "仅本次允许".to_string(),
            }],
            approval_context: None,
        });
        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id: id2,
            agent_name: "agent2".to_string(),
            tool_name: "tool2".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![ApprovalOption {
                id: "allow_once".to_string(),
                label: "Allow Once".to_string(),
                description: "仅本次允许".to_string(),
            }],
            approval_context: None,
        });

        assert_eq!(app.pending_approvals.len(), 2);
        let queued_count = app
            .messages
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    ChatMessage::ApprovalCard(ApprovalCardState::Queued { .. })
                )
            })
            .count();
        assert_eq!(queued_count, 1);
    }

    #[test]
    fn approval_mode_up_down_updates_selected_index() {
        let mut app = test_app();
        let request_id = Uuid::new_v4();

        // 创建有3个选项的审批
        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id,
            agent_name: "agent".to_string(),
            tool_name: "tool".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![
                ApprovalOption {
                    id: "opt1".to_string(),
                    label: "Option 1".to_string(),
                    description: "desc1".to_string(),
                },
                ApprovalOption {
                    id: "opt2".to_string(),
                    label: "Option 2".to_string(),
                    description: "desc2".to_string(),
                },
                ApprovalOption {
                    id: "opt3".to_string(),
                    label: "Option 3".to_string(),
                    description: "desc3".to_string(),
                },
            ],
            approval_context: None,
        });

        // 初始状态应该是 Approval 模式，selected_index = 0
        assert!(matches!(
            app.mode,
            AppMode::Approval {
                selected_index: 0,
                ..
            }
        ));

        // 按 Down 键，应该移动到 index 1
        app.handle_key_event(KeyEvent::from(KeyCode::Down));
        match &app.mode {
            AppMode::Approval { selected_index, .. } => {
                assert_eq!(*selected_index, 1);
            }
            _ => panic!("should be in Approval mode"),
        }

        // 再按 Down 键，应该移动到 index 2
        app.handle_key_event(KeyEvent::from(KeyCode::Down));
        match &app.mode {
            AppMode::Approval { selected_index, .. } => {
                assert_eq!(*selected_index, 2);
            }
            _ => panic!("should be in Approval mode"),
        }

        // 再按 Down 键，应该保持在 index 2（已经是最后一个）
        app.handle_key_event(KeyEvent::from(KeyCode::Down));
        match &app.mode {
            AppMode::Approval { selected_index, .. } => {
                assert_eq!(*selected_index, 2);
            }
            _ => panic!("should be in Approval mode"),
        }

        // 按 Up 键，应该移动到 index 1
        app.handle_key_event(KeyEvent::from(KeyCode::Up));
        match &app.mode {
            AppMode::Approval { selected_index, .. } => {
                assert_eq!(*selected_index, 1);
            }
            _ => panic!("should be in Approval mode"),
        }

        // 按 Up 键，应该移动到 index 0
        app.handle_key_event(KeyEvent::from(KeyCode::Up));
        match &app.mode {
            AppMode::Approval { selected_index, .. } => {
                assert_eq!(*selected_index, 0);
            }
            _ => panic!("should be in Approval mode"),
        }

        // 再按 Up 键，应该保持在 index 0（已经是第一个）
        app.handle_key_event(KeyEvent::from(KeyCode::Up));
        match &app.mode {
            AppMode::Approval { selected_index, .. } => {
                assert_eq!(*selected_index, 0);
            }
            _ => panic!("should be in Approval mode"),
        }
    }

    #[test]
    fn approval_card_selected_index_synced_on_key_press() {
        let mut app = test_app();
        let request_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id,
            agent_name: "agent".to_string(),
            tool_name: "tool".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![
                ApprovalOption {
                    id: "opt1".to_string(),
                    label: "Option 1".to_string(),
                    description: "desc1".to_string(),
                },
                ApprovalOption {
                    id: "opt2".to_string(),
                    label: "Option 2".to_string(),
                    description: "desc2".to_string(),
                },
            ],
            approval_context: None,
        });

        // 按 Down 键
        app.handle_key_event(KeyEvent::from(KeyCode::Down));

        // 检查 ApprovalCard 中的 selected_index 是否同步更新
        let mut card_selected_index = None;
        for msg in &app.messages {
            if let ChatMessage::ApprovalCard(ApprovalCardState::Active {
                request_id: rid,
                selected_index,
                ..
            }) = msg
                && *rid == request_id
            {
                card_selected_index = Some(*selected_index);
            }
        }
        assert_eq!(
            card_selected_index,
            Some(1),
            "ApprovalCard selected_index should be synced"
        );
    }

    #[test]
    fn failed_status_counted_as_completed() {
        let mut app = test_app();
        let main_id = Uuid::new_v4();
        let sub1_id = Uuid::new_v4();
        let sub2_id = Uuid::new_v4();

        // 添加主任务
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: main_id,
            name: "main task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 添加子任务 1（已完成）
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: sub1_id,
            name: "subtask 1".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: Some(main_id),
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 添加子任务 2（失败）
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: sub2_id,
            name: "subtask 2".to_string(),
            status: TaskStatusKind::Failed,
            old_status: None,
            result: Some("error".to_string()),
            parent_id: Some(main_id),
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 验证：Done 和 Failed 都计入已完成
        let main_task = app.tasks.iter().find(|t| t.id == main_id).unwrap();
        assert_eq!(main_task.subtask_count, 2);
        assert_eq!(main_task.completed_count, 2);
    }

    #[test]
    fn main_task_without_subtasks_has_zero_progress() {
        let mut app = test_app();
        let main_id = Uuid::new_v4();

        // 添加主任务
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: main_id,
            name: "lonely task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 触发另一个任务更新，确保零进度保持
        let other_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: other_id,
            name: "other task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        let main_task = app.tasks.iter().find(|t| t.id == main_id).unwrap();
        assert_eq!(main_task.subtask_count, 0);
        assert_eq!(main_task.completed_count, 0);
    }

    #[test]
    fn task_state_origin_channel_from_event() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        let qq_channel = ChannelId {
            frontend: FrontendKind::QQ,
            user_id: "qq_user".to_string(),
            thread_id: None,
        };
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "qq task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: Some(qq_channel.clone()),
            agent_name: None,
            waiting_reason: None,
        });
        assert_eq!(app.tasks[0].origin_channel, Some(qq_channel));
        assert_eq!(app.tasks[0].completed_at, None);
    }

    #[test]
    fn task_completed_at_set_on_terminal_status() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "done task".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        assert!(app.tasks[0].completed_at.is_some());
    }

    #[test]
    fn same_task_id_does_not_duplicate() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "task".to_string(),
            status: TaskStatusKind::Done,
            old_status: Some(TaskStatusKind::Running),
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].status, TaskStatusKind::Done);
    }

    #[test]
    fn cleanup_removes_expired_completed_tasks() {
        let mut app = test_app();
        let main_id = Uuid::new_v4();
        let sub_id = Uuid::new_v4();

        // 添加已完成的主任务
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: main_id,
            name: "old done".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });
        // 添加已完成的子任务
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id: sub_id,
            name: "sub done".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: Some(main_id),
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // 手动将 completed_at 设为 6 秒前（超过 5 秒阈值）
        let six_secs_ago = std::time::Instant::now() - std::time::Duration::from_secs(6);
        for task in &mut app.tasks {
            task.completed_at = Some(six_secs_ago);
        }

        app.cleanup_completed_tasks();
        assert!(
            app.tasks.is_empty(),
            "expired completed tasks should be removed"
        );
    }

    #[test]
    fn cleanup_keeps_recent_completed_tasks() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "fresh done".to_string(),
            status: TaskStatusKind::Done,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        // completed_at 刚设置，不会超过 5 秒
        app.cleanup_completed_tasks();
        assert_eq!(
            app.tasks.len(),
            1,
            "recently completed tasks should be kept"
        );
    }

    #[test]
    fn cleanup_keeps_active_tasks() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "running task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: None,
            waiting_reason: None,
        });

        app.cleanup_completed_tasks();
        assert_eq!(
            app.tasks.len(),
            1,
            "active tasks should never be cleaned up"
        );
    }

    #[test]
    fn reject_with_feedback_option_enters_feedback_mode() {
        let mut app = test_app();
        let request_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id,
            agent_name: "agent".to_string(),
            tool_name: "experience_governance".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![
                ApprovalOption {
                    id: "approve".to_string(),
                    label: "批准".to_string(),
                    description: "批准".to_string(),
                },
                ApprovalOption {
                    id: "reject".to_string(),
                    label: "拒绝".to_string(),
                    description: "拒绝".to_string(),
                },
                ApprovalOption {
                    id: "reject_with_feedback".to_string(),
                    label: "拒绝并反馈".to_string(),
                    description: "拒绝并反馈".to_string(),
                },
            ],
            approval_context: None,
        });

        // 移动到 reject_with_feedback（index 2）
        app.handle_key_event(KeyEvent::from(KeyCode::Down));
        app.handle_key_event(KeyEvent::from(KeyCode::Down));

        // 按 Enter 应进入 Feedback 模式而非发送确认
        app.handle_key_event(KeyEvent::from(KeyCode::Enter));

        assert!(matches!(
            app.mode,
            AppMode::Feedback {
                request_id: rid,
                ref feedback_buffer,
                cursor_position: 0
            } if rid == request_id && feedback_buffer.is_empty()
        ));
    }

    #[test]
    fn feedback_mode_esc_returns_to_approval() {
        let mut app = test_app();
        let request_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id,
            agent_name: "agent".to_string(),
            tool_name: "tool".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![ApprovalOption {
                id: "reject_with_feedback".to_string(),
                label: "拒绝并反馈".to_string(),
                description: "拒绝并反馈".to_string(),
            }],
            approval_context: None,
        });

        // 按 Enter 进入 Feedback 模式
        app.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.mode, AppMode::Feedback { .. }));

        // 按 Esc 应回到 Approval 模式
        app.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.mode, AppMode::Approval { .. }));
    }

    #[test]
    fn feedback_mode_enter_sends_confirmation_with_feedback() {
        use crossbeam_channel::TryRecvError;

        use crate::domain::UserAction;

        let (action_tx, action_rx) = unbounded();
        let mut app = App::new(action_tx);
        let request_id = Uuid::new_v4();

        app.handle_engine_event(EngineEvent::ApprovalRequest {
            target: EventTarget::Broadcast,
            request_id,
            agent_name: "agent".to_string(),
            tool_name: "tool".to_string(),
            tool_input: serde_json::json!({}),
            options: vec![ApprovalOption {
                id: "reject_with_feedback".to_string(),
                label: "拒绝并反馈".to_string(),
                description: "拒绝并反馈".to_string(),
            }],
            approval_context: None,
        });

        // 进入 Feedback 模式
        app.handle_key_event(KeyEvent::from(KeyCode::Enter));

        // 输入反馈文本
        app.handle_key_event(KeyEvent::from(KeyCode::Char('n')));
        app.handle_key_event(KeyEvent::from(KeyCode::Char('a')));
        app.handle_key_event(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key_event(KeyEvent::from(KeyCode::Char('e')));

        // 按 Enter 提交
        app.handle_key_event(KeyEvent::from(KeyCode::Enter));

        // 应发送 Confirmation with feedback
        let action = action_rx.try_recv();
        assert!(!matches!(action, Err(TryRecvError::Empty)));
        if let Ok(UserAction::Confirmation {
            option_id,
            feedback,
            ..
        }) = action
        {
            assert_eq!(option_id, "reject_with_feedback");
            assert_eq!(feedback, Some("name".to_string()));
        } else {
            panic!("expected Confirmation action with feedback");
        }

        // 审批应被移除，回到 Chat 模式
        assert!(app.pending_approvals.is_empty());
        assert!(matches!(app.mode, AppMode::Chat));
    }

    #[test]
    fn tool_call_started_converted_to_system_message() {
        let mut app = test_app();
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        app.handle_engine_event(EngineEvent::ToolCallStarted {
            target: EventTarget::Broadcast,
            task_id,
            agent_name: "TestAgent".to_string(),
            tool_name: "shell_exec".to_string(),
            tool_input_summary: "ls -la".to_string(),
        });
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(
            app.messages.last(),
            Some(ChatMessage::System(content)) if content == "[a1b2c3d4] 🔧 TestAgent 调用 shell_exec: ls -la"
        ));
    }

    #[test]
    fn tool_call_started_without_summary_omits_colon() {
        let mut app = test_app();
        let task_id = Uuid::parse_str("a1b2c3d4-1111-2222-3333-444444444444").unwrap();
        app.handle_engine_event(EngineEvent::ToolCallStarted {
            target: EventTarget::Broadcast,
            task_id,
            agent_name: "TestAgent".to_string(),
            tool_name: "shell_exec".to_string(),
            tool_input_summary: String::new(),
        });
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(
            app.messages.last(),
            Some(ChatMessage::System(content)) if content == "[a1b2c3d4] 🔧 TestAgent 调用 shell_exec"
        ));
    }

    #[test]
    fn task_status_changed_persists_agent_name_and_waiting_reason() {
        let mut app = test_app();
        let task_id = Uuid::new_v4();
        app.handle_engine_event(EngineEvent::TaskStatusChanged {
            target: EventTarget::Broadcast,
            task_id,
            name: "test task".to_string(),
            status: TaskStatusKind::Running,
            old_status: None,
            result: None,
            parent_id: None,
            origin_channel: None,
            agent_name: Some("TestAgent".to_string()),
            waiting_reason: Some(WaitingReasonKind::Tool),
        });
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].agent_name, Some("TestAgent".to_string()));
        assert_eq!(app.tasks[0].waiting_reason, Some(WaitingReasonKind::Tool));
    }
}
