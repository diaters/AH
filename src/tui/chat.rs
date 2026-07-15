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
        *self = Self::Done {
            tool_name,
            decision,
        };
    }
}

pub struct ChatPanel;

impl ChatPanel {
    pub fn render(app: &mut super::app::App, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for msg in &app.messages {
            match msg {
                ChatMessage::User(content) => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "You",
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(": ", Style::default().fg(Color::Green)),
                        Span::styled(content.clone(), Style::default().fg(Color::Green)),
                    ]));
                }
                ChatMessage::Agent { name, content } => {
                    lines.push(Line::from(vec![
                        Span::styled(
                            name.clone(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(": ", Style::default().fg(Color::Cyan)),
                    ]));
                    for line in content.lines() {
                        lines.push(Line::from(Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::White),
                        )));
                    }
                }
                ChatMessage::System(content) => {
                    lines.push(Line::from(Span::styled(
                        format!("[system] {content}"),
                        Style::default().fg(Color::Yellow),
                    )));
                }
                ChatMessage::ApprovalCard(state) => match state {
                    ApprovalCardState::Active {
                        agent_name,
                        tool_name,
                        tool_input,
                        options,
                        selected_index,
                        ..
                    } => {
                        // 标题行
                        lines.push(Line::from(vec![
                            Span::styled(
                                " REVIEW ".to_string(),
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!(" {tool_name}"),
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]));

                        // Agent 来源
                        lines.push(Line::from(Span::styled(
                            format!("  from: {agent_name}"),
                            Style::default().fg(Color::DarkGray),
                        )));

                        // Tool 输入（缩进展示）
                        for line in tool_input.lines().take(6) {
                            lines.push(Line::from(Span::styled(
                                format!("  {line}"),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                        if tool_input.lines().count() > 6 {
                            lines.push(Line::from(Span::styled(
                                "  ...",
                                Style::default().fg(Color::DarkGray),
                            )));
                        }

                        lines.push(Line::from(""));

                        // 选项列表：用醒目的 ◉ / ○ + 反色高亮
                        for (i, opt) in options.iter().enumerate() {
                            let is_selected = i == *selected_index;
                            let (bullet, style) = if is_selected {
                                (
                                    "\u{25c9}", // ◉
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(Color::Cyan)
                                        .add_modifier(Modifier::BOLD),
                                )
                            } else {
                                (
                                    "\u{25cb}", // ○
                                    Style::default().fg(Color::DarkGray),
                                )
                            };
                            let label = format!(" {bullet} {} - {}", opt.label, opt.description);
                            lines.push(Line::from(Span::styled(label, style)));
                        }

                        // 操作提示
                        lines.push(Line::from(vec![
                            Span::styled(
                                "  \u{2191}\u{2193}",
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(" 选择 ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                "Enter",
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(" 确认 ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                "Esc",
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(" 跳过", Style::default().fg(Color::DarkGray)),
                        ]));
                    }
                    ApprovalCardState::Queued { tool_name } => {
                        lines.push(Line::from(vec![
                            Span::styled(
                                " WAIT ".to_string(),
                                Style::default().fg(Color::Black).bg(Color::DarkGray),
                            ),
                            Span::styled(
                                format!(" {tool_name}"),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                    ApprovalCardState::Done {
                        tool_name,
                        decision,
                    } => {
                        lines.push(Line::from(vec![
                            Span::styled(
                                " DONE ".to_string(),
                                Style::default().fg(Color::Black).bg(Color::Green),
                            ),
                            Span::styled(
                                format!(" {tool_name} \u{2192} {decision}"),
                                Style::default().fg(Color::Green),
                            ),
                        ]));
                    }
                },
            }
        }

        // 空状态提示
        if app.messages.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Welcome to AI Harness",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                "Type a message below to get started",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Feedback 模式渲染：标题行 + 提示文本 + 输入行
        if let crate::tui::app::AppMode::Feedback {
            feedback_buffer, ..
        } = &app.mode
        {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " FEEDBACK ".to_string(),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " 请输入评审反馈".to_string(),
                    Style::default().fg(Color::Magenta),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                "  Enter 提交 · Esc 返回选项列表".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("\u{276f} ", Style::default().fg(Color::Magenta)),
                Span::styled(feedback_buffer.clone(), Style::default().fg(Color::White)),
                Span::styled(
                    if feedback_buffer.is_empty() {
                        "输入评审建议..."
                    } else {
                        ""
                    },
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset, 0));

        frame.render_widget(paragraph, area);
    }
}
