//! Live Claude Code session id helpers.
//!
//! task-journal already *parses* session ids out of Claude Code
//! transcripts (`session::parser`) — that is a passive, read-only
//! lookup of someone else's identifier. This module is the other
//! direction: additively stamping the live session id onto the events
//! the journal itself emits (hooks + MCP tools), so downstream
//! consumers can correlate those events with the originating session
//! without time-window heuristics.
//!
//! Source order: hook payload field `session_id` → `CLAUDE_CODE_SESSION_ID`
//! env var → `None`. `None` means standalone behaviour is unchanged —
//! nothing is added to `meta`.

use serde_json::Value;

/// Pull `session_id` out of a Claude Code hook payload (or a pending-v2
/// chunk, which carries the same field). Empty strings count as absent.
pub fn session_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("session_id")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read `CLAUDE_CODE_SESSION_ID` from the environment. Empty counts as absent.
pub fn session_id_from_env() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Resolve the live session id: hook payload first, env var as fallback.
/// `None` when neither source provides one (standalone — caller adds nothing).
pub fn live_session_id(payload: Option<&Value>) -> Option<String> {
    payload
        .and_then(session_id_from_payload)
        .or_else(session_id_from_env)
}

/// Additively record `session_id` into a free-form `meta` value.
///
/// No-op when `sid` is `None` or `meta` is not a JSON object. Never
/// overwrites or removes existing keys — additive by construction.
pub fn stamp_session_id(meta: &mut Value, sid: Option<&str>) {
    if let (Some(sid), Some(obj)) = (sid, meta.as_object_mut()) {
        obj.insert("session_id".to_string(), Value::String(sid.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    // Serialises the env-touching tests — std env is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn payload_session_id_extracted() {
        let p = json!({"session_id": "abc-123", "hook_event_name": "PostToolUse"});
        assert_eq!(session_id_from_payload(&p).as_deref(), Some("abc-123"));
    }

    #[test]
    fn payload_empty_or_missing_is_none() {
        assert_eq!(session_id_from_payload(&json!({"session_id": ""})), None);
        assert_eq!(session_id_from_payload(&json!({})), None);
        assert_eq!(session_id_from_payload(&Value::Null), None);
    }

    #[test]
    fn stamp_adds_to_object_meta() {
        let mut meta = json!({"title": "Goal"});
        stamp_session_id(&mut meta, Some("s-1"));
        assert_eq!(meta["session_id"], json!("s-1"));
        assert_eq!(meta["title"], json!("Goal"));
    }

    #[test]
    fn stamp_none_is_noop() {
        let mut meta = json!({"title": "Goal"});
        stamp_session_id(&mut meta, None);
        assert!(meta.get("session_id").is_none());
    }

    #[test]
    fn stamp_on_non_object_is_noop() {
        let mut meta = Value::Null;
        stamp_session_id(&mut meta, Some("s-1"));
        assert_eq!(meta, Value::Null);
    }

    #[test]
    fn live_payload_wins_over_env() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "from-env");
        let p = json!({"session_id": "from-payload"});
        assert_eq!(live_session_id(Some(&p)).as_deref(), Some("from-payload"));
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    }

    #[test]
    fn live_falls_back_to_env() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "from-env");
        let p = json!({"hook_event_name": "Stop"});
        assert_eq!(live_session_id(Some(&p)).as_deref(), Some("from-env"));
        assert_eq!(live_session_id(None).as_deref(), Some("from-env"));
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    }

    #[test]
    fn live_none_when_no_source() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
        assert_eq!(live_session_id(None), None);
        assert_eq!(live_session_id(Some(&json!({}))), None);
    }
}
