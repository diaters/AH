use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::domain::{ChannelId, FrontendKind, TaskStatusKind};

pub struct StatusPanel;

impl StatusPanel {
    fn is_task_completed(status: TaskStatusKind) -> bool {
        matches!(status, TaskStatusKind::Done | TaskStatusKind::Failed)
    }

    fn get_dimmed_color_if_completed(status: TaskStatusKind, base_color: Color) -> Color {
        if Self::is_task_completed(status) {
            Color::DarkGray // #6272a4 的近似色
        } else {
            base_color
        }
    }

    fn channel_label(channel: &ChannelId) -> (&'static str, Color) {
        match channel.frontend {
            FrontendKind::Tui => ("TUI", Color::Green),
            FrontendKind::QQ => ("QQ", Color::Magenta),
            FrontendKind::Telegram => ("TG", Color::Blue),
            FrontendKind::Web => ("Web", Color::DarkGray),
            FrontendKind::Feishu => ("FS", Color::DarkGray),
        }
    }

    fn origin_label(origin_channel: &Option<ChannelId>) -> (&'static str, Color) {
        match origin_channel {
            Some(ch) => Self::channel_label(ch),
            None => ("EVT", Color::DarkGray),
        }
    }

    pub fn render(app: &crate::tui::app::App, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        // 当前模式指示
        let (mode_label, mode_color) = match &app.mode {
            crate::tui::app::AppMode::Chat => ("CHAT", Color::Green),
            crate::tui::app::AppMode::Approval { .. } => ("REVIEW", Color::Yellow),
            crate::tui::app::AppMode::Feedback { .. } => ("FEEDBACK", Color::Magenta),
        };
        lines.push(Line::from(vec![Span::styled(
            format!(" {mode_label} "),
            Style::default()
                .fg(Color::Black)
                .bg(mode_color)
                .add_modifier(Modifier::BOLD),
        )]));

        // 审批待处理
        if !app.pending_approvals.is_empty() {
            let count = app.pending_approvals.len();
            lines.push(Line::from(vec![Span::styled(
                format!(" {count} approval{}", if count > 1 { "s" } else { "" }),
                Style::default().fg(Color::Yellow),
            )]));
        }

        lines.push(Line::from(""));

        // Task 列表（层级显示）
        lines.push(Line::from(Span::styled(
            "Tasks",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        if app.tasks.is_empty() {
            lines.push(Line::from(Span::styled(
                "  No active tasks",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            // 分离主任务和子任务
            let main_tasks: Vec<_> = app.tasks.iter().filter(|t| t.parent_id.is_none()).collect();

            let subtasks_by_parent: std::collections::HashMap<uuid::Uuid, Vec<_>> = app
                .tasks
                .iter()
                .filter(|t| t.parent_id.is_some())
                .filter_map(|t| t.parent_id.map(|pid| (pid, t)))
                .fold(std::collections::HashMap::new(), |mut acc, (pid, task)| {
                    acc.entry(pid).or_default().push(task);
                    acc
                });

            // 渲染主任务及其子任务
            for main_task in main_tasks {
                let (icon, color) = match main_task.status {
                    TaskStatusKind::Pending => ("○", Color::DarkGray),
                    TaskStatusKind::Running => ("●", Color::Yellow),
                    TaskStatusKind::Waiting => ("○", Color::Cyan),
                    TaskStatusKind::Done => ("✓", Color::Green),
                    TaskStatusKind::Failed => ("✗", Color::Red),
                };

                // 主任务颜色（已完成则变暗）
                let main_color = Self::get_dimmed_color_if_completed(main_task.status, color);

                // Calculate progress from actual subtasks instead of stored fields
                let subtasks = subtasks_by_parent.get(&main_task.id);
                let progress_text = if let Some(subs) = subtasks {
                    let total = subs.len();
                    if total > 0 {
                        let completed = subs
                            .iter()
                            .filter(|s| Self::is_task_completed(s.status))
                            .count();
                        format!(" ({}/{})", completed, total)
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                let (label_text, label_color) = Self::origin_label(&main_task.origin_channel);
                let mut spans = vec![
                    Span::styled(format!("{icon} "), Style::default().fg(main_color)),
                    Span::styled(format!("[{label_text}] "), Style::default().fg(label_color)),
                    Span::styled(
                        format!("{}{}", main_task.name, progress_text),
                        Style::default().fg(main_color).add_modifier(Modifier::BOLD),
                    ),
                ];
                if let Some(ref agent) = main_task.agent_name {
                    spans.push(Span::styled(
                        format!(" @{agent}"),
                        Style::default().fg(Color::White),
                    ));
                }
                if let Some(reason) = main_task.waiting_reason {
                    let reason_text = match reason {
                        crate::domain::WaitingReasonKind::Agent => "⏳agent",
                        crate::domain::WaitingReasonKind::Tool => "⏳tool",
                        crate::domain::WaitingReasonKind::User => "⏳user",
                        crate::domain::WaitingReasonKind::Retry => "⏳retry",
                        crate::domain::WaitingReasonKind::Other => "⏳other",
                    };
                    spans.push(Span::styled(
                        format!(" {reason_text}"),
                        Style::default().fg(Color::Cyan),
                    ));
                }
                lines.push(Line::from(spans));

                // Render subtasks
                if let Some(subtasks) = subtasks_by_parent.get(&main_task.id) {
                    // Sort subtasks by id for consistent ordering
                    let mut sorted_subtasks: Vec<_> = subtasks.iter().collect();
                    sorted_subtasks.sort_by_key(|t| t.id);

                    for subtask in sorted_subtasks {
                        let (sub_icon, sub_color) = match subtask.status {
                            TaskStatusKind::Pending => ("○", Color::DarkGray),
                            TaskStatusKind::Running => ("●", Color::Yellow),
                            TaskStatusKind::Waiting => ("○", Color::Cyan),
                            TaskStatusKind::Done => ("✓", Color::Green),
                            TaskStatusKind::Failed => ("✗", Color::Red),
                        };

                        // 子任务颜色（已完成则变暗）
                        let sub_task_color =
                            Self::get_dimmed_color_if_completed(subtask.status, sub_color);

                        // 子任务行：缩进 + 虚线前缀
                        let mut sub_spans = vec![
                            Span::styled("  │ ", Style::default().fg(Color::DarkGray)), // 虚线效果
                            Span::styled(
                                format!("{sub_icon} "),
                                Style::default().fg(sub_task_color),
                            ),
                            Span::styled(&subtask.name, Style::default().fg(sub_task_color)),
                        ];
                        if let Some(ref agent) = subtask.agent_name {
                            sub_spans.push(Span::styled(
                                format!(" @{agent}"),
                                Style::default().fg(Color::White),
                            ));
                        }
                        lines.push(Line::from(sub_spans));
                    }
                }
            }
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
            crate::tui::app::AppMode::Feedback { .. } => {
                lines.push(Line::from(Span::styled(
                    "Enter  提交反馈",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "Esc    返回选项",
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
