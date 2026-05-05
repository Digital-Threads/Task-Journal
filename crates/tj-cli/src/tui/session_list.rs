//! Session list screen — shows all sessions for the project.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use tj_core::session::parser::ParsedSession;

pub struct SessionList {
    pub sessions: Vec<ParsedSession>,
    pub selected: Option<usize>,
    pub state: ListState,
}

impl SessionList {
    pub fn new(sessions: Vec<ParsedSession>) -> Self {
        let mut state = ListState::default();
        if !sessions.is_empty() {
            state.select(Some(0));
        }
        SessionList {
            selected: if sessions.is_empty() { None } else { Some(0) },
            sessions,
            state,
        }
    }

    pub fn next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.selected {
            Some(i) => {
                if i >= self.sessions.len() - 1 {
                    i
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.selected = Some(i);
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = match self.selected {
            Some(0) | None => 0,
            Some(i) => i - 1,
        };
        self.selected = Some(i);
        self.state.select(Some(i));
    }

    pub fn first(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = Some(0);
            self.state.select(Some(0));
        }
    }

    pub fn last(&mut self) {
        if !self.sessions.is_empty() {
            let last = self.sessions.len() - 1;
            self.selected = Some(last);
            self.state.select(Some(last));
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // header
                Constraint::Min(5),    // list
                Constraint::Length(3), // footer/help
            ])
            .split(frame.area());

        self.render_header(frame, chunks[0]);
        self.render_list(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = format!(
            " Task Journal — {} session{}",
            self.sessions.len(),
            if self.sessions.len() == 1 { "" } else { "s" }
        );
        let block = Paragraph::new(title)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(block, area);
    }

    fn render_list(&self, frame: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem<'_>> = self
            .sessions
            .iter()
            .map(|s| {
                let title = session_title(s);
                let date = format_date(&s.first_timestamp);
                let msgs = format!(
                    "{}u/{}a",
                    s.user_message_count(),
                    s.assistant_message_count()
                );
                let id_short = &s.session_id[..8.min(s.session_id.len())];

                let line = Line::from(vec![
                    Span::styled(
                        format!("{date} "),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{id_short} "),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("{msgs:>8} "),
                        Style::default().fg(Color::Green),
                    ),
                    Span::styled(title, Style::default().fg(Color::White)),
                ]);

                ListItem::new(line)
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

        let mut state = self.state.clone();
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let help = Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" navigate  "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" open  "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(" quit"),
        ]);
        let block = Paragraph::new(help)
            .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(block, area);
    }
}

fn session_title(s: &ParsedSession) -> String {
    if let Some(text) = s.first_user_text() {
        let clean = strip_xml_tags(&text);
        let line = clean.lines().find(|l| !l.trim().is_empty()).unwrap_or(&clean);
        let trimmed = line.trim();
        if trimmed.len() > 80 {
            format!("{}…", &trimmed[..80])
        } else {
            trimmed.to_string()
        }
    } else {
        format!("Session {}", &s.session_id[..8.min(s.session_id.len())])
    }
}

fn strip_xml_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    result
}

fn format_date(ts: &Option<String>) -> String {
    match ts {
        Some(ts) => {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
                dt.format("%Y-%m-%d %H:%M").to_string()
            } else {
                ts[..16.min(ts.len())].to_string()
            }
        }
        None => "????-??-?? ??:??".to_string(),
    }
}
