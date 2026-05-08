//! TUI task detail: renders the compact resume-pack of a task — same
//! text the CLI prints from `task-journal pack <id>`. Read-only,
//! scrollable, escape goes back to the task list.

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

pub struct TaskDetail {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub body: String,
    pub scroll: u16,
}

impl TaskDetail {
    pub fn new(task_id: String, title: String, status: String, body: String) -> Self {
        Self {
            task_id,
            title,
            status,
            body,
            scroll: 0,
        }
    }

    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_bottom(&mut self) {
        // Approximate — Paragraph clamps to its content; we set a large
        // scroll value and let the widget cap it.
        self.scroll = u16::MAX / 2;
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(frame.area());

        let header = Paragraph::new(format!(
            " {} · {} · [{}]",
            self.task_id, self.title, self.status
        ))
        .style(Style::default().fg(Color::White).bg(Color::Blue));
        frame.render_widget(header, chunks[0]);

        let body = Paragraph::new(self.body.as_str())
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(body, chunks[1]);

        let footer = Paragraph::new(Line::from(vec![
            Span::styled("↑↓/jk", Style::default().fg(Color::Cyan)),
            Span::raw(" scroll · "),
            Span::styled("PgUp/PgDn", Style::default().fg(Color::Cyan)),
            Span::raw(" page · "),
            Span::styled("Esc/q", Style::default().fg(Color::Cyan)),
            Span::raw(" back"),
        ]));
        frame.render_widget(footer, chunks[2]);
    }
}
