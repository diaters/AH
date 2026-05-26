use crossterm::event::{KeyCode, KeyEvent};
use crossbeam_channel::Sender;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};
use uuid::Uuid;

use crate::domain::{
    AgentStatusKind, ApprovalOption, ChannelId, EngineEvent, FrontendKind,
    MessageRole, TaskStatusKind, UserAction,
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
            AppMode::Approval { request_id, selected_index, options } => {
                self.handle_approval_key(key, *request_id, *selected_index, options.clone());
            }
        }
    }

    fn handle_chat_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                let content = self.input_buffer.clone();
                if !content.is_empty() {
                    let channel = ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                    };
                    let _ = self.action_tx.send(UserAction::Text { channel, content: content.clone() });
                    self.messages.push(ChatMessage::User(content));
                    self.input_buffer.clear();
                    self.cursor_position = 0;
                }
            }
            KeyCode::Char(c) => {
                self.input_buffer.insert(self.cursor_position, c);
                self.cursor_position += 1;
            }
            KeyCode::Backspace => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                    self.input_buffer.remove(self.cursor_position);
                }
            }
            KeyCode::Delete => {
                if self.cursor_position < self.input_buffer.len() {
                    self.input_buffer.remove(self.cursor_position);
                }
            }
            KeyCode::Left => {
                if self.cursor_position > 0 {
                    self.cursor_position -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_position < self.input_buffer.len() {
                    self.cursor_position += 1;
                }
            }
            KeyCode::Up => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
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
            KeyCode::Up => {
                if selected_index > 0 {
                    self.mode = AppMode::Approval {
                        request_id,
                        selected_index: selected_index - 1,
                        options,
                    };
                }
            }
            KeyCode::Down => {
                if selected_index < options.len() - 1 {
                    self.mode = AppMode::Approval {
                        request_id,
                        selected_index: selected_index + 1,
                        options,
                    };
                }
            }
            KeyCode::Enter => {
                if let Some(option) = options.get(selected_index) {
                    let channel = ChannelId {
                        frontend: FrontendKind::Tui,
                        user_id: "default".to_string(),
                    };
                    let _ = self.action_tx.send(UserAction::Confirmation {
                        channel,
                        request_id,
                        option_id: option.id.clone(),
                    });

                    // 更新消息列表中的审批卡片状态
                    for msg in &mut self.messages {
                        if let ChatMessage::ApprovalCard(state) = msg {
                            if state.is_active_for(request_id) {
                                state.mark_done(option.label.clone());
                            }
                        }
                    }

                    // 移除已处理的审批
                    self.pending_approvals.retain(|a| a.request_id != request_id);

                    // 切换到下一个审批或回到 Chat 模式
                    if let Some(next) = self.pending_approvals.first() {
                        self.mode = AppMode::Approval {
                            request_id: next.request_id,
                            selected_index: 0,
                            options: next.options.clone(),
                        };
                    } else {
                        self.mode = AppMode::Chat;
                    }
                }
            }
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
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

                let is_first = matches!(self.mode, AppMode::Chat)
                    && self.pending_approvals.is_empty();

                self.pending_approvals.push(pending);

                if is_first {
                    self.mode = AppMode::Approval {
                        request_id,
                        selected_index: 0,
                        options,
                    };
                    self.messages.push(ChatMessage::ApprovalCard(
                        ApprovalCardState::Active {
                            request_id,
                            agent_name,
                            tool_name,
                            tool_input: tool_input_str,
                            options: self.pending_approvals.last().map(|p| p.options.clone()).unwrap_or_default(),
                            selected_index: 0,
                        },
                    ));
                } else {
                    self.messages.push(ChatMessage::ApprovalCard(
                        ApprovalCardState::Queued { tool_name },
                    ));
                }
            }
            EngineEvent::ApprovalResult {
                request_id,
                decision,
                ..
            } => {
                for msg in &mut self.messages {
                    if let ChatMessage::ApprovalCard(state) = msg {
                        if state.is_active_for(request_id) {
                            state.mark_done(decision.clone());
                        }
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
                ..
            } => {
                if let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = status;
                    task.result = result;
                } else {
                    self.tasks.push(TaskState {
                        id: task_id,
                        name,
                        status,
                        result,
                        parent_id: None,
                    });
                }
            }
            EngineEvent::BatchProgress { .. } => {}
        }
    }

    /// 渲染 TUI
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(area);

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(30),
            ])
            .split(main_layout[0]);

        ChatPanel::render(self, frame, content_layout[0]);
        StatusPanel::render(self, frame, content_layout[1]);
        InputBar::render(self, frame, main_layout[1]);
    }
}
