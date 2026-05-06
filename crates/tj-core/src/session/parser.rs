//! Parser for Claude Code session JSONL files.
//!
//! Each line is a JSON object with `type` field determining its structure.
//! We only care about `user` and `assistant` entries for backfill.

use serde::Deserialize;

/// Top-level entry in a Claude Code session JSONL file.
/// Uses untagged enum because the `type` field values don't map cleanly to Rust enum variants.
#[derive(Debug, Clone)]
pub enum SessionEntry {
    User(UserEntry),
    Assistant(AssistantEntry),
    Summary(SummaryEntry),
    /// Entries we don't need for backfill (attachment, system, queue-operation, last-prompt).
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserEntry {
    pub uuid: String,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub message: Option<UserMessage>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserMessage {
    pub content: serde_json::Value, // String or array of content blocks
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantEntry {
    pub uuid: String,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub message: Option<AssistantMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        #[serde(default)]
        content: serde_json::Value,
    },
    Thinking {
        #[serde(default)]
        thinking: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SummaryEntry {
    pub summary: String,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// A parsed session with all meaningful entries extracted.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    pub session_id: String,
    pub file_path: String,
    pub entries: Vec<SessionEntry>,
    pub first_timestamp: Option<String>,
    pub last_timestamp: Option<String>,
}

impl ParsedSession {
    /// Extract the first user message text as a potential task title.
    pub fn first_user_text(&self) -> Option<String> {
        for entry in &self.entries {
            if let SessionEntry::User(u) = entry {
                let text = extract_user_text(u)?;
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }

    /// Extract session summary if present.
    pub fn summary(&self) -> Option<&str> {
        for entry in &self.entries {
            if let SessionEntry::Summary(s) = entry {
                return Some(&s.summary);
            }
        }
        None
    }

    /// Count user messages.
    pub fn user_message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, SessionEntry::User(_)))
            .count()
    }

    /// Count assistant messages.
    pub fn assistant_message_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e, SessionEntry::Assistant(_)))
            .count()
    }
}

/// Parse a Claude Code session JSONL file into structured entries.
pub fn parse_session(path: &std::path::Path) -> anyhow::Result<ParsedSession> {
    let file_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);

    let mut entries = Vec::new();
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;

    use std::io::BufRead;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let raw: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };

        let entry_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = raw
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let Some(ref ts) = timestamp {
            if first_ts.is_none() {
                first_ts = Some(ts.clone());
            }
            last_ts = Some(ts.clone());
        }

        let entry = match entry_type {
            "user" => match serde_json::from_value::<UserEntry>(raw) {
                Ok(u) => SessionEntry::User(u),
                Err(_) => SessionEntry::Other,
            },
            "assistant" => match serde_json::from_value::<AssistantEntry>(raw) {
                Ok(a) => SessionEntry::Assistant(a),
                Err(_) => SessionEntry::Other,
            },
            "summary" => match serde_json::from_value::<SummaryEntry>(raw) {
                Ok(s) => SessionEntry::Summary(s),
                Err(_) => SessionEntry::Other,
            },
            _ => SessionEntry::Other,
        };

        // Only keep meaningful entries.
        if !matches!(entry, SessionEntry::Other) {
            entries.push(entry);
        }
    }

    Ok(ParsedSession {
        session_id: file_name,
        file_path: path.to_string_lossy().into_owned(),
        entries,
        first_timestamp: first_ts,
        last_timestamp: last_ts,
    })
}

/// Extract text content from a user message.
/// Content can be a plain string or an array of content blocks.
pub fn extract_user_text(entry: &UserEntry) -> Option<String> {
    let msg = entry.message.as_ref()?;
    match &msg.content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(arr) => {
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|block| {
                    if block.get("type")?.as_str()? == "text" {
                        block.get("text")?.as_str()
                    } else {
                        None
                    }
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => None,
    }
}

/// Extract all text blocks from an assistant message.
pub fn extract_assistant_texts(entry: &AssistantEntry) -> Vec<String> {
    let Some(msg) = &entry.message else {
        return vec![];
    };
    msg.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Extract tool_use calls from an assistant message.
pub fn extract_tool_uses(entry: &AssistantEntry) -> Vec<(String, serde_json::Value)> {
    let Some(msg) = &entry.message else {
        return vec![];
    };
    msg.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, input } => Some((name.clone(), input.clone())),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_string_content() {
        let json = r#"{"type":"user","uuid":"abc","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello world"}}"#;
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let entry: UserEntry = serde_json::from_value(raw).unwrap();
        let text = extract_user_text(&entry).unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn parse_user_array_content() {
        let json = r#"{"type":"user","uuid":"abc","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"fix the bug"}]}}"#;
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let entry: UserEntry = serde_json::from_value(raw).unwrap();
        let text = extract_user_text(&entry).unwrap();
        assert_eq!(text, "fix the bug");
    }

    #[test]
    fn parse_assistant_with_tool_use() {
        let json = r#"{"type":"assistant","uuid":"def","timestamp":"2026-01-01T00:00:01Z","message":{"content":[{"type":"text","text":"Let me check"},{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/x"}}]}}"#;
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let entry: AssistantEntry = serde_json::from_value(raw).unwrap();
        let texts = extract_assistant_texts(&entry);
        assert_eq!(texts, vec!["Let me check"]);
        let tools = extract_tool_uses(&entry);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0, "Read");
    }

    // --- parse_session() with tempfile ---

    #[test]
    fn parse_session_with_valid_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc123.jsonl");
        let lines = vec![
            r#"{"type":"user","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello"}}"#,
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-01-01T00:00:01Z","message":{"content":[{"type":"text","text":"hi there"}]}}"#,
            r#"{"type":"summary","summary":"This session was about greeting.","timestamp":"2026-01-01T00:00:02Z"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.session_id, "abc123");
        assert_eq!(session.entries.len(), 3);
        assert_eq!(session.first_timestamp.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(session.last_timestamp.as_deref(), Some("2026-01-01T00:00:02Z"));
    }

    #[test]
    fn parse_session_skips_empty_and_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        let lines = vec![
            "",
            "not-json-at-all",
            r#"{"type":"user","uuid":"u1","timestamp":"2026-01-01T00:00:00Z","message":{"content":"valid"}}"#,
            "   ",
            r#"{"type":"unknown_type","data":"ignored"}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let session = parse_session(&path).unwrap();
        // Only the valid user entry should be kept; unknown types are SessionEntry::Other and filtered out.
        assert_eq!(session.entries.len(), 1);
    }

    #[test]
    fn parse_session_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();

        let session = parse_session(&path).unwrap();
        assert!(session.entries.is_empty());
        assert!(session.first_timestamp.is_none());
        assert!(session.last_timestamp.is_none());
    }

    #[test]
    fn parse_session_nonexistent_file() {
        let result = parse_session(std::path::Path::new("/nonexistent/path.jsonl"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_session_session_id_from_filename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my-session-id.jsonl");
        std::fs::write(&path, "").unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.session_id, "my-session-id");
    }

    // --- ParsedSession::first_user_text() edge cases ---

    #[test]
    fn first_user_text_returns_none_when_no_users() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::Assistant(AssistantEntry {
                    uuid: "a1".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    message: Some(AssistantMessage {
                        content: vec![ContentBlock::Text { text: "hello".into() }],
                        model: None,
                        stop_reason: None,
                    }),
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert!(session.first_user_text().is_none());
    }

    #[test]
    fn first_user_text_skips_empty_messages() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::User(UserEntry {
                    uuid: "u1".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    message: Some(UserMessage {
                        content: serde_json::json!("   "),
                    }),
                    cwd: None,
                }),
                SessionEntry::User(UserEntry {
                    uuid: "u2".into(),
                    timestamp: "2026-01-01T00:00:01Z".into(),
                    session_id: None,
                    message: Some(UserMessage {
                        content: serde_json::json!("actual text"),
                    }),
                    cwd: None,
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert_eq!(session.first_user_text().unwrap(), "actual text");
    }

    #[test]
    fn first_user_text_with_xml_tagged_content() {
        // first_user_text does NOT strip XML tags — it returns raw text.
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::User(UserEntry {
                    uuid: "u1".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    message: Some(UserMessage {
                        content: serde_json::json!("<command-name>init</command-name> Setup project"),
                    }),
                    cwd: None,
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        let text = session.first_user_text().unwrap();
        assert!(text.contains("<command-name>"));
        assert!(text.contains("Setup project"));
    }

    #[test]
    fn first_user_text_no_message() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::User(UserEntry {
                    uuid: "u1".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    message: None,
                    cwd: None,
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert!(session.first_user_text().is_none());
    }

    // --- ParsedSession::summary() ---

    #[test]
    fn summary_returns_none_when_no_summary_entry() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::User(UserEntry {
                    uuid: "u1".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    session_id: None,
                    message: None,
                    cwd: None,
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert!(session.summary().is_none());
    }

    #[test]
    fn summary_returns_first_summary_text() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::Summary(SummaryEntry {
                    summary: "Worked on tests".into(),
                    timestamp: Some("2026-01-01T00:00:00Z".into()),
                }),
                SessionEntry::Summary(SummaryEntry {
                    summary: "Second summary ignored".into(),
                    timestamp: None,
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert_eq!(session.summary().unwrap(), "Worked on tests");
    }

    // --- user_message_count / assistant_message_count ---

    #[test]
    fn message_counts() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![
                SessionEntry::User(UserEntry {
                    uuid: "u1".into(),
                    timestamp: "t".into(),
                    session_id: None,
                    message: None,
                    cwd: None,
                }),
                SessionEntry::User(UserEntry {
                    uuid: "u2".into(),
                    timestamp: "t".into(),
                    session_id: None,
                    message: None,
                    cwd: None,
                }),
                SessionEntry::Assistant(AssistantEntry {
                    uuid: "a1".into(),
                    timestamp: "t".into(),
                    session_id: None,
                    message: None,
                }),
                SessionEntry::Summary(SummaryEntry {
                    summary: "s".into(),
                    timestamp: None,
                }),
            ],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert_eq!(session.user_message_count(), 2);
        assert_eq!(session.assistant_message_count(), 1);
    }

    #[test]
    fn message_counts_empty_session() {
        let session = ParsedSession {
            session_id: "s1".into(),
            file_path: "/tmp/s1.jsonl".into(),
            entries: vec![],
            first_timestamp: None,
            last_timestamp: None,
        };
        assert_eq!(session.user_message_count(), 0);
        assert_eq!(session.assistant_message_count(), 0);
    }

    // --- extract_user_text() edge cases ---

    #[test]
    fn extract_user_text_null_content() {
        let entry = UserEntry {
            uuid: "u1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(UserMessage {
                content: serde_json::Value::Null,
            }),
            cwd: None,
        };
        assert!(extract_user_text(&entry).is_none());
    }

    #[test]
    fn extract_user_text_empty_array() {
        let entry = UserEntry {
            uuid: "u1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(UserMessage {
                content: serde_json::json!([]),
            }),
            cwd: None,
        };
        assert!(extract_user_text(&entry).is_none());
    }

    #[test]
    fn extract_user_text_array_no_text_blocks() {
        let entry = UserEntry {
            uuid: "u1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(UserMessage {
                content: serde_json::json!([{"type": "image", "url": "http://example.com/img.png"}]),
            }),
            cwd: None,
        };
        assert!(extract_user_text(&entry).is_none());
    }

    #[test]
    fn extract_user_text_multiple_text_blocks_joined() {
        let entry = UserEntry {
            uuid: "u1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(UserMessage {
                content: serde_json::json!([
                    {"type": "text", "text": "first"},
                    {"type": "text", "text": "second"}
                ]),
            }),
            cwd: None,
        };
        assert_eq!(extract_user_text(&entry).unwrap(), "first\nsecond");
    }

    // --- extract_assistant_texts() edge cases ---

    #[test]
    fn extract_assistant_texts_no_message() {
        let entry = AssistantEntry {
            uuid: "a1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: None,
        };
        assert!(extract_assistant_texts(&entry).is_empty());
    }

    #[test]
    fn extract_assistant_texts_filters_out_thinking_and_tool_result() {
        let entry = AssistantEntry {
            uuid: "a1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(AssistantMessage {
                content: vec![
                    ContentBlock::Thinking { thinking: Some("internal thought".into()) },
                    ContentBlock::Text { text: "visible text".into() },
                    ContentBlock::ToolResult { content: serde_json::json!("result data") },
                    ContentBlock::ToolUse { name: "Read".into(), input: serde_json::json!({}) },
                    ContentBlock::Text { text: "more text".into() },
                ],
                model: None,
                stop_reason: None,
            }),
        };
        let texts = extract_assistant_texts(&entry);
        assert_eq!(texts, vec!["visible text", "more text"]);
    }

    #[test]
    fn extract_assistant_texts_empty_content() {
        let entry = AssistantEntry {
            uuid: "a1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(AssistantMessage {
                content: vec![],
                model: None,
                stop_reason: None,
            }),
        };
        assert!(extract_assistant_texts(&entry).is_empty());
    }

    // --- extract_tool_uses() filtering ---

    #[test]
    fn extract_tool_uses_no_message() {
        let entry = AssistantEntry {
            uuid: "a1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: None,
        };
        assert!(extract_tool_uses(&entry).is_empty());
    }

    #[test]
    fn extract_tool_uses_only_returns_tool_use_blocks() {
        let entry = AssistantEntry {
            uuid: "a1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(AssistantMessage {
                content: vec![
                    ContentBlock::Text { text: "Let me help".into() },
                    ContentBlock::ToolUse { name: "Write".into(), input: serde_json::json!({"file_path": "/tmp/a"}) },
                    ContentBlock::Thinking { thinking: None },
                    ContentBlock::ToolUse { name: "Bash".into(), input: serde_json::json!({"command": "ls"}) },
                    ContentBlock::ToolResult { content: serde_json::json!(null) },
                ],
                model: None,
                stop_reason: None,
            }),
        };
        let tools = extract_tool_uses(&entry);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].0, "Write");
        assert_eq!(tools[1].0, "Bash");
    }

    #[test]
    fn extract_tool_uses_preserves_input() {
        let entry = AssistantEntry {
            uuid: "a1".into(),
            timestamp: "t".into(),
            session_id: None,
            message: Some(AssistantMessage {
                content: vec![
                    ContentBlock::ToolUse {
                        name: "Edit".into(),
                        input: serde_json::json!({"file_path": "/src/main.rs", "old_string": "foo", "new_string": "bar"}),
                    },
                ],
                model: None,
                stop_reason: None,
            }),
        };
        let tools = extract_tool_uses(&entry);
        assert_eq!(tools[0].1["file_path"], "/src/main.rs");
        assert_eq!(tools[0].1["old_string"], "foo");
    }
}
