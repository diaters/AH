use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::app::AppMode;

pub struct InputBar;

impl InputBar {
    pub fn render(app: &crate::tui::app::App, frame: &mut Frame, area: Rect) {
        let (prompt, content, content_color) = match &app.mode {
            AppMode::Chat => {
                let display = if app.input_buffer.is_empty() {
                    "\u{8f93}\u{5165}\u{6d88}\u{606f}...".to_string()
                } else {
                    app.input_buffer.clone()
                };
                let color = if app.input_buffer.is_empty() {
                    Color::DarkGray
                } else {
                    Color::White
                };
                ("\u{276f}", display, color)
            }
            AppMode::Approval { .. } => (
                "\u{276f}",
                "\u{2191}\u{2193} \u{9009}\u{62e9}\u{5ba1}\u{6279}\u{9009}\u{9879} · Enter \u{786e}\u{8ba4} · Esc \u{8fd4}\u{56de}".to_string(),
                Color::DarkGray,
            ),
            AppMode::Feedback {
                feedback_buffer, ..
            } => {
                let display = if feedback_buffer.is_empty() {
                    "\u{8f93}\u{5165}\u{8bc4}\u{5ba1}\u{53cd}\u{9988}...".to_string()
                } else {
                    feedback_buffer.clone()
                };
                let color = if feedback_buffer.is_empty() {
                    Color::DarkGray
                } else {
                    Color::White
                };
                ("\u{276f}", display, color)
            }
        };

        let paragraph = Paragraph::new(Line::from(vec![
            Span::styled(format!("{prompt} "), Style::default().fg(Color::DarkGray)),
            Span::styled(content, Style::default().fg(content_color)),
        ]))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

        frame.render_widget(paragraph, area);
    }
}
