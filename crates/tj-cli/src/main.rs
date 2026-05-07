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
    issues: Vec<String>,
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

fn run_doctor() -> Result<DoctorReport> {
    let mut issues: Vec<String> = Vec::new();

    // 1. claude binary in PATH
    let claude_check = PCommand::new("claude").arg("--version").output();
    let (claude_in_path, claude_version) = match claude_check {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (true, Some(v))
        }
        Ok(_) | Err(_) => {
            issues.push(
                "claude CLI not found on PATH — auto-capture hooks will fall back to API \
                 backend (set ANTHROPIC_API_KEY) or fail silently"
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
    },
    /// Show local classifier and journal statistics.
    Stats,
    /// Interactive TUI: browse sessions and read chats.
    #[command(alias = "tui")]
    Ui {
        /// Project path override (default: current directory).
        #[arg(long)]
        project: Option<String>,
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
    IngestHook {
        /// Hook kind: UserPromptSubmit | PostToolUse | Stop | SessionStart.
        #[arg(long)]
        kind: String,
        /// The chat chunk text.
        #[arg(long)]
        text: String,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Create { title, context } => {
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
        Commands::Close { task_id, reason } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            // Catch up the index then assert the task is real before we
            // append a close event for an id that never existed.
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
        Commands::InstallHooks { scope, uninstall } => {
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
                hooks_obj.remove("hooks");
            } else {
                // Wrap with `|| true` so a failed classifier (network down, rate limit,
                // missing API key) NEVER breaks Claude Code. Failures land in pending/
                // and replay on next ingest.
                // Default to subscription-based classifier (`claude -p`).
                // Power users with API key can run install-hooks --backend=api below.
                let cmd = "task-journal ingest-hook --kind=$CLAUDE_HOOK_NAME --text=\"$CLAUDE_HOOK_TEXT\" --backend=cli || true";
                let entries = serde_json::json!({
                    "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    "PostToolUse":     [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                    "Stop":            [{ "matcher": "", "hooks": [{ "type": "command", "command": cmd }] }],
                });
                hooks_obj.insert("hooks".into(), entries);
            }
            std::fs::write(&settings_path, serde_json::to_string_pretty(&current)?)?;
            println!("{}", settings_path.display());
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
        Commands::IngestHook {
            kind,
            text,
            backend,
            mock_event_type,
            mock_task_id,
            mock_confidence,
        } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

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
                    let recent = recent_task_contexts(&conn, 5)?;
                    if recent.is_empty() {
                        // No active tasks — nothing to classify against. Skip silently.
                        return Ok(());
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
                other => {
                    anyhow::bail!("unknown format: {other} (expected `md`, `json`, or `html`)")
                }
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
        Commands::Ui { project } => {
            let project_path = match project {
                Some(p) => std::path::PathBuf::from(p),
                None => std::env::current_dir()?,
            };
            let mut app = tui::app::App::new(&project_path)?;
            if app.session_list.sessions.is_empty() {
                eprintln!(
                    "No Claude Code sessions found for: {}",
                    project_path.display()
                );
                return Ok(());
            }
            app.run()?;
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
