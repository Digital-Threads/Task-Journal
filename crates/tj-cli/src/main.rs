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
    }
    Ok(())
}
