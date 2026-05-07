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
    pub project_path: String,
    // Search/filter state
    pub filter_text: String,
    pub filter_mode: bool,
    pub filtered_indices: Vec<usize>,
}

impl SessionList {
    pub fn new(sessions: Vec<ParsedSession>, project_path: String) -> Self {
        let mut state = ListState::default();
        if !sessions.is_empty() {
            state.select(Some(0));
        }
        let filtered_indices: Vec<usize> = (0..sessions.len()).collect();
        SessionList {
            selected: if sessions.is_empty() { None } else { Some(0) },
            sessions,
            state,
            project_path,
            filter_text: String::new(),
            filter_mode: false,
            filtered_indices,
        }
    }

    /// Enter search/filter mode.
    pub fn enter_filter_mode(&mut self) {
        self.filter_mode = true;
    }

    /// Exit search/filter mode and clear the filter.
    pub fn clear_filter(&mut self) {
        self.filter_mode = false;
        self.filter_text.clear();
        self.filtered_indices = (0..self.sessions.len()).collect();
        // Reset selection to first item
        if !self.filtered_indices.is_empty() {
            self.selected = Some(0);
            self.state.select(Some(0));
        } else {
            self.selected = None;
            self.state.select(None);
        }
    }

    /// Accept current filter and exit filter mode (keep filter active).
    pub fn accept_filter(&mut self) {
        self.filter_mode = false;
        // Keep filter_text and filtered_indices as-is
        if !self.filtered_indices.is_empty() {
            self.selected = Some(0);
            self.state.select(Some(0));
        } else {
            self.selected = None;
            self.state.select(None);
        }
    }

    /// Push a character into the filter and re-filter.
    pub fn filter_push(&mut self, ch: char) {
        self.filter_text.push(ch);
        self.apply_filter();
    }

    /// Remove the last character from the filter and re-filter.
    pub fn filter_pop(&mut self) {
        self.filter_text.pop();
        self.apply_filter();
    }

    /// Re-compute filtered_indices from filter_text.
    fn apply_filter(&mut self) {
        let query = self.filter_text.to_lowercase();
        self.filtered_indices = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                if query.is_empty() {
                    return true;
                }
                let title = session_title(s).to_lowercase();
                title.contains(&query)
            })
            .map(|(i, _)| i)
            .collect();

        // Reset selection
        if !self.filtered_indices.is_empty() {
            self.selected = Some(0);
            self.state.select(Some(0));
        } else {
            self.selected = None;
            self.state.select(None);
        }
    }

    /// Returns the actual session index for the current selection (maps through filter).
    pub fn selected_session_index(&self) -> Option<usize> {
        self.selected
            .and_then(|i| self.filtered_indices.get(i).copied())
    }

    pub fn next(&mut self) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let i = match self.selected {
            Some(i) => {
                if i >= self.filtered_indices.len() - 1 {
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
        if self.filtered_indices.is_empty() {
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
        if !self.filtered_indices.is_empty() {
            self.selected = Some(0);
            self.state.select(Some(0));
        }
    }

    pub fn last(&mut self) {
        if !self.filtered_indices.is_empty() {
            let last = self.filtered_indices.len() - 1;
            self.selected = Some(last);
            self.state.select(Some(last));
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        if self.filter_mode {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // search bar
                    Constraint::Min(5),    // list
                    Constraint::Length(3), // footer/help
                ])
                .split(frame.area());

            self.render_search_bar(frame, chunks[0]);
            self.render_list(frame, chunks[1]);
            self.render_footer(frame, chunks[2]);
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // header
                    Constraint::Min(5),    // list
                    Constraint::Length(3), // footer/help
                ])
                .split(frame.area());

            self.render_header(frame, chunks[0]);
            self.render_list(frame, chunks[1]);
            self.render_footer(frame, chunks[2]);
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let short_path = shorten_path(&self.project_path);
        let total = self.sessions.len();
        let showing = if self.filter_text.is_empty() {
            format!("{} session{}", total, if total == 1 { "" } else { "s" })
        } else {
            format!(
                "{}/{} session{}",
                self.filtered_indices.len(),
                total,
                if total == 1 { "" } else { "s" }
            )
        };
        let header = Line::from(vec![
            Span::styled(
                " Task Journal ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("— ", Style::default().fg(Color::DarkGray)),
            Span::styled(short_path, Style::default().fg(Color::White)),
            Span::styled(" — ", Style::default().fg(Color::DarkGray)),
            Span::styled(showing, Style::default().fg(Color::Cyan)),
        ]);
        let block = Paragraph::new(header).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(block, area);
    }

    fn render_search_bar(&self, frame: &mut Frame<'_>, area: Rect) {
        let match_count = format!(
            "{} match{}",
            self.filtered_indices.len(),
            if self.filtered_indices.len() == 1 {
                ""
            } else {
                "es"
            }
        );
        let search_line = Line::from(vec![
            Span::styled(
                " / ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(self.filter_text.clone(), Style::default().fg(Color::White)),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled(match_count, Style::default().fg(Color::DarkGray)),
        ]);
        let block = Paragraph::new(search_line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::Yellow)),
        );
        frame.render_widget(block, area);
    }

    fn render_list(&self, frame: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem<'_>> = self
            .filtered_indices
            .iter()
            .map(|&idx| {
                let s = &self.sessions[idx];
                let title = session_title(s);
                let date = format_date(&s.first_timestamp);
                let msgs = format!(
                    "{}u/{}a",
                    s.user_message_count(),
                    s.assistant_message_count()
                );
                let duration = format_duration(&s.first_timestamp, &s.last_timestamp);
                let id_short = &s.session_id[..8.min(s.session_id.len())];

                let line = Line::from(vec![
                    Span::styled(format!("{date} "), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{id_short} "), Style::default().fg(Color::Yellow)),
                    Span::styled(format!("{msgs:>8} "), Style::default().fg(Color::Green)),
                    Span::styled(
                        format!("{duration:>6} "),
                        Style::default().fg(Color::DarkGray),
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
        let help = if self.filter_mode {
            Line::from(vec![
                Span::styled(" Type", Style::default().fg(Color::Yellow)),
                Span::raw(" to filter  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" accept  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" clear"),
            ])
        } else {
            let mut spans = vec![
                Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
                Span::raw(" navigate  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" open  "),
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(" search  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ];
            if !self.filter_text.is_empty() {
                spans.push(Span::styled(
                    "  [filtered]",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        };
        let block = Paragraph::new(help).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(block, area);
    }
}

fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn session_title(s: &ParsedSession) -> String {
    if let Some(text) = s.first_user_text() {
        let clean = strip_xml_tags(&text);
        let line = clean
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or(&clean);
        truncate_with_ellipsis(line.trim(), 80)
    } else {
        let head: String = s.session_id.chars().take(8).collect();
        format!("Session {head}")
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

fn format_duration(first: &Option<String>, last: &Option<String>) -> String {
    let (Some(f), Some(l)) = (first, last) else {
        return "--".to_string();
    };
    let Ok(dt_first) = chrono::DateTime::parse_from_rfc3339(f) else {
        return "--".to_string();
    };
    let Ok(dt_last) = chrono::DateTime::parse_from_rfc3339(l) else {
        return "--".to_string();
    };
    let dur = dt_last.signed_duration_since(dt_first);
    let total_secs = dur.num_seconds().max(0);

    if total_secs < 60 {
        format!("{}s", total_secs)
    } else if total_secs < 3600 {
        format!("{}m", total_secs / 60)
    } else if total_secs < 86400 {
        let h = total_secs / 3600;
        let m = (total_secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h{}m", h, m)
        }
    } else {
        let d = total_secs / 86400;
        format!("{}d", d)
    }
}

fn shorten_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        parts.join("/")
    } else {
        parts[parts.len() - 2..].join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_with_ellipsis;

    #[test]
    fn truncate_ascii_under_limit_returns_input_as_is() {
        assert_eq!(truncate_with_ellipsis("hello", 80), "hello");
    }

    #[test]
    fn truncate_ascii_over_limit_appends_ellipsis() {
        let long = "a".repeat(100);
        let out = truncate_with_ellipsis(&long, 10);
        assert_eq!(out, format!("{}…", "a".repeat(10)));
    }

    #[test]
    fn truncate_cyrillic_does_not_panic_at_char_boundary() {
        let cyr = "Можешь ли ты проанализировать проект? Мне нужен вот md файл, то есть который описывает, вообще делает этот проект. Все полностью.";
        let out = truncate_with_ellipsis(cyr, 80);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 81);
    }

    #[test]
    fn truncate_emoji_counts_by_chars_not_bytes() {
        let s = "🦀".repeat(50);
        let out = truncate_with_ellipsis(&s, 10);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().filter(|c| *c == '🦀').count(), 10);
    }

    #[test]
    fn truncate_exact_length_no_ellipsis() {
        let s: String = (0..80).map(|_| 'x').collect();
        assert_eq!(truncate_with_ellipsis(&s, 80), s);
    }
}
