//! Live agent session id helpers (Claude Code and Codex).
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
//! env var → `CODEX_THREAD_ID` → `CODEX_SESSION_ID` (Codex exports both,
//! with the same value, to the processes it starts) → `None`. `None` means
//! standalone behaviour is unchanged — nothing is added to `meta`.

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

/// Pull the session id out of an MCP request's `_meta`. Codex sends it on
/// every `tools/call` as `x-codex-turn-metadata.session_id` (and the same
/// value as `thread_id`); its MCP servers get no session env var, so this is
/// the only way a Codex event learns its session. Empty counts as absent.
pub fn session_id_from_mcp_meta(meta: &serde_json::Map<String, Value>) -> Option<String> {
    let turn = meta.get("x-codex-turn-metadata")?;

    ["session_id", "thread_id"]
        .iter()
        .filter_map(|key| turn.get(*key).and_then(|v| v.as_str()))
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read the live session id from the environment: `CLAUDE_CODE_SESSION_ID`,
/// then Codex's `CODEX_THREAD_ID`, then `CODEX_SESSION_ID`. Empty counts as
/// absent.
pub fn session_id_from_env() -> Option<String> {
    [
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    ]
    .into_iter()
    .find_map(|var| std::env::var(var).ok().filter(|s| !s.is_empty()))
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
    use crate::test_env_lock;
    use serde_json::json;

    #[test]
    fn payload_session_id_extracted() {
        let p = json!({"session_id": "abc-123", "hook_event_name": "PostToolUse"});
        assert_eq!(session_id_from_payload(&p).as_deref(), Some("abc-123"));
    }

    #[test]
    fn mcp_meta_reads_codex_turn_metadata() {
        let meta = json!({
            "callId": "exec-1",
            "x-codex-turn-metadata": { "session_id": "c-1", "thread_id": "c-1", "model": "x" }
        });
        let meta = meta.as_object().unwrap();
        assert_eq!(session_id_from_mcp_meta(meta).as_deref(), Some("c-1"));

        let thread_only =
            json!({ "x-codex-turn-metadata": { "session_id": "", "thread_id": "t-2" } });
        assert_eq!(
            session_id_from_mcp_meta(thread_only.as_object().unwrap()).as_deref(),
            Some("t-2")
        );
    }

    #[test]
    fn mcp_meta_without_codex_metadata_is_none() {
        // Claude Code sends only its tool-use id.
        let claude = json!({ "claudecode/toolUseId": "toolu_1", "progressToken": 2 });
        assert_eq!(session_id_from_mcp_meta(claude.as_object().unwrap()), None);
        assert_eq!(session_id_from_mcp_meta(&serde_json::Map::new()), None);
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
        let _g = test_env_lock();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "from-env");
        let p = json!({"session_id": "from-payload"});
        assert_eq!(live_session_id(Some(&p)).as_deref(), Some("from-payload"));
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    }

    #[test]
    fn live_falls_back_to_env() {
        let _g = test_env_lock();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "from-env");
        let p = json!({"hook_event_name": "Stop"});
        assert_eq!(live_session_id(Some(&p)).as_deref(), Some("from-env"));
        assert_eq!(live_session_id(None).as_deref(), Some("from-env"));
        std::env::remove_var("CLAUDE_CODE_SESSION_ID");
    }

    #[test]
    fn live_none_when_no_source() {
        let _g = test_env_lock();
        for var in SESSION_ENV_VARS {
            std::env::remove_var(var);
        }
        assert_eq!(live_session_id(None), None);
        assert_eq!(live_session_id(Some(&json!({}))), None);
    }

    const SESSION_ENV_VARS: [&str; 3] = [
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    ];

    /// Run `f` with exactly the given session env vars set (the rest unset).
    fn with_session_env(set: &[(&str, &str)], f: impl FnOnce()) {
        let _g = test_env_lock();
        for var in SESSION_ENV_VARS {
            std::env::remove_var(var);
        }
        for (k, v) in set {
            std::env::set_var(k, v);
        }

        f();

        for var in SESSION_ENV_VARS {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn env_falls_back_to_codex_thread_then_session_id() {
        with_session_env(
            &[
                ("CODEX_THREAD_ID", "thread"),
                ("CODEX_SESSION_ID", "session"),
            ],
            || assert_eq!(session_id_from_env().as_deref(), Some("thread")),
        );
        with_session_env(&[("CODEX_SESSION_ID", "session")], || {
            assert_eq!(session_id_from_env().as_deref(), Some("session"))
        });
        with_session_env(
            &[("CODEX_THREAD_ID", ""), ("CODEX_SESSION_ID", "session")],
            || assert_eq!(session_id_from_env().as_deref(), Some("session")),
        );
    }

    #[test]
    fn env_prefers_claude_code_over_codex() {
        with_session_env(
            &[
                ("CLAUDE_CODE_SESSION_ID", "claude"),
                ("CODEX_THREAD_ID", "thread"),
            ],
            || assert_eq!(session_id_from_env().as_deref(), Some("claude")),
        );
    }
}
