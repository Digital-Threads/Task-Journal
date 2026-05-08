//! TUI task list: tasks of the current project, ordered open-first by
//! recency. Replaces the older session-browser default — surfaces what
//! the journal is *for* (tracked tasks with reasoning chains) instead
//! of raw chat session JSONLs.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use tj_core::db::TaskRow;

pub struct TaskList {
    pub tasks: Vec<TaskRow>,
    pub project_path: String,
    pub state: ListState,
}

impl TaskList {
    pub fn new(tasks: Vec<TaskRow>, project_path: String) -> Self {
        let mut state = ListState::default();
        if !tasks.is_empty() {
            state.select(Some(0));
        }
        Self {
            tasks,
            project_path,
            state,
        }
    }

    pub fn selected(&self) -> Option<&TaskRow> {
        self.state.selected().and_then(|i| self.tasks.get(i))
    }

    pub fn next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let i = self
            .state
            .selected()
            .map(|i| (i + 1) % self.tasks.len())
            .unwrap_or(0);
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let i = self
            .state
            .selected()
            .map(|i| if i == 0 { self.tasks.len() - 1 } else { i - 1 })
            .unwrap_or(0);
        self.state.select(Some(i));
    }

    pub fn first(&mut self) {
        if !self.tasks.is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn last(&mut self) {
        if !self.tasks.is_empty() {
            self.state.select(Some(self.tasks.len() - 1));
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(2),
            ])
            .split(frame.area());

        self.render_header(frame, chunks[0]);
        self.render_list(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let open = self.tasks.iter().filter(|t| t.status == "open").count();
        let closed = self.tasks.len() - open;
        let header = Paragraph::new(format!(
            " Task Journal — {} — {open} open · {closed} closed",
            shorten_path(&self.project_path)
        ))
        .style(Style::default().fg(Color::White).bg(Color::Blue));
        frame.render_widget(header, area);
    }

    fn render_list(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if self.tasks.is_empty() {
            let msg = Paragraph::new(
                "\n  No tasks yet.\n\n  Run `task-journal create \"<title>\"` to open one,\n  or `task-journal install-hooks --backfill` to import\n  existing Claude Code history.\n",
            )
            .style(Style::default().fg(Color::Yellow));
            frame.render_widget(msg, area);
            return;
        }

        let items: Vec<ListItem> = self
            .tasks
            .iter()
            .map(|t| {
                let status_glyph = if t.status == "open" { "○" } else { "✓" };
                let status_color = if t.status == "open" {
                    Color::Cyan
                } else {
                    Color::DarkGray
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {status_glyph} "),
                        Style::default().fg(status_color),
                    ),
                    Span::styled(
                        format!("{:<14}", truncate(&t.task_id, 14)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(" "),
                    Span::raw(truncate(&t.title, 60)),
                    Span::styled(
                        format!("  {} ev", t.event_count),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("  {}", short_date(&t.last_event_at)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::NONE))
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, area, &mut self.state);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let footer = Paragraph::new(Line::from(vec![
            Span::styled("↑↓/jk", Style::default().fg(Color::Cyan)),
            Span::raw(" navigate · "),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::raw(" open task · "),
            Span::styled("q", Style::default().fg(Color::Cyan)),
            Span::raw(" quit"),
        ]));
        frame.render_widget(footer, area);
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn short_date(iso: &str) -> String {
    // Display the date portion (YYYY-MM-DD) plus HH:MM if present.
    iso.chars().take(16).collect::<String>().replace('T', " ")
}

fn shorten_path(p: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().to_string();
        if let Some(rest) = p.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    p.to_string()
}
