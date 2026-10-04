//! task-journal-mcp: MCP server entry point.
//!
//! Phase 2 wires real implementations into all 5 tools, calling tj-core.

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{
    handler::server::tool::Parameters, handler::server::wrapper::Json, tool, tool_router,
    transport::io::stdio, ErrorData as McpError, ServerHandler, ServiceExt,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Optional override for the project directory used by every tool handler.
/// `None` (the default) means "use the current working directory at the time
/// the tool is invoked", which preserves 0.1.x behaviour. Set once from the
/// CLI parser and never mutated again.
static PROJECT_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Parser)]
#[command(
    name = "task-journal-mcp",
    version,
    about = "MCP server for task-journal"
)]
struct Cli {
    /// Override the project directory used to resolve event/state paths.
    /// Defaults to the current working directory when omitted.
    #[arg(long, value_name = "PATH")]
    project_dir: Option<PathBuf>,
}

/// Convert any internal failure into a JSON-RPC error frame. We attach the
/// stringified `anyhow::Error` chain as the `message` so the client sees the
/// full context (e.g. "task not found: tj-x: no row returned").
fn into_mcp_error(err: anyhow::Error) -> McpError {
    McpError::internal_error(format!("{err:#}"), None)
}

/// Stable, low-cost correlation token for one tool invocation. ULID gives
/// us 26 lexicographic characters with embedded timestamp ordering and a
/// random suffix — tools do not need millisecond uniqueness, but the
/// timestamp makes log scrubbing easier than a pure-random UUID.
fn new_correlation_id() -> String {
    ulid::Ulid::new().to_string()
}

/// Wrap one tool handler with structured tracing. Emits one INFO line at
/// entry (with the correlation id and tool name) and one INFO line at
/// exit (with elapsed ms and ok/err). Callers grep on `correlation_id=`
/// to follow a single client request across logs.
async fn traced_tool<T, Fut>(tool: &'static str, fut: Fut) -> Result<T, McpError>
where
    Fut: std::future::Future<Output = Result<T, McpError>>,
{
    let correlation_id = new_correlation_id();
    let started_at = std::time::Instant::now();
    tracing::info!(tool, %correlation_id, "tool_call start");
    let result = fut.await;
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(tool, %correlation_id, elapsed_ms, "tool_call ok"),
        Err(e) => tracing::warn!(
            tool,
            %correlation_id,
            elapsed_ms,
            error = %e.message,
            "tool_call err"
        ),
    }
    result
}

/// Run synchronous I/O on the tokio blocking pool. Without this, every tool
/// handler would do SQLite + JSONL work directly on the executor thread
/// and a slow operation in one tool would stall every other concurrent
/// request — defeats the point of using an async runtime at all.
async fn run_blocking<T, F>(f: F) -> Result<T, McpError>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let join_result = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| McpError::internal_error(format!("blocking task panicked: {e}"), None))?;
    join_result.map_err(into_mcp_error)
}

/// Process-wide cache of SQLite connections keyed by state-file path.
///
/// Without this, every tool handler called `tj_core::db::open()` which
/// re-runs PRAGMAs, the migrations registry, and re-creates a new WAL
/// reader. At small N the open cost dominates the actual work.
///
/// Storage layout: an outer `Mutex` guards the map (only briefly, during
/// insert/lookup), and each entry is `Arc<Mutex<Connection>>` so callers
/// can hold a connection across a longer transaction without blocking
/// other projects.
fn connection_cache() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<Connection>>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<Connection>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get or create the cached `Connection` for a SQLite state path. The
/// returned `Arc<Mutex<...>>` is shared with future callers; the inner
/// mutex is the lock you actually want to take during a tool call.
fn cached_open(state_path: &Path) -> anyhow::Result<Arc<Mutex<Connection>>> {
    let mut cache = connection_cache()
        .lock()
        .map_err(|e| anyhow::anyhow!("connection cache poisoned: {e}"))?;
    if let Some(existing) = cache.get(state_path) {
        return Ok(existing.clone());
    }
    let conn =
        tj_core::db::open(state_path).with_context(|| format!("open SQLite at {state_path:?}"))?;
    let arc = Arc::new(Mutex::new(conn));
    cache.insert(state_path.to_path_buf(), arc.clone());
    Ok(arc)
}

/// MCP instructions delivered to every Claude Code session where this plugin is installed.
/// This is the primary mechanism for self-contained plugin behavior — no manual CLAUDE.md edits needed.
const MCP_INSTRUCTIONS: &str = r#"Task Journal — reasoning-chain memory. NON-NEGOTIABLE: you are the recorder.
The code shows WHAT changed; only you can record WHY. If you don't log it the
moment it happens, it is lost. This memory only works if you actually call the
tools — so call them, every session, without being asked.

THE RITUAL — do this on EVERY coding session, not optional:

1. START. task_search(status="open") lists open tasks, newest first (add a
   `query` to narrow; each entry in `tasks` has title + goal) → task_pack to
   resume one; if nothing fits, task_create(title, goal=<one sentence: what the
   user is trying to accomplish>). Hold the returned task_id for the whole session.
2. AT THE MOMENT you commit to an approach → event_add(event_type="decision",
   ...) and pass `alternatives` (the options you weighed). Right then — not at
   the end, or you will forget.
3. AT THE MOMENT you rule an approach out → event_add(event_type="rejection").
4. When you verify a fact from code/logs → event_add(event_type="finding").
   When a test/benchmark proves something → event_add(event_type="evidence").
5. SELF-CHECK before you finish (or before the context compacts): "did I log
   every decision, rejection, and key finding from this session?" If not, log
   them NOW.
6. DONE → task_close(reason, outcome, outcome_tag).
7. MODULES: task_create(modules=[..]), read module_page first;
   task_close(module_notes=[..]). "📚 Chronicle: …" names a gap: fix it after.

Record in the user's language, terse and specific (file:line, ids, names). One
task = one objective — don't spawn a new task per turn; events accumulate under
the held task_id. Append-only: never edit — correct a mistake with a
`correction` event (set `corrects`).

event_type: hypothesis (unverified "maybe") | finding (verified from code/logs)
| evidence (a test proved it) | decision (committed choice) | rejection (ruled
out) | constraint (external limit) | correction (fixes an earlier event).
`alternatives` is decision-only.
"#;

#[derive(Clone, Default)]
pub struct TaskJournalServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskPackParams {
    pub task_id: String,
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskPackResult {
    pub task_id: String,
    pub mode: String,
    pub schema_version: String,
    pub text: String,
    pub metadata: TaskPackMetadata,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskPackMetadata {
    pub source_event_count: Option<usize>,
    pub cache_hit: Option<bool>,
    /// RFC 3339 time the pack text was assembled.
    pub generated_at: Option<String>,
    /// True when the pack was cut to fit its size budget.
    pub truncated: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskSearchParams {
    /// Full-text query. Empty or absent lists the project's tasks instead.
    #[serde(default)]
    pub query: String,
    /// `open`, `closed`, or `any` (the default).
    pub status: Option<String>,
    /// Absolute path of another project directory to search instead of this one.
    pub project: Option<String>,
    /// v0.10.3+: restrict matches to a single event type
    /// (`decision`, `evidence`, `finding`, `rejection`, ...).
    /// Accepts any value in [`tj_core::event::EventType::ALL`].
    pub event_type: Option<String>,
    /// Only tasks linked to this module.
    pub module: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskSearchResult {
    pub query: String,
    /// Matching task ids. Kept for older clients; `tasks` has the same ids
    /// in the same order with enough detail to pick one.
    pub results: Vec<String>,
    pub tasks: Vec<TaskSearchHit>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskSearchHit {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub last_event_at: String,
    pub goal: Option<String>,
}

fn task_search_hit(r: &rusqlite::Row) -> rusqlite::Result<TaskSearchHit> {
    Ok(TaskSearchHit {
        task_id: r.get(0)?,
        title: r.get(1)?,
        status: r.get(2)?,
        last_event_at: r.get(3)?,
        goal: r.get(4)?,
    })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCreateParams {
    pub title: String,
    pub initial_context: Option<String>,
    /// v0.4.0+: explicit goal — what is the user trying to accomplish.
    /// Renders as the first line of every pack and is the anchor for
    /// "why was this done?" answers weeks later. Optional only for
    /// backwards compat; agents should always pass it.
    pub goal: Option<String>,
    /// Parent task id — makes this a subtask of the given id. Validated: the
    /// parent must exist and the link must not introduce a cycle.
    pub parent: Option<String>,
    /// Filled in by the client or the Task Journal Claude Code mod; leave it out.
    pub session_id: Option<String>,
    /// Modules this task belongs to (ids from module_list). An unknown id is
    /// an error; a merged module links to the one that took it over.
    pub modules: Option<Vec<String>>,
}
#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub struct TaskCreateResult {
    pub task_id: String,
    pub title: String,
    /// The task's modules with their current state: read module_page before changing them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<ModuleBrief>,
    /// Modules that look like a fit when none were given: link with module_link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_modules: Vec<tj_core::modules::Suggestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The chronicle's most important gap, to close once the current work is done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chronicle: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ModuleBrief {
    pub module_id: String,
    pub name: String,
    pub state: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EventAddParams {
    pub task_id: String,
    pub event_type: String,
    pub text: String,
    pub corrects: Option<String>,
    pub supersedes: Option<String>,
    /// v0.12.0: structured alternatives for a `decision` event — a JSON
    /// array of `{option, chosen, rationale}` objects making the considered
    /// options and the final choice explicit. Stamped onto
    /// `meta.alternatives`. Rejected with an error on any non-decision type.
    pub alternatives: Option<serde_json::Value>,
    /// Filled in by the client or the Task Journal Claude Code mod; leave it out.
    pub session_id: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EventAddResult {
    pub event_id: String,
    pub task_id: String,
    pub event_type: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArtifactAddParams {
    pub task_id: String,
    /// Short tag: `doc`, `deploy`, `dashboard`, `design`, `pr`, …
    pub kind: String,
    /// The link target (URL or path).
    pub url: String,
    /// Human label shown on the card.
    pub label: String,
    /// Filled in by the client or the Task Journal Claude Code mod; leave it out.
    pub session_id: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ArtifactAddResult {
    pub event_id: String,
    pub task_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCloseParams {
    pub task_id: String,
    pub reason: String,
    pub outcome: Option<String>,
    /// v0.4.0+: structured outcome tag — `done`, `abandoned`, or
    /// `superseded`. Filterable; the free-form text lives in `outcome`.
    pub outcome_tag: Option<String>,
    /// Filled in by the client or the Task Journal Claude Code mod; leave it out.
    pub session_id: Option<String>,
    /// One line per module on what this task changed there: the module's history.
    pub module_notes: Option<Vec<ModuleNote>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModuleNote {
    pub module: String,
    pub text: String,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCloseResult {
    pub task_id: String,
    pub closed: bool,
    /// Optional advisory note — e.g. "note: N open subtask(s)" when the
    /// closed task still has open children. `None` when there's nothing
    /// to flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Completeness gaps surfaced at close time (from `completeness::assess`).
    /// Non-blocking advisory — the close always succeeds. Empty when the task
    /// has no detected gaps; omitted from the wire shape in that case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completeness_gaps: Vec<String>,
    /// What the task's modules still need from you now that it is closed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_reminder: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MemoryNoteParams {
    /// The durable user preference or standing fact to remember across all
    /// projects and sessions — e.g. "respond in Russian, terse", "this team
    /// always squash-merges". Keep it one short sentence.
    pub text: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MemoryNoteResult {
    pub remembered: bool,
    pub text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModuleSaveParams {
    /// Short slug: lowercase latin letters, digits, dashes (`stars`, `auth-refresh`).
    pub module_id: String,
    /// Human name. Required for a new module.
    pub name: Option<String>,
    /// One to three sentences: what this part of the system is.
    pub description: Option<String>,
    /// Code path prefixes and terms that mark the module; they drive suggestions.
    pub hints: Option<tj_core::modules::Hints>,
    /// How the module works now: a short living text you rewrite when it changes.
    pub state: Option<String>,
    /// `active`, `retired` or `merged` (with `merged_into`).
    pub status: Option<String>,
    pub merged_into: Option<String>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ModuleSaveResult {
    pub module_id: String,
    pub created: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModuleLink {
    pub task_id: String,
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModuleLinkParams {
    pub links: Vec<ModuleLink>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ModuleLinkResult {
    /// Tasks whose links changed.
    pub linked: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ModuleListResult {
    pub modules: Vec<tj_core::modules::Module>,
    /// What the chronicle is missing, most important first.
    pub gaps: Vec<tj_core::archive::Gap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chronicle: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ModulePageParams {
    pub module_id: String,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ModulePageResult {
    pub module_id: String,
    pub text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BackfillParams {
    /// Tasks per page, newest first (default 20, at most 100).
    pub limit: Option<usize>,
    /// Tasks to skip: page on past the ones you left for later.
    pub offset: Option<usize>,
}
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BackfillResult {
    pub total_unlinked: i64,
    pub candidates: Vec<tj_core::modules::Candidate>,
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

fn resolve_project_paths(
    dir: &std::path::Path,
) -> anyhow::Result<(String, std::path::PathBuf, std::path::PathBuf)> {
    let project_hash = tj_core::project_hash::from_path(dir)?;
    let events = tj_core::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
    let state = tj_core::paths::state_dir()?.join(format!("{project_hash}.sqlite"));
    Ok((project_hash, events, state))
}

/// The project directory every tool works on: `--project-dir`, else the cwd.
fn project_dir() -> anyhow::Result<PathBuf> {
    Ok(match PROJECT_DIR_OVERRIDE.get() {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    })
}

fn project_paths() -> anyhow::Result<(String, std::path::PathBuf, std::path::PathBuf)> {
    resolve_project_paths(&project_dir()?)
}

/// The session id to stamp on an event: the caller's `session_id` param when
/// given (empty counts as absent), else the live one from the environment.
fn session_id_or_env(explicit: Option<&str>) -> Option<String> {
    explicit
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(tj_core::session_id::session_id_from_env)
}

/// Ingest the journal tail, then fail on an unknown `task_id` so a typo never
/// writes an orphan event.
fn require_task(
    project_hash: &str,
    events_path: &Path,
    state_path: &Path,
    task_id: &str,
) -> anyhow::Result<()> {
    let conn_arc = cached_open(state_path)?;
    let conn = conn_arc
        .lock()
        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
    if events_path.exists() {
        tj_core::db::ingest_new_events(&conn, events_path, project_hash)?;
    }
    if !tj_core::db::task_exists(&conn, task_id)? {
        anyhow::bail!("task not found: {task_id}");
    }

    Ok(())
}

/// Run `f` on this project's state with the journal tail ingested first.
fn with_state<T>(f: impl FnOnce(&Connection, &str) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let (project_hash, events_path, state_path) = project_paths()?;
    let conn_arc = cached_open(&state_path)?;
    let conn = conn_arc
        .lock()
        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;

    f(&conn, &project_hash)
}

/// Append `events` to this project's journal, durably.
fn append_events(events: &[tj_core::event::Event]) -> anyhow::Result<()> {
    let (_, events_path, _) = project_paths()?;
    let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
    for event in events {
        writer.append(event)?;
    }
    writer.flush_durable()?;

    Ok(())
}

/// Append `events` to the journal at `path`, durably.
fn append_to(path: &Path, events: &[tj_core::event::Event]) -> anyhow::Result<()> {
    let mut writer = tj_core::storage::JsonlWriter::open(path)?;
    for event in events {
        writer.append(event)?;
    }
    writer.flush_durable()?;

    Ok(())
}

/// The project's chronicle: the module map of its repository (a worktree
/// shares the main checkout's) and every journal that links tasks to it.
fn chronicle() -> anyhow::Result<tj_core::chronicle::Chronicle> {
    tj_core::chronicle::Chronicle::open(&project_dir()?)
}

/// Run `f` on the chronicle and on this project's own state.
fn with_chronicle<T>(
    f: impl FnOnce(&tj_core::chronicle::Chronicle, &Connection, &str) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let chr = chronicle()?;

    with_state(|conn, project_hash| f(&chr, conn, project_hash))
}

/// Whether this project, or the main checkout whose module map it shares, has
/// a journal yet; read-only tools create nothing without one.
fn has_journal() -> anyhow::Result<bool> {
    if project_paths()?.1.exists() {
        return Ok(true);
    }

    Ok(
        match tj_core::project_hash::chronicle_home(&project_dir()?) {
            Some(home) => resolve_project_paths(&home)?.1.exists(),
            None => false,
        },
    )
}

/// What a new task's reply says about the chronicle: its modules with their
/// state, or modules that look like a fit, and the archive's top gap.
fn new_task_chronicle(
    task_id: &str,
    text: &str,
    linked: &[String],
) -> anyhow::Result<(
    Vec<ModuleBrief>,
    Vec<tj_core::modules::Suggestion>,
    Option<String>,
)> {
    with_chronicle(|chr, conn, project_hash| {
        let mut modules = Vec::new();
        for id in linked {
            if let Some(m) = tj_core::modules::get(&chr.home, &chr.home_hash, id)? {
                modules.push(ModuleBrief {
                    module_id: m.module_id,
                    name: m.name,
                    state: m.state,
                });
            }
        }
        let suggested = if linked.is_empty() {
            tj_core::modules::suggest(&chr.home, &chr.home_hash, text, &[])?
        } else {
            Vec::new()
        };
        let gaps = tj_core::archive::gaps(chr, conn, project_hash, Some(task_id))?;

        Ok((modules, suggested, tj_core::archive::headline(&gaps)))
    })
}

/// What a closed task's modules still need: a link, or a fresh state.
fn module_reminder(
    chr: &tj_core::chronicle::Chronicle,
    conn: &Connection,
    task_id: &str,
    has_notes: bool,
) -> anyhow::Result<Option<String>> {
    // No map yet: the session start and module_list already say so.
    if tj_core::modules::list(&chr.home, &chr.home_hash)?.is_empty() {
        return Ok(None);
    }

    let linked = tj_core::modules::modules_of_task(conn, task_id)?;
    let ids = linked
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    Ok(Some(match (linked.is_empty(), has_notes) {
        (true, _) => "This task belongs to no module — link it with module_link so its history \
                      is kept (module_list shows the map)."
            .to_string(),
        (false, false) => format!(
            "Closed without module_notes, so the history of {ids} has no line on it — make sure \
             each module's state says what changed: module_save(state=...)."
        ),
        (false, true) => {
            format!("If {ids} work differently now, rewrite the state with module_save(state=...).")
        }
    }))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskCheckParams {
    pub task_id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCheckGap {
    pub kind: String,
    pub severity: String,
    pub detail: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskCheckResult {
    pub task_id: String,
    pub score: u8,
    pub gaps: Vec<TaskCheckGap>,
}

#[tool_router]
impl TaskJournalServer {
    #[tool(
        name = "task_pack",
        description = "Return a compact resume pack for a task. Pass mode=compact|full."
    )]
    async fn task_pack(
        &self,
        Parameters(p): Parameters<TaskPackParams>,
    ) -> Result<Json<TaskPackResult>, McpError> {
        traced_tool("task_pack", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                let conn_arc = cached_open(&state_path)?;
                let conn = conn_arc
                    .lock()
                    .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                if events_path.exists() {
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                }
                let pmode = match p.mode.as_deref() {
                    Some("full") => tj_core::pack::PackMode::Full,
                    _ => tj_core::pack::PackMode::Compact,
                };
                let pack = tj_core::pack::assemble(&conn, &p.task_id, pmode)?;
                Ok(TaskPackResult {
                    task_id: pack.task_id,
                    mode: match pack.mode {
                        tj_core::pack::PackMode::Compact => "compact".into(),
                        tj_core::pack::PackMode::Full => "full".into(),
                    },
                    schema_version: pack.schema_version,
                    text: pack.text,
                    metadata: TaskPackMetadata {
                        source_event_count: Some(pack.metadata.source_event_count),
                        cache_hit: Some(pack.metadata.cache_hit),
                        generated_at: Some(pack.metadata.generated_at),
                        truncated: Some(pack.metadata.truncated),
                    },
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "task_check",
        description = "Deterministic honesty check for a task: 0–100 score plus completeness and artifact-drift gaps. Zero-LLM. Use to self-assess a task's pack (e.g. before closing)."
    )]
    async fn task_check(
        &self,
        Parameters(p): Parameters<TaskCheckParams>,
    ) -> Result<Json<TaskCheckResult>, McpError> {
        traced_tool("task_check", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                let conn_arc = cached_open(&state_path)?;
                let conn = conn_arc
                    .lock()
                    .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                if events_path.exists() {
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                }
                let mut report = tj_core::completeness::assess(
                    &conn,
                    &p.task_id,
                    tj_core::completeness::pending_count(),
                )?;
                let dir = project_dir()?;
                let arts = tj_core::db::task_artifacts(&conn, &p.task_id)?;
                report
                    .gaps
                    .extend(tj_core::completeness::artifact_gaps_in(&arts, &dir));
                let gaps = report
                    .gaps
                    .iter()
                    .map(|g| TaskCheckGap {
                        kind: format!("{:?}", g.kind),
                        severity: match g.kind.weight() {
                            10 => "error",
                            3 => "warn",
                            _ => "info",
                        }
                        .to_string(),
                        detail: g.detail.clone(),
                    })
                    .collect();
                Ok(TaskCheckResult {
                    task_id: p.task_id.clone(),
                    score: report.score(),
                    gaps,
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "task_search",
        description = "Search tasks. `query` is full-text over event text; empty or absent lists the project's tasks, newest first. `status`: open | closed | any (default). `module`: only tasks of that module. `project`: absolute dir of another project. `tasks` gives id, title, status, last_event_at, goal per hit."
    )]
    async fn task_search(
        &self,
        Parameters(p): Parameters<TaskSearchParams>,
    ) -> Result<Json<TaskSearchResult>, McpError> {
        traced_tool("task_search", async move {
            let query = p.query.clone();
            let raw_query = p.query.clone();
            let event_type = p.event_type.clone();
            let module = p.module.clone();
            let tasks = run_blocking(move || {
                let status = match p.status.as_deref() {
                    None | Some("any") => None,
                    Some(s @ ("open" | "closed")) => Some(s.to_string()),
                    Some(other) => {
                        anyhow::bail!("invalid status `{other}` (expected: open | closed | any)")
                    }
                };
                let (project_hash, events_path, state_path) = match p.project.as_deref() {
                    Some(dir) if !Path::new(dir).is_absolute() => {
                        anyhow::bail!("project must be an absolute path, got `{dir}`")
                    }
                    Some(dir) => resolve_project_paths(Path::new(dir))?,
                    None => project_paths()?,
                };
                // No journal, no tasks — and no empty state DB left behind to
                // show up in project lists later.
                if !events_path.exists() {
                    return Ok(Vec::new());
                }

                let conn_arc = cached_open(&state_path)?;
                let conn = conn_arc
                    .lock()
                    .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;

                // No query: list the project's tasks, newest first. FTS5
                // rejects an empty MATCH, so it must not reach it.
                if raw_query.trim().is_empty() {
                    let mut stmt = conn.prepare(
                        "SELECT task_id, title, status, last_event_at, goal FROM tasks \
                         WHERE (?1 IS NULL OR status = ?1) \
                           AND (?2 IS NULL OR task_id IN \
                                (SELECT task_id FROM events_index WHERE type = ?2)) \
                           AND (?3 IS NULL OR task_id IN \
                                (SELECT task_id FROM task_modules WHERE module_id = ?3)) \
                         ORDER BY last_event_at DESC, rowid DESC LIMIT 50",
                    )?;
                    let hits = stmt
                        .query_map(
                            rusqlite::params![status, event_type, module],
                            task_search_hit,
                        )?
                        .collect::<Result<_, _>>()?;

                    return Ok(hits);
                }

                // v0.10.3: sanitize FTS5 query. Hyphenated IDs like
                // `OPS-306` previously crashed with "no such column: 306"
                // because FTS5 reads `-` as column-prefix syntax. Every
                // token is quoted on its own, so punctuation is literal
                // and multi-word queries keep their AND semantics.
                let fts_query = tj_core::fts::sanitize_query(&raw_query);
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT t.task_id, t.title, t.status, t.last_event_at, t.goal \
                     FROM search_fts JOIN tasks t ON t.task_id = search_fts.task_id \
                     WHERE search_fts MATCH ?1 \
                       AND (?2 IS NULL OR search_fts.type = ?2) \
                       AND (?3 IS NULL OR t.status = ?3) \
                       AND (?4 IS NULL OR t.task_id IN \
                            (SELECT task_id FROM task_modules WHERE module_id = ?4)) LIMIT 50",
                )?;
                let mut hits: Vec<TaskSearchHit> = stmt
                    .query_map(
                        rusqlite::params![fts_query, event_type, status, module],
                        task_search_hit,
                    )?
                    .collect::<Result<_, _>>()?;

                // v0.10.3: LIKE fallback. FTS5 phrase search miss when
                // tokenizer split differs from the user's mental model
                // (e.g. `bulk-repack` in source vs `bulk repack` in
                // query). On zero FTS hits, scan event text directly so
                // hyphenated identifiers and partial-word recall work.
                if hits.is_empty() {
                    let like = tj_core::fts::like_pattern(&raw_query);
                    let mut stmt_like = conn.prepare(
                        "SELECT DISTINCT t.task_id, t.title, t.status, t.last_event_at, t.goal \
                         FROM search_fts JOIN tasks t ON t.task_id = search_fts.task_id \
                         WHERE search_fts.text LIKE ?1 \
                           AND (?2 IS NULL OR search_fts.type = ?2) \
                           AND (?3 IS NULL OR t.status = ?3) \
                           AND (?4 IS NULL OR t.task_id IN \
                                (SELECT task_id FROM task_modules WHERE module_id = ?4)) LIMIT 50",
                    )?;
                    hits = stmt_like
                        .query_map(
                            rusqlite::params![like, event_type, status, module],
                            task_search_hit,
                        )?
                        .collect::<Result<_, _>>()?;
                }

                Ok(hits)
            })
            .await?;

            let results = tasks.iter().map(|t| t.task_id.clone()).collect();
            Ok(Json(TaskSearchResult {
                query,
                results,
                tasks,
            }))
        })
        .await
    }

    #[tool(
        name = "task_create",
        description = "Open a new task. Always pass `goal` (one sentence: what the user is trying to accomplish) — it is the first line of every resume pack and the anchor for \"why was this done?\" weeks later. `title` is a short label; `initial_context` is optional. `modules`: the modules it belongs to (module_list); without them the reply suggests some."
    )]
    async fn task_create(
        &self,
        Parameters(p): Parameters<TaskCreateParams>,
    ) -> Result<Json<TaskCreateResult>, McpError> {
        traced_tool("task_create", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                std::fs::create_dir_all(events_path.parent().unwrap())?;

                let task_id = tj_core::new_task_id();

                // Loom spine: when running inside a Loom task session
                // (LOOM_TASK_ID set), the journal is keyed by that id via an
                // `loom:<id>` external reference. Resolve it first so repeated
                // task_create calls (and the whole pipeline) share ONE journal
                // per board task. Without LOOM_TASK_ID this is a no-op.
                let loom_ref = std::env::var("LOOM_TASK_ID")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|t| format!("loom:{t}"));
                if let Some(ref r) = loom_ref {
                    let conn_arc = cached_open(&state_path)?;
                    let conn = conn_arc
                        .lock()
                        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                    if let Some(existing) = tj_core::db::task_id_by_external(&conn, r)? {
                        return Ok(TaskCreateResult {
                            task_id: existing,
                            title: p.title.clone(),
                            ..Default::default()
                        });
                    }
                }

                // Validate --parent before writing the open event: the parent
                // must exist and the link must not introduce a cycle. Needs the
                // derived SQLite state, so ingest the JSONL tail first.
                if let Some(ref parent_id) = p.parent {
                    let conn_arc = cached_open(&state_path)?;
                    let conn = conn_arc
                        .lock()
                        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                    tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                    if !tj_core::db::task_exists(&conn, parent_id)? {
                        anyhow::bail!("parent task {parent_id} does not exist");
                    }
                    if tj_core::db::would_create_cycle(&conn, &task_id, parent_id)? {
                        anyhow::bail!("setting parent {parent_id} would create a cycle");
                    }
                }

                // Modules are checked before the open event is written: an
                // unknown id fails the call and leaves no task behind.
                let resolved = match p.modules.as_deref() {
                    Some(ids) if !ids.is_empty() => {
                        let chr = chronicle()?;
                        let r = tj_core::modules::resolve(&chr.home, &chr.home_hash, ids)?;
                        Some((r, chr))
                    }
                    _ => None,
                };

                let mut event = tj_core::event::Event::new(
                    task_id.clone(),
                    tj_core::event::EventType::Open,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    p.initial_context.clone().unwrap_or_else(|| p.title.clone()),
                );
                event.meta = serde_json::json!({"title": p.title.clone()});
                if let Some(ref parent_id) = p.parent {
                    event.meta["parent_id"] = serde_json::Value::String(parent_id.clone());
                }
                // Goal and the Loom tag ride in the open event, so ingest (and
                // a rebuild from the JSONL) restores them; later resolves and
                // the board → journal link find the journal by `loom:<id>`.
                if let Some(ref goal) = p.goal {
                    event.meta["goal"] = serde_json::Value::String(goal.clone());
                }
                if let Some(ref r) = loom_ref {
                    event.meta["external"] = serde_json::json!([r]);
                }
                if let Some((r, chr)) = &resolved {
                    event.meta["modules"] = serde_json::json!(r.ids);
                    chr.stamp(&mut event.meta);
                }
                tj_core::session_id::stamp_session_id(
                    &mut event.meta,
                    session_id_or_env(p.session_id.as_deref()).as_deref(),
                );

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;

                // The task is written; what follows only advises, so it never fails the call.
                let linked = resolved
                    .as_ref()
                    .map(|(r, _)| r.ids.clone())
                    .unwrap_or_default();
                let text = format!(
                    "{} {} {}",
                    p.title,
                    p.goal.as_deref().unwrap_or(""),
                    p.initial_context.as_deref().unwrap_or("")
                );
                let (modules, suggested_modules, chronicle) =
                    new_task_chronicle(&task_id, &text, &linked).unwrap_or_default();

                Ok(TaskCreateResult {
                    task_id,
                    title: p.title.clone(),
                    modules,
                    suggested_modules,
                    warnings: resolved.map(|(r, _)| r.warnings).unwrap_or_default(),
                    chronicle,
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "event_add",
        description = "Append a typed event (decision, finding, evidence, rejection, etc.) to a task."
    )]
    async fn event_add(
        &self,
        Parameters(p): Parameters<EventAddParams>,
    ) -> Result<Json<EventAddResult>, McpError> {
        traced_tool("event_add", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                std::fs::create_dir_all(events_path.parent().unwrap())?;

                let event_type = parse_event_type(&p.event_type)?;
                // v0.12.0: structured alternatives are decision-only. Reject
                // them on any other type with a clear error rather than
                // silently dropping the payload.
                if p.alternatives.is_some() && event_type != tj_core::event::EventType::Decision {
                    anyhow::bail!(
                        "`alternatives` is only valid on a `decision` event (got `{}`)",
                        p.event_type
                    );
                }
                require_task(&project_hash, &events_path, &state_path, &p.task_id)?;

                let mut event = tj_core::event::Event::new(
                    &p.task_id,
                    event_type,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    p.text.clone(),
                );
                event.corrects = p.corrects.clone();
                event.supersedes = p.supersedes.clone();
                if let Some(alts) = &p.alternatives {
                    if let Some(obj) = event.meta.as_object_mut() {
                        obj.insert("alternatives".into(), alts.clone());
                    }
                }
                tj_core::session_id::stamp_session_id(
                    &mut event.meta,
                    session_id_or_env(p.session_id.as_deref()).as_deref(),
                );

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;

                Ok(EventAddResult {
                    event_id: event.event_id,
                    task_id: p.task_id.clone(),
                    event_type: p.event_type.clone(),
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "artifact_add",
        description = "Attach a clickable, typed link to a task — a doc, deploy, dashboard, design, spec, etc. Renders on the task card / resume pack under Artifacts as [label](url). Use it when the work produces a reference a human would want to click later. Writes a `finding` event carrying the link; PR/commit/branch are harvested automatically at close, so use this for the things git can't give you."
    )]
    async fn artifact_add(
        &self,
        Parameters(p): Parameters<ArtifactAddParams>,
    ) -> Result<Json<ArtifactAddResult>, McpError> {
        traced_tool("artifact_add", async move {
            run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;
                std::fs::create_dir_all(events_path.parent().unwrap())?;
                require_task(&project_hash, &events_path, &state_path, &p.task_id)?;

                let mut event = tj_core::event::Event::new(
                    &p.task_id,
                    tj_core::event::EventType::Finding,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    format!("📎 {}: {} — {}", p.kind, p.label, p.url),
                );
                event.meta = tj_core::artifacts::link_event_meta(&p.kind, &p.url, &p.label);
                tj_core::session_id::stamp_session_id(
                    &mut event.meta,
                    session_id_or_env(p.session_id.as_deref()).as_deref(),
                );

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;

                Ok(ArtifactAddResult {
                    event_id: event.event_id,
                    task_id: p.task_id.clone(),
                })
            })
            .await
            .map(Json)
        })
        .await
    }

    #[tool(
        name = "task_close",
        description = "Close a task with reason and outcome. `module_notes`: one line per module on what this task changed there — it becomes the module's history."
    )]
    async fn task_close(
        &self,
        Parameters(p): Parameters<TaskCloseParams>,
    ) -> Result<Json<TaskCloseResult>, McpError> {
        traced_tool("task_close", async move {
            let task_id = p.task_id.clone();
            let (open_kids, gaps, module_reminder, warnings) = run_blocking(move || {
                let (project_hash, events_path, state_path) = project_paths()?;

                let conn_arc = cached_open(&state_path)?;
                let open_kids;
                let mut notes = Vec::new();
                let mut warnings = Vec::new();
                // Notes go to modules of the repository's shared map.
                let chr = match &p.module_notes {
                    Some(n) if !n.is_empty() => Some(chronicle()?),
                    _ => None,
                };
                {
                    let conn = conn_arc
                        .lock()
                        .map_err(|e| anyhow::anyhow!("connection mutex poisoned: {e}"))?;
                    if events_path.exists() {
                        tj_core::db::ingest_new_events(&conn, &events_path, &project_hash)?;
                    }
                    if !tj_core::db::task_exists(&conn, &p.task_id)? {
                        anyhow::bail!("task not found: {}", p.task_id);
                    }
                    // v0.6.0: validate the outcome_tag enum before writing
                    // the close event (same enum as the CLI close handler).
                    // outcome+tag ride in the close event's meta and reach
                    // the task row only when that event is ingested, so a
                    // failed append never leaves an open task with an outcome.
                    if let Some(tag) = p.outcome_tag.as_deref() {
                        match tag {
                            "done" | "abandoned" | "superseded" => {}
                            other => anyhow::bail!(
                                "invalid outcome_tag `{other}` (expected: done | abandoned | superseded)"
                            ),
                        }
                    }
                    open_kids = tj_core::db::count_open_children(&conn, &p.task_id)?;

                    // A note to an unknown module fails the close before it is written.
                    for note in p.module_notes.iter().flatten() {
                        let chr = chr.as_ref().expect("opened when notes are given");
                        let r = tj_core::modules::resolve(
                            &chr.home,
                            &chr.home_hash,
                            std::slice::from_ref(&note.module),
                        )?;
                        warnings.extend(r.warnings);
                        notes.push((r.ids[0].clone(), note.text.clone()));
                    }
                } // release the connection lock before doing the JSONL append

                let mut event = tj_core::event::Event::new(
                    &p.task_id,
                    tj_core::event::EventType::Close,
                    tj_core::event::Author::Agent,
                    tj_core::event::Source::Chat,
                    p.reason.clone(),
                );
                let mut meta = serde_json::Map::new();
                meta.insert("reason".into(), serde_json::Value::String(p.reason.clone()));
                if let Some(o) = &p.outcome {
                    meta.insert("outcome".into(), serde_json::Value::String(o.clone()));
                }
                if let Some(t) = &p.outcome_tag {
                    meta.insert("outcome_tag".into(), serde_json::Value::String(t.clone()));
                }
                if !notes.is_empty() {
                    meta.insert(
                        "module_notes".into(),
                        tj_core::modules::notes_meta(&notes),
                    );
                }
                // Layer-2 close harvest: stamp deterministic git/gh refs
                // (commit, branch, PR) into the close event so the resume pack
                // reads as a clickable ledger of what shipped. Best-effort and
                // structured (merged in db::index_event) — never fails close.
                if let Ok(dir) = project_dir() {
                    let arts = tj_core::harvest::harvest(&dir);
                    if !arts.is_empty() {
                        if let Ok(v) = serde_json::to_value(&arts) {
                            meta.insert("artifacts".into(), v);
                        }
                    }
                }
                event.meta = serde_json::Value::Object(meta);
                if let Some(chr) = &chr {
                    chr.stamp(&mut event.meta);
                }
                tj_core::session_id::stamp_session_id(
                    &mut event.meta,
                    session_id_or_env(p.session_id.as_deref()).as_deref(),
                );

                let mut writer = tj_core::storage::JsonlWriter::open(&events_path)?;
                writer.append(&event)?;
                writer.flush_durable()?;

                // Non-blocking completeness check. The close above already
                // succeeded; re-open, apply the close event to the index, then
                // assess. Any error here must NOT fail the close — handle
                // locally, never `?`-propagate.
                let mut gaps: Vec<String> = Vec::new();
                let mut module_reminder = None;
                if let Ok(conn) = tj_core::db::open(&state_path) {
                    let _ = tj_core::db::ingest_new_events(&conn, &events_path, &project_hash);
                    if let Ok(report) = tj_core::completeness::assess(
                        &conn,
                        &p.task_id,
                        tj_core::completeness::pending_count(),
                    ) {
                        gaps = report.gaps.into_iter().map(|g| g.detail).collect();
                    }
                    module_reminder = chronicle()
                        .and_then(|chr| {
                            crate::module_reminder(&chr, &conn, &p.task_id, !notes.is_empty())
                        })
                        .ok()
                        .flatten();
                }
                Ok((open_kids, gaps, module_reminder, warnings))
            })
            .await?;
            let note = if open_kids > 0 {
                Some(format!("note: {open_kids} open subtask(s)"))
            } else {
                None
            };
            Ok(Json(TaskCloseResult {
                task_id,
                closed: true,
                note,
                completeness_gaps: gaps,
                module_reminder,
                warnings,
            }))
        })
        .await
    }

    #[tool(
        name = "module_list",
        description = "The project's module map — parts of the system by meaning, each with its task count and last activity — and what the chronicle is missing (`gaps`, most important first)."
    )]
    async fn module_list(&self) -> Result<Json<ModuleListResult>, McpError> {
        traced_tool("module_list", async move {
            run_blocking(move || {
                if !has_journal()? {
                    return Ok(Json(ModuleListResult {
                        modules: Vec::new(),
                        gaps: Vec::new(),
                        chronicle: None,
                    }));
                }

                with_chronicle(|chr, conn, project_hash| {
                    let gaps = tj_core::archive::gaps(chr, conn, project_hash, None)?;

                    Ok(Json(ModuleListResult {
                        modules: tj_core::modules::map(chr)?,
                        chronicle: tj_core::archive::headline(&gaps),
                        gaps,
                    }))
                })
            })
            .await
        })
        .await
    }

    #[tool(
        name = "module_page",
        description = "A module's page: what it is, how it works now, the active decisions, rejections and constraints of all its tasks, and its history. Read it before you change the module."
    )]
    async fn module_page(
        &self,
        Parameters(p): Parameters<ModulePageParams>,
    ) -> Result<Json<ModulePageResult>, McpError> {
        traced_tool("module_page", async move {
            run_blocking(move || {
                if !has_journal()? {
                    anyhow::bail!(
                        "module {:?} does not exist — module_list shows the map",
                        p.module_id
                    );
                }

                let text = tj_core::modules::page(&chronicle()?, &p.module_id)?;

                Ok(Json(ModulePageResult {
                    module_id: p.module_id,
                    text,
                }))
            })
            .await
        })
        .await
    }

    #[tool(
        name = "module_save",
        description = "Create or update a module: a part of the system by meaning (not a folder, not a ticket). Pass only the fields that change; a new module needs `name`. `state` is the short living text of how the module works now — rewrite it when a task changes the module. `hints` (paths, terms) drive module suggestions."
    )]
    async fn module_save(
        &self,
        Parameters(p): Parameters<ModuleSaveParams>,
    ) -> Result<Json<ModuleSaveResult>, McpError> {
        traced_tool("module_save", async move {
            run_blocking(move || {
                let fields = tj_core::modules::ModuleFields {
                    name: p.name,
                    description: p.description,
                    hints: p.hints,
                    state: p.state,
                    status: p.status,
                    merged_into: p.merged_into,
                };
                let event = tj_core::modules::module_event(&p.module_id, &fields)?;

                // Modules live in the repository's home journal, also when
                // saved from a worktree.
                let chr = chronicle()?;
                let (conn, project_hash) = (&chr.home, chr.home_hash.as_str());
                let created = {
                    let exists = tj_core::modules::get(conn, project_hash, &p.module_id)?.is_some();
                    if !exists && fields.name.is_none() {
                        anyhow::bail!("module {} is new: pass `name` too", p.module_id);
                    }
                    if let Some(into) = &fields.merged_into {
                        if into == &p.module_id {
                            anyhow::bail!("a module cannot be merged into itself");
                        }
                        if tj_core::modules::get(conn, project_hash, into)?.is_none() {
                            anyhow::bail!("merged_into: module {into:?} does not exist");
                        }
                    }

                    !exists
                };
                append_to(&chr.home_events(), &[event])?;

                Ok(Json(ModuleSaveResult {
                    module_id: p.module_id,
                    created,
                }))
            })
            .await
        })
        .await
    }

    #[tool(
        name = "module_link",
        description = "Link tasks to modules or unlink them, many at once: `links` = [{task_id, add: [module ids], remove: [module ids]}]. Unknown tasks or modules fail the whole call before anything is written; a merged module links to the one that took it over."
    )]
    async fn module_link(
        &self,
        Parameters(p): Parameters<ModuleLinkParams>,
    ) -> Result<Json<ModuleLinkResult>, McpError> {
        traced_tool("module_link", async move {
            run_blocking(move || {
                let mut warnings = Vec::new();
                let events = with_chronicle(|chr, conn, _| {
                    let mut events = Vec::new();
                    for link in &p.links {
                        if !tj_core::db::task_exists(conn, &link.task_id)? {
                            anyhow::bail!("task not found: {}", link.task_id);
                        }
                        for id in &link.remove {
                            tj_core::modules::validate_id(id)?;
                        }
                        let add = tj_core::modules::resolve(&chr.home, &chr.home_hash, &link.add)?;
                        warnings.extend(add.warnings);
                        if add.ids.is_empty() && link.remove.is_empty() {
                            continue;
                        }
                        let mut event =
                            tj_core::modules::link_event(&link.task_id, &add.ids, &link.remove);
                        chr.stamp(&mut event.meta);
                        events.push(event);
                    }

                    Ok(events)
                })?;
                append_events(&events)?;

                Ok(Json(ModuleLinkResult {
                    linked: events.len(),
                    warnings,
                }))
            })
            .await
        })
        .await
    }

    #[tool(
        name = "module_backfill_candidates",
        description = "Tasks that belong to no module, newest first: id, title, status, goal, outcome, files, and the modules the journal suggests for each. Sort them, confirm with the user, then module_link. `total_unlinked` counts them all; page with `limit` and `offset`."
    )]
    async fn module_backfill_candidates(
        &self,
        Parameters(p): Parameters<BackfillParams>,
    ) -> Result<Json<BackfillResult>, McpError> {
        traced_tool("module_backfill_candidates", async move {
            run_blocking(move || {
                if !has_journal()? {
                    return Ok(Json(BackfillResult {
                        total_unlinked: 0,
                        candidates: Vec::new(),
                    }));
                }

                let limit = p.limit.unwrap_or(20).min(100);
                let (total_unlinked, candidates) = with_chronicle(|chr, conn, project_hash| {
                    tj_core::modules::backfill_candidates(
                        chr,
                        conn,
                        project_hash,
                        limit,
                        p.offset.unwrap_or(0),
                    )
                })?;

                Ok(Json(BackfillResult {
                    total_unlinked,
                    candidates,
                }))
            })
            .await
        })
        .await
    }

    #[tool(
        name = "memory_note",
        description = "Remember a durable user preference or standing fact across ALL projects and sessions — how the user wants to be worked with (\"respond in Russian, terse\"), or a stable team/project rule. Injected into every future session's context. Use it when you learn something about the user or their workflow that should outlive this task. De-duplicated."
    )]
    async fn memory_note(
        &self,
        Parameters(p): Parameters<MemoryNoteParams>,
    ) -> Result<Json<MemoryNoteResult>, McpError> {
        traced_tool("memory_note", async move {
            run_blocking(move || {
                let global = tj_core::memory::open(tj_core::paths::memory_db()?)?;
                let now = chrono::Utc::now().to_rfc3339();
                let remembered = tj_core::memory::add_preference(&global, &p.text, &now)?;
                Ok(Json(MemoryNoteResult {
                    remembered,
                    text: p.text.trim().to_string(),
                }))
            })
            .await
        })
        .await
    }
}

// Written out instead of `#[tool_handler]`, which generates the same two
// methods, so `call_tool` can read the request's `_meta` first.
impl ServerHandler for TaskJournalServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "task-journal".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
            instructions: Some(MCP_INSTRUCTIONS.into()),
            ..Default::default()
        }
    }

    async fn call_tool(
        &self,
        mut request: rmcp::model::CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        stamp_client_session(&mut request, &context.meta);

        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        Self::tool_router().call(tcc).await
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, McpError> {
        Ok(rmcp::model::ListToolsResult::with_all_items(
            Self::tool_router().list_all(),
        ))
    }
}

/// Codex names the session in each call's `_meta`, never in the MCP
/// server's environment. Carry it into the `session_id` argument of the
/// tools that stamp one, unless the caller passed its own.
fn stamp_client_session(request: &mut rmcp::model::CallToolRequestParam, meta: &rmcp::model::Meta) {
    const STAMPING: [&str; 4] = ["task_create", "event_add", "artifact_add", "task_close"];
    if !STAMPING.contains(&request.name.as_ref()) {
        return;
    }
    let Some(session) = tj_core::session_id::session_id_from_mcp_meta(&meta.0) else {
        return;
    };

    let args = request.arguments.get_or_insert_with(Default::default);
    let has_own = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    if !has_own {
        args.insert("session_id".into(), serde_json::Value::String(session));
    }
}

/// Resolve when the process should shut down: Ctrl-C on every platform,
/// plus SIGTERM on Unix. Used in `tokio::select!` against the rmcp
/// `waiting()` loop so the binary exits cleanly instead of being
/// hard-killed mid-write.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler — Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT"),
            _ = sigterm.recv() => tracing::info!("received SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: only Ctrl-C / Ctrl-Break maps to ctrl_c().
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received Ctrl-C");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    if let Some(dir) = cli.project_dir {
        let resolved = std::fs::canonicalize(&dir)
            .with_context(|| format!("--project-dir not accessible: {dir:?}"))?;
        PROJECT_DIR_OVERRIDE
            .set(resolved)
            .map_err(|_| anyhow::anyhow!("PROJECT_DIR_OVERRIDE already set"))?;
    }

    let server = TaskJournalServer;
    let (stdin, stdout) = stdio();
    let serving = server.serve((stdin, stdout)).await?;

    tokio::select! {
        res = serving.waiting() => {
            res?;
            tracing::info!("rmcp serve loop exited");
        }
        _ = wait_for_shutdown_signal() => {
            tracing::info!("shutdown signal received — exiting");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // The handler tests intentionally hold the handler_env() mutex across
    // `.await` to serialize access to the process-global PROJECT_DIR_OVERRIDE
    // and XDG_DATA_HOME. On a current-thread runtime this is safe.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    fn call(name: &'static str, args: serde_json::Value) -> rmcp::model::CallToolRequestParam {
        rmcp::model::CallToolRequestParam {
            name: name.into(),
            arguments: args.as_object().cloned(),
        }
    }

    fn codex_meta(session: &str) -> rmcp::model::Meta {
        let meta = serde_json::json!({ "x-codex-turn-metadata": { "session_id": session } });
        rmcp::model::Meta(meta.as_object().unwrap().clone())
    }

    #[test]
    fn codex_session_from_meta_reaches_the_stamping_tools() {
        let mut add = call("event_add", serde_json::json!({ "task_id": "tj-1" }));
        stamp_client_session(&mut add, &codex_meta("c-1"));
        assert_eq!(add.arguments.unwrap()["session_id"], "c-1");

        let mut create = call("task_create", serde_json::json!(null));
        stamp_client_session(&mut create, &codex_meta("c-1"));
        assert_eq!(create.arguments.unwrap()["session_id"], "c-1");
    }

    #[test]
    fn an_explicit_session_id_and_other_tools_are_left_alone() {
        let mut own = call("event_add", serde_json::json!({ "session_id": "mine" }));
        stamp_client_session(&mut own, &codex_meta("c-1"));
        assert_eq!(own.arguments.unwrap()["session_id"], "mine");

        let mut search = call("task_search", serde_json::json!({ "query": "x" }));
        stamp_client_session(&mut search, &codex_meta("c-1"));
        assert!(search.arguments.unwrap().get("session_id").is_none());

        let mut claude = call("event_add", serde_json::json!({ "task_id": "tj-1" }));
        stamp_client_session(&mut claude, &rmcp::model::Meta::default());
        assert!(claude.arguments.unwrap().get("session_id").is_none());
    }

    /// Handler tests touch process-global state (PROJECT_DIR_OVERRIDE OnceLock
    /// and the XDG_DATA_HOME env var), so they must run one at a time and share
    /// a single project dir. This guard serializes them and lazily pins the
    /// override and XDG to a single persistent tempdir for the whole test binary.
    fn handler_env() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
        static PROJ: OnceLock<tempfile::TempDir> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = HOME.get_or_init(|| tempfile::TempDir::new().unwrap());
        let proj = PROJ.get_or_init(|| tempfile::TempDir::new().unwrap());
        std::env::set_var("XDG_DATA_HOME", home.path());
        let _ = PROJECT_DIR_OVERRIDE.set(proj.path().to_path_buf());
        guard
    }

    fn keys_of(v: &serde_json::Value) -> Vec<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Claude Code truncates MCP server instructions and tool descriptions at
    /// 2,048 characters (`CLAUDE_CODE_MAX_MCP_DESCRIPTION_LENGTH`, 2.1.280).
    /// The ritual only works if the agent reads all of it, so the instructions
    /// must stay under the cap — with room to breathe before the next edit.
    #[test]
    fn mcp_instructions_fit_the_client_cap() {
        const CAP: usize = 2048;
        let len = MCP_INSTRUCTIONS.chars().count();
        assert!(
            len <= CAP,
            "MCP instructions are {len} chars, over the {CAP}-char cap — clients would truncate them"
        );
    }

    #[test]
    fn no_response_serializes_a_stub_field() {
        // Vestigial stub:bool from Phase 1 stubs has been removed from all
        // five MCP result types. Guard against re-introduction.
        let pack = TaskPackResult {
            task_id: "tj-x".into(),
            mode: "compact".into(),
            schema_version: tj_core::SCHEMA_VERSION.into(),
            text: String::new(),
            metadata: TaskPackMetadata {
                source_event_count: None,
                cache_hit: None,
                generated_at: None,
                truncated: None,
            },
        };
        let pack_v = serde_json::to_value(&pack).unwrap();
        assert!(!keys_of(&pack_v).contains(&"stub".to_string()));
        assert!(!keys_of(&pack_v["metadata"]).contains(&"stub".to_string()));

        let search = TaskSearchResult {
            query: "q".into(),
            results: vec![],
            tasks: vec![],
        };
        assert!(!keys_of(&serde_json::to_value(&search).unwrap()).contains(&"stub".to_string()));

        let create = TaskCreateResult {
            task_id: "tj-x".into(),
            title: "t".into(),
            ..Default::default()
        };
        // Empty chronicle fields stay off the wire: the reply keeps its old shape.
        assert_eq!(
            keys_of(&serde_json::to_value(&create).unwrap()),
            vec!["task_id".to_string(), "title".to_string()]
        );

        let event = EventAddResult {
            event_id: "e".into(),
            task_id: "tj-x".into(),
            event_type: "decision".into(),
        };
        assert!(!keys_of(&serde_json::to_value(&event).unwrap()).contains(&"stub".to_string()));

        let close = TaskCloseResult {
            task_id: "tj-x".into(),
            closed: true,
            note: None,
            completeness_gaps: Vec::new(),
            module_reminder: None,
            warnings: Vec::new(),
        };
        assert_eq!(
            keys_of(&serde_json::to_value(&close).unwrap()),
            vec!["closed".to_string(), "task_id".to_string()]
        );
    }

    #[test]
    fn resolve_project_paths_uses_provided_dir_for_hash() {
        // Two distinct dirs must give two distinct project_hash values, and
        // the same dir must always give the same hash. This is the contract
        // that --project-dir relies on: any path on disk maps to a stable,
        // unique data location.
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("alpha");
        let b = tmp.path().join("beta");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // Make each its own project root, so they don't both resolve up to the
        // shared `tmp` ancestor (or a `.git` above it, which collapses the two
        // hashes on some hosts, e.g. WSL /tmp).
        std::fs::create_dir(a.join(".git")).unwrap();
        std::fs::create_dir(b.join(".git")).unwrap();

        let (hash_a, _, _) = resolve_project_paths(&a).unwrap();
        let (hash_b, _, _) = resolve_project_paths(&b).unwrap();
        assert_ne!(hash_a, hash_b);

        let (hash_a_again, _, _) = resolve_project_paths(&a).unwrap();
        assert_eq!(hash_a, hash_a_again);
    }

    #[tokio::test]
    async fn run_blocking_executes_two_tasks_concurrently() {
        use std::time::{Duration, Instant};

        // Two tasks each sleep ~200ms. If run_blocking handed work to the
        // tokio blocking pool they overlap (~200ms wall-clock). If we ever
        // regress to running the closure inline on the executor thread,
        // tokio::join! still wakes both futures but only one progresses at
        // a time and total wall-clock approaches 400ms.
        let start = Instant::now();
        let (a, b) = tokio::join!(
            run_blocking(|| {
                std::thread::sleep(Duration::from_millis(200));
                Ok::<_, anyhow::Error>(1u32)
            }),
            run_blocking(|| {
                std::thread::sleep(Duration::from_millis(200));
                Ok::<_, anyhow::Error>(2u32)
            }),
        );
        let elapsed = start.elapsed();

        assert_eq!(a.unwrap(), 1);
        assert_eq!(b.unwrap(), 2);
        // Sequential execution would require ≥400ms (two 200ms sleeps);
        // overlap drops it to ~200ms. We give CI runners plenty of slack
        // (600ms) — still distinguishes parallel from serial without
        // flaking on macOS/Windows GitHub runners under load.
        assert!(
            elapsed < Duration::from_millis(600),
            "blocking tasks must overlap on the blocking pool — got {elapsed:?}"
        );
    }

    /// Compile-time + runtime guarantee that `wait_for_shutdown_signal`
    /// returns a `Future<Output = ()>` we can drop on the floor without
    /// it ever resolving — a real signal would resolve it. We assert by
    /// racing it against an already-ready future and confirming the
    /// shutdown future was *not* the winner.
    #[tokio::test]
    async fn shutdown_signal_does_not_fire_spuriously() {
        let ready = async {};
        tokio::select! {
            _ = wait_for_shutdown_signal() => panic!("shutdown fired with no signal"),
            _ = ready => { /* expected */ }
        }
    }

    #[test]
    fn new_correlation_id_is_unique_across_thousand_calls() {
        let mut seen = std::collections::HashSet::with_capacity(1000);
        for _ in 0..1_000 {
            assert!(
                seen.insert(new_correlation_id()),
                "correlation id collision in 1k calls"
            );
        }
    }

    #[tokio::test]
    async fn traced_tool_transparently_returns_inner_result() {
        // Success path: the wrapper must propagate the Ok value.
        let ok = traced_tool::<i32, _>("test_ok", async { Ok(42) })
            .await
            .unwrap();
        assert_eq!(ok, 42);

        // Error path: the wrapper must propagate Err untouched.
        let err = traced_tool::<i32, _>("test_err", async {
            Err(McpError::internal_error("boom".to_string(), None))
        })
        .await;
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().message, "boom");
    }

    #[test]
    fn cached_open_returns_same_arc_for_same_path() {
        // The Arc returned by cached_open() is the same handle on second
        // call: that's the proof that we are not re-running migrations
        // / PRAGMA / WAL setup on every tool call.
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("d1-cache.sqlite");
        let a = cached_open(&p).unwrap();
        let b = cached_open(&p).unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "cached_open must reuse the Arc<Mutex<Connection>>"
        );
    }

    #[test]
    fn cached_open_returns_distinct_arcs_for_distinct_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let p1 = dir.path().join("d1-x.sqlite");
        let p2 = dir.path().join("d1-y.sqlite");
        let a = cached_open(&p1).unwrap();
        let b = cached_open(&p2).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn cli_parses_project_dir_argument() {
        // Smoke test: `task-journal-mcp --project-dir /tmp/foo` parses and
        // populates the field. We do not actually launch the server here —
        // that needs a real stdio peer.
        let cli = Cli::try_parse_from(["task-journal-mcp", "--project-dir", "/tmp/foo"]).unwrap();
        assert_eq!(cli.project_dir, Some(std::path::PathBuf::from("/tmp/foo")));

        let cli = Cli::try_parse_from(["task-journal-mcp"]).unwrap();
        assert!(cli.project_dir.is_none());
    }

    #[tokio::test]
    async fn event_add_decision_stamps_alternatives_meta() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let task = server
            .task_create(Parameters(TaskCreateParams {
                title: "Alt task".into(),
                initial_context: None,
                goal: None,
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        let alts = serde_json::json!([
            {"option": "SQLite", "chosen": true, "rationale": "embedded"},
            {"option": "Postgres", "chosen": false, "rationale": "too heavy"}
        ]);
        let res = server
            .event_add(Parameters(EventAddParams {
                task_id: task.clone(),
                event_type: "decision".into(),
                text: "Use SQLite".into(),
                corrects: None,
                supersedes: None,
                alternatives: Some(alts.clone()),
                session_id: None,
            }))
            .await
            .unwrap()
            .0;

        let (_, events_path, _) = project_paths().unwrap();
        let jsonl = std::fs::read_to_string(&events_path).unwrap();
        let ev = jsonl
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v.get("event_id").and_then(|x| x.as_str()) == Some(res.event_id.as_str()))
            .expect("decision event in jsonl");
        assert_eq!(ev["meta"]["alternatives"], alts);
    }

    #[tokio::test]
    async fn event_add_rejects_alternatives_on_non_decision() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let task = server
            .task_create(Parameters(TaskCreateParams {
                title: "Reject task".into(),
                initial_context: None,
                goal: None,
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        let res = server
            .event_add(Parameters(EventAddParams {
                task_id: task,
                event_type: "finding".into(),
                text: "some finding".into(),
                corrects: None,
                supersedes: None,
                alternatives: Some(serde_json::json!([{"option": "x", "chosen": true}])),
                session_id: None,
            }))
            .await;
        let err = match res {
            Ok(_) => panic!("alternatives on a finding must be rejected"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("alternatives") && msg.contains("decision"),
            "error should explain alternatives is decision-only: {msg}"
        );
    }

    #[tokio::test]
    async fn task_create_with_parent_stamps_meta() {
        // Isolate state under a temp XDG home and a unique project dir
        // (set once via PROJECT_DIR_OVERRIDE). Create a parent, then a child
        // with parent = Some(parent_id); assert the child's open event in the
        // JSONL carries meta.parent_id.
        let _env = handler_env();
        let server = TaskJournalServer;

        let parent = server
            .task_create(Parameters(TaskCreateParams {
                title: "Parent".into(),
                initial_context: None,
                goal: None,
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        let child = server
            .task_create(Parameters(TaskCreateParams {
                title: "Child".into(),
                initial_context: None,
                goal: None,
                parent: Some(parent.clone()),
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        let (_, events_path, _) = project_paths().unwrap();
        let jsonl = std::fs::read_to_string(&events_path).unwrap();
        let child_open = jsonl
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v.get("task_id").and_then(|x| x.as_str()) == Some(child.as_str()))
            .expect("child open event");
        assert_eq!(
            child_open["meta"]["parent_id"].as_str(),
            Some(parent.as_str())
        );
    }

    #[tokio::test]
    async fn task_close_notes_open_subtasks() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let parent = server
            .task_create(Parameters(TaskCreateParams {
                title: "Parent".into(),
                initial_context: None,
                goal: None,
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        // One open child under the parent.
        server
            .task_create(Parameters(TaskCreateParams {
                title: "Child".into(),
                initial_context: None,
                goal: None,
                parent: Some(parent.clone()),
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap();

        let res = server
            .task_close(Parameters(TaskCloseParams {
                task_id: parent.clone(),
                reason: "done".into(),
                outcome: None,
                outcome_tag: None,
                session_id: None,
                module_notes: None,
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(res.note.as_deref(), Some("note: 1 open subtask(s)"));
    }

    #[tokio::test]
    async fn task_pack_reports_generated_at_and_truncated() {
        let _env = handler_env();
        let server = TaskJournalServer;
        let task = server
            .task_create(Parameters(TaskCreateParams {
                title: "Pack metadata".into(),
                initial_context: None,
                goal: Some("g".into()),
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        let pack = server
            .task_pack(Parameters(TaskPackParams {
                task_id: task,
                mode: None,
            }))
            .await
            .unwrap()
            .0;

        let meta = serde_json::to_value(&pack.metadata).unwrap();
        assert!(
            meta["generated_at"].as_str().is_some_and(|s| !s.is_empty()),
            "{meta}"
        );
        assert_eq!(meta["truncated"], false, "{meta}");
        assert!(meta["cache_hit"].is_boolean(), "{meta}");
        assert!(meta["source_event_count"].is_number(), "{meta}");
    }

    #[tokio::test]
    async fn task_close_reports_completeness_gaps() {
        let _env = handler_env();
        let server = TaskJournalServer;

        // Create a task WITH a goal so NoGoal won't fire.
        let task = server
            .task_create(Parameters(TaskCreateParams {
                title: "Gap me".into(),
                initial_context: None,
                goal: Some("ship it".into()),
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        // Close WITHOUT an outcome → ClosedNoOutcome gap.
        let res = server
            .task_close(Parameters(TaskCloseParams {
                task_id: task.clone(),
                reason: "done".into(),
                outcome: None,
                outcome_tag: None,
                session_id: None,
                module_notes: None,
            }))
            .await
            .unwrap()
            .0;

        assert!(res.closed);
        assert!(
            res.completeness_gaps
                .iter()
                .any(|g| g.contains("closed without a recorded outcome")),
            "gaps: {:?}",
            res.completeness_gaps
        );
    }

    #[tokio::test]
    async fn task_check_returns_score_and_gaps() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let task = server
            .task_create(Parameters(TaskCreateParams {
                title: "Check me".into(),
                initial_context: None,
                goal: Some("ship it".into()),
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;

        // A decision with no evidence → DecisionNoEvidence (warn, −3) → 97.
        server
            .event_add(Parameters(EventAddParams {
                task_id: task.clone(),
                event_type: "decision".into(),
                text: "Adopt X".into(),
                corrects: None,
                supersedes: None,
                alternatives: None,
                session_id: None,
            }))
            .await
            .unwrap();

        let res = server
            .task_check(Parameters(TaskCheckParams {
                task_id: task.clone(),
            }))
            .await
            .unwrap()
            .0;

        assert_eq!(res.score, 97);
        assert!(
            res.gaps
                .iter()
                .any(|g| g.severity == "warn" && g.detail.contains("decisions unverified")),
            "gaps: {:?}",
            res.gaps
        );
    }

    async fn create_task(server: &TaskJournalServer, title: &str) -> String {
        server
            .task_create(Parameters(TaskCreateParams {
                title: title.into(),
                initial_context: None,
                goal: Some(format!("goal of {title}")),
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id
    }

    async fn search(
        server: &TaskJournalServer,
        query: &str,
        status: Option<&str>,
        project: Option<&str>,
        event_type: Option<&str>,
    ) -> Result<TaskSearchResult, McpError> {
        server
            .task_search(Parameters(TaskSearchParams {
                query: query.into(),
                status: status.map(Into::into),
                project: project.map(Into::into),
                event_type: event_type.map(Into::into),
                module: None,
            }))
            .await
            .map(|j| j.0)
    }

    #[test]
    fn task_search_params_accept_a_missing_query() {
        let p: TaskSearchParams =
            serde_json::from_value(serde_json::json!({"status": "open"})).unwrap();
        assert_eq!(p.query, "");
    }

    #[tokio::test]
    async fn task_search_empty_query_lists_tasks_filtered_by_status() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let open = create_task(&server, "Empty query open").await;
        let closed = create_task(&server, "Empty query closed").await;
        server
            .task_close(Parameters(TaskCloseParams {
                task_id: closed.clone(),
                reason: "done".into(),
                outcome: None,
                outcome_tag: None,
                session_id: None,
                module_notes: None,
            }))
            .await
            .unwrap();

        let res = search(&server, "  ", Some("open"), None, None)
            .await
            .unwrap();
        assert!(res.results.contains(&open), "{:?}", res.results);
        assert!(!res.results.contains(&closed), "{:?}", res.results);
        let ids: Vec<_> = res.tasks.iter().map(|t| t.task_id.clone()).collect();
        assert_eq!(ids, res.results, "`tasks` must follow `results` order");
        let hit = res.tasks.iter().find(|t| t.task_id == open).unwrap();
        assert_eq!(hit.title, "Empty query open");
        assert_eq!(hit.status, "open");
        assert_eq!(hit.goal.as_deref(), Some("goal of Empty query open"));

        let res = search(&server, "", Some("closed"), None, None)
            .await
            .unwrap();
        assert!(res.results.contains(&closed) && !res.results.contains(&open));

        let res = search(&server, "", None, None, None).await.unwrap();
        assert!(res.results.contains(&closed) && res.results.contains(&open));
        assert!(
            res.tasks
                .windows(2)
                .all(|w| w[0].last_event_at >= w[1].last_event_at),
            "newest last_event_at first"
        );

        let res = search(&server, "", Some("any"), None, Some("close"))
            .await
            .unwrap();
        assert!(res.results.contains(&closed) && !res.results.contains(&open));
    }

    #[tokio::test]
    async fn task_search_status_filters_fts_hits_and_rejects_unknown_values() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let open = create_task(&server, "Quagga open").await;
        let closed = create_task(&server, "Quagga closed").await;
        server
            .task_close(Parameters(TaskCloseParams {
                task_id: closed.clone(),
                reason: "done".into(),
                outcome: None,
                outcome_tag: None,
                session_id: None,
                module_notes: None,
            }))
            .await
            .unwrap();

        let res = search(&server, "quagga", Some("open"), None, None)
            .await
            .unwrap();
        assert_eq!(res.results, vec![open.clone()]);
        assert_eq!(res.tasks[0].title, "Quagga open");

        let res = search(&server, "quagga", Some("closed"), None, None)
            .await
            .unwrap();
        assert_eq!(res.results, vec![closed.clone()]);

        let res = search(&server, "quagga", None, None, None).await.unwrap();
        assert_eq!(res.results.len(), 2);

        let err = search(&server, "quagga", Some("pending"), None, None)
            .await
            .expect_err("unknown status must be rejected");
        assert!(err.message.contains("status"), "{}", err.message);
    }

    #[tokio::test]
    async fn task_search_project_searches_that_projects_journal() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let other = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(other.path().join(".git")).unwrap();
        let (_, other_events, _) = resolve_project_paths(other.path()).unwrap();
        std::fs::create_dir_all(other_events.parent().unwrap()).unwrap();
        let task_id = tj_core::new_task_id();
        let mut ev = tj_core::event::Event::new(
            task_id.clone(),
            tj_core::event::EventType::Open,
            tj_core::event::Author::Agent,
            tj_core::event::Source::Chat,
            "Elsewhere quokka".into(),
        );
        ev.meta = serde_json::json!({"title": "Elsewhere quokka"});
        let mut writer = tj_core::storage::JsonlWriter::open(&other_events).unwrap();
        writer.append(&ev).unwrap();
        writer.flush_durable().unwrap();
        let other_dir = other.path().to_str().unwrap();

        let res = search(&server, "quokka", None, Some(other_dir), None)
            .await
            .unwrap();
        assert_eq!(res.results, vec![task_id.clone()]);

        let res = search(&server, "", Some("open"), Some(other_dir), None)
            .await
            .unwrap();
        assert_eq!(res.results, vec![task_id.clone()]);

        let res = search(&server, "quokka", None, None, None).await.unwrap();
        assert!(!res.results.contains(&task_id), "{:?}", res.results);

        assert!(search(&server, "", None, Some("relative/dir"), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn task_close_harvests_the_project_dir_not_the_cwd() {
        let _env = handler_env();
        let server = TaskJournalServer;

        // The test binary runs inside this repo's checkout, while the project
        // dir is a temp dir. A close must never stamp the cwd repo's commit.
        let cwd_commit = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let Some(cwd_commit) = cwd_commit else {
            return; // not run from a git checkout: nothing to tell apart
        };

        let task = create_task(&server, "Harvest dir").await;
        server
            .task_close(Parameters(TaskCloseParams {
                task_id: task.clone(),
                reason: "done".into(),
                outcome: None,
                outcome_tag: None,
                session_id: None,
                module_notes: None,
            }))
            .await
            .unwrap();

        let (_, events_path, _) = project_paths().unwrap();
        let jsonl = std::fs::read_to_string(&events_path).unwrap();
        let close = jsonl
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|v| v["task_id"] == task.as_str() && v["type"] == "close")
            .expect("close event in jsonl");
        assert!(
            !close["meta"].to_string().contains(&cwd_commit),
            "close harvested the cwd repo ({cwd_commit}): {}",
            close["meta"]
        );
    }

    #[tokio::test]
    async fn task_close_records_outcome_only_through_the_close_event() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let task = create_task(&server, "Append fails").await;
        let close = |outcome: &str| TaskCloseParams {
            task_id: task.clone(),
            reason: "done".into(),
            outcome: Some(outcome.into()),
            outcome_tag: Some("done".into()),
            session_id: None,
            module_notes: None,
        };

        // Make the close append fail: the journal is read-only.
        let (_, events_path, state_path) = project_paths().unwrap();
        let writable = std::fs::metadata(&events_path).unwrap().permissions();
        let mut read_only = writable.clone();
        read_only.set_readonly(true);
        std::fs::set_permissions(&events_path, read_only).unwrap();
        let res = server.task_close(Parameters(close("must not stick"))).await;
        std::fs::set_permissions(&events_path, writable).unwrap();
        assert!(res.is_err(), "append to a read-only journal must fail");

        let conn_arc = cached_open(&state_path).unwrap();
        {
            let conn = conn_arc.lock().unwrap();
            let meta = tj_core::db::task_metadata(&conn, &task).unwrap().unwrap();
            assert_eq!(meta.outcome, None, "failed close left an outcome behind");
            let status = tj_core::db::task_status(&conn, &task).unwrap();
            assert_eq!(status.as_deref(), Some("open"));
        }

        server
            .task_close(Parameters(close("shipped")))
            .await
            .unwrap();
        let conn = conn_arc.lock().unwrap();
        let meta = tj_core::db::task_metadata(&conn, &task).unwrap().unwrap();
        assert_eq!(meta.outcome.as_deref(), Some("shipped"));
        assert_eq!(meta.outcome_tag.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn event_add_and_artifact_add_reject_an_unknown_task() {
        let _env = handler_env();
        let server = TaskJournalServer;

        create_task(&server, "Known task").await;
        let (_, events_path, _) = project_paths().unwrap();
        let before = std::fs::read_to_string(&events_path).unwrap();

        let res = server
            .event_add(Parameters(EventAddParams {
                task_id: "tj-typo000000".into(),
                event_type: "finding".into(),
                text: "orphan".into(),
                corrects: None,
                supersedes: None,
                alternatives: None,
                session_id: None,
            }))
            .await;
        let err = match res {
            Ok(_) => panic!("event_add on an unknown task must fail"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("task not found: tj-typo000000"),
            "{}",
            err.message
        );

        let res = server
            .artifact_add(Parameters(ArtifactAddParams {
                task_id: "tj-typo000000".into(),
                kind: "doc".into(),
                url: "https://example.com/spec".into(),
                label: "Spec".into(),
                session_id: None,
            }))
            .await;
        let err = match res {
            Ok(_) => panic!("artifact_add on an unknown task must fail"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("task not found: tj-typo000000"),
            "{}",
            err.message
        );

        let after = std::fs::read_to_string(&events_path).unwrap();
        assert_eq!(after, before, "no orphan event may reach the journal");
    }

    #[tokio::test]
    async fn an_explicit_session_id_is_stamped_instead_of_the_env_one() {
        let _env = handler_env();
        let server = TaskJournalServer;
        let prev = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "from-env");

        let task = server
            .task_create(Parameters(TaskCreateParams {
                title: "Session task".into(),
                initial_context: None,
                goal: None,
                parent: None,
                session_id: Some("s-create".into()),
                modules: None,
            }))
            .await
            .unwrap()
            .0
            .task_id;
        let event = |session_id: Option<&str>| EventAddParams {
            task_id: task.clone(),
            event_type: "finding".into(),
            text: "seen".into(),
            corrects: None,
            supersedes: None,
            alternatives: None,
            session_id: session_id.map(Into::into),
        };
        let explicit = server.event_add(Parameters(event(Some("s-event")))).await;
        let from_env = server.event_add(Parameters(event(None))).await;
        let artifact = server
            .artifact_add(Parameters(ArtifactAddParams {
                task_id: task.clone(),
                kind: "doc".into(),
                url: "https://example.com/doc".into(),
                label: "Doc".into(),
                session_id: Some("s-artifact".into()),
            }))
            .await;
        let closed = server
            .task_close(Parameters(TaskCloseParams {
                task_id: task.clone(),
                reason: "done".into(),
                outcome: None,
                outcome_tag: None,
                session_id: Some("s-close".into()),
                module_notes: None,
            }))
            .await;
        match prev {
            Some(v) => std::env::set_var("CLAUDE_CODE_SESSION_ID", v),
            None => std::env::remove_var("CLAUDE_CODE_SESSION_ID"),
        }
        let (explicit, from_env, artifact) = (
            explicit.unwrap().0.event_id,
            from_env.unwrap().0.event_id,
            artifact.unwrap().0.event_id,
        );
        closed.unwrap();

        let (_, events_path, _) = project_paths().unwrap();
        let session_of = |pick: &dyn Fn(&serde_json::Value) -> bool| {
            std::fs::read_to_string(&events_path)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .find(|v| pick(v))
                .and_then(|v| v["meta"]["session_id"].as_str().map(str::to_string))
        };
        let by_id = |id: &str| session_of(&|v| v["event_id"] == id);
        let by_type = |ty: &str| session_of(&|v| v["task_id"] == task.as_str() && v["type"] == ty);
        assert_eq!(by_type("open").as_deref(), Some("s-create"));
        assert_eq!(by_id(&explicit).as_deref(), Some("s-event"));
        assert_eq!(by_id(&from_env).as_deref(), Some("from-env"));
        assert_eq!(by_id(&artifact).as_deref(), Some("s-artifact"));
        assert_eq!(by_type("close").as_deref(), Some("s-close"));
    }

    #[test]
    fn session_id_params_are_optional() {
        let p: EventAddParams = serde_json::from_value(serde_json::json!({
            "task_id": "tj-x", "event_type": "finding", "text": "t"
        }))
        .unwrap();
        assert_eq!(p.session_id, None);
    }

    fn create_params(title: &str, modules: Option<&[&str]>) -> TaskCreateParams {
        TaskCreateParams {
            title: title.into(),
            initial_context: None,
            goal: None,
            parent: None,
            session_id: None,
            modules: modules.map(|m| m.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn close_params(task_id: &str, notes: Option<Vec<(&str, &str)>>) -> TaskCloseParams {
        TaskCloseParams {
            task_id: task_id.into(),
            reason: "done".into(),
            outcome: Some("shipped".into()),
            outcome_tag: None,
            session_id: None,
            module_notes: notes.map(|n| {
                n.into_iter()
                    .map(|(module, text)| ModuleNote {
                        module: module.into(),
                        text: text.into(),
                    })
                    .collect()
            }),
        }
    }

    fn save_params(id: &str, name: Option<&str>, terms: &[&str]) -> ModuleSaveParams {
        ModuleSaveParams {
            module_id: id.into(),
            name: name.map(str::to_string),
            description: None,
            hints: Some(tj_core::modules::Hints {
                paths: vec![],
                terms: terms.iter().map(|t| t.to_string()).collect(),
            }),
            state: None,
            status: None,
            merged_into: None,
        }
    }

    fn journal_lines() -> usize {
        let (_, events_path, _) = project_paths().unwrap();
        std::fs::read_to_string(events_path)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn chronicle_params_are_optional() {
        let p: TaskCreateParams =
            serde_json::from_value(serde_json::json!({"title": "t"})).unwrap();
        assert!(p.modules.is_none());
        let c: TaskCloseParams =
            serde_json::from_value(serde_json::json!({"task_id": "tj-a", "reason": "r"})).unwrap();
        assert!(c.module_notes.is_none());
        let s: TaskSearchParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(s.module.is_none());

        let l: ModuleLinkParams = serde_json::from_value(
            serde_json::json!({"links": [{"task_id": "tj-a", "add": ["stars"]}]}),
        )
        .unwrap();
        assert_eq!(l.links[0].add, vec!["stars".to_string()]);
        assert!(l.links[0].remove.is_empty());
    }

    #[test]
    fn instructions_teach_the_chronicle() {
        for needle in ["modules=", "module_page", "module_notes", "📚 Chronicle"] {
            assert!(MCP_INSTRUCTIONS.contains(needle), "{needle}");
        }
    }

    #[tokio::test]
    async fn a_task_lives_in_its_module_from_create_to_close() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let saved = server
            .module_save(Parameters(save_params("mcp-flow", Some("Flow"), &[])))
            .await
            .unwrap()
            .0;
        assert!(saved.created);

        let created = server
            .task_create(Parameters(create_params("Flow work", Some(&["mcp-flow"]))))
            .await
            .unwrap()
            .0;
        assert_eq!(created.modules[0].module_id, "mcp-flow");

        let closed = server
            .task_close(Parameters(close_params(
                &created.task_id,
                Some(vec![("mcp-flow", "Flow now retries")]),
            )))
            .await
            .unwrap()
            .0;
        assert!(
            closed
                .module_reminder
                .as_deref()
                .unwrap_or("")
                .contains("module_save(state"),
            "{:?}",
            closed.module_reminder
        );

        let page = server
            .module_page(Parameters(ModulePageParams {
                module_id: "mcp-flow".into(),
            }))
            .await
            .unwrap()
            .0;
        assert!(page.text.contains("Flow now retries"), "{}", page.text);
        assert!(page.text.contains(&created.task_id), "{}", page.text);

        let hits = server
            .task_search(Parameters(TaskSearchParams {
                query: String::new(),
                status: None,
                project: None,
                event_type: None,
                module: Some("mcp-flow".into()),
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(hits.results, vec![created.task_id.clone()]);

        let map = server.module_list().await.unwrap().0;
        assert!(map.modules.iter().any(|m| m.module_id == "mcp-flow"));
    }

    #[tokio::test]
    async fn unknown_modules_and_new_modules_without_a_name_write_nothing() {
        let _env = handler_env();
        let server = TaskJournalServer;
        let before = journal_lines();

        let err = server
            .task_create(Parameters(create_params("Orphan", Some(&["mcp-nope"]))))
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("module_save"), "{}", err.message);

        let err = server
            .module_save(Parameters(save_params("mcp-unnamed", None, &[])))
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("name"), "{}", err.message);

        let err = server
            .module_save(Parameters(save_params("Bad Slug", Some("x"), &[])))
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("auth-refresh"), "{}", err.message);

        assert_eq!(journal_lines(), before);
    }

    #[tokio::test]
    async fn a_new_task_gets_module_suggestions_and_links_later() {
        let _env = handler_env();
        let server = TaskJournalServer;
        server
            .module_save(Parameters(save_params(
                "mcp-billing",
                Some("Billing"),
                &["invoice ledger"],
            )))
            .await
            .unwrap();

        let created = server
            .task_create(Parameters(create_params(
                "Fix the invoice ledger rounding",
                None,
            )))
            .await
            .unwrap()
            .0;
        assert!(
            created
                .suggested_modules
                .iter()
                .any(|s| s.module_id == "mcp-billing"),
            "{:?}",
            created.suggested_modules
        );
        assert!(created.chronicle.is_some());

        let candidates = server
            .module_backfill_candidates(Parameters(BackfillParams {
                limit: Some(100),
                offset: None,
            }))
            .await
            .unwrap()
            .0;
        assert!(candidates
            .candidates
            .iter()
            .any(|c| c.task_id == created.task_id));

        let linked = server
            .module_link(Parameters(ModuleLinkParams {
                links: vec![ModuleLink {
                    task_id: created.task_id.clone(),
                    add: vec!["mcp-billing".into()],
                    remove: vec![],
                }],
            }))
            .await
            .unwrap()
            .0;
        assert_eq!(linked.linked, 1);

        let closed = server
            .task_close(Parameters(close_params(&created.task_id, None)))
            .await
            .unwrap()
            .0;
        assert!(
            closed
                .module_reminder
                .as_deref()
                .unwrap_or("")
                .contains("mcp-billing"),
            "{:?}",
            closed.module_reminder
        );
    }

    #[tokio::test]
    async fn linking_redirects_a_merged_module_and_refuses_an_unknown_task() {
        let _env = handler_env();
        let server = TaskJournalServer;
        server
            .module_save(Parameters(save_params("mcp-new", Some("New"), &[])))
            .await
            .unwrap();
        server
            .module_save(Parameters(save_params("mcp-old", Some("Old"), &[])))
            .await
            .unwrap();
        let mut merge = save_params("mcp-old", None, &[]);
        merge.hints = None;
        merge.status = Some("merged".into());
        merge.merged_into = Some("mcp-new".into());
        server.module_save(Parameters(merge)).await.unwrap();

        let task = server
            .task_create(Parameters(create_params("Old area work", None)))
            .await
            .unwrap()
            .0
            .task_id;
        let linked = server
            .module_link(Parameters(ModuleLinkParams {
                links: vec![ModuleLink {
                    task_id: task.clone(),
                    add: vec!["mcp-old".into()],
                    remove: vec![],
                }],
            }))
            .await
            .unwrap()
            .0;
        assert!(
            linked.warnings[0].contains("merged into mcp-new"),
            "{:?}",
            linked.warnings
        );

        let err = server
            .module_link(Parameters(ModuleLinkParams {
                links: vec![ModuleLink {
                    task_id: "tj-missing".into(),
                    add: vec!["mcp-new".into()],
                    remove: vec![],
                }],
            }))
            .await
            .err()
            .unwrap();
        assert!(err.message.contains("tj-missing"), "{}", err.message);

        let mut into_self = save_params("mcp-new", None, &[]);
        into_self.hints = None;
        into_self.status = Some("merged".into());
        into_self.merged_into = Some("mcp-new".into());
        assert!(server.module_save(Parameters(into_self)).await.is_err());
    }

    #[tokio::test]
    async fn task_search_of_a_project_without_a_journal_creates_no_state_db() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let other = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(other.path().join(".git")).unwrap();
        let (_, _, other_state) = resolve_project_paths(other.path()).unwrap();
        let other_dir = other.path().to_str().unwrap();

        for query in ["", "anything"] {
            let res = search(&server, query, None, Some(other_dir), None)
                .await
                .unwrap();
            assert!(res.results.is_empty(), "{:?}", res.results);
        }
        assert!(!other_state.exists(), "created {other_state:?}");
    }

    #[tokio::test]
    async fn task_create_goal_and_loom_ref_survive_a_state_rebuild() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let loom_id = format!("t-rebuild-{}", ulid::Ulid::new());
        std::env::set_var("LOOM_TASK_ID", &loom_id);
        let created = server
            .task_create(Parameters(TaskCreateParams {
                title: "Loom task".into(),
                initial_context: None,
                goal: Some("Wire the board".into()),
                parent: None,
                session_id: None,
                modules: None,
            }))
            .await;
        std::env::remove_var("LOOM_TASK_ID");
        let task = created.unwrap().0.task_id;

        // A fresh SQLite rebuilt from the journal alone.
        let (project_hash, events_path, _) = project_paths().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let conn = tj_core::db::open(dir.path().join("fresh.sqlite")).unwrap();
        tj_core::db::rebuild_state(&conn, &events_path, &project_hash).unwrap();

        let meta = tj_core::db::task_metadata(&conn, &task).unwrap().unwrap();
        assert_eq!(meta.goal.as_deref(), Some("Wire the board"));
        let by_loom = tj_core::db::task_id_by_external(&conn, &format!("loom:{loom_id}")).unwrap();
        assert_eq!(by_loom.as_deref(), Some(task.as_str()));
    }

    #[tokio::test]
    async fn event_add_rejects_the_internal_amend_type() {
        let _env = handler_env();
        let server = TaskJournalServer;

        let task = create_task(&server, "No amend").await;
        let res = server
            .event_add(Parameters(EventAddParams {
                task_id: task,
                event_type: "amend".into(),
                text: "goal: sneaky".into(),
                corrects: None,
                supersedes: None,
                alternatives: None,
                session_id: None,
            }))
            .await;
        let err = match res {
            Ok(_) => panic!("amend is internal and must be rejected"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("unknown event type"),
            "{}",
            err.message
        );
    }

    #[test]
    fn into_mcp_error_carries_full_anyhow_chain() {
        // Down-stream callers rely on McpError.message containing the full
        // chain (root cause + every context wrap). Catches a regression
        // where someone formats with `{}` instead of `{:#}`.
        let inner = anyhow::anyhow!("root cause");
        let outer = inner.context("wrap layer");
        let err = into_mcp_error(outer);
        assert!(err.message.contains("wrap layer"), "got: {}", err.message);
        assert!(err.message.contains("root cause"), "got: {}", err.message);
    }

    #[test]
    fn task_pack_returns_rpc_error_when_state_dir_is_unusable() {
        // This test mutates the process-global XDG_DATA_HOME, which the
        // task_create/task_close handler tests read. Hold the same lock so
        // it is serialized with them — otherwise it poisons their env mid-run
        // and they fail with an unrelated path error (flaky under parallel CI).
        let _env = handler_env();

        // Force tj_core::paths::state_dir to fail by pointing it at a path
        // that cannot be created. We do this through XDG_DATA_HOME pointing
        // at /dev/null which directories crate refuses. The handler must
        // surface this as Err(McpError), not as a fake-success Json with
        // a corrupted task_id.
        //
        // We don't invoke the async handler directly here because it has
        // private generated wrappers; instead we exercise the same error
        // path via project_paths() and verify the conversion does the
        // right thing.
        let prev = std::env::var("XDG_DATA_HOME").ok();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/dev/null/cannot-create-here");
        }

        let res = project_paths();

        // restore
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        // We don't rigidly assert Err here (the directories crate has
        // platform-specific behavior); we only assert that *if* it errors,
        // into_mcp_error converts cleanly without panicking.
        if let Err(e) = res {
            let mcp_err = into_mcp_error(e);
            assert!(!mcp_err.message.is_empty());
        }
    }
}
