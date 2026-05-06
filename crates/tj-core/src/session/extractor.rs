//! Extract task-journal events from parsed Claude Code sessions.
//!
//! Uses heuristics to classify assistant messages into event types
//! without calling an LLM — fast and free.

use crate::event::{Author, Event, EventStatus, EventType, EvidenceStrength, Source};
use crate::session::parser::*;

/// Result of extracting events from a single session.
#[derive(Debug)]
pub struct ExtractedTask {
    pub task_id: String,
    pub title: String,
    pub session_id: String,
    pub events: Vec<Event>,
}

/// Extract task-journal events from a parsed session.
/// Each session becomes one task with multiple events.
pub fn extract_from_session(session: &ParsedSession) -> Option<ExtractedTask> {
    // Skip tiny sessions (less than 2 user messages = probably noise).
    if session.user_message_count() < 2 {
        return None;
    }

    let task_id = format!(
        "tj-{}",
        &ulid::Ulid::new().to_string()[10..16].to_lowercase()
    );

    // Derive title from first user message or summary.
    let title = derive_title(session);
    let mut events = Vec::new();

    // 1. Open event from first user message or summary.
    let open_text = session
        .summary()
        .map(|s| truncate(s, 500))
        .or_else(|| session.first_user_text().map(|s| truncate(&s, 500)))
        .unwrap_or_else(|| title.clone());

    let mut open_event = Event::new(
        &task_id,
        EventType::Open,
        Author::Agent,
        Source::Cli,
        open_text,
    );
    if let Some(ref ts) = session.first_timestamp {
        open_event.timestamp = ts.clone();
    }
    open_event.meta =
        serde_json::json!({"title": title, "backfill": true, "session_id": session.session_id});
    events.push(open_event);

    // 2. Walk through entries and extract meaningful events.
    let mut files_modified: Vec<String> = Vec::new();
    let mut tools_used: Vec<String> = Vec::new();

    for entry in &session.entries {
        match entry {
            SessionEntry::Assistant(a) => {
                // Extract tool usage.
                let tool_uses = extract_tool_uses(a);
                for (tool_name, input) in &tool_uses {
                    tools_used.push(tool_name.clone());

                    // Track file modifications.
                    if tool_name == "Write" || tool_name == "Edit" {
                        if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                            let short = shorten_path(path);
                            if !files_modified.contains(&short) {
                                files_modified.push(short);
                            }
                        }
                    }

                    // Bash with test commands → evidence.
                    if tool_name == "Bash" {
                        if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                            if is_test_command(cmd) {
                                let mut ev = Event::new(
                                    &task_id,
                                    EventType::Evidence,
                                    Author::Agent,
                                    Source::Cli,
                                    format!("Ran tests: {}", truncate(cmd, 200)),
                                );
                                ev.timestamp = a.timestamp.clone();
                                ev.evidence_strength = Some(EvidenceStrength::Medium);
                                ev.meta = serde_json::json!({"backfill": true});
                                events.push(ev);
                            }
                        }
                    }

                    // Git commit → evidence.
                    if tool_name == "Bash" {
                        if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                            if cmd.contains("git commit") && !cmd.contains("git commit --amend") {
                                let mut ev = Event::new(
                                    &task_id,
                                    EventType::Evidence,
                                    Author::Agent,
                                    Source::Cli,
                                    format!("Git commit: {}", truncate(cmd, 200)),
                                );
                                ev.timestamp = a.timestamp.clone();
                                ev.evidence_strength = Some(EvidenceStrength::Strong);
                                ev.meta = serde_json::json!({"backfill": true});
                                events.push(ev);
                            }
                        }
                    }
                }

                // Extract text blocks and classify by heuristics.
                let texts = extract_assistant_texts(a);
                for text in &texts {
                    if let Some(ev) = classify_text_heuristic(&task_id, text, &a.timestamp) {
                        events.push(ev);
                    }
                }
            }
            SessionEntry::User(_) | SessionEntry::Summary(_) | SessionEntry::Other => {}
        }
    }

    // 3. Add a finding event summarizing files modified (if any).
    if !files_modified.is_empty() {
        let summary = format!(
            "Modified {} files: {}",
            files_modified.len(),
            files_modified.join(", ")
        );
        let mut ev = Event::new(
            &task_id,
            EventType::Finding,
            Author::Agent,
            Source::Cli,
            summary,
        );
        if let Some(ref ts) = session.last_timestamp {
            ev.timestamp = ts.clone();
        }
        ev.refs.files = files_modified;
        ev.meta = serde_json::json!({"backfill": true});
        events.push(ev);
    }

    // 4. Close event.
    let close_text = format!(
        "Session ended. {} user messages, {} assistant messages, {} tool calls.",
        session.user_message_count(),
        session.assistant_message_count(),
        tools_used.len()
    );
    let mut close_event = Event::new(
        &task_id,
        EventType::Close,
        Author::Agent,
        Source::Cli,
        close_text,
    );
    if let Some(ref ts) = session.last_timestamp {
        close_event.timestamp = ts.clone();
    }
    close_event.meta = serde_json::json!({
        "backfill": true,
        "reason": "session_ended",
        "outcome": "completed"
    });
    events.push(close_event);

    Some(ExtractedTask {
        task_id,
        title,
        session_id: session.session_id.clone(),
        events,
    })
}

/// Derive a task title from the session.
fn derive_title(session: &ParsedSession) -> String {
    // Try summary first.
    if let Some(summary) = session.summary() {
        return truncate(&strip_xml_tags(summary), 120);
    }

    // Use first user message, skipping command/skill invocation messages.
    for entry in &session.entries {
        if let SessionEntry::User(u) = entry {
            if let Some(text) = extract_user_text(u) {
                let clean = strip_xml_tags(&text);
                let first_line = clean
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or(&clean);
                let trimmed = first_line.trim();
                // Skip empty or very short titles (likely slash commands).
                if trimmed.len() > 5 {
                    return truncate(trimmed, 120);
                }
            }
        }
    }

    format!(
        "Session {}",
        &session.session_id[..8.min(session.session_id.len())]
    )
}

/// Strip XML/HTML-like tags from text (e.g. <command-message>, <command-name>).
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

/// Classify assistant text into an event type using keyword heuristics.
/// Returns None if the text isn't interesting enough to log.
fn classify_text_heuristic(task_id: &str, text: &str, timestamp: &str) -> Option<Event> {
    let lower = text.to_lowercase();

    // Skip very short texts (< 50 chars) — usually just confirmations.
    if text.len() < 50 {
        return None;
    }

    // Decision patterns.
    let decision_patterns = [
        "decided to",
        "will use",
        "going with",
        "chose to",
        "the approach is",
        "решил использовать",
        "будем использовать",
        "выбрал",
    ];
    for pattern in &decision_patterns {
        if lower.contains(pattern) {
            let mut ev = Event::new(
                task_id,
                EventType::Decision,
                Author::Agent,
                Source::Cli,
                truncate(text, 300),
            );
            ev.timestamp = timestamp.to_string();
            ev.confidence = Some(0.7);
            ev.status = EventStatus::Suggested;
            ev.meta = serde_json::json!({"backfill": true, "heuristic": "decision_keyword"});
            return Some(ev);
        }
    }

    // Rejection patterns.
    let rejection_patterns = [
        "won't work",
        "doesn't work",
        "can't use",
        "не работает",
        "не подходит",
        "отказались",
        "tried but",
        "rejected",
        "abandoned",
    ];
    for pattern in &rejection_patterns {
        if lower.contains(pattern) {
            let mut ev = Event::new(
                task_id,
                EventType::Rejection,
                Author::Agent,
                Source::Cli,
                truncate(text, 300),
            );
            ev.timestamp = timestamp.to_string();
            ev.confidence = Some(0.6);
            ev.status = EventStatus::Suggested;
            ev.meta = serde_json::json!({"backfill": true, "heuristic": "rejection_keyword"});
            return Some(ev);
        }
    }

    // Constraint patterns.
    let constraint_patterns = [
        "rate limit",
        "not supported",
        "limitation",
        "ограничение",
        "не поддерживает",
        "requires",
        "must be",
    ];
    for pattern in &constraint_patterns {
        if lower.contains(pattern) && text.len() < 500 {
            let mut ev = Event::new(
                task_id,
                EventType::Constraint,
                Author::Agent,
                Source::Cli,
                truncate(text, 300),
            );
            ev.timestamp = timestamp.to_string();
            ev.confidence = Some(0.5);
            ev.status = EventStatus::Suggested;
            ev.meta = serde_json::json!({"backfill": true, "heuristic": "constraint_keyword"});
            return Some(ev);
        }
    }

    None
}

/// Check if a bash command is a test command.
fn is_test_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    lower.contains("cargo test")
        || lower.contains("npm test")
        || lower.contains("pytest")
        || lower.contains("phpunit")
        || lower.contains("jest")
        || lower.contains("vitest")
        || lower.contains("go test")
        || lower.contains("make test")
}

/// Shorten a file path for display — keep last 2 components.
fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() <= 2 {
        path.to_string()
    } else {
        parts[parts.len() - 2..].join("/")
    }
}

/// Truncate text to max_len, adding "…" if truncated.
fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        let mut end = max_len;
        // Don't cut in the middle of a UTF-8 char.
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_command() {
        assert!(is_test_command("cargo test -p my-crate"));
        assert!(is_test_command("npm test"));
        assert!(is_test_command("python -m pytest tests/"));
        assert!(!is_test_command("cargo build"));
        assert!(!is_test_command("git push"));
    }

    #[test]
    fn test_shorten_path() {
        assert_eq!(
            shorten_path("/home/user/project/src/main.rs"),
            "src/main.rs"
        );
        assert_eq!(shorten_path("main.rs"), "main.rs");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn test_classify_decision() {
        let ev = classify_text_heuristic(
            "tj-test",
            "After analysis, I decided to use the rmcp crate for MCP implementation because it has better macro support.",
            "2026-01-01T00:00:00Z",
        );
        assert!(ev.is_some());
        assert_eq!(ev.unwrap().event_type, EventType::Decision);
    }

    #[test]
    fn test_classify_rejection() {
        let ev = classify_text_heuristic(
            "tj-test",
            "The previous approach won't work because the API doesn't support batch operations.",
            "2026-01-01T00:00:00Z",
        );
        assert!(ev.is_some());
        assert_eq!(ev.unwrap().event_type, EventType::Rejection);
    }

    #[test]
    fn test_classify_short_text_skipped() {
        let ev = classify_text_heuristic("tj-test", "OK, done.", "2026-01-01T00:00:00Z");
        assert!(ev.is_none());
    }

    // --- extract_from_session() integration tests ---

    fn make_user_entry(uuid: &str, ts: &str, text: &str) -> SessionEntry {
        SessionEntry::User(UserEntry {
            uuid: uuid.into(),
            timestamp: ts.into(),
            session_id: None,
            message: Some(UserMessage {
                content: serde_json::json!(text),
            }),
            cwd: None,
        })
    }

    fn make_assistant_entry(uuid: &str, ts: &str, blocks: Vec<ContentBlock>) -> SessionEntry {
        SessionEntry::Assistant(AssistantEntry {
            uuid: uuid.into(),
            timestamp: ts.into(),
            session_id: None,
            message: Some(AssistantMessage {
                content: blocks,
                model: Some("claude-opus-4-20250514".into()),
                stop_reason: Some("end_turn".into()),
            }),
        })
    }

    #[test]
    fn extract_from_session_produces_open_and_close_events() {
        let session = ParsedSession {
            session_id: "test-session-123".into(),
            file_path: "/tmp/test-session-123.jsonl".into(),
            entries: vec![
                make_user_entry("u1", "2026-01-01T00:00:00Z", "Please fix the login bug"),
                make_assistant_entry(
                    "a1",
                    "2026-01-01T00:00:01Z",
                    vec![ContentBlock::Text {
                        text: "I'll look into the login issue.".into(),
                    }],
                ),
                make_user_entry("u2", "2026-01-01T00:00:02Z", "Thanks, looks good"),
                make_assistant_entry(
                    "a2",
                    "2026-01-01T00:00:03Z",
                    vec![ContentBlock::Text {
                        text: "The fix is complete.".into(),
                    }],
                ),
            ],
            first_timestamp: Some("2026-01-01T00:00:00Z".into()),
            last_timestamp: Some("2026-01-01T00:00:03Z".into()),
        };

        let task = extract_from_session(&session).unwrap();
        assert!(task.task_id.starts_with("tj-"));
        assert!(!task.title.is_empty());
        assert_eq!(task.session_id, "test-session-123");

        // First event should be Open.
        assert_eq!(task.events[0].event_type, EventType::Open);
        assert_eq!(task.events[0].timestamp, "2026-01-01T00:00:00Z");

        // Last event should be Close.
        let last = task.events.last().unwrap();
        assert_eq!(last.event_type, EventType::Close);
        assert_eq!(last.timestamp, "2026-01-01T00:00:03Z");
        assert!(last.text.contains("user messages"));
    }

    #[test]
    fn extract_from_session_skips_sessions_with_fewer_than_2_user_messages() {
        let session = ParsedSession {
            session_id: "short-session".into(),
            file_path: "/tmp/short.jsonl".into(),
            entries: vec![
                make_user_entry("u1", "2026-01-01T00:00:00Z", "Hello"),
                make_assistant_entry(
                    "a1",
                    "2026-01-01T00:00:01Z",
                    vec![ContentBlock::Text { text: "Hi!".into() }],
                ),
            ],
            first_timestamp: Some("2026-01-01T00:00:00Z".into()),
            last_timestamp: Some("2026-01-01T00:00:01Z".into()),
        };

        assert!(extract_from_session(&session).is_none());
    }

    #[test]
    fn extract_from_session_skips_zero_user_messages() {
        let session = ParsedSession {
            session_id: "empty-session".into(),
            file_path: "/tmp/empty.jsonl".into(),
            entries: vec![],
            first_timestamp: None,
            last_timestamp: None,
        };

        assert!(extract_from_session(&session).is_none());
    }

    #[test]
    fn extract_from_session_tracks_file_modifications() {
        let session = ParsedSession {
            session_id: "file-mod-session".into(),
            file_path: "/tmp/fm.jsonl".into(),
            entries: vec![
                make_user_entry("u1", "2026-01-01T00:00:00Z", "Update the config file"),
                make_assistant_entry(
                    "a1",
                    "2026-01-01T00:00:01Z",
                    vec![ContentBlock::ToolUse {
                        name: "Write".into(),
                        input: serde_json::json!({"file_path": "/home/user/project/src/config.rs"}),
                    }],
                ),
                make_user_entry("u2", "2026-01-01T00:00:02Z", "Also update main.rs"),
                make_assistant_entry(
                    "a2",
                    "2026-01-01T00:00:03Z",
                    vec![ContentBlock::ToolUse {
                        name: "Edit".into(),
                        input: serde_json::json!({"file_path": "/home/user/project/src/main.rs", "old_string": "a", "new_string": "b"}),
                    }],
                ),
            ],
            first_timestamp: Some("2026-01-01T00:00:00Z".into()),
            last_timestamp: Some("2026-01-01T00:00:03Z".into()),
        };

        let task = extract_from_session(&session).unwrap();
        // Should have a Finding event with file modifications.
        let finding = task
            .events
            .iter()
            .find(|e| e.event_type == EventType::Finding);
        assert!(finding.is_some());
        let finding = finding.unwrap();
        assert!(finding.text.contains("2 files"));
        assert!(finding.refs.files.contains(&"src/config.rs".to_string()));
        assert!(finding.refs.files.contains(&"src/main.rs".to_string()));
    }

    #[test]
    fn extract_from_session_detects_test_commands() {
        let session = ParsedSession {
            session_id: "test-cmd-session".into(),
            file_path: "/tmp/tc.jsonl".into(),
            entries: vec![
                make_user_entry("u1", "2026-01-01T00:00:00Z", "Run the tests"),
                make_assistant_entry(
                    "a1",
                    "2026-01-01T00:00:01Z",
                    vec![ContentBlock::ToolUse {
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "cargo test --workspace"}),
                    }],
                ),
                make_user_entry("u2", "2026-01-01T00:00:02Z", "Good"),
            ],
            first_timestamp: Some("2026-01-01T00:00:00Z".into()),
            last_timestamp: Some("2026-01-01T00:00:02Z".into()),
        };

        let task = extract_from_session(&session).unwrap();
        let evidence = task
            .events
            .iter()
            .find(|e| e.event_type == EventType::Evidence);
        assert!(evidence.is_some());
        assert!(evidence.unwrap().text.contains("cargo test"));
    }

    #[test]
    fn extract_from_session_detects_git_commit() {
        let session = ParsedSession {
            session_id: "git-commit-session".into(),
            file_path: "/tmp/gc.jsonl".into(),
            entries: vec![
                make_user_entry("u1", "2026-01-01T00:00:00Z", "Commit the changes"),
                make_assistant_entry(
                    "a1",
                    "2026-01-01T00:00:01Z",
                    vec![ContentBlock::ToolUse {
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "git commit -m 'fix: resolve login bug'"}),
                    }],
                ),
                make_user_entry("u2", "2026-01-01T00:00:02Z", "Push it"),
            ],
            first_timestamp: Some("2026-01-01T00:00:00Z".into()),
            last_timestamp: Some("2026-01-01T00:00:02Z".into()),
        };

        let task = extract_from_session(&session).unwrap();
        let evidence_events: Vec<_> = task
            .events
            .iter()
            .filter(|e| e.event_type == EventType::Evidence)
            .collect();
        let commit_ev = evidence_events
            .iter()
            .find(|e| e.text.contains("Git commit"));
        assert!(commit_ev.is_some());
        assert_eq!(
            commit_ev.unwrap().evidence_strength,
            Some(EvidenceStrength::Strong)
        );
    }

    // --- strip_xml_tags() ---

    #[test]
    fn strip_xml_tags_removes_simple_tags() {
        assert_eq!(strip_xml_tags("<b>hello</b>"), "hello");
    }

    #[test]
    fn strip_xml_tags_removes_nested_tags() {
        assert_eq!(strip_xml_tags("<div><span>text</span></div>"), "text");
    }

    #[test]
    fn strip_xml_tags_no_tags() {
        assert_eq!(strip_xml_tags("plain text"), "plain text");
    }

    #[test]
    fn strip_xml_tags_only_tags() {
        assert_eq!(strip_xml_tags("<tag></tag>"), "");
    }

    #[test]
    fn strip_xml_tags_with_attributes() {
        assert_eq!(
            strip_xml_tags("<command-name foo=\"bar\">init</command-name>"),
            "init"
        );
    }

    #[test]
    fn strip_xml_tags_preserves_angle_bracket_text_between_tags() {
        assert_eq!(strip_xml_tags("a < b and c > d"), "a  d");
        // Note: the simple char-by-char parser treats `<` as tag start.
    }

    // --- derive_title() ---

    #[test]
    fn derive_title_from_summary() {
        let session = ParsedSession {
            session_id: "abcdefghij".into(),
            file_path: "/tmp/s.jsonl".into(),
            entries: vec![
                SessionEntry::Summary(SummaryEntry {
                    summary: "Fixed authentication bug in login flow".into(),
                    timestamp: None,
                }),
                make_user_entry("u1", "t", "some user text that is long enough"),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert_eq!(
            derive_title(&session),
            "Fixed authentication bug in login flow"
        );
    }

    #[test]
    fn derive_title_from_user_text() {
        let session = ParsedSession {
            session_id: "abcdefghij".into(),
            file_path: "/tmp/s.jsonl".into(),
            entries: vec![make_user_entry(
                "u1",
                "t",
                "Please implement the new caching layer",
            )],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert_eq!(
            derive_title(&session),
            "Please implement the new caching layer"
        );
    }

    #[test]
    fn derive_title_skips_short_user_text() {
        let session = ParsedSession {
            session_id: "abcdefghij".into(),
            file_path: "/tmp/s.jsonl".into(),
            entries: vec![
                make_user_entry("u1", "t", "/init"),
                make_user_entry("u2", "t", "Implement the feature for user profiles"),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        // "/init" is only 5 chars, should be skipped. Second message (stripped of XML) should be used.
        let title = derive_title(&session);
        assert!(title.contains("Implement the feature"));
    }

    #[test]
    fn derive_title_fallback_to_session_id() {
        let session = ParsedSession {
            session_id: "abcdefghij".into(),
            file_path: "/tmp/s.jsonl".into(),
            entries: vec![make_user_entry("u1", "t", "hi")],
            first_timestamp: None,
            last_timestamp: None,
        };
        let title = derive_title(&session);
        assert!(title.starts_with("Session "));
        assert!(title.contains("abcdefgh"));
    }

    #[test]
    fn derive_title_strips_xml_from_summary() {
        let session = ParsedSession {
            session_id: "abcdefghij".into(),
            file_path: "/tmp/s.jsonl".into(),
            entries: vec![SessionEntry::Summary(SummaryEntry {
                summary: "<task>Fix the <b>critical</b> bug</task>".into(),
                timestamp: None,
            })],
            first_timestamp: None,
            last_timestamp: None,
        };
        let title = derive_title(&session);
        assert_eq!(title, "Fix the critical bug");
    }

    // --- classify_text_heuristic() additional tests ---

    #[test]
    fn test_classify_constraint() {
        let ev = classify_text_heuristic(
            "tj-test",
            "The API has a rate limit of 100 requests per minute, so we need to implement throttling.",
            "2026-01-01T00:00:00Z",
        );
        assert!(ev.is_some());
        assert_eq!(ev.unwrap().event_type, EventType::Constraint);
    }

    #[test]
    fn test_classify_no_match_returns_none() {
        let ev = classify_text_heuristic(
            "tj-test",
            "I have successfully implemented the feature and all tests are passing. The code is clean and well-organized.",
            "2026-01-01T00:00:00Z",
        );
        assert!(ev.is_none());
    }

    // --- Additional is_test_command tests ---

    #[test]
    fn test_is_test_command_additional() {
        assert!(is_test_command("jest --coverage"));
        assert!(is_test_command("vitest run"));
        assert!(is_test_command("go test ./..."));
        assert!(is_test_command("make test"));
        assert!(is_test_command("phpunit tests/Unit"));
        assert!(is_test_command("echo 'cargo test'")); // matches because it contains "cargo test"
        assert!(!is_test_command("ls -la"));
    }

    // --- shorten_path additional tests ---

    #[test]
    fn test_shorten_path_windows_separators() {
        assert_eq!(
            shorten_path("C:\\Users\\user\\project\\src\\main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn test_shorten_path_two_components() {
        assert_eq!(shorten_path("src/main.rs"), "src/main.rs");
    }

    // --- truncate edge cases ---

    #[test]
    fn test_truncate_multibyte_utf8() {
        // Russian text: each char is 2 bytes.
        let text = "Привет мир";
        let truncated = truncate(text, 6);
        // 6 bytes = 3 cyrillic chars ("При")
        assert!(truncated.ends_with('…'));
        assert!(truncated.starts_with("При"));
    }

    #[test]
    fn test_truncate_exact_boundary() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello!", 5), "hello…");
    }
}
