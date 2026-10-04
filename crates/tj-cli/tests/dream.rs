//! End-to-end checks for the offline mining commands (`dream`, `backfill`)
//! against a sandboxed Claude Code projects dir and a mocked LLM backend.
//! Unix-only: the fixtures rely on the OS reporting a canonical cwd.
#![cfg(unix)]

use assert_cmd::Command;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

struct Fixture {
    xdg: assert_fs::TempDir,
    proj: assert_fs::TempDir,
    claude: assert_fs::TempDir,
    /// `<claude>/projects/<encoded project path>` — where transcripts live.
    sessions_dir: PathBuf,
    events_path: PathBuf,
}

/// A project with an (empty) Claude Code sessions dir and events log dir.
fn fixture() -> Fixture {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let claude = assert_fs::TempDir::new().unwrap();
    // The CLI resolves the project from its cwd, which the OS reports
    // canonicalised (macOS `/var` → `/private/var`).
    let path = proj.path().canonicalize().unwrap();
    let sessions_dir =
        claude
            .path()
            .join("projects")
            .join(tj_core::session::discovery::encode_project_path(
                &path.to_string_lossy(),
            ));
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let hash = tj_core::project_hash::from_path(&path).unwrap();
    let events_path = xdg
        .path()
        .join("task-journal")
        .join("events")
        .join(format!("{hash}.jsonl"));
    std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
    Fixture {
        xdg,
        proj,
        claude,
        sessions_dir,
        events_path,
    }
}

/// Write a two-turn transcript containing `marker`, aged `ago_secs`.
fn write_session(fx: &Fixture, id: &str, marker: &str, ago_secs: u64) {
    let p = fx.sessions_dir.join(format!("{id}.jsonl"));
    let body = format!(
        "{{\"type\":\"user\",\"uuid\":\"u1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{{\"content\":\"{marker} first question\"}}}}\n\
         {{\"type\":\"assistant\",\"uuid\":\"a1\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"an answer\"}}]}}}}\n\
         {{\"type\":\"user\",\"uuid\":\"u2\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"message\":{{\"content\":\"second question\"}}}}\n"
    );
    std::fs::write(&p, body).unwrap();
    let f = std::fs::File::options().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(ago_secs))
        .unwrap();
}

/// Tie task `tj-dream` to each session via `meta.session_id`.
fn write_task_events(fx: &Fixture, session_ids: &[&str]) {
    use tj_core::event::{Author, Event, EventType, Source};
    let mut w = tj_core::storage::JsonlWriter::open(&fx.events_path).unwrap();
    for sid in session_ids {
        let mut e = Event::new(
            "tj-dream",
            EventType::Open,
            Author::User,
            Source::Cli,
            "Dream task".into(),
        );
        e.meta = serde_json::json!({ "session_id": sid });
        w.append(&e).unwrap();
    }
    w.flush_durable().unwrap();
}

/// Mock OpenAI-compatible server: transcripts with OKSESSION succeed (no
/// events), transcripts with FAILME get a 500.
fn mock_llm() -> (mockito::ServerGuard, Vec<mockito::Mock>) {
    let mut server = mockito::Server::new();
    let ok = server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::Regex("OKSESSION".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"choices":[{"message":{"content":"[]"}}]}"#)
        .create();
    let fail = server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::Regex("FAILME".into()))
        .with_status(500)
        .create();
    (server, vec![ok, fail])
}

fn dream(fx: &Fixture, url: &str, extra: &[&str]) -> String {
    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(fx.proj.path())
        .env("XDG_DATA_HOME", fx.xdg.path())
        .env("CLAUDE_CONFIG_DIR", fx.claude.path())
        .env("TJ_OLLAMA_URL", url)
        .args(["dream", "--backend", "ollama"])
        .args(extra)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn dream_scoped_run_does_not_advance_the_watermark() {
    let fx = fixture();
    write_session(&fx, "sess-old", "OKSESSION", 7200);
    write_session(&fx, "sess-new", "OKSESSION", 3600);
    write_task_events(&fx, &["sess-old", "sess-new"]);
    let (server, _mocks) = mock_llm();

    let scoped = dream(&fx, &server.url(), &["--limit", "1"]);
    assert!(scoped.contains("1 session(s) processed"), "{scoped}");

    // The --limit run skipped sess-old, so an unscoped run still sees both.
    let full = dream(&fx, &server.url(), &[]);
    assert!(full.contains("2 session(s) processed"), "{full}");

    // Everything mined cleanly → watermark reached the newest session.
    let again = dream(&fx, &server.url(), &[]);
    assert!(again.contains("0 session(s) processed"), "{again}");
}

#[test]
fn dream_session_with_a_failed_chunk_is_mined_again() {
    let fx = fixture();
    write_session(&fx, "sess-old", "OKSESSION", 7200);
    write_session(&fx, "sess-new", "FAILME", 3600);
    write_task_events(&fx, &["sess-old", "sess-new"]);
    let (server, _mocks) = mock_llm();

    let first = dream(&fx, &server.url(), &[]);
    assert!(first.contains("2 session(s) processed"), "{first}");

    // sess-old was clean so the watermark moved to it; sess-new failed and
    // must stay in scope.
    let second = dream(&fx, &server.url(), &[]);
    assert!(second.contains("1 session(s) processed"), "{second}");
}
