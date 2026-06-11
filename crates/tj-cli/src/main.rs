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

    // (source, destination) tuples to attempt to rename.
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

    let mut moved: Vec<String> = Vec::new();
    for (src, dst) in &pairs {
        if !src.exists() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if dst.exists() && force {
            std::fs::remove_file(dst).with_context(|| format!("remove existing {dst:?}"))?;
        }
        std::fs::rename(src, dst).with_context(|| format!("rename {src:?} -> {dst:?}"))?;
        moved.push(dst.display().to_string());
    }

    // Re-key the project_hash columns inside the (now renamed) SQLite.
    let new_state_path = state_dir.join(format!("{to_hash}.sqlite"));
    if new_state_path.exists() {
        let conn = tj_core::db::open(&new_state_path)?;
        conn.execute(
            "UPDATE tasks SET project_hash = ?1 WHERE project_hash = ?2",
            rusqlite::params![to_hash, from_hash],
        )?;
        conn.execute(
            "UPDATE index_state SET project_hash = ?1 WHERE project_hash = ?2",
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
        let title = task_events
            .iter()
            .find(|e| e.event_type == tj_core::event::EventType::Open)
            .and_then(|e| {
                e.meta
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| Some(e.text.clone()))
            })
            .unwrap_or_else(|| "(untitled)".into());

        let closed = task_events
            .last()
            .map(|e| e.event_type == tj_core::event::EventType::Close)
            .unwrap_or(false);
        let status = if closed { "closed" } else { "open" };

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

fn run_pending_list() -> Result<()> {
    let dir = pending_dir()?;
    if !dir.exists() {
        println!("(no pending entries)");
        return Ok(());
    }
    let mut entries: Vec<(String, String, String, u32)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
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
        let dead_marker = if id.ends_with(".dead") { " [DEAD]" } else { "" };
        entries.push((id, queued_at, text_preview, attempts));
        let _ = dead_marker;
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

    let mut succeeded = 0usize;
    let mut died = 0usize;
    let mut still_pending = 0usize;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
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

        // The real retry path would call the classifier. The CI-safe
        // mock branch lets tests drive a deterministic outcome.
        let outcome: anyhow::Result<()> = match (mock_etype, mock_tid) {
            (Some(etype), Some(tid)) => {
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
            _ => Err(anyhow::anyhow!(
                "no real classifier wired in retry path yet — pass --mock-* for tests, or run install-hooks and let the hook drain the queue"
            )),
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
                "claude CLI not on PATH — that's fine if you use the API backend \
                 (set ANTHROPIC_API_KEY). For the CLI backend (free with Pro/Max), \
                 install Claude Code from https://claude.com/claude-code"
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
        /// Parent task id — makes this a subtask of <id>.
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
    /// Render and print the resume pack for a task.
    Pack {
        /// Task id (e.g. tj-7f3a).
        task_id: String,
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
    /// Garbage-collect the pending classifier queue. Removes entries
    /// older than N days OR marked dead by retry exhaustion. Run after
    /// classifier auth was broken for a while and the queue grew
    /// stale.
    PendingGc {
        /// Age threshold in days. Default 7.
        #[arg(long, default_value_t = 7)]
        days: i64,
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
    /// Install Claude Code hooks that ingest events into the task journal.
    InstallHooks {
        /// Scope: user (~/.claude/settings.json) or project (./.claude/settings.json).
        #[arg(long, default_value = "user")]
        scope: String,
        /// Remove our hook entries instead of installing.
        #[arg(long)]
        uninstall: bool,
        /// After installing hooks, retro-import existing Claude Code session
        /// history for the current project. Equivalent to running
        /// `task-journal backfill` afterwards. Onboarding shortcut.
        #[arg(long)]
        backfill: bool,
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
        ///   - "hybrid" (default) — keyword heuristic first (free, offline);
        ///     Anthropic API fallback when uncertain (needs ANTHROPIC_API_KEY).
        ///   - "api" — always call the Anthropic API. Best quality, paid.
        ///   - "heuristic" — heuristic only, no LLM. Fastest, lowest coverage.
        ///   - "cli" — deprecated alias for hybrid. The old `claude -p` path
        ///     was removed in v0.8.0 because Anthropic now bills it
        ///     separately from Pro/Max.
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
        /// Classifier backend: "hybrid", "api", or "heuristic". Defaults to hybrid.
        #[arg(long, default_value = "hybrid")]
        backend: String,
    },
    /// One-line status snapshot for the Claude Code statusline. Prints
    /// `[tj-x9rz · open: N · pending: N · stale: N]`. Sub-100ms by
    /// design — wire it via `~/.claude/settings.json` `statusLine`.
    /// Hidden from --help; not a human command.
    #[command(hide = true)]
    Statusline,
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
    Retry {
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

fn main() -> Result<()> {
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
                let body = std::fs::read_to_string(&events_path)?;
                let mut events: Vec<tj_core::event::Event> = body
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<Result<_, _>>()?;
                events.reverse();
                for e in events.into_iter().take(limit) {
                    let title = e
                        .meta
                        .get("title")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| e.text.clone());
                    println!("{}  [{:?}]  {}", e.timestamp, e.event_type, title);
                }
            }
        },
        Commands::Pack { task_id, mode } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
            }
            let pmode = match mode.as_str() {
                "compact" => tj_core::pack::PackMode::Compact,
                "full" => tj_core::pack::PackMode::Full,
                other => anyhow::bail!("unknown mode: {other}"),
            };
            let pack = tj_core::pack::assemble(&conn, &task_id, pmode)?;
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
            if let Some(r) = reason {
                event.meta = serde_json::json!({"reason": r});
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
        Commands::PendingGc { days } => {
            let pending_dir = tj_core::paths::events_dir()?
                .parent()
                .ok_or_else(|| anyhow::anyhow!("events_dir has no parent"))?
                .join("pending");
            if !pending_dir.exists() {
                println!("(no pending dir — nothing to gc)");
                return Ok(());
            }
            let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
            let mut removed = 0usize;
            for entry in std::fs::read_dir(&pending_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                // Prefer the file's mtime over JSON parsing — pending
                // payloads include their own queued_at but are not
                // guaranteed parseable when the classifier corrupted
                // input mid-stream.
                let mtime = entry
                    .metadata()
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
            uninstall,
            backfill,
        } => {
            let settings_path = match scope.as_str() {
                "user" => {
                    let home =
                        std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
                    std::path::PathBuf::from(home)
                        .join(".claude")
                        .join("settings.json")
                }
                "project" => std::env::current_dir()?
                    .join(".claude")
                    .join("settings.json"),
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
                                    .map(|c| !c.contains("task-journal ingest-hook"))
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
                // No --backend flag: the binary's default (hybrid) wins.
                // Hybrid = free heuristic first, Anthropic API fallback when
                // uncertain. Users wanting always-api can edit settings.json
                // and add `--backend=api`.
                let cmd = "task-journal ingest-hook || true";
                let entries = serde_json::json!({
                    "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    "PostToolUse":     [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    "Stop":            [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    // SessionStart drives the auto resume-pack injection:
                    // ingest-hook short-circuits on this kind, queries open
                    // tasks for the current project, and emits the
                    // additionalContext envelope Claude Code expects.
                    "SessionStart":    [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    // PreCompact: drop a marker decision event on the most-recent
                    // open task so the post-compact agent sees a clear boundary
                    // in the journal between pre- and post-compaction reasoning.
                    "PreCompact":      [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                });
                hooks_obj.insert("hooks".into(), entries);
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
                mock_event_type,
                mock_task_id,
                mock_confidence,
            } => {
                run_pending_retry(
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
            if std::env::var("TJ_IN_CLASSIFIER").is_ok() {
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

            // SessionStart: emit a JSON envelope with compact resume-packs of
            // open tasks so Claude Code injects them into its system context
            // automatically. This is the load-bearing UX for "the journal
            // remembers" — without it, users would have to call task_pack
            // manually each session. Empty stdout when no open tasks → no
            // injection, keeps system prompt clean for fresh projects.
            if kind == "SessionStart" {
                // Skip early on a clean machine: nothing to surface, and we
                // don't want SessionStart to spawn empty SQLite files in
                // every project Claude Code is opened in.
                if !events_path.exists() {
                    return Ok(());
                }
                let state_path =
                    tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                let recent = recent_task_contexts(&conn, 3)?;
                if recent.is_empty() {
                    return Ok(());
                }
                let mut bundle = String::new();
                for tc in &recent {
                    let pack = tj_core::pack::assemble(
                        &conn,
                        &tc.task_id,
                        tj_core::pack::PackMode::Compact,
                    )?;
                    bundle.push_str(&pack.text);
                    bundle.push_str("\n\n");
                }

                // v0.10.2 X4: emit `watchPaths` so Claude Code starts
                // monitoring our marker files (CLAUDE.md, README.md,
                // .docs/plans). When any of them changes, Claude Code
                // fires a FileChanged hook event — our ingest-hook
                // handler below treats those as `evidence` entries on
                // the active task so the journal captures
                // "instructions were updated mid-session" without the
                // user manually logging it. Only paths that exist at
                // SessionStart time are emitted (no point watching a
                // non-existent file — Claude Code logs `watcher error`
                // and gives up on it). Gated by TJ_WATCH_PATHS=0.
                let allow_watch_paths = std::env::var("TJ_WATCH_PATHS").as_deref() != Ok("0");
                let watch_candidates = [
                    cwd.join("CLAUDE.md"),
                    cwd.join("README.md"),
                    cwd.join(".docs").join("plans"),
                ];
                let watch_paths: Vec<String> = if allow_watch_paths {
                    watch_candidates
                        .iter()
                        .filter(|p| p.exists())
                        .map(|p| p.to_string_lossy().to_string())
                        .collect()
                } else {
                    Vec::new()
                };

                // v0.10.1 X2: extend SessionStart envelope with the
                // undocumented `sessionTitle` + `initialUserMessage`
                // fields verified in Claude Code 2.1.160's K45 Zod
                // schema. additionalContext already injects the full
                // pack into the system prompt; these two extras give
                // the model a sharper "where were we" signal so it
                // doesn't have to grep the pack to find the active
                // task ID.
                //
                // sessionTitle: terminal tab / window label. Always
                // emitted when there are open tasks — the count alone
                // is useful even with no primary activity.
                //
                // initialUserMessage: prepended to the user's first
                // real prompt this session. We only emit it when the
                // primary task already has events — otherwise it's an
                // unsolicited "resuming" preamble on a hollow task and
                // adds noise. Gated by TJ_INITIAL_USER_MESSAGE=0 for
                // tests / users who'd rather not see it.
                let primary = &recent[0];
                let mut hook_specific = serde_json::json!({
                    "hookEventName": "SessionStart",
                    "additionalContext": bundle.trim_end(),
                    "sessionTitle": format!(
                        "TJ — {} ({} open)",
                        primary.task_id,
                        recent.len(),
                    ),
                });
                if !watch_paths.is_empty() {
                    hook_specific["watchPaths"] = serde_json::Value::Array(
                        watch_paths
                            .into_iter()
                            .map(serde_json::Value::String)
                            .collect(),
                    );
                }
                let allow_initial_user_msg =
                    std::env::var("TJ_INITIAL_USER_MESSAGE").as_deref() != Ok("0");
                // `task-journal create` writes an [open] event, so
                // last_events is never literally empty even for a
                // freshly created task. Require at least one non-open
                // event so we don't inject "Resuming task X" the moment
                // it was just opened with nothing to resume yet.
                let has_real_events = primary.last_events.iter().any(|e| !e.starts_with("[open]"));
                if allow_initial_user_msg && has_real_events {
                    hook_specific["initialUserMessage"] = serde_json::Value::String(format!(
                        "[Task Journal resumed: {} — {}]",
                        primary.task_id, primary.title,
                    ));
                }
                let envelope = serde_json::json!({
                    "hookSpecificOutput": hook_specific,
                });
                println!("{}", serde_json::to_string(&envelope)?);
                return Ok(());
            }

            // v0.10.2 X4: FileChanged. Claude Code 2.1.x fires this
            // event whenever a path in `watchPaths` (emitted on
            // SessionStart) changes. Payload: { file_path, event:
            // "change"|"add"|"unlink" }. We translate it into an
            // `evidence` event on the active task — captures
            // "the user/agent edited CLAUDE.md mid-session" without
            // anyone typing anything. Schema verified in 2.1.160:
            // `literal("FileChanged"), file_path: y.string(), event:
            // y.enum(["change","add","unlink"])`.
            //
            // No active task → drop silently (we're not opening a
            // task just because a watched file moved). No events_path
            // → ditto, fresh project.
            if kind == "FileChanged" {
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
                let file_path = payload
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unknown)");
                let change = payload
                    .get("event")
                    .and_then(|v| v.as_str())
                    .unwrap_or("change");
                // Trim noisy absolute paths to project-relative when
                // possible — the journal is per-project so the prefix
                // is redundant and just steals tokens from the pack.
                let display_path = cwd
                    .to_str()
                    .and_then(|c| file_path.strip_prefix(c))
                    .map(|s| s.trim_start_matches('/').to_string())
                    .unwrap_or_else(|| file_path.to_string());
                let evidence_text = format!("FileChanged ({change}): {display_path}");
                let mut event = tj_core::event::Event::new(
                    &tc.task_id,
                    tj_core::event::EventType::Evidence,
                    tj_core::event::Author::Classifier,
                    tj_core::event::Source::Hook,
                    evidence_text,
                );
                event.confidence = Some(0.9);
                event.status = tj_core::event::EventStatus::Confirmed;
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

            // Drain any pending entries first (Task 10 fills the real-classifier branch).
            drain_pending(
                &events_path,
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
                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;
                println!("{}", event.event_id);
                return Ok(());
            }

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
                let _ = persist_pending_v2(&events_path, &kind, &text, &project_hash, &backend, live_session_id.as_deref())?;
                // Fire-and-forget worker. Errors here are best-effort —
                // a failure to spawn just means the entry sits in
                // pending/ until the next hook fires another spawn.
                let _ = spawn_classify_worker(&backend);

                // v0.10.0 asyncRewake backlog signal. Only the PostToolUse
                // hook runs as asyncRewake (hooks.json sets TJ_ASYNC_REWAKE=1
                // there), so other kinds — and direct CLI invocations —
                // never exit 2 even on overflow. Exit code 2 from a sync
                // hook would BLOCK the operation; only asyncRewake hooks
                // treat code 2 as "wake the model with rewakeMessage". stdout
                // is appended to the wake message, so the user sees the
                // drain command without us reaching into stderr.
                let allow_wake = std::env::var("TJ_ASYNC_REWAKE").as_deref() == Ok("1");
                if allow_wake && kind == "PostToolUse" {
                    let pending_count = count_pending_entries(&events_path).unwrap_or(0);
                    if pending_count > PENDING_OVERFLOW_THRESHOLD {
                        println!(
                            "Task Journal pending queue: {pending_count} entries. Classifier behind — run `task-journal pending-gc --days 0` to drain.",
                        );
                        std::process::exit(2);
                    }
                }
                return Ok(());
            }

            // Derive author_hint from hook kind: user prompts → "user", everything else → "assistant"
            let author_hint = if kind.contains("UserPrompt") {
                "user"
            } else {
                "assistant"
            };

            let (etype, task_id, confidence, evidence_strength, suggested_text) = if let (
                Some(t),
                Some(tid),
            ) =
                (mock_event_type.as_deref(), mock_task_id.as_deref())
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
                    let new_task =
                        auto_open_task_from_prompt(&events_path, &project_hash, &conn, &text)?;
                    recent.push(new_task);
                }

                use tj_core::classifier::Classifier;
                let classifier: Box<dyn Classifier> = match backend.as_str() {
                    // v0.8.0: hybrid is the new default. Heuristic
                    // pattern-matching first (free), Anthropic API
                    // fallback when uncertain (requires ANTHROPIC_API_KEY).
                    // No background spawn of `claude -p` — that subprocess
                    // now bills tokens separately from Pro/Max.
                    "hybrid" | "" => {
                        Box::new(tj_core::classifier::hybrid::HybridClassifier::from_env())
                    }
                    "api" => Box::new(tj_core::classifier::http::AnthropicClassifier::from_env()?),
                    "heuristic" => {
                        // Heuristic-only: no LLM at all. Trades coverage
                        // for absolute zero-cost / offline operation.
                        use tj_core::classifier::heuristic::try_heuristic;
                        use tj_core::classifier::{ClassifyInput, ClassifyOutput};
                        struct HeuristicOnly;
                        impl Classifier for HeuristicOnly {
                            fn classify(
                                &self,
                                input: &ClassifyInput,
                            ) -> anyhow::Result<ClassifyOutput> {
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
                        "unknown backend: {other} (expected `hybrid`, `api`, or `heuristic`)"
                    ),
                };
                let input = tj_core::classifier::ClassifyInput {
                    text: text.clone(),
                    author_hint: author_hint.into(),
                    recent_tasks: recent,
                };
                let out = match classifier.classify(&input) {
                    Ok(o) => o,
                    Err(e) => {
                        persist_pending(&events_path, &text, &e.to_string())?;
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
                            &text,
                            &format!("task_id_guess `{tid}` not found"),
                        )?;
                        return Ok(());
                    }
                    Some(s) if s == "closed" => {
                        persist_pending(
                            &events_path,
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
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path =
                tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path =
                tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
            let conn = tj_core::db::open(&state_path)?;

            // 1. Resolve session files in scope.
            let project_dir = tj_core::session::discovery::find_project_dir(&cwd)?;
            let Some(project_dir) = project_dir else {
                println!("dream: no Claude Code sessions found for this project");
                return Ok(());
            };
            let session_paths = tj_core::session::discovery::list_sessions(&project_dir)?;

            let since_time = if let Some(days) = since {
                Some(
                    std::time::SystemTime::now()
                        - std::time::Duration::from_secs((days.max(0) as u64) * 86_400),
                )
            } else {
                // Watermark → SystemTime. Absent watermark = all sessions.
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
            let in_scope = tj_core::dream::scope::in_scope(scoped, since_time, limit);

            // 2. Assemble (session_id, BackfillInput) per session.
            let run_id = ulid::Ulid::new().to_string();
            let sessions = build_dream_inputs(&events_path, &in_scope, task.as_deref())?;

            // 3. Run.
            let opts = tj_core::dream::DreamOptions {
                project_hash: project_hash.clone(),
                dry_run,
            };
            if dry_run {
                println!("dream (dry-run): {} session(s) in scope", sessions.len());
                return Ok(());
            }
            let backend = tj_core::dream::http::AnthropicDreamBackend::from_env()?;
            let report =
                tj_core::dream::run_dream(&conn, &events_path, &opts, &backend, sessions, &run_id)?;

            // 4. Advance watermark to now (only reached on success).
            tj_core::dream::state::set_last_dream_at(
                &conn,
                &project_hash,
                &chrono::Utc::now().to_rfc3339(),
            )?;
            println!(
                "dream: {} session(s) processed, {} event(s) backfilled",
                report.sessions_processed, report.events_backfilled
            );
        }
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

            let body = std::fs::read_to_string(&events_path)?;
            let all_events: Vec<tj_core::event::Event> = body
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()?;

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
                        // Derive title from the first open event's meta, or text.
                        let title = task_events
                            .iter()
                            .find(|e| e.event_type == tj_core::event::EventType::Open)
                            .and_then(|e| {
                                e.meta
                                    .get("title")
                                    .and_then(|v| v.as_str())
                                    .map(String::from)
                                    .or_else(|| Some(e.text.clone()))
                            })
                            .unwrap_or_else(|| "(untitled)".into());

                        // Determine status: closed if last event is close, else open.
                        let status = if task_events
                            .last()
                            .map(|e| e.event_type == tj_core::event::EventType::Close)
                            .unwrap_or(false)
                        {
                            "closed"
                        } else {
                            "open"
                        };

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
                        Err(_) => continue,
                    };
                    let ids = match run_search(
                        &conn,
                        &fts_query,
                        &like_query,
                        event_type.as_deref(),
                        limit,
                    ) {
                        Ok(v) => v,
                        Err(_) => continue,
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
                        "No Claude Code sessions found for: {}",
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

            // List available sessions.
            let mut sessions = discovery::list_sessions(&proj_dir)?;
            if let Some(max) = limit {
                sessions.truncate(max);
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

            // Check which sessions are already imported (idempotent).
            let already_imported = if events_path.exists() {
                let content = std::fs::read_to_string(&events_path).unwrap_or_default();
                sessions
                    .iter()
                    .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
                    .filter(|sid| content.contains(sid))
                    .collect::<std::collections::HashSet<_>>()
            } else {
                std::collections::HashSet::new()
            };

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

    // Pending dir is global — one entry per queued classifier failure.
    // No project filter (matches the brief; per-project counting would
    // need extra metadata in each pending file).
    let pending_count = pending_dir()
        .ok()
        .and_then(|d| std::fs::read_dir(&d).ok())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| x == "json")
                        .unwrap_or(false)
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
    for hash in hashes {
        let path = state_dir.join(format!("{hash}.sqlite"));
        let conn = match rusqlite::Connection::open(&path) {
            Ok(c) => c,
            Err(_) => continue,
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
            Err(_) => continue,
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
            Err(_) => continue,
        };
        for row in rows.flatten() {
            hits.push(row);
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
        out.push(tj_core::classifier::TaskContext {
            task_id,
            title,
            last_events,
        });
    }
    Ok(out)
}

/// v0.5.0 Phase A: when ingest-hook fires UserPromptSubmit and there
/// are no open tasks, synthesize one from the prompt itself. Title is
/// the first line trimmed to 80 chars; goal is the prompt trimmed to
/// 200 chars. Returns a TaskContext so the classifier has somewhere
/// to attach the same prompt as the first real event.
fn auto_open_task_from_prompt(
    events_path: &std::path::Path,
    project_hash: &str,
    conn: &rusqlite::Connection,
    prompt: &str,
) -> anyhow::Result<tj_core::classifier::TaskContext> {
    let cleaned = prompt.trim();
    // Title: first non-empty line, ≤80 chars. Falls back to "(empty
    // prompt)" so we never write a NULL title — the classifier and
    // the TUI both display titles directly.
    let title: String = cleaned
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(80).collect())
        .unwrap_or_else(|| "(auto-opened: empty prompt)".to_string());
    let goal: String = cleaned.chars().take(200).collect();

    let task_id = tj_core::new_task_id();
    let mut event = tj_core::event::Event::new(
        task_id.clone(),
        tj_core::event::EventType::Open,
        tj_core::event::Author::User,
        tj_core::event::Source::Cli,
        title.clone(),
    );
    event.meta = serde_json::json!({ "title": title, "auto_opened": true });

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

    Ok(tj_core::classifier::TaskContext {
        task_id,
        title,
        last_events: vec![],
    })
}

fn persist_pending(events_path: &std::path::Path, text: &str, err: &str) -> anyhow::Result<()> {
    let pending_dir = events_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pending");
    std::fs::create_dir_all(&pending_dir)?;
    let id = ulid::Ulid::new().to_string();
    let payload = serde_json::json!({"text": text, "error": err, "queued_at": chrono::Utc::now().to_rfc3339()});
    std::fs::write(
        pending_dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&payload)?,
    )?;
    Ok(())
}

/// v0.6.2: queue an ingest event for the detached classify-worker. The
/// hook returns immediately after writing this entry so it does not
/// block Claude Code's hook timeout (was 5-30s, now <100ms). Schema "v2"
/// Threshold for the v0.10.0 asyncRewake backlog signal. When the
/// PostToolUse hook (configured with `asyncRewake: true` in
/// `hooks.json`) finds more than this many entries already queued
/// in `pending/`, it exits with code 2 to wake the model with a
/// system reminder pointing at `task-journal pending-gc`. Tuned so
/// that normal load (<5 in-flight at any moment) never trips, but
/// a stuck classifier surfaces visibly before the queue grows into
/// the hundreds (the v0.6.2 fork-bomb era saw 515 entries before a
/// user noticed).
const PENDING_OVERFLOW_THRESHOLD: usize = 25;

/// Count `.json` (and `.json.dead`) entries currently sitting in
/// `pending/` next to `events_path`. Best-effort: any IO error
/// returns 0 so a borked filesystem never wakes the model with
/// noise. Used by the asyncRewake backlog signal.
fn count_pending_entries(events_path: &std::path::Path) -> anyhow::Result<usize> {
    let dir = events_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("events_path has no grandparent"))?
        .join("pending");
    if !dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some("json") = path.extension().and_then(|e| e.to_str()) {
            count += 1;
        }
    }
    Ok(count)
}

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
    let path = pending_dir.join(format!("{id}.json"));
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
        .env_remove("TJ_IN_CLASSIFIER");
    let _child = cmd.spawn().context("spawn classify-worker")?;
    // Drop child intentionally — Linux init reaps when parent exits.
    Ok(())
}

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
                    // worker is running; back off. If dead/missing →
                    // remove stale file and retry.
                    let body = std::fs::read_to_string(&path).unwrap_or_default();
                    let pid: Option<u32> = body.trim().parse().ok();
                    if let Some(pid) = pid {
                        if pid_is_alive(pid) {
                            return Ok(None);
                        }
                    }
                    // Stale (no PID, or dead PID) — remove and retry.
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
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    for e in std::fs::read_dir(&pending)? {
        let e = e?;
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) == Some("json") {
            entries.push(p);
        }
    }

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
            std::fs::remove_file(path)?;
            return Ok(());
        }
        let new_task = auto_open_task_from_prompt(events_path, project_hash, &conn, &text)?;
        recent.push(new_task);
    }

    let author_hint = if kind.contains("UserPrompt") {
        "user"
    } else {
        "assistant"
    };

    use tj_core::classifier::Classifier;
    let classifier: Box<dyn Classifier> = match backend {
        "hybrid" | "" => Box::new(tj_core::classifier::hybrid::HybridClassifier::from_env()),
        "api" => Box::new(tj_core::classifier::http::AnthropicClassifier::from_env()?),
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
        other => {
            anyhow::bail!("unknown backend: {other} (expected `hybrid`, `api`, or `heuristic`)")
        }
    };
    let input = tj_core::classifier::ClassifyInput {
        text: text.clone(),
        author_hint: author_hint.into(),
        recent_tasks: recent,
    };
    let out = match classifier.classify(&input) {
        Ok(o) => o,
        Err(e) => {
            // Persist as legacy v1 pending entry so `pending retry`
            // surfaces it; remove the v2 source.
            persist_pending(events_path, &text, &e.to_string())?;
            std::fs::remove_file(path)?;
            return Ok(());
        }
    };

    let Some(tid) = out.task_id_guess else {
        std::fs::remove_file(path)?;
        return Ok(());
    };

    use tj_core::event::EventType;
    if matches!(out.event_type, EventType::Close) && kind == "Stop" {
        std::fs::remove_file(path)?;
        return Ok(());
    }
    match tj_core::db::task_status(&conn, &tid)? {
        None => {
            persist_pending(
                events_path,
                &text,
                &format!("task_id_guess `{tid}` not found"),
            )?;
            std::fs::remove_file(path)?;
            return Ok(());
        }
        Some(s) if s == "closed" => {
            persist_pending(
                events_path,
                &text,
                &format!("task_id_guess `{tid}` is closed"),
            )?;
            std::fs::remove_file(path)?;
            return Ok(());
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
    tj_core::session_id::stamp_session_id(&mut event.meta, chunk_session_id.as_deref());

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

    std::fs::remove_file(path)?;
    Ok(())
}

fn drain_pending(
    events_path: &std::path::Path,
    mock_etype: Option<&str>,
    mock_tid: Option<&str>,
    mock_conf: Option<f64>,
) -> anyhow::Result<()> {
    let pending_dir = events_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pending");
    if !pending_dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(&pending_dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let body = std::fs::read_to_string(entry.path())?;
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
        if !text.is_empty() {
            if let (Some(t), Some(tid)) = (mock_etype, mock_tid) {
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
            }
        }
        std::fs::remove_file(entry.path())?;
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

/// Read the project's events from `events_path`, group by `task_id`, and
/// return candidate task contexts for sessions whose events match this
/// session (precise session_id, or legacy time-window). Each context
/// carries the task title and up to the last ~20 event texts (dedup
/// context for the backend).
fn candidate_tasks_for_session(
    events_path: &std::path::Path,
    session_id: &str,
    first_ts: Option<&str>,
    last_ts: Option<&str>,
) -> anyhow::Result<Vec<tj_core::dream::backend::BackfillTaskContext>> {
    use std::collections::BTreeMap;
    use tj_core::dream::backend::BackfillTaskContext;
    use tj_core::event::{Event, EventType};

    if !events_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(events_path)?;
    let mut by_task: BTreeMap<String, Vec<Event>> = BTreeMap::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<Event>(line) {
            by_task.entry(e.task_id.clone()).or_default().push(e);
        }
    }

    let mut out = Vec::new();
    for (task_id, events) in by_task {
        if !task_matches_session(&events, session_id, first_ts, last_ts) {
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
            task_id,
            title,
            existing_events,
        });
    }
    Ok(out)
}

/// Assemble per-session `(session_id, BackfillInput)` from the in-scope
/// session transcripts and the project's existing events.
fn build_dream_inputs(
    events_path: &std::path::Path,
    sessions: &[std::path::PathBuf],
    task_filter: Option<&str>,
) -> anyhow::Result<Vec<(String, tj_core::dream::backend::BackfillInput)>> {
    use tj_core::dream::backend::BackfillInput;
    use tj_core::session::parser::parse_session;

    let mut out = Vec::new();
    for path in sessions {
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let parsed = parse_session(path)?;

        let candidates = candidate_tasks_for_session(
            events_path,
            &session_id,
            parsed.first_timestamp.as_deref(),
            parsed.last_timestamp.as_deref(),
        )?;
        let tasks: Vec<_> = candidates
            .into_iter()
            .filter(|t| task_filter.map_or(true, |f| f == t.task_id))
            .collect();
        if tasks.is_empty() {
            continue;
        }

        let transcript = flatten_transcript(&parsed);
        out.push((session_id, BackfillInput { tasks, transcript }));
    }
    Ok(out)
}

#[cfg(test)]
mod inline_tests {
    // Sits at the bottom of the file to satisfy
    // `clippy::items_after_test_module` — every other free fn must be
    // declared before this module begins.
    use super::*;

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
        let mut tagged =
            Event::new("tj-1", EventType::Finding, Author::Agent, Source::Hook, "x".into());
        tagged.meta = serde_json::json!({"session_id": "sess-1"});
        assert!(task_matches_session(&[tagged], "sess-1", None, None));

        let mut legacy =
            Event::new("tj-2", EventType::Finding, Author::Agent, Source::Hook, "y".into());
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
    fn persist_pending_v2_includes_session_id_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let p = persist_pending_v2(&events_path, "PostToolUse", "txt", "h", "hybrid", Some("sess-9"))
            .unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["session_id"], serde_json::json!("sess-9"));
        assert_eq!(
            tj_core::session_id::session_id_from_payload(&v).as_deref(),
            Some("sess-9")
        );
    }

    #[test]
    fn persist_pending_v2_omits_session_id_when_none() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events").join("h.jsonl");
        std::fs::create_dir_all(events_path.parent().unwrap()).unwrap();
        let p = persist_pending_v2(&events_path, "PostToolUse", "txt", "h", "hybrid", None).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v.get("session_id").is_none());
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
    fn topic_is_fts_safe_basic() {
        assert!(topic_is_fts_safe("oauth"));
        assert!(topic_is_fts_safe("foo bar"));
        assert!(!topic_is_fts_safe("foo-bar"));
        assert!(!topic_is_fts_safe("\"quote\""));
        assert!(!topic_is_fts_safe("col:name"));
    }
}
