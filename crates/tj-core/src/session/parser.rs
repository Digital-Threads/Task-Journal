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
}
