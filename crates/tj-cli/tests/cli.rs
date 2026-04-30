use assert_cmd::Command;
use assert_fs::prelude::*;
use predicates::prelude::*;
use predicates::str::contains;

#[test]
fn pack_command_prints_markdown_for_existing_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Pack me"])
            .assert().success()
            .get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("# Pack me"));
}

#[test]
fn event_command_appends_decision_visible_in_pack() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["event", &task_id, "--type", "decision", "--text", "Adopt Rust"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Adopt Rust"));
}

#[test]
fn close_command_marks_task_closed_in_pack() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"]).assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &task_id, "--reason", "shipped"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("status: closed"));
}

#[test]
fn search_all_projects_finds_match_in_other_project_hash() {
    let dir = assert_fs::TempDir::new().unwrap();

    let state = dir.path().join("task-journal").join("state");
    std::fs::create_dir_all(&state).unwrap();

    for hash in ["aaaa1111aaaa1111", "bbbb2222bbbb2222"] {
        let db_path = state.join(format!("{hash}.sqlite"));
        let conn = tj_core::db::open(&db_path).unwrap();
        let mut e = tj_core::event::Event::new(
            format!("tj-{}", &hash[..6]),
            tj_core::event::EventType::Open,
            tj_core::event::Author::User,
            tj_core::event::Source::Cli,
            format!("Marker {hash}"),
        );
        e.meta = serde_json::json!({"title": format!("Title {hash}")});
        tj_core::db::upsert_task_from_event(&conn, &e, hash).unwrap();
        tj_core::db::index_event(&conn, &e).unwrap();
    }

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "Marker", "--all-projects"])
        .assert().success()
        .stdout(contains("aaaa1111").and(contains("bbbb2222")));
}

#[test]
fn search_command_finds_task_by_event_text() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "OAuth thing"]).assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["event", &task_id, "--type", "decision", "--text", "Adopt Rust + rmcp"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "rmcp"])
        .assert().success()
        .stdout(contains(&task_id));
}

#[test]
fn e2e_create_event_close_pack_search() {
    let dir = assert_fs::TempDir::new().unwrap();
    let env = || {
        let mut cmd = Command::cargo_bin("task-journal").unwrap();
        cmd.env("XDG_DATA_HOME", dir.path());
        cmd
    };

    let task_id = String::from_utf8(
        env().args(["create", "Build pack assembler"]).assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    env().args(["event", &task_id, "--type", "hypothesis", "--text", "Use SQLite views"]).assert().success();
    env().args(["event", &task_id, "--type", "decision", "--text", "Rust + rmcp"]).assert().success();
    env().args(["event", &task_id, "--type", "rejection", "--text", "Node loses binary"]).assert().success();
    env().args(["close", &task_id, "--reason", "shipped"]).assert().success();

    env().args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("Build pack assembler")
            .and(contains("Rust + rmcp"))
            .and(contains("Node loses binary"))
            .and(contains("status: closed")));

    env().args(["search", "rmcp"])
        .assert().success()
        .stdout(contains(&task_id));
}

#[test]
fn e2e_hook_simulation_classifies_and_packs_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Stack choice for journal"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind", "Stop",
            "--text", "After review, we adopt Rust because of the single-binary distribution.",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.92",
        ])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("Stack choice for journal")
            .and(contains("[decision]"))
            .and(contains("single-binary"))
            .and(contains("[?]").not()));
}

#[test]
fn event_correct_links_to_corrected_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Correct me"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    let bad = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["event", &task_id, "--type", "finding", "--text", "Migration done (wrong)"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event-correct",
            "--corrects", &bad,
            "--task", &task_id,
            "--text", "Migration was NOT done; finding was wrong",
        ])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert().success()
        .stdout(contains("Migration was NOT done").and(contains("[correction]")));
}

#[test]
fn install_hooks_writes_to_settings_json() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();

    let settings_path = dir.path().join(".claude").join("settings.json");
    assert!(settings_path.exists());
    let content = std::fs::read_to_string(&settings_path).unwrap();
    assert!(content.contains("UserPromptSubmit"));
    assert!(content.contains("PostToolUse"));
    assert!(content.contains("task-journal ingest-hook"));
}

#[test]
fn install_hooks_is_idempotent_and_uninstall_works() {
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.json"),
        serde_json::json!({"theme": "dark"}).to_string()).unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();
    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert().success();

    let after_install = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(after_install.contains("\"theme\":\"dark\"") || after_install.contains("\"theme\": \"dark\""), "must preserve unrelated keys");
    assert!(after_install.contains("UserPromptSubmit"));

    Command::cargo_bin("task-journal").unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--uninstall"])
        .assert().success();

    let after_uninstall = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(after_uninstall.contains("\"theme\":\"dark\"") || after_uninstall.contains("\"theme\": \"dark\""), "must still preserve theme");
    assert!(!after_uninstall.contains("UserPromptSubmit"));
}

#[test]
fn ingest_hook_drains_pending_queue_via_mock() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Drain"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    let pending = dir.path().join("task-journal").join("pending");
    std::fs::create_dir_all(&pending).unwrap();
    std::fs::write(pending.join("01stuck.json"), serde_json::json!({
        "text": "We decided to adopt PKCE flow.",
        "queued_at": "2026-04-30T00:00:00Z"
    }).to_string()).unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind", "Stop",
            "--text", "Live chunk",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.95",
        ])
        .assert().success();

    let remaining: Vec<_> = std::fs::read_dir(&pending).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .collect();
    assert_eq!(remaining.len(), 0, "pending queue must be empty after successful ingest");
}

#[test]
fn ingest_hook_with_mock_writes_classified_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Mock target"])
            .assert().success().get_output().stdout.clone()
    ).unwrap().trim().to_string();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind", "Stop",
            "--text", "We decided to adopt Rust.",
            "--mock-event-type", "decision",
            "--mock-task-id", &task_id,
            "--mock-confidence", "0.95",
        ])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("We decided to adopt Rust.").and(contains("[decision]")));
}

#[test]
fn create_back_to_back_yields_distinct_task_ids() {
    let dir = assert_fs::TempDir::new().unwrap();

    let ids: Vec<String> = (0..5).map(|_| {
        let out = Command::cargo_bin("task-journal").unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Same title"])
            .assert().success()
            .get_output().stdout.clone();
        String::from_utf8(out).unwrap().trim().to_string()
    }).collect();

    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 5, "task ids must be unique, got: {ids:?}");
}

#[test]
fn create_writes_open_event_to_jsonl() {
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Add OAuth login"])
        .assert()
        .success();

    let events_glob = dir.path().join("task-journal").join("events");
    let mut found_lines = 0;
    for entry in std::fs::read_dir(&events_glob).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let body = std::fs::read_to_string(&p).unwrap();
            for line in body.lines() {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                if v["type"] == "open" && v["text"].as_str().unwrap_or("").contains("OAuth") {
                    found_lines += 1;
                }
            }
        }
    }
    assert_eq!(found_lines, 1);
}

#[test]
fn events_list_shows_recent_events() {
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "First task"])
        .assert().success();
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Second task"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["events", "list", "--limit", "10"])
        .assert()
        .success()
        .stdout(contains("First task").and(contains("Second task")));
}

#[test]
fn rebuild_state_creates_sqlite_with_one_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Build it"])
        .assert().success();

    Command::cargo_bin("task-journal").unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["rebuild-state"])
        .assert()
        .success()
        .stdout(contains("rebuilt"));

    let state_dir = dir.path().join("task-journal").join("state");
    let mut found = 0;
    for entry in std::fs::read_dir(&state_dir).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("sqlite") {
            let conn = rusqlite::Connection::open(&p).unwrap();
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
            assert_eq!(n, 1);
            found += 1;
        }
    }
    assert_eq!(found, 1);
}

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("task-journal").unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("events"))
        .stdout(contains("rebuild-state"));
}
