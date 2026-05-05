//! Chat view screen — renders a session as a readable conversation.
#![allow(dead_code)] // Workaround for rustc 1.95 ICE in check_mod_deathness

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use tj_core::session::parser::*;

/// A rendered chat message ready for display.
#[derive(Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
    pub timestamp: String,
    pub tools: Vec<String>, // tool names used
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

pub struct ChatView {
    pub messages: Vec<ChatMessage>,
    pub scroll: u16,
    pub title: String,
    pub total_lines: u16,
}

impl ChatView {
    pub fn from_session(session: &ParsedSession) -> Self {
        let mut messages = Vec::new();

        for entry in &session.entries {
            match entry {
                SessionEntry::User(u) => {
                    if let Some(text) = extract_user_text(u) {
                        let clean = strip_xml_tags(&text);
                        if !clean.trim().is_empty() {
                            messages.push(ChatMessage {
                                role: Role::User,
                                text: clean,
                                timestamp: format_ts(&u.timestamp),
                                tools: vec![],
                            });
                        }
                    }
                }
                SessionEntry::Assistant(a) => {
                    let texts = extract_assistant_texts(a);
                    let tools = extract_tool_uses(a);
                    let tool_names: Vec<String> = tools.iter().map(|(n, _)| n.clone()).collect();

                    let combined = texts.join("\n");
                    if !combined.trim().is_empty() || !tool_names.is_empty() {
                        let display_text = if combined.trim().is_empty() {
                            format!("[{} tool call(s)]", tool_names.len())
                        } else {
                            combined
                        };
                        messages.push(ChatMessage {
                            role: Role::Assistant,
                            text: display_text,
                            timestamp: format_ts(&a.timestamp),
                            tools: tool_names,
                        });
                    }
                }
                _ => {}
            }
        }

        let title = if let Some(first) = session.first_user_text() {
            let clean = strip_xml_tags(&first);
            let line = clean.lines().find(|l| !l.trim().is_empty()).unwrap_or(&clean);
            truncate(line.trim(), 60)
        } else {
            format!("Session {}", &session.session_id[..8])
        };

        ChatView {
            messages,
            scroll: 0,
            title,
            total_lines: 0, // calculated during render
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
        // Will be clamped in render.
        self.scroll = u16::MAX;
    }

    pub fn render(&self, frame: &mut Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // header
                Constraint::Min(5),    // chat
                Constraint::Length(3), // footer
            ])
            .split(frame.area());

        self.render_header(frame, chunks[0]);
        self.render_chat(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = format!(
            " {} — {} messages",
            self.title,
            self.messages.len()
        );
        let block = Paragraph::new(title)
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(block, area);
    }

    fn render_chat(&self, frame: &mut Frame<'_>, area: Rect) {
        let width = area.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line<'_>> = Vec::new();

        for msg in &self.messages {
            // Role header.
            let (role_label, role_color) = match msg.role {
                Role::User => ("YOU", Color::Green),
                Role::Assistant => ("CLAUDE", Color::Blue),
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("─── {role_label} "),
                    Style::default().fg(role_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    &msg.timestamp,
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {}", "─".repeat(width.saturating_sub(role_label.len() + msg.timestamp.len() + 6))),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));

            // Tool badges.
            if !msg.tools.is_empty() {
                let tool_text = msg.tools.iter()
                    .take(5) // max 5 tools shown
                    .map(|t| format!("[{t}]"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let suffix = if msg.tools.len() > 5 {
                    format!(" +{}", msg.tools.len() - 5)
                } else {
                    String::new()
                };
                lines.push(Line::from(Span::styled(
                    format!("  {tool_text}{suffix}"),
                    Style::default().fg(Color::Yellow),
                )));
            }

            // Message text — word-wrapped.
            let text_color = match msg.role {
                Role::User => Color::White,
                Role::Assistant => Color::Gray,
            };
            for text_line in msg.text.lines() {
                // Simple word wrapping.
                let wrapped = word_wrap(text_line, width.saturating_sub(2));
                for wl in wrapped {
                    lines.push(Line::from(Span::styled(
                        format!("  {wl}"),
                        Style::default().fg(text_color),
                    )));
                }
            }

            // Empty line between messages.
            lines.push(Line::from(""));
        }

        let total = lines.len() as u16;
        let visible = area.height.saturating_sub(0);
        let max_scroll = total.saturating_sub(visible);
        let scroll = self.scroll.min(max_scroll);

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, area);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let help = Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(" scroll  "),
            Span::styled("PgUp/PgDn", Style::default().fg(Color::Yellow)),
            Span::raw(" page  "),
            Span::styled("Home/End", Style::default().fg(Color::Yellow)),
            Span::raw(" top/bottom  "),
            Span::styled("Esc/q", Style::default().fg(Color::Yellow)),
            Span::raw(" back"),
        ]);
        let block = Paragraph::new(help)
            .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(block, area);
    }
}

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current = word.to_string();
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
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

fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    }
}

fn format_ts(ts: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        dt.format("%H:%M:%S").to_string()
    } else if ts.len() >= 19 {
        ts[11..19].to_string()
    } else {
        ts.to_string()
    }
}
