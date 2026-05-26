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
        let (prompt, content) = match &app.mode {
            AppMode::Chat => {
                let display = if app.input_buffer.is_empty() {
                    "\u{8f93}\u{5165}\u{6d88}\u{606f}...".to_string()
                } else {
                    app.input_buffer.clone()
                };
                ("\u{276f}", display)
            }
            AppMode::Approval { .. } => {
                ("\u{276f}", "\u{2191}\u{2193} \u{9009}\u{62e9}\u{5ba1}\u{6279}\u{9009}\u{9879} · Enter \u{786e}\u{8ba4} · Esc \u{8fd4}\u{56de}".to_string())
            }
        };

        let content_color = if app.input_buffer.is_empty() {
            Color::DarkGray
        } else {
            Color::White
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
