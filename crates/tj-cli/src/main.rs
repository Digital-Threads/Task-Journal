use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command as PCommand;

mod tui;

/// Diagnostic snapshot returned by `task-journal doctor`. Fields are
/// stable enough for scripting against `--json`. `issues` is the empty
/// list when everything looks healthy.
#[derive(Serialize)]
struct DoctorReport {
    task_journal_version: &'static str,
    claude_in_path: bool,
    claude_version: Option<String>,
    /// Informational only: needed just for `--backend codex`.
    codex_in_path: bool,
    data_dir: PathBuf,
    events_dir: PathBuf,
    state_dir: PathBuf,
    metrics_dir: PathBuf,
    events_dir_writable: bool,
    state_dir_writable: bool,
    metrics_dir_writable: bool,
    known_projects: Vec<String>,
    schema_versions_applied: Vec<i64>,
    /// Hard problems that block normal use (non-writable dirs, broken
    /// schema, missing files, etc.). A non-empty `issues` list causes
    /// `task-journal doctor` to exit with code 1.
    issues: Vec<String>,
    /// Soft observations: install hints, optional dependencies missing,
    /// configuration suggestions. Always exits 0 even if non-empty —
    /// these are informational, not errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

impl DoctorReport {
    fn print_human(&self) {
        println!("task-journal doctor");
        println!("  version          {}", self.task_journal_version);
        println!(
            "  claude binary    {}",
            if self.claude_in_path {
                self.claude_version
                    .clone()
                    .unwrap_or_else(|| "found (version unknown)".into())
            } else {
                "NOT FOUND in PATH".into()
            }
        );
        println!(
            "  codex binary     {}",
            if self.codex_in_path {
                "found"
            } else {
                "not found in PATH (only needed for the codex backend)"
            }
        );
        println!("  data dir         {}", self.data_dir.display());
        println!(
            "  events dir       {} ({})",
            self.events_dir.display(),
            if self.events_dir_writable {
                "writable"
            } else {
                "NOT writable"
            }
        );
        println!(
            "  state dir        {} ({})",
            self.state_dir.display(),
            if self.state_dir_writable {
                "writable"
            } else {
                "NOT writable"
            }
        );
        println!(
            "  metrics dir      {} ({})",
            self.metrics_dir.display(),
            if self.metrics_dir_writable {
                "writable"
            } else {
                "NOT writable"
            }
        );
        println!("  known projects   {}", self.known_projects.len());
        if !self.schema_versions_applied.is_empty() {
            let v: Vec<String> = self
                .schema_versions_applied
                .iter()
                .map(|n| format!("v{n:03}"))
                .collect();
            println!("  schema (current) {}", v.join(", "));
        }
        if !self.notes.is_empty() {
            println!("\nℹ {} note(s):", self.notes.len());
            for n in &self.notes {
                println!("  - {n}");
            }
        }
        if self.issues.is_empty() {
            println!("\n✓ all checks passed");
        } else {
            println!("\n✗ {} issue(s):", self.issues.len());
            for i in &self.issues {
                println!("  - {i}");
            }
        }
    }
}

fn dir_writable(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".tj-doctor-write-probe");
    let r = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    r
}

/// Read a project's JSONL event log. Malformed lines are skipped with a
/// warning on stderr, the same policy as `rebuild_state`, so one bad line
/// cannot abort a read-only command.
fn read_events_lenient(
    path: &std::path::Path,
    command: &str,
) -> Result<Vec<tj_core::event::Event>> {
    let body = std::fs::read_to_string(path)?;
    let mut events = Vec::new();

    for (i, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(e) => events.push(e),
            Err(err) => eprintln!(
                "warning: skipping malformed JSONL line {} in {command}: {err}",
                i + 1
            ),
        }
    }

    Ok(events)
}

/// Move all on-disk data for one project_hash to another. Used by the
/// `migrate-project` subcommand when a project's directory has been
/// moved on disk and the canonical-path hash no longer matches.
fn run_migrate_project(from: &std::path::Path, to: &std::path::Path, force: bool) -> Result<()> {
    let from_hash = tj_core::project_hash::from_path(from)
        .with_context(|| format!("compute project_hash for --from {from:?}"))?;
    let to_hash = tj_core::project_hash::from_path(to)
        .with_context(|| format!("compute project_hash for --to {to:?}"))?;

    if from_hash == to_hash {
        anyhow::bail!(
            "--from and --to resolve to the same project_hash ({from_hash}) — nothing to migrate"
        );
    }

    let events_dir = tj_core::paths::events_dir()?;
    let state_dir = tj_core::paths::state_dir()?;
    let metrics_dir = tj_core::paths::metrics_dir()?;

    // (source, destination) tuples to attempt to rename. The SQLite runs in
    // WAL mode, so its `-wal` / `-shm` sidecars travel with it.
    let pairs = [
        (
            events_dir.join(format!("{from_hash}.jsonl")),
            events_dir.join(format!("{to_hash}.jsonl")),
        ),
        (
            state_dir.join(format!("{from_hash}.sqlite")),
            state_dir.join(format!("{to_hash}.sqlite")),
        ),
        (
            state_dir.join(format!("{from_hash}.sqlite-wal")),
            state_dir.join(format!("{to_hash}.sqlite-wal")),
        ),
        (
            state_dir.join(format!("{from_hash}.sqlite-shm")),
            state_dir.join(format!("{to_hash}.sqlite-shm")),
        ),
        (
            metrics_dir.join(format!("{from_hash}.jsonl")),
            metrics_dir.join(format!("{to_hash}.jsonl")),
        ),
    ];

    // Pre-flight: refuse overwrite of any destination unless --force.
    if !force {
        for (_src, dst) in &pairs {
            if dst.exists() {
                anyhow::bail!(
                    "destination already exists: {} — pass --force to overwrite",
                    dst.display()
                );
            }
        }
    }

    // Fold uncheckpointed writes into the main file before it moves. When
    // no one else holds the DB, closing this connection also deletes the
    // sidecars; any that survive are moved with it below.
    let src_state_path = state_dir.join(format!("{from_hash}.sqlite"));
    if src_state_path.exists() {
        let conn = rusqlite::Connection::open(&src_state_path)?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .with_context(|| format!("checkpoint WAL of {src_state_path:?}"))?;
    }

    let mut moved: Vec<String> = Vec::new();
    for (src, dst) in &pairs {
        if !src.exists() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // No remove-then-rename: rename replaces an existing destination
        // atomically (POSIX rename, MOVEFILE_REPLACE_EXISTING on Windows), so
        // a failed move under --force leaves the destination intact.
        std::fs::rename(src, dst).with_context(|| format!("rename {src:?} -> {dst:?}"))?;
        moved.push(dst.display().to_string());
    }

    // Re-key the project_hash columns inside the (now renamed) SQLite.
    let new_state_path = state_dir.join(format!("{to_hash}.sqlite"));
    if new_state_path.exists() {
        let conn = tj_core::db::open(&new_state_path)?;
        for table in ["tasks", "index_state", "embeddings", "dream_state"] {
            conn.execute(
                &format!("UPDATE {table} SET project_hash = ?1 WHERE project_hash = ?2"),
                rusqlite::params![to_hash, from_hash],
            )?;
        }
    }

    // The global cross-project index keys its rows by project_hash too.
    let memory_path = tj_core::paths::memory_db()?;
    if memory_path.exists() {
        tj_core::memory::open(&memory_path)?.execute(
            "UPDATE global_memory SET project_hash = ?1 WHERE project_hash = ?2",
            rusqlite::params![to_hash, from_hash],
        )?;
    }

    if moved.is_empty() {
        println!("no on-disk data found for project_hash {from_hash} — nothing to migrate");
    } else {
        println!("migrated {} file(s):", moved.len());
        for path in moved {
            println!("  {path}");
        }
        println!("  project_hash {from_hash} -> {to_hash}");
    }
    Ok(())
}

/// Minimal HTML attribute/text escape. Five characters cover the body of
/// `text/html` for our use case (no script context, no URL emission).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const HTML_TIMELINE_CSS: &str = r#"
:root { color-scheme: light dark; --fg:#222; --bg:#fafafa; --muted:#666; --accent:#0366d6; }
@media (prefers-color-scheme: dark) { :root { --fg:#eee; --bg:#1a1a1a; --muted:#999; --accent:#58a6ff; } }
* { box-sizing: border-box; }
body { font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
       color: var(--fg); background: var(--bg); margin: 0; padding: 1.5rem; }
header h1 { margin: 0 0 1.5rem; font-size: 1.4rem; }
article { margin-bottom: 2rem; padding: 1rem 1.25rem; background: rgba(127,127,127,0.07);
          border-radius: 6px; }
article h2 { margin: 0; font-size: 1.05rem; font-weight: 600; }
.tid { font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
       color: var(--accent); margin-right: 0.4em; }
.meta { color: var(--muted); font-size: 0.85rem; margin: 0.25rem 0 0.75rem; }
ol.timeline { list-style: none; margin: 0; padding-left: 0; }
ol.timeline li { padding: 0.4rem 0; border-top: 1px solid rgba(127,127,127,0.15); }
ol.timeline li:first-child { border-top: none; }
time { font-family: ui-monospace, monospace; color: var(--muted); margin-right: 0.6em; }
.type { display: inline-block; padding: 0 0.35em; margin-right: 0.4em; border-radius: 3px;
        font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.05em;
        background: rgba(127,127,127,0.15); }
.type-decision { background: rgba(3,102,214,0.18); color: var(--accent); }
.type-rejection { background: rgba(214,3,3,0.18); }
.type-evidence { background: rgba(40,167,69,0.18); }
.type-finding { background: rgba(255,166,0,0.20); }
.suggested::after { content: " ?"; color: var(--muted); }
"#;

/// Title and status of one task for the md/html export, folded over its
/// events with the same rules as the SQLite projection
/// (`tj_core::db::upsert_task_from_event`): the first `open` sets the title,
/// a later `rename` replaces it, and the last `close`/`reopen` decides status.
fn export_title_and_status(task_events: &[&tj_core::event::Event]) -> (String, &'static str) {
    use tj_core::event::EventType;

    let mut title: Option<String> = None;
    let mut status = "open";

    for e in task_events {
        let named = || {
            e.meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&e.text)
                .to_string()
        };
        match e.event_type {
            EventType::Open if title.is_none() => title = Some(named()),
            EventType::Rename if title.is_some() => title = Some(named()),
            EventType::Close => status = "closed",
            EventType::Reopen => status = "open",
            _ => {}
        }
    }

    (title.unwrap_or_else(|| "(untitled)".into()), status)
}

fn render_html_timeline(events: &[&tj_core::event::Event]) -> String {
    use std::collections::BTreeMap;

    let mut tasks: BTreeMap<String, Vec<&tj_core::event::Event>> = BTreeMap::new();
    for e in events {
        tasks.entry(e.task_id.clone()).or_default().push(e);
    }

    let mut out = String::new();
    out.push_str("<!doctype html>\n");
    out.push_str("<html lang=\"en\"><head>");
    out.push_str("<meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    out.push_str("<title>Task Journal — Export</title>");
    out.push_str("<style>");
    out.push_str(HTML_TIMELINE_CSS);
    out.push_str("</style>");
    out.push_str("</head><body>");
    out.push_str("<header><h1>Task Journal — Export</h1></header>");
    out.push_str("<main>");

    for (task_id, task_events) in &tasks {
        let (title, status) = export_title_and_status(task_events);

        let created = task_events
            .first()
            .map(|e| e.timestamp.as_str())
            .unwrap_or("?");

        out.push_str("<article>");
        out.push_str(&format!(
            "<h2><span class=\"tid\">{}</span>{}</h2>",
            html_escape(task_id),
            html_escape(&title)
        ));
        out.push_str(&format!(
            "<p class=\"meta\">status: {} · created: {}</p>",
            status,
            html_escape(created)
        ));
        out.push_str("<ol class=\"timeline\">");
        for e in task_events {
            let etype = serde_json::to_value(e.event_type)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "unknown".into());
            let suggested_class = if matches!(e.status, tj_core::event::EventStatus::Suggested) {
                " suggested"
            } else {
                ""
            };
            out.push_str(&format!(
                "<li class=\"event{}\"><time>{}</time>\
                 <span class=\"type type-{}\">{}</span>{}</li>",
                suggested_class,
                html_escape(&e.timestamp),
                html_escape(&etype),
                html_escape(&etype),
                html_escape(&e.text)
            ));
        }
        out.push_str("</ol>");
        out.push_str("</article>");
    }

    out.push_str("</main></body></html>\n");
    out
}

/// Resolve `<events_dir>/../../pending` for the current project. Mirrors
/// the path layout used by `persist_pending`.
fn pending_dir() -> Result<std::path::PathBuf> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let dir = events_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("events_dir has no grandparent"))?
        .join("pending");
    Ok(dir)
}

/// The current project's entries in the global `pending/` dir. New entries
/// are named `<project_hash>.<ulid>.json` (`….dead.json` once retries are
/// exhausted); a legacy bare `<ulid>.json` counts as ours unless its JSON
/// names another project. Callers filter schema / dead state on top.
fn project_pending_entries(
    dir: &std::path::Path,
    project_hash: &str,
) -> Result<Vec<std::path::PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };
        let stem = stem.strip_suffix(".dead").unwrap_or(stem);
        let (ulid, ours) = match stem.split_once('.') {
            Some((prefix, ulid)) => (ulid, prefix == project_hash),
            None => (stem, !legacy_pending_is_foreign(&path, project_hash)),
        };
        if ours {
            out.push((ulid.to_string(), path));
        }
    }

    // ULIDs sort by creation time: oldest first, so a user prompt is
    // classified before the assistant turn that answered it.
    out.sort();
    Ok(out.into_iter().map(|(_, path)| path).collect())
}

/// A legacy (un-prefixed) entry belongs to another project only when its
/// JSON says so; one without `project_hash` stays visible here.
fn legacy_pending_is_foreign(path: &std::path::Path, project_hash: &str) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("project_hash")?.as_str().map(|h| h != project_hash))
        .unwrap_or(false)
}

fn run_pending_list() -> Result<()> {
    let dir = pending_dir()?;
    let project_hash = tj_core::project_hash::from_path(std::env::current_dir()?)?;
    let mut entries: Vec<(String, String, String, u32)> = Vec::new();
    for path in project_pending_entries(&dir, &project_hash)? {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        let id = stem
            .strip_prefix(&format!("{project_hash}."))
            .unwrap_or(stem)
            .to_string();
        let body = std::fs::read_to_string(&path)?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let queued_at = v
            .get("queued_at")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string();
        let text_preview: String = v
            .get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .chars()
            .take(72)
            .collect();
        let attempts = v.get("attempts").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        entries.push((id, queued_at, text_preview, attempts));
    }
    if entries.is_empty() {
        println!("(no pending entries)");
        return Ok(());
    }
    println!("{:<26} {:<25} attempts  text", "id", "queued_at");
    for (id, qa, text, attempts) in &entries {
        println!("{id:<26} {qa:<25} {attempts:<8}  {text}");
    }
    Ok(())
}

fn run_pending_retry(
    backend: &str,
    mock_etype: Option<&str>,
    mock_tid: Option<&str>,
    mock_conf: Option<f64>,
) -> Result<()> {
    let dir = pending_dir()?;
    if !dir.exists() {
        println!("(no pending entries)");
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));

    // The CI-safe mock branch needs no classifier. Without a usable backend
    // a retry can't do better than the attempt that queued the entry, so
    // leave everything as it is rather than burn attempts toward `.dead`.
    let classifier = match (mock_etype, mock_tid) {
        (Some(_), Some(_)) => None,
        _ => match retry_classifier(backend)? {
            Some(c) => Some(c),
            None => {
                println!(
                    "pending retry: no classifier backend available for `{backend}` \
                     (no `claude` on PATH, no ANTHROPIC_API_KEY) — entries left untouched"
                );
                return Ok(());
            }
        },
    };

    let mut succeeded = 0usize;
    let mut died = 0usize;
    let mut still_pending = 0usize;
    for path in project_pending_entries(&dir, &project_hash)? {
        if path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.ends_with(".dead"))
            .unwrap_or(false)
        {
            continue; // already dead, skip
        }
        let body = std::fs::read_to_string(&path)?;
        let mut v: serde_json::Value = serde_json::from_str(&body)?;
        // v0.6.2: skip v2 entries here — those are async-queued events
        // owned by classify-worker. The retry path is for legacy v1
        // entries that already failed in the inline path.
        if v.get("schema").and_then(|x| x.as_str()) == Some("v2") {
            continue;
        }
        let attempts = v.get("attempts").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let text = v
            .get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let kind = v
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or("Stop")
            .to_string();

        let outcome: anyhow::Result<()> = match (mock_etype, mock_tid, &classifier) {
            (Some(etype), Some(tid), _) => {
                let mut event = tj_core::event::Event::new(
                    tid,
                    parse_event_type(etype)?,
                    tj_core::event::Author::Classifier,
                    tj_core::event::Source::Hook,
                    text,
                );
                event.confidence = mock_conf;
                event.status = tj_core::classifier::decide_status(mock_conf.unwrap_or(1.0));
                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;
                Ok(())
            }
            (_, _, Some(classifier)) => match classify_chunk(
                classifier.as_ref(),
                &events_path,
                &project_hash,
                &kind,
                &text,
                None,
            )? {
                ChunkOutcome::Unplaced(err) => Err(anyhow::anyhow!(err)),
                ChunkOutcome::Recorded | ChunkOutcome::Dropped => Ok(()),
            },
            _ => unreachable!("a missing classifier returns before the loop"),
        };

        match outcome {
            Ok(()) => {
                std::fs::remove_file(&path)?;
                succeeded += 1;
            }
            Err(_) => {
                let new_attempts = attempts + 1;
                if new_attempts >= PENDING_MAX_ATTEMPTS {
                    let dead_path = path.with_file_name(format!(
                        "{}.dead.json",
                        path.file_stem().and_then(|s| s.to_str()).unwrap_or("dead")
                    ));
                    std::fs::rename(&path, &dead_path)?;
                    died += 1;
                } else {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert(
                            "attempts".into(),
                            serde_json::Value::Number(new_attempts.into()),
                        );
                    }
                    std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
                    still_pending += 1;
                }
            }
        }
    }
    println!(
        "pending retry: {succeeded} drained, {still_pending} still pending, {died} marked dead"
    );
    Ok(())
}

/// The classifier `pending retry` runs, or `None` when the backend has
/// nothing beyond what already failed: hybrid without an LLM fallback, or
/// agent-sdk / api without `claude` / a key.
fn retry_classifier(
    backend: &str,
) -> anyhow::Result<Option<Box<dyn tj_core::classifier::Classifier>>> {
    Ok(match backend {
        "hybrid" | "" => {
            let hybrid = tj_core::classifier::hybrid::HybridClassifier::from_env();
            hybrid
                .has_llm_fallback()
                .then(|| Box::new(hybrid) as Box<dyn tj_core::classifier::Classifier>)
        }
        "agent-sdk" | "api" => build_classifier(backend).ok(),
        other => Some(build_classifier(other)?),
    })
}

fn run_doctor() -> Result<DoctorReport> {
    let mut issues: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // 1. claude binary in PATH (note, not issue — API backend works without it)
    let claude_check = PCommand::new("claude").arg("--version").output();
    let (claude_in_path, claude_version) = match claude_check {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (true, Some(v))
        }
        Ok(_) | Err(_) => {
            notes.push(
                "claude CLI not on PATH — that's fine if you use the `api` backend \
                 (set ANTHROPIC_API_KEY). For the `agent-sdk` backend (no API key; \
                 uses your Claude login, drawing the Agent SDK credit pool since \
                 2026-06-15), install Claude Code from https://claude.com/claude-code"
                    .into(),
            );
            (false, None)
        }
    };

    // 2. data dir + sub-dir writability
    let data_dir = tj_core::paths::data_dir()?;
    let events_dir = tj_core::paths::events_dir()?;
    let state_dir = tj_core::paths::state_dir()?;
    let metrics_dir = tj_core::paths::metrics_dir()?;
    let events_dir_writable = dir_writable(&events_dir);
    let state_dir_writable = dir_writable(&state_dir);
    let metrics_dir_writable = dir_writable(&metrics_dir);
    if !events_dir_writable {
        issues.push(format!("events dir not writable: {}", events_dir.display()));
    }
    if !state_dir_writable {
        issues.push(format!("state dir not writable: {}", state_dir.display()));
    }
    if !metrics_dir_writable {
        issues.push(format!(
            "metrics dir not writable: {}",
            metrics_dir.display()
        ));
    }

    // 3. known projects (from state dir SQLite stems)
    let known_projects = tj_core::db::list_all_projects(&state_dir).unwrap_or_default();

    // 4. schema versions for the current cwd's project (if any).
    let schema_versions_applied = (|| -> Result<Vec<i64>> {
        let cwd = std::env::current_dir()?;
        let project_hash = tj_core::project_hash::from_path(&cwd)?;
        let state_path = state_dir.join(format!("{project_hash}.sqlite"));
        if !state_path.exists() {
            return Ok(Vec::new());
        }
        let conn = tj_core::db::open(&state_path)?;
        let mut stmt = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
        let v: Vec<i64> = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<_, _>>()?;
        Ok(v)
    })()
    .unwrap_or_default();

    Ok(DoctorReport {
        task_journal_version: env!("CARGO_PKG_VERSION"),
        claude_in_path,
        claude_version,
        codex_in_path: tj_core::llm::codex_on_path(),
        data_dir,
        events_dir,
        state_dir,
        metrics_dir,
        events_dir_writable,
        state_dir_writable,
        metrics_dir_writable,
        known_projects,
        schema_versions_applied,
        issues,
        notes,
    })
}

#[derive(Parser)]
#[command(name = "task-journal", version, about = "Task Journal CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new task (writes an `open` event).
    Create {
        /// Task title (one line).
        title: String,
        /// Optional initial context paragraph.
        #[arg(long)]
        context: Option<String>,
        /// Optional one-line goal: what is this task trying to achieve?
        /// Renders prominently in `pack`/TUI; can be filled in later
        /// with `task-journal goal <id> "<text>"`.
        #[arg(long)]
        goal: Option<String>,
        /// Parent task id — makes this a subtask of the given id.
        #[arg(long)]
        parent: Option<String>,
    },
    /// List tasks for the current project.
    List {
        /// Render tasks as a tree, children indented under parents.
        #[arg(long)]
        tree: bool,
    },
    /// Inspect events for a project.
    Events {
        #[command(subcommand)]
        action: EventsCmd,
    },
    /// Rebuild SQLite state from the JSONL log.
    RebuildState,
    /// Embed events for semantic search (Pillar A). Computes a vector per event
    /// and stores it in the v008 `embeddings` table. `--backfill` drains the
    /// whole project; without it, only newly-unembedded events are processed.
    /// Uses the dependency-free hash embedder by default — fully offline.
    Embed {
        /// Vectorise the entire project history, not just new events.
        #[arg(long)]
        backfill: bool,
    },
    /// Semantic search over this project's journal (Pillar A). Embeds the query
    /// and returns the most relevant events by meaning, not keyword. New events
    /// are embedded on the fly, so the index stays current with zero setup.
    Ask {
        /// The question or topic to search for.
        query: String,
        /// Maximum number of results.
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Emit a JSON array instead of human lines (for tooling / the Loom host).
        #[arg(long)]
        json: bool,
    },
    /// Cross-project recall (Pillar B): search EVERY project's decisions,
    /// rejections and constraints for reasoning relevant to the query —
    /// prior choices and dead-ends from your whole history, not just this repo.
    Recall {
        /// The topic / approach to check against prior reasoning.
        query: String,
        /// Maximum number of results.
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Emit a JSON array instead of human lines (for tooling / the Loom host).
        #[arg(long)]
        json: bool,
    },
    /// Record a durable user preference (Pillar C) — e.g. "prefer terse output",
    /// "respond in Russian", "always run the full test suite before tagging".
    /// Stored user-level (across all projects) and injected into every session
    /// so the agent remembers how you work without being re-told.
    Remember {
        /// The preference text to remember.
        text: String,
    },
    /// List your stored user preferences.
    Preferences,
    /// Turn realtime hook capture on or off via a `.capture-disabled` marker.
    /// `off` no-ops the capture path of `ingest-hook` immediately — even in an
    /// already-running session — without touching the read-only SessionStart
    /// resume. Use it to silence a stale auto-capture hook.
    Capture {
        /// "on" (remove the marker), "off" (write it), or "status" (report it).
        state: String,
    },
    /// Distil this project's recurring decisions and constraints into durable
    /// semantic/procedural facts (Pillar C). MANUAL and opt-in — it makes ONE
    /// LLM call per run and is never wired to a hook, so it can't spend
    /// automatically. Facts are stored as events in a per-project "conventions"
    /// task and surface in ask/recall.
    Consolidate {
        /// Maximum number of facts to produce.
        #[arg(long, default_value_t = 8)]
        max_facts: usize,
        /// LLM backend override: claude-p (default) | anthropic | openai | ollama.
        /// Defaults to TJ_BACKEND, then claude-p (subscription, no API key).
        #[arg(long)]
        backend: Option<String>,
        /// Also write the conventions into ./CLAUDE.md as a managed block, so
        /// they're always-on for every session (regenerated on each run).
        #[arg(long)]
        write_claude_md: bool,
    },
    /// Render and print the resume pack for a task.
    Pack {
        /// Task id (e.g. tj-7f3a). Optional when --external is given.
        task_id: Option<String>,
        /// Resolve the task by an external reference instead of its id
        /// (e.g. `loom:t-abc`). Mutually exclusive with a positional id.
        #[arg(long)]
        external: Option<String>,
        /// Output mode: compact|full.
        #[arg(long, default_value = "compact")]
        mode: String,
    },
    /// Append a typed event to a task.
    Event {
        task_id: String,
        /// Event type: hypothesis, finding, evidence, decision, rejection,
        /// constraint, correction, reopen, supersede, close, redirect.
        #[arg(long, name = "type")]
        r#type: String,
        /// Event text body.
        #[arg(long)]
        text: String,
        /// Optional event id this corrects (for type=correction).
        #[arg(long)]
        corrects: Option<String>,
        /// Optional event id this supersedes (for type=supersede).
        #[arg(long)]
        supersedes: Option<String>,
    },
    /// Close a task (writes a `close` event).
    Close {
        task_id: String,
        #[arg(long)]
        reason: Option<String>,
        /// One-line outcome: what shipped / why we stopped.
        #[arg(long)]
        outcome: Option<String>,
        /// Structured tag for the outcome: `done`, `abandoned`, or
        /// `superseded`. Free-form text via `--outcome` is the
        /// primary field; the tag is for filtering / aggregation.
        #[arg(long)]
        outcome_tag: Option<String>,
    },
    /// Attach a clickable, typed link to a task (doc, deploy, dashboard,
    /// design, …). Renders under the pack's Artifacts as `[label](url)` so a
    /// host like the Loom board shows it on the task card. Writes a `finding`
    /// event carrying the link in `meta.artifacts`.
    ArtifactAdd {
        task_id: String,
        /// Short tag: `doc`, `deploy`, `dashboard`, `design`, `pr`, …
        #[arg(long)]
        kind: String,
        /// The link target (URL or path).
        #[arg(long)]
        url: String,
        /// Human label shown on the card.
        #[arg(long)]
        label: String,
    },
    /// Reopen a previously closed task (writes a `reopen` event and
    /// flips status back to `open`). Use when the same scope comes
    /// back, e.g. a regression on a shipped fix or a follow-up bug
    /// that belongs in the original chain rather than a new task.
    Reopen {
        task_id: String,
        /// One-line reason for reopening (regression, follow-up, etc).
        #[arg(long)]
        reason: Option<String>,
    },
    /// List open tasks with no activity for N+ days. Use to clean up
    /// tasks that auto-opened, got a few events, then went silent —
    /// candidates for `task-journal close --outcome-tag abandoned`.
    Stale {
        /// Inactivity threshold in days. Default 7.
        #[arg(long, default_value_t = 7)]
        days: i64,
    },
    /// Garbage-collect the current project's pending classifier queue.
    /// Removes entries older than N days OR marked dead by retry
    /// exhaustion. Run after classifier auth was broken for a while and
    /// the queue grew stale.
    PendingGc {
        /// Age threshold in days. Default 7.
        #[arg(long, default_value_t = 7)]
        days: i64,
        /// Collect every project's entries, not just the current one's.
        #[arg(long)]
        all: bool,
    },
    /// Set or update the goal of an existing task.
    Goal {
        task_id: String,
        /// New goal text (one line). Pass an empty string to clear.
        text: String,
    },
    /// Manage external references on a task (beads ids, GitHub PRs,
    /// JIRA issues — anything that ties this journal entry to work
    /// outside the journal).
    External {
        task_id: String,
        /// Reference to append, e.g. `beads:claude-memory-rsw`,
        /// `github:#42`. Append-only; pass multiple times to add
        /// several references over time.
        #[arg(long = "add")]
        add: String,
    },
    /// Re-run artifact extraction over every event of a task and
    /// refresh the pack cache. Use after upgrading from v0.4.x — older
    /// events were ingested before the artifact column was populated,
    /// so they have empty `artifacts` JSON until reclassify backfills.
    Reclassify { task_id: String },
    /// Full-text search across events (FTS5).
    Search {
        /// Query string.
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Search across all projects on this machine, not just the cwd one.
        #[arg(long)]
        all_projects: bool,
        /// v0.10.3+: restrict matches to a single event type
        /// (`decision`, `evidence`, `finding`, `rejection`, ...).
        #[arg(long = "type", value_name = "TYPE")]
        event_type: Option<String>,
    },
    /// Append a correction event referencing an earlier event_id.
    EventCorrect {
        #[arg(long)]
        corrects: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        text: String,
    },
    /// Install agent hooks that ingest events into the task journal.
    InstallHooks {
        /// Scope: user (home directory) or project (current directory).
        #[arg(long, default_value = "user")]
        scope: String,
        /// Which agent to wire: "claude" (~/.claude/settings.json) or
        /// "codex" (~/.codex/hooks.json). Both speak the same hook protocol —
        /// one JSON payload on stdin, `hookSpecificOutput.additionalContext`
        /// back on stdout — so the same ingest command serves both.
        #[arg(long, default_value = "claude")]
        client: String,
        /// Remove our hook entries instead of installing.
        #[arg(long)]
        uninstall: bool,
        /// After installing hooks, retro-import existing Claude Code session
        /// history for the current project. Equivalent to running
        /// `task-journal backfill` afterwards. Onboarding shortcut.
        #[arg(long)]
        backfill: bool,
        /// Classifier backend baked into the installed hook command:
        /// "hybrid" (default), "agent-sdk", "api", or "heuristic". Use
        /// "agent-sdk" to classify via the local `claude` login without an
        /// ANTHROPIC_API_KEY (see `ingest-hook --help` for the credit note).
        #[arg(long, default_value = "hybrid")]
        backend: String,
        /// v0.14.0: opt in to realtime auto-capture. Without it, install-hooks
        /// wires ONLY the cheap, read-only SessionStart resume hook — no
        /// per-message classifier, no `claude -p`, no cost. Primary capture is
        /// the agent self-tagging via the MCP tools. With `--auto-capture` the
        /// per-message + PreCompact ingest hooks are installed too (they spawn
        /// the classifier, honoring `--backend`).
        #[arg(long)]
        auto_capture: bool,
        /// Opt in to proactive cross-project recall (Pillar B). Adds a
        /// UserPromptSubmit hook that injects relevant prior decisions/
        /// rejections/constraints from any project before you act. Off by
        /// default (it surfaces extra context on every prompt). Fast keyword
        /// path, no model; gated at runtime by TJ_PROACTIVE_RECALL=0.
        #[arg(long)]
        proactive_recall: bool,
    },
    /// Show local classifier and journal statistics.
    Stats,
    /// Interactive TUI: browse the journal's tasks (default) or, with
    /// `--chats`, the underlying Claude Code chat-session JSONLs.
    #[command(alias = "tui")]
    Ui {
        /// Project path override (default: current directory).
        #[arg(long)]
        project: Option<String>,
        /// Legacy mode: open the chat-session browser instead of the
        /// task list. Lets you read raw Claude Code session history
        /// when the task journal alone isn't enough.
        #[arg(long)]
        chats: bool,
    },
    /// Import task-journal events from existing Claude Code session history.
    /// Parses JSONL session files and creates tasks retroactively.
    Backfill {
        /// Dry run: show what would be imported without writing.
        #[arg(long)]
        dry_run: bool,
        /// Limit to N most recent sessions (default: all).
        #[arg(long)]
        limit: Option<usize>,
        /// Project path override (default: current directory).
        #[arg(long)]
        project: Option<String>,
    },
    /// Offline memory backfill: re-read session transcripts and append
    /// significant events the realtime classifier missed (dream Pass A).
    Dream {
        /// Only sessions in the last N days (overrides the watermark).
        #[arg(long)]
        since: Option<i64>,
        /// Only this task's sessions.
        #[arg(long)]
        task: Option<String>,
        /// Show scope without calling the API or writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Cap sessions processed this run.
        #[arg(long)]
        limit: Option<usize>,
        /// LLM backend override: claude-p (default) | anthropic | openai | ollama.
        #[arg(long)]
        backend: Option<String>,
    },
    /// Finalize a task: fix a junk auto-title and close it IF the events
    /// clearly show it is done — the model decides from the content, in
    /// seconds. Omit the id to finalize every open task (batch, with a
    /// reviewable list). Add `--enrich` to also re-read the task's sessions and
    /// backfill missed events first — thorough but slow (one `claude -p` call
    /// per session; minutes on a big multi-session task).
    Complete {
        /// The task id to finalize. Omit to finalize all open tasks (batch).
        task: Option<String>,
        /// Show scope and planned actions without calling the model or writing.
        #[arg(long)]
        dry_run: bool,
        /// Also backfill missed events from the task's sessions before judging.
        /// Thorough but slow (one `claude -p` call per session).
        #[arg(long)]
        enrich: bool,
        /// Required for batch finalize when stdin is not an interactive terminal.
        #[arg(long)]
        yes: bool,
        /// LLM backend override: claude-p (default) | anthropic | openai | ollama.
        #[arg(long)]
        backend: Option<String>,
    },
    /// Export tasks as Markdown or JSON to stdout.
    Export {
        /// Output format: md, json.
        #[arg(long, default_value = "md")]
        format: String,
        /// Export specific task by ID (default: all open tasks).
        #[arg(long)]
        task: Option<String>,
        /// Project path override.
        #[arg(long)]
        project: Option<String>,
    },
    /// Self-check the install: claude binary, data dirs, known projects,
    /// schema migrations. Exits 0 when all checks pass; 1 otherwise.
    Doctor {
        /// Emit a machine-readable JSON report instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Inspect or retry classifier failures queued under pending/.
    /// The auto-capture hook writes a pending entry whenever the
    /// classifier errors (network down, rate limit, missing API key);
    /// this command surfaces them.
    Pending {
        #[command(subcommand)]
        action: PendingCmd,
    },
    /// Re-key on-disk data when a project moved on disk. The project_hash
    /// is derived from the canonical path, so a moved project orphans its
    /// own data; this command renames the JSONL + SQLite + metrics files.
    MigrateProject {
        /// Old project path (the data we want to keep).
        #[arg(long, value_name = "PATH")]
        from: PathBuf,
        /// New project path (where the project lives now).
        #[arg(long, value_name = "PATH")]
        to: PathBuf,
        /// Overwrite the destination if data already exists for it.
        #[arg(long)]
        force: bool,
    },
    /// Hook entry point: ingest a chat chunk through the classifier.
    ///
    /// When `--kind` and `--text` are both omitted, reads the Claude Code
    /// hook payload as JSON from stdin (the actual production wiring).
    /// `--kind` / `--text` remain for tests and ad-hoc use.
    IngestHook {
        /// Hook kind: UserPromptSubmit | PostToolUse | Stop | SessionStart.
        /// If omitted, derived from stdin JSON (`hook_event_name`).
        #[arg(long)]
        kind: Option<String>,
        /// The chat chunk text. If omitted, derived from stdin JSON
        /// (`prompt` for UserPromptSubmit, synthesized from
        /// tool_name+input+response for PostToolUse, etc.).
        #[arg(long)]
        text: Option<String>,
        /// Classifier backend:
        ///   - "hybrid" (default) — keyword heuristic first (free, offline),
        ///     then the configured LLM fallback chain (agent-sdk, then api;
        ///     reorder with TJ_HYBRID_LLM_ORDER). Only available backends run.
        ///   - "agent-sdk" — classify via the local, already-logged-in `claude`
        ///     binary; no ANTHROPIC_API_KEY needed. Pinned to Haiku (override
        ///     with TJ_AGENT_SDK_MODEL). NOTE: since 2026-06-15 a headless
        ///     `claude -p` draws from the separate Agent SDK monthly credit
        ///     pool (~$20 Pro / $100 Max 5x / $200 Max 20x at API rates), not
        ///     the interactive pool. Classification is tiny, so it lasts.
        ///   - "api" — always call the Anthropic API. Needs ANTHROPIC_API_KEY.
        ///   - "heuristic" — heuristic only, no LLM. Fastest, lowest coverage.
        ///   - "cli" — removed in v0.8.0; use "agent-sdk" (its resurrection).
        #[arg(long, default_value = "hybrid")]
        backend: String,
        /// Test/dev override: bypass classifier and force this event type. Hidden from --help.
        #[arg(long, hide = true)]
        mock_event_type: Option<String>,
        /// Test/dev override: target task id. Hidden from --help.
        #[arg(long, hide = true)]
        mock_task_id: Option<String>,
        /// Test/dev override: confidence value. Hidden from --help.
        #[arg(long, hide = true)]
        mock_confidence: Option<f64>,
    },
    /// Internal: drain pending v2 entries and classify each one.
    /// Spawned as a detached child by ingest-hook so the hook can
    /// return in <100ms instead of blocking 5-30s on `claude -p`.
    /// Holds a project-scoped file lock — only one worker per project
    /// at a time. Hidden from --help; not a public API.
    #[command(hide = true)]
    ClassifyWorker {
        /// Classifier backend: "hybrid", "agent-sdk", "api", or "heuristic".
        /// Defaults to hybrid.
        #[arg(long, default_value = "hybrid")]
        backend: String,
    },
    /// One-line status snapshot for the Claude Code statusline. Prints
    /// `[tj-x9rz · open: N · pending: N · stale: N]`. Sub-100ms by
    /// design — wire it via `~/.claude/settings.json` `statusLine`.
    /// Hidden from --help; not a human command.
    #[command(hide = true)]
    Statusline,
    /// Read-only reminder hook (no model, never spawns `claude -p`). Emits a
    /// UserPromptSubmit additionalContext line nudging the agent to record
    /// reasoning via the MCP tools as it goes. Wired by `install-hooks` by
    /// default. Hidden from --help; not a human command.
    #[command(hide = true)]
    Nudge,
    /// Opt-in proactive recall hook (Pillar B). On UserPromptSubmit, injects a
    /// budgeted additionalContext block of prior decisions/rejections/
    /// constraints from ANY project relevant to the prompt — a guardrail
    /// against re-deciding or repeating a dead-end. Fast keyword path, no
    /// model. Wired only by `install-hooks --proactive-recall`. Gated by
    /// TJ_PROACTIVE_RECALL=0. Hidden from --help; not a human command.
    #[command(hide = true)]
    RecallHook,
    /// Cross-task search for `rejection` events matching a topic. Helpful
    /// when the agent is about to repeat a path that was already turned
    /// down — query the topic, see the prior rejection.
    Rejected {
        /// Search topic (FTS5 when possible, LIKE fallback for tokens
        /// containing FTS-unfriendly chars like `-`).
        topic: String,
        /// Search across all projects on this machine.
        #[arg(long)]
        all_projects: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Restrict to events newer than N days.
        #[arg(long)]
        since: Option<i64>,
    },
    /// Render a task as PR-description Markdown (Summary, Changes,
    /// Why-this-approach, Verification, Affected). Reuses event log +
    /// artifacts; introduces no new tables.
    ExportPr { task_id: String },
    /// Print a task's honesty score (0–100) and completeness gaps —
    /// deterministic, zero-LLM (like `mex check`). `--json` for machines.
    Check {
        task_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print a targeted gap-fill prompt for a task (deterministic, zero-LLM,
    /// like `mex sync --dry-run`); the in-session agent runs it to close gaps.
    Gaps {
        task_id: String,
        /// Emit the full fix prompt (default: just list gaps + score).
        #[arg(long)]
        fill: bool,
    },
    /// Export task knowledge as Claude-memory frontmatter files (feeds native dream).
    ExportMemory {
        /// Export a single task by id.
        #[arg(long, conflicts_with = "all_closed")]
        task: Option<String>,
        /// Export all closed tasks (default scope when no flag is given).
        #[arg(long)]
        all_closed: bool,
        /// Print target paths + content without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum EventsCmd {
    /// List events (most recent first).
    List {
        /// Limit to N events.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum PendingCmd {
    /// List queued classifier failures.
    List,
    /// Re-feed every pending entry through the classifier. Marks an
    /// entry as `<id>.dead.json` after PENDING_MAX_ATTEMPTS failures.
    /// With no usable backend, entries are left untouched.
    Retry {
        /// Classifier backend: "hybrid", "agent-sdk", "api", or "heuristic".
        /// Defaults to hybrid.
        #[arg(long, default_value = "hybrid")]
        backend: String,
        /// Test/dev override: bypass classifier and force this event
        /// type. Hidden from --help.
        #[arg(long, hide = true)]
        mock_event_type: Option<String>,
        /// Test/dev override: target task id. Hidden from --help.
        #[arg(long, hide = true)]
        mock_task_id: Option<String>,
        /// Test/dev override: confidence value. Hidden from --help.
        #[arg(long, hide = true)]
        mock_confidence: Option<f64>,
    },
}

const PENDING_MAX_ATTEMPTS: u32 = 3;

/// Windows' default main-thread stack is 1 MiB and our command dispatch in
/// [`real_main`] sits near that limit — a single added branch overflowed it
/// (STATUS_STACK_OVERFLOW on every command; see `run_session_end_catchup`).
/// Run the real work on a thread with a generous stack so the dispatch can
/// grow safely and this can't recur. Errors and the panic case propagate so
/// the process still exits non-zero on failure.
fn main() -> Result<()> {
    let handle = std::thread::Builder::new()
        .name("tj-main".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(real_main)
        .context("spawn main worker thread")?;
    match handle.join() {
        Ok(result) => result,
        Err(_) => anyhow::bail!("main worker thread panicked"),
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Create {
            title,
            context,
            goal,
            parent,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_dir = tj_core::paths::events_dir()?;
            let events_path = events_dir.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(&events_dir)?;

            let task_id = tj_core::new_task_id();

            // Validate --parent before writing the open event: the parent must
            // already exist and the link must not introduce a cycle. Needs the
            // derived SQLite state, so ingest the JSONL tail first.
            if let Some(ref parent_id) = parent {
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                if !tj_core::db::task_exists(&conn, parent_id)? {
                    anyhow::bail!("parent task {parent_id} does not exist");
                }
                if tj_core::db::would_create_cycle(&conn, &task_id, parent_id)? {
                    anyhow::bail!("setting parent {parent_id} would create a cycle");
                }
            }

            let mut event = tj_core::event::Event::new(
                task_id.clone(),
                tj_core::event::EventType::Open,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                context.clone().unwrap_or_else(|| title.clone()),
            );
            let mut meta = serde_json::json!({ "title": title });
            if let Some(ref parent_id) = parent {
                meta["parent_id"] = serde_json::Value::String(parent_id.clone());
            }
            event.meta = meta;

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;

            // If --goal was provided, ingest the open event into SQLite
            // (so the row exists) and write the goal column. Skipping
            // this when --goal is absent keeps the SQLite hot path
            // exclusive to ingest-hook / pack callers.
            if let Some(g) = goal {
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                tj_core::db::set_task_goal(&conn, &task_id, &g)?;
            }

            println!("{}", task_id);
        }
        Commands::List { tree } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if tree {
                for t in tj_core::db::top_level_tasks(&conn, &project_hash)? {
                    println!("{} [{}] {}", t.task_id, t.status, t.title);
                    for c in tj_core::db::children_of(&conn, &t.task_id)? {
                        println!("  {} [{}] {}", c.task_id, c.status, c.title);
                    }
                }
            } else {
                for t in tj_core::db::list_tasks_by_project(&conn, &project_hash)? {
                    println!("{} [{}] {}", t.task_id, t.status, t.title);
                }
            }
        }
        Commands::Events { action } => match action {
            EventsCmd::List { limit } => {
                let cwd = std::env::current_dir()?;
                let project_hash = tj_core::project_hash::from_path(&cwd)?;
                let events_path =
                    tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
                if !events_path.exists() {
                    println!("(no events yet)");
                    return Ok(());
                }
                let mut events = read_events_lenient(&events_path, "events list")?;
                events.reverse();
                for e in events.into_iter().take(limit) {
                    let title = e
                        .meta
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| e.text.clone());
                    let etype = serde_json::to_value(e.event_type)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_else(|| "?".into());
                    println!("{}  [{etype}]  {}", e.timestamp, title);
                }
            }
        },
        Commands::Pack {
            task_id,
            external,
            mode,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            // Resolve the target task: explicit id, else by external reference.
            let resolved = match (task_id, external) {
                (Some(id), _) => id,
                (None, Some(ext)) => match tj_core::db::task_id_by_external(&conn, &ext)? {
                    Some(id) => id,
                    None => anyhow::bail!("no task with external reference: {ext}"),
                },
                (None, None) => anyhow::bail!("a task id or --external is required"),
            };
            let pmode = match mode.as_str() {
                "compact" => tj_core::pack::PackMode::Compact,
                "full" => tj_core::pack::PackMode::Full,
                other => anyhow::bail!("unknown mode: {other}"),
            };
            let pack = tj_core::pack::assemble(&conn, &resolved, pmode)?;
            print!("{}", pack.text);
        }
        Commands::RebuildState => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            if !events_path.exists() {
                anyhow::bail!("no events file at {events_path:?}");
            }

            let conn = tj_core::db::open(&state_path)?;
            let n = tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
            println!("rebuilt {n} events into {state_path:?}");
        }
        Commands::Embed { backfill } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            if !events_path.exists() {
                anyhow::bail!("no events file at {events_path:?}");
            }
            let conn = tj_core::db::open(&state_path)?;
            // search_fts must be current before we embed from it.
            tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;

            let embedder = tj_core::embed::default_embedder();
            let now = chrono::Utc::now().to_rfc3339();
            let batch = if backfill { 256 } else { 64 };
            let mut total = 0usize;
            loop {
                let n = tj_core::db::embed_pending(
                    &conn,
                    &project_hash,
                    embedder.as_ref(),
                    &now,
                    batch,
                )?;
                total += n;
                // Without --backfill, one batch of newly-unembedded events is enough.
                if n == 0 || !backfill {
                    break;
                }
            }
            sync_global_memory(&conn, &project_hash);
            println!(
                "embedded {total} event(s) with model {} ({} dim)",
                embedder.model_id(),
                embedder.dim()
            );
        }
        Commands::Ask { query, k, json } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            if !events_path.exists() {
                anyhow::bail!("no events file at {events_path:?}");
            }
            let conn = tj_core::db::open(&state_path)?;
            tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;

            let embedder = tj_core::embed::default_embedder();
            // Embed-on-ask: vectorise anything new so the answer reflects the
            // latest events without the user running `embed` first.
            let now = chrono::Utc::now().to_rfc3339();
            tj_core::db::embed_pending(&conn, &project_hash, embedder.as_ref(), &now, 512)?;
            sync_global_memory(&conn, &project_hash);

            let qv = embedder.embed_one(&query)?;
            let hits =
                tj_core::db::semantic_search(&conn, &project_hash, &qv, embedder.model_id(), k)?;
            if json {
                let arr: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "task_id": h.task_id,
                            "project_hash": project_hash,
                            "event_type": h.event_type,
                            "text": h.text,
                            "score": h.score,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string(&arr)?);
            } else if hits.is_empty() {
                println!("no matches");
            } else {
                for h in hits {
                    let snippet: String = h.text.chars().take(100).collect();
                    println!(
                        "{:.3}  [{}] {}  ({})",
                        h.score, h.event_type, snippet, h.task_id
                    );
                }
            }
        }
        Commands::Recall { query, k, json } => {
            let global_path = tj_core::paths::memory_db()?;
            if !global_path.exists() {
                if json {
                    println!("[]");
                } else {
                    println!("global memory is empty — run `ask` or `embed` in a project first");
                }
                return Ok(());
            }
            let global = tj_core::memory::open(&global_path)?;
            let embedder = tj_core::embed::default_embedder();
            let qv = embedder.embed_one(&query)?;
            let hits = tj_core::memory::search(&global, &qv, embedder.model_id(), k)?;
            if json {
                println!("{}", recall_hits_json(&hits));
            } else if hits.is_empty() {
                println!("no relevant prior reasoning found");
            } else {
                for h in hits {
                    let snippet: String = h.text.chars().take(100).collect();
                    let proj: String = h.project_hash.chars().take(8).collect();
                    println!(
                        "{:.3}  [{}] {}  ({}/{})",
                        h.score, h.event_type, snippet, proj, h.task_id
                    );
                }
            }
        }
        Commands::Remember { text } => {
            let global = tj_core::memory::open(tj_core::paths::memory_db()?)?;
            let now = chrono::Utc::now().to_rfc3339();
            if tj_core::memory::add_preference(&global, &text, &now)? {
                println!("remembered: {}", text.trim());
            } else {
                println!("already remembered");
            }
        }
        Commands::Preferences => {
            let path = tj_core::paths::memory_db()?;
            let prefs = if path.exists() {
                tj_core::memory::list_preferences(&tj_core::memory::open(&path)?)?
            } else {
                Vec::new()
            };
            if prefs.is_empty() {
                println!("no preferences yet — add one with `task-journal remember \"...\"`");
            } else {
                for p in prefs {
                    println!("- {p}");
                }
            }
        }
        Commands::Capture { state } => {
            let marker = tj_core::paths::data_dir()?.join(".capture-disabled");
            match state.trim().to_lowercase().as_str() {
                "off" | "false" | "0" => {
                    std::fs::create_dir_all(marker.parent().unwrap())?;
                    std::fs::write(&marker, "")?;
                    println!("realtime capture OFF — the marker no-ops ingest-hook capture now (resume still works).");
                }
                "on" | "true" | "1" => {
                    let _ = std::fs::remove_file(&marker);
                    println!("realtime capture ON.");
                }
                "status" => {
                    if marker.exists() {
                        println!("realtime capture OFF (.capture-disabled present).");
                    } else {
                        println!("realtime capture ON.");
                    }
                }
                other => anyhow::bail!("expected `on`, `off`, or `status`, got `{other}`"),
            }
        }
        Commands::Consolidate {
            max_facts,
            backend,
            write_claude_md,
        } => {
            run_consolidate(max_facts, backend.as_deref(), write_claude_md)?;
        }
        Commands::Event {
            task_id,
            r#type,
            text,
            corrects,
            supersedes,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            let event_type = parse_event_type(&r#type)?;
            let mut event = tj_core::event::Event::new(
                &task_id,
                event_type,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                text,
            );
            event.corrects = corrects;
            event.supersedes = supersedes;

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            println!("{}", event.event_id);
        }
        Commands::ArtifactAdd {
            task_id,
            kind,
            url,
            label,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            // A `finding` event carries the link in meta.artifacts; index_event
            // merges it so it renders under the pack's Artifacts.
            let mut event = tj_core::event::Event::new(
                &task_id,
                tj_core::event::EventType::Finding,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                format!("📎 {kind}: {label} — {url}"),
            );
            event.meta = tj_core::artifacts::link_event_meta(&kind, &url, &label);

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            println!("{}", event.event_id);
        }
        Commands::Close {
            task_id,
            reason,
            outcome,
            outcome_tag,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            // Validate the outcome_tag enum so users don't accumulate
            // arbitrary values in the column. Free-text lives in
            // `outcome`; the tag is for filter/aggregate.
            if let Some(tag) = outcome_tag.as_deref() {
                match tag {
                    "done" | "abandoned" | "superseded" => {}
                    other => anyhow::bail!(
                        "invalid --outcome-tag `{other}` (expected: done | abandoned | superseded)"
                    ),
                }
            }

            // Catch up the index then assert the task is real before we
            // append a close event for an id that never existed.
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if !tj_core::db::task_exists(&conn, &task_id)? {
                anyhow::bail!("task not found: {task_id}");
            }
            // Persist outcome BEFORE the close event so the cache wipe
            // inside set_task_outcome doesn't compete with subsequent
            // assemble calls. Both columns optional — caller can pass
            // neither and just get the close event.
            if let Some(o) = outcome.as_deref() {
                tj_core::db::set_task_outcome(&conn, &task_id, o, outcome_tag.as_deref())?;
            }
            let open_kids = tj_core::db::count_open_children(&conn, &task_id)?;
            drop(conn);

            let mut event = tj_core::event::Event::new(
                &task_id,
                tj_core::event::EventType::Close,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                reason.clone().unwrap_or_else(|| "(closed)".into()),
            );
            let mut meta = serde_json::Map::new();
            if let Some(r) = reason {
                meta.insert("reason".into(), serde_json::Value::String(r));
            }
            // Layer-2 close harvest: stamp deterministic git/gh refs (commit,
            // branch, PR) so the closed pack reads as a clickable ledger of
            // what shipped. Best-effort; structured artifacts are merged in
            // db::index_event — never fails the close.
            let arts = tj_core::harvest::harvest(&cwd);
            if !arts.is_empty() {
                if let Ok(v) = serde_json::to_value(&arts) {
                    meta.insert("artifacts".into(), v);
                }
            }
            if !meta.is_empty() {
                event.meta = serde_json::Value::Object(meta);
            }

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            if open_kids > 0 {
                eprintln!("note: {open_kids} open subtask(s) under {task_id}");
            }

            // Non-blocking completeness warning. The close above already
            // succeeded; re-open, apply the close event to the index, then
            // assess. Any error here must NOT fail the close — handle
            // locally, never `?`-propagate.
            if let Ok(conn) = tj_core::db::open(&state_path) {
                let _ = tj_core::db::ingest_new_events(&conn, &events_path, &project_hash);
                if let Ok(report) = tj_core::completeness::assess(
                    &conn,
                    &task_id,
                    tj_core::completeness::pending_count(),
                ) {
                    if !report.is_complete() {
                        eprintln!(
                            "note: task {task_id} closed with {} completeness gap(s):",
                            report.gaps.len()
                        );
                        for g in &report.gaps {
                            eprintln!("  ⚠ {}", g.detail);
                        }
                    }
                }
            }

            println!("{}", event.event_id);
        }
        Commands::Stale { days } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            let stale = tj_core::db::stale_tasks(&conn, days)?;
            if stale.is_empty() {
                println!("(no stale tasks — all open tasks active within {days} days)");
            } else {
                println!("# Stale tasks (idle ≥ {days} days)\n");
                for t in stale {
                    println!(
                        "{}  {} days idle  {}  {}",
                        t.task_id, t.days_idle, t.last_event_at, t.title
                    );
                }
                println!(
                    "\nClose abandoned ones with: task-journal close <id> --outcome-tag abandoned --reason <why>"
                );
            }
        }
        Commands::PendingGc { days, all } => {
            let pending_dir = tj_core::paths::events_dir()?
                .parent()
                .ok_or_else(|| anyhow::anyhow!("events_dir has no parent"))?
                .join("pending");
            if !pending_dir.exists() {
                println!("(no pending dir — nothing to gc)");
                return Ok(());
            }
            let entries: Vec<std::path::PathBuf> = if all {
                std::fs::read_dir(&pending_dir)?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                    .collect()
            } else {
                let project_hash = tj_core::project_hash::from_path(std::env::current_dir()?)?;
                project_pending_entries(&pending_dir, &project_hash)?
            };
            let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
            let mut removed = 0usize;
            for path in entries {
                // Prefer the file's mtime over JSON parsing — pending
                // payloads include their own queued_at but are not
                // guaranteed parseable when the classifier corrupted
                // input mid-stream.
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| {
                        chrono::DateTime::<chrono::Utc>::from(t)
                            .signed_duration_since(cutoff)
                            .num_seconds()
                            .into()
                    });
                if let Some(secs) = mtime {
                    if secs < 0 && std::fs::remove_file(&path).is_ok() {
                        removed += 1;
                    }
                }
            }
            println!(
                "removed {} stale pending entries (older than {} days)",
                removed, days
            );
        }
        Commands::Reopen { task_id, reason } => {
            // The Reopen event itself flips tasks.status back to open
            // when ingested (db::apply_lifecycle handles this). The CLI
            // job is just to assert the task exists and write the event.
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if !tj_core::db::task_exists(&conn, &task_id)? {
                anyhow::bail!("task not found: {task_id}");
            }
            drop(conn);

            let mut event = tj_core::event::Event::new(
                &task_id,
                tj_core::event::EventType::Reopen,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                reason.clone().unwrap_or_else(|| "(reopened)".into()),
            );
            if let Some(r) = reason {
                event.meta = serde_json::json!({"reason": r});
            }
            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            println!("{}", event.event_id);
        }
        Commands::Goal { task_id, text } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if !tj_core::db::task_exists(&conn, &task_id)? {
                anyhow::bail!("task not found: {task_id}");
            }
            tj_core::db::set_task_goal(&conn, &task_id, &text)?;
            println!("ok");
        }
        Commands::External { task_id, add } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if !tj_core::db::task_exists(&conn, &task_id)? {
                anyhow::bail!("task not found: {task_id}");
            }
            tj_core::db::add_task_external(&conn, &task_id, &add)?;
            println!("ok");
        }
        Commands::Reclassify { task_id } => {
            // Walk events_index for this task, re-run artifact extraction
            // over each event's text (looked up via search_fts), and
            // overwrite the artifacts column. Pack cache is wiped after
            // so the next render picks up the new artifacts block. Used
            // primarily to backfill v0.4.x events that were ingested
            // before extraction existed.
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            if !tj_core::db::task_exists(&conn, &task_id)? {
                anyhow::bail!("task not found: {task_id}");
            }
            let count = tj_core::db::reclassify_task_artifacts(&conn, &task_id)?;
            println!("reclassified {} events", count);
        }
        Commands::EventCorrect {
            corrects,
            task,
            text,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            let mut event = tj_core::event::Event::new(
                &task,
                tj_core::event::EventType::Correction,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                text,
            );
            event.corrects = Some(corrects);
            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            println!("{}", event.event_id);
        }
        Commands::InstallHooks {
            scope,
            client,
            uninstall,
            backfill,
            backend,
            auto_capture,
            proactive_recall,
        } => {
            // Codex keeps hooks in their own file rather than in a settings
            // file, and gives SessionEnd a 1-second budget capped at 3 (Claude
            // Code allows up to 60). Everything else about the wiring is the
            // same, so the two clients differ only in these two values.
            let (config_dir, config_file, session_end_timeout) = match client.as_str() {
                "claude" => (".claude", "settings.json", 30),
                "codex" => (".codex", "hooks.json", 3),
                other => anyhow::bail!("unknown --client: {other} (expected `claude` or `codex`)"),
            };
            let settings_path = match scope.as_str() {
                "user" => {
                    let home =
                        std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
                    std::path::PathBuf::from(home)
                        .join(config_dir)
                        .join(config_file)
                }
                "project" => std::env::current_dir()?.join(config_dir).join(config_file),
                other => anyhow::bail!("unknown scope: {other}"),
            };
            if let Some(p) = settings_path.parent() {
                std::fs::create_dir_all(p)?;
            }

            let mut current: serde_json::Value = if settings_path.exists() {
                serde_json::from_str(&std::fs::read_to_string(&settings_path)?)
                    .unwrap_or_else(|_| serde_json::json!({}))
            } else {
                serde_json::json!({})
            };

            let hooks_obj = current
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("settings is not a JSON object"))?;
            if uninstall {
                // Surgical removal: walk the `hooks` block, drop only
                // entries whose command contains "task-journal ingest-hook"
                // — leaves co-located third-party plugin hooks (token-pilot
                // etc.) intact. Old behavior `remove("hooks")` nuked
                // everyone's hooks; this is the bxl-bug fix.
                if let Some(hooks_block) =
                    hooks_obj.get_mut("hooks").and_then(|v| v.as_object_mut())
                {
                    let kinds: Vec<String> = hooks_block.keys().cloned().collect();
                    for kind in kinds {
                        let Some(arr) = hooks_block.get_mut(&kind).and_then(|v| v.as_array_mut())
                        else {
                            continue;
                        };
                        // Each entry is { matcher, hooks: [{type, command}, ...] }.
                        // Filter the inner array; keep only non-task-journal commands.
                        for entry in arr.iter_mut() {
                            let Some(inner) = entry.get_mut("hooks").and_then(|v| v.as_array_mut())
                            else {
                                continue;
                            };
                            inner.retain(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(|c| {
                                        !(c.contains("task-journal ingest-hook")
                                            || c.contains("task-journal nudge"))
                                    })
                                    .unwrap_or(true)
                            });
                        }
                        // Drop matcher entries with empty inner arrays.
                        arr.retain(|entry| {
                            entry
                                .get("hooks")
                                .and_then(|v| v.as_array())
                                .map(|a| !a.is_empty())
                                .unwrap_or(true)
                        });
                        // If the whole kind is empty, remove it.
                        if arr.is_empty() {
                            hooks_block.remove(&kind);
                        }
                    }
                    // Empty hooks block → remove entirely so settings.json
                    // stays tidy when we were the only user.
                    if hooks_block.is_empty() {
                        hooks_obj.remove("hooks");
                    }
                }
                // Remove our env key too — preserve other env entries.
                if let Some(env) = hooks_obj.get_mut("env").and_then(|v| v.as_object_mut()) {
                    env.remove("TJ_CLASSIFIER_CLI");
                    // Drop empty env block to keep settings.json clean.
                    if env.is_empty() {
                        hooks_obj.remove("env");
                    }
                }
            } else {
                // Wrap with `|| true` so a failed classifier (network down, rate limit,
                // missing API key) NEVER breaks Claude Code. Failures land in pending/
                // and replay on next ingest.
                // Default to subscription-based classifier (`claude -p`).
                // Power users with API key can run install-hooks --backend=api below.
                // Claude Code pipes the hook payload as JSON on stdin; the
                // `--kind` / `--text` flags from earlier templates pointed
                // at env vars Claude Code never sets and therefore always
                // fed the classifier empty text. Stdin-only is the correct
                // wiring (see claude-memory-rsw).
                // Bake the selected backend into the hook command. Default
                // "hybrid" stays flag-free (heuristic first, then the agent-sdk
                // → api fallback chain). A non-default backend — e.g.
                // `--backend=agent-sdk` for subscription users with no API key
                // — is passed through so the spawned classify-worker honors it.
                if !matches!(
                    backend.as_str(),
                    "hybrid" | "agent-sdk" | "api" | "heuristic"
                ) {
                    anyhow::bail!(
                        "unknown --backend: {backend} (expected `hybrid`, `agent-sdk`, `api`, or `heuristic`)"
                    );
                }
                let cmd_string = if backend == "hybrid" {
                    "task-journal ingest-hook || true".to_string()
                } else {
                    format!("task-journal ingest-hook --backend={backend} || true")
                };
                let cmd = cmd_string.as_str();
                let nudge_cmd = "task-journal nudge || true";
                // v0.14.x — self-tagging-first. The DEFAULT wires only no-model
                // hooks: SessionStart → ingest-hook short-circuits to inject the
                // read-only resume pack (no classifier); UserPromptSubmit → `nudge`
                // prints a reminder to keep recording (no model, no spawn). The
                // per-message classifier (`claude -p`) is opt-in via
                // `--auto-capture`, which appends `ingest-hook` to the message
                // events. Primary capture is the agent self-tagging via the MCP
                // tools.
                // Timing fields matter as much as the commands:
                // - SessionStart and the nudge stay synchronous — their stdout
                //   is context Claude must see on the first turn — so they only
                //   carry a `timeout` well under the event budget.
                // - The classifier hooks run `async` (Claude Code 2.1.x): the
                //   chat never waits on them and no timeout is enforced.
                // - SessionEnd can't be async (the session is going away) and
                //   shares a 1.5-second budget unless a per-hook `timeout`
                //   raises it, up to 60s (Claude Code 2.1.268). Without this
                //   the last-chance catch-up was cancelled mid-write.
                let mut entries = serde_json::json!({
                    "SessionStart":     [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd, "timeout": 20 }] }],
                    "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": nudge_cmd, "timeout": 10 }] }],
                });
                if auto_capture {
                    let obj = entries.as_object_mut().expect("entries is an object");
                    // UserPromptSubmit keeps the nudge AND gains the classifier.
                    obj.insert(
                        "UserPromptSubmit".into(),
                        serde_json::json!([{ "matcher": "", "hooks": [
                            { "type": "command", "command": nudge_cmd, "timeout": 10 },
                            { "type": "command", "command": cmd, "async": true },
                        ]}]),
                    );
                    // PostModelSwitch is Claude Code only — Codex has no such
                    // event, and an unknown key there is dead config.
                    let async_events: &[&str] = if client == "codex" {
                        &["PostToolUse", "Stop", "PreCompact"]
                    } else {
                        &["PostToolUse", "Stop", "PreCompact", "PostModelSwitch"]
                    };
                    for ev in async_events {
                        obj.insert(
                            ev.to_string(),
                            serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": cmd, "async": true }] }]),
                        );
                    }
                    obj.insert(
                        "SessionEnd".into(),
                        serde_json::json!([{ "matcher": "", "hooks": [
                            { "type": "command", "command": cmd, "timeout": session_end_timeout },
                        ]}]),
                    );
                }
                if proactive_recall {
                    // Append the recall injector to the UserPromptSubmit hooks,
                    // keeping whatever is already there (nudge, and ingest when
                    // --auto-capture is also set).
                    let obj = entries.as_object_mut().expect("entries is an object");
                    let ups = obj
                        .entry("UserPromptSubmit")
                        .or_insert_with(|| serde_json::json!([{ "matcher": "", "hooks": [] }]));
                    if let Some(hooks) = ups
                        .as_array_mut()
                        .and_then(|a| a.get_mut(0))
                        .and_then(|e| e.get_mut("hooks"))
                        .and_then(|h| h.as_array_mut())
                    {
                        hooks.push(serde_json::json!({
                            "type": "command",
                            "command": "task-journal recall-hook || true",
                            "timeout": 10,
                        }));
                    }
                }
                // MERGE our entries into the existing `hooks` block — touch ONLY
                // task-journal hooks, never clobber other plugins' hooks. For each
                // event we (a) strip any prior task-journal entry (idempotent
                // re-install) then (b) append ours, leaving foreign hooks and
                // untouched events intact.
                let is_tj = |c: &str| {
                    c.contains("task-journal ingest-hook")
                        || c.contains("task-journal nudge")
                        || c.contains("task-journal recall-hook")
                };
                let hooks_block = hooks_obj
                    .entry("hooks".to_string())
                    .or_insert_with(|| serde_json::json!({}));
                let hooks_block = hooks_block
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("settings `hooks` is not an object"))?;
                for (event, our_arr) in entries.as_object().expect("entries is an object") {
                    let existing = hooks_block
                        .entry(event.clone())
                        .or_insert_with(|| serde_json::json!([]));
                    let existing = existing
                        .as_array_mut()
                        .ok_or_else(|| anyhow::anyhow!("hooks.{event} is not an array"))?;
                    for entry in existing.iter_mut() {
                        if let Some(inner) = entry.get_mut("hooks").and_then(|v| v.as_array_mut()) {
                            inner.retain(|h| {
                                h.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(|c| !is_tj(c))
                                    .unwrap_or(true)
                            });
                        }
                    }
                    existing.retain(|e| {
                        e.get("hooks")
                            .and_then(|v| v.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(true)
                    });
                    for our_entry in our_arr.as_array().expect("event entry is an array") {
                        existing.push(our_entry.clone());
                    }
                }
            }
            std::fs::write(&settings_path, serde_json::to_string_pretty(&current)?)?;
            println!("{}", settings_path.display());

            // Onboarding convenience: retro-import existing Claude Code history
            // so the journal isn't empty on day one. Always operates on the
            // current working directory; install-hooks scope is independent.
            // We re-exec ourselves rather than refactoring the (~150-line)
            // backfill body — keeps the pipe simple and the output identical
            // to a manual `task-journal backfill`.
            if !uninstall && backfill {
                let exe =
                    std::env::current_exe().context("locate task-journal binary for backfill")?;
                let status = std::process::Command::new(&exe)
                    .arg("backfill")
                    .status()
                    .with_context(|| format!("spawn `{} backfill`", exe.display()))?;
                if !status.success() {
                    eprintln!("backfill exited with {status}");
                }
            }
        }
        Commands::Stats => {
            let metrics_dir = tj_core::paths::metrics_dir()?;
            let mut total = 0usize;
            let mut confirmed = 0usize;
            let mut suggested = 0usize;
            let mut errors = 0usize;
            if metrics_dir.exists() {
                for entry in std::fs::read_dir(&metrics_dir)? {
                    let path = entry?.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let body = std::fs::read_to_string(&path)?;
                    for line in body.lines().filter(|l| !l.trim().is_empty()) {
                        total += 1;
                        let v: serde_json::Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(_) => {
                                errors += 1;
                                continue;
                            }
                        };
                        match v.get("status").and_then(|s| s.as_str()) {
                            Some("confirmed") => confirmed += 1,
                            Some("suggested") => suggested += 1,
                            _ => {}
                        }
                    }
                }
            }
            println!("classified: {total}");
            println!("  confirmed: {confirmed}");
            println!("  suggested: {suggested}");
            println!("  parse errors: {errors}");
            if total > 0 {
                let ratio = confirmed as f64 / total as f64 * 100.0;
                println!("  confirmed ratio: {ratio:.1}%");
            }
            // Memory platform (Pillars A/B/C): the global cross-project index.
            let mem_path = tj_core::paths::memory_db()?;
            if mem_path.exists() {
                if let Ok(g) = tj_core::memory::open(&mem_path) {
                    let entries = tj_core::memory::count(&g).unwrap_or(0);
                    let prefs = tj_core::memory::list_preferences(&g)
                        .map(|p| p.len())
                        .unwrap_or(0);
                    println!("memory (global cross-project recall index):");
                    println!("  recall entries: {entries}");
                    println!("  preferences: {prefs}");
                }
            }
        }
        Commands::Doctor { json } => {
            let report = run_doctor()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                report.print_human();
            }
            if !report.issues.is_empty() {
                std::process::exit(1);
            }
        }
        Commands::MigrateProject { from, to, force } => {
            run_migrate_project(&from, &to, force)?;
        }
        Commands::Pending { action } => match action {
            PendingCmd::List => {
                run_pending_list()?;
            }
            PendingCmd::Retry {
                backend,
                mock_event_type,
                mock_task_id,
                mock_confidence,
            } => {
                run_pending_retry(
                    &backend,
                    mock_event_type.as_deref(),
                    mock_task_id.as_deref(),
                    mock_confidence,
                )?;
            }
        },
        Commands::IngestHook {
            kind,
            text,
            backend,
            mock_event_type,
            mock_task_id,
            mock_confidence,
        } => {
            // Recursion guard. The classifier spawns `claude -p` to do
            // the actual work; that nested claude invocation re-reads
            // ~/.claude/settings.json and would re-fire our hooks,
            // recursively calling ingest-hook → classifier → claude → …
            // Until v0.2.8 we relied on `--bare` to suppress the hooks
            // on the inner invocation, but --bare doesn't work with
            // subscription auth (claude-memory-0kk), so the classifier
            // now sets TJ_IN_CLASSIFIER=1 in the child env and we bail
            // here when we see it.
            if std::env::var(tj_core::classifier::agent_sdk::IN_CLASSIFIER_ENV).is_ok() {
                return Ok(());
            }

            // Resolve (kind, text) source: explicit args win; otherwise
            // read the Claude Code hook payload from stdin. The earlier
            // settings.json template interpolated `$CLAUDE_HOOK_NAME` /
            // `$CLAUDE_HOOK_TEXT` env vars that Claude Code does NOT set,
            // so production was always called with empty text and every
            // event ended up rejected — see claude-memory-rsw.
            let (kind, text, payload) = match (kind, text) {
                (Some(k), Some(t)) => (k, t, serde_json::Value::Null),
                _ => parse_hook_stdin()?,
            };

            // The Claude Code mod captures in-process and marks the hooks it
            // starts; the classic capture stands down instead of doing the
            // same work twice. Resume packs and model switches stay ours.
            if mod_active()
                && matches!(
                    kind.as_str(),
                    "UserPromptSubmit" | "PostToolUse" | "Stop" | "PreCompact" | "SessionEnd"
                )
            {
                return Ok(());
            }

            // Emergency capture kill-switch: a `.capture-disabled` marker in the
            // data dir no-ops realtime capture (the read-only SessionStart
            // resume still runs). Because the hook re-invokes this binary on
            // every event, dropping the marker stops a stale auto-capture hook
            // in an already-running session immediately — no restart needed.
            if kind != "SessionStart"
                && tj_core::paths::data_dir()
                    .map(|d| d.join(".capture-disabled").exists())
                    .unwrap_or(false)
            {
                return Ok(());
            }

            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            // Live Claude Code session id (hook payload → env fallback),
            // stamped additively onto the live events this hook emits so
            // consumers can correlate them with the session. None when
            // neither source is present (standalone behaviour unchanged).
            let live_session_id = tj_core::session_id::live_session_id(Some(&payload));

            // Push-recall (claude-memory-60m). Best-effort, fail-open, read-only.
            // After a (non-MCP) tool call, surface a relevant prior
            // rejection/decision via an additionalContext envelope so the agent
            // doesn't re-walk a ruled-out path. Gated by TJ_PUSH_RECALL=0.
            //
            // Dedup vs claude-memory-7km: skip MCP-tool turns — those are
            // handled by 7km's updatedMCPToolOutput path, so emitting
            // additionalContext here too would double-surface the same recall.
            // The two paths are mutually exclusive by tool type (this =
            // non-mcp tools; 7km = mcp__ tools). The block only adds a stdout
            // envelope; it never touches the JSONL log or the pending flow
            // below, and any error is swallowed so the hook can't break.
            let tool_is_mcp = payload
                .get("tool_name")
                .and_then(|v| v.as_str())
                .map(|n| n.starts_with("mcp__"))
                .unwrap_or(false);
            if kind == "PostToolUse"
                && !tool_is_mcp
                && std::env::var("TJ_PUSH_RECALL").as_deref() != Ok("0")
                && events_path.exists()
            {
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                if let Ok(conn) = tj_core::db::open(&state_path) {
                    let _ = tj_core::db::ingest_new_events(&conn, &events_path, &project_hash);
                    if let Ok(hits) = tj_core::recall::relevant_recall(
                        &conn,
                        &text,
                        tj_core::recall::DEFAULT_MAX_HITS,
                    ) {
                        let hits = fresh_recall_hits(
                            hits,
                            live_session_id.as_deref(),
                            &events_path,
                            &project_hash,
                        );
                        if !hits.is_empty() {
                            let mut ctx = String::new();
                            for h in &hits {
                                let verb = match h.event_type {
                                    tj_core::event::EventType::Rejection => "previously rejected",
                                    _ => "previously decided",
                                };
                                ctx.push_str(&format!(
                                    "⚠ recall: in task {} you {}: {}\n",
                                    h.task_id, verb, h.text
                                ));
                            }
                            let envelope = serde_json::json!({
                                "hookSpecificOutput": {
                                    "hookEventName": "PostToolUse",
                                    "additionalContext": ctx.trim_end(),
                                }
                            });
                            println!("{}", serde_json::to_string(&envelope)?);
                        }
                    }
                }
            }

            // Push-recall via updatedMCPToolOutput (claude-memory-7km). For an
            // MCP PostToolUse turn whose input echoes a prior rejection/decision,
            // prepend a recall banner to what Claude sees of that tool's output.
            // Best-effort, read-only: any miss or error emits nothing and the
            // real output passes through unchanged. Complements 60m (which skips
            // mcp__ tools) — gated MCP-only, falls through to the queue path so
            // event capture is unaffected. Disabled by TJ_PUSH_RECALL=0.
            if kind == "PostToolUse" && std::env::var("TJ_PUSH_RECALL").as_deref() != Ok("0") {
                if let Some(envelope) = push_recall_envelope(
                    &payload,
                    &events_path,
                    &project_hash,
                    live_session_id.as_deref(),
                ) {
                    println!("{}", serde_json::to_string(&envelope)?);
                }
            }

            // SessionStart: emit a JSON envelope with compact resume-packs of
            // open tasks so Claude Code injects them into its system context
            // automatically. This is the load-bearing UX for "the journal
            // remembers" — without it, users would have to call task_pack
            // manually each session. Empty stdout when no open tasks → no
            // injection, keeps system prompt clean for fresh projects.
            if kind == "SessionStart" {
                // User preferences are global, so they surface even in a fresh
                // project with no events of its own (Pillar C "remember me").
                let prefs_block = session_preferences_block();
                // Skip early on a clean machine: nothing to surface, and we
                // don't want SessionStart to spawn empty SQLite files in
                // every project Claude Code is opened in. Preferences still go
                // out if there are any.
                if !events_path.exists() {
                    if !prefs_block.is_empty() {
                        emit_session_context(&prefs_block);
                    }
                    return Ok(());
                }
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                let recent = recent_task_contexts(&conn, 3)?;
                if recent.is_empty() {
                    if !prefs_block.is_empty() {
                        emit_session_context(&prefs_block);
                    }
                    return Ok(());
                }
                // After a compaction (source=="compact"), re-inject the
                // active task + its in-force constraints so the rebuilt
                // context doesn't lose what it was doing. Best-effort:
                // any error → no reminder, never abort SessionStart.
                let source = payload.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let mut bundle = String::new();
                // Preferences lead the bundle — they're the smallest, most
                // durable signal about how the user wants to be worked with.
                if !prefs_block.is_empty() {
                    bundle.push_str(&prefs_block);
                    bundle.push_str("\n\n");
                }
                // A resumed or forked session keeps its transcript, so the
                // reminder would be noise — except when the conversation sat
                // idle long enough that what it was doing is no longer fresh
                // for the user either. Claude Code 2.1.251+ reports the gap in
                // `seconds_since_last_response` on `resume` and `fork`.
                let idle_secs = payload
                    .get("seconds_since_last_response")
                    .and_then(|v| v.as_u64());
                let stale_resume = matches!(source, "resume" | "fork")
                    && idle_secs.is_some_and(|s| s >= STALE_RESUME_SECS);
                if source == "compact" || stale_resume {
                    let label = if source == "compact" {
                        "Active task after compaction".to_string()
                    } else {
                        format!(
                            "Active task, idle for {}",
                            human_gap(idle_secs.unwrap_or_default())
                        )
                    };
                    if let Ok(Some(reminder)) =
                        tj_core::reminder::active_task_reminder(&conn, &label)
                    {
                        bundle.push_str(&reminder);
                        bundle.push_str("\n\n");
                    }
                }
                if source == "compact" {
                    // Advisory (the hook can't force it): suggest the main agent
                    // delegate the just-compacted segment to the in-session
                    // distiller subagent, which backfills missed reasoning from
                    // the transcript file (which survives compaction) for the
                    // active task(s). Background → never blocks. Gated off by
                    // TJ_DISTILLER_HINT=0 for users who don't want it.
                    if std::env::var("TJ_DISTILLER_HINT").as_deref() != Ok("0") {
                        let transcript_hint = payload
                            .get("transcript_path")
                            .and_then(|v| v.as_str())
                            .map(|p| format!(" (transcript: {p})"))
                            .unwrap_or_default();
                        bundle.push_str(&format!(
                            "[task-journal] A compaction just occurred. If decisions, rejections, \
or findings from before it are not yet in the journal for the active task(s) above, delegate to \
the `task-journal-distiller` subagent to capture them from the transcript{transcript_hint}. It \
runs in the background and won't block you; it only fills gaps and never closes tasks.\n\n"
                        ));
                    }
                }
                for tc in &recent {
                    let pack = tj_core::pack::assemble(
                        &conn,
                        &tc.task_id,
                        tj_core::pack::PackMode::Compact,
                    )?;
                    bundle.push_str(&pack.text);
                    bundle.push_str("\n\n");
                }

                // We deliberately DO NOT emit `sessionTitle` or
                // `initialUserMessage` here. The v0.10.1 X2 experiment set
                // `sessionTitle` to "TJ — <task_id> (<n> open)", which
                // OVERRODE Claude Code's native session name with our task id
                // — users saw "TJ — tj-qqay98cpc2" instead of a name derived
                // from their own prompt. `initialUserMessage` injected a
                // "[Task Journal resumed: …]" banner into the next prompt,
                // which the auto-open path then captured as a garbage task
                // title. The resume context the model actually needs already
                // rides in `additionalContext`; the tab label belongs to
                // Claude Code, not to us. (0.14.3)
                let hook_specific = serde_json::json!({
                    "hookEventName": "SessionStart",
                    "additionalContext": bundle.trim_end(),
                });
                let envelope = serde_json::json!({
                    "hookSpecificOutput": hook_specific,
                });
                println!("{}", serde_json::to_string(&envelope)?);
                return Ok(());
            }

            // PostModelSwitch (Claude Code 2.1.251+). Payload: { from_model,
            // to_model, source: "user"|"auto"|"resume", requested_model }.
            // Which model did the work is a fact about the task that neither
            // the diff nor the transcript keeps once the session is gone —
            // "why did this stretch come out shallow" is usually answered by
            // "it ran on a fallback model". Recorded as a `constraint`: it's an
            // external condition the work happened under, not a decision.
            // No active task → drop silently.
            if kind == "PostModelSwitch" {
                if !events_path.exists() {
                    return Ok(());
                }
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                let recent = recent_task_contexts(&conn, 1)?;
                let Some(tc) = recent.into_iter().next() else {
                    return Ok(());
                };
                let to_model = payload
                    .get("to_model")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if to_model.is_empty() {
                    return Ok(());
                }
                let from_model = payload
                    .get("from_model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unknown)");
                let switch_source = payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user");
                let text = format!(
                    "{}{switch_source}): {from_model} → {to_model}",
                    tj_core::reminder::MODEL_SWITCH_TEXT_PREFIX
                );
                let mut event = tj_core::event::Event::new(
                    &tc.task_id,
                    tj_core::event::EventType::Constraint,
                    tj_core::event::Author::Classifier,
                    tj_core::event::Source::Hook,
                    text,
                );
                event.confidence = Some(0.9);
                event.status = tj_core::event::EventStatus::Confirmed;
                // Kept in the journal, but left out of the constraint lists
                // (resume reminder, classifier context) — see
                // MODEL_SWITCH_TEXT_PREFIX.
                event.meta = serde_json::json!({ "kind": "model_switch" });
                tj_core::session_id::stamp_session_id(&mut event.meta, live_session_id.as_deref());
                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;
                println!("{}", event.event_id);
                return Ok(());
            }

            // PreCompact: Claude Code is about to compact the conversation.
            // Two responsibilities:
            //   1. Catch-up ingest — read the transcript JSONL tail (entries
            //      newer than the active task's last event timestamp) and
            //      enqueue them as pending v2 chunks for the classify-worker.
            //      Closes the gap between the last PostToolUse hook and the
            //      compaction event, where chunks would otherwise be lost.
            //   2. Boundary marker — synthetic decision event so the
            //      post-compact agent sees a clear cut in the journal.
            if kind == "PreCompact" {
                if !events_path.exists() {
                    return Ok(());
                }
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                let recent = recent_task_contexts(&conn, 1)?;
                let Some(tc) = recent.into_iter().next() else {
                    return Ok(());
                };

                // (1) Catch-up ingest. Best-effort: missing transcript_path
                // or unreadable JSONL falls through to the marker only.
                let last_event_ts: Option<String> = conn
                    .query_row(
                        "SELECT timestamp FROM events_index WHERE task_id=?1 \
                         ORDER BY timestamp DESC LIMIT 1",
                        rusqlite::params![&tc.task_id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok();
                let transcript_path = payload
                    .get("transcript_path")
                    .and_then(|x| x.as_str())
                    .map(std::path::PathBuf::from);
                if let Some(tp) = transcript_path.as_ref() {
                    if tp.exists() {
                        let enq = enqueue_transcript_chunks_since_last_event(
                            tp,
                            &events_path,
                            &project_hash,
                            &backend,
                            last_event_ts.as_deref(),
                            "PreCompactChunk",
                            live_session_id.as_deref(),
                        )
                        .unwrap_or(0);
                        if enq > 0 && std::env::var("TJ_DISABLE_CLASSIFY_SPAWN").is_err() {
                            let _ = spawn_classify_worker(&backend);
                        }
                    }
                }

                // (2) Boundary marker.
                let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

                // v0.10.3: dedupe near-duplicate markers. Two PreCompact
                // hook firings within DEDUP_WINDOW_SECS — caused by
                // multi-plugin race, rapid compact-then-restore, or a
                // retried hook — both append "Conversation compacted at
                // T" events with the same wall-clock second. Skip if
                // the most recent decision event already carries this
                // marker text and was written under a minute ago.
                const DEDUP_WINDOW_SECS: i64 = 60;
                let last_marker: Option<(String, String)> = conn
                    .query_row(
                        "SELECT ei.timestamp, COALESCE(sf.text, '') \
                         FROM events_index ei \
                         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id \
                         WHERE ei.task_id = ?1 AND ei.type = 'decision' \
                         ORDER BY ei.timestamp DESC LIMIT 1",
                        rusqlite::params![&tc.task_id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .ok();
                if let Some((ts, text)) = last_marker {
                    if text.starts_with("Conversation compacted at") {
                        if let Ok(prev) = chrono::DateTime::parse_from_rfc3339(&ts) {
                            let delta = (chrono::Utc::now()
                                .signed_duration_since(prev.with_timezone(&chrono::Utc)))
                            .num_seconds();
                            if delta.abs() < DEDUP_WINDOW_SECS {
                                // Marker recently appended — skip the
                                // second one. Still print SOMETHING so
                                // hook callers see a stable exit shape;
                                // emit the previous event_id we'd have
                                // duplicated would not be available here
                                // without an extra query, so emit empty.
                                return Ok(());
                            }
                        }
                    }
                }

                let marker_text = format!(
                    "Conversation compacted at {now}; preceding events should be treated as a single reasoning unit."
                );
                let mut event = tj_core::event::Event::new(
                    &tc.task_id,
                    tj_core::event::EventType::Decision,
                    tj_core::event::Author::Classifier,
                    tj_core::event::Source::Hook,
                    marker_text,
                );
                event.confidence = Some(1.0);
                event.status = tj_core::event::EventStatus::Confirmed;
                tj_core::session_id::stamp_session_id(&mut event.meta, live_session_id.as_deref());
                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;
                let metrics_path =
                    tj_core::paths::metrics_dir()?.join(format!("{project_hash}.jsonl"));
                let _ = tj_core::classifier::telemetry::append(
                    &metrics_path,
                    &tj_core::classifier::telemetry::TelemetryRecord {
                        timestamp: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        project_hash: project_hash.clone(),
                        task_id_guess: Some(tc.task_id.clone()),
                        event_type: "decision".into(),
                        confidence: 1.0,
                        status: "confirmed".into(),
                        error: None,
                    },
                );
                println!("{}", event.event_id);
                return Ok(());
            }

            // Stop: Claude Code is about to end the session. Same
            // catch-up logic as PreCompact (read transcript tail,
            // enqueue chunks newer than the active task's last
            // event timestamp), but no boundary marker — a session
            // end isn't a reasoning boundary, the task is just
            // pausing. The v0.7.0-era Stop hook fired with hardcoded
            // text="Session ended" which carried no signal and just
            // littered the pending queue with noise; v0.9.3 replaces
            // that with a real catch-up.
            //
            // Skip the catch-up when running through the mock test
            // path (mock_event_type + mock_task_id) — those tests
            // expect their explicit `--kind=Stop` invocation to fall
            // through to the mock-classifier dispatch below, not be
            // intercepted by the new transcript-tail logic.
            let is_mock_stop = mock_event_type.is_some() && mock_task_id.is_some();
            if !is_mock_stop && kind == "Stop" {
                if !events_path.exists() {
                    return Ok(());
                }
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                let recent = recent_task_contexts(&conn, 1)?;
                let Some(tc) = recent.into_iter().next() else {
                    return Ok(());
                };

                let last_event_ts: Option<String> = conn
                    .query_row(
                        "SELECT timestamp FROM events_index WHERE task_id=?1 \
                         ORDER BY timestamp DESC LIMIT 1",
                        rusqlite::params![&tc.task_id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok();
                let transcript_path = payload
                    .get("transcript_path")
                    .and_then(|x| x.as_str())
                    .map(std::path::PathBuf::from);
                if let Some(tp) = transcript_path.as_ref() {
                    if tp.exists() {
                        let enq = enqueue_transcript_chunks_since_last_event(
                            tp,
                            &events_path,
                            &project_hash,
                            &backend,
                            last_event_ts.as_deref(),
                            "StopChunk",
                            live_session_id.as_deref(),
                        )
                        .unwrap_or(0);
                        if enq > 0 && std::env::var("TJ_DISABLE_CLASSIFY_SPAWN").is_err() {
                            let _ = spawn_classify_worker(&backend);
                        }
                    }
                }
                return Ok(());
            }

            // SessionEnd with reason "clear": /clear discards the conversation
            // and the transcript orphans, so this is the LAST chance to capture
            // the final segment. Extracted to its own function so its locals do
            // NOT bloat `main`'s already-huge stack frame — inlining it here
            // overflowed the 1 MiB Windows main-thread stack on every command.
            if kind == "SessionEnd" {
                return run_session_end_catchup(
                    &payload,
                    &events_path,
                    &project_hash,
                    &backend,
                    live_session_id.as_deref(),
                );
            }

            // Mock path only: drain legacy pending entries first.
            drain_pending(
                &events_path,
                &project_hash,
                mock_event_type.as_deref(),
                mock_task_id.as_deref(),
                mock_confidence,
            )?;

            // v0.6.3: drop empty-text events before queueing. PostToolUse
            // hooks for tools without a `tool_response` (SlashCommand,
            // background ops, etc.) used to reach the classifier with
            // text="" — wasting a haiku call per event and littering
            // pending/ with v1 dead entries. Mock path keeps the event
            // for explicit test coverage, so this guard runs only outside
            // mock paths.
            let is_mock_pre = mock_event_type.is_some() && mock_task_id.is_some();
            if !is_mock_pre && text.trim().is_empty() {
                return Ok(());
            }

            // v0.7.0 /rewind sentinel. When the user prepends `/rewind`
            // to their prompt they're telling us: the path I just walked
            // was wrong, ignore it. We don't mass-mark prior events as
            // rejected (too destructive — the agent might have learned
            // useful negatives along the way). Instead leave a single
            // correction event so any pack consumer sees the boundary.
            if !is_mock_pre && kind == "UserPromptSubmit" && is_rewind_prompt(&text) {
                if !events_path.exists() {
                    return Ok(());
                }
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                let recent = recent_task_contexts(&conn, 1)?;
                let Some(tc) = recent.into_iter().next() else {
                    return Ok(());
                };
                let mut event = tj_core::event::Event::new(
                    &tc.task_id,
                    tj_core::event::EventType::Correction,
                    tj_core::event::Author::User,
                    tj_core::event::Source::Hook,
                    "User invoked /rewind — preceding events on this task should be reconsidered. They may have been part of a path the user explicitly rolled back.".to_string(),
                );
                event.confidence = Some(1.0);
                event.status = tj_core::event::EventStatus::Confirmed;
                tj_core::session_id::stamp_session_id(&mut event.meta, live_session_id.as_deref());
                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;
                println!("{}", event.event_id);
                return Ok(());
            }

            // A tool call arrives as its full input + response JSON; cap it
            // so one big file read or command output can't flood the queue.
            let text: String = if kind == "PostToolUse" {
                text.chars().take(POST_TOOL_USE_TEXT_MAX).collect()
            } else {
                text
            };

            // v0.6.2 fork-bomb fix. The real-classifier path used to run
            // `claude -p` synchronously inside the hook, blocking each
            // UserPromptSubmit/PostToolUse/Stop for 5-30s. Symptoms:
            // ~19 stale ingest-hook + task-journal-mcp procs accumulated
            // within minutes (claude-memory-9ty). Now: queue the event
            // to pending/<id>.json (schema v2) and spawn a detached
            // classify-worker child. Hook returns in <100ms.
            //
            // Mock path stays synchronous — many tests rely on it. The
            // env override TJ_INGEST_SYNC=1 also forces sync, used by
            // tests that exercise the real-classifier code path with
            // /bin/false stubs.
            let force_sync = std::env::var("TJ_INGEST_SYNC")
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let is_mock = mock_event_type.is_some() && mock_task_id.is_some();
            if !is_mock && !force_sync {
                let _ = persist_pending_v2(
                    &events_path,
                    &kind,
                    &text,
                    &project_hash,
                    &backend,
                    live_session_id.as_deref(),
                )?;
                // Fire-and-forget worker. Errors here are best-effort —
                // a failure to spawn just means the entry sits in
                // pending/ until the next hook fires another spawn.
                if std::env::var("TJ_DISABLE_CLASSIFY_SPAWN").is_err() {
                    let _ = spawn_classify_worker(&backend);
                }
                return Ok(());
            }

            // Derive author_hint from hook kind: user prompts → "user", everything else → "assistant"
            let author_hint = if kind.contains("UserPrompt") {
                "user"
            } else {
                "assistant"
            };

            let (etype, task_id, confidence, evidence_strength, suggested_text) =
                if let (Some(t), Some(tid)) = (mock_event_type.as_deref(), mock_task_id.as_deref())
                {
                    (
                        parse_event_type(t)?,
                        tid.to_string(),
                        mock_confidence.unwrap_or(1.0),
                        None,
                        None,
                    )
                } else {
                    let state_path =
                        tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                    let conn = tj_core::db::open(&state_path)?;
                    if events_path.exists() {
                        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                    }
                    let mut recent = recent_task_contexts(&conn, 5)?;
                    if recent.is_empty() {
                        // No open tasks. v0.5.0 Phase A: auto-open a new
                        // task from the user's prompt so subsequent
                        // events have somewhere to land. Without this
                        // every fresh session was a black hole — events
                        // dropped silently because there was nothing to
                        // classify against. Opt-out via
                        // TJ_AUTO_OPEN_TASKS=0; only fires for
                        // UserPromptSubmit (assistant tool calls
                        // shouldn't conjure tasks).
                        let auto_open_disabled = std::env::var("TJ_AUTO_OPEN_TASKS")
                            .ok()
                            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
                            .unwrap_or(false);
                        if auto_open_disabled || !kind.contains("UserPrompt") {
                            return Ok(());
                        }
                        let Some(new_task) = auto_open_task_from_prompt(
                            &events_path,
                            &project_hash,
                            &conn,
                            &text,
                            live_session_id.as_deref(),
                        )?
                        else {
                            // Prompt was only machine noise — nothing worth a task.
                            return Ok(());
                        };
                        recent.push(new_task);
                    }

                    let classifier = build_classifier(&backend)?;
                    let input = tj_core::classifier::ClassifyInput {
                        text: text.clone(),
                        author_hint: author_hint.into(),
                        recent_tasks: recent,
                        tool_output: kind == "PostToolUse",
                    };
                    let out = match classifier.classify(&input) {
                        Ok(o) => o,
                        Err(e) => {
                            persist_pending(
                                &events_path,
                                &project_hash,
                                &kind,
                                &text,
                                &e.to_string(),
                            )?;
                            return Ok(());
                        }
                    };

                    let Some(tid) = out.task_id_guess else {
                        return Ok(());
                    };

                    // Journal-integrity safeguards. The classifier sometimes
                    // mis-attributes events to old or closed tasks (no fault
                    // of the model — its prompt only sees recent_tasks). We
                    // reject three patterns that produce confusing journals:
                    //
                    //   1. Stop-hook → Close event. The Stop hook fires at
                    //      every Claude Code session end. Session ending
                    //      != task done. Closes happen via explicit
                    //      `task-journal close <id>` only.
                    //   2. task_id_guess pointing at a non-existent task —
                    //      route to pending so the user can decide later.
                    //   3. task_id_guess pointing at a CLOSED task — same
                    //      treatment; closed tasks must stay closed.
                    use tj_core::event::EventType;
                    if matches!(out.event_type, EventType::Close) && kind == "Stop" {
                        return Ok(());
                    }
                    match tj_core::db::task_status(&conn, &tid)? {
                        None => {
                            persist_pending(
                                &events_path,
                                &project_hash,
                                &kind,
                                &text,
                                &format!("task_id_guess `{tid}` not found"),
                            )?;
                            return Ok(());
                        }
                        Some(s) if s == "closed" => {
                            persist_pending(
                                &events_path,
                                &project_hash,
                                &kind,
                                &text,
                                &format!("task_id_guess `{tid}` is closed"),
                            )?;
                            return Ok(());
                        }
                        _ => {}
                    }

                    (
                        out.event_type,
                        tid,
                        out.confidence,
                        out.evidence_strength,
                        Some(out.suggested_text),
                    )
                };

            // Use classifier's suggested_text if available (it's more concise and specific),
            // fall back to raw hook text for mock/manual events.
            let event_text = suggested_text.unwrap_or(text);

            let mut event = tj_core::event::Event::new(
                &task_id,
                etype,
                tj_core::event::Author::Classifier,
                tj_core::event::Source::Hook,
                event_text,
            );
            event.confidence = Some(confidence);
            event.status = tj_core::classifier::decide_status(confidence);
            event.evidence_strength = evidence_strength;
            tj_core::session_id::stamp_session_id(&mut event.meta, live_session_id.as_deref());

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;

            // Append telemetry. Errors here MUST NOT fail the hook (best-effort).
            let metrics_path = tj_core::paths::metrics_dir()?.join(format!("{project_hash}.jsonl"));
            let etype_str = serde_json::to_value(etype)?
                .as_str()
                .unwrap_or("?")
                .to_string();
            let status_str = serde_json::to_value(event.status)?
                .as_str()
                .unwrap_or("?")
                .to_string();
            let _ = tj_core::classifier::telemetry::append(
                &metrics_path,
                &tj_core::classifier::telemetry::TelemetryRecord {
                    timestamp: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    project_hash: project_hash.clone(),
                    task_id_guess: Some(task_id.clone()),
                    event_type: etype_str,
                    confidence,
                    status: status_str,
                    error: None,
                },
            );

            println!("{}", event.event_id);
        }
        Commands::ClassifyWorker { backend } => {
            run_classify_worker(&backend)?;
        }
        Commands::Dream {
            since,
            task,
            dry_run,
            limit,
            backend,
        } => {
            run_dream_op(since, task, dry_run, limit, backend.as_deref())?;
        }
        Commands::Complete {
            task,
            dry_run,
            enrich,
            yes,
            backend,
        } => match task {
            Some(id) => run_complete_single(&id, dry_run, enrich, backend.as_deref())?,
            None => run_complete_batch(dry_run, enrich, yes, backend.as_deref())?,
        },
        Commands::Export {
            format,
            task,
            project,
        } => {
            let cwd = match project {
                Some(p) => std::path::PathBuf::from(p),
                None => std::env::current_dir()?,
            };
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));

            if !events_path.exists() {
                anyhow::bail!("no events file at {events_path:?}");
            }

            let all_events = read_events_lenient(&events_path, "export")?;

            // Filter to specific task if requested.
            let events: Vec<&tj_core::event::Event> = if let Some(ref tid) = task {
                all_events.iter().filter(|e| e.task_id == *tid).collect()
            } else {
                all_events.iter().collect()
            };

            if events.is_empty() {
                if let Some(tid) = task {
                    anyhow::bail!("no events found for task {tid}");
                } else {
                    anyhow::bail!("no events in project");
                }
            }

            match format.as_str() {
                "json" => {
                    let json = serde_json::to_string_pretty(&events)?;
                    println!("{json}");
                }
                "md" => {
                    println!("# Task Journal Export\n");

                    // Group events by task_id.
                    let mut tasks: std::collections::BTreeMap<String, Vec<&tj_core::event::Event>> =
                        std::collections::BTreeMap::new();
                    for e in &events {
                        tasks.entry(e.task_id.clone()).or_default().push(e);
                    }

                    for (task_id, task_events) in &tasks {
                        let (title, status) = export_title_and_status(task_events);

                        // Created timestamp from first event.
                        let created = task_events
                            .first()
                            .map(|e| e.timestamp.as_str())
                            .unwrap_or("?");

                        println!("## [{task_id}] {title}");
                        println!("**Status**: {status}  ");
                        println!("**Created**: {created}\n");
                        println!("### Timeline");
                        for e in task_events {
                            let etype = serde_json::to_value(e.event_type)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_else(|| "?".into());
                            println!("- **[{}] {}**: {}", e.timestamp, etype, e.text);
                        }
                        println!();
                    }
                }
                "html" => {
                    print!("{}", render_html_timeline(&events));
                }
                "sqlite" => {
                    // Snapshot the derived SQLite state. VACUUM INTO
                    // produces a clean, defragmented copy at the target
                    // path; we then shovel its bytes to stdout so the
                    // user can `> backup.sqlite`.
                    //
                    // Always rebuild from JSONL first so the snapshot
                    // reflects every event ever appended, not just what
                    // the latest ingest happened to capture.
                    let state_path =
                        tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                    let conn = tj_core::db::open(&state_path)?;
                    tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;

                    let tmp = tempfile::TempDir::new()?;
                    let out_path = tmp.path().join("export.sqlite");
                    conn.execute(
                        "VACUUM INTO ?1",
                        rusqlite::params![out_path.to_string_lossy().into_owned()],
                    )?;
                    drop(conn);

                    let bytes = std::fs::read(&out_path)?;
                    use std::io::Write;
                    std::io::stdout()
                        .lock()
                        .write_all(&bytes)
                        .context("write sqlite snapshot to stdout")?;
                }
                other => anyhow::bail!(
                    "unknown format: {other} (expected `md`, `json`, `html`, or `sqlite`)"
                ),
            }
        }
        Commands::Search {
            query,
            limit,
            all_projects,
            event_type,
        } => {
            // v0.10.3: sanitize FTS5 query so hyphenated IDs / paths /
            // colons no longer crash with "no such column" mid-search.
            let fts_query = tj_core::fts::sanitize_query(&query);
            let like_query = tj_core::fts::like_pattern(&query);
            if all_projects {
                let state_dir = tj_core::paths::state_dir()?;
                let hashes = tj_core::db::list_all_projects(&state_dir)?;
                for hash in hashes {
                    let path = state_dir.join(format!("{hash}.sqlite"));
                    let conn = match rusqlite::Connection::open(&path) {
                        Ok(c) => c,
                        Err(e) => {
                            warn_skipped_project(&hash, e);
                            continue;
                        }
                    };
                    let ids = match run_search(
                        &conn,
                        &fts_query,
                        &like_query,
                        event_type.as_deref(),
                        limit,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            warn_skipped_project(&hash, e);
                            continue;
                        }
                    };
                    for id in ids {
                        println!("{hash}\t{id}");
                    }
                }
            } else {
                let cwd = std::env::current_dir()?;
                let project_hash = tj_core::project_hash::from_path(&cwd)?;
                let events_path =
                    tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

                let conn = tj_core::db::open(&state_path)?;
                if events_path.exists() {
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                }
                let ids = run_search(&conn, &fts_query, &like_query, event_type.as_deref(), limit)?;
                for id in ids {
                    println!("{id}");
                }
            }
        }
        Commands::Ui { project, chats } => {
            let project_path = match project {
                Some(p) => std::path::PathBuf::from(p),
                None => std::env::current_dir()?,
            };
            if chats {
                // Legacy chat-session browser. Bail early when there's
                // nothing to show — the old behavior — so users running
                // `--chats` outside a Claude Code project don't get a
                // confusing empty TUI.
                let mut app = tui::app::App::new_chats(&project_path)?;
                let empty = app
                    .session_list
                    .as_ref()
                    .map(|sl| sl.sessions.is_empty())
                    .unwrap_or(true);
                if empty {
                    eprintln!(
                        "No Claude Code sessions found for: {}",
                        project_path.display()
                    );
                    return Ok(());
                }
                app.run()?;
            } else {
                // Default: task journal browser. Empty list is fine —
                // TaskList renders a helpful "no tasks yet" placeholder
                // pointing at create / install-hooks --backfill.
                let mut app = tui::app::App::new(&project_path)?;
                app.run()?;
            }
        }
        Commands::Backfill {
            dry_run,
            limit,
            project,
        } => {
            use tj_core::session::{discovery, extractor, parser};

            let project_path = match project {
                Some(p) => std::path::PathBuf::from(p),
                None => std::env::current_dir()?,
            };

            let project_hash = tj_core::project_hash::from_path(&project_path)?;
            let events_dir = tj_core::paths::events_dir()?;
            let events_path = events_dir.join(format!("{project_hash}.jsonl"));

            // Find the Claude Code project directory for this path.
            let proj_dir = discovery::find_project_dir(&project_path)?;
            let proj_dir = match proj_dir {
                Some(d) => d,
                None => {
                    eprintln!(
                        "No Claude Code sessions found for: {} — backfill reads Claude Code \
transcripts only (Codex sessions are not read yet)",
                        project_path.display()
                    );
                    eprintln!(
                        "Looked in: {}",
                        discovery::projects_dir()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| "?".into())
                    );
                    return Ok(());
                }
            };

            // Check which sessions are already imported (idempotent): a session
            // counts once some event is tagged with it in `meta.session_id` —
            // not merely mentioned in some unrelated event's text.
            let already_imported: std::collections::HashSet<String> =
                std::fs::read_to_string(&events_path)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| serde_json::from_str::<tj_core::event::Event>(l).ok())
                    .filter_map(|e| {
                        e.meta
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .collect();

            // List available sessions. --limit counts only sessions not yet
            // imported, so a rerun reaches older ones; imported ones stay in
            // the list and are reported as skipped below.
            let mut sessions = discovery::list_sessions(&proj_dir)?;
            if let Some(max) = limit {
                let mut fresh = 0;
                sessions.retain(|p| {
                    let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                    if already_imported.contains(id) {
                        return true;
                    }

                    fresh += 1;
                    fresh <= max
                });
            }

            if sessions.is_empty() {
                eprintln!("No session JSONL files found in: {}", proj_dir.display());
                return Ok(());
            }

            eprintln!(
                "Found {} session(s) for {}",
                sessions.len(),
                project_path.display()
            );

            let mut total_tasks = 0;
            let mut total_events = 0;

            for session_path in &sessions {
                let session_id = session_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();

                if already_imported.contains(&session_id) {
                    eprintln!(
                        "  ⊘ {} — already imported, skipping",
                        &session_id[..8.min(session_id.len())]
                    );
                    continue;
                }

                // Parse the session JSONL.
                let parsed = match parser::parse_session(session_path) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!(
                            "  ✗ {} — parse error: {}",
                            &session_id[..8.min(session_id.len())],
                            e
                        );
                        continue;
                    }
                };

                // Extract events.
                let task = match extractor::extract_from_session(&parsed) {
                    Some(t) => t,
                    None => {
                        eprintln!(
                            "  ⊘ {} — too small ({} msgs), skipping",
                            &session_id[..8.min(session_id.len())],
                            parsed.user_message_count()
                        );
                        continue;
                    }
                };

                if dry_run {
                    eprintln!(
                        "  ▸ {} → task {} \"{}\" ({} events)",
                        &session_id[..8.min(session_id.len())],
                        task.task_id,
                        task.title.chars().take(60).collect::<String>(),
                        task.events.len()
                    );
                    for ev in &task.events {
                        let etype = serde_json::to_value(ev.event_type)
                            .ok()
                            .and_then(|v| v.as_str().map(String::from))
                            .unwrap_or_else(|| "?".into());
                        eprintln!(
                            "      {:12} {}",
                            etype,
                            ev.text.chars().take(80).collect::<String>()
                        );
                    }
                } else {
                    // Write events to JSONL.
                    std::fs::create_dir_all(&events_dir)?;
                    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                    for event in &task.events {
                        writer.append(event)?;
                    }
                    writer.flush_durable()?;

                    eprintln!(
                        "  ✓ {} → {} \"{}\" ({} events)",
                        &session_id[..8.min(session_id.len())],
                        task.task_id,
                        task.title.chars().take(60).collect::<String>(),
                        task.events.len()
                    );
                }

                total_tasks += 1;
                total_events += task.events.len();
            }

            if dry_run {
                eprintln!(
                    "\nDry run: would create {total_tasks} task(s) with {total_events} event(s)."
                );
                eprintln!("Run without --dry-run to import.");
            } else {
                // Index the appended events so search / pack see them now.
                if total_tasks > 0 {
                    let state_path =
                        tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                    let conn = tj_core::db::open(&state_path)?;
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                }
                eprintln!("\nImported {total_tasks} task(s) with {total_events} event(s).");
            }
        }
        Commands::Statusline => {
            // Failure mode: print empty + exit 0. CC re-renders the
            // statusline on every keystroke; a panic or non-zero exit
            // would visibly break the bottom strip. Better to look
            // empty than to look broken.
            print!("{}", run_statusline().unwrap_or_default());
        }
        Commands::Nudge => {
            run_nudge()?;
        }
        Commands::RecallHook => {
            run_recall_hook()?;
        }
        Commands::Rejected {
            topic,
            all_projects,
            limit,
            since,
        } => {
            run_rejected(&topic, all_projects, limit, since)?;
        }
        Commands::ExportPr { task_id } => {
            run_export_pr(&task_id)?;
        }
        Commands::Check { task_id, json } => {
            run_check(&task_id, json)?;
        }
        Commands::Gaps { task_id, fill } => {
            run_gaps(&task_id, fill)?;
        }
        Commands::ExportMemory {
            task,
            all_closed,
            dry_run,
        } => {
            run_export_memory(task.as_deref(), all_closed, dry_run)?;
        }
    }
    Ok(())
}

/// Returns the rendered statusline string. Sub-100ms target: ONE
/// SQLite open per project, no classifier calls, no FTS5 hits — only
/// the small `tasks` table. Empty string when there's no project
/// state at all (clean cwd outside any tracked project).
fn run_statusline() -> anyhow::Result<String> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    // Bail early on a clean machine. Both files missing → nothing to
    // show; printing empty keeps CC's bottom strip silent.
    if !state_path.exists() && !events_path.exists() {
        return Ok(String::new());
    }
    // Lazy-bootstrap the SQLite when events exist but state doesn't —
    // happens right after `create` and before any pack/search call.
    // tj_core::db::open runs migrations; ingest_new_events backfills
    // the tasks/events_index tables from JSONL.
    if !state_path.exists() && events_path.exists() {
        let conn = tj_core::db::open(&state_path)?;
        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    }
    let conn = rusqlite::Connection::open(&state_path)?;

    // Most-recently-touched open task. NULL is fine — the task line
    // becomes optional in the output.
    let recent_open: Option<String> = conn
        .query_row(
            "SELECT task_id FROM tasks WHERE project_hash = ?1 AND status = 'open'
             ORDER BY last_event_at DESC LIMIT 1",
            rusqlite::params![project_hash],
            |r| r.get::<_, String>(0),
        )
        .ok();

    let open_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE project_hash = ?1 AND status = 'open'",
            rusqlite::params![project_hash],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Stale: open + last_event_at older than 7 days. Reuse stale_tasks
    // to keep the cutoff arithmetic in one place.
    let stale_count = tj_core::db::stale_tasks(&conn, 7)?
        .into_iter()
        .filter(|t| {
            // stale_tasks doesn't filter by project, so do it here.
            // Cheaper than a second query.
            conn.query_row(
                "SELECT project_hash FROM tasks WHERE task_id = ?1",
                rusqlite::params![t.task_id],
                |r| r.get::<_, String>(0),
            )
            .map(|h| h == project_hash)
            .unwrap_or(false)
        })
        .count();

    // Pending dir is global; this project's entries carry its hash as a
    // filename prefix, so counting stays a cheap readdir with no JSON
    // parsing. Legacy un-prefixed entries are not counted.
    let prefix = format!("{project_hash}.");
    let pending_count = pending_dir()
        .ok()
        .and_then(|d| std::fs::read_dir(&d).ok())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(&prefix) && name.ends_with(".json")
                })
                .count()
        })
        .unwrap_or(0);

    let inner = match recent_open {
        Some(id) => {
            format!("{id} · open: {open_count} · pending: {pending_count} · stale: {stale_count}")
        }
        None => format!("open: {open_count} · pending: {pending_count} · stale: {stale_count}"),
    };
    Ok(format!("[{inner}]"))
}

/// `True` when the prompt's first non-whitespace token is `/rewind`
/// (case-insensitive). Pulled out as a free function so unit tests
/// can hammer the parsing without spinning up a binary.
fn is_rewind_prompt(text: &str) -> bool {
    let trimmed = text.trim_start();
    let token = trimmed.split_whitespace().next().unwrap_or("");
    token.eq_ignore_ascii_case("/rewind")
}

/// Tokens FTS5 considers special — fall back to LIKE when the topic
/// contains one of these. Mirrors the heuristic in `task_search`.
fn topic_is_fts_safe(topic: &str) -> bool {
    !topic
        .chars()
        .any(|c| matches!(c, '-' | '"' | '*' | ':' | '(' | ')'))
}

/// v0.10.3: shared search helper used by `Commands::Search` for both
/// the cwd and `--all-projects` paths. Runs the sanitized FTS5 MATCH
/// first; on zero hits, scans `search_fts.text` via `LIKE` so
/// hyphenated identifiers (e.g. `OPS-306`) and substrings missed by
/// the unicode61 tokenizer still surface.
fn run_search(
    conn: &rusqlite::Connection,
    fts_query: &str,
    like_query: &str,
    event_type: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    let (fts_sql, fts_uses_type) = match event_type {
        Some(_) => (
            "SELECT DISTINCT task_id FROM search_fts \
             WHERE search_fts MATCH ?1 AND type = ?2 LIMIT ?3",
            true,
        ),
        None => (
            "SELECT DISTINCT task_id FROM search_fts \
             WHERE search_fts MATCH ?1 LIMIT ?2",
            false,
        ),
    };
    let mut stmt = conn.prepare(fts_sql)?;
    let ids: Vec<String> = if fts_uses_type {
        let ty = event_type.unwrap();
        stmt.query_map(rusqlite::params![fts_query, ty, limit as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<_>>()?
    } else {
        stmt.query_map(rusqlite::params![fts_query, limit as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    if !ids.is_empty() {
        return Ok(ids);
    }

    let (like_sql, like_uses_type) = match event_type {
        Some(_) => (
            "SELECT DISTINCT task_id FROM search_fts \
             WHERE text LIKE ?1 AND type = ?2 LIMIT ?3",
            true,
        ),
        None => (
            "SELECT DISTINCT task_id FROM search_fts \
             WHERE text LIKE ?1 LIMIT ?2",
            false,
        ),
    };
    let mut stmt_like = conn.prepare(like_sql)?;
    let ids_like: Vec<String> = if like_uses_type {
        let ty = event_type.unwrap();
        stmt_like
            .query_map(rusqlite::params![like_query, ty, limit as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<_>>()?
    } else {
        stmt_like
            .query_map(rusqlite::params![like_query, limit as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<_>>()?
    };
    Ok(ids_like)
}

/// Cross-project reads keep going past a project they cannot read, but say
/// so on stderr instead of dropping it silently.
fn warn_skipped_project(hash: &str, err: impl std::fmt::Display) {
    eprintln!("warning: skipping project {hash}: {err}");
}

fn run_rejected(topic: &str, all_projects: bool, limit: usize, since: Option<i64>) -> Result<()> {
    let cutoff: Option<String> = since.map(|d| {
        (chrono::Utc::now() - chrono::Duration::days(d))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });

    let state_dir = tj_core::paths::state_dir()?;
    let project_filter: Option<String> = if all_projects {
        None
    } else {
        let cwd = std::env::current_dir()?;
        Some(tj_core::project_hash::from_path(&cwd)?)
    };

    let hashes: Vec<String> = if let Some(h) = &project_filter {
        // Lazy-create the SQLite for the current project so the cwd
        // case still works on a fresh clone (no events_dir yet).
        let events_path = tj_core::paths::events_dir()?.join(format!("{h}.jsonl"));
        if events_path.exists() {
            let state_path = state_dir.join(format!("{h}.sqlite"));
            let conn = tj_core::db::open(&state_path)?;
            tj_core::db::ingest_new_events(&conn, &events_path, h)?;
        }
        vec![h.clone()]
    } else {
        tj_core::db::list_all_projects(&state_dir)?
    };

    // Collect → sort by ts desc → take limit. A single UNION ALL across
    // attached DBs would be faster but rusqlite's bundled build doesn't
    // ship ATTACH-friendly ergonomics; per-project loop is fine here.
    let mut hits: Vec<(String, String, String, String, String)> = Vec::new();
    // Only the --all-projects sweep reports a skipped project: the current
    // project of a fresh clone legitimately has no tables yet.
    let skip = |hash: &str, e: rusqlite::Error| {
        if all_projects {
            warn_skipped_project(hash, e);
        }
    };
    for hash in hashes {
        let path = state_dir.join(format!("{hash}.sqlite"));
        let conn = match rusqlite::Connection::open(&path) {
            Ok(c) => c,
            Err(e) => {
                skip(&hash, e);
                continue;
            }
        };

        let use_fts = topic_is_fts_safe(topic);
        let sql = if use_fts {
            "SELECT ei.event_id, ei.task_id, ei.timestamp, sf.text, t.title
             FROM events_index ei
             JOIN search_fts sf ON sf.event_id = ei.event_id
             JOIN tasks t ON t.task_id = ei.task_id
             WHERE ei.type = 'rejection'
               AND search_fts MATCH ?1
               AND (?2 IS NULL OR ei.timestamp >= ?2)
             ORDER BY ei.timestamp DESC LIMIT ?3"
        } else {
            "SELECT ei.event_id, ei.task_id, ei.timestamp, sf.text, t.title
             FROM events_index ei
             JOIN search_fts sf ON sf.event_id = ei.event_id
             JOIN tasks t ON t.task_id = ei.task_id
             WHERE ei.type = 'rejection'
               AND sf.text LIKE ?1
               AND (?2 IS NULL OR ei.timestamp >= ?2)
             ORDER BY ei.timestamp DESC LIMIT ?3"
        };

        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(e) => {
                skip(&hash, e);
                continue;
            }
        };
        let bind_q = if use_fts {
            topic.to_string()
        } else {
            format!("%{topic}%")
        };
        let rows = match stmt.query_map(rusqlite::params![bind_q, cutoff, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, String>(4)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                skip(&hash, e);
                continue;
            }
        };
        // Keep going past a row that fails to map, but say so once.
        let mut first_err = None;
        for row in rows {
            match row {
                Ok(row) => hits.push(row),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if let Some(e) = first_err {
            eprintln!("warning: skipped unreadable rejection row(s) in project {hash}: {e}");
        }
    }

    // Cross-project re-sort. Within one project the SQL ORDER BY
    // already did this, but UNION-ALL semantics need a second pass.
    hits.sort_by(|a, b| b.2.cmp(&a.2));
    hits.truncate(limit);

    for (_eid, task_id, ts, text, title) in hits {
        // YYYY-MM-DD slice of an RFC3339 timestamp; cheap and stable.
        let date = ts.get(..10).unwrap_or(&ts);
        // Squash newlines so multi-line rejections still render as
        // one block per hit.
        let one_line: String = text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        println!("{task_id}\t{date}\t\"{one_line}\"");
        println!("\t\t(in task: {title})");
    }
    Ok(())
}

fn run_export_pr(task_id: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    }

    // Fetch task title up-front; bail with a typed exit code so callers
    // can distinguish "not found" from a generic IO error.
    let title: String = match conn.query_row(
        "SELECT title FROM tasks WHERE task_id = ?1",
        rusqlite::params![task_id],
        |r| r.get::<_, String>(0),
    ) {
        Ok(t) => t,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            eprintln!("Error: task not found: {task_id}");
            std::process::exit(1);
        }
        Err(e) => return Err(e.into()),
    };

    let meta = tj_core::db::task_metadata(&conn, task_id)?.unwrap_or_default();
    let summary = meta.goal.unwrap_or_else(|| title.clone());

    // Pull all events ordered ASC so the PR description reads like a
    // narrative (oldest decision first → newest).
    let mut stmt = conn.prepare(
        "SELECT ei.type, sf.text FROM events_index ei
         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.task_id = ?1 ORDER BY ei.timestamp ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![task_id], |r| {
        let ty: String = r.get(0)?;
        let txt: Option<String> = r.get(1)?;
        Ok((ty, txt.unwrap_or_default()))
    })?;
    let mut decisions: Vec<String> = Vec::new();
    let mut rejections: Vec<String> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();
    for row in rows {
        let (ty, text) = row?;
        let one_line: String = text.lines().next().unwrap_or("").trim().to_string();
        if one_line.is_empty() {
            continue;
        }
        match ty.as_str() {
            "decision" => decisions.push(one_line),
            "rejection" => rejections.push(one_line),
            "evidence" => evidence.push(one_line),
            _ => {}
        }
    }

    let arts = tj_core::db::task_artifacts(&conn, task_id)?;

    let mut out = String::new();
    out.push_str("## Summary\n");
    out.push_str(&summary);
    out.push_str("\n\n");

    out.push_str("## Changes\n");
    if decisions.is_empty() {
        out.push_str("- (no decision events recorded)\n");
    } else {
        for d in &decisions {
            out.push_str(&format!("- {d}\n"));
        }
    }
    out.push('\n');

    if !rejections.is_empty() {
        out.push_str("## Why this approach (vs alternatives)\n");
        for r in &rejections {
            out.push_str(&format!("- {r}\n"));
        }
        out.push('\n');
    }

    if !evidence.is_empty() {
        out.push_str("## Verification\n");
        for e in &evidence {
            out.push_str(&format!("- {e}\n"));
        }
        out.push('\n');
    }

    let any_arts = !arts.files.is_empty()
        || !arts.commit_hashes.is_empty()
        || !arts.linked_issues.is_empty()
        || !arts.branch_names.is_empty()
        || !arts.pr_urls.is_empty();
    if any_arts {
        out.push_str("## Affected\n");
        if !arts.files.is_empty() {
            out.push_str(&format!("- Files: {}\n", arts.files.join(", ")));
        }
        if !arts.commit_hashes.is_empty() {
            out.push_str(&format!("- Commits: {}\n", arts.commit_hashes.join(", ")));
        }
        if !arts.linked_issues.is_empty() {
            out.push_str(&format!("- Issues: {}\n", arts.linked_issues.join(", ")));
        }
        if !arts.branch_names.is_empty() {
            out.push_str(&format!("- Branches: {}\n", arts.branch_names.join(", ")));
        }
        if !arts.pr_urls.is_empty() {
            out.push_str(&format!("- PRs: {}\n", arts.pr_urls.join(", ")));
        }
        out.push('\n');
    }

    print!("{}", out);
    Ok(())
}

/// Severity label for a gap weight (10/3/1 → error/warn/info).
fn severity_for(weight: u32) -> &'static str {
    match weight {
        10 => "error",
        3 => "warn",
        _ => "info",
    }
}

/// Open the project DB, ingest new events, and build the full completeness
/// report — structural gaps plus artifact honesty drift — for `task_id`.
/// Exits(1) when the task is unknown.
fn assess_task(
    task_id: &str,
) -> Result<(
    rusqlite::Connection,
    tj_core::completeness::CompletenessReport,
)> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    }

    // Typed not-found exit, mirroring run_export_pr.
    if let Err(rusqlite::Error::QueryReturnedNoRows) = conn.query_row(
        "SELECT 1 FROM tasks WHERE task_id = ?1",
        rusqlite::params![task_id],
        |r| r.get::<_, i64>(0),
    ) {
        eprintln!("Error: task not found: {task_id}");
        std::process::exit(1);
    }

    let mut report =
        tj_core::completeness::assess(&conn, task_id, tj_core::completeness::pending_count())?;
    let arts = tj_core::db::task_artifacts(&conn, task_id)?;
    report
        .gaps
        .extend(tj_core::completeness::artifact_gaps_for_cwd(&arts));
    Ok((conn, report))
}

/// `task-journal check <id> [--json]` — print honesty score + gaps.
fn run_check(task_id: &str, json: bool) -> Result<()> {
    let (_conn, report) = assess_task(task_id)?;
    if json {
        let gaps: Vec<_> = report
            .gaps
            .iter()
            .map(|g| {
                serde_json::json!({
                    "kind": format!("{:?}", g.kind),
                    "severity": severity_for(g.kind.weight()),
                    "detail": g.detail,
                })
            })
            .collect();
        let out = serde_json::json!({
            "task_id": task_id,
            "score": report.score(),
            "gaps": gaps,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "honesty score: {}/100 ({} gap(s))",
            report.score(),
            report.gaps.len()
        );
        for g in &report.gaps {
            println!("- [{}] {}", severity_for(g.kind.weight()), g.detail);
        }
    }
    Ok(())
}

/// `task-journal gaps <id> [--fill]` — list gaps, or with `--fill` print the
/// deterministic gap-fill prompt (embedding the current pack) for the agent.
fn run_gaps(task_id: &str, fill: bool) -> Result<()> {
    let (conn, report) = assess_task(task_id)?;
    if !fill {
        println!(
            "honesty score: {}/100 ({} gap(s))",
            report.score(),
            report.gaps.len()
        );
        for g in &report.gaps {
            println!("- [{}] {}", severity_for(g.kind.weight()), g.detail);
        }
        return Ok(());
    }
    let pack = tj_core::pack::assemble(&conn, task_id, tj_core::pack::PackMode::Full)?;
    match tj_core::completeness::build_gap_fill_prompt(task_id, &report, &pack.text) {
        Some(prompt) => print!("{prompt}"),
        None => println!("honesty score: 100/100 — no gaps to fill"),
    }
    Ok(())
}

fn run_export_memory(task: Option<&str>, _all_closed: bool, dry_run: bool) -> Result<()> {
    const MAX_ITEMS: usize = 10;

    let cwd = std::env::current_dir()?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    }

    // Resolve scope.
    let task_ids: Vec<String> = match task {
        Some(id) => {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM tasks WHERE task_id = ?1",
                    rusqlite::params![id],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if !exists {
                eprintln!("Error: task not found: {id}");
                std::process::exit(1);
            }
            vec![id.to_string()]
        }
        None => {
            // default + --all-closed → all closed tasks
            let mut stmt =
                conn.prepare("SELECT task_id FROM tasks WHERE status='closed' ORDER BY task_id")?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ids
        }
    };

    if task_ids.is_empty() {
        eprintln!("note: no closed tasks to export");
        return Ok(());
    }

    // Memory dir: ~/.claude/projects/<encoded-cwd>/memory/
    let memory_dir = tj_core::session::discovery::projects_dir()?
        .join(tj_core::session::discovery::encode_project_path(&cwd_str))
        .join("memory");

    for id in &task_ids {
        let title: String = conn.query_row(
            "SELECT title FROM tasks WHERE task_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )?;
        let meta = tj_core::db::task_metadata(&conn, id)?.unwrap_or_default();

        // decision + constraint one-liners, oldest-first (== run_export_pr style).
        let mut stmt = conn.prepare(
            "SELECT ei.type, sf.text FROM events_index ei
             LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
             WHERE ei.task_id = ?1 AND ei.type IN ('decision','constraint')
             ORDER BY ei.timestamp ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        let mut decisions = Vec::new();
        let mut constraints = Vec::new();
        for row in rows {
            let (ty, text) = row?;
            let line = text.lines().next().unwrap_or("").trim().to_string();
            if line.is_empty() {
                continue;
            }
            match ty.as_str() {
                "decision" if decisions.len() < MAX_ITEMS => decisions.push(line),
                "constraint" if constraints.len() < MAX_ITEMS => constraints.push(line),
                _ => {}
            }
        }

        let slug = tj_core::frontmatter::slugify(&title);
        let content = tj_core::frontmatter::render_memory(&tj_core::frontmatter::MemoryInput {
            title: &title,
            meta: &meta,
            decisions: &decisions,
            constraints: &constraints,
        });
        let file_path = memory_dir.join(format!("tj-{id}-{slug}.md"));

        if dry_run {
            println!("# would write: {}", file_path.display());
            println!("{content}");
        } else {
            std::fs::create_dir_all(&memory_dir)?;
            std::fs::write(&file_path, content)?;
            println!("wrote {}", file_path.display());
        }
    }
    Ok(())
}

/// How many of a task's most-recent `constraint` events to surface in
/// the classifier prompt. Kept small so the prompt stays bounded.
const CONSTRAINT_CONTEXT_LIMIT: i64 = 5;

fn recent_task_contexts(
    conn: &rusqlite::Connection,
    limit: usize,
) -> anyhow::Result<Vec<tj_core::classifier::TaskContext>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, title FROM tasks WHERE status='open' ORDER BY last_event_at DESC LIMIT ?1",
    )?;
    let task_rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::with_capacity(task_rows.len());
    for (task_id, title) in task_rows {
        let mut e_stmt = conn.prepare(
            "SELECT ei.type, sf.text FROM events_index ei
             LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
             WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT 3",
        )?;
        let last_events: Vec<String> = e_stmt
            .query_map(rusqlite::params![task_id], |r| {
                let ty: String = r.get(0)?;
                let txt: Option<String> = r.get(1)?;
                Ok(format!(
                    "[{ty}] {}",
                    txt.unwrap_or_default().chars().take(80).collect::<String>()
                ))
            })?
            .collect::<Result<_, _>>()?;

        // Gather the task's most-recent `constraint` events so the
        // classifier can recognise chunks that violate a known limit.
        // Mirrors the last_events join, filtered to constraints and
        // bounded to keep the prompt small.
        let mut c_stmt = conn.prepare(
            "SELECT sf.text FROM events_index ei
             LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
             WHERE ei.task_id = ?1 AND ei.type = 'constraint'
             AND COALESCE(sf.text, '') NOT LIKE ?3
             ORDER BY ei.timestamp DESC LIMIT ?2",
        )?;
        let model_switch = format!("{}%", tj_core::reminder::MODEL_SWITCH_TEXT_PREFIX);
        let constraints: Vec<String> = c_stmt
            .query_map(
                rusqlite::params![task_id, CONSTRAINT_CONTEXT_LIMIT, model_switch],
                |r| {
                    let txt: Option<String> = r.get(0)?;
                    Ok(txt
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect::<String>())
                },
            )?
            .collect::<Result<Vec<String>, _>>()?
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();

        out.push(tj_core::classifier::TaskContext {
            task_id,
            title,
            last_events,
            constraints,
        });
    }
    Ok(out)
}

/// v0.5.0 Phase A: when ingest-hook fires UserPromptSubmit and there
/// are no open tasks, synthesize one from the prompt itself. Title is
/// the first line trimmed to 80 chars; goal is the prompt trimmed to
/// 200 chars. Returns a TaskContext so the classifier has somewhere
/// to attach the same prompt as the first real event.
/// Best-effort sync of a project's high-signal events into the global
/// cross-project memory index. Never fails the caller — a slightly stale recall
/// index is fine; a broken `ask`/`embed` is not.
fn sync_global_memory(project_conn: &rusqlite::Connection, project_hash: &str) {
    let result = tj_core::paths::memory_db()
        .and_then(tj_core::memory::open)
        .and_then(|g| tj_core::memory::sync_from_project(&g, project_conn, project_hash));
    if let Err(e) = result {
        tracing::debug!("global memory sync skipped: {e:#}");
    }
}

const NUDGE_BASE: &str = "📓 task-journal — record as you go: the moment you commit to a decision, rule an approach out, or verify a fact, call event_add (open or resume a task first). Don't batch it to the end. This memory only works if you log it now.";
/// A session whose transcript is at least this big counts as "substantial work".
const NUDGE_WORK_THRESHOLD: u64 = 60_000;
/// Below this many journal entries for a substantial session, escalate.
const NUDGE_MIN_EVENTS: usize = 2;

/// The escalation line, or `None` when no escalation is warranted. Pure so the
/// thresholds are unit-testable without touching the filesystem.
fn nudge_escalation_text(work_bytes: u64, recorded: usize) -> Option<String> {
    if work_bytes < NUDGE_WORK_THRESHOLD || recorded >= NUDGE_MIN_EVENTS {
        return None;
    }
    Some(format!(
        "⚠ task-journal: this session has done substantial work but recorded only \
{recorded} journal entr{} — log the key decisions, rejections, and findings NOW via \
event_add before this reasoning is lost.",
        if recorded == 1 { "y" } else { "ies" }
    ))
}

/// Count events in the tail of `path` stamped with `sid`. A tail scan keeps this
/// cheap even for a large journal; a session's events are always at the end.
fn count_session_events_tail(path: &std::path::Path, sid: &str, tail_lines: usize) -> usize {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(tail_lines);
    lines[start..]
        .iter()
        .filter(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|e| {
                    e.get("meta")
                        .and_then(|m| m.get("session_id"))
                        .and_then(|s| s.as_str())
                        .map(|s| s == sid)
                })
                .unwrap_or(false)
        })
        .count()
}

/// True when the Claude Code mod (`plugin/hooks/register.ts`) runs in this
/// session: it sets `TJ_MOD_ACTIVE` for every hook it starts. Codex never does.
fn mod_active() -> bool {
    std::env::var("TJ_MOD_ACTIVE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Adaptive UserPromptSubmit nudge (caveman pattern, non-blocking, free): always
/// emit the base "record as you go" reminder, and — when the session has done
/// substantial work but logged little — escalate. All signals are cheap (a file
/// size + a tail scan); no model, never blocks the prompt.
fn run_nudge() -> anyhow::Result<()> {
    // Recursion guard, same as recall-hook: never inject into our own
    // classifier child (`claude -p` / `codex exec` re-run the user's hooks).
    if std::env::var(tj_core::classifier::agent_sdk::IN_CLASSIFIER_ENV).is_ok() {
        return Ok(());
    }
    // The Claude Code mod nudges by itself (after N turns without an entry).
    if mod_active() {
        return Ok(());
    }

    let mut ctx = NUDGE_BASE.to_string();
    let escalation = (|| -> Option<String> {
        use std::io::{IsTerminal, Read};
        // Manual `task-journal nudge` in a terminal has no hook payload — don't
        // block waiting on stdin.
        if std::io::stdin().is_terminal() {
            return None;
        }
        let mut buf = String::new();
        if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
            return None;
        }
        let payload: serde_json::Value = serde_json::from_str(&buf).ok()?;
        let sid = tj_core::session_id::live_session_id(Some(&payload))?;
        let transcript = payload.get("transcript_path").and_then(|v| v.as_str())?;
        let work_bytes = std::fs::metadata(transcript).map(|m| m.len()).unwrap_or(0);
        let cwd = std::env::current_dir().ok()?;
        let project_hash = tj_core::project_hash::from_path(&cwd).ok()?;
        let events_path = tj_core::paths::events_dir()
            .ok()?
            .join(format!("{project_hash}.jsonl"));
        let recorded = count_session_events_tail(&events_path, &sid, 400);
        nudge_escalation_text(work_bytes, recorded)
    })();
    if let Some(extra) = escalation {
        ctx.push('\n');
        ctx.push_str(&extra);
    }
    let env = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": ctx,
        }
    });
    print!("{env}");
    Ok(())
}

/// Proactive recall injector (opt-in hook). Reads the UserPromptSubmit payload
/// SessionEnd(reason=clear) catch-up: enqueue transcript chunks newer than the
/// active task's last event, then spawn the classify-worker. Kept OUT of `main`
/// so its locals don't grow `main`'s already-huge stack frame — inlining it
/// overflowed the 1 MiB Windows main-thread stack on every command.
fn run_session_end_catchup(
    payload: &serde_json::Value,
    events_path: &std::path::Path,
    project_hash: &str,
    backend: &str,
    live_session_id: Option<&str>,
) -> anyhow::Result<()> {
    let reason = payload.get("reason").and_then(|x| x.as_str()).unwrap_or("");
    if reason != "clear" || !events_path.exists() {
        return Ok(());
    }
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    tj_core::db::ingest_new_events(&conn, events_path, project_hash)?;
    let Some(tc) = recent_task_contexts(&conn, 1)?.into_iter().next() else {
        return Ok(());
    };
    let last_event_ts: Option<String> = conn
        .query_row(
            "SELECT timestamp FROM events_index WHERE task_id=?1 ORDER BY timestamp DESC LIMIT 1",
            rusqlite::params![&tc.task_id],
            |r| r.get::<_, String>(0),
        )
        .ok();
    let transcript_path = payload
        .get("transcript_path")
        .and_then(|x| x.as_str())
        .map(std::path::PathBuf::from);
    if let Some(tp) = transcript_path.as_ref() {
        if tp.exists() {
            let enq = enqueue_transcript_chunks_since_last_event(
                tp,
                events_path,
                project_hash,
                backend,
                last_event_ts.as_deref(),
                "SessionEndChunk",
                live_session_id,
            )
            .unwrap_or(0);
            if enq > 0 && std::env::var("TJ_DISABLE_CLASSIFY_SPAWN").is_err() {
                let _ = spawn_classify_worker(backend);
            }
        }
    }
    Ok(())
}

/// from stdin, keyword-searches the global index for relevant prior
/// decisions/rejections/constraints across all projects, and emits a budgeted
/// `additionalContext` block. Never blocks the prompt: any miss, empty result,
/// or error exits silently with no output.
fn run_recall_hook() -> anyhow::Result<()> {
    // Opt-out and recursion guard (never inject into our own classifier spawn).
    if std::env::var("TJ_PROACTIVE_RECALL").as_deref() == Ok("0") {
        return Ok(());
    }
    if std::env::var(tj_core::classifier::agent_sdk::IN_CLASSIFIER_ENV).is_ok() {
        return Ok(());
    }
    let global_path = tj_core::paths::memory_db()?;
    if !global_path.exists() {
        return Ok(());
    }

    use std::io::Read;
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return Ok(());
    }
    // The UserPromptSubmit payload carries the prompt under `prompt`; fall back
    // to the raw stdin if it isn't JSON.
    let prompt = serde_json::from_str::<serde_json::Value>(&buf)
        .ok()
        .and_then(|v| {
            v.get("prompt")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or(buf);
    if prompt.trim().is_empty() {
        return Ok(());
    }

    let conn = tj_core::memory::open(&global_path)?;
    let k: usize = std::env::var("TJ_RECALL_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let hits = tj_core::memory::keyword_search(&conn, &prompt, k)?;
    if hits.is_empty() {
        return Ok(());
    }

    let budget: usize = std::env::var("TJ_RECALL_BUDGET_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);
    let mut ctx = String::from(
        "📓 task-journal — relevant prior reasoning from your history (you may have decided this before):\n",
    );
    for h in &hits {
        let snippet: String = h.text.chars().take(160).collect();
        let proj: String = h.project_hash.chars().take(8).collect();
        let line = format!(
            "⚠ [{}] {} (project {proj}, {})\n",
            h.event_type, snippet, h.task_id
        );
        if ctx.len() + line.len() > budget {
            break;
        }
        ctx.push_str(&line);
    }
    let env = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": ctx.trim_end(),
        }
    });
    print!("{env}");
    Ok(())
}

/// Serialize recall hits to a compact JSON array for machine consumers (the Loom
/// host). Pure — split out so the shape is unit-testable without touching stdout.
fn recall_hits_json(hits: &[tj_core::memory::GlobalHit]) -> String {
    let out: Vec<serde_json::Value> = hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "task_id": h.task_id,
                "project_hash": h.project_hash,
                "event_type": h.event_type,
                "text": h.text,
                "score": h.score,
            })
        })
        .collect();
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

/// Render the user's standing preferences as a SessionStart context block, or
/// "" when there are none. Capped so it never floods the system prompt.
fn session_preferences_block() -> String {
    let prefs = match tj_core::paths::memory_db()
        .and_then(tj_core::memory::open)
        .and_then(|c| tj_core::memory::list_preferences(&c))
    {
        Ok(p) if !p.is_empty() => p,
        _ => return String::new(),
    };
    let mut s = String::from("## Your standing preferences (remember these across sessions):\n");
    for p in prefs {
        let line = format!("- {p}\n");
        if s.len() + line.len() > 800 {
            break;
        }
        s.push_str(&line);
    }
    s.trim_end().to_string()
}

/// How long a resumed or forked conversation must have sat idle before the
/// active-task reminder earns its place in `additionalContext`. Eight hours —
/// roughly "picked it up the next day".
const STALE_RESUME_SECS: u64 = 8 * 60 * 60;

/// Coarse, human-readable gap ("14h", "3d") for the reminder label.
fn human_gap(secs: u64) -> String {
    let hours = secs / 3600;
    if hours >= 48 {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

/// Emit a SessionStart `additionalContext` envelope and nothing else.
fn emit_session_context(ctx: &str) {
    let env = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": ctx.trim_end(),
        }
    });
    println!("{env}");
}

const CONSOLIDATE_TASK_TITLE: &str = "Project conventions (consolidated)";

/// Dream backfill, shared by `dream` and `complete`: re-read the in-scope
/// session transcripts and append the events the live capture missed, via the
/// chosen pluggable LLM backend. Skips cleanly when no backend is available.
fn run_dream_op(
    since: Option<i64>,
    task: Option<String>,
    dry_run: bool,
    limit: Option<usize>,
    backend: Option<&str>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;

    // 1. Resolve session files in scope.
    let project_dir = tj_core::session::discovery::find_project_dir(&cwd)?;
    let Some(project_dir) = project_dir else {
        println!(
            "dream: no Claude Code session directory for this project — dream mines \
Claude Code transcripts only (Codex sessions are not read yet)"
        );
        return Ok(());
    };
    let session_paths = tj_core::session::discovery::list_sessions(&project_dir)?;

    let since_time = if let Some(days) = since {
        Some(
            std::time::SystemTime::now()
                - std::time::Duration::from_secs((days.max(0) as u64) * 86_400),
        )
    } else {
        match tj_core::dream::state::last_dream_at(&conn, &project_hash)? {
            Some(ts) => chrono::DateTime::parse_from_rfc3339(&ts)
                .ok()
                .map(std::time::SystemTime::from),
            None => None,
        }
    };

    let scoped: Vec<tj_core::dream::scope::SessionFile> = session_paths
        .into_iter()
        .filter_map(|p| {
            let mtime = std::fs::metadata(&p).ok()?.modified().ok()?;
            Some(tj_core::dream::scope::SessionFile { path: p, mtime })
        })
        .collect();
    let mtimes: std::collections::HashMap<std::path::PathBuf, std::time::SystemTime> =
        scoped.iter().map(|s| (s.path.clone(), s.mtime)).collect();
    let in_scope = tj_core::dream::scope::in_scope(scoped, since_time, limit);

    // 2. Assemble (session_id, BackfillInput) per session.
    let run_id = ulid::Ulid::new().to_string();
    let (sessions, unreadable) = build_dream_inputs(&events_path, &in_scope, task.as_deref())?;

    let opts = tj_core::dream::DreamOptions {
        project_hash: project_hash.clone(),
        dry_run,
    };
    if dry_run {
        println!("dream (dry-run): {} session(s) in scope", sessions.len());
        return Ok(());
    }

    // 3. Backend via the unified pluggable selector (default claude-p).
    let llm = match tj_core::llm::backend_from_env(backend)? {
        Some(l) => l,
        None => {
            println!(
                "dream: no usable LLM backend. Default `claude-p` needs Claude Code on \
PATH; or pick one via --backend / TJ_BACKEND: anthropic, openai, ollama (free, local)."
            );
            return Ok(());
        }
    };
    let dream_backend = tj_core::dream::llm_backend::LlmDreamBackend::new(llm);
    eprintln!("dream: backend={}", dream_backend.backend_name());
    let report = tj_core::dream::run_dream(
        &conn,
        &events_path,
        &opts,
        &dream_backend,
        sessions,
        &run_id,
    )?;

    // 4. Advance the watermark — only on an unscoped run (--task / --limit /
    // --since skip sessions they never looked at), and only to the newest
    // session such that it and every older in-scope one were mined cleanly.
    if since.is_none() && task.is_none() && limit.is_none() {
        let mined: Vec<(std::time::SystemTime, bool)> = in_scope
            .iter()
            .map(|p| {
                let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let clean = !unreadable.iter().any(|u| u == id)
                    && !report.failed_sessions.iter().any(|f| f == id);
                (mtimes[p], clean)
            })
            .collect();
        if let Some(t) = tj_core::dream::scope::next_watermark(&mined) {
            let at = chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339();
            tj_core::dream::state::set_last_dream_at(&conn, &project_hash, &at)?;
        }
    }

    let mut summary = format!(
        "dream: {} session(s) processed, {} event(s) backfilled",
        report.sessions_processed, report.events_backfilled
    );
    if report.events_dropped_unknown_task > 0 {
        summary.push_str(&format!(
            ", {} dropped (unknown task id)",
            report.events_dropped_unknown_task
        ));
    }
    if !report.failed_sessions.is_empty() {
        summary.push_str(&format!(
            ", {} only partly mined (retried next run)",
            report.failed_sessions.len()
        ));
    }
    println!("{summary}");
    Ok(())
}

// ---------------------------------------------------------------------------
// complete / finalize: enrich a task's memory, fix a junk title, and close it
// if the events clearly show it is done. The model judges from content.
// ---------------------------------------------------------------------------

/// What `finalize_one_task` did, for the caller to report.
#[derive(Default)]
struct FinalizeOutcome {
    enriched: usize,
    retitled: Option<(String, String)>,
    closed: bool,
    done: bool,
    reason: String,
    /// True when no LLM backend was available — nothing was judged or written.
    skipped_no_backend: bool,
    /// Exact token usage spent on this task (judge + any enrich calls).
    spent: tj_core::llm::LlmUsage,
    /// Estimated memory compression: raw session tokens → compact pack tokens.
    saved: Option<Savings>,
}

/// Rough memory-compression estimate for a finalized task (≈ chars / 4).
#[derive(Default, Clone, Copy)]
struct Savings {
    raw_tokens: u64,
    pack_tokens: u64,
}

/// ~tokens from a char count (a rough 4-chars-per-token estimate — enough for
/// an order-of-magnitude "how much memory this compresses" signal).
fn est_tokens(chars: usize) -> u64 {
    (chars as u64).div_ceil(4)
}

/// Estimate how much raw session material a task's compact pack stands in for:
/// the summed transcript size of the sessions it touched vs the pack size.
/// `None` when sessions aren't reachable (no project dir).
fn compute_savings(
    conn: &rusqlite::Connection,
    events_path: &std::path::Path,
    project_dir: Option<&std::path::Path>,
    task_id: &str,
) -> Option<Savings> {
    let dir = project_dir?;
    let sessions = task_sessions(events_path, dir, task_id).ok()?;
    if sessions.is_empty() {
        return None;
    }
    let raw_chars: usize = sessions.iter().map(|(_, inp)| inp.transcript.len()).sum();
    let pack = tj_core::pack::assemble(conn, task_id, tj_core::pack::PackMode::Compact).ok()?;
    Some(Savings {
        raw_tokens: est_tokens(raw_chars),
        pack_tokens: est_tokens(pack.text.len()),
    })
}

/// Format a token count compactly: 980 → "980", 3_240 → "3.2k", 88_000 → "88k",
/// 2_760_000 → "2.8M".
fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 100_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Human spent/saved suffix for a finalize line, e.g.
/// " | spent 3.2k tok ($0.0012) · saved ~88k→1.5k tok (59×)".
fn stats_suffix(spent: &tj_core::llm::LlmUsage, saved: &Option<Savings>) -> String {
    let mut parts = Vec::new();
    // claude -p reports a (notional) dollar cost but muddy token counts — its
    // big prompt lands in `cache_creation`, not `input_tokens` — so lead with
    // the cost there. API backends report no cost but clean tokens, so show
    // those instead.
    match spent.cost_usd {
        Some(c) if c > 0.0 => parts.push(format!("cost ${c:.4}")),
        _ if spent.total_tokens() > 0 => {
            parts.push(format!("spent {} tok", fmt_tokens(spent.total_tokens())))
        }
        _ => {}
    }
    if let Some(s) = saved {
        if s.pack_tokens > 0 && s.raw_tokens > s.pack_tokens {
            let factor = s.raw_tokens as f64 / s.pack_tokens as f64;
            parts.push(format!(
                "saved ~{}→{} tok ({:.0}×)",
                fmt_tokens(s.raw_tokens),
                fmt_tokens(s.pack_tokens),
                factor
            ));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" | {}", parts.join(" · "))
    }
}

/// Per-project handles threaded through the finalize helpers.
struct ProjectCtx<'a> {
    conn: &'a rusqlite::Connection,
    events_path: &'a std::path::Path,
    project_hash: &'a str,
    project_dir: Option<&'a std::path::Path>,
}

/// The (session_id, BackfillInput) pairs for every session that touched
/// `task_id`. No watermark: finalize scopes to the task, not to "since the
/// last dream", so tasks can be finalized independently and repeatedly.
fn task_sessions(
    events_path: &std::path::Path,
    project_dir: &std::path::Path,
    task_id: &str,
) -> anyhow::Result<Vec<(String, tj_core::dream::backend::BackfillInput)>> {
    let session_paths = tj_core::session::discovery::list_sessions(project_dir)?;
    let scoped: Vec<tj_core::dream::scope::SessionFile> = session_paths
        .into_iter()
        .filter_map(|p| {
            let mtime = std::fs::metadata(&p).ok()?.modified().ok()?;
            Some(tj_core::dream::scope::SessionFile { path: p, mtime })
        })
        .collect();
    let in_scope = tj_core::dream::scope::in_scope(scoped, None, None);
    Ok(build_dream_inputs(events_path, &in_scope, Some(task_id))?.0)
}

/// Enrich a single task from every session that touched it. Unlike `dream`,
/// this is task-scoped and does NOT read or advance the project-global dream
/// watermark. `dedup_guard` inside `run_dream` keeps re-runs from duplicating
/// events. Returns the number of events appended.
fn enrich_task(
    conn: &rusqlite::Connection,
    events_path: &std::path::Path,
    project_hash: &str,
    project_dir: &std::path::Path,
    task_id: &str,
    llm: Box<dyn tj_core::llm::LlmBackend>,
) -> anyhow::Result<(usize, tj_core::llm::LlmUsage)> {
    let sessions = task_sessions(events_path, project_dir, task_id)?;
    if sessions.is_empty() {
        return Ok((0, tj_core::llm::LlmUsage::default()));
    }
    // Enrich is the slow part — one (or more, for big transcripts) `claude -p`
    // call per session. Announce it so a multi-minute run doesn't look hung;
    // `--quick` skips this entirely.
    eprintln!(
        "complete: enriching {} session(s) via {} — can take a few minutes (or use --quick to skip)…",
        sessions.len(),
        llm.name()
    );
    let run_id = ulid::Ulid::new().to_string();
    let dream_backend = tj_core::dream::llm_backend::LlmDreamBackend::new(llm);
    let opts = tj_core::dream::DreamOptions {
        project_hash: project_hash.to_string(),
        dry_run: false,
    };
    let report =
        tj_core::dream::run_dream(conn, events_path, &opts, &dream_backend, sessions, &run_id)?;
    Ok((report.events_backfilled, dream_backend.usage()))
}

/// Current title for a task ("" if somehow unset).
fn task_title(conn: &rusqlite::Connection, task_id: &str) -> anyhow::Result<String> {
    let mut stmt = conn.prepare("SELECT title FROM tasks WHERE task_id=?1")?;
    let mut rows = stmt.query(rusqlite::params![task_id])?;
    Ok(match rows.next()? {
        Some(r) => r.get::<_, String>(0)?,
        None => String::new(),
    })
}

/// A task's events as `[type] text` lines, oldest first, capped so the judge
/// prompt stays bounded on very long tasks.
fn task_event_lines(conn: &rusqlite::Connection, task_id: &str) -> anyhow::Result<Vec<String>> {
    const MAX_LINES: usize = 150;
    let mut stmt = conn.prepare(
        "SELECT ei.type, sf.text FROM events_index ei
         LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
         WHERE ei.task_id=?1 ORDER BY ei.timestamp ASC",
    )?;
    let all: Vec<String> = stmt
        .query_map(rusqlite::params![task_id], |r| {
            let ty: String = r.get(0)?;
            let txt: Option<String> = r.get(1)?;
            let one = txt
                .unwrap_or_default()
                .replace('\n', " ")
                .chars()
                .take(200)
                .collect::<String>();
            Ok(format!("[{ty}] {one}"))
        })?
        .collect::<Result<_, _>>()?;
    let start = all.len().saturating_sub(MAX_LINES);
    Ok(all[start..].to_vec())
}

/// Finalize one task: enrich → judge → retitle-if-junk → close-if-done.
/// Writes Rename/Close events (which carry their metadata so a rebuild keeps
/// them) and refreshes the index. Reports what happened via `FinalizeOutcome`.
fn finalize_one_task(
    ctx: &ProjectCtx<'_>,
    task_id: &str,
    enrich: bool,
    dry_run: bool,
    backend: Option<&str>,
) -> anyhow::Result<FinalizeOutcome> {
    let mut out = FinalizeOutcome::default();
    let conn = ctx.conn;
    let events_path = ctx.events_path;
    let project_hash = ctx.project_hash;

    // 1. Enrich (only when asked, and not on a dry-run) — needs sessions and a
    // backend. Off by default because it is slow (one claude -p per session).
    if enrich && !dry_run {
        if let Some(dir) = ctx.project_dir {
            if let Some(llm) = tj_core::llm::backend_from_env(backend)? {
                let (n, enrich_usage) =
                    enrich_task(conn, events_path, project_hash, dir, task_id, llm)?;
                out.enriched = n;
                out.spent.add(enrich_usage);
                tj_core::db::ingest_new_events(conn, events_path, project_hash)?;
            }
        }
    }

    // 2. Gather the (now enriched) title + history.
    let title = task_title(conn, task_id)?;
    let lines = task_event_lines(conn, task_id)?;

    if dry_run {
        let sessions = match ctx.project_dir {
            Some(dir) => task_sessions(events_path, dir, task_id)?.len(),
            None => 0,
        };
        println!(
            "complete (dry-run) {task_id}: {} event(s), {sessions} session(s) to enrich, title={title:?}",
            lines.len()
        );
        return Ok(out);
    }

    // 3. Judge — essential, so a missing backend stops here.
    let Some(judge_backend) = tj_core::llm::backend_from_env(backend)? else {
        out.skipped_no_backend = true;
        return Ok(out);
    };
    let (j, judge_usage) = tj_core::finalize::judge(&title, &lines, judge_backend.as_ref())?;
    out.spent.add(judge_usage);
    out.done = j.done;
    out.reason = j.reason.clone();

    let mut writer = tj_core::storage::JsonlWriter::open(events_path)?;

    // 4. Retitle only when the model flagged the current title as junk.
    if j.should_apply_title(&title) {
        let mut ev = tj_core::event::Event::new(
            task_id,
            tj_core::event::EventType::Rename,
            tj_core::event::Author::Agent,
            tj_core::event::Source::Cli,
            j.title.clone(),
        );
        ev.meta = serde_json::json!({ "title": j.title, "was": title });
        writer.append(&ev)?;
        out.retitled = Some((title.clone(), j.title.clone()));
    }

    // 5. Close only when the events clearly show the task is done. The
    // outcome rides on the event meta so it survives a rebuild_state replay.
    if j.done {
        let reason = if j.reason.is_empty() {
            "(finalized)".to_string()
        } else {
            j.reason.clone()
        };
        let mut ev = tj_core::event::Event::new(
            task_id,
            tj_core::event::EventType::Close,
            tj_core::event::Author::Agent,
            tj_core::event::Source::Cli,
            reason,
        );
        ev.meta = serde_json::json!({
            "outcome": j.outcome,
            "outcome_tag": j.normalized_tag(),
            "reason": j.reason,
        });
        writer.append(&ev)?;
        out.closed = true;
    }

    writer.flush_durable()?;
    tj_core::db::ingest_new_events(conn, events_path, project_hash)?;

    // 6. Estimate the memory compression this finalize represents.
    out.saved = compute_savings(conn, events_path, ctx.project_dir, task_id);
    Ok(out)
}

/// A one-line nudge shown when a cost-reporting backend (claude -p) was used:
/// the same Haiku via a direct API skips Claude Code's harness overhead. Only
/// claude -p reports a non-zero `cost_usd`, so this fires for it alone.
fn backend_cost_tip(cost: Option<f64>) -> Option<String> {
    match cost {
        Some(c) if c > 0.0 => Some(
            "tip: that cost is claude -p's Claude Code overhead (notional under a \
subscription). For ~50× cheaper per task, use --backend anthropic (direct Haiku API, \
needs ANTHROPIC_API_KEY) — or --backend ollama for free, local."
                .to_string(),
        ),
        _ => None,
    }
}

/// Human-readable one-liner for a finalize result.
fn print_finalize_outcome(task_id: &str, out: &FinalizeOutcome) {
    if out.skipped_no_backend {
        println!(
            "complete {task_id}: no usable LLM backend. Default `claude-p` needs Claude Code on \
PATH; or pick one via --backend / TJ_BACKEND: anthropic, openai, ollama (free, local)."
        );
        return;
    }
    let mut parts = Vec::new();
    if out.enriched > 0 {
        parts.push(format!("{} event(s) backfilled", out.enriched));
    }
    if let Some((old, new)) = &out.retitled {
        parts.push(format!("retitled {old:?} → {new:?}"));
    }
    if out.closed {
        parts.push("closed".to_string());
    } else {
        let why = if out.reason.is_empty() {
            "not clearly done".to_string()
        } else {
            out.reason.clone()
        };
        parts.push(format!("left open ({why})"));
    }
    if parts.is_empty() {
        parts.push("no change".to_string());
    }
    println!(
        "complete {task_id}: {}{}",
        parts.join("; "),
        stats_suffix(&out.spent, &out.saved)
    );
}

/// The Claude Code session dir that `--enrich` reads. With `--enrich` and no
/// such dir, say why nothing gets enriched instead of staying silent.
fn complete_project_dir(
    cwd: &std::path::Path,
    enrich: bool,
) -> anyhow::Result<Option<std::path::PathBuf>> {
    let dir = tj_core::session::discovery::find_project_dir(cwd)?;
    if enrich && dir.is_none() {
        eprintln!(
            "complete: no Claude Code session directory for this project — --enrich reads \
Claude Code transcripts only (Codex sessions are not read yet)"
        );
    }

    Ok(dir)
}

/// `complete <id>` — finalize a single task.
fn run_complete_single(
    task_id: &str,
    dry_run: bool,
    enrich: bool,
    backend: Option<&str>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    }
    if !tj_core::db::task_exists(&conn, task_id)? {
        anyhow::bail!("task not found: {task_id}");
    }
    let project_dir = complete_project_dir(&cwd, enrich)?;
    let ctx = ProjectCtx {
        conn: &conn,
        events_path: &events_path,
        project_hash: &project_hash,
        project_dir: project_dir.as_deref(),
    };
    let out = finalize_one_task(&ctx, task_id, enrich, dry_run, backend)?;
    print_finalize_outcome(task_id, &out);
    if let Some(tip) = backend_cost_tip(out.spent.cost_usd) {
        eprintln!("{tip}");
    }
    Ok(())
}

/// `complete` (no id) — finalize every open task, with a reviewable list the
/// user can prune before confirming. Refuses without a TTY unless `--yes`.
fn run_complete_batch(
    dry_run: bool,
    enrich: bool,
    yes: bool,
    backend: Option<&str>,
) -> anyhow::Result<()> {
    use std::io::IsTerminal;

    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    }

    let mut stmt = conn.prepare(
        "SELECT task_id, title FROM tasks WHERE status='open' ORDER BY last_event_at DESC",
    )?;
    let open: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    if open.is_empty() {
        println!("complete: no open tasks in this project.");
        return Ok(());
    }

    let project_dir = complete_project_dir(&cwd, enrich)?;

    // Show the numbered list with event/session counts so the user can judge
    // what to keep before anything is touched.
    println!("Open tasks ({}):", open.len());
    for (i, (id, title)) in open.iter().enumerate() {
        let events: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events_index WHERE task_id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )?;
        let sessions = match project_dir.as_deref() {
            Some(dir) => task_sessions(&events_path, dir, id)?.len(),
            None => 0,
        };
        println!("  {}. {id}  [{events} ev, {sessions} sess]  {title}", i + 1);
    }

    let ctx = ProjectCtx {
        conn: &conn,
        events_path: &events_path,
        project_hash: &project_hash,
        project_dir: project_dir.as_deref(),
    };

    // Dry-run: report planned scope per task and stop — no prompts, no writes.
    if dry_run {
        println!();
        for (id, _) in &open {
            finalize_one_task(&ctx, id, enrich, true, backend)?;
        }
        return Ok(());
    }

    let interactive = std::io::stdin().is_terminal();
    if !interactive && !yes {
        anyhow::bail!(
            "batch complete needs an interactive terminal to confirm; pass --yes to run it non-interactively"
        );
    }

    // Let the user exclude tasks, then confirm.
    let mut excluded: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if interactive && !yes {
        println!("\nNumbers to EXCLUDE (space/comma separated), or Enter to finalize all:");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        for tok in buf.split(|c: char| c.is_whitespace() || c == ',') {
            if let Ok(n) = tok.trim().parse::<usize>() {
                if n >= 1 && n <= open.len() {
                    excluded.insert(n - 1);
                }
            }
        }
    }
    let targets: Vec<&(String, String)> = open
        .iter()
        .enumerate()
        .filter(|(i, _)| !excluded.contains(i))
        .map(|(_, r)| r)
        .collect();
    if targets.is_empty() {
        println!("complete: nothing selected.");
        return Ok(());
    }
    if interactive && !yes {
        println!(
            "\nWill finalize {} task(s){}. Proceed? [y/N]",
            targets.len(),
            if enrich {
                " (with --enrich: slow, reads sessions)"
            } else {
                ""
            }
        );
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        if !matches!(buf.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("aborted.");
            return Ok(());
        }
    }

    let mut left_open: Vec<(String, String)> = Vec::new();
    let mut total_spent = tj_core::llm::LlmUsage::default();
    let mut total_saved = Savings::default();
    let mut done_count = 0usize;
    for (id, _) in &targets {
        let out = finalize_one_task(&ctx, id, enrich, false, backend)?;
        print_finalize_outcome(id, &out);
        if out.skipped_no_backend {
            println!("complete: stopping batch — no LLM backend available.");
            return Ok(());
        }
        total_spent.add(out.spent);
        if let Some(s) = out.saved {
            total_saved.raw_tokens += s.raw_tokens;
            total_saved.pack_tokens += s.pack_tokens;
        }
        done_count += 1;
        if !out.closed {
            left_open.push((id.clone(), out.reason.clone()));
        }
    }

    let totals = stats_suffix(&total_spent, &Some(total_saved));
    if !totals.is_empty() {
        println!(
            "\nTotals across {done_count} task(s): {}",
            totals.trim_start_matches(" | ")
        );
    }
    if let Some(tip) = backend_cost_tip(total_spent.cost_usd) {
        eprintln!("{tip}");
    }

    if !left_open.is_empty() {
        println!("\nLeft open ({}):", left_open.len());
        for (id, reason) in &left_open {
            let why = if reason.is_empty() {
                "not clearly done"
            } else {
                reason
            };
            println!("  {id} — {why}");
        }
    }
    Ok(())
}

/// Manual consolidation: read this project's recurring decisions/constraints,
/// distil them into durable facts via one LLM call through the chosen backend,
/// and store the facts as events in a per-project conventions task. Skips
/// cleanly (no spend) when no backend is available.
fn run_consolidate(
    max_facts: usize,
    backend: Option<&str>,
    write_claude_md: bool,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;
    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    if !events_path.exists() {
        anyhow::bail!("no events file at {events_path:?}");
    }
    let conn = tj_core::db::open(&state_path)?;
    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;

    let sources = tj_core::db::high_signal_events(&conn, 200)?;
    if sources.is_empty() {
        println!("nothing to consolidate — no decisions/constraints/rejections recorded yet");
        return Ok(());
    }
    let texts: Vec<String> = sources.iter().map(|(_, t)| t.clone()).collect();
    let source_ids: Vec<String> = sources.iter().map(|(id, _)| id.clone()).collect();

    let (backend_used, facts) = match tj_core::consolidate::summarize(&texts, max_facts, backend)? {
        Some(x) => x,
        None => {
            println!(
                "skipped: no usable LLM backend. Default is `claude-p` (install Claude \
Code so `claude` is on PATH — uses your subscription, no API key). Or pick \
another via --backend / TJ_BACKEND: anthropic (ANTHROPIC_API_KEY), openai \
(OPENAI_API_KEY), ollama (free, local)."
            );
            return Ok(());
        }
    };
    eprintln!(
        "consolidating {} high-signal event(s) via {backend_used} …",
        texts.len()
    );
    if facts.is_empty() {
        println!("no durable facts found");
        return Ok(());
    }

    // Reuse the per-project conventions task, or create it.
    let task_id = match tj_core::db::find_task_by_title(&conn, CONSOLIDATE_TASK_TITLE)? {
        Some(id) => id,
        None => {
            let id = tj_core::new_task_id();
            let mut ev = tj_core::event::Event::new(
                id.clone(),
                tj_core::event::EventType::Open,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                CONSOLIDATE_TASK_TITLE.to_string(),
            );
            ev.meta = serde_json::json!({ "title": CONSOLIDATE_TASK_TITLE });
            let mut w = tj_core::storage::JsonlWriter::open(&events_path)?;
            w.append(&ev)?;
            w.flush_durable()?;
            tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            id
        }
    };

    // De-dup against facts already stored in the conventions task.
    let existing: std::collections::HashSet<String> =
        tj_core::db::task_event_texts(&conn, &task_id)?
            .into_iter()
            .collect();

    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    let mut written = 0usize;
    for f in &facts {
        if existing.contains(&f.text) {
            continue;
        }
        let mut ev = tj_core::event::Event::new(
            task_id.clone(),
            tj_core::event::EventType::Finding,
            tj_core::event::Author::Agent,
            tj_core::event::Source::Cli,
            f.text.clone(),
        );
        ev.meta = serde_json::json!({
            "memory_tier": f.tier,
            "consolidated": true,
            "derived_from": source_ids,
        });
        writer.append(&ev)?;
        written += 1;
    }
    writer.flush_durable()?;

    // Index the new facts and push them to the global recall index.
    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
    let embedder = tj_core::embed::default_embedder();
    let now = chrono::Utc::now().to_rfc3339();
    tj_core::db::embed_pending(&conn, &project_hash, embedder.as_ref(), &now, 512)?;
    sync_global_memory(&conn, &project_hash);

    println!(
        "consolidated {written} new fact(s) into task {task_id} (\"{CONSOLIDATE_TASK_TITLE}\")"
    );

    // Promote to always-on: regenerate the managed conventions block in
    // ./CLAUDE.md from this run's full set of facts.
    if write_claude_md {
        let path = cwd.join("CLAUDE.md");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let updated = tj_core::consolidate::upsert_conventions_block(&existing, &facts);
        std::fs::write(&path, updated)?;
        println!(
            "wrote {} convention(s) into the managed block in {}",
            facts.len(),
            path.display()
        );
    }
    Ok(())
}

fn auto_open_task_from_prompt(
    events_path: &std::path::Path,
    project_hash: &str,
    conn: &rusqlite::Connection,
    prompt: &str,
    session_id: Option<&str>,
) -> anyhow::Result<Option<tj_core::classifier::TaskContext>> {
    // Title/goal must read like a human wrote them on purpose. When the
    // prompt is only machine noise — session-start scrollback
    // (`685] INFO: Mapped {…}`), a shell prompt, the journal's own resume
    // banner — `humanize_title` returns None and we decline to auto-open.
    // Better no task than a task labelled with a log line that then leaks
    // into the task list and the Claude Code session name.
    let Some(title) = tj_core::title::humanize_title(prompt) else {
        return Ok(None);
    };
    let goal: String = tj_core::title::humanize_goal(prompt, 200).unwrap_or_else(|| title.clone());

    let task_id = tj_core::new_task_id();
    let mut event = tj_core::event::Event::new(
        task_id.clone(),
        tj_core::event::EventType::Open,
        tj_core::event::Author::User,
        tj_core::event::Source::Cli,
        title.clone(),
    );
    event.meta = serde_json::json!({ "title": title, "auto_opened": true });
    tj_core::session_id::stamp_session_id(&mut event.meta, session_id);

    let mut writer = tj_core::storage::JsonlWriter::open(events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;

    tj_core::db::ingest_new_events(conn, events_path, project_hash)?;
    if !goal.is_empty() {
        tj_core::db::set_task_goal(conn, &task_id, &goal)?;
    }

    // v0.5.0 Phase C / v0.6.0: score-based linking. Pull artifacts
    // from the prompt — ticket ids, commit hashes, file paths — then
    // ask the journal which prior tasks share enough signal to be a
    // probable continuation. Anything with score > 0 gets linked via
    // External; the strongest closed match also triggers a stderr
    // hint so the user can reopen instead of accumulating duplicates.
    let prompt_arts = tj_core::artifacts::extract(prompt);
    if !prompt_arts.is_empty() {
        let related = tj_core::db::find_related_tasks(conn, &prompt_arts)?;
        let mut warned = false;
        for r in related.iter().take(5) {
            if r.task_id == task_id {
                continue;
            }
            let _ =
                tj_core::db::add_task_external(conn, &task_id, &format!("linked:{}", r.task_id));
            if !warned && r.status == "closed" {
                eprintln!(
                    "task-journal: this prompt looks like a continuation of closed task {} \
                     (score {:.1}) — run `task-journal reopen {}` if it is.",
                    r.task_id, r.score, r.task_id
                );
                warned = true;
            }
        }
    }

    Ok(Some(tj_core::classifier::TaskContext {
        task_id,
        title,
        last_events: vec![],
        constraints: vec![],
    }))
}

fn persist_pending(
    events_path: &std::path::Path,
    project_hash: &str,
    kind: &str,
    text: &str,
    err: &str,
) -> anyhow::Result<()> {
    let pending_dir = events_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pending");
    std::fs::create_dir_all(&pending_dir)?;
    let id = ulid::Ulid::new().to_string();
    // `kind` lets `pending retry` classify the chunk the way the hook would.
    let payload = serde_json::json!({"kind": kind, "text": text, "error": err, "queued_at": chrono::Utc::now().to_rfc3339()});
    std::fs::write(
        pending_dir.join(format!("{project_hash}.{id}.json")),
        serde_json::to_string_pretty(&payload)?,
    )?;
    Ok(())
}

/// v0.6.2: queue an ingest event for the detached classify-worker. The
/// hook returns immediately after writing this entry so it does not
/// block Claude Code's hook timeout (was 5-30s, now <100ms). Schema "v2"
/// distinguishes async-ingest entries from legacy v1 (text+error) ones
/// the `pending retry` path knows how to handle.
fn persist_pending_v2(
    events_path: &std::path::Path,
    kind: &str,
    text: &str,
    project_hash: &str,
    backend: &str,
    session_id: Option<&str>,
) -> anyhow::Result<std::path::PathBuf> {
    let pending_dir = events_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pending");
    std::fs::create_dir_all(&pending_dir)?;
    let id = ulid::Ulid::new().to_string();
    let mut payload = serde_json::json!({
        "schema": "v2",
        "kind": kind,
        "text": text,
        "project_hash": project_hash,
        "events_path": events_path.to_string_lossy(),
        "backend": backend,
        "queued_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(sid) = session_id {
        payload["session_id"] = serde_json::Value::String(sid.to_string());
    }
    // `pending/` is shared by every project: the hash prefix tells each
    // project's worker which entries are its own.
    let path = pending_dir.join(format!("{project_hash}.{id}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&payload)?)?;
    Ok(path)
}

/// Transcript catch-up: parse the JSONL session log and enqueue user
/// and assistant text entries newer than `last_event_ts` as pending v2
/// chunks. The classify-worker picks them up afterwards. Returns the
/// number of chunks queued. Errors are absorbed — best-effort, never
/// fatal. Used by both PreCompact (before compaction) and Stop (end
/// of session) hooks to recover events the synchronous PostToolUse
/// hook didn't see (internal classifier calls, MCP responses with
/// thinking-only assistant turns, or the final assistant message
/// before a session ends).
///
/// `assistant_chunk_kind` tags assistant-side entries so the source
/// hook is visible in the pending queue (e.g. "PreCompactChunk"
/// vs "StopChunk"). User entries always tag as "UserPromptSubmit"
/// to trigger `process_pending_entry`'s auto-open behavior.
fn enqueue_transcript_chunks_since_last_event(
    transcript_path: &std::path::Path,
    events_path: &std::path::Path,
    project_hash: &str,
    backend: &str,
    last_event_ts: Option<&str>,
    assistant_chunk_kind: &str,
    session_id: Option<&str>,
) -> anyhow::Result<usize> {
    use tj_core::session::parser::{
        extract_assistant_texts, extract_user_text, parse_session, SessionEntry,
    };
    let parsed = match parse_session(transcript_path) {
        Ok(p) => p,
        Err(_) => return Ok(0),
    };
    let mut count = 0usize;
    for entry in &parsed.entries {
        let (ts, text, kind) = match entry {
            SessionEntry::User(u) => {
                let text = extract_user_text(u).unwrap_or_default();
                (u.timestamp.clone(), text, "UserPromptSubmit")
            }
            SessionEntry::Assistant(a) => {
                let texts = extract_assistant_texts(a);
                if texts.is_empty() {
                    continue;
                }
                (a.timestamp.clone(), texts.join("\n"), assistant_chunk_kind)
            }
            _ => continue,
        };
        if text.trim().len() < 20 {
            continue;
        }
        if let Some(last) = last_event_ts {
            if ts.as_str() <= last {
                continue;
            }
        }
        persist_pending_v2(events_path, kind, &text, project_hash, backend, session_id)?;
        count += 1;
    }
    Ok(count)
}

/// Spawn the classify-worker as a detached child. We deliberately drop
/// the `Child` handle so the parent (the actual Claude Code hook child)
/// can exit without waiting; the worker re-parents to init on Linux.
/// stdin/stdout/stderr are nulled so the worker doesn't keep the hook's
/// pipes open. TJ_CLASSIFIER_BUMP marks the spawn for telemetry; clear
/// TJ_IN_CLASSIFIER because the worker NEEDS to call the classifier.
fn spawn_classify_worker(backend: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locate current task-journal exe")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("classify-worker")
        .arg("--backend")
        .arg(backend)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("TJ_CLASSIFIER_BUMP", "1")
        .env_remove(tj_core::classifier::agent_sdk::IN_CLASSIFIER_ENV);
    let _child = cmd.spawn().context("spawn classify-worker")?;
    // Drop child intentionally — Linux init reaps when parent exits.
    Ok(())
}

/// Characters of a PostToolUse chunk (tool input + response) that get queued.
const POST_TOOL_USE_TEXT_MAX: usize = 2000;

/// How long an empty (pid not yet written) worker lockfile counts as held.
const LOCK_PID_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// File-lock guard for the classify-worker. Holds the lockfile until
/// dropped; ensures cleanup on panic. One worker per project_hash.
struct WorkerLock {
    path: std::path::PathBuf,
}

impl WorkerLock {
    /// Try to acquire the lock. Returns Ok(Some(_)) on success, Ok(None)
    /// if another live worker holds it, Err on filesystem failure.
    fn try_acquire(project_hash: &str) -> anyhow::Result<Option<Self>> {
        let dir = tj_core::paths::state_dir()?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("classifier-{project_hash}.lock"));

        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Some(Self { path }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Inspect existing lockfile. If PID is alive → another
                    // worker is running; back off. If dead → remove stale
                    // file and retry.
                    let body = std::fs::read_to_string(&path).unwrap_or_default();
                    match body.trim().parse::<u32>() {
                        Ok(pid) if pid_is_alive(pid) => return Ok(None),
                        Ok(_) => {}
                        Err(_) => {
                            // No PID yet: the holder may sit between
                            // `create_new` and the pid write. Only a lock
                            // that stayed empty past the grace is stale.
                            let fresh = std::fs::metadata(&path)
                                .and_then(|m| m.modified())
                                .map(|t| t.elapsed().unwrap_or_default() < LOCK_PID_GRACE)
                                .unwrap_or(false);
                            if fresh {
                                return Ok(None);
                            }
                        }
                    }
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // kill(pid, 0) probes existence without sending a signal.
    // SAFETY: libc::kill is a thin syscall wrapper, no aliasing concerns.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }

    // EPERM: the process exists but belongs to another user.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    // Conservative on non-Unix: assume alive so we don't double-spawn.
    // The lockfile gets cleaned up on Drop in the normal exit path.
    true
}

/// classify-worker: drain pending v2 entries by running the real
/// classifier. v1 entries (legacy text+error shape) are left for
/// `pending retry`. Holds a project-scoped file lock so only one
/// worker per project runs at a time.
fn run_classify_worker(backend: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_hash = tj_core::project_hash::from_path(&cwd)?;

    let lock = match WorkerLock::try_acquire(&project_hash)? {
        Some(l) => l,
        None => return Ok(()), // another worker is running
    };

    let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let pending = events_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("events_dir has no grandparent"))?
        .join("pending");
    if !pending.exists() {
        drop(lock);
        return Ok(());
    }

    // Snapshot entries up front so concurrent re-queues don't loop us.
    // Only this project's entries: the lock is per project, so a worker
    // of another project may be draining the same directory right now.
    let entries = project_pending_entries(&pending, &project_hash)?;

    for path in entries {
        if let Err(err) = process_pending_entry(&path, &events_path, &project_hash, backend) {
            // Non-fatal: leave the file in place; pending-retry / next
            // worker invocation can re-attempt. Avoid writing to stderr
            // since stderr is nulled — but in tests stderr is captured.
            eprintln!("classify-worker: {} failed: {err:#}", path.display());
        }
    }

    drop(lock);
    Ok(())
}

/// Process one pending entry. Routes by schema:
/// - "v2" → real-classifier path (auto_open + classify + persist event)
/// - anything else (legacy "v1" with text/error) → leave for `pending retry`
fn process_pending_entry(
    path: &std::path::Path,
    events_path: &std::path::Path,
    project_hash: &str,
    backend: &str,
) -> anyhow::Result<()> {
    let body = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let schema = v.get("schema").and_then(|x| x.as_str()).unwrap_or("v1");
    if schema != "v2" {
        return Ok(()); // legacy entry, handled by `pending retry`
    }
    // A legacy un-prefixed entry is ours only when it names this project;
    // one without `project_hash` is left for `pending retry`.
    if v.get("project_hash").and_then(|x| x.as_str()) != Some(project_hash) {
        return Ok(());
    }

    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .unwrap_or("Stop")
        .to_string();
    let text = v
        .get("text")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    // Inherit the session id queued on the v2 chunk (additive; absent → None).
    let chunk_session_id = tj_core::session_id::session_id_from_payload(&v);

    let classifier = build_classifier(backend)?;
    let outcome = classify_chunk(
        classifier.as_ref(),
        events_path,
        project_hash,
        &kind,
        &text,
        chunk_session_id.as_deref(),
    )?;
    if let ChunkOutcome::Unplaced(err) = outcome {
        // Persist as legacy v1 pending entry so `pending retry`
        // surfaces it; remove the v2 source.
        persist_pending(events_path, project_hash, &kind, &text, &err)?;
    }

    std::fs::remove_file(path)?;
    Ok(())
}

/// What became of one classified chunk.
enum ChunkOutcome {
    /// An event was written to the journal.
    Recorded,
    /// Nothing worth recording: no task to attach to, machine noise, no
    /// task guess, or a session-end "close".
    Dropped,
    /// The classifier failed or guessed a missing / closed task; the reason
    /// goes back into `pending/` with the chunk.
    Unplaced(String),
}

/// The classifier behind a `--backend` name.
fn build_classifier(backend: &str) -> anyhow::Result<Box<dyn tj_core::classifier::Classifier>> {
    use tj_core::classifier::Classifier;
    let classifier: Box<dyn Classifier> = match backend {
        "hybrid" | "" => Box::new(tj_core::classifier::hybrid::HybridClassifier::from_env()),
        "api" => Box::new(tj_core::classifier::http::AnthropicClassifier::from_env()?),
        "agent-sdk" => Box::new(
            tj_core::classifier::agent_sdk::ClaudeCliClassifier::from_env().ok_or_else(|| {
                anyhow::anyhow!(
                    "agent-sdk backend selected but no `claude` binary on PATH — \
                     install Claude Code (https://claude.com/claude-code) or pick another --backend"
                )
            })?,
        ),
        "heuristic" => {
            use tj_core::classifier::heuristic::try_heuristic;
            use tj_core::classifier::{ClassifyInput, ClassifyOutput};
            struct HeuristicOnly;
            impl Classifier for HeuristicOnly {
                fn classify(&self, input: &ClassifyInput) -> anyhow::Result<ClassifyOutput> {
                    try_heuristic(input).ok_or_else(|| {
                        anyhow::anyhow!(
                            "heuristic uncertain (heuristic-only mode has no LLM fallback)"
                        )
                    })
                }
            }
            Box::new(HeuristicOnly)
        }
        other => anyhow::bail!(
            "unknown backend: {other} (expected `hybrid`, `agent-sdk`, `api`, or `heuristic`)"
        ),
    };
    Ok(classifier)
}

/// Classify one chunk against the project's open tasks and record the
/// event. Shared by classify-worker and `pending retry`, so both auto-open,
/// check attribution, stamp the session and write telemetry the same way.
fn classify_chunk(
    classifier: &dyn tj_core::classifier::Classifier,
    events_path: &std::path::Path,
    project_hash: &str,
    kind: &str,
    text: &str,
    session_id: Option<&str>,
) -> anyhow::Result<ChunkOutcome> {
    // Mirror the synchronous flow that used to live in IngestHook —
    // see commit history of v0.6.1 for the original. Auto-open, run
    // classifier, apply integrity safeguards, persist event, telemetry.
    let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path)?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, events_path, project_hash)?;
    }

    let mut recent = recent_task_contexts(&conn, 5)?;
    if recent.is_empty() {
        let auto_open_disabled = std::env::var("TJ_AUTO_OPEN_TASKS")
            .ok()
            .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        if auto_open_disabled || !kind.contains("UserPrompt") {
            // Nothing to do — drop the entry silently.
            return Ok(ChunkOutcome::Dropped);
        }
        let Some(new_task) =
            auto_open_task_from_prompt(events_path, project_hash, &conn, text, session_id)?
        else {
            // Prompt was only machine noise — drop the entry silently.
            return Ok(ChunkOutcome::Dropped);
        };
        recent.push(new_task);
    }

    let author_hint = if kind.contains("UserPrompt") {
        "user"
    } else {
        "assistant"
    };

    let input = tj_core::classifier::ClassifyInput {
        text: text.to_string(),
        author_hint: author_hint.into(),
        recent_tasks: recent,
        tool_output: kind == "PostToolUse",
    };
    let out = match classifier.classify(&input) {
        Ok(o) => o,
        Err(e) => return Ok(ChunkOutcome::Unplaced(e.to_string())),
    };

    let Some(tid) = out.task_id_guess else {
        return Ok(ChunkOutcome::Dropped);
    };

    use tj_core::event::EventType;
    if matches!(out.event_type, EventType::Close) && kind == "Stop" {
        return Ok(ChunkOutcome::Dropped);
    }
    match tj_core::db::task_status(&conn, &tid)? {
        None => {
            return Ok(ChunkOutcome::Unplaced(format!(
                "task_id_guess `{tid}` not found"
            )))
        }
        Some(s) if s == "closed" => {
            return Ok(ChunkOutcome::Unplaced(format!(
                "task_id_guess `{tid}` is closed"
            )))
        }
        _ => {}
    }

    let confidence = out.confidence;
    let evidence_strength = out.evidence_strength;
    let etype = out.event_type;
    let event_text = out.suggested_text;

    let mut event = tj_core::event::Event::new(
        &tid,
        etype,
        tj_core::event::Author::Classifier,
        tj_core::event::Source::Hook,
        event_text,
    );
    event.confidence = Some(confidence);
    event.status = tj_core::classifier::decide_status(confidence);
    event.evidence_strength = evidence_strength;
    tj_core::session_id::stamp_session_id(&mut event.meta, session_id);

    let mut writer = tj_core::storage::JsonlWriter::open(events_path)?;
    writer.append(&event)?;
    writer.flush_durable()?;

    let metrics_path = tj_core::paths::metrics_dir()?.join(format!("{project_hash}.jsonl"));
    let etype_str = serde_json::to_value(etype)?
        .as_str()
        .unwrap_or("?")
        .to_string();
    let status_str = serde_json::to_value(event.status)?
        .as_str()
        .unwrap_or("?")
        .to_string();
    let _ = tj_core::classifier::telemetry::append(
        &metrics_path,
        &tj_core::classifier::telemetry::TelemetryRecord {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            project_hash: project_hash.to_string(),
            task_id_guess: Some(tid.clone()),
            event_type: etype_str,
            confidence,
            status: status_str,
            error: None,
        },
    );

    Ok(ChunkOutcome::Recorded)
}

/// Mock-only drain: with the mock flags, turn this project's legacy (v1)
/// pending entries into events. Without them it does nothing — a v1 entry is
/// a classifier failure waiting for `pending retry`, and v2 entries belong to
/// classify-worker. An entry is removed only after its event is written.
fn drain_pending(
    events_path: &std::path::Path,
    project_hash: &str,
    mock_etype: Option<&str>,
    mock_tid: Option<&str>,
    mock_conf: Option<f64>,
) -> anyhow::Result<()> {
    let (Some(t), Some(tid)) = (mock_etype, mock_tid) else {
        return Ok(());
    };
    let pending_dir = events_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pending");

    for path in project_pending_entries(&pending_dir, project_hash)? {
        let body = std::fs::read_to_string(&path)?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        // v0.6.2: skip v2 entries — those are owned by classify-worker.
        // Removing them here would silently drop async-queued events.
        if v.get("schema").and_then(|x| x.as_str()) == Some("v2") {
            continue;
        }
        let text = v
            .get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if text.is_empty() {
            continue;
        }

        let mut event = tj_core::event::Event::new(
            tid,
            parse_event_type(t)?,
            tj_core::event::Author::Classifier,
            tj_core::event::Source::Hook,
            text,
        );
        event.confidence = mock_conf;
        event.status = tj_core::classifier::decide_status(mock_conf.unwrap_or(1.0));
        let mut writer = tj_core::storage::JsonlWriter::open(events_path)?;
        writer.append(&event)?;
        writer.flush_durable()?;

        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Read a Claude Code hook payload from stdin and project it down to
/// the (kind, text) pair the rest of `ingest-hook` operates on.
///
/// Claude Code passes hook input as a JSON object on stdin. The fields
/// we care about (per the public hooks spec):
///
/// - common: `hook_event_name`
/// - UserPromptSubmit: `prompt`
/// - PreToolUse / PostToolUse: `tool_name`, `tool_input`, `tool_response`
/// - Stop / SessionStart: nothing extra worth ingesting (SessionStart
///   takes a separate fast path further up)
///
/// If stdin is empty (someone runs the command interactively without
/// piping), we silently return ("Stop", "") so the hook becomes a no-op
/// instead of erroring — matches the `|| true` safety net in the
/// installed hook command.
/// Build a PostToolUse `updatedMCPToolOutput` envelope when an MCP tool call
/// echoes a prior rejection/decision (claude-memory-7km). Returns None
/// (pass through, emit nothing) for non-MCP tools, no hits, or any error.
/// Never panics, never mutates the journal.
///
/// Dedup vs claude-memory-60m: this fires ONLY for `mcp__` tools; 60m's
/// `additionalContext` path skips those. The two are mutually exclusive by
/// tool type so a single recall is never double-surfaced.
fn push_recall_envelope(
    payload: &serde_json::Value,
    events_path: &std::path::Path,
    project_hash: &str,
    session_id: Option<&str>,
) -> Option<serde_json::Value> {
    // MCP-only gate: Claude Code prefixes MCP tools `mcp__<server>__<tool>`.
    let tool_name = payload.get("tool_name").and_then(|v| v.as_str())?;
    if !tool_name.starts_with("mcp__") {
        return None;
    }
    if !events_path.exists() {
        return None;
    }
    let query_text = payload
        .get("tool_input")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if query_text.trim().is_empty() {
        return None;
    }
    let original = payload
        .get("tool_response")
        .map(render_tool_response)
        .unwrap_or_default();

    let state_path = tj_core::paths::state_dir()
        .ok()?
        .join(format!("{project_hash}.sqlite"));
    let conn = tj_core::db::open(&state_path).ok()?;
    let _ = tj_core::db::ingest_new_events(&conn, events_path, project_hash);
    // Reuse 60m's recall engine + threshold — no recall logic lives here.
    let hits =
        tj_core::recall::relevant_recall(&conn, &query_text, tj_core::recall::DEFAULT_MAX_HITS)
            .ok()?;
    let hits = fresh_recall_hits(hits, session_id, events_path, project_hash);
    if hits.is_empty() {
        return None;
    }
    let updated = format!("{}\n\n{}", render_recall_banner(&hits), original);
    Some(serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedMCPToolOutput": updated,
        }
    }))
}

/// Most `<session> <event_id>` lines the shown-recall log keeps.
const RECALL_SHOWN_CAP: usize = 2000;

/// The recall hits still worth pushing to `session_id`: each one at most once
/// per session (remembered in `<state_dir>/<project>.recall-shown`, newest
/// [`RECALL_SHOWN_CAP`] lines), and never an event the session wrote on its
/// current task — the agent just wrote it. Without a session id every hit
/// passes, as before. Best-effort: an unreadable log never hides a hit.
fn fresh_recall_hits(
    hits: Vec<tj_core::recall::RecallHit>,
    session_id: Option<&str>,
    events_path: &std::path::Path,
    project_hash: &str,
) -> Vec<tj_core::recall::RecallHit> {
    let Some(sid) = session_id else {
        return hits;
    };
    if hits.is_empty() {
        return hits;
    }
    let Ok(log) =
        tj_core::paths::state_dir().map(|d| d.join(format!("{project_hash}.recall-shown")))
    else {
        return hits;
    };

    let own = session_task_event_ids(events_path, sid);
    let body = std::fs::read_to_string(&log).unwrap_or_default();
    let shown: std::collections::HashSet<&str> = body.lines().collect();
    let fresh: Vec<_> = hits
        .into_iter()
        .filter(|h| !own.contains(&h.event_id))
        .filter(|h| !shown.contains(format!("{sid} {}", h.event_id).as_str()))
        .collect();
    if fresh.is_empty() {
        return fresh;
    }

    // ponytail: read-modify-write without a lock; two parallel hooks can
    // re-show a hit once. Add a file lock if that ever shows up in practice.
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
    lines.extend(fresh.iter().map(|h| format!("{sid} {}", h.event_id)));
    let keep = &lines[lines.len().saturating_sub(RECALL_SHOWN_CAP)..];
    let _ = std::fs::write(&log, keep.join("\n") + "\n");

    fresh
}

/// Ids of the events session `sid` wrote on its current task — the task of
/// its latest event — read from the journal by `meta.session_id`.
fn session_task_event_ids(
    events_path: &std::path::Path,
    sid: &str,
) -> std::collections::HashSet<String> {
    let body = std::fs::read_to_string(events_path).unwrap_or_default();
    let mine: Vec<(String, String)> = body
        .lines()
        .filter(|l| l.contains(sid))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["meta"]["session_id"].as_str() == Some(sid))
        .filter_map(|e| {
            Some((
                e["event_id"].as_str()?.into(),
                e["task_id"].as_str()?.into(),
            ))
        })
        .collect();

    let Some((_, current)) = mine.last() else {
        return Default::default();
    };
    mine.iter()
        .filter(|(_, task)| task == current)
        .map(|(id, _)| id.clone())
        .collect()
}

/// One ⚠ line per recall hit (mirrors the close-gate / SessionStart convention).
fn render_recall_banner(hits: &[tj_core::recall::RecallHit]) -> String {
    let mut s = String::from("\u{26a0} Task Journal recall — you may be repeating a prior path:");
    for h in hits {
        let verb = match h.event_type {
            tj_core::event::EventType::Rejection => "rejected",
            _ => "decided on",
        };
        s.push_str(&format!(
            "\n  \u{26a0} in task {} you previously {} this: {}",
            h.task_id, verb, h.text
        ));
    }
    s
}

/// Collapse a `tool_response` JSON value to the text Claude would have seen.
/// A bare string is used as-is; any other JSON is stringified (mirrors how
/// `parse_hook_stdin` stringifies `tool_response`).
fn render_tool_response(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn parse_hook_stdin() -> anyhow::Result<(String, String, serde_json::Value)> {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .context("read hook payload from stdin")?;
    let buf = buf.trim();
    if buf.is_empty() {
        return Ok(("Stop".into(), String::new(), serde_json::Value::Null));
    }
    let v: serde_json::Value =
        serde_json::from_str(buf).with_context(|| format!("parse hook payload JSON: {buf}"))?;

    let kind = v
        .get("hook_event_name")
        .and_then(|s| s.as_str())
        .unwrap_or("Stop")
        .to_string();

    let text = match kind.as_str() {
        "UserPromptSubmit" => v
            .get("prompt")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        "PreToolUse" | "PostToolUse" => {
            let tool = v
                .get("tool_name")
                .and_then(|s| s.as_str())
                .unwrap_or("tool");
            let input = v
                .get("tool_input")
                .map(|x| x.to_string())
                .unwrap_or_default();
            let response = v
                .get("tool_response")
                .map(|x| x.to_string())
                .unwrap_or_default();
            if response.is_empty() {
                format!("{tool}: {input}")
            } else {
                format!("{tool}: {input} → {response}")
            }
        }
        _ => String::new(),
    };

    Ok((kind, text, v))
}

fn parse_event_type(s: &str) -> anyhow::Result<tj_core::event::EventType> {
    use tj_core::event::EventType::*;
    Ok(match s {
        "open" => Open,
        "hypothesis" => Hypothesis,
        "finding" => Finding,
        "evidence" => Evidence,
        "decision" => Decision,
        "rejection" => Rejection,
        "constraint" => Constraint,
        "correction" => Correction,
        "reopen" => Reopen,
        "supersede" => Supersede,
        "close" => Close,
        "redirect" => Redirect,
        other => anyhow::bail!("unknown event type: {other}"),
    })
}

/// Flatten a parsed session transcript into role-tagged turns, in order.
fn flatten_transcript(parsed: &tj_core::session::parser::ParsedSession) -> String {
    use tj_core::session::parser::{extract_assistant_texts, extract_user_text, SessionEntry};
    let mut s = String::new();
    for entry in &parsed.entries {
        match entry {
            SessionEntry::User(u) => {
                if let Some(text) = extract_user_text(u) {
                    s.push_str("user: ");
                    s.push_str(&text);
                    s.push('\n');
                }
            }
            SessionEntry::Assistant(a) => {
                for text in extract_assistant_texts(a) {
                    s.push_str("assistant: ");
                    s.push_str(&text);
                    s.push('\n');
                }
            }
            _ => {}
        }
    }
    s
}

/// True when any of `events` ties this task to the session: precise match
/// on `meta.session_id`, or (for legacy events with no session_id) a
/// timestamp falling inside the session's `[first_ts, last_ts]` window.
fn task_matches_session(
    events: &[tj_core::event::Event],
    session_id: &str,
    first_ts: Option<&str>,
    last_ts: Option<&str>,
) -> bool {
    events.iter().any(|e| {
        // Precise: event tagged with this session.
        if e.meta.get("session_id").and_then(|v| v.as_str()) == Some(session_id) {
            return true;
        }
        // Legacy fallback: timestamp inside the session window.
        if e.meta.get("session_id").is_none() {
            if let (Some(f), Some(l)) = (first_ts, last_ts) {
                return e.timestamp.as_str() >= f && e.timestamp.as_str() <= l;
            }
        }
        false
    })
}

/// Read the project's events from `events_path`, grouped by `task_id`.
/// Read once per run and shared by every session's candidate lookup.
fn events_by_task(
    events_path: &std::path::Path,
) -> anyhow::Result<std::collections::BTreeMap<String, Vec<tj_core::event::Event>>> {
    use tj_core::event::Event;

    let mut by_task = std::collections::BTreeMap::new();
    if !events_path.exists() {
        return Ok(by_task);
    }
    let body = std::fs::read_to_string(events_path)?;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<Event>(line) {
            by_task.entry(e.task_id.clone()).or_default().push(e);
        }
    }
    Ok(by_task)
}

/// Candidate task contexts for the tasks whose events match this session
/// (precise session_id, or legacy time-window). Each context carries the
/// task title and up to the last ~20 event texts (dedup context for the
/// backend).
fn candidate_tasks_for_session(
    by_task: &std::collections::BTreeMap<String, Vec<tj_core::event::Event>>,
    session_id: &str,
    first_ts: Option<&str>,
    last_ts: Option<&str>,
) -> Vec<tj_core::dream::backend::BackfillTaskContext> {
    use tj_core::dream::backend::BackfillTaskContext;
    use tj_core::event::EventType;

    let mut out = Vec::new();
    for (task_id, events) in by_task {
        if !task_matches_session(events, session_id, first_ts, last_ts) {
            continue;
        }
        // Title from the Open event when present, else the first event's text.
        let title = events
            .iter()
            .find(|e| e.event_type == EventType::Open)
            .or_else(|| events.first())
            .map(|e| e.text.clone())
            .unwrap_or_default();
        let existing_events: Vec<String> = events
            .iter()
            .rev()
            .take(20)
            .rev()
            .map(|e| e.text.clone())
            .collect();
        out.push(BackfillTaskContext {
            task_id: task_id.clone(),
            title,
            existing_events,
        });
    }
    out
}

/// Per-session `(session_id, BackfillInput)` pairs fed to `run_dream`.
type DreamInputs = Vec<(String, tj_core::dream::backend::BackfillInput)>;

/// Assemble per-session `(session_id, BackfillInput)` from the in-scope
/// session transcripts and the project's existing events. Also returns the
/// ids of sessions skipped as unreadable.
fn build_dream_inputs(
    events_path: &std::path::Path,
    sessions: &[std::path::PathBuf],
    task_filter: Option<&str>,
) -> anyhow::Result<(DreamInputs, Vec<String>)> {
    use tj_core::dream::backend::BackfillInput;
    use tj_core::session::parser::parse_session;

    let by_task = events_by_task(events_path)?;
    let mut out = Vec::new();
    let mut unreadable = Vec::new();
    for path in sessions {
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // One unreadable transcript must not abort mining the rest.
        let parsed = match parse_session(path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "dream: skipping unreadable session {}: {e:#}",
                    path.display()
                );
                unreadable.push(session_id);
                continue;
            }
        };

        let candidates = candidate_tasks_for_session(
            &by_task,
            &session_id,
            parsed.first_timestamp.as_deref(),
            parsed.last_timestamp.as_deref(),
        );
        let tasks: Vec<_> = candidates
            .into_iter()
            .filter(|t| task_filter.is_none_or(|f| f == t.task_id))
            .collect();
        if tasks.is_empty() {
            continue;
        }

        let transcript = flatten_transcript(&parsed);
        out.push((session_id, BackfillInput { tasks, transcript }));
    }
    Ok((out, unreadable))
}

#[cfg(test)]
mod inline_tests {
    // Sits at the bottom of the file to satisfy
    // `clippy::items_after_test_module` — every other free fn must be
    // declared before this module begins.
    use super::*;

    #[test]
    fn recall_hits_json_serializes_fields() {
        let h = tj_core::memory::GlobalHit {
            event_id: "e1".into(),
            project_hash: "abc12345".into(),
            task_id: "tj-1".into(),
            event_type: "rejection".into(),
            tier: "high".into(),
            text: "ruled out the shared-table approach".into(),
            score: 2.5,
        };
        let json = recall_hits_json(&[h]);
        assert!(json.contains("\"task_id\":\"tj-1\""));
        assert!(json.contains("\"event_type\":\"rejection\""));
        assert!(json.contains("ruled out the shared-table approach"));
    }

    #[test]
    fn recall_hits_json_empty_is_array() {
        assert_eq!(recall_hits_json(&[]), "[]");
    }

    #[test]
    fn fmt_tokens_scales_units() {
        assert_eq!(fmt_tokens(980), "980");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(88_000), "88.0k");
        assert_eq!(fmt_tokens(204_000), "204k");
    }

    #[test]
    fn stats_suffix_shows_spent_and_saved() {
        let spent = tj_core::llm::LlmUsage {
            input_tokens: 1200,
            output_tokens: 300,
            cost_usd: Some(0.0012),
        };
        let saved = Some(Savings {
            raw_tokens: 90_000,
            pack_tokens: 1_500,
        });
        let s = stats_suffix(&spent, &saved);
        // Cost-reporting backend (claude -p) → lead with cost, not muddy tokens.
        assert!(s.contains("cost $0.0012"), "{s}");
        assert!(s.contains("saved ~90.0k→1.5k tok (60×)"), "{s}");
    }

    #[test]
    fn stats_suffix_shows_tokens_for_costless_backend() {
        // API backend reports clean tokens, no cost → show the token count.
        let spent = tj_core::llm::LlmUsage {
            input_tokens: 1800,
            output_tokens: 200,
            cost_usd: None,
        };
        assert_eq!(
            stats_suffix(&spent, &None),
            " | spent 2.0k tok",
            "API backend should show tokens"
        );
    }

    #[test]
    fn stats_suffix_empty_when_nothing_to_report() {
        let spent = tj_core::llm::LlmUsage::default();
        assert_eq!(stats_suffix(&spent, &None), "");
        // Cost omitted when zero/None; tokens still shown.
        let spent = tj_core::llm::LlmUsage {
            input_tokens: 500,
            output_tokens: 0,
            cost_usd: None,
        };
        assert_eq!(stats_suffix(&spent, &None), " | spent 500 tok");
    }

    #[test]
    fn nudge_escalates_only_for_substantial_thin_sessions() {
        // Small session → never escalate, regardless of capture.
        assert!(nudge_escalation_text(1_000, 0).is_none());
        // Substantial session with enough capture → no escalation.
        assert!(nudge_escalation_text(200_000, NUDGE_MIN_EVENTS).is_none());
        // Substantial session, thin capture → escalate.
        let e = nudge_escalation_text(200_000, 0).expect("should escalate");
        assert!(e.contains("substantial work") && e.contains("event_add"));
        // Singular grammar at exactly 1 entry.
        assert!(nudge_escalation_text(200_000, 1)
            .unwrap()
            .contains("1 journal entry"));
    }

    #[test]
    fn flatten_transcript_tags_roles_in_order() {
        use tj_core::session::parser::parse_session;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sess-1.jsonl");
        std::fs::write(&p,
            "{\"type\":\"user\",\"uuid\":\"u1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"content\":\"why?\"}}\n\
             {\"type\":\"assistant\",\"uuid\":\"a1\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"because X\"}]}}\n").unwrap();
        let parsed = parse_session(&p).unwrap();
        let t = flatten_transcript(&parsed);
        let u = t.find("why?").unwrap();
        let a = t.find("because X").unwrap();
        assert!(u < a, "user turn should precede assistant turn");
    }

    #[test]
    fn task_matches_by_session_id_or_time_window() {
        use tj_core::event::{Author, Event, EventType, Source};
        let mut tagged = Event::new(
            "tj-1",
            EventType::Finding,
            Author::Agent,
            Source::Hook,
            "x".into(),
        );
        tagged.meta = serde_json::json!({"session_id": "sess-1"});
        assert!(task_matches_session(&[tagged], "sess-1", None, None));

        let mut legacy = Event::new(
            "tj-2",
            EventType::Finding,
            Author::Agent,
            Source::Hook,
            "y".into(),
        );
        legacy.timestamp = "2026-01-01T00:00:30Z".into();
        legacy.meta = serde_json::json!({}); // no session_id
        assert!(task_matches_session(
            &[legacy.clone()],
            "sess-1",
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:01:00Z"),
        ));
        // Outside the window and no session id → no match.
        assert!(!task_matches_session(
            &[legacy],
            "sess-1",
            Some("2026-02-01T00:00:00Z"),
            Some("2026-02-01T00:01:00Z"),
        ));
    }

    #[test]
    fn candidate_tasks_come_from_events_loaded_once() {
        // The events log is grouped once per run and reused for every
        // session, so candidate lookup takes the grouped map, not a path.
        use tj_core::event::{Author, Event, EventType, Source};
        let mut tagged = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            "Task one".into(),
        );
        tagged.meta = serde_json::json!({"session_id": "sess-1"});
        let by_task = std::collections::BTreeMap::from([("tj-1".to_string(), vec![tagged])]);

        let hit = candidate_tasks_for_session(&by_task, "sess-1", None, None);
        let miss = candidate_tasks_for_session(&by_task, "sess-2", None, None);

        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].task_id, "tj-1");
        assert_eq!(hit[0].title, "Task one");
        assert!(miss.is_empty());
    }

    #[test]
    fn build_dream_inputs_skips_an_unreadable_session() {
        use tj_core::event::{Author, Event, EventType, Source};
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("h.jsonl");
        let mut ev = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            "task".into(),
        );
        ev.meta = serde_json::json!({"session_id": "good"});
        let mut writer = tj_core::storage::JsonlWriter::open(&events_path).unwrap();
        writer.append(&ev).unwrap();
        writer.flush_durable().unwrap();

        // Invalid UTF-8 makes parse_session fail for this one file.
        let bad = dir.path().join("bad.jsonl");
        std::fs::write(&bad, [0xff, 0xfe, b'\n']).unwrap();
        let good = dir.path().join("good.jsonl");
        std::fs::write(&good,
            "{\"type\":\"user\",\"uuid\":\"u1\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"message\":{\"content\":\"hi\"}}\n").unwrap();

        let (inputs, unreadable) = build_dream_inputs(&events_path, &[bad, good], None).unwrap();

        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].0, "good");
        assert_eq!(unreadable, vec!["bad".to_string()]);
    }

    #[test]
    fn persist_pending_v2_includes_session_id_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let p = persist_pending_v2(
            &events_path,
            "PostToolUse",
            "txt",
            "h",
            "hybrid",
            Some("sess-9"),
        )
        .unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["session_id"], serde_json::json!("sess-9"));
        assert_eq!(
            tj_core::session_id::session_id_from_payload(&v).as_deref(),
            Some("sess-9")
        );
    }

    /// PID 1 always exists; for a non-root user `kill(1, 0)` fails with EPERM,
    /// which means "alive, not ours" — never "dead".
    #[cfg(unix)]
    #[test]
    fn pid_is_alive_treats_eperm_as_alive() {
        assert!(pid_is_alive(1));
    }

    #[test]
    fn persist_pending_v2_omits_session_id_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let p =
            persist_pending_v2(&events_path, "PostToolUse", "txt", "h", "hybrid", None).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("session_id").is_none());
    }

    #[test]
    fn pending_entries_are_named_after_their_project() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();

        persist_pending_v2(&events_path, "PostToolUse", "txt", "h", "hybrid", None).unwrap();
        persist_pending(&events_path, "h", "Stop", "txt", "err").unwrap();

        let pending = dir.path().join("pending");
        let names: Vec<String> = std::fs::read_dir(&pending)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| n.starts_with("h.")), "{names:?}");
        assert_eq!(
            project_pending_entries(&pending, "h").unwrap().len(),
            2,
            "both entries belong to project h"
        );
        assert!(project_pending_entries(&pending, "other")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pending_entries_come_back_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        // ULID order == queue order; legacy un-prefixed names interleave by
        // their ULID, not by the hash prefix of the new names.
        let mut expected = Vec::new();
        for i in 0..20u32 {
            let ulid = format!("01JA{i:022}");
            let name = if i % 5 == 0 {
                format!("{ulid}.json")
            } else {
                format!("h.{ulid}.json")
            };
            expected.push(name);
        }
        for name in expected.iter().rev() {
            std::fs::write(dir.path().join(name), "{}").unwrap();
        }

        let got: Vec<String> = project_pending_entries(dir.path(), "h")
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn is_rewind_prompt_simple() {
        assert!(is_rewind_prompt("/rewind"));
        assert!(is_rewind_prompt("/rewind back to plan A"));
        assert!(is_rewind_prompt("  /rewind"));
        assert!(is_rewind_prompt("\t/rewind"));
    }

    #[test]
    fn is_rewind_prompt_case_insensitive() {
        assert!(is_rewind_prompt("/Rewind"));
        assert!(is_rewind_prompt("/REWIND"));
    }

    #[test]
    fn is_rewind_prompt_rejects_non_match() {
        assert!(!is_rewind_prompt("rewind"));
        assert!(!is_rewind_prompt("hello /rewind"));
        assert!(!is_rewind_prompt(""));
        assert!(!is_rewind_prompt("/rewinder"));
    }

    #[test]
    fn recent_task_contexts_gathers_constraints() {
        use tj_core::event::{Author, Event, EventType, Source};
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let state_path = dir.path().join("h.sqlite");
        let project_hash = "h";

        let mut writer = tj_core::storage::JsonlWriter::open(&events_path).unwrap();
        let mut open = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            "task one".into(),
        );
        open.meta = serde_json::json!({ "title": "task one" });
        open.timestamp = "2026-01-01T00:00:00Z".into();
        writer.append(&open).unwrap();
        let mut cons = Event::new(
            "tj-1",
            EventType::Constraint,
            Author::Agent,
            Source::Hook,
            "API limit 100/min".into(),
        );
        cons.timestamp = "2026-01-01T00:00:01Z".into();
        writer.append(&cons).unwrap();
        let mut find = Event::new(
            "tj-1",
            EventType::Finding,
            Author::Agent,
            Source::Hook,
            "read http.rs".into(),
        );
        find.timestamp = "2026-01-01T00:00:02Z".into();
        writer.append(&find).unwrap();
        writer.flush_durable().unwrap();

        let conn = tj_core::db::open(&state_path).unwrap();
        tj_core::db::ingest_new_events(&conn, &events_path, project_hash).unwrap();

        let ctxs = recent_task_contexts(&conn, 5).unwrap();
        let ctx = ctxs.iter().find(|c| c.task_id == "tj-1").unwrap();
        assert!(
            ctx.constraints
                .iter()
                .any(|s| s.contains("API limit 100/min")),
            "constraints should include the constraint event, got {:?}",
            ctx.constraints
        );
        assert!(
            !ctx.constraints.iter().any(|s| s.contains("read http.rs")),
            "constraints must exclude non-constraint events, got {:?}",
            ctx.constraints
        );
    }

    #[test]
    fn recent_task_contexts_bounds_constraints_to_n() {
        use tj_core::event::{Author, Event, EventType, Source};
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let state_path = dir.path().join("h.sqlite");
        let project_hash = "h";

        let mut writer = tj_core::storage::JsonlWriter::open(&events_path).unwrap();
        let mut open = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            "task one".into(),
        );
        open.meta = serde_json::json!({ "title": "task one" });
        open.timestamp = "2026-01-01T00:00:00Z".into();
        writer.append(&open).unwrap();
        for i in 0..7 {
            let mut cons = Event::new(
                "tj-1",
                EventType::Constraint,
                Author::Agent,
                Source::Hook,
                format!("constraint number {i}"),
            );
            // Increasing timestamps so DESC ordering keeps the most recent.
            cons.timestamp = format!("2026-01-01T00:00:1{i}Z");
            writer.append(&cons).unwrap();
        }
        writer.flush_durable().unwrap();

        let conn = tj_core::db::open(&state_path).unwrap();
        tj_core::db::ingest_new_events(&conn, &events_path, project_hash).unwrap();

        let ctxs = recent_task_contexts(&conn, 5).unwrap();
        let ctx = ctxs.iter().find(|c| c.task_id == "tj-1").unwrap();
        assert_eq!(
            ctx.constraints.len(),
            5,
            "bounded to CONSTRAINT_CONTEXT_LIMIT"
        );
        // The 5 most recent are numbers 2..=6.
        assert!(ctx
            .constraints
            .iter()
            .any(|s| s.contains("constraint number 6")));
        assert!(!ctx
            .constraints
            .iter()
            .any(|s| s.contains("constraint number 0")));
        assert!(!ctx
            .constraints
            .iter()
            .any(|s| s.contains("constraint number 1")));
    }

    #[test]
    fn recent_task_contexts_skips_model_switch_constraints() {
        use tj_core::event::{Author, Event, EventType, Source};
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let state_path = dir.path().join("h.sqlite");

        let mut writer = tj_core::storage::JsonlWriter::open(&events_path).unwrap();
        let mut open = Event::new(
            "tj-1",
            EventType::Open,
            Author::User,
            Source::Cli,
            "task one".into(),
        );
        open.meta = serde_json::json!({ "title": "task one" });
        open.timestamp = "2026-01-01T00:00:00Z".into();
        writer.append(&open).unwrap();
        for i in 0..8 {
            // Five real constraints, then three newer model switches.
            let text = if i < 5 {
                format!("constraint number {i}")
            } else {
                format!("Model switched (auto): opus → haiku {i}")
            };
            let mut cons = Event::new(
                "tj-1",
                EventType::Constraint,
                Author::Agent,
                Source::Hook,
                text,
            );
            cons.timestamp = format!("2026-01-01T00:00:1{i}Z");
            writer.append(&cons).unwrap();
        }
        writer.flush_durable().unwrap();

        let conn = tj_core::db::open(&state_path).unwrap();
        tj_core::db::ingest_new_events(&conn, &events_path, "h").unwrap();

        let ctxs = recent_task_contexts(&conn, 5).unwrap();
        let ctx = ctxs.iter().find(|c| c.task_id == "tj-1").unwrap();
        assert_eq!(ctx.constraints.len(), 5, "{:?}", ctx.constraints);
        assert!(
            ctx.constraints
                .iter()
                .all(|s| s.starts_with("constraint number")),
            "{:?}",
            ctx.constraints
        );
    }

    #[test]
    fn topic_is_fts_safe_basic() {
        assert!(topic_is_fts_safe("oauth"));
        assert!(topic_is_fts_safe("foo bar"));
        assert!(!topic_is_fts_safe("foo-bar"));
        assert!(!topic_is_fts_safe("\"quote\""));
        assert!(!topic_is_fts_safe("col:name"));
    }
}
