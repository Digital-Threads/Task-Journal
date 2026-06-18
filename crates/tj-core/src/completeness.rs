//! Capture completeness: deterministic, read-only detection of structural
//! gaps in a task's captured history. Measure + flag only — no mutation.

use std::path::Path;

use rusqlite::Connection;

use crate::artifacts::Artifacts;

#[derive(Debug, Clone, PartialEq)]
pub enum GapKind {
    ClosedNoOutcome,
    DecisionNoEvidence,
    SuggestedUnconfirmed,
    NoGoal,
    PendingLeak,
    /// A file referenced by the task's artifacts no longer exists on disk.
    MissingFile,
    /// A commit hash referenced by the task is unknown to git.
    DeadCommit,
    /// A local-file link (e.g. a doc) referenced by the task is missing.
    BrokenLink,
}

impl GapKind {
    /// Honesty-score weight, mirroring mex: error −10, warn −3, info −1.
    /// A structurally broken task (no goal, closed without outcome) is an
    /// error; a stale reference or unverified decision is a warning; soft
    /// hygiene signals are info.
    pub fn weight(&self) -> u32 {
        match self {
            GapKind::NoGoal | GapKind::ClosedNoOutcome => 10,
            GapKind::DecisionNoEvidence
            | GapKind::PendingLeak
            | GapKind::MissingFile
            | GapKind::DeadCommit
            | GapKind::BrokenLink => 3,
            GapKind::SuggestedUnconfirmed => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gap {
    pub kind: GapKind,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompletenessReport {
    pub gaps: Vec<Gap>,
}

impl CompletenessReport {
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }

    /// Honesty score 0–100: start at 100 and deduct each gap's weight,
    /// clamped at 0. A complete report scores 100.
    pub fn score(&self) -> u8 {
        let deduction: u32 = self.gaps.iter().map(|g| g.kind.weight()).sum();
        100u32.saturating_sub(deduction).min(100) as u8
    }
}

/// Assess a task's captured history for structural gaps. Deterministic and
/// read-only. `pending_count` (project-level unprocessed entries) is injected
/// so this fn stays filesystem-free and unit-testable.
pub fn assess(
    conn: &Connection,
    task_id: &str,
    pending_count: usize,
) -> anyhow::Result<CompletenessReport> {
    let mut gaps = Vec::new();

    // Metadata rules: read status/goal/outcome from the tasks row.
    let row: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT status, goal, outcome FROM tasks WHERE task_id = ?1",
            rusqlite::params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    let Some((status, goal, outcome)) = row else {
        // Unknown task → empty report (no panic).
        return Ok(CompletenessReport { gaps });
    };

    if goal.as_deref().unwrap_or("").is_empty() {
        gaps.push(Gap {
            kind: GapKind::NoGoal,
            detail: "no goal recorded".to_string(),
        });
    }
    if status == "closed"
        && !goal.as_deref().unwrap_or("").is_empty()
        && outcome.as_deref().unwrap_or("").is_empty()
    {
        gaps.push(Gap {
            kind: GapKind::ClosedNoOutcome,
            detail: "closed without a recorded outcome".to_string(),
        });
    }

    // Event rules: tally types and statuses for this task.
    let mut decisions = 0usize;
    let mut evidence = 0usize;
    let mut suggested = 0usize;
    {
        let mut stmt = conn.prepare("SELECT type, status FROM events_index WHERE task_id = ?1")?;
        let rows = stmt.query_map(rusqlite::params![task_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (ty, st) = row?;
            match ty.as_str() {
                "decision" => decisions += 1,
                "evidence" => evidence += 1,
                _ => {}
            }
            if st == "suggested" {
                suggested += 1;
            }
        }
    }
    if decisions > 0 && evidence == 0 {
        gaps.push(Gap {
            kind: GapKind::DecisionNoEvidence,
            detail: "decisions unverified (no evidence captured)".to_string(),
        });
    }
    if suggested > 0 {
        gaps.push(Gap {
            kind: GapKind::SuggestedUnconfirmed,
            detail: format!("{suggested} suggested event(s) unconfirmed"),
        });
    }

    if pending_count > 0 {
        gaps.push(Gap {
            kind: GapKind::PendingLeak,
            detail: format!(
                "{pending_count} pending entr{} not yet classified",
                if pending_count == 1 { "y" } else { "ies" }
            ),
        });
    }

    Ok(CompletenessReport { gaps })
}

/// Best-effort count of unprocessed pending entries for the cwd's project.
/// Returns 0 on any resolution/IO error — the PendingLeak rule then stays
/// silent rather than failing the whole assessment.
pub fn pending_count() -> usize {
    fn inner() -> anyhow::Result<usize> {
        let cwd = std::env::current_dir()?;
        let project_hash = crate::project_hash::from_path(&cwd)?;
        let events_path = crate::paths::events_dir()?.join(format!("{project_hash}.jsonl"));
        let dir = events_path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| anyhow::anyhow!("no grandparent"))?
            .join("pending");
        if !dir.exists() {
            return Ok(0);
        }
        let mut n = 0;
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            // Count live .json chunks; skip .dead and non-json.
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                n += 1;
            }
        }
        Ok(n)
    }
    inner().unwrap_or(0)
}

/// True when `url` is a local filesystem path rather than an http(s) link.
fn is_local_path(url: &str) -> bool {
    !url.starts_with("http://") && !url.starts_with("https://")
}

/// Pure artifact-honesty checker: given a task's aggregated artifacts and two
/// predicates — does a path exist? is a commit known to git? — return the
/// drift gaps. Filesystem- and git-free so it stays deterministic and
/// unit-testable; `assess_artifacts` supplies the real predicates.
pub fn check_artifacts(
    arts: &Artifacts,
    file_exists: impl Fn(&str) -> bool,
    commit_alive: impl Fn(&str) -> bool,
) -> Vec<Gap> {
    let mut gaps = Vec::new();
    for f in &arts.files {
        if !file_exists(f) {
            gaps.push(Gap {
                kind: GapKind::MissingFile,
                detail: format!("referenced file no longer exists: {f}"),
            });
        }
    }
    for c in &arts.commit_hashes {
        if !commit_alive(c) {
            gaps.push(Gap {
                kind: GapKind::DeadCommit,
                detail: format!("commit not found in git: {c}"),
            });
        }
    }
    // Only links that point at a local file can break locally; http(s) links
    // (commit/PR/branch web URLs) are remote and not checked here.
    for l in &arts.links {
        if is_local_path(&l.url) && !file_exists(&l.url) {
            gaps.push(Gap {
                kind: GapKind::BrokenLink,
                detail: format!("broken {} link: {} ({})", l.kind, l.label, l.url),
            });
        }
    }
    gaps
}

/// Filesystem+git artifact-honesty assessment for a task's aggregated
/// artifacts, rooted at `project_root`. A path is resolved relative to the
/// root; a commit is "alive" iff `git cat-file -e <sha>^{commit}` succeeds.
/// When `project_root` is not a git repo, commit checks are skipped (treated
/// as alive) so we never raise false DeadCommit drift outside a repo.
pub fn assess_artifacts(arts: &Artifacts, project_root: &Path) -> Vec<Gap> {
    let in_git = is_git_repo(project_root);
    let file_exists = |p: &str| -> bool {
        let path = Path::new(p);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };
        abs.exists()
    };
    let commit_alive = |sha: &str| -> bool { !in_git || git_has_commit(project_root, sha) };
    check_artifacts(arts, file_exists, commit_alive)
}

/// Best-effort artifact drift for the current working directory's project.
/// Returns no gaps unless cwd is a git repo — this avoids false
/// MissingFile/DeadCommit when a pack is assembled outside its project (e.g.
/// on the Loom host or in tests where paths resolve against an unrelated cwd).
pub fn artifact_gaps_for_cwd(arts: &Artifacts) -> Vec<Gap> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    artifact_gaps_in(arts, &cwd)
}

/// Best-effort artifact drift rooted at an explicit `dir` (e.g. the MCP
/// project-dir override). Returns no gaps unless `dir` is a git repo.
pub fn artifact_gaps_in(arts: &Artifacts, dir: &Path) -> Vec<Gap> {
    if !is_git_repo(dir) {
        return Vec::new();
    }
    assess_artifacts(arts, dir)
}

/// True when `dir` is inside a git working tree.
fn is_git_repo(dir: &Path) -> bool {
    std::process::Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True when `sha` resolves to a commit object in the repo at `dir`.
fn git_has_commit(dir: &Path, sha: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(dir)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Render the Completeness section, or None when there are no gaps.
pub fn render_section(report: &CompletenessReport) -> Option<String> {
    if report.gaps.is_empty() {
        return None;
    }
    let mut s = format!("\n## Completeness ({})\n", report.gaps.len());
    s.push_str(&format!("- honesty score: {}/100\n", report.score()));
    for g in &report.gaps {
        s.push_str(&format!("- ⚠ {}\n", g.detail));
    }
    Some(s)
}

/// Severity label for a gap, derived from its score weight.
fn severity_label(k: &GapKind) -> &'static str {
    match k.weight() {
        10 => "error",
        3 => "warn",
        _ => "info",
    }
}

/// Concrete, deterministic fix instruction for each gap kind.
fn fix_instruction(k: &GapKind) -> &'static str {
    match k {
        GapKind::NoGoal => "Record the task's one-sentence goal.",
        GapKind::ClosedNoOutcome => {
            "Add the outcome: re-close with an outcome/reason, or add a finding summarising the result."
        }
        GapKind::DecisionNoEvidence => "Add an evidence event proving the key decision(s).",
        GapKind::SuggestedUnconfirmed => "Review the suggested event(s) and confirm or correct each.",
        GapKind::PendingLeak => "Classify the pending entries (run the classifier / drain the queue).",
        GapKind::MissingFile => {
            "Referenced file is gone — add a correction event with the current path, or confirm intentional removal."
        }
        GapKind::DeadCommit => {
            "Referenced commit is not in git — correct the hash via a correction event."
        }
        GapKind::BrokenLink => {
            "Local link is broken — fix or drop it (artifact_add / correction event)."
        }
    }
}

/// Build a targeted, deterministic gap-fill prompt for an in-session agent to
/// close the gaps in `report` for `task_id`. Mirrors mex's sync brief: each
/// issue gets a concrete fix instruction, and the current pack is embedded as
/// read-only context. Emits NO LLM call — the caller prints it; the agent runs
/// it cheaply on the session subscription. Returns None when there are no gaps.
pub fn build_gap_fill_prompt(
    task_id: &str,
    report: &CompletenessReport,
    pack_text: &str,
) -> Option<String> {
    if report.gaps.is_empty() {
        return None;
    }
    let mut s = format!(
        "Task {task_id} has {} completeness gap(s) (honesty {}/100). \
Close ONLY these — do not invent work:\n\n",
        report.gaps.len(),
        report.score()
    );
    for g in &report.gaps {
        s.push_str(&format!(
            "- [{}] {}\n  → {}\n",
            severity_label(&g.kind),
            g.detail,
            fix_instruction(&g.kind)
        ));
    }
    s.push_str(
        "\nCurrent task pack (context — fix against this, change nothing already correct):\n\n```markdown\n",
    );
    s.push_str(pack_text);
    s.push_str("\n```\n");
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Author, Event, EventType, Source};
    use tempfile::TempDir;

    fn conn() -> (TempDir, Connection) {
        let d = TempDir::new().unwrap();
        let c = crate::db::open(d.path().join("s.sqlite")).unwrap();
        (d, c)
    }

    fn open_task(c: &Connection, id: &str) {
        let e = Event::new(id, EventType::Open, Author::User, Source::Cli, id.into());
        crate::db::upsert_task_from_event(c, &e, "ph").unwrap();
    }

    fn add_event(c: &Connection, task: &str, ty: EventType, status: crate::event::EventStatus) {
        let mut e = Event::new(task, ty, Author::Agent, Source::Hook, "x".into());
        e.status = status;
        crate::db::upsert_task_from_event(c, &e, "ph").unwrap();
        crate::db::index_event(c, &e).unwrap();
    }

    #[test]
    fn no_goal_fires_when_goal_absent() {
        let (_d, c) = conn();
        open_task(&c, "t1");
        let r = assess(&c, "t1", 0).unwrap();
        assert!(r.gaps.iter().any(|g| g.kind == GapKind::NoGoal));
    }

    #[test]
    fn closed_no_outcome_fires() {
        let (_d, c) = conn();
        open_task(&c, "t2");
        // Set a goal, then close without outcome.
        c.execute("UPDATE tasks SET goal='ship X' WHERE task_id='t2'", [])
            .unwrap();
        c.execute("UPDATE tasks SET status='closed' WHERE task_id='t2'", [])
            .unwrap();
        let r = assess(&c, "t2", 0).unwrap();
        assert!(r.gaps.iter().any(|g| g.kind == GapKind::ClosedNoOutcome));
        assert!(!r.gaps.iter().any(|g| g.kind == GapKind::NoGoal));
    }

    #[test]
    fn unknown_task_is_empty_report() {
        let (_d, c) = conn();
        let r = assess(&c, "nope", 0).unwrap();
        assert!(r.is_complete());
    }

    #[test]
    fn decision_without_evidence_fires_then_clears() {
        use crate::event::EventStatus;
        let (_d, c) = conn();
        open_task(&c, "t3");
        c.execute("UPDATE tasks SET goal='g' WHERE task_id='t3'", [])
            .unwrap();
        add_event(&c, "t3", EventType::Decision, EventStatus::Confirmed);
        let r = assess(&c, "t3", 0).unwrap();
        assert!(r.gaps.iter().any(|g| g.kind == GapKind::DecisionNoEvidence));

        add_event(&c, "t3", EventType::Evidence, EventStatus::Confirmed);
        let r2 = assess(&c, "t3", 0).unwrap();
        assert!(!r2
            .gaps
            .iter()
            .any(|g| g.kind == GapKind::DecisionNoEvidence));
    }

    #[test]
    fn suggested_unconfirmed_counts() {
        use crate::event::EventStatus;
        let (_d, c) = conn();
        open_task(&c, "t4");
        c.execute("UPDATE tasks SET goal='g' WHERE task_id='t4'", [])
            .unwrap();
        add_event(&c, "t4", EventType::Finding, EventStatus::Suggested);
        add_event(&c, "t4", EventType::Finding, EventStatus::Suggested);
        let r = assess(&c, "t4", 0).unwrap();
        let g = r
            .gaps
            .iter()
            .find(|g| g.kind == GapKind::SuggestedUnconfirmed)
            .unwrap();
        assert!(g.detail.contains('2'));
    }

    #[test]
    fn pending_leak_fires_when_count_positive() {
        let (_d, c) = conn();
        open_task(&c, "t5");
        c.execute("UPDATE tasks SET goal='g' WHERE task_id='t5'", [])
            .unwrap();
        let r = assess(&c, "t5", 3).unwrap();
        let g = r
            .gaps
            .iter()
            .find(|g| g.kind == GapKind::PendingLeak)
            .unwrap();
        assert!(g.detail.contains('3'));

        let r0 = assess(&c, "t5", 0).unwrap();
        assert!(!r0.gaps.iter().any(|g| g.kind == GapKind::PendingLeak));
    }

    #[test]
    fn pending_count_zero_when_no_dir() {
        // Best-effort contract: resolution may succeed or fail, but it must
        // never panic. In a clean env with no pending dir the count is 0.
        let _ = pending_count();
    }

    #[test]
    fn render_section_none_when_complete() {
        let r = CompletenessReport::default();
        assert!(render_section(&r).is_none());
    }

    #[test]
    fn render_section_lists_gaps() {
        let r = CompletenessReport {
            gaps: vec![Gap {
                kind: GapKind::NoGoal,
                detail: "no goal recorded".into(),
            }],
        };
        let s = render_section(&r).unwrap();
        assert!(s.contains("Completeness (1)"));
        assert!(s.contains("no goal recorded"));
    }

    #[test]
    fn score_is_100_when_complete() {
        let r = CompletenessReport::default();
        assert_eq!(r.score(), 100);
    }

    #[test]
    fn score_deducts_by_weight_and_clamps() {
        // NoGoal (10) + MissingFile (3) + SuggestedUnconfirmed (1) = 14 → 86.
        let r = CompletenessReport {
            gaps: vec![
                Gap {
                    kind: GapKind::NoGoal,
                    detail: "x".into(),
                },
                Gap {
                    kind: GapKind::MissingFile,
                    detail: "x".into(),
                },
                Gap {
                    kind: GapKind::SuggestedUnconfirmed,
                    detail: "x".into(),
                },
            ],
        };
        assert_eq!(r.score(), 86);

        // 11 errors × 10 = 110 deduction → clamps to 0, never underflows.
        let many = CompletenessReport {
            gaps: (0..11)
                .map(|_| Gap {
                    kind: GapKind::NoGoal,
                    detail: "x".into(),
                })
                .collect(),
        };
        assert_eq!(many.score(), 0);
    }

    #[test]
    fn render_section_shows_honesty_score() {
        let r = CompletenessReport {
            gaps: vec![Gap {
                kind: GapKind::MissingFile,
                detail: "gone.rs".into(),
            }],
        };
        let s = render_section(&r).unwrap();
        assert!(s.contains("honesty score: 97/100"));
    }

    #[test]
    fn check_artifacts_flags_missing_file_dead_commit_broken_link() {
        let arts = Artifacts {
            files: vec!["src/live.rs".into(), "src/gone.rs".into()],
            commit_hashes: vec!["dead00".into(), "alive1".into()],
            links: vec![
                crate::artifacts::ArtifactLink {
                    kind: "doc".into(),
                    url: "docs/missing.md".into(),
                    label: "spec".into(),
                },
                crate::artifacts::ArtifactLink {
                    kind: "commit".into(),
                    url: "https://github.com/x/y/commit/abc".into(),
                    label: "abc".into(),
                },
            ],
            ..Default::default()
        };
        // live file present, gone file absent; alive1 alive, dead00 dead.
        let gaps = check_artifacts(&arts, |p| p == "src/live.rs", |c| c == "alive1");
        assert!(gaps
            .iter()
            .any(|g| g.kind == GapKind::MissingFile && g.detail.contains("src/gone.rs")));
        assert!(gaps
            .iter()
            .any(|g| g.kind == GapKind::DeadCommit && g.detail.contains("dead00")));
        assert!(gaps
            .iter()
            .any(|g| g.kind == GapKind::BrokenLink && g.detail.contains("docs/missing.md")));
        // The live file, alive commit, and remote http link raise nothing.
        assert_eq!(gaps.len(), 3);
    }

    #[test]
    fn build_gap_fill_prompt_none_when_complete_else_lists_gaps() {
        let complete = CompletenessReport::default();
        assert!(build_gap_fill_prompt("t1", &complete, "pack").is_none());

        let r = CompletenessReport {
            gaps: vec![
                Gap {
                    kind: GapKind::ClosedNoOutcome,
                    detail: "closed without a recorded outcome".into(),
                },
                Gap {
                    kind: GapKind::MissingFile,
                    detail: "referenced file no longer exists: src/gone.rs".into(),
                },
            ],
        };
        let p = build_gap_fill_prompt("tj-abc", &r, "# PACK BODY").unwrap();
        assert!(p.contains("tj-abc"));
        assert!(p.contains("honesty 87/100")); // 100 - 10 - 3
        assert!(p.contains("[error] closed without a recorded outcome"));
        assert!(p.contains("[warn] referenced file no longer exists: src/gone.rs"));
        assert!(p.contains("# PACK BODY"));
    }

    #[test]
    fn check_artifacts_clean_when_all_present() {
        let arts = Artifacts {
            files: vec!["a.rs".into()],
            commit_hashes: vec!["c1".into()],
            ..Default::default()
        };
        let gaps = check_artifacts(&arts, |_| true, |_| true);
        assert!(gaps.is_empty());
    }
}
