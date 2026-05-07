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
        // Collapse tool-only assistant messages. When the assistant
        // emits N consecutive turns of pure tool_use without any
        // commentary, we accumulate the tool names and attach them to
        // the *next* assistant message that does have text (or, if the
        // session ends mid-tool-storm, surface a single collapsed
        // entry so the activity is not invisible).
        let mut pending_tools: Vec<String> = Vec::new();
        let mut last_tool_ts: Option<String> = None;

        for entry in &session.entries {
            match entry {
                SessionEntry::User(u) => {
                    if !pending_tools.is_empty() {
                        let count = pending_tools.len();
                        messages.push(ChatMessage {
                            role: Role::Assistant,
                            text: format!("({count} tool call(s) — no commentary)"),
                            timestamp: last_tool_ts.take().unwrap_or_default(),
                            tools: std::mem::take(&mut pending_tools),
                        });
                    }
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

                    if combined.trim().is_empty() {
                        // Tool-only turn: accumulate, do not render yet.
                        if !tool_names.is_empty() {
                            pending_tools.extend(tool_names);
                            last_tool_ts = Some(format_ts(&a.timestamp));
                        }
                    } else {
                        // Text turn: flush accumulated tools onto this
                        // message together with its own.
                        let mut all_tools = std::mem::take(&mut pending_tools);
                        last_tool_ts = None;
                        all_tools.extend(tool_names);
                        messages.push(ChatMessage {
                            role: Role::Assistant,
                            text: combined,
                            timestamp: format_ts(&a.timestamp),
                            tools: all_tools,
                        });
                    }
                }
                _ => {}
            }
        }
        // Trailing tool-only burst: surface as one collapsed entry so
        // it's visible the assistant did work, but without polluting
        // the timeline with empty turns.
        if !pending_tools.is_empty() {
            let count = pending_tools.len();
            messages.push(ChatMessage {
                role: Role::Assistant,
                text: format!("({count} tool call(s) — no commentary)"),
                timestamp: last_tool_ts.unwrap_or_default(),
                tools: pending_tools,
            });
        }

        let title = if let Some(first) = session.first_user_text() {
            let clean = strip_xml_tags(&first);
            let line = clean
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(&clean);
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
                Constraint::Length(3), // header
                Constraint::Min(5),    // chat
                Constraint::Length(3), // footer
            ])
            .split(frame.area());

        self.render_header(frame, chunks[0]);
        self.render_chat(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = format!(" {} — {} messages", self.title, self.messages.len());
        let block = Paragraph::new(title)
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
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
                Span::styled(&msg.timestamp, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        " {}",
                        "─".repeat(
                            width.saturating_sub(role_label.len() + msg.timestamp.len() + 6)
                        )
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));

            // Tool badges.
            if !msg.tools.is_empty() {
                let tool_text = msg
                    .tools
                    .iter()
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
            Span::styled("Backspace/Esc/q", Style::default().fg(Color::Yellow)),
            Span::raw(" back"),
        ]);
        let block = Paragraph::new(help).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- word_wrap() tests ---

    #[test]
    fn word_wrap_empty_text() {
        let result = word_wrap("", 40);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn word_wrap_single_word_longer_than_width() {
        let result = word_wrap("superlongwordthatexceedswidth", 10);
        // Word cannot be broken, so it stays on its own line.
        assert_eq!(result, vec!["superlongwordthatexceedswidth"]);
    }

    #[test]
    fn word_wrap_normal_wrapping() {
        let result = word_wrap("hello world foo bar", 11);
        assert_eq!(result, vec!["hello world", "foo bar"]);
    }

    #[test]
    fn word_wrap_exact_fit() {
        let result = word_wrap("hello world", 11);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn word_wrap_one_char_over() {
        let result = word_wrap("hello world", 10);
        assert_eq!(result, vec!["hello", "world"]);
    }

    #[test]
    fn word_wrap_zero_width() {
        let result = word_wrap("hello world", 0);
        // Zero width returns original text as-is.
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn word_wrap_multiple_spaces_collapsed() {
        // split_whitespace collapses multiple spaces.
        let result = word_wrap("hello   world", 40);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn word_wrap_only_whitespace() {
        let result = word_wrap("   ", 40);
        // split_whitespace yields nothing, so we get a single empty string.
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn word_wrap_many_short_words() {
        let result = word_wrap("a b c d e f g h i j", 5);
        assert_eq!(result, vec!["a b c", "d e f", "g h i", "j"]);
    }

    // --- strip_xml_tags() tests ---

    #[test]
    fn strip_xml_tags_simple() {
        assert_eq!(strip_xml_tags("<b>bold</b>"), "bold");
    }

    #[test]
    fn strip_xml_tags_nested() {
        assert_eq!(strip_xml_tags("<div><span>inner</span></div>"), "inner");
    }

    #[test]
    fn strip_xml_tags_no_tags_passthrough() {
        assert_eq!(strip_xml_tags("no tags here"), "no tags here");
    }

    #[test]
    fn strip_xml_tags_only_tags_empty_result() {
        assert_eq!(strip_xml_tags("<tag></tag>"), "");
    }

    #[test]
    fn strip_xml_tags_self_closing() {
        assert_eq!(strip_xml_tags("before<br/>after"), "beforeafter");
    }

    #[test]
    fn strip_xml_tags_command_message() {
        let input = "<command-name>brainstorm</command-name><command-message>Design the new auth system</command-message>";
        let result = strip_xml_tags(input);
        assert_eq!(result, "brainstormDesign the new auth system");
    }

    #[test]
    fn strip_xml_tags_mixed_content() {
        assert_eq!(
            strip_xml_tags("Hello <b>world</b>, how are <i>you</i>?"),
            "Hello world, how are you?"
        );
    }

    // --- truncate() tests ---

    #[test]
    fn truncate_short_text() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_cuts_with_ellipsis() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_multibyte_utf8_cyrillic() {
        // "Привет" = 6 chars, 12 bytes (each Cyrillic char is 2 bytes).
        let text = "Привет мир";
        let result = truncate(text, 8);
        // 8 bytes = 4 cyrillic chars "Прив"
        assert!(result.ends_with('…'));
        assert!(result.starts_with("Прив"));
    }

    #[test]
    fn truncate_multibyte_utf8_emoji() {
        // Emoji is 4 bytes, so truncating at 3 bytes should back up to 0.
        let text = "\u{1F600}hello";
        let result = truncate(text, 3);
        // Can't fit the emoji (4 bytes), backs up to 0.
        assert_eq!(result, "…");
    }

    #[test]
    fn truncate_empty_string() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn truncate_single_ascii_char() {
        assert_eq!(truncate("x", 1), "x");
        assert_eq!(truncate("xy", 1), "x…");
    }

    // --- format_ts() tests ---

    #[test]
    fn format_ts_valid_rfc3339() {
        let result = format_ts("2026-01-15T14:30:45Z");
        assert_eq!(result, "14:30:45");
    }

    #[test]
    fn format_ts_valid_rfc3339_with_offset() {
        let result = format_ts("2026-01-15T14:30:45+04:00");
        assert_eq!(result, "14:30:45");
    }

    #[test]
    fn format_ts_valid_rfc3339_with_millis() {
        let result = format_ts("2026-01-15T14:30:45.123Z");
        assert_eq!(result, "14:30:45");
    }

    #[test]
    fn format_ts_invalid_but_long_enough() {
        // Not valid RFC3339 but >= 19 chars: extracts chars 11..19.
        let result = format_ts("2026-01-15 14:30:45 extra");
        assert_eq!(result, "14:30:45");
    }

    #[test]
    fn format_ts_short_string() {
        let result = format_ts("short");
        assert_eq!(result, "short");
    }

    #[test]
    fn format_ts_empty_string() {
        let result = format_ts("");
        assert_eq!(result, "");
    }

    #[test]
    fn format_ts_exactly_19_chars() {
        let result = format_ts("2026-01-15T14:30:45");
        // Not valid RFC3339 (missing timezone), so falls to the length check.
        // 19 chars >= 19, extracts [11..19] = "14:30:45"
        assert_eq!(result, "14:30:45");
    }
}
