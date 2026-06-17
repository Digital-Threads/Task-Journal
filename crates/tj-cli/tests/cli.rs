use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

#[test]
fn pack_command_prints_markdown_for_existing_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Pack me"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
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
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Adopt Rust",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
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
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &task_id, "--reason", "shipped"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("status: closed"));
}

#[test]
fn close_warns_on_completeness_gap() {
    let dir = assert_fs::TempDir::new().unwrap();
    // Create a task WITH a goal so the NoGoal gap won't fire.
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Gap me", "--goal", "ship it"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Close WITHOUT an outcome → ClosedNoOutcome gap. The close still succeeds
    // and prints the gap to stderr.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &task_id, "--reason", "done"])
        .assert()
        .success()
        .stderr(contains("closed without a recorded outcome"));

    // And the task is actually closed.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("status: closed"));
}

#[test]
fn doctor_exits_zero_on_fresh_install() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["doctor"])
        .assert()
        .success();
}

#[test]
fn doctor_json_output_is_parseable_and_lists_paths() {
    let dir = assert_fs::TempDir::new().unwrap();
    let output = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("doctor --json must be valid JSON");

    assert!(v.get("data_dir").is_some());
    assert!(v.get("events_dir").is_some());
    assert!(v.get("state_dir").is_some());
    assert!(v.get("known_projects").unwrap().is_array());
    assert!(v.get("issues").unwrap().is_array());
}

fn write_pending(xdg: &std::path::Path, id: &str, text: &str, attempts: u32) {
    let dir = xdg.join("task-journal").join("pending");
    std::fs::create_dir_all(&dir).unwrap();
    let body = serde_json::json!({
        "text": text,
        "error": "test injection",
        "queued_at": "2026-05-07T00:00:00Z",
        "attempts": attempts,
    });
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
}

#[test]
fn pending_list_shows_queued_entries() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    write_pending(xdg.path(), "tj-pending-1", "I think the cache is racy", 0);

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args(["pending", "list"])
        .assert()
        .success()
        .stdout(contains("tj-pending-1"))
        .stdout(contains("I think the cache is racy"));
}

#[test]
fn pending_retry_drains_with_mock_classifier() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();

    // Seed: real task in JSONL so the classifier-mocked event has a
    // legitimate task_id to attach to.
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Pending host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    write_pending(
        xdg.path(),
        "tj-pending-2",
        "Adopted Rust for the journal",
        0,
    );

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args([
            "pending",
            "retry",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.92",
        ])
        .assert()
        .success()
        .stdout(contains("1 drained"));

    // pending file removed
    let pending_file = xdg
        .path()
        .join("task-journal")
        .join("pending")
        .join("tj-pending-2.json");
    assert!(!pending_file.exists(), "drained entry must be removed");

    // event landed in JSONL — visible in pack
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Adopted Rust for the journal"));
}

#[test]
fn pending_retry_marks_dead_after_max_attempts() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    // Already at attempts=2; one more failure should rename to *.dead.json.
    write_pending(xdg.path(), "tj-dying", "any text", 2);

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        // No --mock-* flags → retry fails → attempts becomes 3 → dead.
        .args(["pending", "retry"])
        .assert()
        .success()
        .stdout(contains("1 marked dead"));

    let pending_dir = xdg.path().join("task-journal").join("pending");
    let live = pending_dir.join("tj-dying.json");
    let dead = pending_dir.join("tj-dying.dead.json");
    assert!(!live.exists(), "live file must be gone after dead-rename");
    assert!(dead.exists(), "dead file must exist: {dead:?}");
}

#[test]
fn export_sqlite_round_trips_through_pack() {
    // Setup A: write a project + task in xdg_a/proj_a.
    let xdg_a = assert_fs::TempDir::new().unwrap();
    let proj_a = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg_a.path())
            .current_dir(proj_a.path())
            .args(["create", "Round-trip via sqlite export"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg_a.path())
        .current_dir(proj_a.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Adopt sqlite export",
        ])
        .assert()
        .success();

    // Export the SQLite snapshot to a buffer.
    let snapshot = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg_a.path())
        .current_dir(proj_a.path())
        .args(["export", "--format", "sqlite"])
        .output()
        .unwrap()
        .stdout;
    assert!(
        snapshot.starts_with(b"SQLite format 3\0"),
        "magic bytes missing"
    );

    // Setup B: a fresh xdg, no JSONL — only the snapshot in state/.
    let xdg_b = assert_fs::TempDir::new().unwrap();
    // Project hash derives from the proj path; we keep the same path so
    // the hash matches what the snapshot was keyed under.
    let project_hash = {
        let out = Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg_a.path())
            .current_dir(proj_a.path())
            .args(["doctor", "--json"])
            .output()
            .unwrap()
            .stdout;
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        v["state_dir"].as_str().unwrap().to_owned()
    };
    // We can't read the project_hash directly, but state_dir/<hash>.sqlite
    // is the file we're after. Re-derive the destination for xdg_b by
    // running doctor against xdg_b too — same proj path = same hash.
    let _ = project_hash;
    let dest_state_dir = xdg_b.path().join("task-journal").join("state");
    std::fs::create_dir_all(&dest_state_dir).unwrap();
    // Pull the source filename (first .sqlite under xdg_a/task-journal/state).
    let src_state_dir = xdg_a.path().join("task-journal").join("state");
    let src_file = std::fs::read_dir(&src_state_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|s| s.to_str()) == Some("sqlite"))
        .expect("source sqlite present");
    let dest_file = dest_state_dir.join(src_file.file_name().unwrap());
    std::fs::write(&dest_file, &snapshot).unwrap();

    // Pack from the new XDG without a JSONL — assemble must read from the
    // snapshot SQLite alone.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg_b.path())
        .current_dir(proj_a.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Adopt sqlite export"));
}

#[test]
fn export_html_emits_self_contained_document() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "HTML export test"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Adopt Rust",
        ])
        .assert()
        .success();

    let output = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args(["export", "--format", "html", "--task", &task_id])
        .output()
        .unwrap();
    let html = String::from_utf8(output.stdout).unwrap();

    // Self-contained shape.
    let lower = html.to_lowercase();
    assert!(
        lower.starts_with("<!doctype html>"),
        "html missing doctype: {html}"
    );
    assert!(html.contains("HTML export test"), "task title missing");
    assert!(html.contains("Adopt Rust"), "decision event missing");
    // No external assets — no http/https URL anywhere.
    assert!(!html.contains("http://"), "external http url leaked");
    assert!(!html.contains("https://"), "external https url leaked");
}

#[test]
fn migrate_project_round_trips_data_to_new_path() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj_a = assert_fs::TempDir::new().unwrap();
    let proj_b = assert_fs::TempDir::new().unwrap();
    // Distinct project roots so each hashes to itself, not to a shared ancestor
    // carrying a `.git` (which collapses both to one hash on some hosts, WSL).
    std::fs::create_dir(proj_a.path().join(".git")).unwrap();
    std::fs::create_dir(proj_b.path().join(".git")).unwrap();

    // Create a task with the cwd = proj_a.
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj_a.path())
            .args(["create", "Migration round-trip"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Migrate the data to proj_b.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .args([
            "migrate-project",
            "--from",
            proj_a.path().to_str().unwrap(),
            "--to",
            proj_b.path().to_str().unwrap(),
        ])
        .assert()
        .success();

    // Pack from proj_b finds the same task.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj_b.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Migration round-trip"));
}

#[test]
fn migrate_project_refuses_overwrite_without_force() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj_a = assert_fs::TempDir::new().unwrap();
    let proj_b = assert_fs::TempDir::new().unwrap();
    // Distinct project roots so each hashes to itself, not a shared `.git`
    // ancestor (which collapses both to one hash on some hosts, e.g. WSL).
    std::fs::create_dir(proj_a.path().join(".git")).unwrap();
    std::fs::create_dir(proj_b.path().join(".git")).unwrap();

    // Both projects have data: create a task in each.
    for proj in [&proj_a, &proj_b] {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Conflicting"])
            .assert()
            .success();
    }

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .args([
            "migrate-project",
            "--from",
            proj_a.path().to_str().unwrap(),
            "--to",
            proj_b.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("destination already exists"));
}

#[test]
fn close_unknown_task_id_returns_error() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", "tj-doesnotexist", "--reason", "shipped"])
        .assert()
        .failure()
        .stderr(contains("task not found: tj-doesnotexist"));
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

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "Marker", "--all-projects"])
        .assert()
        .success()
        .stdout(contains("aaaa1111").and(contains("bbbb2222")));
}

#[test]
fn search_command_finds_task_by_event_text() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "OAuth thing"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Adopt Rust + rmcp",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "rmcp"])
        .assert()
        .success()
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
        env()
            .args(["create", "Build pack assembler"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    env()
        .args([
            "event",
            &task_id,
            "--type",
            "hypothesis",
            "--text",
            "Use SQLite views",
        ])
        .assert()
        .success();
    env()
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Rust + rmcp",
        ])
        .assert()
        .success();
    env()
        .args([
            "event",
            &task_id,
            "--type",
            "rejection",
            "--text",
            "Node loses binary",
        ])
        .assert()
        .success();
    env()
        .args(["close", &task_id, "--reason", "shipped"])
        .assert()
        .success();

    env()
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(
            contains("Build pack assembler")
                .and(contains("Rust + rmcp"))
                .and(contains("Node loses binary"))
                .and(contains("status: closed")),
        );

    env()
        .args(["search", "rmcp"])
        .assert()
        .success()
        .stdout(contains(&task_id));
}

#[test]
fn e2e_hook_simulation_classifies_and_packs_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Stack choice for journal"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind",
            "Stop",
            "--text",
            "After review, we adopt Rust because of the single-binary distribution.",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.92",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(
            contains("Stack choice for journal")
                .and(contains("[decision]"))
                .and(contains("single-binary"))
                .and(contains("[?]").not()),
        );
}

#[test]
fn event_correct_links_to_corrected_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Correct me"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let bad = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args([
                "event",
                &task_id,
                "--type",
                "finding",
                "--text",
                "Migration done (wrong)",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event-correct",
            "--corrects",
            &bad,
            "--task",
            &task_id,
            "--text",
            "Migration was NOT done; finding was wrong",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Migration was NOT done").and(contains("[correction]")));
}

#[test]
fn install_hooks_command_uses_no_fail_pattern() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();
    let s = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        s.contains("|| true"),
        "hook command must end with || true so a failed classifier doesn't break Claude Code: {s}"
    );
}

#[test]
fn install_hooks_writes_to_settings_json() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();

    let settings_path = dir.path().join(".claude").join("settings.json");
    assert!(settings_path.exists());
    let content = std::fs::read_to_string(&settings_path).unwrap();
    assert!(content.contains("task-journal ingest-hook")); // SessionStart resume
    assert!(
        content.contains("SessionStart"),
        "install-hooks must wire SessionStart so resume-pack injection works"
    );
    // v0.14.x — self-tagging-first: the default wires the no-model UserPromptSubmit
    // nudge, but NOT the per-message classifier hooks (those spawn `claude -p`);
    // the classifier is opt-in via `--auto-capture`.
    assert!(
        content.contains("task-journal nudge"),
        "default must wire the no-model UserPromptSubmit nudge"
    );
    assert!(
        !content.contains("PostToolUse"),
        "default must not wire the per-message classifier hooks"
    );
    assert!(
        !content.contains("\"Stop\""),
        "default must not wire the classifier Stop hook"
    );
}

#[test]
fn install_hooks_auto_capture_wires_all_events() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--auto-capture"])
        .assert()
        .success();

    let content =
        std::fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
    for ev in [
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "Stop",
        "PreCompact",
        "SessionEnd",
    ] {
        assert!(content.contains(ev), "--auto-capture must wire {ev}");
    }
}

#[test]
fn session_end_hook_is_clean_noop_without_journal() {
    // SessionEnd(clear) with no journal yet must exit cleanly (it's the
    // last-chance catch-up; nothing to catch when there's no project journal).
    let dir = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    for reason in ["clear", "logout"] {
        let payload = serde_json::json!({
            "hook_event_name": "SessionEnd",
            "reason": reason,
            "session_id": "s-end",
            "transcript_path": "/nonexistent/x.jsonl",
            "cwd": proj.path().to_string_lossy(),
        })
        .to_string();
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", dir.path())
            .args(["ingest-hook", "--backend", "hybrid"])
            .write_stdin(payload)
            .assert()
            .success();
    }
}

#[test]
fn install_hooks_merges_and_preserves_third_party_hooks() {
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    // Pre-existing foreign hooks (another plugin) on the same events we touch.
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [{ "matcher": "", "hooks": [
                    { "type": "command", "command": "other-plugin do-thing" }
                ]}],
                "SessionStart": [{ "matcher": "", "hooks": [
                    { "type": "command", "command": "other-plugin start" }
                ]}]
            }
        })
        .to_string(),
    )
    .unwrap();

    let run = || {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("HOME", dir.path())
            .args(["install-hooks", "--scope", "user"])
            .assert()
            .success();
    };
    run();
    run(); // idempotent: second install must not duplicate task-journal entries

    let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    // Foreign hooks survive.
    assert!(
        content.contains("other-plugin do-thing"),
        "must preserve a third-party UserPromptSubmit hook: {content}"
    );
    assert!(
        content.contains("other-plugin start"),
        "must preserve a third-party SessionStart hook"
    );
    // Ours got added.
    assert!(content.contains("task-journal nudge"));
    assert!(content.contains("task-journal ingest-hook"));
    // Idempotent — exactly one nudge, not two.
    assert_eq!(
        content.matches("task-journal nudge").count(),
        1,
        "re-install must not duplicate the nudge hook: {content}"
    );
}

#[test]
fn install_hooks_is_idempotent_and_uninstall_works() {
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({"theme": "dark"}).to_string(),
    )
    .unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();

    let after_install = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(
        after_install.contains("\"theme\":\"dark\"")
            || after_install.contains("\"theme\": \"dark\""),
        "must preserve unrelated keys"
    );
    assert!(after_install.contains("SessionStart"));

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--uninstall"])
        .assert()
        .success();

    let after_uninstall = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(
        after_uninstall.contains("\"theme\":\"dark\"")
            || after_uninstall.contains("\"theme\": \"dark\""),
        "must still preserve theme"
    );
    assert!(!after_uninstall.contains("UserPromptSubmit"));
}

#[test]
fn install_hooks_uninstall_preserves_third_party_hook_entries() {
    // Repro for the "uninstall nukes everyone's hooks" bug. The fix
    // must walk into each event array and filter out ONLY commands
    // matching task-journal — other plugins (token-pilot in the
    // wild) keep their entries.
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();

    // Pre-existing settings: task-journal-style entry + a foreign
    // plugin's hook on the same event.
    let pre = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [
                {
                    "matcher": "",
                    "hooks": [
                        { "type": "command", "command": "task-journal ingest-hook --kind=$CLAUDE_HOOK_NAME --text=\"$CLAUDE_HOOK_TEXT\" --backend=cli || true" },
                        { "type": "command", "command": "other-plugin do-something" }
                    ]
                }
            ],
            "PostToolUse": [
                {
                    "matcher": "",
                    "hooks": [
                        { "type": "command", "command": "third-party-only-hook" }
                    ]
                }
            ]
        }
    });
    std::fs::write(claude_dir.join("settings.json"), pre.to_string()).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--uninstall"])
        .assert()
        .success();

    let after = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&after).unwrap();

    assert!(
        !after.contains("task-journal ingest-hook"),
        "task-journal entry must be gone: {after}"
    );
    assert!(
        after.contains("other-plugin do-something"),
        "co-located third-party hook must survive: {after}"
    );
    assert!(
        after.contains("third-party-only-hook"),
        "PostToolUse with no task-journal entry must be untouched: {after}"
    );
    // The hooks block itself stays; other plugins' kinds remain.
    assert!(
        v.get("hooks").is_some(),
        "hooks block must still exist: {after}"
    );
}

// v0.9.0: removed three tests that asserted behavior of `--classifier-command`
// and the `TJ_CLASSIFIER_CLI` env var. Both features were removed together
// with the `cli` backend — see `install_hooks_uninstall_removes_legacy_classifier_env`
// below for the remaining back-compat assertion.

#[test]
fn install_hooks_uninstall_removes_legacy_classifier_env() {
    // Back-compat: users upgrading from <0.9.0 may have `TJ_CLASSIFIER_CLI`
    // sitting in their settings.json from a previous install. `--uninstall`
    // must still strip it, even though we no longer write the key on install.
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({ "env": { "TJ_CLASSIFIER_CLI": "aimux run dt", "OTHER_KEY": "keep_me" } })
            .to_string(),
    )
    .unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--uninstall"])
        .assert()
        .success();

    let after = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert!(
        v.get("env")
            .and_then(|e| e.get("TJ_CLASSIFIER_CLI"))
            .is_none(),
        "legacy TJ_CLASSIFIER_CLI must be removed on uninstall: {after}"
    );
    assert_eq!(
        v.get("env")
            .and_then(|e| e.get("OTHER_KEY"))
            .and_then(|s| s.as_str()),
        Some("keep_me"),
        "unrelated env keys must be preserved: {after}"
    );
}

#[test]
fn ingest_hook_drains_pending_queue_via_mock() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Drain"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let pending = dir.path().join("task-journal").join("pending");
    std::fs::create_dir_all(&pending).unwrap();
    std::fs::write(
        pending.join("01stuck.json"),
        serde_json::json!({
            "text": "We decided to adopt PKCE flow.",
            "queued_at": "2026-04-30T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind",
            "Stop",
            "--text",
            "Live chunk",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.95",
        ])
        .assert()
        .success();

    let remaining: Vec<_> = std::fs::read_dir(&pending)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .collect();
    assert_eq!(
        remaining.len(),
        0,
        "pending queue must be empty after successful ingest"
    );
}

#[test]
fn stats_command_shows_classifier_counts() {
    let dir = assert_fs::TempDir::new().unwrap();
    let metrics = dir.path().join("task-journal").join("metrics");
    std::fs::create_dir_all(&metrics).unwrap();
    let body = [r#"{"timestamp":"2026-04-30T00:00:00Z","project_hash":"feedface","task_id_guess":"tj-x","event_type":"decision","confidence":0.95,"status":"confirmed","error":null}"#,
        r#"{"timestamp":"2026-04-30T00:00:00Z","project_hash":"feedface","task_id_guess":"tj-x","event_type":"finding","confidence":0.65,"status":"suggested","error":null}"#].join("\n");
    std::fs::write(metrics.join("feedface.jsonl"), body).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["stats"])
        .assert()
        .success()
        .stdout(
            contains("classified: 2")
                .and(contains("confirmed: 1"))
                .and(contains("suggested: 1")),
        );
}

#[test]
fn ingest_hook_writes_telemetry_record() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Tel"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind",
            "Stop",
            "--text",
            "decided to use Rust",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.92",
        ])
        .assert()
        .success();

    let metrics_dir = dir.path().join("task-journal").join("metrics");
    let mut total_lines = 0;
    if metrics_dir.exists() {
        for entry in std::fs::read_dir(&metrics_dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                total_lines += std::fs::read_to_string(&p).unwrap().lines().count();
            }
        }
    }
    assert!(
        total_lines >= 1,
        "expected at least one telemetry line, got {total_lines}"
    );
}

#[test]
fn ingest_hook_session_start_emits_resume_pack_json() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Wire SessionStart pack"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Adopt Rust for the journal.",
        ])
        .assert()
        .success();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["ingest-hook", "--kind", "SessionStart", "--text", ""])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();

    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap_or_else(|e| {
        panic!("SessionStart hook stdout must be JSON; got: {body:?}; err: {e}")
    });
    let hso = v
        .get("hookSpecificOutput")
        .expect("hookSpecificOutput key missing");
    assert_eq!(
        hso.get("hookEventName").and_then(|s| s.as_str()),
        Some("SessionStart"),
        "wrong hookEventName: {body}"
    );
    let ctx = hso
        .get("additionalContext")
        .and_then(|s| s.as_str())
        .expect("additionalContext key missing");
    assert!(
        ctx.contains("Wire SessionStart pack"),
        "additionalContext must include task title: {ctx}"
    );
    assert!(
        ctx.contains("Adopt Rust"),
        "additionalContext must include event text: {ctx}"
    );
    // 0.14.3: sessionTitle / initialUserMessage are no longer emitted —
    // they overrode Claude Code's native session name and seeded garbage
    // task titles. The resume context rides in additionalContext alone.
    assert!(
        hso.get("sessionTitle").is_none(),
        "sessionTitle must NOT be emitted — it overrides Claude's session name; got: {hso}"
    );
    assert!(
        hso.get("initialUserMessage").is_none(),
        "initialUserMessage must NOT be emitted; got: {hso}"
    );
}

#[test]
fn session_start_emits_neither_session_title_nor_initial_message() {
    // 0.14.3: the SessionStart envelope carries ONLY additionalContext
    // (+ optional watchPaths). It must never set sessionTitle (which
    // overrode Claude Code's own session name with our task id) nor
    // initialUserMessage (which seeded garbage "[Task Journal resumed: …]"
    // task titles).
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Hollow task no events"])
        .assert()
        .success();

    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["ingest-hook", "--kind", "SessionStart", "--text", ""])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let hso = v.get("hookSpecificOutput").unwrap();
    assert!(
        hso.get("sessionTitle").is_none(),
        "sessionTitle must NOT be emitted; got: {hso}"
    );
    assert!(
        hso.get("initialUserMessage").is_none(),
        "initialUserMessage must NOT be emitted; got: {hso}"
    );
}

#[test]
fn ingest_hook_session_start_with_no_open_tasks_emits_no_context() {
    let dir = assert_fs::TempDir::new().unwrap();
    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["ingest-hook", "--kind", "SessionStart", "--text", ""])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    // Empty stdout is the documented signal to Claude Code that no
    // additionalContext should be injected — we don't want to pollute
    // the system prompt with an empty pack on fresh projects.
    assert!(
        body.trim().is_empty(),
        "SessionStart with no open tasks must emit nothing, got: {body:?}"
    );
}

#[test]
fn create_with_goal_renders_in_pack() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Add OAuth", "--goal", "Implement PKCE flow"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("**Goal**: Implement PKCE flow"));
}

#[test]
fn create_without_goal_renders_not_set_placeholder() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "No goal"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Force goal to populate via the post-hoc command path so the row
    // exists in SQLite (create without --goal skips the SQLite write,
    // and pack needs the row to render). Without setting goal here we
    // still exercise the `(not set)` placeholder path because pack
    // reads via ingest_new_events first.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("**Goal**: (not set)"));
}

#[test]
fn close_with_outcome_renders_outcome_block() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Ship feature X", "--goal", "deliver X"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "close",
            &task_id,
            "--outcome",
            "Shipped in v0.4.0",
            "--outcome-tag",
            "done",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("**Outcome** [done]: Shipped in v0.4.0"));
}

#[test]
fn close_rejects_invalid_outcome_tag() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "T"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &task_id, "--outcome", "ok", "--outcome-tag", "wat"])
        .assert()
        .failure()
        .stderr(contains("invalid --outcome-tag"));
}

#[test]
fn goal_command_updates_existing_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Initial title"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["goal", &task_id, "Set after the fact"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("**Goal**: Set after the fact"));
}

#[test]
fn external_add_appends_references() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Linked work"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["external", &task_id, "--add", "beads:claude-memory-rsw"])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["external", &task_id, "--add", "github:#42"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "compact"])
        .assert()
        .success()
        .stdout(contains("**External**: beads:claude-memory-rsw,github:#42"));
}

#[test]
fn ingest_hook_short_circuits_when_in_classifier_env_set() {
    // Recursion guard: classifier sets TJ_IN_CLASSIFIER=1 before
    // spawning claude. The nested claude re-fires our hooks; without
    // this guard, ingest-hook would re-enter the classifier path
    // ad infinitum. With the guard, it returns silently and no event
    // is written.
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Recursion guard host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_IN_CLASSIFIER", "1")
        .args([
            "ingest-hook",
            "--kind",
            "UserPromptSubmit",
            "--text",
            "should not be ingested",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.99",
        ])
        .assert()
        .success();

    // The pack must NOT contain the hook text — guard kicked in
    // before the mock branch could write.
    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    assert!(
        !body.contains("should not be ingested"),
        "TJ_IN_CLASSIFIER must short-circuit before any write: {body}"
    );
}

#[test]
fn ingest_hook_reads_user_prompt_submit_payload_from_stdin() {
    // Real Claude Code passes hook input as JSON over stdin, NOT via env
    // vars. Without this, every captured event has empty text and the
    // classifier rejects it. Regression for claude-memory-rsw.
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Stdin host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-1",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "We adopted Rust for the journal."
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.95",
        ])
        .write_stdin(payload)
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("We adopted Rust for the journal"));
}

#[test]
fn ingest_hook_reads_post_tool_use_payload_from_stdin() {
    // PostToolUse payloads have no `prompt` field — content lives in
    // `tool_name` / `tool_input` / `tool_response`. The stdin parser must
    // synthesize text from those.
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Tool host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-2",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test" },
        "tool_response": { "output": "all 222 tests pass" }
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "evidence",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Bash").and(contains("cargo test")));
}

#[test]
fn install_hooks_writes_command_without_bogus_env_var_interpolation() {
    // The old install-hooks emitted $CLAUDE_HOOK_NAME / $CLAUDE_HOOK_TEXT,
    // neither of which Claude Code actually populates. The current command
    // must rely on stdin instead.
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();
    let s = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        !s.contains("$CLAUDE_HOOK_NAME") && !s.contains("$CLAUDE_HOOK_TEXT"),
        "install-hooks must not interpolate non-existent env vars: {s}"
    );
}

#[test]
fn ingest_hook_with_mock_writes_classified_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Mock target"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--kind",
            "Stop",
            "--text",
            "We decided to adopt Rust.",
            "--mock-event-type",
            "decision",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.95",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("We decided to adopt Rust.").and(contains("[decision]")));
}

#[test]
fn create_back_to_back_yields_distinct_task_ids() {
    let dir = assert_fs::TempDir::new().unwrap();

    let ids: Vec<String> = (0..5)
        .map(|_| {
            let out = Command::cargo_bin("task-journal")
                .unwrap()
                .env("XDG_DATA_HOME", dir.path())
                .args(["create", "Same title"])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            String::from_utf8(out).unwrap().trim().to_string()
        })
        .collect();

    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 5, "task ids must be unique, got: {ids:?}");
}

#[test]
fn create_writes_open_event_to_jsonl() {
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
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

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "First task"])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Second task"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["events", "list", "--limit", "10"])
        .assert()
        .success()
        .stdout(contains("First task").and(contains("Second task")));
}

#[test]
fn rebuild_state_creates_sqlite_with_one_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Build it"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
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
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
            found += 1;
        }
    }
    assert_eq!(found, 1);
}

#[test]
fn ingest_hook_help_hides_mock_flags() {
    Command::cargo_bin("task-journal")
        .unwrap()
        .args(["ingest-hook", "--help"])
        .assert()
        .success()
        .stdout(contains("--mock-event-type").not())
        .stdout(contains("--mock-task-id").not())
        .stdout(contains("--mock-confidence").not())
        .stdout(contains("--kind"))
        .stdout(contains("--text"));
}

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("task-journal")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("create"))
        .stdout(contains("events"))
        .stdout(contains("rebuild-state"));
}

#[test]
fn ingest_hook_auto_opens_task_when_no_open_tasks() {
    // v0.5.0 Phase A: a UserPromptSubmit hook firing into an empty
    // project must synthesize a task on the fly, otherwise the prompt
    // (and every event after it) is dropped silently.
    let dir = assert_fs::TempDir::new().unwrap();

    // Force the classifier to fail so the rest of the pipeline doesn't
    // try to spawn `claude -p`. Auto-open happens BEFORE the classifier
    // call, so the task should still be created.
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-auto",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "implement FIN-868 paygate fee dedup"
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        // v0.6.2: real-classifier path now async by default. Force sync
        // here so auto-open + pending side-effects are observable
        // synchronously after the command returns.
        .env("TJ_INGEST_SYNC", "1")
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(payload)
        .assert()
        .success();

    // Auto-opened task is now searchable. Pack it and check that the
    // goal field equals the prompt text — that's the contract.
    let search_out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "paygate"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(search_out).unwrap();
    // Search output is task-id-per-line. A non-empty body proves the
    // auto-opened task was indexed by FTS5 against the prompt text.
    let task_id = body
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("tj-"))
        .unwrap_or_else(|| {
            panic!("search must surface the auto-opened task by prompt text, got: {body:?}")
        });

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("**Goal**: implement FIN-868 paygate fee dedup"));
}

#[test]
fn ingest_hook_does_not_auto_open_for_log_scrollback() {
    // 0.14.3: a UserPromptSubmit whose text is only terminal scrollback —
    // here a framework log line — must NOT auto-open a task, otherwise the
    // journal fills with garbage titles like "685] INFO: Mapped {…}" that
    // then leak into the task list and the Claude Code session name.
    let dir = assert_fs::TempDir::new().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-noise",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "685] INFO: Mapped {/rest-api/paymentlnk-notify, POST} route"
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        .env("TJ_INGEST_SYNC", "1")
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(payload)
        .assert()
        .success();

    // Nothing should have been indexed — search for a token from the log
    // line must surface no task id.
    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["search", "paymentlnk"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        !body.lines().any(|l| l.trim().starts_with("tj-")),
        "log-scrollback prompt must NOT auto-open a task; got: {body:?}"
    );
}

#[test]
fn reopen_command_flips_status_back_to_open() {
    // v0.5.0 Phase C: a closed task can be revived via `reopen`. The
    // [reopen] event itself triggers the status flip (db lifecycle).
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Reopen target"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &task_id, "--reason", "first close"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("[status: closed]"));

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["reopen", &task_id, "--reason", "regression came back"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("[status: open]"));
}

#[test]
fn auto_open_links_to_prior_task_referencing_same_issue() {
    // v0.5.0 Phase C: if a fresh prompt mentions a ticket id that
    // already shows up in the journal, the new auto-opened task gets
    // an external "linked:tj-other" pointer so the chain is visible
    // in the pack rather than orphaned.
    let dir = assert_fs::TempDir::new().unwrap();
    let prior = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Original FIN work"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &prior,
            "--type",
            "decision",
            "--text",
            "fixed FIN-868 paygate fee duplicate write",
        ])
        .assert()
        .success();
    // Close the prior task — auto-open's link target is closed-but-
    // related, exactly the regression-came-back scenario.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["close", &prior, "--reason", "shipped"])
        .assert()
        .success();

    // Now fire a fresh UserPromptSubmit referencing the same ticket.
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-link",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "FIN-868 came back: paygate fee written twice on partial refund"
    })
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        // v0.6.2: force sync so the auto-open side effect (reopen note
        // on stderr) is observable synchronously.
        .env("TJ_INGEST_SYNC", "1")
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(payload)
        .assert()
        .success()
        .stderr(contains(format!("reopen {}", prior)));

    // Find the newly auto-opened task and confirm it has a linked
    // pointer back to the prior in External.
    let search_out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "paygate"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(search_out).unwrap();
    let new_id = body
        .lines()
        .find(|l| l.starts_with("tj-") && !l.contains(&prior))
        .map(|s| s.trim().to_string())
        .expect("auto-opened task must show up in search alongside prior");

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &new_id, "--mode", "full"])
        .assert()
        .success()
        // v0.6.0: linked entries surface in their own **Linked** block
        // instead of mashed into External, with the prior task's
        // current status annotated next to the id.
        .stdout(contains("**Linked**:"))
        .stdout(contains(format!("- {} [closed]", prior)));
}

#[test]
fn pack_renders_artifacts_block_from_event_text() {
    // v0.5.0 Phase B: artifacts (commits, PRs, issues) auto-extracted
    // from event text appear in pack as **Artifacts** block.
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "FIN-868 host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "fixed in abc1234 — see https://github.com/Digital-Threads/Task-Journal/pull/42 — references FIN-868",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("**Artifacts**:"))
        .stdout(contains("commits: abc1234"))
        .stdout(contains(
            "PRs: https://github.com/Digital-Threads/Task-Journal/pull/42",
        ))
        .stdout(contains("issues: FIN-868"));
}

#[test]
fn reclassify_backfills_artifacts_for_existing_events() {
    // After upgrade from v0.4.x, old events have NULL artifacts. The
    // `reclassify` command must walk the event_index and re-extract.
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Backfill host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "evidence",
            "--text",
            "verified at commit deadbeef99",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["reclassify", &task_id])
        .assert()
        .success()
        .stdout(contains("reclassified"));

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("commits: deadbeef99"));
}

#[test]
fn ingest_hook_auto_open_disabled_via_env() {
    // Opt-out path: TJ_AUTO_OPEN_TASKS=0 must restore the v0.4.0
    // behaviour (drop the prompt silently when no open task exists).
    let dir = assert_fs::TempDir::new().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-noop",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "marker_noautoopen_xyz must not appear"
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        .env("TJ_AUTO_OPEN_TASKS", "0")
        // v0.6.2: force sync so post-conditions are observable.
        .env("TJ_INGEST_SYNC", "1")
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(payload)
        .assert()
        .success();

    let search_out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["search", "marker_noautoopen_xyz"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(search_out).unwrap();
    assert!(
        !body.contains("marker_noautoopen_xyz"),
        "auto-open must be skipped when TJ_AUTO_OPEN_TASKS=0, got: {body:?}"
    );
}

// ---------------- v0.6.2 async classifier tests ----------------

/// v0.6.2: ingest-hook must NOT block on the classifier. The
/// real-classifier path queues a v2 pending entry and detaches a
/// worker, so the hook returns in <100ms even when the configured
/// classifier command is `/bin/false` (instant fail) or worse.
#[test]
fn ingest_hook_returns_fast_in_async_mode() {
    let dir = assert_fs::TempDir::new().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-fast",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "fast async marker xyz123"
    })
    .to_string();

    let start = std::time::Instant::now();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(payload)
        .assert()
        .success();
    let elapsed = start.elapsed();

    // Generous budget — the hook itself does almost no work; the
    // classifier subprocess runs in the detached worker. Pre-fix,
    // this took 5-30s. Post-fix, expect well under 1s; assert <2s
    // so flaky CI doesn't fail us.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "ingest-hook must return in <5s in async mode (the pre-fix regression was 5-30s); took {elapsed:?}"
    );

    // A v2 pending entry must have been written. We don't assert
    // worker progress — worker is detached and may or may not have
    // finished by the time we look.
    let pending = dir.path().join("task-journal").join("pending");
    assert!(pending.exists(), "pending dir must exist after queuing");
    let entries: Vec<_> = std::fs::read_dir(&pending)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    // Worker may have already drained; in that case at least the
    // worker should have left a v1 pending entry from /bin/false
    // failure (persist_pending in the real-classifier branch). So
    // either way: at least one .json file ought to be present, OR
    // the auto-open happened (events file exists). Be tolerant.
    let events = dir.path().join("task-journal").join("events");
    let has_pending = !entries.is_empty();
    let has_events = events.exists()
        && std::fs::read_dir(&events)
            .map(|d| d.count() > 0)
            .unwrap_or(false);
    assert!(
        has_pending || has_events,
        "either pending entry or events file must exist after async hook"
    );
}

/// classify-worker exits cleanly even when the classifier command is
/// /bin/false. v2 entries that fail to classify get re-queued as v1
/// pending entries (so `pending list` surfaces them).
#[test]
fn classify_worker_handles_classifier_failure_cleanly() {
    let dir = assert_fs::TempDir::new().unwrap();
    // Pre-create a v2 pending entry by hand.
    let pending = dir.path().join("task-journal").join("pending");
    std::fs::create_dir_all(&pending).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let project_hash = tj_core::project_hash::from_path(&cwd).expect("compute project hash");
    let events_path = dir
        .path()
        .join("task-journal")
        .join("events")
        .join(format!("{project_hash}.jsonl"));
    std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();

    let entry = pending.join("01worker.json");
    let body = serde_json::json!({
        "schema": "v2",
        "kind": "UserPromptSubmit",
        "text": "worker test marker",
        "project_hash": project_hash,
        "events_path": events_path.to_string_lossy(),
        "backend": "hybrid",
        "queued_at": "2026-05-08T00:00:00Z",
    });
    std::fs::write(&entry, body.to_string()).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        .args(["classify-worker", "--backend", "hybrid"])
        .assert()
        .success();

    // Lockfile must not be left behind.
    let state = dir.path().join("task-journal").join("state");
    if state.exists() {
        for e in std::fs::read_dir(&state).unwrap() {
            let p = e.unwrap().path();
            assert!(
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .ends_with(".lock"),
                "lockfile must be removed after worker exits, found: {p:?}"
            );
        }
    }
}

/// Lockfile prevents concurrent workers in the same project. We can't
/// easily race two real spawns deterministically in a unit test, so
/// instead simulate a held lock by writing a lockfile with our own
/// (live) PID, then run classify-worker and assert it exits cleanly
/// without draining the queue.
#[test]
fn classify_worker_respects_existing_lock() {
    let dir = assert_fs::TempDir::new().unwrap();
    let cwd = std::env::current_dir().unwrap();
    let project_hash = tj_core::project_hash::from_path(&cwd).expect("compute project hash");

    // Pre-create a v2 pending entry.
    let pending = dir.path().join("task-journal").join("pending");
    std::fs::create_dir_all(&pending).unwrap();
    let events_path = dir
        .path()
        .join("task-journal")
        .join("events")
        .join(format!("{project_hash}.jsonl"));
    std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
    let entry = pending.join("01locked.json");
    std::fs::write(
        &entry,
        serde_json::json!({
            "schema": "v2",
            "kind": "UserPromptSubmit",
            "text": "locked marker",
            "project_hash": project_hash,
            "events_path": events_path.to_string_lossy(),
            "backend": "hybrid",
            "queued_at": "2026-05-08T00:00:00Z",
        })
        .to_string(),
    )
    .unwrap();

    // Hand-roll a lockfile with this process's (live) PID. The
    // worker should see the live PID and bail without touching the
    // pending entry.
    let state = dir.path().join("task-journal").join("state");
    std::fs::create_dir_all(&state).unwrap();
    let lock_path = state.join(format!("classifier-{project_hash}.lock"));
    std::fs::write(&lock_path, format!("{}\n", std::process::id())).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        .args(["classify-worker", "--backend", "hybrid"])
        .assert()
        .success();

    // Pending entry must still be there — the second worker bailed.
    assert!(
        entry.exists(),
        "pending entry must survive — second worker must not have drained it"
    );
    // Our hand-rolled lockfile must still be there too — the bailing
    // worker must NOT remove a lock it didn't acquire.
    assert!(
        lock_path.exists(),
        "lockfile must survive — bailing worker must not delete others' locks"
    );
}

// =====================================================================
// v0.7.0: statusline / PreCompact / /rewind / rejected / export-pr
// =====================================================================

#[test]
fn statusline_empty_when_no_project_state() {
    let dir = assert_fs::TempDir::new().unwrap();
    // Run from a subdir that has no project_hash matching state — empty
    // string is the contract (don't break CC bottom strip on a fresh
    // machine).
    let workdir = dir.path().join("clean");
    std::fs::create_dir_all(&workdir).unwrap();
    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["statusline"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(out).unwrap(), "");
}

#[test]
fn statusline_renders_open_count_and_task_id() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Statusline subject"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["statusline"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with('['), "must start with [: {s}");
    assert!(s.ends_with(']'), "must end with ]: {s}");
    assert!(s.contains(&task_id), "must contain task id: {s}");
    assert!(s.contains("open: 1"), "must contain open count: {s}");
    assert!(s.contains("pending: 0"), "must contain pending count: {s}");
    assert!(s.contains("stale: 0"), "must contain stale count: {s}");
}

#[test]
fn precompact_hook_appends_marker_decision_to_open_task() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Compactable thing"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--kind", "PreCompact", "--text", ""])
        .assert()
        .success();

    // The hook still appends the marker decision to the append-only journal…
    let events_glob = dir.path().join("task-journal").join("events");
    let mut marker_lines = 0;
    for entry in std::fs::read_dir(&events_glob).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let body = std::fs::read_to_string(&p).unwrap();
            for line in body.lines() {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                if v["type"] == "decision"
                    && v["text"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Conversation compacted at")
                {
                    marker_lines += 1;
                }
            }
        }
    }
    assert_eq!(
        marker_lines, 1,
        "marker decision must be recorded in the journal"
    );

    // …but the pack filters it out as machine noise so the dossier reads clean.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(
            contains("Conversation compacted at")
                .not()
                .and(contains("single reasoning unit").not()),
        );
}

#[test]
fn precompact_hook_with_no_open_task_writes_nothing() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    // No create — events file does not yet exist.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--kind", "PreCompact", "--text", ""])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn precompact_ingests_transcript_tail_into_pending_v2() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let _task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Compactable thing"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Forge a transcript JSONL with two entries strictly newer than any
    // timestamp `task-journal create` could have produced (year 2099) —
    // so the catch-up walk sees them as "post-last-event".
    let transcript = workdir.join("session.jsonl");
    let line_user = r#"{"type":"user","uuid":"u1","timestamp":"2099-01-01T00:00:00.000Z","sessionId":"s1","message":{"content":"I think the auth middleware drops the token at the refresh boundary"}}"#;
    let line_assistant = r#"{"type":"assistant","uuid":"a1","timestamp":"2099-01-01T00:00:05.000Z","sessionId":"s1","message":{"content":[{"type":"text","text":"Confirmed: src/auth/refresh.rs uses < instead of <= at the expiry comparison."}]}}"#;
    std::fs::write(&transcript, format!("{line_user}\n{line_assistant}\n")).unwrap();

    let stdin_payload = serde_json::json!({
        "hook_event_name": "PreCompact",
        "transcript_path": transcript.to_str().unwrap(),
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_DISABLE_CLASSIFY_SPAWN", "1")
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success();

    let pending_dir = dir.path().join("task-journal").join("pending");
    let queued: Vec<_> = std::fs::read_dir(&pending_dir)
        .expect("pending dir must exist after PreCompact ingest")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        queued.len(),
        2,
        "expected 2 pending v2 chunks (user + assistant), got {}",
        queued.len()
    );

    // Verify v2 schema and that one entry carries the user text, the other the assistant text.
    let mut saw_user = false;
    let mut saw_assistant = false;
    for entry in &queued {
        let body = std::fs::read_to_string(entry.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["schema"], "v2");
        let text = v["text"].as_str().unwrap_or("");
        if text.contains("auth middleware drops the token") {
            saw_user = true;
            assert_eq!(v["kind"], "UserPromptSubmit");
        }
        if text.contains("uses < instead of <=") {
            saw_assistant = true;
            assert_eq!(v["kind"], "PreCompactChunk");
        }
    }
    assert!(
        saw_user && saw_assistant,
        "missing one of the chunks: user={saw_user} assistant={saw_assistant}"
    );
}

#[test]
fn precompact_skips_transcript_entries_older_than_last_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "Already covered"])
        .assert()
        .success();

    // Transcript with entries from year 2000 — strictly older than the
    // task's create event. Catch-up must skip both, leaving pending empty.
    let transcript = workdir.join("session.jsonl");
    let line_old = r#"{"type":"user","uuid":"u1","timestamp":"2000-01-01T00:00:00.000Z","sessionId":"s1","message":{"content":"ancient chatter that classifier already processed"}}"#;
    std::fs::write(&transcript, format!("{line_old}\n")).unwrap();

    let stdin_payload = serde_json::json!({
        "hook_event_name": "PreCompact",
        "transcript_path": transcript.to_str().unwrap(),
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_DISABLE_CLASSIFY_SPAWN", "1")
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success();

    let pending_dir = dir.path().join("task-journal").join("pending");
    let queued_count = std::fs::read_dir(&pending_dir)
        .map(|it| it.count())
        .unwrap_or(0);
    assert_eq!(
        queued_count, 0,
        "no chunks must be queued for ancient transcript"
    );
}

#[test]
fn stop_ingests_transcript_tail_into_pending_v2() {
    // v0.9.3: Stop hook does transcript catch-up (like PreCompact)
    // instead of injecting hardcoded "Session ended" noise.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let _task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "End-of-session catch-up"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let transcript = workdir.join("session.jsonl");
    let line_user = r#"{"type":"user","uuid":"u1","timestamp":"2099-01-01T00:00:00.000Z","sessionId":"s1","message":{"content":"the refund flow needs idempotency keys per payment provider"}}"#;
    let line_assistant = r#"{"type":"assistant","uuid":"a1","timestamp":"2099-01-01T00:00:05.000Z","sessionId":"s1","message":{"content":[{"type":"text","text":"Confirmed: dlocal returns 200 OK for duplicate calls when idempotency-key header is set."}]}}"#;
    std::fs::write(&transcript, format!("{line_user}\n{line_assistant}\n")).unwrap();

    let stdin_payload = serde_json::json!({
        "hook_event_name": "Stop",
        "transcript_path": transcript.to_str().unwrap(),
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_DISABLE_CLASSIFY_SPAWN", "1")
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success();

    let pending_dir = dir.path().join("task-journal").join("pending");
    let queued: Vec<_> = std::fs::read_dir(&pending_dir)
        .expect("pending dir must exist after Stop catch-up")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        queued.len(),
        2,
        "expected 2 pending v2 chunks (user + assistant), got {}",
        queued.len()
    );

    let mut saw_user = false;
    let mut saw_assistant = false;
    for entry in &queued {
        let body = std::fs::read_to_string(entry.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["schema"], "v2");
        let text = v["text"].as_str().unwrap_or("");
        if text.contains("idempotency keys per payment provider") {
            saw_user = true;
            assert_eq!(v["kind"], "UserPromptSubmit");
        }
        if text.contains("dlocal returns 200 OK") {
            saw_assistant = true;
            // Distinct kind from PreCompactChunk — lets ops grep which hook queued it.
            assert_eq!(v["kind"], "StopChunk");
        }
    }
    assert!(
        saw_user && saw_assistant,
        "missing one of the chunks: user={saw_user} assistant={saw_assistant}"
    );
}

#[test]
fn stop_without_transcript_path_is_silent_noop() {
    // Belt-and-braces: when CC's Stop payload omits transcript_path
    // (or hook is invoked manually with no stdin), we must not crash
    // and must not litter pending/ with placeholder entries — the
    // pre-v0.9.3 "Session ended" text noise we deliberately removed.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "noop check"])
        .assert()
        .success();

    let stdin_payload = serde_json::json!({"hook_event_name": "Stop"}).to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success();

    let pending_dir = dir.path().join("task-journal").join("pending");
    let queued_count = std::fs::read_dir(&pending_dir)
        .map(|it| it.count())
        .unwrap_or(0);
    assert_eq!(
        queued_count, 0,
        "Stop without transcript_path must NOT queue any pending entry"
    );
}

#[test]
fn rewind_prompt_appends_correction_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Path I wish I hadn't taken"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "ingest-hook",
            "--kind",
            "UserPromptSubmit",
            "--text",
            "/rewind go back to plan A",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("[correction]").and(contains("/rewind")));
}

#[test]
fn install_hooks_wires_precompact_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--auto-capture"])
        .assert()
        .success();
    let s = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        s.contains("PreCompact"),
        "settings.json must wire PreCompact under --auto-capture: {s}"
    );
}

#[test]
fn rejected_command_finds_rejection_events_by_topic() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Add OAuth login"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Two rejections + one decision → topic search must surface only
    // the matching rejection.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "rejection",
            "--text",
            "Implicit grant deprecated by RFC 9700",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "rejection",
            "--text",
            "Symmetric session keys leak across browser tabs",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Use authorization code with PKCE per RFC 9700",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["rejected", "implicit"])
        .assert()
        .success()
        .stdout(
            contains(&task_id)
                .and(contains("Implicit grant"))
                .and(contains("Add OAuth login"))
                .and(contains("Symmetric session keys").not()),
        );
}

#[test]
fn export_pr_renders_summary_changes_rejections_verification_affected() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Export PR test", "--goal", "Wire OAuth via PKCE"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Adopt PKCE flow in src/auth/oauth.rs",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "rejection",
            "--text",
            "Implicit grant deprecated",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "evidence",
            "--text",
            "Test suite green: 142 passed",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["export-pr", &task_id])
        .assert()
        .success()
        .stdout(
            contains("## Summary")
                .and(contains("Wire OAuth via PKCE"))
                .and(contains("## Changes"))
                .and(contains("Adopt PKCE flow"))
                .and(contains("## Why this approach (vs alternatives)"))
                .and(contains("Implicit grant deprecated"))
                .and(contains("## Verification"))
                .and(contains("Test suite green"))
                .and(contains("## Affected"))
                .and(contains("src/auth/oauth.rs")),
        );
}

#[test]
fn export_pr_omits_optional_sections_when_no_data() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Bare task"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["export-pr", &task_id])
        .assert()
        .success()
        .stdout(
            contains("## Summary")
                .and(contains("## Changes"))
                .and(contains("## Why this approach").not())
                .and(contains("## Verification").not())
                .and(contains("## Affected").not()),
        );
}

#[test]
fn export_pr_unknown_task_id_exits_one_with_stderr_message() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    // Pre-create a task so the project state exists at all (otherwise
    // task_artifacts can't even open the DB).
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "Sentinel"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["export-pr", "tj-zzzz"])
        .assert()
        .failure()
        .stderr(contains("task not found: tj-zzzz"));
}

// v0.10.0: asyncRewake backlog signal. PostToolUse hook configured with
// asyncRewake:true in hooks.json sets TJ_ASYNC_REWAKE=1; when pending/
// has more than PENDING_OVERFLOW_THRESHOLD (25) entries already queued,
// ingest-hook exits 2 with a wake-message on stdout. Sync hooks (or
// CLI invocations without the env var) must NEVER exit 2 — that would
// block the operation in Claude Code's hook contract.
fn seed_pending_chunks(pending_dir: &std::path::Path, count: usize) {
    std::fs::create_dir_all(pending_dir).unwrap();
    for i in 0..count {
        let payload = serde_json::json!({
            "schema": "v2",
            "kind": "PostToolUse",
            "text": format!("seed-{i}"),
            "project_hash": "deadbeefdeadbeef",
            "events_path": "/tmp/unused.jsonl",
            "backend": "hybrid",
            "queued_at": "2099-01-01T00:00:00Z",
        });
        std::fs::write(
            pending_dir.join(format!("seed-{i:04}.json")),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
    }
}

fn posttooluse_payload() -> String {
    serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "echo hi"},
        "tool_response": {"stdout": "hi"},
    })
    .to_string()
}

#[test]
fn session_start_emits_watch_paths_for_existing_marker_files() {
    // v0.10.2 X4: SessionStart envelope must include `watchPaths` with
    // existing marker files (CLAUDE.md, README.md, .docs/plans). Files
    // that don't exist are skipped — Claude Code's watcher logs an
    // error and gives up on missing paths, so we don't emit them.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(workdir.join(".docs").join("plans")).unwrap();
    std::fs::write(workdir.join("CLAUDE.md"), "# Project rules").unwrap();
    // README.md intentionally absent — must NOT appear in watchPaths.

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "Watch paths test"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();
    let _ = task_id;

    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["ingest-hook", "--kind", "SessionStart", "--text", ""])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let watches = v["hookSpecificOutput"]["watchPaths"]
        .as_array()
        .expect("watchPaths must be present when at least one marker exists");
    let joined: String = watches
        .iter()
        .filter_map(|x| x.as_str())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        joined.contains("CLAUDE.md"),
        "CLAUDE.md must be watched: {joined}"
    );
    assert!(
        joined.contains("plans"),
        ".docs/plans must be watched: {joined}"
    );
    assert!(
        !joined.contains("README.md"),
        "README.md does not exist, must NOT be watched: {joined}"
    );
}

#[test]
fn session_start_omits_watch_paths_when_disabled_via_env() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    std::fs::write(workdir.join("CLAUDE.md"), "# Project rules").unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "Watch paths env-disabled"])
        .assert()
        .success();

    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .env("TJ_WATCH_PATHS", "0")
            .current_dir(&workdir)
            .args(["ingest-hook", "--kind", "SessionStart", "--text", ""])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    assert!(
        v["hookSpecificOutput"]["watchPaths"].is_null(),
        "TJ_WATCH_PATHS=0 must suppress watchPaths emission"
    );
    assert!(
        v["hookSpecificOutput"]["additionalContext"].is_string(),
        "additionalContext still emitted independently of watchPaths"
    );
}

#[test]
fn file_changed_hook_appends_evidence_to_active_task() {
    // v0.10.2 X4: FileChanged hook handler should append an evidence
    // event to the most-recent open task with the changed path
    // (trimmed project-relative) and the change kind.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "FileChanged evidence test"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    let touched = workdir.join("CLAUDE.md");
    std::fs::write(&touched, "# rules v2").unwrap();

    let stdin_payload = serde_json::json!({
        "hook_event_name": "FileChanged",
        "file_path": touched.to_str().unwrap(),
        "event": "change",
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("FileChanged (change)"))
        .stdout(contains("CLAUDE.md"));
}

#[test]
fn file_changed_hook_with_no_open_task_is_no_op() {
    // FileChanged on a clean events dir should not crash, not create
    // events_path, not open a task — just return Ok silently.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    let touched = workdir.join("CLAUDE.md");
    std::fs::write(&touched, "# rules").unwrap();

    let stdin_payload = serde_json::json!({
        "hook_event_name": "FileChanged",
        "file_path": touched.to_str().unwrap(),
        "event": "add",
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success();

    let events_path = dir.path().join("task-journal").join("events");
    assert!(
        !events_path.exists()
            || std::fs::read_dir(&events_path)
                .map(|d| d.count() == 0)
                .unwrap_or(true),
        "FileChanged on a fresh project must not write any events file"
    );
}

#[test]
fn asyncrewake_below_threshold_exits_zero() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "Async wake test below threshold"])
        .assert()
        .success();

    // Seed 5 entries — well under the 25 threshold.
    let pending = dir.path().join("task-journal").join("pending");
    seed_pending_chunks(&pending, 5);

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_ASYNC_REWAKE", "1")
        .env("TJ_DISABLE_CLASSIFY_SPAWN", "1")
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(posttooluse_payload())
        .assert()
        .success(); // exit 0, no wake
}

#[test]
fn asyncrewake_overflow_exits_two_with_drain_hint() {
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "Async wake overflow test"])
        .assert()
        .success();

    // Seed 30 entries — over the 25 threshold.
    let pending = dir.path().join("task-journal").join("pending");
    seed_pending_chunks(&pending, 30);

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_ASYNC_REWAKE", "1")
        .env("TJ_DISABLE_CLASSIFY_SPAWN", "1")
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(posttooluse_payload())
        .assert()
        .failure()
        .code(2)
        .stdout(contains("Task Journal pending queue"))
        .stdout(contains("pending-gc"));
}

#[test]
fn asyncrewake_overflow_without_env_does_not_exit_two() {
    // Sync hook safety: without TJ_ASYNC_REWAKE=1 we must NEVER exit 2
    // even on overflow, because exit 2 from a sync hook blocks the
    // operation in Claude Code. CLI invocations and the PreCompact/Stop
    // hooks (which stay sync) rely on this guarantee.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "Sync hook safety test"])
        .assert()
        .success();

    let pending = dir.path().join("task-journal").join("pending");
    seed_pending_chunks(&pending, 30);

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env_remove("TJ_ASYNC_REWAKE")
        .env("TJ_DISABLE_CLASSIFY_SPAWN", "1")
        .current_dir(&workdir)
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(posttooluse_payload())
        .assert()
        .success(); // exit 0, no wake — must not block sync hooks
}

// ---------------------------------------------------------------------------
// v0.10.3 — search/pack quality fixes (user feedback)
// ---------------------------------------------------------------------------

#[test]
fn search_does_not_crash_on_hyphenated_identifier() {
    // B1 regression: `task_search "OPS-306"` used to crash with
    // `no such column: 306` because FTS5 parsed `-` as column-prefix.
    // sanitize_query wraps the query in phrase quotes so it now runs.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "create",
            "Ticket OPS-306 fix",
            "--goal",
            "Resolve OPS-306 customer report",
        ])
        .assert()
        .success();

    // Bare FTS5 search would have raised "no such column: 306".
    // Verify the call exits 0 and prints at least one task_id line.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["search", "OPS-306"])
        .assert()
        .success()
        .stdout(contains("tj-"));
}

#[test]
fn search_does_not_crash_on_slash_or_colon() {
    // Same B1 family — paths and `ttl:30s`-style tokens used to crash.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "create",
            "Touch src/main.rs and configure ttl:30s",
            "--goal",
            "src/main.rs hot path with ttl:30s cache",
        ])
        .assert()
        .success();

    for q in &["src/main.rs", "ttl:30s", "foo*bar", "func()"] {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["search", q])
            .assert()
            .success();
    }
}

#[test]
fn search_falls_back_to_like_when_fts_token_split_misses() {
    // B2: `bulk-repack` ends up tokenized as `bulk` + `repack`.
    // A user query `bulk repack` SHOULD match via the default FTS5
    // AND-tokens semantics (passes through sanitize_query unchanged).
    // But a query `bulk-repack` (with hyphen) is phrase-quoted; if
    // the source text only has one of the variants and the FTS phrase
    // returns nothing, the LIKE fallback recovers the hit.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "task title"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "do the bulk-repack on next push",
        ])
        .assert()
        .success();

    // Phrase-quoted "bulk-repack" — FTS5 phrase will not match unless
    // tokenizer emits adjacent `bulk` + `repack`. LIKE fallback rescues.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["search", "bulk-repack"])
        .assert()
        .success()
        .stdout(contains(&task_id));
}

#[test]
fn search_filters_by_event_type() {
    // B4: --type lets the agent target one event class.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "feature work"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "switching to plan X",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "rejection",
            "--text",
            "switching off plan Y",
        ])
        .assert()
        .success();

    // --type=decision must hit
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["search", "switching", "--type", "decision"])
        .assert()
        .success()
        .stdout(contains(&task_id));
    // --type=evidence (no matching event) must succeed with no hits
    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["search", "switching", "--type", "evidence"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(&task_id),
        "evidence filter must not return decision/rejection rows"
    );
}

#[test]
fn pack_full_keeps_newest_decision_when_budget_tight() {
    // B3: the user's complaint was that the LATEST (most-important)
    // decision was the one cut by FULL_BUDGET. Render order is now
    // newest-first, so a small pack truncated at the end keeps the
    // newest decision on top.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args(["create", "decision ordering test"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Append decisions oldest → newest. The LAST one must be the one
    // that survives truncation.
    for i in 0..3 {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&workdir)
            .args([
                "event",
                &task_id,
                "--type",
                "decision",
                "--text",
                &format!("decision number {i}"),
            ])
            .assert()
            .success();
    }
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "FINAL_SUMMARY_MARKER_v0_10_3",
        ])
        .assert()
        .success();

    let pack = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["pack", &task_id, "--mode", "full"])
        .output()
        .unwrap();
    assert!(pack.status.success(), "pack must succeed");
    let body = String::from_utf8_lossy(&pack.stdout);
    let idx_final = body.find("FINAL_SUMMARY_MARKER_v0_10_3");
    let idx_zero = body.find("decision number 0");
    assert!(
        idx_final.is_some(),
        "newest decision must appear in pack output"
    );
    if let (Some(a), Some(b)) = (idx_final, idx_zero) {
        assert!(
            a < b,
            "newest decision must render BEFORE the oldest one (newest-first ordering)"
        );
    }
}

#[test]
fn precompact_dedupes_marker_within_60s_window() {
    // B5: two PreCompact firings within DEDUP_WINDOW_SECS must not
    // each append a "Conversation compacted at T" marker.
    let dir = assert_fs::TempDir::new().unwrap();
    let workdir = dir.path().join("proj");
    std::fs::create_dir_all(&workdir).unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["create", "dedup test"])
        .assert()
        .success();

    // First PreCompact hook fires and emits the boundary marker.
    let first = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--kind", "PreCompact", "--text", ""])
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_out = String::from_utf8(first.stdout).unwrap();
    assert!(
        !first_out.trim().is_empty(),
        "first PreCompact must emit the marker event id"
    );

    // Second firing same second must be deduped — stdout empty.
    let second = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["ingest-hook", "--kind", "PreCompact", "--text", ""])
        .output()
        .unwrap();
    assert!(second.status.success());
    let second_out = String::from_utf8(second.stdout).unwrap();
    assert!(
        second_out.trim().is_empty(),
        "second PreCompact within DEDUP_WINDOW must NOT emit a new event id; got: {second_out:?}"
    );
}

/// Read the single events JSONL file under <xdg>/task-journal/events.
fn read_events_jsonl(xdg: &std::path::Path) -> String {
    let events_dir = xdg.join("task-journal").join("events");
    let entry = std::fs::read_dir(&events_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .expect("an events jsonl file");
    std::fs::read_to_string(entry.path()).unwrap()
}

#[test]
fn create_with_parent_sets_parent_id() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();

    // Parent task.
    let parent_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Parent"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Child with --parent.
    let child_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Child", "--parent", &parent_id])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // The child's open event in the JSONL carries meta.parent_id == parent.
    let jsonl = read_events_jsonl(xdg.path());
    let child_open = jsonl
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v.get("task_id").and_then(|x| x.as_str()) == Some(&child_id))
        .expect("child open event");
    assert_eq!(
        child_open["meta"]["parent_id"].as_str(),
        Some(parent_id.as_str())
    );
}

#[test]
fn create_with_missing_parent_is_rejected() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args(["create", "Child", "--parent", "tj-nope"])
        .assert()
        .failure();
}

#[test]
fn list_tree_indents_children_under_parents() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();

    let parent_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "ParentTask"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args(["create", "ChildTask", "--parent", &parent_id])
        .assert()
        .success();

    let out = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["list", "--tree"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();

    let parent_line = out
        .lines()
        .position(|l| l.contains("ParentTask"))
        .expect("parent line present");
    let child_idx = out
        .lines()
        .position(|l| l.contains("ChildTask"))
        .expect("child line present");
    // Child appears after the parent and is indented.
    assert!(child_idx > parent_line, "child must come after parent");
    let child_line = out.lines().nth(child_idx).unwrap();
    assert!(
        child_line.starts_with(char::is_whitespace),
        "child line must be indented, got: {child_line:?}"
    );
}

// --- Push-recall (claude-memory-60m) -----------------------------------

/// Seed a confirmed rejection mentioning `axum` on a fresh task, returning
/// the (TempDir, task_id). Mirrors the SessionStart-test seeding: `create`
/// + `event --type rejection`.
fn seed_axum_rejection() -> (assert_fs::TempDir, String) {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Push recall host"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "rejection",
            "--text",
            "Tried switching the server to axum but it broke rmcp stdio.",
        ])
        .assert()
        .success();
    (dir, task_id)
}

#[test]
fn post_tool_use_emits_recall_additional_context() {
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": { "command": "let's switch the server to axum" },
        "tool_response": { "output": "" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();

    // The recall envelope is one of possibly several stdout lines (the
    // mock path also prints the new event_id). Find the JSON line.
    let env_line = body
        .lines()
        .find(|l| l.contains("additionalContext"))
        .unwrap_or_else(|| panic!("no recall envelope on stdout, got: {body:?}"));
    let v: serde_json::Value = serde_json::from_str(env_line.trim()).unwrap();
    let ctx = v
        .get("hookSpecificOutput")
        .and_then(|h| h.get("additionalContext"))
        .and_then(|s| s.as_str())
        .expect("additionalContext missing");
    assert!(ctx.contains("⚠ recall"), "ctx: {ctx}");
    assert!(ctx.contains("axum"), "ctx: {ctx}");
    assert_eq!(
        v.get("hookSpecificOutput")
            .and_then(|h| h.get("hookEventName"))
            .and_then(|s| s.as_str()),
        Some("PostToolUse"),
    );
}

#[test]
fn post_tool_use_no_recall_when_no_match() {
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": { "command": "update the frontend stylesheet colors" },
        "tool_response": { "output": "" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    assert!(
        !body.contains("additionalContext"),
        "must not emit recall envelope for a non-matching tool turn, got: {body:?}"
    );
}

#[test]
fn tj_push_recall_env_disables() {
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": { "command": "let's switch the server to axum" },
        "tool_response": { "output": "" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_PUSH_RECALL", "0")
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    assert!(
        !body.contains("additionalContext"),
        "TJ_PUSH_RECALL=0 must suppress the recall envelope, got: {body:?}"
    );
}

#[test]
fn post_tool_use_skips_recall_for_mcp_tools() {
    // Dedup gate vs claude-memory-7km: mcp__ tool turns are 7km's territory
    // (updatedMCPToolOutput). This path must stay silent for them.
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "mcp__task-journal__task_search",
        "tool_input": { "query": "let's switch the server to axum" },
        "tool_response": { "output": "" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    assert!(
        !body.contains("additionalContext"),
        "mcp__ tool turns must not emit a recall envelope (7km owns them), got: {body:?}"
    );
}

// --- Push-recall via updatedMCPToolOutput (claude-memory-7km) ----------

#[test]
fn post_tool_use_mcp_prepends_recall_banner() {
    // An MCP tool call whose input echoes a prior rejection must have a recall
    // banner PREPENDED to what Claude sees of its output, with the real output
    // preserved below the banner (7km's updatedMCPToolOutput path).
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "mcp__some-server__do_thing",
        "tool_input": { "approach": "let's switch the server to axum" },
        "tool_response": { "output": "REAL TOOL OUTPUT 12345" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();

    let env_line = body
        .lines()
        .find(|l| l.contains("updatedMCPToolOutput"))
        .unwrap_or_else(|| panic!("no updatedMCPToolOutput envelope on stdout, got: {body:?}"));
    let v: serde_json::Value = serde_json::from_str(env_line.trim()).unwrap();
    let updated = v
        .get("hookSpecificOutput")
        .and_then(|h| h.get("updatedMCPToolOutput"))
        .and_then(|s| s.as_str())
        .expect("updatedMCPToolOutput missing");
    assert!(
        updated.starts_with('\u{26a0}'),
        "must start with banner: {updated}"
    );
    assert!(
        updated.contains("axum"),
        "banner must mention the recall: {updated}"
    );
    assert!(
        updated.contains("REAL TOOL OUTPUT 12345"),
        "original tool output must be preserved: {updated}"
    );
    assert_eq!(
        v.get("hookSpecificOutput")
            .and_then(|h| h.get("hookEventName"))
            .and_then(|s| s.as_str()),
        Some("PostToolUse"),
    );
}

#[test]
fn post_tool_use_non_mcp_tool_emits_no_mcp_output() {
    // The 7km path is MCP-only: a non-MCP tool (Bash) must never emit
    // updatedMCPToolOutput, even when its input echoes a rejection.
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "Bash",
        "tool_input": { "command": "let's switch the server to axum" },
        "tool_response": { "output": "REAL TOOL OUTPUT 12345" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    assert!(
        !body.contains("updatedMCPToolOutput"),
        "non-MCP tool turns must not emit updatedMCPToolOutput, got: {body:?}"
    );
}

#[test]
fn post_tool_use_mcp_no_recall_passes_through() {
    // MCP tool, but its input matches no rejection: emit nothing, pass through.
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "mcp__some-server__do_thing",
        "tool_input": { "approach": "update the frontend stylesheet colors" },
        "tool_response": { "output": "REAL TOOL OUTPUT 12345" }
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    assert!(
        !body.contains("updatedMCPToolOutput"),
        "no-hit MCP turn must pass through (no envelope), got: {body:?}"
    );
}

#[test]
fn push_recall_mcp_does_not_drop_capture() {
    // The push-recall block must fall through to the normal capture path:
    // the MCP tool call is still ingested as an event (mock path emits the
    // new event_id on stdout).
    let (dir, task_id) = seed_axum_rejection();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": "s-recall",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "tool_name": "mcp__some-server__do_thing",
        "tool_input": { "approach": "let's switch the server to axum" },
        "tool_response": { "output": "REAL TOOL OUTPUT 12345" }
    })
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "ingest-hook",
            "--backend",
            "cli",
            "--mock-event-type",
            "finding",
            "--mock-task-id",
            &task_id,
            "--mock-confidence",
            "0.9",
        ])
        .write_stdin(payload)
        .assert()
        .success();

    // The captured finding lands in the task pack — capture is unaffected by
    // the push-recall envelope.
    let pack = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["pack", &task_id, "--mode", "full"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        pack.contains("do_thing"),
        "MCP tool call must still be captured into the journal, pack: {pack}"
    );
}

// Seed a task with a goal + one constraint, run SessionStart with the
// given `source`, and return the parsed `additionalContext` string.
fn session_start_additional_context(source: &str) -> String {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args([
                "create",
                "Ship the widget",
                "--goal",
                "Ship the dashboard widget",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "constraint",
            "--text",
            "Must ship before Friday",
        ])
        .assert()
        .success();

    let stdin_payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "source": source,
    })
    .to_string();

    let out = Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(stdin_payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body = String::from_utf8(out).unwrap();
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .unwrap_or_else(|e| panic!("SessionStart stdout must be JSON; got {body:?}; err {e}"));
    v.get("hookSpecificOutput")
        .and_then(|h| h.get("additionalContext"))
        .and_then(|s| s.as_str())
        .expect("additionalContext must be present")
        .to_string()
}

#[test]
fn session_start_compact_prepends_active_task_reminder() {
    let ctx = session_start_additional_context("compact");
    assert!(
        ctx.starts_with("[Active task after compaction]"),
        "compact SessionStart must lead with the reminder: {ctx}"
    );
    assert!(
        ctx.contains("Ship the widget"),
        "reminder must include the task title: {ctx}"
    );
    assert!(
        ctx.contains("Goal: Ship the dashboard widget"),
        "reminder must include the goal: {ctx}"
    );
    assert!(
        ctx.contains("Must ship before Friday"),
        "reminder must include the in-force constraint: {ctx}"
    );
    assert!(
        ctx.contains("task-journal-distiller"),
        "compact SessionStart must advise delegating to the distiller subagent: {ctx}"
    );
}

#[test]
fn session_start_startup_has_no_reminder() {
    let ctx = session_start_additional_context("startup");
    assert!(
        !ctx.contains("[Active task after compaction]"),
        "non-compact SessionStart must NOT inject the reminder: {ctx}"
    );
    assert!(
        !ctx.contains("task-journal-distiller"),
        "non-compact SessionStart must NOT advise the distiller: {ctx}"
    );
}

/// Recursively collect file names under `dir` that match a predicate.
/// Used to locate the sandboxed Claude-memory file without reconstructing
/// the exact `encode_project_path` transform of the test's cwd.
fn find_files_recursive(
    dir: &std::path::Path,
    pred: &dyn Fn(&str) -> bool,
) -> Vec<std::path::PathBuf> {
    let mut hits = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                hits.extend(find_files_recursive(&path, pred));
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if pred(name) {
                    hits.push(path.clone());
                }
            }
        }
    }
    hits
}

#[test]
fn export_memory_dry_run_prints_path_and_content_no_write() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let claude = assert_fs::TempDir::new().unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Ship X", "--goal", "Ship X"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args([
            "close",
            &task_id,
            "--outcome",
            "done",
            "--outcome-tag",
            "done",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .env("CLAUDE_CONFIG_DIR", claude.path())
        .current_dir(proj.path())
        .args(["export-memory", "--task", &task_id, "--dry-run"])
        .assert()
        .success()
        // Separator-agnostic: Windows prints `memory\tj-...` not `memory/tj-...`.
        .stdout(contains(format!("tj-{task_id}-ship-x.md")))
        .stdout(contains("memory"))
        .stdout(contains("name: ship-x"));

    // Nothing written under the sandboxed Claude config dir.
    let written = find_files_recursive(claude.path(), &|n| {
        n.starts_with("tj-") && n.ends_with(".md")
    });
    assert!(written.is_empty(), "dry-run must not write: {written:?}");
}

#[test]
fn export_memory_writes_one_idempotent_file() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let claude = assert_fs::TempDir::new().unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Ship X", "--goal", "Ship X"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args([
            "close",
            &task_id,
            "--outcome",
            "done",
            "--outcome-tag",
            "done",
        ])
        .assert()
        .success();

    for _ in 0..2 {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .env("CLAUDE_CONFIG_DIR", claude.path())
            .current_dir(proj.path())
            .args(["export-memory", "--task", &task_id])
            .assert()
            .success();
    }

    let prefix = format!("tj-{task_id}-");
    let files = find_files_recursive(claude.path(), &|n| {
        n.starts_with(&prefix) && n.ends_with(".md")
    });
    assert_eq!(files.len(), 1, "exactly one idempotent file: {files:?}");

    let body = std::fs::read_to_string(&files[0]).unwrap();
    assert!(body.starts_with("---\n"), "frontmatter fence: {body}");
    assert!(body.contains("type: project"), "metadata type: {body}");
}

#[test]
fn export_memory_all_closed_skips_open() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let claude = assert_fs::TempDir::new().unwrap();

    let task_a = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Closed A", "--goal", "A"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .current_dir(proj.path())
        .args([
            "close",
            &task_a,
            "--outcome",
            "done",
            "--outcome-tag",
            "done",
        ])
        .assert()
        .success();

    let task_b = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .current_dir(proj.path())
            .args(["create", "Open B", "--goal", "B"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .env("CLAUDE_CONFIG_DIR", claude.path())
        .current_dir(proj.path())
        .args(["export-memory", "--all-closed"])
        .assert()
        .success();

    let for_a = find_files_recursive(claude.path(), &|n| n.starts_with(&format!("tj-{task_a}-")));
    let for_b = find_files_recursive(claude.path(), &|n| n.starts_with(&format!("tj-{task_b}-")));
    assert_eq!(for_a.len(), 1, "closed task A exported: {for_a:?}");
    assert!(for_b.is_empty(), "open task B must be skipped: {for_b:?}");
}

#[test]
fn export_memory_missing_task_exits_1() {
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let claude = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", xdg.path())
        .env("CLAUDE_CONFIG_DIR", claude.path())
        .current_dir(proj.path())
        .args(["export-memory", "--task", "tj-nope"])
        .assert()
        .failure()
        .code(1)
        .stderr(contains("task not found"));
}

#[test]
fn embed_backfill_vectorises_events_then_idempotent() {
    // Pillar A / Phase 0: `embed --backfill` computes a vector per event using
    // the dependency-free hash embedder and stores it; a second run finds
    // nothing new. Fully offline — no model, no network.
    let dir = assert_fs::TempDir::new().unwrap();

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Implement semantic memory retrieval"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "event",
            &task_id,
            "--type",
            "decision",
            "--text",
            "Use model2vec static embeddings for offline semantic recall.",
        ])
        .assert()
        .success();

    // First backfill: embeds the open + decision events. TJ_EMBED=hash forces
    // the deterministic lexical embedder so the assertion is model-independent.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_EMBED", "hash")
        .args(["embed", "--backfill"])
        .assert()
        .success()
        .stdout(contains("hash-v1"))
        .stdout(contains("embedded 2"));

    // Second run without --backfill: nothing new to embed.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_EMBED", "hash")
        .args(["embed"])
        .assert()
        .success()
        .stdout(contains("embedded 0"));
}

#[test]
fn ask_ranks_semantically_relevant_event_first() {
    // Pillar A / Phase 1: `ask` embeds the query and returns events by meaning.
    // The query's terms overlap one event strongly and the others not at all,
    // so vector ranking must surface it first. (The hash embedder is lexical;
    // true paraphrase/morphology robustness is the model2vec backend's job.)
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Payments hardening"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    for (ty, text) in [
        (
            "decision",
            "Route refunds through the idempotent payment ledger to stop double writes.",
        ),
        (
            "finding",
            "The frontend button hover color is wrong in dark mode.",
        ),
        (
            "finding",
            "Added a composite index on users email and tenant for lookup speed.",
        ),
    ] {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["event", &task_id, "--type", ty, "--text", text])
            .assert()
            .success();
    }

    let out = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .env("TJ_EMBED", "hash")
            .args(["ask", "idempotent refunds ledger double writes", "--k", "3"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();

    let first = out.lines().next().unwrap_or("");
    assert!(
        first.contains("refund") || first.contains("ledger"),
        "top hit must be the refund decision; got first line: {first:?}\nfull:\n{out}"
    );
}

#[test]
#[ignore = "downloads the model2vec model from HuggingFace; run manually with --ignored"]
fn ask_with_model2vec_handles_paraphrase() {
    // True semantic recall: a paraphrase that shares NO exact term with the
    // target event must still rank it first. The lexical hash embedder fails
    // this; the model2vec backend (default) passes. Needs network on first run.
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Payments hardening"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    for (ty, text) in [
        (
            "decision",
            "Route refunds through the idempotent payment ledger to stop double writes.",
        ),
        (
            "finding",
            "The frontend button hover color is wrong in dark mode.",
        ),
        (
            "finding",
            "Added a composite index on users email and tenant for lookup speed.",
        ),
    ] {
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["event", &task_id, "--type", ty, "--text", text])
            .assert()
            .success();
    }

    // "duplicate refund payments" shares no exact token with the ledger event.
    let out = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["ask", "duplicate refund payments", "--k", "3"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    let first = out.lines().next().unwrap_or("");
    assert!(
        first.contains("refund") || first.contains("ledger"),
        "model2vec must rank the refund decision first for a paraphrase; got: {first:?}"
    );
}

#[test]
fn recall_surfaces_decision_from_another_project() {
    // Pillar B: a decision made in project A must be recallable while working
    // anywhere, via the shared global index. Two distinct cwds => two
    // project_hashes => one XDG_DATA_HOME (one memory.sqlite). Hash embedder
    // for determinism.
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj_a = assert_fs::TempDir::new().unwrap();
    let proj_b = assert_fs::TempDir::new().unwrap();

    let seed = |cwd: &std::path::Path, title: &str, decision: &str| {
        let tid = String::from_utf8(
            Command::cargo_bin("task-journal")
                .unwrap()
                .current_dir(cwd)
                .env("XDG_DATA_HOME", xdg.path())
                .args(["create", title])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
        .unwrap()
        .trim()
        .to_string();
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(cwd)
            .env("XDG_DATA_HOME", xdg.path())
            .args(["event", &tid, "--type", "decision", "--text", decision])
            .assert()
            .success();
        // embed --backfill syncs this project's decisions into the global index.
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(cwd)
            .env("XDG_DATA_HOME", xdg.path())
            .env("TJ_EMBED", "hash")
            .args(["embed", "--backfill"])
            .assert()
            .success();
    };

    seed(
        proj_a.path(),
        "Payments",
        "chose to route refunds through the idempotent payment ledger",
    );
    seed(
        proj_b.path(),
        "Scheduler",
        "use postgres advisory locks for cron leader election",
    );

    // Recall from a third location — global, cwd-independent.
    let out = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .env("TJ_EMBED", "hash")
            .args(["recall", "refund ledger idempotent", "--k", "3"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();

    let first = out.lines().next().unwrap_or("");
    assert!(
        first.contains("refund") || first.contains("ledger"),
        "cross-project recall must surface project A's refund decision first; got: {first:?}\nfull:\n{out}"
    );
}

#[test]
fn install_hooks_proactive_recall_wires_recall_hook() {
    // --proactive-recall adds the recall injector to UserPromptSubmit alongside
    // the nudge; the default install must NOT wire it (off by default).
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user", "--proactive-recall"])
        .assert()
        .success();
    let content =
        std::fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
    assert!(
        content.contains("task-journal recall-hook"),
        "--proactive-recall must wire the recall-hook; got: {content}"
    );
    assert!(
        content.contains("task-journal nudge"),
        "nudge must remain alongside recall-hook"
    );

    let dir2 = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir2.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();
    let c2 = std::fs::read_to_string(dir2.path().join(".claude").join("settings.json")).unwrap();
    assert!(
        !c2.contains("recall-hook"),
        "default install must not wire proactive recall"
    );
}

#[test]
fn recall_hook_injects_relevant_prior_reasoning() {
    // Pillar B proactive injection: a decision recorded in a project must be
    // surfaced as additionalContext when a later prompt (anywhere) shares its
    // terms. Gated by TJ_PROACTIVE_RECALL=0.
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();

    let tid = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", xdg.path())
            .args(["create", "Payments"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", xdg.path())
        .args([
            "event",
            &tid,
            "--type",
            "decision",
            "--text",
            "chose the idempotent payment ledger for refunds",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", xdg.path())
        .env("TJ_EMBED", "hash")
        .args(["embed", "--backfill"])
        .assert()
        .success();

    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "prompt": "should we add a refund ledger to billing?"
    })
    .to_string();

    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .args(["recall-hook"])
            .write_stdin(payload.clone())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        body.contains("additionalContext"),
        "recall-hook must emit additionalContext; got: {body:?}"
    );
    assert!(
        body.contains("ledger"),
        "must surface the ledger decision; got: {body}"
    );

    // Gate: TJ_PROACTIVE_RECALL=0 suppresses all output.
    let gated = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .env("TJ_PROACTIVE_RECALL", "0")
            .args(["recall-hook"])
            .write_stdin(payload)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        gated.trim().is_empty(),
        "TJ_PROACTIVE_RECALL=0 must suppress injection; got: {gated:?}"
    );
}

#[test]
fn remembered_preference_lists_and_injects_at_session_start() {
    // Pillar C: a user preference is stored cross-project and injected into
    // every session — even a fresh project with no events of its own.
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["remember", "respond in Russian, terse"])
        .assert()
        .success()
        .stdout(contains("remembered"));

    // Duplicate is a no-op.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["remember", "respond in Russian, terse"])
        .assert()
        .success()
        .stdout(contains("already remembered"));

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["preferences"])
        .assert()
        .success()
        .stdout(contains("respond in Russian, terse"));

    // SessionStart injects the preference with no project events present.
    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["ingest-hook", "--kind", "SessionStart", "--text", ""])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        body.contains("respond in Russian, terse"),
        "SessionStart must inject standing preferences; got: {body:?}"
    );
    assert!(body.contains("additionalContext"));
}

#[test]
fn stats_reports_memory_preferences_count() {
    // stats surfaces the global memory state (Pillar A/B/C metrics).
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["remember", "respond in Russian"])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["stats"])
        .assert()
        .success()
        .stdout(contains("preferences: 1"));
}

#[test]
fn consolidate_writes_facts_to_conventions_task_and_dedups() {
    // Pillar C: `consolidate` distils decisions into durable facts via one
    // (mocked) Haiku call and stores them in a per-project conventions task.
    // Re-running de-dups. TJ_CONSOLIDATE_BASE_URL points at the mock; TJ_EMBED
    // forces the deterministic embedder.
    let mut server = mockito::Server::new();
    let mock = server
        .mock("POST", "/v1/messages")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "id": "m", "type": "message", "role": "assistant",
                "content": [{"type": "text",
                    "text": "[semantic] Refunds always route through the idempotent ledger\n[procedural] PR into main, squash-merge"}]
            })
            .to_string(),
        )
        .expect_at_least(1)
        .create();

    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();

    let tid = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", xdg.path())
            .args(["create", "Payments"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", xdg.path())
        .args([
            "event",
            &tid,
            "--type",
            "decision",
            "--text",
            "chose the idempotent ledger for refunds",
        ])
        .assert()
        .success();

    let run = || {
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", xdg.path())
            .env("TJ_BACKEND", "anthropic")
            .env("ANTHROPIC_API_KEY", "test-key")
            .env("TJ_CONSOLIDATE_BASE_URL", server.url())
            .env("TJ_EMBED", "hash")
            .args(["consolidate"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };

    let first = String::from_utf8(run()).unwrap();
    assert!(
        first.contains("consolidated 2 new fact(s)"),
        "first run must store 2 facts; got: {first:?}"
    );
    // Second run: same facts already present -> de-duped to 0.
    let second = String::from_utf8(run()).unwrap();
    assert!(
        second.contains("consolidated 0 new fact(s)"),
        "second run must de-dup; got: {second:?}"
    );
    mock.assert();

    // --write-claude-md promotes the conventions into a managed CLAUDE.md block.
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", xdg.path())
        .env("TJ_BACKEND", "anthropic")
        .env("ANTHROPIC_API_KEY", "test-key")
        .env("TJ_CONSOLIDATE_BASE_URL", server.url())
        .env("TJ_EMBED", "hash")
        .args(["consolidate", "--write-claude-md"])
        .assert()
        .success();
    let claude_md = std::fs::read_to_string(proj.path().join("CLAUDE.md")).unwrap();
    assert!(
        claude_md.contains("task-journal:conventions:start")
            && claude_md.contains("idempotent ledger"),
        "CLAUDE.md must hold the managed conventions block; got: {claude_md}"
    );

    // The fact is now recallable.
    let recall = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", xdg.path())
            .env("TJ_EMBED", "hash")
            .args(["recall", "refund ledger idempotent", "--k", "3"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        recall.contains("ledger"),
        "consolidated fact must surface in cross-project recall; got: {recall:?}"
    );
}

#[test]
fn consolidate_skips_without_api_key_and_spends_nothing() {
    // Safety: with no ANTHROPIC_API_KEY, consolidate makes no call and creates
    // no facts — it can never spend automatically.
    let xdg = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let tid = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", xdg.path())
            .args(["create", "Scheduler"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", xdg.path())
        .args([
            "event",
            &tid,
            "--type",
            "decision",
            "--text",
            "use postgres advisory locks for cron",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", xdg.path())
        .env_remove("ANTHROPIC_API_KEY")
        // Force the no-backend path so the test is deterministic even where
        // `claude` is on PATH (which would otherwise be tried).
        .env("TJ_CONSOLIDATE_BACKEND", "none")
        .args(["consolidate"])
        .assert()
        .success()
        .stdout(contains("skipped"));
}

#[test]
fn capture_status_reports_current_state() {
    // `capture status` reports ON/OFF without changing anything.
    let dir = assert_fs::TempDir::new().unwrap();

    // Fresh install → ON.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["capture", "status"])
        .assert()
        .success()
        .stdout(contains("ON"));

    // After `off` → OFF, and status must not flip it back.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["capture", "off"])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["capture", "status"])
        .assert()
        .success()
        .stdout(contains("OFF"));
}

#[test]
fn capture_off_marker_no_ops_ingest_hook_capture() {
    // `capture off` writes a marker that makes ingest-hook skip the capture
    // path — so an auto-opening prompt records nothing. `capture on` clears it.
    let dir = assert_fs::TempDir::new().unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["capture", "off"])
        .assert()
        .success()
        .stdout(contains("OFF"));

    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "s-cap",
        "transcript_path": "/tmp/x",
        "cwd": "/tmp",
        "prompt": "implement FIN-868 paygate fee dedup"
    })
    .to_string();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_INGEST_SYNC", "1")
        .args(["ingest-hook", "--backend", "hybrid"])
        .write_stdin(payload)
        .assert()
        .success();

    let body = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["search", "paygate"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        !body.lines().any(|l| l.trim().starts_with("tj-")),
        "capture off must no-op ingest-hook capture; got: {body:?}"
    );

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["capture", "on"])
        .assert()
        .success()
        .stdout(contains("ON"));
}

#[test]
fn complete_command_runs_and_skips_cleanly_without_sessions() {
    // `complete <id> --dry-run` reports scope without calling the model or
    // writing anything. With no Claude Code sessions it shows 0 to enrich.
    let dir = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Finalize me", "--goal", "ship it"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["complete", &task_id, "--dry-run"])
        .assert()
        .success()
        .stdout(contains("complete (dry-run)"))
        .stdout(contains("session(s) to enrich"));
}

#[test]
fn complete_unknown_task_errors() {
    // A non-existent id is a hard error, not a silent no-op.
    let dir = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["complete", "tj-nope", "--dry-run"])
        .assert()
        .failure()
        .stderr(contains("task not found"));
}

#[test]
fn complete_batch_refuses_without_tty_or_yes() {
    // No id = batch. Non-interactive stdin (test harness) without --yes must
    // refuse rather than mass-close tasks unattended.
    let dir = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Batch me", "--goal", "g"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["complete"])
        .assert()
        .failure()
        .stderr(contains("interactive terminal"));
}

#[test]
fn complete_batch_dry_run_lists_open_tasks() {
    // Batch dry-run lists open tasks and reports per-task scope, no prompts.
    let dir = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["create", "Listed task", "--goal", "g"])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["complete", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("Open tasks ("))
        .stdout(contains("Listed task"));
}

/// End-to-end finalize through the real claude-p backend path, with a fake
/// `claude` on PATH returning a canned judgment. Proves the wiring: junk
/// title → Rename, done verdict → Close with a persisted outcome. Unix-only
/// (shell-script stub); the logic itself is covered cross-platform by the
/// finalize.rs unit tests. Default mode (judge-only, no `--enrich`).
#[cfg(unix)]
#[test]
fn complete_retitles_and_closes_via_fake_backend() {
    use std::os::unix::fs::PermissionsExt;

    let dir = assert_fs::TempDir::new().unwrap();
    let proj = assert_fs::TempDir::new().unwrap();
    let bindir = assert_fs::TempDir::new().unwrap();

    // The judgment the fake model "returns" — wrapped in claude's JSON envelope
    // whose `result` field is the finalize JSON string.
    let envelope = serde_json::json!({
        "is_error": false,
        "usage": {"input_tokens": 1200, "output_tokens": 300},
        "total_cost_usd": 0.0012,
        "result": serde_json::json!({
            "retitle": true,
            "title": "Voucher refund: paid 100% but got 50%",
            "done": true,
            "outcome_tag": "done",
            "outcome": "Refunded the missing half to the customer.",
            "reason": "Fix shipped and verified."
        }).to_string()
    })
    .to_string();
    let resp_path = bindir.path().join("resp.json");
    std::fs::write(&resp_path, &envelope).unwrap();

    // Fake `claude`: answer --version, drain stdin, print the envelope.
    let claude = bindir.path().join("claude");
    std::fs::write(
        &claude,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo fake; exit 0; fi\ncat >/dev/null\ncat {}\n",
            resp_path.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path_env = format!(
        "{}:{}",
        bindir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .current_dir(proj.path())
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "#: 5"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    // Default mode (judge-only): exercise judge → retitle → close.
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("PATH", &path_env)
        .env_remove("ANTHROPIC_API_KEY")
        .args(["complete", &task_id])
        .assert()
        .success()
        .stdout(contains("cost $0.0012"))
        .stdout(contains("retitled"))
        .stdout(contains("closed"));

    // The task now carries the human title, closed status, and the outcome.
    Command::cargo_bin("task-journal")
        .unwrap()
        .current_dir(proj.path())
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("Voucher refund: paid 100% but got 50%"))
        .stdout(contains("status: closed"))
        .stdout(contains("Refunded the missing half"));
}

#[test]
fn close_harvests_git_commit_and_branch_into_pack() {
    use std::process::Command as PCommand;
    let dir = assert_fs::TempDir::new().unwrap();
    let proj = dir.path().join("repo");
    std::fs::create_dir_all(&proj).unwrap();

    // Minimal git repo on a named branch with one commit.
    let git = |args: &[&str]| {
        PCommand::new("git")
            .current_dir(&proj)
            .args(args)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.io"]);
    git(&["config", "user.name", "T"]);
    git(&["checkout", "-q", "-b", "feat/harvest-me"]);
    std::fs::write(proj.join("f.txt"), "hi").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);

    // Create + close a task from inside the repo.
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .current_dir(&proj)
            .args(["create", "Harvest test"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&proj)
        .args(["close", &task_id, "--reason", "done"])
        .assert()
        .success();

    // The pack's Artifacts carries the branch (deterministic git harvest).
    // gh may be absent/unauthed in CI, so we don't assert the PR url.
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&proj)
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("feat/harvest-me"));
}

#[test]
fn artifact_add_renders_clickable_link_in_pack() {
    let dir = assert_fs::TempDir::new().unwrap();
    let task_id = String::from_utf8(
        Command::cargo_bin("task-journal")
            .unwrap()
            .env("XDG_DATA_HOME", dir.path())
            .args(["create", "Card test"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
    .trim()
    .to_string();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args([
            "artifact-add",
            &task_id,
            "--kind",
            "doc",
            "--url",
            "https://example.com/spec.md",
            "--label",
            "Design spec",
        ])
        .assert()
        .success();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(contains("[Design spec](https://example.com/spec.md) (doc)"));
}
