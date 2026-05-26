use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use uuid::Uuid;

use crate::domain::ApprovalOption;

/// 对话消息
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    Agent { name: String, content: String },
    System(String),
    ApprovalCard(ApprovalCardState),
}

/// 审批卡片状态
#[derive(Debug, Clone)]
pub enum ApprovalCardState {
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
    Done {
        tool_name: String,
        decision: String,
    },
}

impl ApprovalCardState {
    pub fn is_active_for(&self, request_id: Uuid) -> bool {
        matches!(self, Self::Active { request_id: rid, .. } if *rid == request_id)
    }

    pub fn mark_done(&mut self, decision: String) {
        let tool_name = match self {
            Self::Active { tool_name, .. } => tool_name.clone(),
            Self::Queued { tool_name } => tool_name.clone(),
            Self::Done { tool_name, .. } => tool_name.clone(),
        };
        *self = Self::Done { tool_name, decision };
    }
}

pub struct ChatPanel;

impl ChatPanel {
    pub fn render(app: &mut super::app::App, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for msg in &app.messages {
            match msg {
                ChatMessage::User(content) => {
                    lines.push(Line::from(Span::styled(
                        format!("You: {content}"),
                        Style::default().fg(Color::Green),
                    )));
                }
                ChatMessage::Agent { name, content } => {
                    lines.push(Line::from(Span::styled(
                        format!("{name}: "),
                        Style::default().fg(Color::Cyan),
                    )));
                    for line in content.lines() {
                        lines.push(Line::from(Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::White),
                        )));
                    }
                }
                ChatMessage::System(content) => {
                    lines.push(Line::from(Span::styled(
                        content.clone(),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                ChatMessage::ApprovalCard(state) => {
                    match state {
                        ApprovalCardState::Active {
                            tool_name,
                            tool_input,
                            options,
                            selected_index,
                            ..
                        } => {
                            lines.push(Line::from(Span::styled(
                                format!("\u{26a1} Approval Required: {tool_name}"),
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            )));
                            lines.push(Line::from(Span::styled(
                                format!("  {tool_input}"),
                                Style::default().fg(Color::DarkGray),
                            )));
                            for (i, opt) in options.iter().enumerate() {
                                let indicator = if i == *selected_index { "\u{203a}" } else { " " };
                                let style = if i == *selected_index {
                                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(Color::DarkGray)
                                };
                                lines.push(Line::from(Span::styled(
                                    format!("  {indicator} {} - {}", opt.label, opt.description),
                                    style,
                                )));
                            }
                            lines.push(Line::from(Span::styled(
                                "  \u{2191}\u{2193} \u{9009}\u{62e9} · Enter \u{786e}\u{8ba4}",
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                        ApprovalCardState::Queued { tool_name } => {
                            lines.push(Line::from(Span::styled(
                                format!("\u{23f3} {tool_name} - \u{6392}\u{961f}\u{4e2d}"),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                        ApprovalCardState::Done { tool_name, decision } => {
                            lines.push(Line::from(Span::styled(
                                format!("\u{2713} {tool_name} \u{5df2}{decision}"),
                                Style::default().fg(Color::Green),
                            )));
                        }
                    }
                }
            }
        }

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset, 0));

        frame.render_widget(paragraph, area);
    }
}
