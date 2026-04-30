use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// Hook entry point: ingest a chat chunk through the classifier.
    IngestHook {
        /// Hook kind: UserPromptSubmit | PostToolUse | Stop | SessionStart.
        #[arg(long)]
        kind: String,
        /// The chat chunk text.
        #[arg(long)]
        text: String,
        /// (test/dev) override: bypass classifier and force this event type.
        #[arg(long)]
        mock_event_type: Option<String>,
        /// (test/dev) override: target task id.
        #[arg(long)]
        mock_task_id: Option<String>,
        /// (test/dev) override: confidence value.
        #[arg(long)]
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

            // ULID layout: chars 0-9 = timestamp (48b), 10-25 = random (80b).
            // Taking from random portion to avoid same-prefix collisions for tasks
            // created within ~12 days (which would happen with [..6]).
            let task_id = format!("tj-{}", &ulid::Ulid::new().to_string()[10..16].to_lowercase());
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
                let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
                if !events_path.exists() {
                    println!("(no events yet)");
                    return Ok(());
                }
                let body = std::fs::read_to_string(&events_path)?;
                let mut events: Vec<tj_core::event::Event> = body.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<Result<_, _>>()?;
                events.reverse();
                for e in events.into_iter().take(limit) {
                    let title = e.meta.get("title")
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
                tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
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
        Commands::Event { task_id, r#type, text, corrects, supersedes } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            let event_type = parse_event_type(&r#type)?;
            let mut event = tj_core::event::Event::new(
                &task_id, event_type,
                tj_core::event::Author::User, tj_core::event::Source::Cli,
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

            let mut event = tj_core::event::Event::new(
                &task_id, tj_core::event::EventType::Close,
                tj_core::event::Author::User, tj_core::event::Source::Cli,
                reason.clone().unwrap_or_else(|| "(closed)".into()),
            );
            if let Some(r) = reason { event.meta = serde_json::json!({"reason": r}); }

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            println!("{}", event.event_id);
        }
        Commands::EventCorrect { corrects, task, text } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            let mut event = tj_core::event::Event::new(
                &task, tj_core::event::EventType::Correction,
                tj_core::event::Author::User, tj_core::event::Source::Cli,
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
                    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME not set"))?;
                    std::path::PathBuf::from(home).join(".claude").join("settings.json")
                }
                "project" => std::env::current_dir()?.join(".claude").join("settings.json"),
                other => anyhow::bail!("unknown scope: {other}"),
            };
            if let Some(p) = settings_path.parent() { std::fs::create_dir_all(p)?; }

            let mut current: serde_json::Value = if settings_path.exists() {
                serde_json::from_str(&std::fs::read_to_string(&settings_path)?)
                    .unwrap_or_else(|_| serde_json::json!({}))
            } else {
                serde_json::json!({})
            };

            let hooks_obj = current.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings is not a JSON object"))?;
            if uninstall {
                hooks_obj.remove("hooks");
            } else {
                let cmd = "task-journal ingest-hook --kind=$CLAUDE_HOOK_NAME --text=\"$CLAUDE_HOOK_TEXT\"";
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
        Commands::IngestHook { kind: _, text, mock_event_type, mock_task_id, mock_confidence } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            std::fs::create_dir_all(events_path.parent().unwrap())?;

            // Drain any pending entries first (Task 10 fills the real-classifier branch).
            drain_pending(&events_path, mock_event_type.as_deref(), mock_task_id.as_deref(), mock_confidence)?;

            let (etype, task_id, confidence) = if let (Some(t), Some(tid)) = (mock_event_type.as_deref(), mock_task_id.as_deref()) {
                (parse_event_type(t)?, tid.to_string(), mock_confidence.unwrap_or(1.0))
            } else {
                let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
                let conn = tj_core::db::open(&state_path)?;
                if events_path.exists() {
                    tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
                }
                let recent = recent_task_contexts(&conn, 5)?;
                if recent.is_empty() {
                    return Ok(());
                }

                use tj_core::classifier::Classifier;
                let classifier = tj_core::classifier::http::AnthropicClassifier::from_env()?;
                let input = tj_core::classifier::ClassifyInput {
                    text: text.clone(),
                    author_hint: "assistant".into(),
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
                (out.event_type, tid, out.confidence)
            };

            let mut event = tj_core::event::Event::new(
                &task_id, etype,
                tj_core::event::Author::Classifier, tj_core::event::Source::Hook,
                text,
            );
            event.confidence = Some(confidence);
            event.status = tj_core::classifier::decide_status(confidence);

            let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
            writer.append(&event)?;
            writer.flush_durable()?;
            println!("{}", event.event_id);
        }
        Commands::Search { query, limit } => {
            let cwd = std::env::current_dir()?;
            let project_hash = tj_core::project_hash::from_path(&cwd)?;
            let events_path = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
            let state_path = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));

            let conn = tj_core::db::open(&state_path)?;
            if events_path.exists() {
                tj_core::db::rebuild_state(&conn, &events_path, &project_hash)?;
            }

            let mut stmt = conn.prepare(
                "SELECT DISTINCT task_id FROM search_fts WHERE search_fts MATCH ?1 LIMIT ?2"
            )?;
            let ids: Vec<String> = stmt
                .query_map(rusqlite::params![query, limit as i64], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            for id in ids { println!("{id}"); }
        }
    }
    Ok(())
}

fn recent_task_contexts(conn: &rusqlite::Connection, limit: usize) -> anyhow::Result<Vec<tj_core::classifier::TaskContext>> {
    let mut stmt = conn.prepare(
        "SELECT task_id, title FROM tasks WHERE status='open' ORDER BY last_event_at DESC LIMIT ?1"
    )?;
    let task_rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::with_capacity(task_rows.len());
    for (task_id, title) in task_rows {
        let mut e_stmt = conn.prepare(
            "SELECT ei.type, sf.text FROM events_index ei
             LEFT JOIN search_fts sf ON sf.event_id = ei.event_id
             WHERE ei.task_id=?1 ORDER BY ei.timestamp DESC LIMIT 3"
        )?;
        let last_events: Vec<String> = e_stmt
            .query_map(rusqlite::params![task_id], |r| {
                let ty: String = r.get(0)?;
                let txt: Option<String> = r.get(1)?;
                Ok(format!("[{ty}] {}", txt.unwrap_or_default().chars().take(80).collect::<String>()))
            })?
            .collect::<Result<_, _>>()?;
        out.push(tj_core::classifier::TaskContext { task_id, title, last_events });
    }
    Ok(out)
}

fn persist_pending(events_path: &std::path::Path, text: &str, err: &str) -> anyhow::Result<()> {
    let pending_dir = events_path.parent().unwrap().parent().unwrap().join("pending");
    std::fs::create_dir_all(&pending_dir)?;
    let id = ulid::Ulid::new().to_string();
    let payload = serde_json::json!({"text": text, "error": err, "queued_at": chrono::Utc::now().to_rfc3339()});
    std::fs::write(pending_dir.join(format!("{id}.json")), serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn drain_pending(
    events_path: &std::path::Path,
    mock_etype: Option<&str>,
    mock_tid: Option<&str>,
    mock_conf: Option<f64>,
) -> anyhow::Result<()> {
    let pending_dir = events_path.parent().unwrap().parent().unwrap().join("pending");
    if !pending_dir.exists() { return Ok(()); }

    for entry in std::fs::read_dir(&pending_dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") { continue; }

        let body = std::fs::read_to_string(entry.path())?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if !text.is_empty() {
            if let (Some(t), Some(tid)) = (mock_etype, mock_tid) {
                let mut event = tj_core::event::Event::new(
                    tid, parse_event_type(t)?,
                    tj_core::event::Author::Classifier, tj_core::event::Source::Hook, text,
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
