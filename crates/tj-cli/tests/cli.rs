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
    assert!(content.contains("UserPromptSubmit"));
    assert!(content.contains("PostToolUse"));
    assert!(content.contains("task-journal ingest-hook"));
    assert!(
        content.contains("SessionStart"),
        "install-hooks must wire SessionStart so resume-pack injection works"
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
    assert!(after_install.contains("UserPromptSubmit"));

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

#[test]
fn install_hooks_with_classifier_command_writes_env() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args([
            "install-hooks",
            "--scope",
            "user",
            "--classifier-command",
            "aimux run dt",
        ])
        .assert()
        .success();
    let content = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        v.get("env")
            .and_then(|e| e.get("TJ_CLASSIFIER_CLI"))
            .and_then(|s| s.as_str()),
        Some("aimux run dt"),
        "env.TJ_CLASSIFIER_CLI must be set: {content}"
    );
}

#[test]
fn install_hooks_without_classifier_command_does_not_set_env() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();
    let content = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        v.get("env")
            .and_then(|e| e.get("TJ_CLASSIFIER_CLI"))
            .is_none(),
        "TJ_CLASSIFIER_CLI must NOT be present when flag not passed: {content}"
    );
}

#[test]
fn install_hooks_uninstall_removes_classifier_env_but_preserves_others() {
    let dir = assert_fs::TempDir::new().unwrap();
    let claude_dir = dir.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({ "env": { "OTHER_KEY": "keep_me" } }).to_string(),
    )
    .unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args([
            "install-hooks",
            "--scope",
            "user",
            "--classifier-command",
            "aimux run dt",
        ])
        .assert()
        .success();
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
        "TJ_CLASSIFIER_CLI must be removed on uninstall: {after}"
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
        .args(["ingest-hook", "--backend", "cli"])
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
        .args(["ingest-hook", "--backend", "cli"])
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
        .args(["ingest-hook", "--backend", "cli"])
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
        .args(["ingest-hook", "--backend", "cli"])
        .write_stdin(payload)
        .assert()
        .success();
    let elapsed = start.elapsed();

    // Generous budget — the hook itself does almost no work; the
    // classifier subprocess runs in the detached worker. Pre-fix,
    // this took 5-30s. Post-fix, expect well under 1s; assert <2s
    // so flaky CI doesn't fail us.
    assert!(
        elapsed < std::time::Duration::from_millis(2000),
        "ingest-hook must return in <2s in async mode, took {elapsed:?}"
    );

    // A v2 pending entry must have been written. We don't assert
    // worker progress — worker is detached and may or may not have
    // finished by the time we look.
    let pending = dir.path().join("task-journal").join("pending");
    assert!(pending.exists(), "pending dir must exist after queuing");
    let entries: Vec<_> = std::fs::read_dir(&pending)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().extension().and_then(|s| s.to_str()) == Some("json")
        })
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
    let project_hash =
        tj_core::project_hash::from_path(&cwd).expect("compute project hash");
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
        "backend": "cli",
        "queued_at": "2026-05-08T00:00:00Z",
    });
    std::fs::write(&entry, body.to_string()).unwrap();

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .env("TJ_CLASSIFIER_CLI", "/bin/false")
        .args(["classify-worker", "--backend", "cli"])
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
    let project_hash =
        tj_core::project_hash::from_path(&cwd).expect("compute project hash");

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
            "backend": "cli",
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
        .args(["classify-worker", "--backend", "cli"])
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

    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args(["pack", &task_id, "--mode", "full"])
        .assert()
        .success()
        .stdout(
            contains("[decision]")
                .and(contains("Conversation compacted at"))
                .and(contains("single reasoning unit")),
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
        .stdout(
            contains("[correction]").and(contains("/rewind")),
        );
}

#[test]
fn install_hooks_wires_precompact_event() {
    let dir = assert_fs::TempDir::new().unwrap();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("HOME", dir.path())
        .args(["install-hooks", "--scope", "user"])
        .assert()
        .success();
    let s = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert!(
        s.contains("PreCompact"),
        "settings.json must wire PreCompact: {s}"
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
            "event", &task_id, "--type", "rejection",
            "--text", "Implicit grant deprecated by RFC 9700",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event", &task_id, "--type", "rejection",
            "--text", "Symmetric session keys leak across browser tabs",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event", &task_id, "--type", "decision",
            "--text", "Use authorization code with PKCE per RFC 9700",
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
            .args([
                "create",
                "Export PR test",
                "--goal",
                "Wire OAuth via PKCE",
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
        .current_dir(&workdir)
        .args([
            "event", &task_id, "--type", "decision",
            "--text", "Adopt PKCE flow in src/auth/oauth.rs",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event", &task_id, "--type", "rejection",
            "--text", "Implicit grant deprecated",
        ])
        .assert()
        .success();
    Command::cargo_bin("task-journal")
        .unwrap()
        .env("XDG_DATA_HOME", dir.path())
        .current_dir(&workdir)
        .args([
            "event", &task_id, "--type", "evidence",
            "--text", "Test suite green: 142 passed",
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
