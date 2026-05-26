use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::domain::{AgentStatusKind, TaskStatusKind};

pub struct StatusPanel;

impl StatusPanel {
    pub fn render(app: &super::app::App, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        // Agent 列表
        lines.push(Line::from(Span::styled(
            "Agents",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));

        for agent in &app.agents {
            let (icon, color) = match agent.status {
                AgentStatusKind::Idle => ("\u{25cf}", Color::Green),
                AgentStatusKind::Running => ("\u{25cf}", Color::Yellow),
                AgentStatusKind::WaitingApproval => ("\u{25cf}", Color::Magenta),
                AgentStatusKind::WaitingTool => ("\u{25cf}", Color::Cyan),
            };
            let status_text = match agent.status {
                AgentStatusKind::Idle => "idle",
                AgentStatusKind::Running => "running",
                AgentStatusKind::WaitingApproval => "waiting approval",
                AgentStatusKind::WaitingTool => "waiting tool",
            };
            lines.push(Line::from(vec![
                Span::styled(icon.to_string(), Style::default().fg(color)),
                Span::styled(format!(" {} ", agent.name), Style::default().fg(Color::White)),
                Span::styled(status_text.to_string(), Style::default().fg(Color::DarkGray)),
            ]));
        }

        lines.push(Line::from(""));

        // Task 列表
        lines.push(Line::from(Span::styled(
            "Tasks",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));

        for task in &app.tasks {
            let (icon, color) = match task.status {
                TaskStatusKind::Pending => ("\u{25cf}", Color::DarkGray),
                TaskStatusKind::Running => ("\u{25cf}", Color::Yellow),
                TaskStatusKind::Waiting => ("\u{25cf}", Color::Cyan),
                TaskStatusKind::Done => ("\u{2713}", Color::Green),
                TaskStatusKind::Failed => ("\u{2717}", Color::Red),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(&task.name, Style::default().fg(Color::White)),
            ]));
        }

        // 审批徽章
        if !app.pending_approvals.is_empty() {
            lines.push(Line::from(""));
            let count = app.pending_approvals.len();
            lines.push(Line::from(vec![
                Span::styled("\u{26a1} Approvals", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!(" [{count}]"),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

        frame.render_widget(paragraph, area);
    }
}
