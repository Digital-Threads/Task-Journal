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
    /// Full-text search across events (FTS5).
    Search {
        /// Query string.
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Search across all projects on this machine, not just the cwd one.
        #[arg(long)]
        all_projects: bool,
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
        /// Override classifier command. Writes env.TJ_CLASSIFIER_CLI into settings.json
        /// so wrappers (aimux, litellm, etc.) work without manual env setup.
        /// Default: classifier uses `claude -p`.
        #[arg(long)]
        classifier_command: Option<String>,
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
        /// Classifier backend: "cli" uses `claude -p` (free with your Pro/Max
        /// subscription) or "api" uses Anthropic API (requires `ANTHROPIC_API_KEY`).
        /// Default: cli.
        #[arg(long, default_value = "cli")]
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
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_dir = tj_core::paths::events_dir()?;
            let events_path = events_dir.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(&events_dir)?;

            let task_id = tj_core::new_task_id();
            let mut event = tj_core::event::Event::new(
                task_id.clone(),
                tj_core::event::EventType::Open,
                tj_core::event::Author::User,
                tj_core::event::Source::Cli,
                context.clone().unwrap_or_else(|| title.clone()),
            );
            event.meta = serde_json::json!({ "title": title });

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
            classifier_command,
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
                let cmd = "task-journal ingest-hook --backend=cli || true";
                let entries = serde_json::json!({
                    "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    "PostToolUse":     [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    "Stop":            [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    // SessionStart drives the auto resume-pack injection:
                    // ingest-hook short-circuits on this kind, queries open
                    // tasks for the current project, and emits the
                    // additionalContext envelope Claude Code expects.
                    "SessionStart":    [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                });
                hooks_obj.insert("hooks".into(), entries);

                // Optional: set env.TJ_CLASSIFIER_CLI for users running classifier
                // through a wrapper (aimux, litellm, etc.). Claude Code reads this
                // env block and propagates the var to hook subprocesses, so users
                // don't need to mess with bashrc.
                if let Some(cmd) = classifier_command {
                    let env = hooks_obj
                        .entry("env".to_string())
                        .or_insert_with(|| serde_json::json!({}));
                    let env_obj = env
                        .as_object_mut()
                        .ok_or_else(|| anyhow::anyhow!("settings.env is not a JSON object"))?;
                    env_obj.insert(
                        "TJ_CLASSIFIER_CLI".to_string(),
                        serde_json::Value::String(cmd),
                    );
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
            let (kind, text) = match (kind, text) {
                (Some(k), Some(t)) => (k, t),
                _ => parse_hook_stdin()?,
            };

            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

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
                let envelope = serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": bundle.trim_end(),
                    }
                });
                println!("{}", serde_json::to_string(&envelope)?);
                return Ok(());
            }

            // Drain any pending entries first (Task 10 fills the real-classifier branch).
            drain_pending(
                &events_path,
                mock_event_type.as_deref(),
                mock_task_id.as_deref(),
                mock_confidence,
            )?;

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
                        let new_task =
                            auto_open_task_from_prompt(&events_path, &project_hash, &conn, &text)?;
                        recent.push(new_task);
                    }

                    use tj_core::classifier::Classifier;
                    let classifier: Box<dyn Classifier> = match backend.as_str() {
                        "cli" => Box::new(tj_core::classifier::cli::ClaudeCliClassifier::default()),
                        "api" => {
                            Box::new(tj_core::classifier::http::AnthropicClassifier::from_env()?)
                        }
                        other => {
                            anyhow::bail!("unknown backend: {other} (expected `cli` or `api`)")
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
        } => {
            if all_projects {
                let state_dir = tj_core::paths::state_dir()?;
                let hashes = tj_core::db::list_all_projects(&state_dir)?;
                for hash in hashes {
                    let path = state_dir.join(format!("{hash}.sqlite"));
                    let conn = match rusqlite::Connection::open(&path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let mut stmt = match conn.prepare(
                        "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT ?2"
                    ) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    let rows = match stmt.query_map(rusqlite::params![&query, limit as i64], |r| {
                        r.get::<_, String>(0)
                    }) {
                        Ok(r) => r,
                        Err(_) => continue,
                    };
                    for id in rows.flatten() {
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
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT ?2",
                )?;
                let ids: Vec<String> = stmt
                    .query_map(rusqlite::params![query, limit as i64], |r| {
                        r.get::<_, String>(0)
                    })?
                    .collect::<Result<_, _>>()?;
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
    }
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
fn parse_hook_stdin() -> anyhow::Result<(String, String)> {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .context("read hook payload from stdin")?;
    let buf = buf.trim();
    if buf.is_empty() {
        return Ok(("Stop".into(), String::new()));
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

    Ok((kind, text))
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
