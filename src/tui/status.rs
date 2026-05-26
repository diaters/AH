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
    pub fn render(app: &crate::tui::app::App, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        // 当前模式指示
        let (mode_label, mode_color) = match &app.mode {
            crate::tui::app::AppMode::Chat => ("CHAT", Color::Green),
            crate::tui::app::AppMode::Approval { .. } => ("REVIEW", Color::Yellow),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {mode_label} "),
                Style::default().fg(Color::Black).bg(mode_color).add_modifier(Modifier::BOLD),
            ),
        ]));

        // 审批待处理
        if !app.pending_approvals.is_empty() {
            let count = app.pending_approvals.len();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {count} approval{}", if count > 1 { "s" } else { "" }),
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        }

        lines.push(Line::from(""));

        // Agent 列表
        lines.push(Line::from(Span::styled(
            "Agents",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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
                AgentStatusKind::WaitingApproval => "waiting",
                AgentStatusKind::WaitingTool => "waiting",
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(
                    format!("{} ", agent.name),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    status_text.to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        lines.push(Line::from(""));

        // Task 列表
        lines.push(Line::from(Span::styled(
            "Tasks",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        for task in &app.tasks {
            let (icon, color) = match task.status {
                TaskStatusKind::Pending => ("\u{25cb}", Color::DarkGray),
                TaskStatusKind::Running => ("\u{25cf}", Color::Yellow),
                TaskStatusKind::Waiting => ("\u{25cb}", Color::Cyan),
                TaskStatusKind::Done => ("\u{2713}", Color::Green),
                TaskStatusKind::Failed => ("\u{2717}", Color::Red),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(&task.name, Style::default().fg(Color::White)),
            ]));
        }

        // 快捷键提示（底部）
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Shortcuts",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        match &app.mode {
            crate::tui::app::AppMode::Chat => {
                lines.push(Line::from(Span::styled(
                    "Enter  发送",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "Ctrl+Q 退出",
                    Style::default().fg(Color::DarkGray),
                )));
                if !app.pending_approvals.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "Tab    审批",
                        Style::default().fg(Color::Yellow),
                    )));
                }
            }
            crate::tui::app::AppMode::Approval { .. } => {
                lines.push(Line::from(Span::styled(
                    "\u{2191}\u{2193}     选择",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "Enter  确认",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "Esc    跳过",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

        frame.render_widget(paragraph, area);
    }
}
