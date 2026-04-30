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
